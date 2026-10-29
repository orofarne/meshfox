use serde_json::Value;
use std::{
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

struct Process(Child);
impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn command(dir: &Path) -> Command {
    #[cfg(target_os = "macos")]
    {
        static SIGN: std::sync::Once = std::sync::Once::new();
        SIGN.call_once(|| {
            assert!(Command::new(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../scripts/run-signed.sh"
            ))
            .args(["--sign-only", env!("CARGO_BIN_EXE_meshfox")])
            .status()
            .unwrap()
            .success())
        });
    }
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_meshfox"));
    cmd.current_dir(dir).env("HOME", dir).env(
        "MESHFOX_SERVER_SOCKET",
        std::fs::read_to_string(dir.join("coordinator-socket")).unwrap_or_default(),
    );
    cmd
}
/// A test's scratch directory, removed when the test ends however it ends.
struct TestDir(PathBuf);
impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
impl std::ops::Deref for TestDir {
    type Target = Path;
    fn deref(&self) -> &Path {
        &self.0
    }
}

fn setup(body: &str) -> (TestDir, PathBuf) {
    let dir = std::env::temp_dir().join(format!("meshfox-status-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(dir.join(".meshfox")).unwrap();
    let canvas = dir.join("test.md");
    std::fs::write(
        &canvas,
        format!("<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n\n{body}"),
    )
    .unwrap();
    (TestDir(dir), canvas)
}
/// The canvas goes first, as in `meshfox README.md status`.
fn invoke(dir: &Path, canvas: &Path, args: &[&str]) -> std::process::Output {
    command(dir).arg(canvas).args(args).output().unwrap()
}
fn json(output: &std::process::Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|e| {
        panic!(
            "{e}: {}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}
fn step<'a>(snapshot: &'a Value, block: &str) -> &'a Value {
    snapshot["steps"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["block"] == block)
        .unwrap_or_else(|| panic!("no step {block} in {snapshot}"))
}

#[test]
fn a_failed_chain_lists_each_step_its_failure_and_exits_1() {
    let (dir, canvas) = setup("```sh name=dep\necho broken-dependency\nexit 4\n```\n\n```sh name=target deps=dep\necho UNREACHABLE\n```\n");
    assert!(!invoke(&dir, &canvas, &["run", "target"]).status.success());

    let human = invoke(&dir, &canvas, &["status"]);
    assert_eq!(human.status.code(), Some(1));
    let text = String::from_utf8_lossy(&human.stdout);
    assert!(text.contains("root/dep"), "{text}");
    assert!(text.contains("failed"), "{text}");
    assert!(text.contains("not-started"), "{text}");
    assert!(text.contains("Failed: root/dep"), "{text}");
    assert!(text.contains("broken-dependency"), "{text}");

    let output = invoke(&dir, &canvas, &["status", "--json"]);
    assert_eq!(output.status.code(), Some(1));
    let snapshot = json(&output);
    assert_eq!(snapshot["type"], "snapshot");
    assert_eq!(snapshot["run"]["status"], "failed");
    assert_eq!(step(&snapshot, "dep")["state"], "failed");
    assert_eq!(step(&snapshot, "dep")["exitCode"], 4);
    assert_eq!(step(&snapshot, "target")["state"], "not-started");
    assert_eq!(snapshot["failures"][0]["step"], "root/dep");
    assert!(snapshot["failures"][0]["tail"]
        .to_string()
        .contains("broken-dependency"));
    assert!(!String::from_utf8_lossy(&output.stdout).contains('\x1b'));
}

#[test]
fn test_totals_are_recognised_and_a_succeeded_chain_exits_0() {
    let (dir, canvas) = setup(
        "```sh name=unit\necho 'test result: ok. 5 passed; 0 failed; 1 ignored; 0 measured'\n```\n",
    );
    assert!(invoke(&dir, &canvas, &["run", "unit"]).status.success());
    let output = invoke(&dir, &canvas, &["status", "--json"]);
    assert_eq!(output.status.code(), Some(0));
    let snapshot = json(&output);
    assert_eq!(snapshot["run"]["status"], "succeeded");
    assert_eq!(step(&snapshot, "unit")["state"], "succeeded");
    assert_eq!(step(&snapshot, "unit")["tests"]["passed"], 5);
    assert_eq!(step(&snapshot, "unit")["tests"]["ignored"], 1);
    assert!(snapshot["failures"].as_array().unwrap().is_empty());
}

#[test]
fn the_canvas_path_is_accepted_before_or_after_the_subcommand() {
    let (dir, canvas) = setup("```sh name=ok\necho hi\n```\n");
    assert!(invoke(&dir, &canvas, &["run", "ok"]).status.success());
    let leading = invoke(&dir, &canvas, &["status", "--json"]);
    let flag = command(&dir)
        .args(["status", "--json", "--canvas"])
        .arg(&canvas)
        .output()
        .unwrap();
    assert_eq!(leading.status.code(), Some(0));
    assert_eq!(flag.status.code(), Some(0));
    assert_eq!(json(&leading)["run"]["id"], json(&flag)["run"]["id"]);
}

#[test]
fn a_chain_that_cannot_be_read_exits_2() {
    let (dir, canvas) = setup("```sh name=ok\necho hi\n```\n");
    assert_eq!(
        invoke(&dir, &canvas, &["status", "--id", "99999"])
            .status
            .code(),
        Some(2)
    );
}

#[test]
fn watch_reports_each_step_change_and_ends_with_done_and_the_chains_exit_code() {
    let (dir, canvas) = setup(
        "```sh name=slow\necho STARTED\nsleep 2\n```\n\n```sh name=after deps=slow\necho FINISHED\n```\n",
    );
    let mut run = Process(
        command(&dir)
            .arg(&canvas)
            .args(["run", "after"])
            .stdout(Stdio::null())
            .stderr(std::fs::File::create(dir.join("run.stderr")).unwrap())
            .spawn()
            .unwrap(),
    );
    // Let the chain be admitted, then watch it from the middle.
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let listing = invoke(&dir, &canvas, &["logs", "--list"]);
        if String::from_utf8_lossy(&listing.stdout).contains("running") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the chain never started; last `logs --list`: {listing:?}; `run` said: {:?}",
            std::fs::read_to_string(dir.join("run.stderr"))
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    let output = invoke(&dir, &canvas, &["status", "--watch", "--json"]);
    assert_eq!(output.status.code(), Some(0));
    assert!(run.0.wait().unwrap().success());
    let lines: Vec<Value> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(lines[0]["type"], "snapshot", "{lines:?}");
    let last = lines.last().unwrap();
    assert_eq!(last["type"], "done", "{lines:?}");
    assert_eq!(last["exitCode"], 0);
    assert_eq!(last["run"]["status"], "succeeded");
    assert!(
        lines.iter().any(|l| l["type"] == "step"
            && l["step"]["block"] == "after"
            && l["step"]["state"] == "succeeded"),
        "{lines:?}"
    );
}
