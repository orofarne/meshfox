//! `meshfox run --skip-fresh`, end to end through the real binary: the block
//! named on the command line is held to the same session-freshness check a
//! pulled-in dependency gets — skipped when it already ran and nothing it
//! depends on changed, run for real otherwise. Without the flag, a block
//! requested by name always runs.

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
    let dir = std::env::temp_dir().join(format!("meshfox-run-skip-fresh-cmd-test-{nanos}-{n}"));
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

/// `dep`, `target` (depends on `dep`) and `forever` (`always`) each append
/// one line to their own counter file every time they really run.
fn canvas_source(dep_marker: &str) -> String {
    format!(
        concat!(
            "<!-- meshfox:canvas -->\n# SkipFresh\n<!-- meshfox:node id=\"root\" -->\n\n",
            "```bash name=\"dep\"\necho ran >> dep.txt # {marker}\n```\n\n",
            "```bash name=\"target\" deps=\"dep\"\necho ran >> target.txt\n```\n\n",
            "```bash name=\"forever\" always\necho ran >> forever.txt\n```\n",
        ),
        marker = dep_marker,
    )
}

fn write_canvas(dir: &Path) -> PathBuf {
    let path = dir.join("skip-fresh.canvas.md");
    std::fs::write(&path, canvas_source("v1")).unwrap();
    path
}

fn runs(dir: &Path, file: &str) -> usize {
    std::fs::read_to_string(dir.join(file))
        .map(|s| s.lines().count())
        .unwrap_or(0)
}

fn run_block(home: &Path, canvas: &Path, extra: &[&str], block: &str) -> std::process::Output {
    meshfox(home)
        .arg("run")
        .arg("--canvas")
        .arg(canvas)
        .args(extra)
        .arg(block)
        .output()
        .unwrap()
}

fn assert_ok(out: &std::process::Output) {
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn skip_fresh_skips_an_up_to_date_requested_block_and_reruns_it_once_stale() {
    let dir = unique_dir();
    let home = unique_dir();
    let canvas = write_canvas(&dir);

    let out = run_block(&home, &canvas, &["--skip-fresh"], "target");
    assert_ok(&out);
    assert_eq!(runs(&dir, "target.txt"), 1, "no record yet: runs");

    let out = run_block(&home, &canvas, &["--skip-fresh"], "target");
    assert_ok(&out);
    assert_eq!(runs(&dir, "target.txt"), 1, "up to date: skipped");
    assert_eq!(runs(&dir, "dep.txt"), 1, "its dependency is skipped too");

    let out = run_block(&home, &canvas, &[], "target");
    assert_ok(&out);
    assert_eq!(
        runs(&dir, "target.txt"),
        2,
        "without the flag an explicitly requested block always runs"
    );

    let out = run_block(&home, &canvas, &["--skip-fresh"], "target");
    assert_ok(&out);
    assert_eq!(
        runs(&dir, "target.txt"),
        2,
        "the plain run recorded itself: still skippable"
    );

    // A stale dependency cascades into the requested block.
    std::fs::write(&canvas, canvas_source("v2")).unwrap();
    let out = run_block(&home, &canvas, &["--skip-fresh"], "target");
    assert_ok(&out);
    assert_eq!(runs(&dir, "dep.txt"), 2, "edited dependency reruns");
    assert_eq!(
        runs(&dir, "target.txt"),
        3,
        "a block whose dependency ran for real runs too"
    );

    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn skip_fresh_still_runs_an_always_block() {
    let dir = unique_dir();
    let home = unique_dir();
    let canvas = write_canvas(&dir);

    for expected in 1..=2 {
        let out = run_block(&home, &canvas, &["--skip-fresh"], "forever");
        assert_ok(&out);
        assert_eq!(runs(&dir, "forever.txt"), expected, "`always` never skips");
    }

    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn skip_fresh_conflicts_with_fresh() {
    let dir = unique_dir();
    let home = unique_dir();
    let canvas = write_canvas(&dir);
    let out = run_block(&home, &canvas, &["--skip-fresh", "--fresh"], "target");
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("--skip-fresh") && stderr.contains("--fresh"),
        "{stderr}"
    );
    assert_eq!(runs(&dir, "target.txt"), 0, "nothing ran");
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn skip_fresh_notices_a_deleted_output_of_an_install_like_block() {
    let dir = unique_dir();
    let home = unique_dir();
    std::fs::write(dir.join("input.txt"), "payload").unwrap();
    let canvas = dir.join("install.canvas.md");
    std::fs::write(
        &canvas,
        concat!(
            "<!-- meshfox:canvas -->\n# Install\n<!-- meshfox:node id=\"root\" -->\n\n",
            "```bash name=\"install\" inputs=\"input.txt\" outputs=\"installed.txt\"\n",
            "echo ran >> install.txt\ncp input.txt installed.txt\n```\n",
        ),
    )
    .unwrap();

    for (expected, why) in [(1, "first run"), (1, "up to date: skipped")] {
        let out = run_block(&home, &canvas, &["--skip-fresh"], "install");
        assert_ok(&out);
        assert_eq!(runs(&dir, "install.txt"), expected, "{why}");
    }

    std::fs::remove_file(dir.join("installed.txt")).unwrap();
    let out = run_block(&home, &canvas, &["--skip-fresh"], "install");
    assert_ok(&out);
    assert_eq!(runs(&dir, "install.txt"), 2, "a deleted output is stale");

    std::fs::write(dir.join("input.txt"), "changed").unwrap();
    let out = run_block(&home, &canvas, &["--skip-fresh"], "install");
    assert_ok(&out);
    assert_eq!(runs(&dir, "install.txt"), 3, "a changed input is stale");

    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&home);
}
