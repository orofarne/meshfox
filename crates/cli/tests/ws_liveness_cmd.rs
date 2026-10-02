//! A client reading a worker's WebSocket must tell a stream that is merely
//! quiet (a run printing nothing for a while) from one whose worker hung or
//! whose connection died: the worker sends a heartbeat every `MESHFOX_WS_HEARTBEAT_SECS`, the
//! client gives up after `MESHFOX_WS_SILENCE_SECS` with nothing at all. Both
//! halves are checked against a real worker process: the quiet run must
//! finish normally although it is silent for longer than the limit, and the
//! hung worker (`SIGSTOP`: the process exists, its sockets stay open, it just
//! never answers) must end the run with an honest error instead of leaving
//! `meshfox run` waiting for ever.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn unique_dir() -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("meshfox-ws-liveness-test-{nanos}-{n}"));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn signal(pid: u32, name: &str) {
    Command::new("kill")
        .args([&format!("-{name}"), &pid.to_string()])
        .stderr(Stdio::null())
        .status()
        .unwrap();
}

/// A canvas with one block that is silent for `quiet_secs` and then prints.
fn write_canvas(dir: &Path, quiet_secs: u64) -> PathBuf {
    let path = dir.join("doc.canvas.md");
    std::fs::write(
        &path,
        format!(
            "<!-- meshfox:canvas -->\n# Doc\n<!-- meshfox:node id=\"doc\" -->\n\n\
             ```bash name=\"slow\"\nsleep {quiet_secs}\necho finished-quietly\n```\n"
        ),
    )
    .unwrap();
    path
}

/// A standalone worker (`--watcher-socket` makes this process the worker
/// itself, so stopping it stops the thing that answers) that pings every
/// second, found by `meshfox run` through its lock file.
fn spawn_worker(dir: &Path, canvas: &Path) -> Child {
    let worker = Command::new(env!("CARGO_BIN_EXE_meshfox"))
        .arg("view")
        .arg(canvas)
        .args(["--port", "0", "--no-open", "--no-auto-exit"])
        .arg("--watcher-socket")
        .arg(dir.join("watcher.sock"))
        .env("HOME", dir.join("home"))
        .env("MESHFOX_SERVER_SOCKET", "")
        .env("MESHFOX_WS_HEARTBEAT_SECS", "1")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let lock = dir.join(".meshfox").join("doc.canvas.md.worker.lock");
    let deadline = Instant::now() + Duration::from_secs(20);
    while !std::fs::read_to_string(&lock).is_ok_and(|s| s.contains("port=")) {
        assert!(Instant::now() < deadline, "the worker never reported its port");
        std::thread::sleep(Duration::from_millis(100));
    }
    worker
}

fn run_command(dir: &Path, canvas: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_meshfox"));
    cmd.arg("run")
        .arg("--canvas")
        .arg(canvas)
        .arg("slow")
        .env("HOME", dir.join("home"))
        .env("MESHFOX_SERVER_SOCKET", "")
        .env("MESHFOX_WS_SILENCE_SECS", "3");
    cmd
}

fn finish(mut worker: Child, dir: &Path) {
    signal(worker.id(), "CONT");
    let _ = worker.kill();
    let _ = worker.wait();
    let _ = std::fs::remove_dir_all(dir);
}

/// Silent for 7 s against a 3 s limit: only the worker's heartbeats keep the
/// stream alive, and the run's own output and exit status still arrive in
/// full once it ends.
#[test]
fn a_quiet_run_outlasts_the_silence_limit_because_the_worker_sends_heartbeats() {
    let dir = unique_dir();
    let canvas = write_canvas(&dir, 7);
    let worker = spawn_worker(&dir, &canvas);

    let started = Instant::now();
    let output = run_command(&dir, &canvas).output().unwrap();
    let elapsed = started.elapsed();
    finish(worker, &dir);

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "a quiet but healthy run failed:\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(stdout.contains("finished-quietly"), "output lost: {stdout}\n{stderr}");
    assert!(
        elapsed > Duration::from_secs(6),
        "the test only means something if the run was quiet for longer than the limit"
    );
}

/// The worker hangs mid-run: `meshfox run` reports it and exits instead of
/// waiting for a frame that will never come.
#[test]
fn a_run_whose_worker_hangs_ends_with_an_error_instead_of_waiting_for_ever() {
    let dir = unique_dir();
    let canvas = write_canvas(&dir, 60);
    let worker = spawn_worker(&dir, &canvas);

    let mut child = run_command(&dir, &canvas)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // Let the run get going, then hang the worker.
    std::thread::sleep(Duration::from_secs(2));
    signal(worker.id(), "STOP");
    let stopped_at = Instant::now();

    let deadline = Instant::now() + Duration::from_secs(20);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break Some(status);
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            break None;
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    let elapsed = stopped_at.elapsed();
    let output = child.wait_with_output().unwrap();
    finish(worker, &dir);

    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let status = status.unwrap_or_else(|| panic!("meshfox run waited on a hung worker:\n{text}"));
    assert!(!status.success(), "a run cut off by a hung worker must not report success:\n{text}");
    assert!(
        text.contains("stopped responding"),
        "the error should say the worker stopped responding:\n{text}"
    );
    assert!(elapsed < Duration::from_secs(15), "took {elapsed:?} to notice");
}
