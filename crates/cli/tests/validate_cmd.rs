//! End-to-end test for `meshfox validate`'s unknown-attribute check
//! (TODO.canvas.md: "Ошибка при неизвестных параметрах в validate") —
//! confirms the *CLI wiring* (exit code, stderr message) on top of what
//! `meshfox_core::validate_known_attrs`'s own unit tests already cover in
//! isolation. Also confirms the flip side: an unknown attribute must NOT
//! break `meshfox run`, only `validate` — the whole point of keeping this
//! check separate from `mdcanvas::parse` itself.

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn unique_path() -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("meshfox-validate-test-{nanos}-{n}.canvas.md"))
}

fn meshfox() -> Command {
    Command::new(env!("CARGO_BIN_EXE_meshfox"))
}

#[test]
fn validate_fails_on_an_unknown_node_attribute_with_a_message_naming_it() {
    let path = unique_path();
    std::fs::write(
        &path,
        "# Root\n<!-- meshfox:node id=\"root\" colr=\"1\" -->\n\nbody\n",
    )
    .unwrap();

    let output = meshfox().arg("validate").arg(&path).output().unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("colr"), "stderr: {stderr}");

    let _ = std::fs::remove_file(&path);
}

#[test]
fn run_still_succeeds_on_the_same_unknown_attribute_validate_rejects() {
    let path = unique_path();
    std::fs::write(
        &path,
        "# Root\n<!-- meshfox:node id=\"root\" colr=\"1\" -->\n\n```bash name=\"hi\"\necho hi\n```\n",
    )
    .unwrap();

    // The whole point: every other reader keeps silently accepting an
    // attribute it doesn't recognize — only `validate` gets stricter.
    // The path is empty (just the block name) since the fence lives
    // directly in the root's own body — the root itself is never a path
    // segment (see e.g. `lib.rs`'s own `run_block` tests: `&["tests"]`
    // for a block under a `## Tests` child, never `&["project", ...]`).
    let output = meshfox()
        .arg("run")
        .arg("--canvas")
        .arg(&path)
        .arg("hi")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("hi"));

    let _ = std::fs::remove_file(&path);
}
