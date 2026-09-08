//! End-to-end proof of `meshfox run`'s own `service`-block behavior (see
//! SPEC.md's "Service blocks (experimental)"): unlike an ordinary chain,
//! the process stays attached once a `service` block has been spawned —
//! streaming its output to the console — until Ctrl-C, rather than exiting
//! the instant the chain finishes.

use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

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
    Command::new(env!("CARGO_BIN_EXE_meshfox"))
}

fn is_alive(pid: u32) -> bool {
    // SAFETY: signal 0 sends nothing, just probes existence/permission.
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
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
    assert!(saw_output, "never saw the service's own streamed output line");
    assert!(
        saw_streaming_banner,
        "never saw the 'staying attached' banner — run exited instead of watching the service"
    );
    let service_pid = service_pid.expect("service pid parsed from the started line");
    assert!(is_alive(service_pid), "service should be running at this point");

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
