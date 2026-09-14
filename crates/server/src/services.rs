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
use std::time::{Duration, Instant};

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
        let result = kill_process_group(self.pid);
        // Doesn't wait for `self.pid` to actually be gone first — a still-
        // running descendant that's already detached into its own group
        // (see `kill_orphaned_descendants`'s own doc comment) is
        // independent of `self.pid` by definition, so there's nothing to
        // gain by waiting on it here.
        kill_orphaned_descendants(self.pid);
        result
    }
}

/// Spawns `block` as a service and hands back a `ServiceHandle` whose
/// `status`/log stay live in the background regardless of whether anything
/// is watching them right now. **The caller must already hold this
/// address's lock** (`meshfox_core::service_lock::acquire`, at the exact
/// path `meshfox_core::service_lock_path(&canvas_path, &node_id,
/// &block_name)` computes — this recomputes and trusts the same path,
/// it doesn't acquire it) before calling this — this used to acquire it
/// internally, but the caller now needs to have already claimed the lock
/// as part of a possibly-larger, all-or-nothing batch (see the webui's own
/// up-front, whole-chain lock pass) before committing to spawning anything,
/// so a second `acquire` in here would just conflict with the caller's own.
/// Released by `ServiceHandle::stop` (explicit stop/restart) or by the
/// background drain task itself the moment it notices the process exited
/// on its own (`Crashed`) — either way, the lock's lifetime now exactly
/// tracks the process's own, not any one caller's request.
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
    // The lock the caller already claimed was necessarily acquired with a
    // placeholder pid (the real one doesn't exist until right *now*) — fix
    // it up to the real child pid so a later `is_alive`/force-kill against
    // this lock file actually targets the right process, not whatever
    // acquired it originally.
    let _ = meshfox_core::service_lock::update_owner_pid(&lock_path, pid);

    let status = Arc::new(Mutex::new(ServiceStatus::Running));
    let log = Arc::new(Mutex::new(RingBuffer::new(LOG_CAPACITY)));
    spawn_drain_task(proc, Arc::clone(&status), Arc::clone(&log), lock_path.clone());

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
/// looks at, anything that depends on `old`. `old.stop()` already released
/// `old`'s own lock, so this reacquires it itself before calling `spawn`
/// (see that function's own doc comment on why it no longer does this
/// internally) — nothing else can have taken it in between, since `stop`
/// and this call happen back to back with no `.await` for anyone else to
/// run in between.
pub fn restart(old: &ServiceHandle) -> io::Result<ServiceHandle> {
    old.stop()?;
    meshfox_core::service_lock::acquire(&old.lock_path, std::process::id(), &old.respawn.owner)?;
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
/// stop` already flipped it first. Also releases `lock_path` on a `Crashed`
/// exit — `stop` already releases it for a deliberate stop, but an
/// unexpected exit used to leave the lock file behind forever (until the
/// whole process exited or someone explicitly stopped it), which meant a
/// crashed service's own address stayed permanently "held by us" even
/// though nothing was actually running under it — quietly wrong even
/// before this module tried to generalize locking to every block kind.
/// How often the drain task below re-scans for `pid`'s own descendants
/// while it's still running — see `spawn_drain_task`'s own doc comment on
/// `seen_descendants` for why this has to happen *before* the process
/// actually exits, not just once afterward.
const DESCENDANT_SCAN_INTERVAL: Duration = Duration::from_millis(250);

fn spawn_drain_task(
    mut proc: SpawnedProcess,
    status: Arc<Mutex<ServiceStatus>>,
    log: Arc<Mutex<RingBuffer>>,
    lock_path: PathBuf,
) {
    tokio::spawn(async move {
        // Captured before `wait()` below — a `tokio::process::Child` can
        // stop reporting its own id once it's been reaped.
        let pid = proc.child.id().unwrap_or(0);
        // Every descendant pid of `pid` ever noticed while this process
        // was still alive — see `kill_orphaned_descendants`'s own doc
        // comment for the daemonizing-tool scenario this exists for.
        // Crucially, this has to accumulate *during* the loop below, not
        // just get scanned fresh once the process has already exited: the
        // kernel reparents an orphan away (to pid 1) essentially the
        // moment its own parent is reaped, which — for a normal, prompt
        // exit — has typically *already happened* by the time anything
        // downstream of `proc.child.wait()` gets around to looking, so a
        // single post-exit scan routinely finds nothing at all (confirmed
        // directly: the very first version of this fix, a one-shot scan
        // right after `wait()`, reliably missed the daemonized descendant
        // in `services::tests::
        // an_exit_leaving_a_daemonized_descendant_alive_still_gets_it_killed`).
        let mut seen_descendants: std::collections::HashSet<sysinfo::Pid> =
            std::collections::HashSet::new();
        let mut scan_interval = tokio::time::interval(DESCENDANT_SCAN_INTERVAL);
        loop {
            tokio::select! {
                line = proc.output_rx.recv() => {
                    match line {
                        Some((stream, text)) => log.lock().unwrap().push(stream, text),
                        None => break,
                    }
                }
                _ = scan_interval.tick() => {
                    seen_descendants.extend(descendant_pids(pid));
                }
            }
        }
        let exit_code = proc.child.wait().await.ok().and_then(|s| s.code()).unwrap_or(-1);
        // One last scan too — belt and suspenders alongside the
        // accumulated history above, for whatever's still reachable this
        // way (a descendant that hasn't been reparented away yet, say).
        seen_descendants.extend(descendant_pids(pid));
        for descendant in seen_descendants {
            // SAFETY: an ordinary, single `SIGKILL` by pid — no process-
            // group semantics involved, unlike `kill_process_group`.
            unsafe {
                libc::kill(descendant.as_u32() as libc::pid_t, libc::SIGKILL);
            }
        }
        let mut current = status.lock().unwrap();
        if !matches!(*current, ServiceStatus::Stopped) {
            *current = ServiceStatus::Crashed { exit_code };
            let _ = meshfox_core::service_lock::release(&lock_path);
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

/// One-shot convenience: `descendant_pids(pid)` (see its own doc comment),
/// `SIGKILL`ed directly by pid, not through `kill_process_group`'s whole-
/// group signal. Exists for what that whole-group kill can't reach: a
/// tool that daemonizes internally (forks, then has the fork call `setsid`
/// to detach into its own brand-new session and process group before
/// doing anything else) ends up with a live process that was never a
/// member of `pid`'s own group to begin with — confirmed directly against
/// a real service (an Elixir/Erlang app, `mix run --no-halt`): its own
/// tracked process exited normally, but left a `beam.smp` node still
/// running, fully reparented to pid 1, invisible to both
/// `kill_process_group` and (once the lock file this module released the
/// moment it saw that "normal" exit) to any later force-start's own
/// stale-owner kill too. A single call site's worth of best-effort — see
/// `spawn_drain_task`'s own repeated-scan use of `descendant_pids` for why
/// a caller watching a process across its *whole* lifetime needs more than
/// one call here to reliably catch this.
pub(crate) fn kill_orphaned_descendants(pid: u32) {
    for descendant in descendant_pids(pid) {
        // SAFETY: a single, ordinary `SIGKILL` by pid — no process-group
        // semantics involved here, unlike `kill_process_group` above.
        unsafe {
            libc::kill(descendant.as_u32() as libc::pid_t, libc::SIGKILL);
        }
    }
}

/// Every still-alive process currently descended from `pid` — children,
/// grandchildren, and so on — found by scanning every process this
/// machine currently has for one whose `parent()` matches something
/// already found reachable from `pid`, breadth-first (not a `pgrep`/`ps`
/// shell-out, reusing the `sysinfo` dependency `services::sample` already
/// has). A snapshot of *right now* only — see `spawn_drain_task`'s own
/// `seen_descendants` for why a caller that cares about a process that
/// might exit and get reparented away before it gets around to killing
/// anything needs to call this repeatedly over the watched process's
/// whole lifetime, not just once at the end.
fn descendant_pids(pid: u32) -> Vec<sysinfo::Pid> {
    if pid == 0 {
        return Vec::new();
    }
    let mut sys = sysinfo::System::new();
    sys.refresh_processes(sysinfo::ProcessesToUpdate::All, true);

    let root = sysinfo::Pid::from_u32(pid);
    let mut frontier = vec![root];
    let mut descendants = Vec::new();
    while let Some(parent) = frontier.pop() {
        for (candidate_pid, process) in sys.processes() {
            if process.parent() == Some(parent) {
                descendants.push(*candidate_pid);
                frontier.push(*candidate_pid);
            }
        }
    }
    descendants
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

    /// Test-only stand-in for what a real caller now must do itself before
    /// calling `spawn` — acquire the address's lock first (see `spawn`'s
    /// own doc comment on why it no longer does this internally).
    fn spawn_locked(
        node_id: String,
        block_name: String,
        block: meshfox_core::CodeBlock,
        env: HashMap<String, String>,
        cwd: PathBuf,
        canvas_path: PathBuf,
        owner: &str,
    ) -> io::Result<ServiceHandle> {
        let lock_path = meshfox_core::service_lock_path(&canvas_path, &node_id, &block_name);
        meshfox_core::service_lock::acquire(&lock_path, std::process::id(), owner)
            .map_err(io::Error::from)?;
        spawn(node_id, block_name, block, env, cwd, canvas_path, owner)
    }

    #[tokio::test]
    async fn spawn_acquires_the_lock_file_and_reports_running() {
        let canvas_path = tmp_canvas_path("spawn");
        let handle = spawn_locked(
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
        let handle = spawn_locked(
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
        let handle = spawn_locked(
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

    /// Regression test for a real orphan found against a live document
    /// (`amulettie/search/README.canvas.md`'s `app` service — `mix run
    /// --no-halt`, an Elixir/Erlang app): its own tracked process exits
    /// normally at some point, but a `beam.smp` node it started keeps
    /// running, fully reparented to pid 1 — daemonized via the classic
    /// double-fork-and-`setsid` pattern, which detaches the real long-lived
    /// process into its own brand-new session/process group *before*
    /// anything here ever gets a chance to see it as part of `pid`'s own
    /// group. `kill_process_group` alone can never reach a process that
    /// was never in that group to begin with — this simulates the same
    /// shape with a small Python script instead of a real Erlang install,
    /// and checks `kill_orphaned_descendants` (called from the drain
    /// task's own unexpected-exit path) catches it anyway.
    #[tokio::test]
    async fn an_exit_leaving_a_daemonized_descendant_alive_still_gets_it_killed() {
        let canvas_path = tmp_canvas_path("daemonize");
        let dir = canvas_path.parent().unwrap().to_path_buf();
        let marker = dir.join("descendant.pid");
        // The detached child also closes its own inherited stdout/stderr
        // (redirecting to `/dev/null`) — same as a real well-behaved
        // daemon (confirmed BEAM itself does this too), and necessary
        // here for the same reason: without it, the pipe `stream_exec`
        // reads this service's own output from would stay held open by
        // the still-running descendant even after the wrapper (the
        // parent branch, right below) exits, so the wrapper's own exit
        // would never even be observed as EOF in the first place. The
        // parent branch also waits a bit before exiting — `spawn_drain_
        // task`'s own periodic scan (`DESCENDANT_SCAN_INTERVAL`) needs at
        // least one real tick to land while the descendant is still
        // reachable through the wrapper; a real daemonizing service
        // (BEAM's own boot alone takes whole seconds) always has far more
        // slack than this in practice — an immediate exit right after
        // forking would race the very first scan for no reason this test
        // needs to reproduce.
        let script = format!(
            "python3 <<'PYEOF'\nimport os, sys, time\npid = os.fork()\nif pid > 0:\n    time.sleep(1)\n    sys.exit(0)\nos.setsid()\ndevnull = os.open(os.devnull, os.O_RDWR)\nos.dup2(devnull, 0)\nos.dup2(devnull, 1)\nos.dup2(devnull, 2)\nwith open({marker:?}, 'w') as f:\n    f.write(str(os.getpid()))\ntime.sleep(30)\nPYEOF\n",
        );

        let handle = spawn_locked(
            "root".to_string(),
            "srv".to_string(),
            test_block(&script),
            HashMap::new(),
            dir.clone(),
            canvas_path.clone(),
            "cli",
        )
        .unwrap();

        // Wait for the daemonized descendant to exist and report its own
        // (freshly detached) pid.
        let mut descendant_pid: Option<u32> = None;
        for _ in 0..100 {
            if let Ok(s) = std::fs::read_to_string(&marker) {
                if let Ok(p) = s.trim().parse::<u32>() {
                    descendant_pid = Some(p);
                    break;
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        let descendant_pid = descendant_pid.expect("descendant never wrote its own pid");
        assert!(
            meshfox_core::service_lock::is_alive(descendant_pid),
            "descendant should be running before its wrapper exits"
        );

        // Wait for the wrapper's own exit to be noticed — an *unexpected*
        // one, `Crashed`, since nothing here ever called `stop()`.
        let mut status = handle.status();
        for _ in 0..100 {
            if !matches!(status, ServiceStatus::Running) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            status = handle.status();
        }
        assert!(
            matches!(status, ServiceStatus::Crashed { .. }),
            "wrapper should be reported crashed: {status:?}"
        );

        // The whole point: the daemonized descendant shouldn't survive
        // that, even though it was never a member of the wrapper's own
        // process group by the time anything tried to clean it up.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut still_alive = meshfox_core::service_lock::is_alive(descendant_pid);
        while still_alive && std::time::Instant::now() < deadline {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            still_alive = meshfox_core::service_lock::is_alive(descendant_pid);
        }
        assert!(
            !still_alive,
            "daemonized descendant pid {descendant_pid} survived its wrapper's exit — orphaned, not swept up"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn log_snapshot_captures_output_lines() {
        let canvas_path = tmp_canvas_path("log");
        let handle = spawn_locked(
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
        let handle = spawn_locked(
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
