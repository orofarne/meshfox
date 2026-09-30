//! `L` run-history view (`App::open_run_history`) — the terminal counterpart
//! of the web UI's run-history dialog: lists the finished runs of the
//! selected node's block the worker keeps, with the selected run's stored
//! output next to them. Driven end to end: a real run (`R`), then `L`.

use std::time::Duration;

use crate::fixtures;
use crate::harness::TuiSession;

#[test]
#[ignore = "pty-based e2e — run via `cargo test --test tui_e2e -- --ignored`"]
fn l_shows_the_finished_run_with_its_exit_code_and_output() {
    let (canvas_path, dir) = fixtures::write_fixture(fixtures::SIMPLE_RUNNABLE);
    let mut session = TuiSession::spawn(&canvas_path, dir, 30, 110);
    session
        .wait_for("Root", Duration::from_secs(5))
        .expect("the tree should render");

    session.send_keys("j");
    session
        .wait_for("Leaf", Duration::from_secs(2))
        .expect("tree still renders");
    session.send_keys("R");
    session
        .wait_for("tui-e2e-marker-output", Duration::from_secs(10))
        .expect("the block's real stdout should show up in the Output pane");

    // The worker stores a run's output when it ends; give the run's own
    // completion a moment so the history has something to list.
    std::thread::sleep(Duration::from_millis(500));
    session.send_keys("L");
    session
        .wait_for("run history", Duration::from_secs(5))
        .unwrap_or_else(|_| {
            panic!(
                "L should open the run-history view; screen:\n{}",
                session.screen_text()
            )
        });
    let screen = session.screen_text();
    assert!(
        screen.contains("exit 0"),
        "the run's exit code is listed:\n{screen}"
    );
    assert!(
        screen.contains("leaf/leaf"),
        "the block is named in the title:\n{screen}"
    );

    session.send_keys("q");
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    while session.screen_text().contains("run history") {
        assert!(
            std::time::Instant::now() < deadline,
            "q should close the view:\n{}",
            session.screen_text()
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
#[ignore = "pty-based e2e — run via `cargo test --test tui_e2e -- --ignored`"]
fn l_on_a_block_that_never_ran_says_so_instead_of_opening_an_empty_view() {
    let (canvas_path, dir) = fixtures::write_fixture(fixtures::SIMPLE_RUNNABLE);
    let mut session = TuiSession::spawn(&canvas_path, dir, 30, 110);
    session
        .wait_for("Root", Duration::from_secs(5))
        .expect("the tree should render");
    session.send_keys("j");
    session
        .wait_for("Leaf", Duration::from_secs(2))
        .expect("tree still renders");
    // The status line lives in the Output pane, collapsed at startup:
    // click its handle open before asking.
    let (row, col) = session.find("Output").expect("the collapsed Output handle");
    session.send_mouse_click(row, col);
    std::thread::sleep(Duration::from_millis(300));
    session.send_keys("L");
    session
        .wait_for("no earlier runs", Duration::from_secs(5))
        .unwrap_or_else(|_| {
            panic!(
                "expected the status message; screen:\n{}",
                session.screen_text()
            )
        });
}

/// Copies `from` (a fixture directory, `.meshfox` session database included)
/// to a fresh sibling — what survives a restart, minus the process itself.
fn copy_dir(from: &std::path::Path, to: &std::path::Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}

/// The point of persisting runs: a *new* TUI (and worker) on the same canvas
/// shows the last run's output again without anything being run.
#[test]
#[ignore = "pty-based e2e — run via `cargo test --test tui_e2e -- --ignored`"]
fn a_restarted_tui_shows_the_last_runs_output_without_running_anything() {
    // The marker is computed (`$((40+2))`), so it appears on screen only as
    // real output — never as part of the block's own source shown above it.
    let (canvas_path, dir) = fixtures::write_fixture(concat!(
        "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
        "## Leaf\n<!-- meshfox:node id=\"leaf\" parent=\"root\" -->\n\n",
        "```bash name=\"leaf\"\necho persisted-$((40+2))\n```\n",
    ));
    let file_name = canvas_path.file_name().unwrap().to_owned();
    let mut first = TuiSession::spawn(&canvas_path, dir.clone(), 30, 110);
    first
        .wait_for("Root", Duration::from_secs(5))
        .expect("the tree should render");
    first.send_keys("j");
    first
        .wait_for("Leaf", Duration::from_secs(2))
        .expect("tree still renders");
    first.send_keys("R");
    first
        .wait_for("persisted-42", Duration::from_secs(10))
        .expect("the block's real stdout should show up");
    std::thread::sleep(Duration::from_millis(500));

    // Quit (the worker goes with it), then carry the fixture — including the
    // session database — over to a directory the first session's `Drop`
    // won't delete.
    first.send_keys("q");
    assert!(
        first.wait_for_exit(Duration::from_secs(5)),
        "q should exit the TUI"
    );
    let carried = dir.with_file_name(format!(
        "{}-carried",
        dir.file_name().unwrap().to_string_lossy()
    ));
    copy_dir(&dir, &carried);
    drop(first);

    let carried_canvas = carried.join(file_name);
    let mut second = TuiSession::spawn(&carried_canvas, carried, 30, 110);
    second
        .wait_for("Root", Duration::from_secs(5))
        .expect("the tree should render");
    second.send_keys("j");
    second
        .wait_for("Leaf", Duration::from_secs(2))
        .expect("tree still renders");
    second
        .wait_for("persisted-42", Duration::from_secs(10))
        .unwrap_or_else(|_| {
            panic!(
                "the last run's output should be back without running; screen:\n{}",
                second.screen_text()
            )
        });
}
