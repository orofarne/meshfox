//! End-to-end test for `meshfox env`: `list` shows a live canvas's
//! environments, `rm` deletes them (all, one named, one kind) and never
//! touches run history or variable caches.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn unique_dir(tag: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir =
        std::env::temp_dir().join(format!("meshfox-env-cmd-{tag}-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn env(root: &Path, args: &[&str]) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_meshfox"));
    // Isolated from the developer's own config.
    cmd.env("HOME", root.join("home"));
    cmd.env("MESHFOX_SERVER_SOCKET", "");
    cmd.arg("env").args(args);
    cmd.output().unwrap()
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn project(root: &Path) -> PathBuf {
    let canvas = root.join("here.canvas.md");
    std::fs::write(&canvas, "<!-- meshfox:canvas -->\n# Root\n").unwrap();
    let state = root.join(".meshfox");
    std::fs::create_dir_all(state.join("here.canvas.md.venv/bin")).unwrap();
    std::fs::write(state.join("here.canvas.md.venv/bin/python3"), "x").unwrap();
    std::fs::create_dir_all(state.join("here.canvas.md.venv/.named/xls")).unwrap();
    std::fs::create_dir_all(state.join("here.canvas.md.node/.named/web")).unwrap();
    std::fs::write(state.join("here.canvas.md.session.sqlite3"), "history").unwrap();
    std::fs::write(state.join("here.canvas.md.env"), "SECRET=1").unwrap();
    canvas
}

#[test]
fn lists_the_environments_of_a_live_canvas() {
    let root = unique_dir("list");
    let canvas = project(&root);

    let out = env(&root, &["list", root.to_str().unwrap()]);
    assert!(out.status.success(), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains("here.canvas.md  python"), "{text}");
    assert!(text.contains("python name=xls"), "{text}");
    assert!(text.contains("node name=web"), "{text}");

    let one = env(&root, &["list", canvas.to_str().unwrap()]);
    assert_eq!(stdout(&one), text);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn rm_removes_only_what_is_asked_and_keeps_history() {
    let root = unique_dir("rm");
    let canvas = project(&root);
    let canvas = canvas.to_str().unwrap();
    let state = root.join(".meshfox");

    let out = env(&root, &["rm", canvas, "--name", "xls"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stdout(&out).contains("--fresh"), "{}", stdout(&out));
    assert!(!state.join("here.canvas.md.venv/.named/xls").exists());
    assert!(state.join("here.canvas.md.venv/bin/python3").exists());

    let out = env(&root, &["rm", canvas, "--python"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(!state.join("here.canvas.md.venv").exists());
    assert!(state.join("here.canvas.md.node").exists());

    let out = env(&root, &["rm", canvas]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(!state.join("here.canvas.md.node").exists());
    assert!(state.join("here.canvas.md.session.sqlite3").exists());
    assert!(state.join("here.canvas.md.env").exists());

    let out = env(&root, &["rm", canvas]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("no environment"), "{}", stderr(&out));
    let _ = std::fs::remove_dir_all(&root);
}
