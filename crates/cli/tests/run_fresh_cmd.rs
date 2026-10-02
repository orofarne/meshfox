//! `meshfox run --fresh` and `meshfox session reset`, end to end through the
//! real binary: a dependency that already ran and looks unchanged is skipped
//! by a later `run` (the session survives between invocations), `--fresh`
//! forces one run of it without forgetting anything, `session reset` makes
//! the next plain run execute it again.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn unique_dir() -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("meshfox-run-fresh-cmd-test-{nanos}-{n}"));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn meshfox(home: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_meshfox"));
    // No real `~/.meshfox/config.toml` (and no external coordinator) may
    // leak in — see `run_cmd.rs`.
    cmd.env("HOME", home).env("MESHFOX_SERVER_SOCKET", "");
    cmd
}

/// A canvas whose `dep` block appends a line to `count.txt` every time it
/// really runs, so the number of lines is the number of real runs.
fn write_canvas(dir: &Path) -> PathBuf {
    let path = dir.join("fresh.canvas.md");
    std::fs::write(
        &path,
        concat!(
            "<!-- meshfox:canvas -->\n# Fresh\n<!-- meshfox:node id=\"root\" -->\n\n",
            "```bash name=\"dep\"\necho ran >> count.txt\n```\n\n",
            "```bash name=\"target\" deps=\"dep\"\ntrue\n```\n",
        ),
    )
    .unwrap();
    path
}

fn runs(dir: &Path) -> usize {
    std::fs::read_to_string(dir.join("count.txt"))
        .map(|s| s.lines().count())
        .unwrap_or(0)
}

fn run_target(home: &Path, canvas: &Path, extra: &[&str]) -> std::process::Output {
    meshfox(home)
        .arg("run")
        .arg("--canvas")
        .arg(canvas)
        .args(extra)
        .arg("target")
        .output()
        .unwrap()
}

#[test]
fn fresh_reruns_a_skippable_dependency_once_and_session_reset_makes_every_run_fresh() {
    let dir = unique_dir();
    let home = unique_dir();
    let canvas = write_canvas(&dir);

    let out = run_target(&home, &canvas, &[]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(runs(&dir), 1, "first run: dep runs");

    let out = run_target(&home, &canvas, &[]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(runs(&dir), 1, "second run: dep already fresh, skipped");

    let out = run_target(&home, &canvas, &["--fresh"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(runs(&dir), 2, "--fresh: dep runs for real this once");

    let out = run_target(&home, &canvas, &[]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(runs(&dir), 2, "--fresh forgot nothing: the next plain run skips again");

    let out = meshfox(&home)
        .arg("session")
        .arg("reset")
        .arg("--canvas")
        .arg(&canvas)
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("session reset"),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );

    let out = run_target(&home, &canvas, &[]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(runs(&dir), 3, "after `session reset` the dependency runs again");

    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn fresh_conflicts_with_no_deps() {
    let dir = unique_dir();
    let home = unique_dir();
    let canvas = write_canvas(&dir);
    let out = run_target(&home, &canvas, &["--fresh", "--no-deps"]);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("--fresh") && stderr.contains("--no-deps"), "{stderr}");
    assert_eq!(runs(&dir), 0, "nothing ran");
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&home);
}
