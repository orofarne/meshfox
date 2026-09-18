//! Regression coverage for a real gap found in the tab-based auto-exit
//! mechanism (`crates/server/src/lib.rs`'s `TabGuard`/`ever_connected`):
//! it only ever counts `/api/watch` connections (a real browser tab), so a
//! worker whose only clients are non-browser API callers — `meshfox run`
//! routed through `coordinator::resolve`, `node <op>` worker routing, MCP
//! `debug_*` — never flips `ever_connected` at all and so never even
//! *attempts* to exit, no matter how idle it's been. Fixed by
//! `spawn_api_idle_checker`/`AppState::last_api_activity_millis`: a
//! periodic (not event-triggered) check that also counts a plain
//! `/api/*` hit as activity, independent of `open_tabs`.
//!
//! Both tests here take real wall-clock time (`AUTO_EXIT_GRACE` +
//! `AUTO_EXIT_POLL_INTERVAL`, ~15s) — there's no way to shrink those
//! constants for a test without threading a config knob through
//! production code that has no other use for one, and this is exactly the
//! kind of "does a background timer actually fire" behavior a unit test
//! can't observe at all (it would need `std::process::exit` to run,
//! which would kill the test binary itself — a real child process is the
//! only way to see it from the outside).

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn unique_dir() -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("meshfox-api-idle-auto-exit-test-{nanos}-{n}"));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn meshfox() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_meshfox"));
    // Same isolation `node_ops_via_worker_cmd.rs`'s own `meshfox()` helper
    // uses — a real `server_socket` in the developer's own
    // `~/.meshfox/config.toml` would otherwise silently reroute this
    // through their actual external coordinator instead of the bare
    // worker this test spawns itself.
    cmd.env("HOME", unique_dir());
    cmd
}

/// Spawns a bare worker (auto-exit on by default — no `--no-auto-exit`)
/// for `canvas_path` and blocks until its `worker_lock` file reports a
/// port, same convention `node_ops_via_worker_cmd.rs` already established.
/// Returns the child process and its discovered port.
fn spawn_worker(dir: &Path, canvas_path: &Path) -> (std::process::Child, u16) {
    spawn_worker_with_untouched_timeout_secs(dir, canvas_path, None)
}

/// Same as `spawn_worker`, optionally shrinking
/// `MESHFOX_TEST_UNTOUCHED_TIMEOUT_SECS` (see
/// `meshfox_server::untouched_worker_timeout`) so a test can prove the
/// absolute never-touched timeout actually fires without waiting out the
/// real 5-minute default.
fn spawn_worker_with_untouched_timeout_secs(
    dir: &Path,
    canvas_path: &Path,
    untouched_timeout_secs: Option<u64>,
) -> (std::process::Child, u16) {
    let mut cmd = meshfox();
    cmd.arg("view")
        .arg(canvas_path)
        .arg("--port")
        .arg("0")
        .arg("--watcher-socket")
        .arg(dir.join("fake.sock"))
        .stdout(std::process::Stdio::null());
    if let Some(secs) = untouched_timeout_secs {
        cmd.env("MESHFOX_TEST_UNTOUCHED_TIMEOUT_SECS", secs.to_string());
    }
    let worker = cmd.spawn().unwrap();

    let lock_name = format!("{}.worker.lock", canvas_path.file_name().unwrap().to_string_lossy());
    let lock_path = dir.join(".meshfox").join(lock_name);
    let mut port = None;
    for _ in 0..100 {
        if let Ok(contents) = std::fs::read_to_string(&lock_path) {
            if let Some(rest) = contents.split("port=").nth(1) {
                if let Ok(p) = rest.trim().parse::<u16>() {
                    port = Some(p);
                    break;
                }
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    (worker, port.expect("worker never reported a port within 5s"))
}

fn kill(mut worker: std::process::Child) {
    let _ = worker.kill();
    let _ = worker.wait();
}

/// A single plain HTTP/1.1 GET, no `reqwest` needed for something this
/// simple — this test only cares that the request *happened*, not about
/// parsing its response.
fn http_get(port: u16, path: &str) {
    let mut stream = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
    write!(stream, "GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n").unwrap();
    let mut buf = Vec::new();
    let _ = stream.read_to_end(&mut buf);
}

fn write_base_canvas(path: &Path) {
    std::fs::write(
        path,
        "<!-- meshfox:canvas -->\n# Base\n<!-- meshfox:node id=\"root\" -->\n",
    )
    .unwrap();
}

/// Waits up to `timeout` for `child` to exit on its own, polling
/// `try_wait` — returns whether it did.
fn exited_within(child: &mut std::process::Child, timeout: Duration) -> bool {
    let start = std::time::Instant::now();
    while start.elapsed() < timeout {
        if matches!(child.try_wait(), Ok(Some(_))) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    false
}

#[test]
fn a_worker_with_only_api_traffic_and_no_browser_tab_still_auto_exits() {
    let dir = unique_dir();
    let canvas_path = dir.join("base.canvas.md");
    write_base_canvas(&canvas_path);

    let (mut worker, port) = spawn_worker(&dir, &canvas_path);

    // Exactly the regression this covers: a client that hits the API
    // (like `meshfox run`/`node <op>` worker routing) but never opens
    // `/api/watch` at all.
    http_get(port, "/api/canvas");

    let exited = exited_within(&mut worker, Duration::from_secs(25));
    if !exited {
        kill(worker);
    }
    assert!(
        exited,
        "worker with only /api traffic (no browser tab) should have auto-exited after going idle"
    );
}

/// The flip side, guarding against an overzealous fix: a worker nobody has
/// touched *at all* yet (no tab, no API call) must keep waiting
/// indefinitely — exactly `ever_connected`'s existing protection against
/// exiting before an auto-opened tab has even had a chance to connect,
/// which `spawn_api_idle_checker` must not undermine.
#[test]
fn a_worker_with_no_traffic_at_all_does_not_auto_exit() {
    let dir = unique_dir();
    let canvas_path = dir.join("base.canvas.md");
    write_base_canvas(&canvas_path);

    let (mut worker, _port) = spawn_worker(&dir, &canvas_path);

    let exited = exited_within(&mut worker, Duration::from_secs(20));
    kill(worker);
    assert!(!exited, "an untouched worker (no tab, no API call) must not auto-exit");
}

/// A worker nobody ever touches at all shouldn't wait *forever* either —
/// real-world case: a `get_port` caller fetches a port and then never
/// actually calls anything, or an `Open` never gets followed by a browser
/// actually loading. Shrinks `MESHFOX_TEST_UNTOUCHED_TIMEOUT_SECS` so this
/// doesn't have to wait out the real 5-minute default.
/// The bug this guards against, found live: `touch_api_activity`
/// (`crates/server/src/lib.rs`) marks activity exactly once, at a run's own
/// WebSocket-upgrade request — for a run whose real work happens *inside*
/// its `async_stream::stream!` generator (see `has_active_runs`'s own doc
/// comment), that single touch goes stale the instant the run itself takes
/// longer than `AUTO_EXIT_GRACE` (10s), with no browser tab open to keep
/// `open_tabs` nonzero (a bare worker-routed `meshfox run` never opens
/// `/api/watch` at all). Before `has_active_runs` backed off this checker
/// for an in-flight run, this exact test would have seen its own worker
/// killed mid-`sleep`, taking the run (and, in the real incident, a
/// `cargo build`'s own `service_lock` file) down with it.
#[tokio::test]
async fn a_worker_with_a_long_running_run_in_flight_does_not_auto_exit() {
    use futures_util::StreamExt;

    let dir = unique_dir();
    let canvas_path = dir.join("base.canvas.md");
    // 18s comfortably clears AUTO_EXIT_GRACE (10s) + AUTO_EXIT_POLL_INTERVAL
    // (5s) — the old bug had time to fire well before this step's own
    // stream would ever end.
    std::fs::write(
        &canvas_path,
        concat!(
            "<!-- meshfox:canvas -->\n# Base\n<!-- meshfox:node id=\"root\" -->\n\n",
            "```bash name=\"slow\"\nsleep 18\necho done\n```\n",
        ),
    )
    .unwrap();

    let (mut worker, port) = spawn_worker(&dir, &canvas_path);

    let url = format!("ws://127.0.0.1:{port}/api/run?block=slow");
    let (mut ws, _) = tokio_tungstenite::connect_async(url).await.expect("connect");

    // Drain events until the run's own terminal `Done` arrives — proves
    // the worker survived the *whole* ~18s run, not just some arbitrary
    // earlier instant. The per-message timeout has to clear the sleep
    // itself (there's no output at all in between `step-start` and
    // `step-end`/`done`), not just a normal "did anything arrive" check.
    let mut saw_done = false;
    while let Ok(Some(Ok(msg))) = tokio::time::timeout(Duration::from_secs(25), ws.next()).await {
        if let Ok(text) = msg.into_text() {
            if text.contains("\"type\":\"done\"") {
                saw_done = true;
                break;
            }
        }
    }
    let _ = ws.close(None).await;
    assert!(saw_done, "the run should have completed with a Done event");

    // The worker itself must still be alive right after — the whole point
    // of this test. A brief grace window in case the process is still
    // tearing down its own WebSocket connection.
    let survived = !exited_within(&mut worker, Duration::from_secs(2));
    kill(worker);
    assert!(
        survived,
        "the worker should still have been running immediately after its own long run finished"
    );
}

/// The sharper edge of the same bug, once `has_active_runs` alone fixed
/// the plain-run case above: a `tty` session is deliberately designed to
/// outlive the connection that started it (`tty_registry`'s own module doc
/// comment — that's the whole point of `/api/run/tty/attach` letting a
/// *different* connection reconnect to it later), so `state.runs`'s own
/// entry — tied to one connection's own stream, not the session itself —
/// disappears the instant this test's own WebSocket closes, well before
/// the underlying pty process actually exits. Without `has_running_tty_
/// sessions` also backing off the idle checker, that gap would let it kill
/// the whole worker (and the still-running pty) the moment nobody happens
/// to be watching, even though a later `/api/run/tty/attach` could have
/// reconnected to it just fine.
#[tokio::test]
async fn a_worker_with_a_tty_session_outliving_its_own_connection_does_not_auto_exit() {
    use futures_util::StreamExt;

    let dir = unique_dir();
    let canvas_path = dir.join("base.canvas.md");
    // Long enough that it's still `Running` well past when this test's own
    // connection closes and the idle checker gets a few chances to fire.
    std::fs::write(
        &canvas_path,
        concat!(
            "<!-- meshfox:canvas -->\n# Base\n<!-- meshfox:node id=\"root\" -->\n\n",
            "```bash name=\"slow-tty\" tty\nsleep 18\necho done\n```\n",
        ),
    )
    .unwrap();

    let (mut worker, port) = spawn_worker(&dir, &canvas_path);

    let url = format!("ws://127.0.0.1:{port}/api/run/tty?block=slow-tty&cols=80&rows=24");
    let (mut ws, _) = tokio_tungstenite::connect_async(url).await.expect("connect");

    // Wait for `tty-start` specifically — proof the session is actually
    // registered in `tty_registry` (not just that the run itself started)
    // before this test disconnects out from under it.
    let mut saw_tty_start = false;
    while let Ok(Some(Ok(msg))) = tokio::time::timeout(Duration::from_secs(10), ws.next()).await {
        if let Ok(text) = msg.into_text() {
            if text.contains("\"type\":\"tty-start\"") {
                saw_tty_start = true;
                break;
            }
        }
    }
    assert!(saw_tty_start, "the tty session should have started");

    // Simulate closing the tab: drop the connection entirely, well before
    // the pty's own ~18s script finishes.
    drop(ws);

    // Past AUTO_EXIT_GRACE + AUTO_EXIT_POLL_INTERVAL at least twice over,
    // with no connection of any kind open to this worker — exactly the
    // window the old bug would have killed it in.
    let survived = !exited_within(&mut worker, Duration::from_secs(16));
    kill(worker);
    assert!(
        survived,
        "the worker should still be running while its own tty session (nobody currently watching) is still active"
    );
}

// `debug_send`'s own case (a plain, non-streaming handler that genuinely
// blocks for its whole command) is deliberately *not* covered here: unlike
// `run`/`tty`, whose liveness is a real fact this worker itself can check
// (`state.runs`/`tty_registry`), a bare `POST /api/debug/send` has no such
// registry of its own to consult — a caller resolving that gap by holding
// its own connection open for as long as it's using the session (the same
// signal a browser tab already gives via `/api/watch`) belongs in that
// caller, not here. See `crate::mcp`'s own `DebugHandle::Remote` for where
// that's actually done — `has_open_tabs`/`TabGuard` already covers it once
// that connection exists, no separate check needed in this file.

#[test]
fn a_worker_with_no_traffic_at_all_still_exits_after_the_untouched_timeout() {
    let dir = unique_dir();
    let canvas_path = dir.join("base.canvas.md");
    write_base_canvas(&canvas_path);

    let (mut worker, _port) = spawn_worker_with_untouched_timeout_secs(&dir, &canvas_path, Some(3));

    let exited = exited_within(&mut worker, Duration::from_secs(20));
    if !exited {
        kill(worker);
    }
    assert!(exited, "a completely untouched worker should still exit after the untouched timeout");
}
