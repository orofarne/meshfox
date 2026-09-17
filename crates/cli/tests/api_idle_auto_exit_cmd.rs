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
