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
fn setup(body: &str, keep: usize) -> (PathBuf, PathBuf) {
    let dir = std::env::temp_dir().join(format!("meshfox-stats-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(dir.join(".meshfox")).unwrap();
    std::fs::write(
        dir.join(".meshfox/config.toml"),
        format!("[session]\nmax_chain_runs={keep}\n"),
    )
    .unwrap();
    let canvas = dir.join("test.canvas.md");
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
fn stats(dir: &Path, canvas: &Path, args: &[&str]) -> Value {
    let output = command(dir)
        .arg(canvas)
        .arg("stats")
        .args(args)
        .arg("--json")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}
fn block<'a>(report: &'a Value, name: &str) -> &'a Value {
    report["blocks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["block"] == name)
        .unwrap()
}
#[test]
fn saved_chain_and_block_statistics_separate_skips_failures_and_selection() {
    let (dir,canvas)=setup("```sh name=dep\necho dep-output\n```\n\n```sh name=target deps=dep\necho target-output\n```\n\n```sh name=fail deps=dep\nexit 7\n```\n",100);
    for target in ["target", "target", "fail"] {
        let output = invoke(&dir, &canvas, &["run", target]);
        assert_eq!(output.status.success(), target != "fail");
    }
    let all = stats(&dir, &canvas, &[]);
    assert_eq!(all["scope"], "canvas");
    assert_eq!(all["coverage"]["retainedChains"], 3);
    assert_eq!(all["chains"]["succeeded"], 2);
    assert_eq!(all["chains"]["failed"], 1);
    assert_eq!(all["chainDuration"]["samples"], 3);
    assert_eq!(block(&all, "dep")["outcomes"]["succeeded"], 1);
    assert_eq!(block(&all, "dep")["skipped"], 2);
    assert_eq!(block(&all, "dep")["duration"]["samples"], 1);
    let dep = stats(&dir, &canvas, &["dep"]);
    assert_eq!(dep["scope"], "block");
    assert_eq!(dep["blocks"].as_array().unwrap().len(), 1);
    assert_eq!(dep["blocks"][0]["successRatePercent"], 100.0);
    let chain = stats(&dir, &canvas, &["target", "--chain"]);
    assert_eq!(chain["coverage"]["matchingChains"], 2);
    assert_eq!(block(&chain, "dep")["skipped"], 1);
    assert!(!chain["blocks"]
        .as_array()
        .unwrap()
        .iter()
        .any(|b| b["block"] == "fail"));
    let before = stats(&dir, &canvas, &["--since", "2000-01-01T00:00:00Z"]);
    assert_eq!(before["coverage"]["matchingChains"], 3);
    let future = stats(&dir, &canvas, &["--since", "2100-01-01T00:00:00Z"]);
    assert_eq!(future["coverage"]["retainedChains"], 3);
    assert_eq!(future["coverage"]["matchingChains"], 0);
    assert!(future["chainDuration"]["medianMs"].is_null());
    let missing = stats(&dir, &canvas, &["absent"]);
    assert_eq!(missing["coverage"]["matchingChains"], 0);
    let invalid = invoke(&dir, &canvas, &["stats", "--since", "9223372036854775807w"]);
    assert!(!invalid.status.success());
    assert!(String::from_utf8_lossy(&invalid.stderr).contains("RFC3339"));
}
#[test]
fn coverage_is_limited_by_retention_and_reading_stats_executes_nothing() {
    let (dir, canvas) = setup("```sh name=counter\necho ran >> counter.txt\n```\n", 2);
    for _ in 0..3 {
        assert!(invoke(&dir, &canvas, &["run", "counter"]).status.success());
    }
    let report = stats(&dir, &canvas, &["--since", "2000-01-01T00:00:00Z"]);
    assert_eq!(report["coverage"]["retainedChains"], 2);
    assert_eq!(report["coverage"]["matchingChains"], 2);
    assert_eq!(report["chains"]["succeeded"], 2);
    assert!(report["coverage"]["retainedFrom"].is_string());
    assert_eq!(
        std::fs::read_to_string(dir.join("counter.txt"))
            .unwrap()
            .lines()
            .count(),
        3
    );
    let text = invoke(&dir, &canvas, &["stats"]);
    let text = String::from_utf8(text.stdout).unwrap();
    assert!(text.contains("Retained history: 2 chains"), "{text}");
    assert!(text.contains("limited to retained journals"), "{text}");
    assert!(!text.contains('\x1b'));
    assert_eq!(
        std::fs::read_to_string(dir.join("counter.txt"))
            .unwrap()
            .lines()
            .count(),
        3
    );
}
fn wait_for(mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !condition() {
        assert!(Instant::now() < deadline, "timed out");
        std::thread::sleep(Duration::from_millis(50));
    }
}
#[test]
fn active_invocation_is_reported_separately_without_duration_samples() {
    let (dir,canvas)=setup("```sh name=wait\necho ready\nwhile [ ! -f release ]; do sleep 0.05; done\necho finished\n```\n",100);
    struct Release(PathBuf);
    impl Drop for Release {
        fn drop(&mut self) {
            let _ = std::fs::write(&self.0, "");
        }
    }
    let release = Release(dir.join("release"));
    let log = dir.join("run.log");
    let mut run = Process(
        command(&dir)
            .arg(&canvas)
            .args(["run", "wait"])
            .stdout(std::fs::File::create(&log).unwrap())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    wait_for(|| std::fs::read_to_string(&log).is_ok_and(|s| s.contains("ready")));
    let current = stats(&dir, &canvas, &[]);
    assert_eq!(current["chains"]["running"], 1);
    assert_eq!(current["chainDuration"]["samples"], 0);
    assert_eq!(block(&current, "wait")["outcomes"]["running"], 1);
    assert_eq!(block(&current, "wait")["duration"]["samples"], 0);
    std::fs::write(&release.0, "").unwrap();
    wait_for(|| run.0.try_wait().unwrap().is_some());
    assert!(run.0.wait().unwrap().success());
    let finished = stats(&dir, &canvas, &[]);
    assert_eq!(finished["chains"]["running"], 0);
    assert_eq!(finished["chains"]["succeeded"], 1);
    assert_eq!(finished["chainDuration"]["samples"], 1);
}

#[test]
fn ambiguous_canvas_diagnostics_match_other_commands() {
    let (dir, _) = setup("", 100);
    std::fs::write(dir.join("another.canvas.md"), "").unwrap();
    let mut expected = None;
    for args in [
        vec!["run", "build"],
        vec!["stats", "build"],
        vec!["logs", "build"],
        vec!["console"],
    ] {
        let output = command(&dir).args(&args).output().unwrap();
        assert!(!output.status.success());
        let error = String::from_utf8(output.stderr).unwrap();
        assert!(error.contains("multiple canvas files found: another.canvas.md, test.canvas.md — pass one explicitly"), "{args:?}: {error}");
        if let Some(expected) = &expected {
            assert_eq!(&error, expected);
        } else {
            expected = Some(error);
        }
    }
    std::fs::remove_dir_all(dir).unwrap();
}
