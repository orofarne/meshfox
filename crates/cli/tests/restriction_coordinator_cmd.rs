//! End-to-end: `--read-only` / `--no-exec` must survive every way a command
//! finds its core. A restriction that a hand-off to an external
//! `server_socket` coordinator (the macOS daemon, `meshfox serve`) or a
//! lookup of an already-running worker silently drops would open an
//! untrusted canvas with execution enabled.
//!
//! The coordinator here is a fake: a Unix socket that records every request
//! line and answers `open` with an ack and `get_port` with a port the test
//! picked, so what the client *asked for* is visible, whatever the real
//! coordinators do with it.
#![cfg(unix)]

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

static COUNTER: AtomicU64 = AtomicU64::new(0);

const CANVAS: &str = "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n";

/// Short: a Unix socket path has a tight length budget (`SUN_LEN`).
fn unique_dir() -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("mfx-rc-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write_canvas(dir: &Path) -> PathBuf {
    let canvas = dir.join("doc.canvas.md");
    std::fs::write(&canvas, CANVAS).unwrap();
    canvas.canonicalize().unwrap()
}

/// A command with an isolated `HOME` (the developer's own config must not
/// pick the coordinator) and no restriction inherited from the environment.
fn meshfox(home: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_meshfox"));
    cmd.env("HOME", home)
        .env("MESHFOX_SERVER_SOCKET", "")
        .env_remove("MESHFOX_READ_ONLY")
        .env_remove("MESHFOX_NO_EXEC");
    cmd
}

/// Kills the wrapped process when dropped, so a failing test leaves nothing
/// running.
struct Reaper(Child);

impl Drop for Reaper {
    fn drop(&mut self) {
        // SIGTERM first: a watcher takes its worker down with it, a SIGKILL
        // would leave that worker running until its own idle exit.
        let _ = Command::new("kill")
            .args(["-TERM", &self.0.id().to_string()])
            .status();
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline && matches!(self.0.try_wait(), Ok(None)) {
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// A coordinator that records each request as parsed JSON.
struct FakeCoordinator {
    socket: PathBuf,
    requests: Arc<Mutex<Vec<serde_json::Value>>>,
}

impl FakeCoordinator {
    /// `port` is what `get_port` answers; `None` answers it with an error.
    fn start(dir: &Path, port: Option<u16>) -> Self {
        let socket = dir.join("coord.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&requests);
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut line = String::new();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                if reader.read_line(&mut line).is_err() {
                    continue;
                }
                let Ok(request) = serde_json::from_str::<serde_json::Value>(line.trim()) else {
                    continue;
                };
                let reply = match request["op"].as_str() {
                    Some("get_port") => match port {
                        Some(port) => format!("{{\"port\":{port}}}\n"),
                        None => "{\"error\":\"fake coordinator has no worker\"}\n".to_string(),
                    },
                    _ => "{}\n".to_string(),
                };
                seen.lock().unwrap().push(request);
                let mut stream = stream;
                let _ = stream.write_all(reply.as_bytes());
            }
        });
        FakeCoordinator { socket, requests }
    }

    fn requests(&self) -> Vec<serde_json::Value> {
        self.requests.lock().unwrap().clone()
    }

    fn wait_for_request(&self) -> serde_json::Value {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if let Some(first) = self.requests().into_iter().next() {
                return first;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!("the coordinator never received a request");
    }
}

/// An ordinary, unrestricted worker for `canvas`; returns once its lock file
/// reports a port, and that port.
fn spawn_unrestricted_worker(dir: &Path, canvas: &Path) -> (Reaper, u16) {
    let worker = meshfox(dir)
        .arg("view")
        .arg(canvas)
        .args(["--port", "0", "--no-open", "--watcher-socket"])
        .arg(dir.join("fake-watcher.sock"))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let lock = dir.join(".meshfox").join(format!(
        "{}.worker.lock",
        canvas.file_name().unwrap().to_string_lossy()
    ));
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        if let Ok(text) = std::fs::read_to_string(&lock) {
            if let Some(port) = text
                .split_whitespace()
                .find_map(|word| word.strip_prefix("port="))
                .and_then(|port| port.parse().ok())
            {
                return (Reaper(worker), port);
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("the worker never reported a port");
}

/// Runs `meshfox <canvas> view --no-open <flags>` against `coordinator` and
/// reports whether it handed off and exited by itself within the window, plus
/// everything it printed.
fn view_with(dir: &Path, canvas: &Path, coordinator: &FakeCoordinator, flags: &[&str]) -> ViewRun {
    // Output goes to files, not pipes: a `view` that serves the canvas itself
    // has a worker child that can outlive it and would hold a pipe open.
    let out_path = dir.join("view.out");
    let log = std::fs::File::create(&out_path).unwrap();
    let child = meshfox(dir)
        .env("MESHFOX_SERVER_SOCKET", &coordinator.socket)
        .arg(canvas)
        .args(["view", "--no-open"])
        .args(flags)
        .stdout(log.try_clone().unwrap())
        .stderr(log)
        .spawn()
        .unwrap();
    let mut child = Reaper(child);
    let deadline = Instant::now() + Duration::from_secs(4);
    let mut exited_by_itself = false;
    while Instant::now() < deadline {
        if child.0.try_wait().unwrap().is_some() {
            exited_by_itself = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    // Stops the watcher and, with it, the worker it started.
    drop(child);
    ViewRun {
        exited_by_itself,
        output: std::fs::read_to_string(&out_path).unwrap_or_default(),
    }
}

struct ViewRun {
    exited_by_itself: bool,
    output: String,
}

#[test]
fn view_read_only_does_not_hand_off_to_an_external_coordinator() {
    for flags in [&["--read-only"][..], &["--read-only", "--no-exec"][..]] {
        let dir = unique_dir();
        let canvas = write_canvas(&dir);
        let coordinator = FakeCoordinator::start(&dir, None);

        let run = view_with(&dir, &canvas, &coordinator, flags);

        assert_eq!(
            coordinator.requests(),
            Vec::<serde_json::Value>::new(),
            "{flags:?}: a read-only view must never ask the coordinator for anything"
        );
        assert!(
            !run.exited_by_itself,
            "{flags:?}: the view should be serving the canvas itself, not have handed it off: {}",
            run.output
        );
        assert!(
            run.output.contains("external coordinator"),
            "{flags:?}: the fallback should be announced, not silent: {}",
            run.output
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[test]
fn view_no_exec_hands_the_restriction_on_to_the_coordinator() {
    let dir = unique_dir();
    let canvas = write_canvas(&dir);
    let coordinator = FakeCoordinator::start(&dir, None);

    let _run = view_with(&dir, &canvas, &coordinator, &["--no-exec"]);

    let request = coordinator.wait_for_request();
    assert_eq!(request["op"], "open", "{request}");
    assert_eq!(
        request["no_exec"], true,
        "the hand-off must say the core is to run without execution: {request}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn no_exec_client_refuses_an_unrestricted_worker_from_the_coordinator() {
    let dir = unique_dir();
    let canvas = write_canvas(&dir);
    let (_worker, port) = spawn_unrestricted_worker(&dir, &canvas);
    let coordinator = FakeCoordinator::start(&dir, Some(port));

    let out = meshfox(&dir)
        .env("MESHFOX_SERVER_SOCKET", &coordinator.socket)
        .env("MESHFOX_NO_EXEC", "1")
        .args(["node", "add", "--canvas"])
        .arg(&canvas)
        .args(["root", "Added"])
        .output()
        .unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    let request = coordinator.wait_for_request();
    assert_eq!(request["op"], "get_port", "{request}");
    assert_eq!(request["no_exec"], true, "{request}");
    assert!(
        !out.status.success(),
        "a no-exec client must not use a worker that can run code: {text}"
    );
    assert!(
        text.contains("no-exec"),
        "the refusal should say why: {text}"
    );
    assert_eq!(std::fs::read_to_string(&canvas).unwrap(), CANVAS);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn no_exec_client_refuses_a_local_worker_that_can_run_code() {
    let dir = unique_dir();
    let canvas = write_canvas(&dir);
    let (_worker, _port) = spawn_unrestricted_worker(&dir, &canvas);

    let out = meshfox(&dir)
        .env("MESHFOX_NO_EXEC", "1")
        .args(["node", "add", "--canvas"])
        .arg(&canvas)
        .args(["root", "Added"])
        .output()
        .unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    assert!(
        !out.status.success(),
        "a no-exec client must not use a worker that can run code: {text}"
    );
    assert!(
        text.contains("no-exec"),
        "the refusal should say why: {text}"
    );
    assert_eq!(std::fs::read_to_string(&canvas).unwrap(), CANVAS);
    let _ = std::fs::remove_dir_all(&dir);
}
