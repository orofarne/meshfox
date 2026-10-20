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
    cmd.current_dir(dir)
        .env("HOME", dir)
        .env("MESHFOX_SERVER_SOCKET", "");
    cmd
}
fn setup(body: &str) -> (PathBuf, PathBuf) {
    let dir = std::env::temp_dir().join(format!("meshfox-logs-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(dir.join(".meshfox")).unwrap();
    std::fs::write(
        dir.join(".meshfox/config.toml"),
        "[session]\nmax_runs_per_block=1\n",
    )
    .unwrap();
    let canvas = dir.join("test.md");
    std::fs::write(
        &canvas,
        format!("<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n\n{body}"),
    )
    .unwrap();
    (dir, canvas)
}
fn invoke(dir: &Path, canvas: &Path, args: &[&str]) -> std::process::Output {
    command(dir).arg(canvas).args(args).output().unwrap()
}
fn success(dir: &Path, canvas: &Path, args: &[&str]) -> String {
    let output = invoke(dir, canvas, args);
    assert!(
        output.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}
fn wait_for(mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !condition() {
        assert!(Instant::now() < deadline, "timed out");
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn chain_block_selection_and_effective_provenance_survive_restart_rotation_and_edits() {
    let (dir,canvas)=setup("```sh name=dep\ncat version.txt\n```\n\n```sh name=target deps=dep\necho target-output\n```\n");
    std::fs::write(dir.join("version.txt"), "ORIGINAL\n").unwrap();
    success(&dir, &canvas, &["run", "target"]);
    success(&dir, &canvas, &["run", "target"]);
    let listing = success(
        &dir,
        &canvas,
        &["logs", "target", "--chain", "--list", "--json"],
    );
    let runs: Vec<Value> = listing
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(runs.len(), 2);
    let chain_id = runs[0]["run"]["id"].as_i64().unwrap().to_string();
    assert_eq!(runs[0]["run"]["source"], "cli");
    let plain = success(&dir, &canvas, &["logs", "--id", &chain_id]);
    assert!(plain.contains("skipped: already fresh"), "{plain}");
    assert!(plain.contains("target-output"));
    assert!(!plain.contains("ORIGINAL"));
    std::fs::write(dir.join("version.txt"), "NEWER\n").unwrap();
    success(&dir, &canvas, &["run", "--fresh", "dep"]);
    let block = success(&dir, &canvas, &["logs", "dep"]);
    assert!(block.contains("NEWER"));
    assert!(!block.contains("target-output"));
    let effective = success(&dir, &canvas, &["logs", "--id", &chain_id, "--effective"]);
    assert!(effective.contains("ORIGINAL"), "{effective}");
    assert!(effective.contains("using block run #"));
    assert!(!effective.contains("NEWER"));
    let selected = success(
        &dir,
        &canvas,
        &["logs", "--id", &chain_id, "--effective", "--block", "dep"],
    );
    assert!(selected.contains("ORIGINAL"));
    assert!(!selected.contains("target-output"));
    // Historical selection must not depend on the block still existing.
    std::fs::write(
        &canvas,
        "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n",
    )
    .unwrap();
    let historical = success(
        &dir,
        &canvas,
        &["logs", "--id", &chain_id, "--effective", "--block", "dep"],
    );
    assert!(historical.contains("ORIGINAL"));
    let json = success(&dir, &canvas, &["logs", "--id", &chain_id, "--json"]);
    for line in json.lines() {
        let _: Value = serde_json::from_str(line).unwrap();
    }
    assert!(!json.contains("ORIGINAL"));
}

#[test]
fn reading_a_failed_chain_succeeds_and_absent_history_fails() {
    let (dir, canvas) = setup("```sh name=fail\necho failure-output >&2\nexit 7\n```\n");
    assert!(!invoke(&dir, &canvas, &["run", "fail"]).status.success());
    let log = success(&dir, &canvas, &["logs", "fail", "--chain"]);
    assert!(log.contains("[stderr] failure-output"), "{log}");
    assert!(log.contains("failed · exit 7"));
    assert!(!invoke(&dir, &canvas, &["logs", "--id", "99999"])
        .status
        .success());
    assert!(!invoke(&dir, &canvas, &["logs", "does-not-exist"])
        .status
        .success());
}

#[test]
fn follow_replays_then_stops_at_this_invocation_and_ctrl_c_only_detaches() {
    let (dir, canvas) = setup("```sh name=slow\necho BEFORE\nsleep 3\necho AFTER\n```\n");
    let run_log = dir.join("run.log");
    let mut run = Process(
        command(&dir)
            .arg(&canvas)
            .args(["run", "slow"])
            .stdout(std::fs::File::create(&run_log).unwrap())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    wait_for(|| std::fs::read_to_string(&run_log).is_ok_and(|s| s.contains("BEFORE")));
    let follow_log = dir.join("follow.log");
    let mut follow = Process(
        command(&dir)
            .arg(&canvas)
            .args(["logs", "slow", "--follow"])
            .stdout(std::fs::File::create(&follow_log).unwrap())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    wait_for(|| std::fs::read_to_string(&follow_log).is_ok_and(|s| s.contains("BEFORE")));
    let detach_log = dir.join("detach.log");
    let mut detach = Process(
        command(&dir)
            .arg(&canvas)
            .args(["logs", "slow", "--follow"])
            .stdout(std::fs::File::create(&detach_log).unwrap())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    wait_for(|| std::fs::read_to_string(&detach_log).is_ok_and(|s| s.contains("BEFORE")));
    // SAFETY: only our owned logs process is signalled, never the run.
    unsafe {
        libc::kill(detach.0.id() as i32, libc::SIGINT);
    }
    wait_for(|| detach.0.try_wait().unwrap().is_some());
    assert!(run.0.wait().unwrap().success());
    wait_for(|| follow.0.try_wait().unwrap().is_some());
    assert!(follow.0.wait().unwrap().success());
    let log = std::fs::read_to_string(&follow_log).unwrap();
    assert_eq!(log.matches("BEFORE").count(), 1, "{log}");
    assert_eq!(log.matches("AFTER").count(), 1, "{log}");
    assert!(log.contains("succeeded"));
}

#[test]
fn failed_dependency_records_the_target_as_not_started() {
    let (dir,canvas)=setup("```sh name=dep\necho broken-dependency\nexit 4\n```\n\n```sh name=target deps=dep\necho UNREACHABLE\n```\n");
    assert!(!invoke(&dir, &canvas, &["run", "target"]).status.success());
    let chain = success(&dir, &canvas, &["logs", "target", "--chain"]);
    assert!(chain.contains("broken-dependency"), "{chain}");
    assert!(
        chain.contains("not started: chain ended before this step"),
        "{chain}"
    );
    assert!(!chain.contains("UNREACHABLE"));
    let block = success(&dir, &canvas, &["logs", "target"]);
    assert!(block.contains("not started:"), "{block}");
    assert!(!block.contains("broken-dependency"));
}
