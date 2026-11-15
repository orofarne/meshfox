//! End-to-end: a `meshfox:var content="node/block"` hands a block the text of
//! another fence through `env=`, and a value too large for the process
//! environment fails the run with an error that names the cause.

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn unique_dir() -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("meshfox-content-var-cmd-test-{nanos}-{n}"));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn run(canvas: &std::path::Path, node: &str, block: &str) -> std::process::Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_meshfox"));
    // Same isolation from the machine's real `~/.meshfox` as the other `run` suites.
    cmd.env("HOME", unique_dir());
    cmd.arg("run")
        .arg("--canvas")
        .arg(canvas)
        .arg(node)
        .arg(block)
        .output()
        .unwrap()
}

fn canvas_with(query: &str) -> String {
    format!(
        concat!(
            "<!-- meshfox:canvas -->\n# Content var\n<!-- meshfox:node id=\"root\" -->\n\n",
            "<!-- meshfox:var name=\"QUERY\" content=\"queries/get-users\" -->\n\n",
            "## Queries\n<!-- meshfox:node id=\"queries\" -->\n\n",
            "```sql name=\"get-users\"\n{query}\n```\n\n",
            "## Use\n<!-- meshfox:node id=\"use\" -->\n\n",
            "```bash name=\"show\" env=\"QUERY\"\nprintf '%s' \"$QUERY\" | wc -c\nprintf '%s\\n' \"$QUERY\" | head -n1\n```\n",
        ),
        query = query
    )
}

#[test]
fn a_block_receives_the_text_of_another_fence_in_its_environment() {
    let dir = unique_dir();
    let path = dir.join("c.canvas.md");
    std::fs::write(&path, canvas_with("SELECT *\nFROM users")).unwrap();

    let out = run(&path, "use", "show");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "stderr: {stderr}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("19"), "stdout: {stdout}"); // "SELECT *\nFROM users"
    assert!(stdout.contains("SELECT *"), "stdout: {stdout}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_missing_content_fence_fails_the_run_naming_the_variable_and_the_fence() {
    let dir = unique_dir();
    let path = dir.join("c.canvas.md");
    std::fs::write(
        &path,
        canvas_with("SELECT 1").replace("name=\"get-users\"", "name=\"other\""),
    )
    .unwrap();

    let out = run(&path, "use", "show");
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("QUERY") && stderr.contains("no fence named \"get-users\""),
        "stderr: {stderr}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// The environment of a process is bounded (a single string by `MAX_ARG_STRLEN`
/// ~128 KiB on Linux, the whole block by `ARG_MAX` ~1 MiB on macOS), so a very
/// large fence cannot be passed through `env=`. This pins the symptom: the run
/// must fail, and the failure must say why instead of a bare "spawn failed".
#[test]
fn a_fence_too_large_for_the_process_environment_fails_with_a_clear_error() {
    let dir = unique_dir();
    let path = dir.join("c.canvas.md");
    // 8 MiB of text, over every platform's limit.
    let big = "SELECT 1 -- padding padding padding padding padding padding\n".repeat(140_000);
    std::fs::write(&path, canvas_with(big.trim_end())).unwrap();

    let out = run(&path, "use", "show");
    assert!(!out.status.success(), "a huge value unexpectedly ran");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("too large") && stderr.contains("QUERY"),
        "the error should name the variable and say it is too large for the environment; stderr: {stderr}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
