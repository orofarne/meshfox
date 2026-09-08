//! End-to-end proof that a running `service` block's process doesn't
//! survive as an orphan when its owning `meshfox view` worker is killed by
//! `SIGTERM` — the exact signal `editors/vscode/src/coordinator.ts`'s
//! `killWorker`/`dispose` send when a webview tab closes or the extension
//! deactivates (see `crates/server/src/lib.rs`'s
//! `spawn_shutdown_signal_handler`). Spawns the worker the same way that
//! coordinator does — `meshfox view <path> --watcher-socket <socket>`
//! directly, not a bare `meshfox view <path>` (which instead becomes a
//! *watcher* that spawns its own worker child — see `crates/cli/src/
//! watcher.rs`'s own doc comment — a different process entirely, and not
//! what's under test here).

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
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
    let dir = std::env::temp_dir().join(format!("meshfox-service-shutdown-test-{nanos}-{n}"));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn meshfox() -> Command {
    Command::new(env!("CARGO_BIN_EXE_meshfox"))
}

/// Blocks until `child`'s own stdout prints its bound port (`"meshfox:
/// serving ... on http://127.0.0.1:<port>"`, see `crates/server/src/
/// lib.rs`'s `run`) or a few seconds pass without it.
fn read_bound_port(child: &mut Child) -> u16 {
    let stdout = child.stdout.take().expect("piped stdout");
    let mut reader = BufReader::new(stdout);
    let mut line = String::new();
    // Generous — a real subprocess, and the whole test suite runs many
    // such subprocess-spawning tests concurrently, competing for CPU.
    for _ in 0..600 {
        line.clear();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            std::thread::sleep(Duration::from_millis(50));
            continue;
        }
        if let Some(colon) = line.trim_end().rfind(':') {
            if let Ok(port) = line[colon + 1..].trim().parse::<u16>() {
                return port;
            }
        }
    }
    panic!("worker never reported its bound port");
}

/// Plain synchronous HTTP/1.1 request over a raw socket — same
/// no-extra-dependency approach `crates/server/src/lib.rs`'s own test
/// helpers use (just sync here, not async: this is a plain `#[test]`).
fn http(port: u16, method: &str, path: &str, body: &str) -> (u16, String) {
    let request = if body.is_empty() {
        format!("{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n")
    } else {
        format!(
            "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
    };
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect");
    stream.write_all(request.as_bytes()).unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    let mut parts = response.splitn(2, "\r\n\r\n");
    let head = parts.next().unwrap_or_default();
    let resp_body = parts.next().unwrap_or_default();
    let status = head
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    (status, resp_body.to_string())
}

fn is_alive(pid: u32) -> bool {
    // SAFETY: signal 0 sends nothing, just probes existence/permission.
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

#[test]
fn killing_the_worker_stops_a_running_service_instead_of_orphaning_it() {
    let dir = unique_dir();
    let canvas_path = dir.join("doc.canvas.md");
    std::fs::write(
        &canvas_path,
        concat!(
            "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
            "```bash name=\"srv\" service\necho ready\nsleep 30\n```\n",
        ),
    )
    .unwrap();

    let fake_socket = dir.join("fake.sock");
    let mut child = meshfox()
        .args([
            "view",
            canvas_path.to_str().unwrap(),
            "--port",
            "0",
            "--watcher-socket",
            fake_socket.to_str().unwrap(),
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn meshfox view");
    let worker_pid = child.id();

    let port = read_bound_port(&mut child);

    let (status, body) = http(
        port,
        "POST",
        "/api/run",
        r#"{"path":[],"block":"srv"}"#,
    );
    assert_eq!(status, 200, "unexpected /api/run body: {body}");
    assert!(
        body.contains("\"type\":\"service-started\""),
        "expected a service-started event, got: {body}"
    );

    let (status, body) = http(port, "GET", "/api/services", "");
    assert_eq!(status, 200);
    let services: Vec<serde_json::Value> = serde_json::from_str(&body).unwrap();
    assert_eq!(services.len(), 1);
    assert_eq!(services[0]["status"], "running");
    let service_pid = services[0]["pid"].as_u64().unwrap() as u32;
    assert!(is_alive(service_pid), "service should be running before the kill");

    // The exact signal `editors/vscode/src/coordinator.ts`'s `killWorker`/
    // `dispose` send (Node's default `ChildProcess.kill()`).
    unsafe {
        libc::kill(worker_pid as libc::pid_t, libc::SIGTERM);
    }

    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        if let Ok(Some(_)) = child.try_wait() {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        matches!(child.try_wait(), Ok(Some(_))),
        "worker should have exited after SIGTERM"
    );

    // Give the (now-exited) worker's own kill-the-service call a moment to
    // actually land — `kill()` itself is synchronous, but the OS still
    // needs a beat to actually reap the signaled process. Generous, same
    // "many concurrent subprocess-spawning tests" reasoning as above.
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut still_alive = is_alive(service_pid);
    while still_alive && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
        still_alive = is_alive(service_pid);
    }
    assert!(
        !still_alive,
        "service pid {service_pid} survived the worker's SIGTERM — it was orphaned, not stopped"
    );

    std::fs::remove_dir_all(&dir).ok();
}
