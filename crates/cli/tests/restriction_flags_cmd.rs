//! End-to-end: `--read-only` and `--no-exec` (or `MESHFOX_READ_ONLY` /
//! `MESHFOX_NO_EXEC`) restrict the core a command starts, whatever the
//! filesystem allows. `--read-only` serves a *writable* canvas without
//! touching it, and any number of such cores run side by side; `--no-exec`
//! keeps editing and refuses every kind of running.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

const CANVAS: &str = concat!(
    "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
    "## A\n<!-- meshfox:node id=\"a\" -->\n\n",
    "```bash name=\"go\" cache\necho hello\nsleep 1\n```\n\n",
    "```bash name=\"srv\" service\nsleep 30\n```\n",
);

fn unique_dir() -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("meshfox-restriction-cmd-test-{nanos}-{n}"));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn writable_canvas() -> (PathBuf, PathBuf) {
    let dir = unique_dir();
    let canvas = dir.join("doc.canvas.md");
    std::fs::write(&canvas, CANVAS).unwrap();
    (dir, canvas)
}

fn command(canvas: &Path, args: &[&str]) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_meshfox"));
    // An isolated `HOME`, and no external coordinator: the machine's own
    // `~/.meshfox/config.toml` must not decide where this goes.
    cmd.env("HOME", unique_dir())
        .env("MESHFOX_SERVER_SOCKET", "")
        .env_remove("MESHFOX_READ_ONLY")
        .env_remove("MESHFOX_NO_EXEC");
    cmd.arg(canvas).args(args);
    cmd
}

fn meshfox(canvas: &Path, args: &[&str]) -> Output {
    command(canvas, args).output().expect("run meshfox")
}

fn text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

fn listing(dir: &Path) -> Vec<std::ffi::OsString> {
    let mut names: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name())
        .collect();
    names.sort();
    names
}

#[test]
fn read_only_serves_a_writable_canvas_without_touching_it() {
    let (dir, canvas) = writable_canvas();
    let before = listing(&dir);

    let run = meshfox(&canvas, &["run", "--read-only", "a", "go"]);
    let out = text(&run);
    assert!(run.status.success(), "run failed: {out}");
    assert!(out.contains("hello"), "the block should have run: {out}");

    assert_eq!(std::fs::read_to_string(&canvas).unwrap(), CANVAS);
    assert_eq!(listing(&dir), before, "state appeared next to the canvas");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn read_only_refuses_edits_and_says_why() {
    let (dir, canvas) = writable_canvas();
    let edit = meshfox(&canvas, &["node", "add", "root", "New node", "--read-only"]);
    let out = text(&edit);
    assert!(!edit.status.success(), "an edit must fail: {out}");
    assert!(
        out.contains("read-only"),
        "the refusal should say why: {out}"
    );
    assert_eq!(std::fs::read_to_string(&canvas).unwrap(), CANVAS);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn read_only_cores_run_side_by_side_and_beside_a_normal_one() {
    let (dir, canvas) = writable_canvas();
    // Each `go` sleeps a second, so all three overlap: the normal core holds
    // the canvas's lock the whole time and still doesn't make the read-only
    // ones wait for it or join it.
    let children: Vec<_> = [
        &["run", "--read-only", "a", "go"][..],
        &["run", "--read-only", "a", "go"][..],
        &["run", "a", "go"][..],
    ]
    .iter()
    .map(|args| {
        command(&canvas, args)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("spawn meshfox")
    })
    .collect();
    for child in children {
        let out = child.wait_with_output().unwrap();
        assert!(out.status.success(), "run failed: {}", text(&out));
    }
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn read_only_never_starts_a_service() {
    let (dir, canvas) = writable_canvas();
    let run = meshfox(&canvas, &["run", "--read-only", "a", "srv"]);
    let out = text(&run);
    assert!(!run.status.success(), "a service must be refused: {out}");
    assert!(out.contains("service"), "the refusal should say why: {out}");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn no_exec_refuses_to_run_by_flag_and_by_environment() {
    let (dir, canvas) = writable_canvas();

    let by_flag = meshfox(&canvas, &["run", "--no-exec", "a", "go"]);
    let out = text(&by_flag);
    assert!(!by_flag.status.success(), "run must be refused: {out}");
    assert!(out.contains("no-exec"), "the refusal should say why: {out}");

    let by_env = command(&canvas, &["run", "a", "go"])
        .env("MESHFOX_NO_EXEC", "1")
        .output()
        .unwrap();
    assert!(
        !by_env.status.success(),
        "run must be refused: {}",
        text(&by_env)
    );

    assert_eq!(std::fs::read_to_string(&canvas).unwrap(), CANVAS);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn no_exec_still_edits_the_canvas() {
    let (dir, canvas) = writable_canvas();
    let edit = meshfox(&canvas, &["node", "add", "root", "New node", "--no-exec"]);
    let out = text(&edit);
    assert!(edit.status.success(), "editing must keep working: {out}");
    assert!(
        std::fs::read_to_string(&canvas)
            .unwrap()
            .contains("New node"),
        "the node was not added"
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn read_only_refuses_a_detached_run() {
    let (dir, canvas) = writable_canvas();
    let run = meshfox(&canvas, &["run", "--read-only", "--detach", "a", "go"]);
    let out = text(&run);
    assert_eq!(run.status.code(), Some(2), "must be a usage error: {out}");
    assert!(out.contains("detach"), "the refusal should say why: {out}");
    assert_eq!(
        listing(&dir),
        vec![std::ffi::OsString::from("doc.canvas.md")]
    );
    std::fs::remove_dir_all(&dir).ok();
}
