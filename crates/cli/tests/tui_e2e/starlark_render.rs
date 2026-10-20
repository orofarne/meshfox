//! Computed views through the real worker, TUI event loop and terminal.
use crate::{fixtures, harness::TuiSession};
use std::{
    path::Path,
    process::Command,
    time::{Duration, Instant},
};

const SOURCE: &str = concat!(
    "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n",
    "```starlark render name=\"tag-counts\"\n",
    "counts = {}\nfor n in [doc] + doc.descendants():\n",
    "    for tag in n.tags:\n        counts[tag] = counts.get(tag, 0) + 1\n",
    "print(md.table(headers=[\"Tag\", \"Nodes\"], rows=[[tag, \"COUNT=\" + str(counts[tag])] for tag in sorted(counts)]))\n",
    "```\n\n```starlark render name=\"broken\"\nundefined_name()\n```\n",
    "## Task\n<!-- meshfox:node id=\"task\" tags=\"todo\" -->\nA task.\n",
);
fn wait_absent(session: &TuiSession, text: &str) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while session.screen_text().contains(text) {
        assert!(
            Instant::now() < deadline,
            "{text} remains visible:\n{}",
            session.screen_text()
        );
        std::thread::sleep(Duration::from_millis(30));
    }
}
fn mutate(path: &Path, args: &[&str]) {
    let output = Command::new(env!("CARGO_BIN_EXE_meshfox"))
        .arg(path)
        .arg("node")
        .args(args)
        .env("HOME", path.parent().unwrap())
        .env("MESHFOX_SERVER_SOCKET", "")
        .output()
        .expect("another client calls the worker");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
#[test]
#[ignore = "pty-based e2e"]
fn starlark_render_table_source_controls_errors_and_remote_mutations() {
    let (path, dir) = fixtures::write_fixture(SOURCE);
    let mut session = TuiSession::spawn(&path, dir, 40, 130);
    session
        .wait_for("COUNT=1", Duration::from_secs(10))
        .unwrap();
    session
        .wait_for("undefined_name", Duration::from_secs(5))
        .unwrap();
    let initial = session.screen_text();
    assert!(
        !initial.contains("counts = {}"),
        "source starts collapsed:\n{initial}"
    );
    assert!(
        !initial.contains("| --- |"),
        "real table, not pipe syntax:\n{initial}"
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap(), SOURCE);

    session.send_keys("\t]c");
    session
        .wait_for("render: tag-counts", Duration::from_secs(5))
        .unwrap();
    assert!(!session.screen_text().contains("R run"));
    session
        .wait_for("counts = {}", Duration::from_secs(5))
        .unwrap();
    assert!(session.screen_text().contains("COUNT=1"));
    session.send_keys("\r");
    wait_absent(&session, "counts = {}");
    let (row, col) = session
        .find("starlark · tag-counts")
        .expect("computed header");
    session.send_mouse_click(row, col + 1);
    session
        .wait_for("counts = {}", Duration::from_secs(5))
        .unwrap();
    let (row, col) = session.find("starlark · tag-counts").unwrap();
    session.send_mouse_click(row, col + 1);
    wait_absent(&session, "counts = {}");

    mutate(
        &path,
        &["meta", "task", "--remove-tag", "todo", "--add-tag", "done"],
    );
    session.wait_for("done", Duration::from_secs(10)).unwrap();
    wait_absent(&session, "todo");
    assert!(session.screen_text().contains("COUNT=1"));
    mutate(&path, &["rm", "task"]);
    wait_absent(&session, "COUNT=1");
    let saved = std::fs::read_to_string(&path).unwrap();
    assert!(!saved.contains("meshfox:output"));
    assert!(!saved.contains("renderResults"));
    assert!(saved.contains("counts = {}"));
}
