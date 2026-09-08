//! Registry of live `service` blocks (see `meshfox_core::CodeBlock::service`
//! and SPEC.md's "Service blocks (experimental)") — the persistent state a
//! `run_block` step branches into instead of waiting for exit (see lib.rs),
//! and what the new `/api/services*` endpoints and the TUI's own service
//! view read/act on. Unlike `AppState::runs` (a per-HTTP-stream kill-switch,
//! cleared the moment that stream closes), an entry here outlives any
//! individual request — it holds the actual live process and a retained
//! log, so a client can discover/observe/stop/restart a service across
//! page reloads, not just within the request that started it.
//!
//! The TUI links this crate as a library rather than talking to it over
//! HTTP (see `stream_exec`'s own doc comment), so it holds its own
//! `HashMap<(String, String), ServiceHandle>` built from these same
//! functions — there's no cross-process sharing between a `meshfox view`
//! server and a `meshfox tui` pointed at the same canvas, only the on-disk
//! lock file (`meshfox_core::service_lock`) keeps the two honest about who
//! owns what.

use crate::stream_exec::{self, OutputStream, SpawnedProcess};
use std::collections::{HashMap, VecDeque};
use std::io;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// How many recent output lines a service's log keeps — old lines fall off
/// the front once this fills up. Generous enough for a dev server's own
/// startup chatter plus a good while of steady-state logging, small enough
/// to never be a real memory concern for a background process nobody's
/// actively piping megabytes of output through.
const LOG_CAPACITY: usize = 2000;

#[derive(Debug, Clone, PartialEq)]
pub enum ServiceStatus {
    Running,
    /// Exited on its own, unexpectedly — `exit_code` is whatever its own
    /// process reported (`-1` if that couldn't be determined).
    Crashed { exit_code: i32 },
    /// Exited because something here (`ServiceHandle::stop`, or a restart)
    /// killed it on purpose — distinguished from `Crashed` so the UI shows
    /// "stopped" rather than a false crash after a deliberate Stop click.
    Stopped,
}

/// Bounded (stdout, stderr)-tagged line buffer — same tagging
/// `stream_exec::SpawnedProcess::output_rx` already uses, just retained
/// instead of streamed straight into an HTTP response the way a normal
/// chain step's output is.
struct RingBuffer {
    cap: usize,
    lines: VecDeque<(OutputStream, String)>,
}

impl RingBuffer {
    fn new(cap: usize) -> Self {
        RingBuffer { cap, lines: VecDeque::new() }
    }

    fn push(&mut self, stream: OutputStream, line: String) {
        if self.lines.len() >= self.cap {
            self.lines.pop_front();
        }
        self.lines.push_back((stream, line));
    }

    fn snapshot(&self) -> Vec<(OutputStream, String)> {
        self.lines.iter().cloned().collect()
    }
}

/// Everything `restart` needs to spawn an equivalent process again, cached
/// from the original spawn rather than re-resolved from the canvas — a
/// deliberate simplification (see SPEC.md's own note on this): restart
/// reruns the exact command this service was last started with, it doesn't
/// re-walk `deps=`/`meshfox:var` resolution the way starting a chain from
/// scratch does. Good enough for "restart this one process" without
/// dragging in the whole variable-resolution machinery `run_block` has.
struct RespawnRecipe {
    block: meshfox_core::CodeBlock,
    env: HashMap<String, String>,
    cwd: PathBuf,
    canvas_path: PathBuf,
    owner: String,
}

/// One running (or just-stopped) service — see the module doc comment.
pub struct ServiceHandle {
    pub node_id: String,
    pub block_name: String,
    pub pid: u32,
    pub started_at: Instant,
    pub lock_path: PathBuf,
    status: Arc<Mutex<ServiceStatus>>,
    log: Arc<Mutex<RingBuffer>>,
    respawn: RespawnRecipe,
}

impl ServiceHandle {
    pub fn status(&self) -> ServiceStatus {
        self.status.lock().unwrap().clone()
    }

    pub fn log_snapshot(&self) -> Vec<(OutputStream, String)> {
        self.log.lock().unwrap().snapshot()
    }

    /// Kills the whole process group (see `kill_process_group`) and marks
    /// this handle `Stopped` — set *before* signaling, so the background
    /// drain task (below) sees `Stopped` already recorded when it notices
    /// the process gone, rather than racing it and reporting a false
    /// `Crashed`.
    pub fn stop(&self) -> io::Result<()> {
        *self.status.lock().unwrap() = ServiceStatus::Stopped;
        let _ = meshfox_core::service_lock::release(&self.lock_path);
        kill_process_group(self.pid)
    }
}

/// Spawns `block` as a service: starts the process, acquires its lock file
/// (`meshfox_core::service_lock`, at `lock_path` — the caller is expected to
/// have already resolved any conflict via `service_lock::check` before
/// calling this), and hands back a `ServiceHandle` whose `status`/log stay
/// live in the background regardless of whether anything is watching them
/// right now.
pub fn spawn(
    node_id: String,
    block_name: String,
    block: meshfox_core::CodeBlock,
    env: HashMap<String, String>,
    cwd: PathBuf,
    canvas_path: PathBuf,
    owner: &str,
) -> io::Result<ServiceHandle> {
    let lock_path = meshfox_core::service_lock_path(&canvas_path, &node_id, &block_name);
    let proc = stream_exec::spawn_block(&block, &env, Some(&cwd), Some(&canvas_path))?;
    let pid = proc.child.id().unwrap_or(0);
    meshfox_core::service_lock::acquire(&lock_path, pid, owner)?;

    let status = Arc::new(Mutex::new(ServiceStatus::Running));
    let log = Arc::new(Mutex::new(RingBuffer::new(LOG_CAPACITY)));
    spawn_drain_task(proc, Arc::clone(&status), Arc::clone(&log));

    Ok(ServiceHandle {
        node_id,
        block_name,
        pid,
        started_at: Instant::now(),
        lock_path,
        status,
        log,
        respawn: RespawnRecipe { block, env, cwd, canvas_path, owner: owner.to_string() },
    })
}

/// Stops `old` and spawns a fresh process with the exact parameters it was
/// last started with (see `RespawnRecipe`'s own doc comment) — "local
/// only" restart, per the product decision: this never touches, or even
/// looks at, anything that depends on `old`.
pub fn restart(old: &ServiceHandle) -> io::Result<ServiceHandle> {
    old.stop()?;
    spawn(
        old.node_id.clone(),
        old.block_name.clone(),
        old.respawn.block.clone(),
        old.respawn.env.clone(),
        old.respawn.cwd.clone(),
        old.respawn.canvas_path.clone(),
        &old.respawn.owner,
    )
}

/// Drains `proc`'s output into `log` for as long as it runs, then reaps it
/// and records whatever `ServiceStatus` that leaves it in — `Crashed` for
/// an unexpected exit, left alone (already `Stopped`) if `ServiceHandle::
/// stop` already flipped it first.
fn spawn_drain_task(
    mut proc: SpawnedProcess,
    status: Arc<Mutex<ServiceStatus>>,
    log: Arc<Mutex<RingBuffer>>,
) {
    tokio::spawn(async move {
        while let Some((stream, line)) = proc.output_rx.recv().await {
            log.lock().unwrap().push(stream, line);
        }
        let exit_code = proc.child.wait().await.ok().and_then(|s| s.code()).unwrap_or(-1);
        let mut current = status.lock().unwrap();
        if !matches!(*current, ServiceStatus::Stopped) {
            *current = ServiceStatus::Crashed { exit_code };
        }
    });
}

/// Same `SIGKILL`-the-whole-process-group primitive `stream_exec::
/// SpawnedProcess::kill`/`pty_exec::PtyProcess::kill` each already carry
/// their own copy of — duplicated rather than shared because a service's
/// `SpawnedProcess` has already been moved into `spawn_drain_task` by the
/// time `stop` needs to kill it, so there's no `&SpawnedProcess` left to
/// call a method on; only the bare `pid` survives in `ServiceHandle`.
fn kill_process_group(pid: u32) -> io::Result<()> {
    if pid == 0 {
        return Ok(());
    }
    // SAFETY: see `stream_exec::SpawnedProcess::kill` — `-pid` signals the
    // whole process group this pid leads (every spawn function in
    // `stream_exec` makes its child a fresh group leader).
    let ret = unsafe { libc::kill(-(pid as libc::pid_t), libc::SIGKILL) };
    if ret != 0 && io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH) {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// A service's CPU/memory usage right now — `None` if `pid` isn't a
/// process this machine currently knows about (already exited, or a
/// permissions boundary). Backs both the webui's `GET /api/services` and
/// the TUI's own service view (see the module doc comment on why the TUI
/// calls this directly rather than over HTTP).
pub struct ResourceSample {
    /// Percent of one CPU core, e.g. `150.0` for a process using 1.5 cores
    /// — same units `sysinfo::Process::cpu_usage` itself reports. Reuses
    /// one process-wide `System` (see `system()`) across calls so this
    /// reflects usage *since the last sample*, not a meaningless
    /// first-ever-refresh reading of `0.0`.
    pub cpu_percent: f32,
    pub mem_bytes: u64,
}

fn system() -> &'static Mutex<sysinfo::System> {
    static SYSTEM: std::sync::OnceLock<Mutex<sysinfo::System>> = std::sync::OnceLock::new();
    SYSTEM.get_or_init(|| Mutex::new(sysinfo::System::new()))
}

pub fn sample(pid: u32) -> Option<ResourceSample> {
    let sys_pid = sysinfo::Pid::from_u32(pid);
    let mut sys = system().lock().unwrap();
    sys.refresh_processes(sysinfo::ProcessesToUpdate::Some(&[sys_pid]), true);
    sys.process(sys_pid).map(|p| ResourceSample {
        cpu_percent: p.cpu_usage(),
        mem_bytes: p.memory(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_block(code: &str) -> meshfox_core::CodeBlock {
        let md = format!("```bash name=\"x\" service\n{code}\n```\n");
        meshfox_core::scan_code_blocks(&md).remove(0)
    }

    fn tmp_canvas_path(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("meshfox-services-test-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("doc.canvas.md")
    }

    #[tokio::test]
    async fn spawn_acquires_the_lock_file_and_reports_running() {
        let canvas_path = tmp_canvas_path("spawn");
        let handle = spawn(
            "root".to_string(),
            "srv".to_string(),
            test_block("sleep 30"),
            HashMap::new(),
            canvas_path.parent().unwrap().to_path_buf(),
            canvas_path.clone(),
            "cli",
        )
        .unwrap();

        assert_eq!(handle.status(), ServiceStatus::Running);
        let lock = meshfox_core::service_lock::check(&handle.lock_path).unwrap();
        assert_eq!(
            lock,
            meshfox_core::ServiceLockState::Held {
                info: meshfox_core::ServiceLockInfo { pid: handle.pid, owner: "cli".to_string() },
                alive: true,
            }
        );

        handle.stop().unwrap();
        std::fs::remove_dir_all(canvas_path.parent().unwrap()).ok();
    }

    #[tokio::test]
    async fn stop_releases_the_lock_and_marks_stopped_not_crashed() {
        let canvas_path = tmp_canvas_path("stop");
        let handle = spawn(
            "root".to_string(),
            "srv".to_string(),
            test_block("sleep 30"),
            HashMap::new(),
            canvas_path.parent().unwrap().to_path_buf(),
            canvas_path.clone(),
            "cli",
        )
        .unwrap();
        let lock_path = handle.lock_path.clone();

        handle.stop().unwrap();
        assert_eq!(handle.status(), ServiceStatus::Stopped);
        assert_eq!(
            meshfox_core::service_lock::check(&lock_path).unwrap(),
            meshfox_core::ServiceLockState::Free
        );

        // Give the background drain task a moment to actually notice the
        // process is gone and run its own status update — it must not
        // clobber the `Stopped` `stop()` already recorded.
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        assert_eq!(handle.status(), ServiceStatus::Stopped);

        std::fs::remove_dir_all(canvas_path.parent().unwrap()).ok();
    }

    #[tokio::test]
    async fn an_unexpected_exit_is_reported_as_crashed() {
        let canvas_path = tmp_canvas_path("crash");
        let handle = spawn(
            "root".to_string(),
            "srv".to_string(),
            test_block("exit 7"),
            HashMap::new(),
            canvas_path.parent().unwrap().to_path_buf(),
            canvas_path.clone(),
            "cli",
        )
        .unwrap();

        // Poll briefly for the background drain task to notice the exit —
        // avoids a fixed sleep racing on a slow CI machine.
        let mut status = handle.status();
        for _ in 0..50 {
            if !matches!(status, ServiceStatus::Running) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            status = handle.status();
        }
        assert_eq!(status, ServiceStatus::Crashed { exit_code: 7 });

        std::fs::remove_dir_all(canvas_path.parent().unwrap()).ok();
    }

    #[tokio::test]
    async fn log_snapshot_captures_output_lines() {
        let canvas_path = tmp_canvas_path("log");
        let handle = spawn(
            "root".to_string(),
            "srv".to_string(),
            test_block("echo one; echo two; sleep 30"),
            HashMap::new(),
            canvas_path.parent().unwrap().to_path_buf(),
            canvas_path.clone(),
            "cli",
        )
        .unwrap();

        let mut lines = handle.log_snapshot();
        for _ in 0..50 {
            if lines.len() >= 2 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            lines = handle.log_snapshot();
        }
        assert_eq!(
            lines,
            vec![
                (OutputStream::Stdout, "one".to_string()),
                (OutputStream::Stdout, "two".to_string()),
            ]
        );

        handle.stop().unwrap();
        std::fs::remove_dir_all(canvas_path.parent().unwrap()).ok();
    }

    #[tokio::test]
    async fn restart_stops_the_old_process_and_starts_a_new_one_with_a_different_pid() {
        let canvas_path = tmp_canvas_path("restart");
        let handle = spawn(
            "root".to_string(),
            "srv".to_string(),
            test_block("sleep 30"),
            HashMap::new(),
            canvas_path.parent().unwrap().to_path_buf(),
            canvas_path.clone(),
            "cli",
        )
        .unwrap();
        let old_pid = handle.pid;

        let restarted = restart(&handle).unwrap();
        assert_eq!(handle.status(), ServiceStatus::Stopped);
        assert_eq!(restarted.status(), ServiceStatus::Running);
        assert_ne!(restarted.pid, old_pid);

        restarted.stop().unwrap();
        std::fs::remove_dir_all(canvas_path.parent().unwrap()).ok();
    }
}
