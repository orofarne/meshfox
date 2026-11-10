//! End-to-end test for `meshfox mv`: a file or a directory moves together with
//! its `.meshfox/` state, nothing is overwritten, `--dry-run` changes nothing.
//! (Secrets in the system store are covered by the unit tests in
//! `meshfox_core::relocate`, with an in-memory backend.)

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn unique_dir(tag: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("meshfox-mv-cmd-{tag}-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir.canonicalize().unwrap()
}

fn mv(home: &Path, args: &[&Path], flags: &[&str]) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_meshfox"));
    // Isolated from the developer's own config, secret index and daemon.
    cmd.env("HOME", home);
    cmd.env("MESHFOX_SERVER_SOCKET", "");
    cmd.arg("mv").args(flags).args(args);
    cmd.output().unwrap()
}

fn text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

fn canvas(path: &Path, body: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, format!("<!-- meshfox:canvas -->\n# Root\n{body}")).unwrap();
}

fn state(dir: &Path, name: &str) {
    let meshfox = dir.join(".meshfox");
    std::fs::create_dir_all(meshfox.join(format!("{name}.venv/bin"))).unwrap();
    std::fs::write(meshfox.join(format!("{name}.env")), "A=1").unwrap();
    std::fs::write(meshfox.join(format!("{name}.session.sqlite3")), "history").unwrap();
}

#[test]
fn a_renamed_file_takes_its_state_and_dry_run_changes_nothing() {
    let home = unique_dir("home");
    let dir = unique_dir("file");
    canvas(&dir.join("a.canvas.md"), "");
    canvas(&dir.join("user.canvas.md"), "\n[a](a.canvas.md)\n");
    state(&dir, "a.canvas.md");
    let (src, dst) = (dir.join("a.canvas.md"), dir.join("b.canvas.md"));

    let dry = mv(&home, &[&src, &dst], &["--dry-run"]);
    let out = text(&dry);
    assert!(dry.status.success(), "{out}");
    assert!(out.contains("dry run"), "{out}");
    assert!(out.contains("user.canvas.md:4"), "{out}");
    assert!(src.exists() && !dst.exists());
    assert!(dir.join(".meshfox/a.canvas.md.env").exists());

    let real = mv(&home, &[&src, &dst], &[]);
    let out = text(&real);
    assert!(real.status.success(), "{out}");
    assert!(!src.exists() && dst.exists());
    let meshfox = dir.join(".meshfox");
    assert_eq!(
        std::fs::read_to_string(meshfox.join("b.canvas.md.env")).unwrap(),
        "A=1"
    );
    assert!(meshfox.join("b.canvas.md.session.sqlite3").exists());
    assert!(!meshfox.join("a.canvas.md.env").exists());
    assert!(!meshfox.join("a.canvas.md.venv").exists());
    assert!(!meshfox.join("b.canvas.md.venv").exists());
    // The reference in the other canvas was reported, not rewritten.
    let user = std::fs::read_to_string(dir.join("user.canvas.md")).unwrap();
    assert!(user.contains("[a](a.canvas.md)"), "{user}");
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn a_directory_moves_whole_with_its_meshfox_folder() {
    let home = unique_dir("home");
    let dir = unique_dir("dir");
    canvas(&dir.join("proj/a.canvas.md"), "");
    canvas(&dir.join("proj/sub/b.canvas.md"), "");
    state(&dir.join("proj"), "a.canvas.md");
    state(&dir.join("proj/sub"), "b.canvas.md");
    let dst = dir.join("moved");

    let out = mv(&home, &[&dir.join("proj"), &dst], &[]);
    assert!(out.status.success(), "{}", text(&out));
    assert!(!dir.join("proj").exists());
    assert!(dst.join("a.canvas.md").exists());
    assert_eq!(
        std::fs::read_to_string(dst.join(".meshfox/a.canvas.md.env")).unwrap(),
        "A=1"
    );
    assert!(dst
        .join("sub/.meshfox/b.canvas.md.session.sqlite3")
        .exists());
    assert!(!dst.join(".meshfox/a.canvas.md.venv").exists());
    assert!(!dst.join("sub/.meshfox/b.canvas.md.venv").exists());
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn nothing_is_overwritten_and_a_missing_source_fails() {
    let home = unique_dir("home");
    let dir = unique_dir("refuse");
    canvas(&dir.join("a.canvas.md"), "");
    canvas(&dir.join("b.canvas.md"), "B");

    let out = mv(
        &home,
        &[&dir.join("a.canvas.md"), &dir.join("b.canvas.md")],
        &[],
    );
    assert!(!out.status.success());
    assert!(text(&out).contains("already exists"), "{}", text(&out));
    assert!(std::fs::read_to_string(dir.join("b.canvas.md"))
        .unwrap()
        .ends_with('B'));

    let out = mv(
        &home,
        &[&dir.join("none.canvas.md"), &dir.join("c.canvas.md")],
        &[],
    );
    assert!(!out.status.success());
    assert!(text(&out).starts_with("meshfox mv:"), "{}", text(&out));
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&home);
}
