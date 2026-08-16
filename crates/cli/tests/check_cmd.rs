//! End-to-end test for `meshfox check` resolving includes before
//! evaluating constraints — the composed-tree behavior it needs to share
//! with `meshfox view`/the tui (`meshfox_core::constraint::annotate_status`
//! called after `include::resolve` there), so a constraint fence living
//! inside an included canvas is actually reachable from the including
//! document rather than silently skipped.

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
    Command::new(env!("CARGO_BIN_EXE_meshfox"))
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
    // The constraint lives in the spliced-in node, addressed under its
    // namespaced id — proof `check` actually reached inside the include
    // rather than only looking at `base.canvas.md`'s own (constraint-free)
    // content.
    assert!(
        stderr.contains("child/root"),
        "expected the namespaced label in stderr, got: {stderr}"
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
