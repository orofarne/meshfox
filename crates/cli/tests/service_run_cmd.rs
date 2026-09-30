//! End-to-end proof of `meshfox run`'s own `service`-block behavior (see
//! SPEC.md's "Service blocks (experimental)"): unlike an ordinary chain,
//! the process stays attached once a `service` block has been spawned —
//! streaming its output to the console — until Ctrl-C, rather than exiting
//! the instant the chain finishes.

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use portable_pty::{native_pty_system, CommandBuilder, PtySize};

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn unique_dir() -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("meshfox-service-run-cmd-test-{nanos}-{n}"));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn meshfox() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_meshfox"));
    // Isolates this process from the real machine's own
    // `~/.meshfox/config.toml` — `meshfox_core::config` reads
    // `$HOME/.meshfox/config.toml` unconditionally, so a developer with
    // (say) `server_socket` set for their own daily use would otherwise
    // have `meshfox run` below silently routed through *their* real
    // external coordinator instead of the in-process path this test means
    // to check (confirmed live: this exact leak broke this file's own
    // service test — and a sibling suite, `run_cmd.rs` — once
    // `server_socket` was set on the machine this was developed on).
    cmd.env("HOME", unique_dir());
    cmd
}

fn is_alive(pid: u32) -> bool {
    // SAFETY: signal 0 sends nothing, just probes existence/permission.
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

/// Seeds `canvas_path`'s own `.meshfox/<canvas>.session.sqlite3` with a
/// `running` `run_ledger` row for `(node_id, block)` — through the real
/// `meshfox_server::session_db`/`run_ledger` API, the exact same connection/
/// schema a real worker would use, not a hand-rolled duplicate of the
/// format — *before* any worker for `canvas_path` ever starts, mimicking
/// the real-world case this mechanism actually exists for: a previous
/// worker's `service` block that outlived it (crashed/`SIGKILL`ed core,
/// see TODO.canvas.md's own "ни один дочерний процесс не переживает ядро"
/// note) — the *first* run against a fresh worker for this address, not a
/// second one colliding with a live sibling (the shared-worker unification
/// means two ordinary `run`s against the same canvas now join one worker
/// instead of ever contending like this at all).
fn seed_stale_service_lock(canvas_path: &std::path::Path, node_id: &str, block: &str, pid: u32) {
    let conn = meshfox_server::session_db::open(canvas_path).unwrap();
    let ledger = meshfox_server::run_ledger::RunLedger::from_connection(conn).unwrap();
    ledger
        .start(
            node_id,
            block,
            meshfox_server::run_ledger::RunKind::Service,
            "cli",
            pid,
        )
        .unwrap();
}

/// A long-lived process to stand in for the "foreign owner" a stale
/// `service_lock` file names. `process_group(0)` (`setpgid(0, 0)`) makes it
/// the leader of its own fresh process group — every real spawner in this
/// codebase already does this for exactly this reason (see
/// `service_lock::kill_and_acquire`'s own doc comment: "same reach as every
/// other kill... every spawner makes its child the leader of a fresh
/// group") — without it, `kill(-pid, SIGKILL)` targets a process group this
/// dummy was never actually the leader of (it'd inherit the test binary's
/// own group instead), and the confirmed retry below would silently fail
/// to kill it.
fn spawn_dummy_owner() -> std::process::Child {
    Command::new("sleep")
        .arg("60")
        .process_group(0)
        .spawn()
        .expect("spawn dummy owner process")
}

#[test]
fn run_stays_attached_streams_output_and_ctrl_c_stops_the_service() {
    let dir = unique_dir();
    let canvas_path = dir.join("doc.canvas.md");
    std::fs::write(
        &canvas_path,
        concat!(
            "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
            "```bash name=\"srv\" service\necho hello-from-service\nsleep 30\n```\n",
        ),
    )
    .unwrap();

    let mut child = meshfox()
        .args(["run", "--canvas", canvas_path.to_str().unwrap(), "srv"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn meshfox run");
    let run_pid = child.id();

    let stdout = child.stdout.take().unwrap();
    let mut reader = BufReader::new(stdout);

    let mut saw_started = false;
    let mut saw_output = false;
    let mut saw_streaming_banner = false;
    let mut service_pid: Option<u32> = None;
    let mut line = String::new();
    // Generous — this spawns a real subprocess tree, and the whole test
    // suite runs many such tests concurrently, competing for scheduling.
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline && !(saw_started && saw_output && saw_streaming_banner) {
        line.clear();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            std::thread::sleep(Duration::from_millis(20));
            continue;
        }
        let trimmed = line.trim_end();
        if trimmed.contains("service started, pid") {
            saw_started = true;
            service_pid = trimmed
                .rsplit("pid ")
                .next()
                .and_then(|s| s.trim_end_matches(')').parse::<u32>().ok());
        }
        if trimmed.contains("hello-from-service") {
            saw_output = true;
        }
        if trimmed.contains("service(s) running") {
            saw_streaming_banner = true;
        }
    }
    assert!(saw_started, "never saw the 'service started' line");
    assert!(
        saw_output,
        "never saw the service's own streamed output line"
    );
    assert!(
        saw_streaming_banner,
        "never saw the 'staying attached' banner — run exited instead of watching the service"
    );
    let service_pid = service_pid.expect("service pid parsed from the started line");
    assert!(
        is_alive(service_pid),
        "service should be running at this point"
    );

    // `meshfox run` should still be alive and blocked here, not exited —
    // the whole point of "staying attached".
    std::thread::sleep(Duration::from_millis(200));
    assert!(
        matches!(child.try_wait(), Ok(None)),
        "meshfox run should still be attached/blocked, watching the service"
    );

    // SIGINT — same signal a real Ctrl-C in the terminal sends.
    unsafe {
        libc::kill(run_pid as libc::pid_t, libc::SIGINT);
    }

    let deadline = Instant::now() + Duration::from_secs(15);
    let mut status = None;
    while Instant::now() < deadline {
        if let Ok(Some(s)) = child.try_wait() {
            status = Some(s);
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let status = status.expect("meshfox run should have exited after Ctrl-C");
    assert_eq!(
        std::os::unix::process::ExitStatusExt::signal(&status),
        None,
        "should exit normally with code 130, not be killed by a signal itself"
    );
    assert_eq!(status.code(), Some(130));

    let deadline = Instant::now() + Duration::from_secs(10);
    let mut still_alive = is_alive(service_pid);
    while still_alive && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
        still_alive = is_alive(service_pid);
    }
    assert!(
        !still_alive,
        "service pid {service_pid} survived Ctrl-C — it was orphaned, not stopped"
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// A `service` block whose on-disk lock file was already seeded with a
/// live foreign pid before this worker ever started (see
/// `seed_stale_service_lock`'s own doc comment for why this is the
/// realistic shape of the scenario, not a contrived one) gets a
/// `RunEvent::LockConflict` on its very first run — piped (non-tty) stdin
/// must refuse the kill-and-retry outright rather than hang forever
/// waiting for an answer nobody can type. See `crates/cli/src/main.rs`'s
/// `confirm_kill_and_retry`.
#[test]
fn lock_conflict_on_worker_routed_run_refuses_non_interactively() {
    let dir = unique_dir();
    let canvas_path = dir.join("doc.canvas.md");
    std::fs::write(
        &canvas_path,
        concat!(
            "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
            "```bash name=\"srv\" service\necho hello-from-srv\nsleep 30\n```\n",
        ),
    )
    .unwrap();

    let mut dummy = spawn_dummy_owner();
    let dummy_pid = dummy.id();
    seed_stale_service_lock(&canvas_path, "root", "srv", dummy_pid);

    let run = meshfox()
        .args(["run", "--canvas", canvas_path.to_str().unwrap(), "srv"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("spawn+wait meshfox run");
    let stderr = String::from_utf8_lossy(&run.stderr);
    assert!(
        stderr.contains("not an interactive terminal, refusing to kill and retry"),
        "stderr: {stderr}"
    );
    assert!(
        !run.status.success(),
        "the run should report failure, not silently succeed"
    );
    assert!(
        is_alive(dummy_pid),
        "the seeded owner process should be untouched by a refused retry"
    );

    let _ = dummy.kill();
    let _ = dummy.wait();
    std::fs::remove_dir_all(&dir).ok();
}

/// The interactive counterpart: a real pty (so stdin *is* a terminal),
/// answering `y` to the kill-and-retry prompt. Should kill the seeded
/// dummy owner and have the run's own `srv` actually start in its place —
/// proving `retry_after_lock_conflict`'s `force: Some((node_id, block))`
/// retry actually reaches the server's `/api/run/force`, not just that the
/// prompt itself renders.
#[test]
fn lock_conflict_on_worker_routed_run_kills_and_retries_when_confirmed() {
    // Same ad-hoc-signature workaround `tui_e2e/harness.rs::TuiSession::spawn`
    // documents — spawning `CARGO_BIN_EXE_meshfox` through a real pty
    // (`portable_pty`) needs it re-signed first on macOS, unlike the plain
    // `std::process::Command` spawns the rest of this file uses.
    #[cfg(target_os = "macos")]
    {
        static SIGN_ONCE: std::sync::Once = std::sync::Once::new();
        SIGN_ONCE.call_once(|| {
            let status = std::process::Command::new("codesign")
                .args(["--force", "-s", "-", env!("CARGO_BIN_EXE_meshfox")])
                .status()
                .expect("run codesign");
            assert!(
                status.success(),
                "codesign failed to re-sign the meshfox binary"
            );
        });
    }

    let dir = unique_dir();
    let canvas_path = dir.join("doc.canvas.md");
    std::fs::write(
        &canvas_path,
        concat!(
            "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
            "```bash name=\"srv\" service\necho hello-from-srv\nsleep 30\n```\n",
        ),
    )
    .unwrap();

    let mut dummy = spawn_dummy_owner();
    let dummy_pid = dummy.id();
    seed_stale_service_lock(&canvas_path, "root", "srv", dummy_pid);

    let pty_system = native_pty_system();
    let pair = pty_system
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("openpty");
    let mut cmd = CommandBuilder::new(env!("CARGO_BIN_EXE_meshfox"));
    cmd.arg("run");
    cmd.arg("--canvas");
    cmd.arg(&canvas_path);
    cmd.arg("srv");
    cmd.env("HOME", unique_dir());
    let mut child = pair
        .slave
        .spawn_command(cmd)
        .expect("spawn meshfox run in pty");
    drop(pair.slave);

    let mut reader = pair.master.try_clone_reader().expect("clone pty reader");
    let mut writer = pair.master.take_writer().expect("pty writer");

    let mut output = String::new();
    let mut buf = [0u8; 4096];
    let mut sent_confirm = false;
    // Waits for the block's own streamed output line, not just "service
    // started" — that line arrives on its own `RunEvent::Output`, strictly
    // after `ServiceStarted`, so waiting for it also proves the retried run
    // kept draining events past the retry instead of stopping early.
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline && !output.contains("hello-from-srv") {
        match reader.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => output.push_str(&String::from_utf8_lossy(&buf[..n])),
        }
        if !sent_confirm && output.contains("kill it and retry?") {
            writer.write_all(b"y\n").expect("write y to pty");
            writer.flush().expect("flush pty");
            sent_confirm = true;
        }
    }

    assert!(
        sent_confirm,
        "never saw the kill-and-retry prompt — output so far:\n{output}"
    );
    assert!(
        output.contains("service started, pid"),
        "never saw 'service started' after confirming — output so far:\n{output}"
    );
    assert!(
        output.contains("hello-from-srv"),
        "the real srv block's own output never streamed — output so far:\n{output}"
    );

    // `try_wait` (not the raw `kill(pid, 0)` `is_alive` uses elsewhere in
    // this file) — SIGKILL leaves a zombie until its parent reaps it, and
    // this test *is* that parent (`dummy` is this process's own `Child`),
    // so a raw `kill(pid, 0)` probe would keep reporting "alive" even after
    // the retry's own kill landed, until the zombie is reaped away.
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut reaped = false;
    while !reaped && Instant::now() < deadline {
        if matches!(dummy.try_wait(), Ok(Some(_))) {
            reaped = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    // Reaps it either way (a no-op if the loop above already did) — a
    // timed-out `!reaped` would otherwise leave a zombie behind even after
    // the `assert!` below panics this test out.
    let _ = dummy.wait();
    assert!(
        reaped,
        "the seeded dummy owner (pid {dummy_pid}) should have been killed by the confirmed retry"
    );

    let _ = child.kill();
    std::fs::remove_dir_all(&dir).ok();
}
