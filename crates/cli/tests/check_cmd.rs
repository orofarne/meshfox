//! End-to-end test for `meshfox check` resolving includes before
//! evaluating constraints — the composed-tree behavior it needs to share
//! with `meshfox view`/the tui (`meshfox_core::constraint::annotate_status`
//! called after `include::resolve` there), so a constraint fence written
//! inside an include target's own text is actually reachable from the
//! including document (dumped straight into the include node's own body)
//! rather than silently skipped.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn unique_dir(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("meshfox-check-test-{tag}-{nanos}-{n}"))
}

fn write_file(path: &Path, contents: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, contents).unwrap();
}

fn meshfox() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_meshfox"));
    // Isolates this process from the real machine's own
    // `~/.meshfox/config.toml` — a developer with `server_socket` set for
    // daily use would otherwise have this command routed through *their*
    // external coordinator (see `validate_cmd.rs`'s `meshfox()` for the
    // full rationale). The directory doesn't need to exist.
    cmd.env("HOME", unique_dir("home"));
    cmd
}

#[test]
fn check_catches_a_failing_constraint_inside_an_included_canvas() {
    let dir = unique_dir("fail");
    write_file(
        &dir.join("child.canvas.md"),
        concat!(
            "<!-- meshfox:canvas -->\n# Child\n<!-- meshfox:node id=\"root\" -->\n\n",
            "```starlark constraint\nfail(\"always fails\")\n```\n",
        ),
    );
    let base = dir.join("base.canvas.md");
    write_file(
        &base,
        concat!(
            "<!-- meshfox:canvas -->\n# Base\n<!-- meshfox:node id=\"base\" -->\n\n",
            "## Child\n<!-- meshfox:node id=\"child\" type=\"include\" -->\n\n[child](./child.canvas.md)\n",
        ),
    );

    let output = meshfox().arg("check").arg(&base).output().unwrap();
    assert!(
        !output.status.success(),
        "expected a non-zero exit for a failing constraint"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    // The constraint fence lives in the include target's own dumped-in
    // text, addressed under the include node's own id ("child") — proof
    // `check` actually reached inside the include rather than only
    // looking at `base.canvas.md`'s own (constraint-free) content.
    assert!(
        stderr.contains("child"),
        "expected the include node's own label in stderr, got: {stderr}"
    );
    assert!(
        stderr.contains("always fails"),
        "expected the fail() message in stderr, got: {stderr}"
    );
}

#[test]
fn check_passes_when_the_included_canvas_has_no_constraints() {
    let dir = unique_dir("ok");
    write_file(
        &dir.join("child.canvas.md"),
        "<!-- meshfox:canvas -->\n# Child\n<!-- meshfox:node id=\"root\" -->\n\nno constraints here\n",
    );
    let base = dir.join("base.canvas.md");
    write_file(
        &base,
        concat!(
            "<!-- meshfox:canvas -->\n# Base\n<!-- meshfox:node id=\"base\" -->\n\n",
            "## Child\n<!-- meshfox:node id=\"child\" type=\"include\" -->\n\n[child](./child.canvas.md)\n",
        ),
    );

    let output = meshfox().arg("check").arg(&base).output().unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn check_reports_a_broken_include_target_instead_of_silently_ignoring_it() {
    let dir = unique_dir("broken");
    let base = dir.join("base.canvas.md");
    write_file(
        &base,
        concat!(
            "<!-- meshfox:canvas -->\n# Base\n<!-- meshfox:node id=\"base\" -->\n\n",
            "## Child\n<!-- meshfox:node id=\"child\" type=\"include\" -->\n\n[child](./missing.canvas.md)\n",
        ),
    );

    let output = meshfox().arg("check").arg(&base).output().unwrap();
    assert!(
        !output.status.success(),
        "expected a non-zero exit for a broken include target"
    );
}

const RENDER_CANVAS: &str = concat!(
    "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
    "```starlark render name=\"total\"\nprint(\"total:\", sum([1, 2, 3]))\n```\n\n",
    "## Child\n<!-- meshfox:node id=\"child\" -->\n\n",
    "```starlark render name=\"broken\"\nprint(no_such_name)\n```\n",
);

#[test]
fn check_fails_on_a_render_block_that_raises_an_error() {
    let dir = unique_dir("render-fail");
    let canvas = dir.join("doc.canvas.md");
    write_file(&canvas, RENDER_CANVAS);

    let output = meshfox().arg("check").arg(&canvas).output().unwrap();
    assert!(
        !output.status.success(),
        "a broken render block must fail the check"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stdout.contains("ok   render root/total"),
        "stdout: {stdout}"
    );
    assert!(
        stderr.contains("FAIL render child/broken"),
        "stderr: {stderr}"
    );
    assert!(stderr.contains("no_such_name"), "stderr: {stderr}");
    assert!(
        stderr.contains("1/2 render blocks failed"),
        "stderr: {stderr}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn check_counts_working_render_blocks_and_passes() {
    let dir = unique_dir("render-ok");
    let canvas = dir.join("doc.canvas.md");
    write_file(
        &canvas,
        concat!(
            "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
            "```starlark render\nprint(\"hi\")\n```\n\n",
            "```starlark constraint\npass\n```\n",
        ),
    );

    let output = meshfox().arg("check").arg(&canvas).output().unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("ok (1 constraint, 1 render block)"),
        "stdout: {stdout}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn render_prints_the_computed_markdown_and_narrows_by_node_and_name() {
    let dir = unique_dir("render-print");
    let canvas = dir.join("doc.canvas.md");
    write_file(&canvas, RENDER_CANVAS);

    // One selected block: just its Markdown, nothing else.
    let output = meshfox()
        .arg("render")
        .arg(&canvas)
        .args(["--name", "total"])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(String::from_utf8_lossy(&output.stdout), "total: 6\n");

    let output = meshfox()
        .arg("render")
        .arg(&canvas)
        .args(["--node", "root"])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(String::from_utf8_lossy(&output.stdout), "total: 6\n");

    // Everything: each block labelled; the failing one reports on stderr
    // and makes the exit status non-zero, without hiding the good one.
    let output = meshfox().arg("render").arg(&canvas).output().unwrap();
    assert!(!output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("<!-- render: root/total -->\ntotal: 6\n"),
        "stdout: {stdout}"
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("child/broken"),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // Nothing matches.
    let output = meshfox()
        .arg("render")
        .arg(&canvas)
        .args(["--node", "nope"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("no starlark render block"),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let _ = std::fs::remove_dir_all(&dir);
}
