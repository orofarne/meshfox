//! End-to-end proof that neither a running `service` block's process nor a
//! plain block's process that's still mid-run survives as an orphan when
//! its owning `meshfox view` worker is killed by `SIGTERM` — the exact
//! signal `editors/vscode/src/coordinator.ts`'s `killWorker`/`dispose` send
//! when a webview tab closes or the extension deactivates (see
//! `crates/server/src/lib.rs`'s `spawn_shutdown_signal_handler`). Spawns
//! the worker the same way that coordinator does — `meshfox view <path>
//! --watcher-socket <socket>` directly, not a bare `meshfox view <path>`
//! (which instead becomes a *watcher* that spawns its own worker child —
//! see `crates/cli/src/watcher.rs`'s own doc comment — a different process
//! entirely, and not what's under test here).

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

/// `/api/run` is a WS upgrade now, not an HTTP-chunked `POST` — spins up a
/// throwaway tokio runtime just for this one round trip (the rest of this
/// file's tests stay plain sync `#[test]`s, so this is a smaller, more
/// localized change than converting the whole file to `#[tokio::test]`).
/// Drains every `RunEvent` text frame until the socket closes and joins
/// them back into a newline-separated string — the same shape `http()`'s
/// own callers here used to get back from the old chunked-HTTP body.
fn run_ws_events(port: u16, query: &str) -> String {
    tokio::runtime::Runtime::new()
        .expect("build a tokio runtime")
        .block_on(async {
            use futures_util::StreamExt;
            let url = format!("ws://127.0.0.1:{port}/api/run?{query}");
            let (mut ws, _) = tokio_tungstenite::connect_async(url).await.expect("connect");
            let mut body = String::new();
            while let Some(msg) = ws.next().await {
                match msg.expect("no ws error") {
                    tokio_tungstenite::tungstenite::Message::Text(t) => {
                        body.push_str(&t);
                        body.push('\n');
                    }
                    tokio_tungstenite::tungstenite::Message::Close(_) => break,
                    _ => continue,
                }
            }
            body
        })
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

    let body = run_ws_events(port, "block=srv");
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

/// Counts the lines in `path`, or 0 if it doesn't exist yet.
fn tick_count(path: &std::path::Path) -> usize {
    std::fs::read_to_string(path).map(|s| s.lines().count()).unwrap_or(0)
}

/// Same proof as `killing_the_worker_stops_a_running_service_instead_of_
/// orphaning_it` above, for a *plain* (non-`service`) block that's still
/// mid-run when the worker is killed — every spawned block, service or
/// not, lands in its own process group (`stream_exec::spawn_bash`'s own
/// `process_group(0)`), specifically so `kill_process_group`/the run
/// registry's own `kill()` can reach a whole subtree without also hitting
/// unrelated siblings — but that same isolation means nothing about the
/// OS's own process hierarchy stops a plain block from surviving its
/// parent's death on its own; it has to be swept up explicitly, the exact
/// thing this test is checking for. `GET /api/runs`'s own `ActiveRunDto`
/// has no `pid` field (unlike `GET /api/services`'s), so this checks the
/// block's own liveness indirectly instead of by pid: it appends a tick to
/// a file once a second, and the test asserts that stops advancing once
/// the worker is killed, rather than reaching all 30 ticks on its own
/// schedule regardless.
#[test]
fn killing_the_worker_stops_a_currently_running_plain_block_instead_of_orphaning_it() {
    let dir = unique_dir();
    let canvas_path = dir.join("doc.canvas.md");
    let heartbeat_path = dir.join("heartbeat.txt");
    std::fs::write(
        &canvas_path,
        format!(
            "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n\n{}",
            format_args!(
                "```bash name=\"slow\"\necho ready\nfor i in $(seq 1 30); do echo tick >> {}; sleep 1; done\n```\n",
                heartbeat_path.display()
            )
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

    // `http()` reads its response to completion, and this block's own run
    // streams for the whole ~30s it takes to finish — run it on its own
    // thread so this test can get on with killing the worker mid-run
    // instead of waiting on it.
    let run_thread = std::thread::spawn(move || run_ws_events(port, "block=slow"));

    // Wait for the block to actually start ticking before touching
    // anything — same "prove it was genuinely running before the kill"
    // requirement the service test's own `is_alive` check up front has.
    let deadline = Instant::now() + Duration::from_secs(10);
    while tick_count(&heartbeat_path) == 0 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        tick_count(&heartbeat_path) >= 1,
        "block should have ticked at least once before the kill"
    );

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

    // Give the (now-exited) worker's own kill-the-block call a moment to
    // actually land, same reasoning the service test's own equivalent
    // wait has — then confirm the tick count has genuinely stopped
    // advancing (not just paused for a beat): sampled twice, a second
    // apart (this block's own tick interval), well after the kill.
    std::thread::sleep(Duration::from_millis(500));
    let count_after_kill = tick_count(&heartbeat_path);
    std::thread::sleep(Duration::from_secs(2));
    let count_later = tick_count(&heartbeat_path);
    assert_eq!(
        count_after_kill, count_later,
        "block kept ticking after the worker's SIGTERM ({count_after_kill} -> {count_later} ticks) — it was orphaned, not stopped"
    );
    assert!(
        count_later < 30,
        "block reached its full 30 ticks on its own schedule — the kill never reached it at all"
    );

    // The run's own HTTP connection should also have been torn down
    // rather than left hanging once the worker process is gone.
    let deadline = Instant::now() + Duration::from_secs(5);
    while !run_thread.is_finished() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(run_thread.is_finished(), "the run's own HTTP connection never closed after the worker died");

    std::fs::remove_dir_all(&dir).ok();
}
