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
            "```bash name=\"dep\"\necho ran >> count.txt\necho dependency-live-output\n```\n\n",
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
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(runs(&dir), 1, "first run: dep runs");
    assert!(String::from_utf8_lossy(&out.stdout).contains("dependency-live-output"));

    let out = run_target(&home, &canvas, &[]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(runs(&dir), 1, "second run: dep already fresh, skipped");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("already run this session, skipped"));
    assert!(!stdout.contains("dependency-live-output"), "skipped steps must not replay old logs: {stdout}");

    let out = run_target(&home, &canvas, &["--fresh"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(runs(&dir), 2, "--fresh: dep runs for real this once");

    let out = run_target(&home, &canvas, &[]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        runs(&dir),
        2,
        "--fresh forgot nothing: the next plain run skips again"
    );

    let out = meshfox(&home)
        .arg("session")
        .arg("reset")
        .arg("--canvas")
        .arg(&canvas)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("session reset"),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );

    let out = run_target(&home, &canvas, &[]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        runs(&dir),
        3,
        "after `session reset` the dependency runs again"
    );

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
    assert!(
        stderr.contains("--fresh") && stderr.contains("--no-deps"),
        "{stderr}"
    );
    assert_eq!(runs(&dir), 0, "nothing ran");
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn always_observation_only_reruns_consumers_when_its_value_changes() {
    let dir = unique_dir();
    let home = unique_dir();
    let canvas = dir.join("values.canvas.md");
    std::fs::write(dir.join("revision"), "one\n").unwrap();
    std::fs::write(&canvas, concat!(
        "<!-- meshfox:canvas -->\n# Values\n<!-- meshfox:node id=\"root\" -->\n\n",
        "<!-- meshfox:var name=\"REV\" from=\"observe\" -->\n\n",
        "```bash name=\"observe\" always\nprintf 'REV=%s\\n' \"$(cat revision)\" >> \"$MESHFOX_VARS_OUT\"\necho checked >> checks.txt\n```\n\n",
        "```bash name=\"prepare\"\necho prepared >> preparations.txt\n```\n\n",
        "```bash name=\"consume\" env=\"REV\" deps=\"prepare!\"\necho \"$REV\" >> count.txt\n```\n\n",
        "```bash name=\"explicit\" env=\"REV\" deps=\"observe\"\necho ran >> explicit.txt\n```\n\n",
        "```bash name=\"target\" deps=\"consume,explicit\"\ntrue\n```\n",
    )).unwrap();
    for expected in [1, 1, 2, 2] {
        if expected == 2 {
            std::fs::write(dir.join("revision"), "two\n").unwrap();
        }
        let out = run_target(&home, &canvas, &[]);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(runs(&dir), expected);
        assert_eq!(
            std::fs::read_to_string(dir.join("preparations.txt"))
                .unwrap()
                .lines()
                .count(),
            expected
        );
    }
    assert_eq!(
        std::fs::read_to_string(dir.join("checks.txt"))
            .unwrap()
            .lines()
            .count(),
        4
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("explicit.txt"))
            .unwrap()
            .lines()
            .count(),
        4
    );
    let out = run_target(&home, &canvas, &["--fresh"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(runs(&dir), 3);
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn artifact_producers_are_discovered_and_consumers_follow_content_not_execution() {
    let dir = unique_dir();
    let home = unique_dir();
    let canvas = dir.join("artifacts.canvas.md");
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(dir.join("src/value.txt"), "X-one\n").unwrap();
    std::fs::write(&canvas, concat!(
        "<!-- meshfox:canvas -->\n# Artifacts\n<!-- meshfox:node id=\"root\" -->\n\n",
        "<!-- meshfox:var name=\"BUILD_DIR\" default=\"build\" -->\n",
        "<!-- meshfox:var name=\"ALIAS\" default=\"./build\" -->\n\n",
        "```sh name=\"build\" inputs=\"src/*.txt\" outputs=\"$BUILD_DIR/bin\" env=\"BUILD_DIR\"\nmkdir -p \"$BUILD_DIR\"\nhead -c 1 src/value.txt > \"$BUILD_DIR/bin\"\necho ran >> builds.txt\n```\n\n",
        "```sh name=\"install\" inputs=\"${ALIAS}/bin\" outputs=\"installed\" env=\"ALIAS\"\ncp \"$ALIAS/bin\" installed\necho ran >> count.txt\n```\n\n",
        "```button name=\"target\" deps=\"install\"\nBuild and install\n```\n",
    )).unwrap();
    let invoke = |builds, installs| {
        let out = run_target(&home, &canvas, &[]);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("builds.txt"))
                .unwrap()
                .lines()
                .count(),
            builds
        );
        assert_eq!(runs(&dir), installs);
    };
    invoke(1, 1);
    invoke(1, 1);
    std::fs::write(dir.join("src/value.txt"), "X-two\n").unwrap();
    invoke(2, 1); // build reruns, binary unchanged
    std::fs::write(dir.join("src/value.txt"), "Y-three\n").unwrap();
    invoke(3, 2);
    std::fs::remove_file(dir.join("build/bin")).unwrap();
    invoke(4, 2); // restored binary has identical content
    std::fs::write(dir.join("build/bin"), "tampered").unwrap();
    invoke(5, 2);
    std::fs::remove_file(dir.join("installed")).unwrap();
    invoke(5, 3);
    std::fs::write(dir.join("src/extra.txt"), "extra").unwrap();
    invoke(6, 3);
    std::fs::remove_file(dir.join("src/extra.txt")).unwrap();
    invoke(7, 3);
    // A changed producer implementation still emits an identical binary.
    let changed = std::fs::read_to_string(&canvas)
        .unwrap()
        .replace("head -c 1", "# implementation changed\nhead -c 1");
    std::fs::write(&canvas, changed).unwrap();
    invoke(8, 3);
    let out = run_target(&home, &canvas, &["--fresh"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(runs(&dir), 4);
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn artifact_contract_errors_fail_the_run_instead_of_certifying_success() {
    let dir = unique_dir();
    let home = unique_dir();
    let canvas = dir.join("contracts.canvas.md");
    let cases = [
        "```sh name=target inputs=missing.csv\ntrue\n```\n",
        "```sh name=target inputs=missing/*.csv\ntrue\n```\n",
        "```sh name=target outputs=missing.csv\ntrue\n```\n",
        "```sh name=target inputs=input outputs=out\necho changed > input\ntouch out\n```\n",
    ];
    for body in cases {
        std::fs::write(dir.join("input"), "initial").unwrap();
        std::fs::write(
            &canvas,
            format!("<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n{body}"),
        )
        .unwrap();
        let out = run_target(&home, &canvas, &[]);
        assert!(
            !out.status.success(),
            "contract violation was accepted: {body}"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn computed_input_paths_select_the_actual_producer_after_observation() {
    let dir = unique_dir();
    let home = unique_dir();
    let canvas = dir.join("dynamic.canvas.md");
    std::fs::write(dir.join("choice"), "a").unwrap();
    std::fs::write(&canvas, concat!(
        "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n",
        "<!-- meshfox:var name=DIR from=choose -->\n",
        "```sh name=choose always\nprintf 'DIR=%s\\n' \"$(cat choice)\" >> \"$MESHFOX_VARS_OUT\"\n```\n",
        "```sh name=make_a outputs=a/bin\nmkdir -p a\necho A > a/bin\necho a >> builds\n```\n",
        "```sh name=make_b outputs=b/bin\nmkdir -p b\necho B > b/bin\necho b >> builds\n```\n",
        "```sh name=install inputs=$DIR/bin outputs=installed env=DIR\ncp \"$DIR/bin\" installed\necho ran >> count.txt\n```\n",
        "```button name=target deps=install\nInstall\n```\n",
    )).unwrap();
    for (choice, expected, installs) in [
        ("a", "A\n", 1),
        ("b", "B\n", 2),
        ("b", "B\n", 2),
        ("a", "A\n", 3),
    ] {
        std::fs::write(dir.join("choice"), choice).unwrap();
        let out = run_target(&home, &canvas, &[]);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("installed")).unwrap(),
            expected
        );
        assert_eq!(runs(&dir), installs);
    }
    assert_eq!(
        std::fs::read_to_string(dir.join("builds")).unwrap(),
        "a\nb\n"
    );
    let _ = std::fs::remove_dir_all(dir);
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn computed_output_paths_are_observed_to_discover_producers() {
    let dir = unique_dir();
    let home = unique_dir();
    let canvas = dir.join("output-path.canvas.md");
    std::fs::write(&canvas, concat!(
        "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n",
        "<!-- meshfox:var name=OUT_DIR from=where -->\n",
        "```sh name=where always\nprintf 'OUT_DIR=build\\n' >> \"$MESHFOX_VARS_OUT\"\n```\n",
        "```sh name=build outputs=$OUT_DIR/bin env=OUT_DIR\nmkdir -p \"$OUT_DIR\"\necho value > \"$OUT_DIR/bin\"\n```\n",
        "```sh name=consume inputs=build/bin\ncat build/bin\necho ran >> count.txt\n```\n",
        "```button name=target deps=consume\nConsume\n```\n",
    )).unwrap();
    for expected in [1, 1] {
        let out = run_target(&home, &canvas, &[]);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(runs(&dir), expected);
    }
    let _ = std::fs::remove_dir_all(dir);
    let _ = std::fs::remove_dir_all(home);
}
