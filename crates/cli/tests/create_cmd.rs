//! End-to-end test for `meshfox create`'s template — specifically the
//! `meshfox:comment`-wrapped "this is a meshfox document" note it now
//! opens the root body with (TODO.canvas.md: "Добавить meshfox:comment").
//! Visible to a plain Markdown viewer (GitHub, ...), invisible to
//! meshfox's own parsing — see `crates/core/src/comment.rs`.

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
    std::env::temp_dir().join(format!("meshfox-create-test-{nanos}-{n}.canvas.md"))
}

fn meshfox() -> Command {
    Command::new(env!("CARGO_BIN_EXE_meshfox"))
}

#[test]
fn create_writes_a_meshfox_comment_wrapped_note_that_the_parser_strips_from_the_root_body() {
    let path = unique_path();

    let output = meshfox().arg("create").arg(&path).output().unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let raw = std::fs::read_to_string(&path).unwrap();
    // Visible to a plain Markdown viewer: the marker pair and the note
    // text between them are both literally in the raw file.
    assert!(raw.contains("<!-- meshfox:comment -->"));
    assert!(raw.contains("<!-- /meshfox:comment -->"));
    assert!(raw.contains("meshfox view"));

    // Invisible to meshfox's own parser: the root node's body is empty
    // once the comment region is stripped.
    let canvas = meshfox_core::mdcanvas::parse(&raw).unwrap();
    let root = canvas.root().unwrap();
    assert_eq!(root.text, "");

    let validate = meshfox().arg("validate").arg(&path).output().unwrap();
    assert!(
        validate.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&validate.stderr)
    );

    let _ = std::fs::remove_file(&path);
}
