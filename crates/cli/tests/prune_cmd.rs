//! End-to-end test for `meshfox prune`: dry run by default, `--yes` to remove,
//! only regenerable orphans, never run history or variable caches.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn unique_dir(tag: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "meshfox-prune-cmd-{tag}-{}-{n}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn prune(root: &Path, temp: &Path, args: &[&str]) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_meshfox"));
    // Isolated from the developer's own config, and pointed at an empty temp
    // dir so the stale-temp scan never looks at the real one.
    cmd.env("HOME", temp.join("home"));
    cmd.env("MESHFOX_SERVER_SOCKET", "");
    cmd.env("TMPDIR", temp);
    cmd.arg("prune").arg(root).args(args);
    cmd.output().unwrap()
}

#[test]
fn lists_by_default_and_removes_only_orphans_with_yes() {
    let root = unique_dir("root");
    let temp = unique_dir("temp");
    let meshfox = root.join(".meshfox");
    std::fs::create_dir_all(meshfox.join("gone.canvas.md.venv/bin")).unwrap();
    std::fs::write(meshfox.join("gone.canvas.md.venv/bin/python3"), "x").unwrap();
    std::fs::write(
        root.join("here.canvas.md"),
        "<!-- meshfox:canvas -->\n# Root\n",
    )
    .unwrap();
    std::fs::create_dir_all(meshfox.join("here.canvas.md.venv")).unwrap();
    std::fs::write(meshfox.join("gone.canvas.md.session.sqlite3"), "history").unwrap();
    std::fs::write(meshfox.join("gone.canvas.md.env"), "SECRET=1").unwrap();
    let tangled = root.join(".meshfox-tangle-old.canvas.md__b.py");
    std::fs::write(&tangled, "x").unwrap();

    let dry = prune(&root, &temp, &[]);
    let out = String::from_utf8_lossy(&dry.stdout);
    assert!(
        dry.status.success(),
        "{out}{}",
        String::from_utf8_lossy(&dry.stderr)
    );
    assert!(out.contains(".meshfox/gone.canvas.md.venv"), "{out}");
    assert!(out.contains(".meshfox-tangle-old.canvas.md__b.py"), "{out}");
    assert!(!out.contains("here.canvas.md.venv"), "{out}");
    assert!(out.contains("--yes"), "{out}");
    assert!(
        meshfox.join("gone.canvas.md.venv").exists(),
        "a dry run removes nothing"
    );
    assert!(tangled.exists());

    let done = prune(&root, &temp, &["--yes"]);
    let out = String::from_utf8_lossy(&done.stdout);
    assert!(
        done.status.success(),
        "{out}{}",
        String::from_utf8_lossy(&done.stderr)
    );
    assert!(out.contains("removed 2 of 2"), "{out}");
    assert!(!meshfox.join("gone.canvas.md.venv").exists());
    assert!(!tangled.exists());
    assert!(
        meshfox.join("here.canvas.md.venv").exists(),
        "its canvas exists"
    );
    assert!(
        meshfox.join("gone.canvas.md.session.sqlite3").exists(),
        "history is never pruned"
    );
    assert!(
        meshfox.join("gone.canvas.md.env").exists(),
        "variable caches are never pruned"
    );

    let again = prune(&root, &temp, &[]);
    assert!(String::from_utf8_lossy(&again.stdout).contains("nothing to prune"));
    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_dir_all(&temp);
}

#[test]
fn a_directory_that_does_not_exist_is_an_error() {
    let temp = unique_dir("temp-missing");
    let out = prune(&temp.join("nope"), &temp, &[]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("isn't a directory"));
    let _ = std::fs::remove_dir_all(&temp);
}
