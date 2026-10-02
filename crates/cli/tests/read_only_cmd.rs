//! End-to-end: a canvas in a directory (or a file) that can't be written is
//! served read-only. Blocks run, the local `.meshfox/config.toml` is still
//! read, edits are refused with a message that says why, and nothing —
//! no `.meshfox/` state, no change to the canvas — is written.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

const CANVAS: &str = concat!(
    "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
    "## A\n<!-- meshfox:node id=\"a\" -->\n\n",
    "```bash name=\"go\" cache\necho \"hello $MESHFOX_RO_TEST\"\n```\n",
);

fn unique_dir() -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("meshfox-read-only-cmd-test-{nanos}-{n}"));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn chmod(path: &Path, mode: u32) {
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
}

/// Root ignores permission bits, so there's nothing read-only to test.
fn running_as_root() -> bool {
    // SAFETY: `geteuid` takes no arguments and can't fail.
    unsafe { libc::geteuid() == 0 }
}

/// A canvas in its own directory, with a local config that sets an env var,
/// then made unwritable the way `mode` says (the directory, or just the file).
fn read_only_canvas(mode: Mode) -> (PathBuf, PathBuf) {
    let dir = unique_dir();
    let canvas = dir.join("doc.canvas.md");
    std::fs::write(&canvas, CANVAS).unwrap();
    std::fs::create_dir_all(dir.join(".meshfox")).unwrap();
    std::fs::write(
        dir.join(".meshfox/config.toml"),
        "[process_env]\nMESHFOX_RO_TEST = \"from-local-config\"\n",
    )
    .unwrap();
    match mode {
        Mode::Dir => {
            // `.meshfox/` exists but can't be written, like its parent.
            chmod(&dir.join(".meshfox"), 0o555);
            chmod(&dir, 0o555);
        }
        Mode::File => chmod(&canvas, 0o444),
    }
    (dir, canvas)
}

#[derive(Clone, Copy)]
enum Mode {
    Dir,
    File,
}

fn cleanup(dir: &Path) {
    chmod(&dir.join(".meshfox"), 0o755);
    chmod(dir, 0o755);
    std::fs::remove_dir_all(dir).ok();
}

fn meshfox(canvas: &Path, args: &[&str]) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_meshfox"));
    // An isolated `HOME`, and no external coordinator: the machine's own
    // `~/.meshfox/config.toml` must not decide where this goes.
    cmd.env("HOME", unique_dir())
        .env("MESHFOX_SERVER_SOCKET", "");
    cmd.arg(canvas).args(args);
    cmd.output().expect("run meshfox")
}

fn text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

fn assert_runs_and_refuses_edits(mode: Mode) {
    let (dir, canvas) = read_only_canvas(mode);
    let before: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name())
        .collect();

    let run = meshfox(&canvas, &["run", "a", "go"]);
    let out = text(&run);
    assert!(run.status.success(), "run failed: {out}");
    assert!(
        out.contains("hello from-local-config"),
        "the local config must still be read in a read-only directory: {out}"
    );

    let edit = meshfox(&canvas, &["node", "add", "root", "New node"]);
    let out = text(&edit);
    assert!(!edit.status.success(), "an edit must fail: {out}");
    assert!(
        out.contains("read-only"),
        "the refusal should say why: {out}"
    );

    assert_eq!(
        std::fs::read_to_string(&canvas).unwrap(),
        CANVAS,
        "the canvas was changed"
    );
    let after: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name())
        .collect();
    assert_eq!(before, after, "files appeared next to a read-only canvas");
    assert_eq!(
        std::fs::read_dir(dir.join(".meshfox")).unwrap().count(),
        1,
        "state was written into .meshfox/ (only config.toml belongs there)"
    );
    cleanup(&dir);
}

#[test]
fn an_unwritable_directory_runs_blocks_and_refuses_edits() {
    if running_as_root() {
        return;
    }
    assert_runs_and_refuses_edits(Mode::Dir);
}

#[test]
fn an_unwritable_file_runs_blocks_and_refuses_edits() {
    if running_as_root() {
        return;
    }
    assert_runs_and_refuses_edits(Mode::File);
}
