//! Registry of live `service` blocks (see `meshfox_core::CodeBlock::service`
//! and SPEC.md's "Service blocks (experimental)") — the persistent state a
//! `run_block` step branches into instead of waiting for exit (see lib.rs),
//! and what the `/api/services*` endpoints (and, through them, the TUI and
//! CLI's own service views) read/act on. Unlike `AppState::runs` (a per-
//! HTTP-stream kill-switch, cleared the moment that stream closes), an
//! entry here outlives any individual request — it holds the actual live
//! process and a retained log, so a client can discover/observe/stop/
//! restart a service across page reloads, not just within the request that
//! started it.
//!
//! Conflict detection against another core process for the same canvas
//! (there's only ever one now — see `AppState::run_ledger`) goes through
//! `crate::run_ledger` — `ServiceHandle` carries the ledger row its own
//! address was claimed under, and resolves it (`finish`) whenever it stops,
//! restarts, or crashes.

use crate::stream_exec::{self, OutputStream};
use tokio::sync::watch;

#[path = "service_runtime.rs"]
mod runtime;
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
    Starting,
    Stopping,
    Restarting,
    StopFailed {
        message: String,
    },
    /// Exited on its own, unexpectedly — `exit_code` is whatever its own
    /// process reported (`-1` if that couldn't be determined).
    Crashed {
        exit_code: i32,
    },
    /// Exited because something here (`ServiceHandle::stop`, or a restart)
    /// killed it on purpose — distinguished from `Crashed` so the UI shows
    /// "stopped" rather than a false crash after a deliberate Stop click.
    Stopped,
}

/// Bounded (stdout, stderr)-tagged line buffer — same tagging
/// `stream_exec::SpawnedProcess::output_rx` already uses, just retained
/// instead of streamed straight into an HTTP response the way a normal
/// chain step's output is.
pub(super) struct RingBuffer {
    cap: usize,
    lines: VecDeque<(OutputStream, String)>,
}

impl RingBuffer {
    fn new(cap: usize) -> Self {
        RingBuffer {
            cap,
            lines: VecDeque::new(),
        }
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
#[derive(Clone)]
struct RespawnRecipe {
    block: meshfox_core::CodeBlock,
    env: HashMap<String, String>,
    environment: Arc<Vec<(std::ffi::OsString, std::ffi::OsString)>>,
    cwd: PathBuf,
    canvas_path: PathBuf,
    owner: String,
}

#[derive(Default)]
struct Leases {
    temporary: bool,
    runs: std::collections::HashSet<String>,
}

/// One running (or just-stopped) service — see the module doc comment.
#[derive(Clone)]
pub struct ServiceHandle {
    pub node_id: String,
    pub block_name: String,
    pub pid: u32,
    pub started_at: Instant,
    ledger: crate::run_ledger::RunLedger,
    status: Arc<Mutex<ServiceStatus>>,
    log: Arc<Mutex<RingBuffer>>,
    respawn: RespawnRecipe,
    control: watch::Sender<u64>,
    completion: watch::Receiver<(u64, Result<(), String>)>,
    action: Arc<tokio::sync::Mutex<()>>,
    epoch: Arc<std::sync::atomic::AtomicI64>,
    leases: Arc<Mutex<Leases>>,
    force_stop: Arc<std::sync::atomic::AtomicBool>,
}

impl ServiceHandle {
    /// Unique for each spawn/restart, including if the OS reuses a PID.
    pub fn generation(&self) -> i64 {
        self.epoch.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// A newly lease-started service is temporary; an existing ordinary
    /// service stays persistent. The action lock serializes last-release/Stop.
    pub async fn acquire_lease(&self, run: &str, newly_started: bool) -> io::Result<()> {
        let _action = self.action.lock().await;
        if !self.is_live() {
            return Err(io::Error::other("service stopped before lease acquisition"));
        }
        let mut leases = self.leases.lock().unwrap();
        if newly_started {
            leases.temporary = true;
        }
        leases.runs.insert(run.into());
        Ok(())
    }
    pub async fn retain(&self) {
        let _action = self.action.lock().await;
        self.leases.lock().unwrap().temporary = false;
    }
    pub async fn release_lease(&self, run: &str) -> io::Result<()> {
        let _action = self.action.lock().await;
        let stop = {
            let mut leases = self.leases.lock().unwrap();
            leases.runs.remove(run) && leases.temporary && leases.runs.is_empty()
        };
        if stop {
            if let ServiceStatus::StopFailed { message } = self.status() {
                return Err(io::Error::other(message));
            }
            self.stop_inner(false).await?;
        }
        Ok(())
    }

    pub async fn wait_ready(&self, recheck: bool) -> io::Result<()> {
        let config = if self.respawn.block.interpreter.as_deref() == Some("@service") {
            Some(meshfox_core::service::ServiceConfig::parse(
                &self.respawn.block.code,
            )?)
        } else {
            None
        };
        let timeout = config
            .as_ref()
            .map_or(Duration::from_secs(30), |c| c.start_timeout);
        let deadline = Instant::now() + timeout;
        loop {
            match self.status() {
                ServiceStatus::Running => break,
                ServiceStatus::Starting | ServiceStatus::Restarting => {}
                status => {
                    return Err(io::Error::other(format!(
                        "service did not become ready: {status:?}"
                    )))
                }
            }
            if Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "service start timed out",
                ));
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        if recheck {
            if let Some(config) = config {
                if let Some(ready) = config.ready {
                    runtime::ready_probe(
                        &ready,
                        config.start_timeout,
                        &self.respawn.environment,
                        &self.respawn.cwd,
                        &self.status,
                        &self.log,
                    )
                    .await?;
                }
            }
        }
        Ok(())
    }

    pub fn autostop(&self) -> bool {
        self.respawn
            .block
            .attrs
            .get("autostop")
            .is_some_and(|v| v != "false")
    }

    pub fn status(&self) -> ServiceStatus {
        self.status.lock().unwrap().clone()
    }

    pub fn log_snapshot(&self) -> Vec<(OutputStream, String)> {
        self.log.lock().unwrap().snapshot()
    }

    /// Services keep their address claimed throughout graceful shutdown.
    pub async fn stop(&self) -> io::Result<()> {
        let _action = self.action.lock().await;
        {
            let mut leases = self.leases.lock().unwrap();
            leases.temporary = false;
            leases.runs.clear();
        }
        self.stop_inner(true).await
    }

    /// Automatic release/shutdown detaches a reused external resource.
    pub async fn stop_automatic(&self) -> io::Result<()> {
        let _action = self.action.lock().await;
        self.stop_inner(false).await
    }

    async fn stop_inner(&self, force: bool) -> io::Result<()> {
        if force {
            self.force_stop
                .store(true, std::sync::atomic::Ordering::SeqCst);
        }
        let request;
        {
            let mut status = self.status.lock().unwrap();
            if matches!(
                *status,
                ServiceStatus::Stopped | ServiceStatus::Crashed { .. }
            ) {
                return Ok(());
            }
            request = *self.control.borrow() + 1;
            *status = ServiceStatus::Stopping;
            self.control.send_replace(request);
        }
        let mut completion = self.completion.clone();
        loop {
            let (finished, result) = completion.borrow_and_update().clone();
            if finished >= request {
                return result.map_err(io::Error::other);
            }
            if completion.changed().await.is_err() {
                return if matches!(
                    self.status(),
                    ServiceStatus::Stopped | ServiceStatus::Crashed { .. }
                ) {
                    Ok(())
                } else {
                    Err(io::Error::other("service supervisor exited"))
                };
            }
        }
    }

    pub fn is_live(&self) -> bool {
        !matches!(
            self.status(),
            ServiceStatus::Stopped | ServiceStatus::Crashed { .. }
        )
    }
}

/// Spawns `block` as a service and hands back a `ServiceHandle` whose
/// `status`/log stay live in the background regardless of whether anything
/// is watching them right now. **The caller must already hold this
/// address's `run_ledger` row** (`crate::run_ledger::RunLedger::start`,
/// `ledger_row_id`) before calling this — this used to acquire a file lock
/// internally, but the caller now needs to have already claimed the row as
/// part of a possibly-larger, all-or-nothing batch (see the webui's own
/// up-front, whole-chain lock pass) before committing to spawning anything,
/// so a second claim in here would just conflict with the caller's own.
/// Finished (`RunLedger::finish`) by `ServiceHandle::stop` (explicit stop/
/// restart) or by the background drain task itself the moment it notices
/// the process exited on its own (`Crashed`) — either way, the row's
/// lifetime now exactly tracks the process's own, not any one caller's
/// request.
#[allow(clippy::too_many_arguments)]
pub fn spawn(
    node_id: String,
    block_name: String,
    block: meshfox_core::CodeBlock,
    env: HashMap<String, String>,
    cwd: PathBuf,
    canvas_path: PathBuf,
    owner: &str,
    ledger: crate::run_ledger::RunLedger,
    ledger_row_id: i64,
) -> io::Result<ServiceHandle> {
    let prepared = meshfox_core::process_env::prepare(Some(&cwd), &env, &[]);
    let mut environment: std::collections::BTreeMap<_, _> = std::env::vars_os().collect();
    for name in prepared.remove {
        environment.remove(&name);
    }
    environment.extend(prepared.values);
    spawn_with_environment(
        node_id,
        block_name,
        block,
        env,
        cwd,
        canvas_path,
        owner,
        ledger,
        ledger_row_id,
        Arc::new(environment.into_iter().collect()),
    )
}

#[allow(clippy::too_many_arguments)]
fn spawn_with_environment(
    node_id: String,
    block_name: String,
    block: meshfox_core::CodeBlock,
    env: HashMap<String, String>,
    cwd: PathBuf,
    canvas_path: PathBuf,
    owner: &str,
    ledger: crate::run_ledger::RunLedger,
    ledger_row_id: i64,
    environment: Arc<Vec<(std::ffi::OsString, std::ffi::OsString)>>,
) -> io::Result<ServiceHandle> {
    let config = if block.interpreter.as_deref() == Some("@service") {
        Some(meshfox_core::service::ServiceConfig::parse(&block.code)?)
    } else {
        None
    };
    // A declarative service has a stable, separately killable identity even
    // while `start` has exited or an automatic restart is waiting. It never
    // uses the worker's own PID as a force-takeover target.
    let proc = if config.is_some() {
        stream_exec::spawn_bash_frozen("exec sleep 2147483647", &environment, &cwd)?
    } else {
        stream_exec::spawn_block(&block, &env, Some(&cwd), Some(&canvas_path))?
    };
    let pid = proc.child.id().unwrap_or(0);
    ledger.update_pid(ledger_row_id, pid)?;
    let status = Arc::new(Mutex::new(if config.is_some() {
        ServiceStatus::Starting
    } else {
        ServiceStatus::Running
    }));
    let log = Arc::new(Mutex::new(RingBuffer::new(LOG_CAPACITY)));
    let (control, stop_rx) = watch::channel(0);
    let (completed, completion) = watch::channel((0, Ok(())));
    let epoch = Arc::new(std::sync::atomic::AtomicI64::new(ledger_row_id));
    let force_stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    runtime::spawn(
        proc,
        config,
        environment.clone(),
        cwd.clone(),
        status.clone(),
        log.clone(),
        ledger.clone(),
        ledger_row_id,
        stop_rx,
        completed,
        epoch.clone(),
        force_stop.clone(),
    );

    Ok(ServiceHandle {
        node_id,
        block_name,
        pid,
        started_at: Instant::now(),
        ledger,
        status,
        log,
        control,
        completion,
        action: Arc::new(tokio::sync::Mutex::new(())),
        epoch,
        force_stop,
        leases: Arc::new(Mutex::new(Leases::default())),
        respawn: RespawnRecipe {
            block,
            env,
            environment,
            cwd,
            canvas_path,
            owner: owner.to_string(),
        },
    })
}

/// Await the previous generation's cleanup before claiming a new row.
/// The saved recipe deliberately does not re-resolve edited canvas content.
pub async fn restart(old: &ServiceHandle) -> io::Result<ServiceHandle> {
    let _action = old.action.lock().await;
    old.stop_inner(true).await?;
    let new_id = match old.ledger.start(
        &old.node_id,
        &old.block_name,
        crate::run_ledger::RunKind::Service,
        &old.respawn.owner,
        std::process::id(),
    ) {
        Ok(id) => id,
        Err(crate::run_ledger::StartError::Conflict(info)) => {
            return Err(io::Error::other(format!(
                "address still claimed immediately after stop (pid {}, {})",
                info.pid, info.owner
            )));
        }
        Err(crate::run_ledger::StartError::Io(e)) => return Err(e),
    };
    let result = spawn_with_environment(
        old.node_id.clone(),
        old.block_name.clone(),
        old.respawn.block.clone(),
        old.respawn.env.clone(),
        old.respawn.cwd.clone(),
        old.respawn.canvas_path.clone(),
        &old.respawn.owner,
        old.ledger.clone(),
        new_id,
        old.respawn.environment.clone(),
    );
    if result.is_err() {
        let _ = old
            .ledger
            .finish(new_id, crate::run_ledger::FinishOutcome::Killed);
    }
    result
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
/// `runtime::Process`'s own repeated-scan use of `descendant_pids` for why
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
/// has). A snapshot of *right now* only — see `runtime::Process`'s own
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
        let dir = std::env::temp_dir().join(format!(
            "meshfox-services-test-{name}-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("doc.canvas.md")
    }

    /// Test-only stand-in for what a real caller now must do itself before
    /// calling `spawn` — claim the address's `run_ledger` row first (see
    /// `spawn`'s own doc comment on why it no longer does this internally).
    #[allow(clippy::too_many_arguments)]
    fn spawn_locked(
        ledger: &crate::run_ledger::RunLedger,
        node_id: String,
        block_name: String,
        block: meshfox_core::CodeBlock,
        env: HashMap<String, String>,
        cwd: PathBuf,
        canvas_path: PathBuf,
        owner: &str,
    ) -> io::Result<ServiceHandle> {
        let id = ledger
            .start(
                &node_id,
                &block_name,
                crate::run_ledger::RunKind::Service,
                owner,
                std::process::id(),
            )
            .map_err(io::Error::other)?;
        spawn(
            node_id,
            block_name,
            block,
            env,
            cwd,
            canvas_path,
            owner,
            ledger.clone(),
            id,
        )
    }

    #[tokio::test]
    async fn spawn_claims_the_ledger_row_and_reports_running() {
        let ledger = crate::run_ledger::RunLedger::open_in_memory().unwrap();
        let canvas_path = tmp_canvas_path("spawn");
        let handle = spawn_locked(
            &ledger,
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
        let active = ledger.active_running().unwrap();
        assert_eq!(active.len(), 1, "active: {active:?}");
        assert_eq!(active[0].node_id, "root");
        assert_eq!(active[0].block, "srv");
        assert_eq!(active[0].pid, handle.pid);
        assert_eq!(active[0].owner, "cli");

        handle.stop().await.unwrap();
        std::fs::remove_dir_all(canvas_path.parent().unwrap()).ok();
    }

    #[tokio::test]
    async fn stop_finishes_the_ledger_row_and_marks_stopped_not_crashed() {
        let ledger = crate::run_ledger::RunLedger::open_in_memory().unwrap();
        let canvas_path = tmp_canvas_path("stop");
        let handle = spawn_locked(
            &ledger,
            "root".to_string(),
            "srv".to_string(),
            test_block("sleep 30"),
            HashMap::new(),
            canvas_path.parent().unwrap().to_path_buf(),
            canvas_path.clone(),
            "cli",
        )
        .unwrap();

        handle.stop().await.unwrap();
        assert_eq!(handle.status(), ServiceStatus::Stopped);
        assert!(ledger.active_running().unwrap().is_empty());

        // Give the background drain task a moment to actually notice the
        // process is gone and run its own status update — it must not
        // clobber the `Stopped` `stop()` already recorded.
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        assert_eq!(handle.status(), ServiceStatus::Stopped);

        std::fs::remove_dir_all(canvas_path.parent().unwrap()).ok();
    }

    #[tokio::test]
    async fn an_unexpected_exit_is_reported_as_crashed() {
        let ledger = crate::run_ledger::RunLedger::open_in_memory().unwrap();
        let canvas_path = tmp_canvas_path("crash");
        let handle = spawn_locked(
            &ledger,
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
        assert!(
            ledger.active_running().unwrap().is_empty(),
            "the crashed row should have been finished too, not left running"
        );

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

        let ledger = crate::run_ledger::RunLedger::open_in_memory().unwrap();
        let handle = spawn_locked(
            &ledger,
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
        let ledger = crate::run_ledger::RunLedger::open_in_memory().unwrap();
        let canvas_path = tmp_canvas_path("log");
        let handle = spawn_locked(
            &ledger,
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

        handle.stop().await.unwrap();
        std::fs::remove_dir_all(canvas_path.parent().unwrap()).ok();
    }

    #[tokio::test]
    async fn restart_stops_the_old_process_and_starts_a_new_one_with_a_different_pid() {
        let ledger = crate::run_ledger::RunLedger::open_in_memory().unwrap();
        let canvas_path = tmp_canvas_path("restart");
        let handle = spawn_locked(
            &ledger,
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

        let restarted = restart(&handle).await.unwrap();
        assert_eq!(handle.status(), ServiceStatus::Stopped);
        assert_eq!(restarted.status(), ServiceStatus::Running);
        assert_ne!(restarted.pid, old_pid);

        restarted.stop().await.unwrap();
        std::fs::remove_dir_all(canvas_path.parent().unwrap()).ok();
    }
    fn managed_block(yaml: &str) -> meshfox_core::CodeBlock {
        meshfox_core::scan_code_blocks(&format!(
            "```yaml name=\"x\" interpreter=\"@service\"\n{yaml}\n```\n"
        ))
        .remove(0)
    }

    fn managed(name: &str, yaml: &str) -> (ServiceHandle, crate::run_ledger::RunLedger, PathBuf) {
        let ledger = crate::run_ledger::RunLedger::open_in_memory().unwrap();
        let path = tmp_canvas_path(&format!("{name}-{}", uuid::Uuid::new_v4()));
        let cwd = path.parent().unwrap().to_path_buf();
        let handle = spawn_locked(
            &ledger,
            "root".into(),
            "srv".into(),
            managed_block(yaml),
            HashMap::new(),
            cwd.clone(),
            path,
            "cli",
        )
        .unwrap();
        (handle, ledger, cwd)
    }

    async fn wait_file(path: &std::path::Path) {
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while !path.exists() {
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn oneshot_without_monitor_is_held_until_automatic_stop() {
        let (handle, _, cwd) = managed("oneshot", "oneshot: true\nstart: touch started\nstop: touch stopped\nready: test -f started\nstop_timeout: 100ms");
        handle.wait_ready(false).await.unwrap();
        assert!(handle.is_live());
        handle.stop_automatic().await.unwrap();
        assert!(cwd.join("stopped").exists());
    }

    #[tokio::test]
    async fn reused_resource_is_not_started_or_stopped_by_lease_release() {
        let (handle, _, cwd) = managed("borrowed", "oneshot: true\nreuse: 'true'\nstart: touch started\nstop: touch stopped\nready: 'true'\nstop_timeout: 100ms");
        handle.acquire_lease("run", true).await.unwrap();
        handle.wait_ready(false).await.unwrap();
        handle.release_lease("run").await.unwrap();
        assert!(!handle.is_live());
        assert!(!cwd.join("started").exists());
        assert!(!cwd.join("stopped").exists());
    }

    #[tokio::test]
    async fn explicit_stop_stops_a_reused_resource() {
        let (handle, _, cwd) = managed("borrowed-stop", "oneshot: true\nreuse: 'true'\nstart: touch started\nstop: touch stopped\nstop_timeout: 100ms");
        handle.wait_ready(false).await.unwrap();
        handle.stop().await.unwrap();
        assert!(!cwd.join("started").exists());
        assert!(cwd.join("stopped").exists());
    }

    #[tokio::test]
    async fn stopped_external_resource_is_started_and_owned() {
        let (handle, _, cwd) = managed("owned-reuse", "oneshot: true\nreuse: exit 1\nstart: touch started\nstop: touch stopped\nready: test -f started\nstop_timeout: 100ms");
        handle.wait_ready(false).await.unwrap();
        handle.stop_automatic().await.unwrap();
        assert!(cwd.join("started").exists());
        assert!(cwd.join("stopped").exists());
    }

    #[tokio::test]
    async fn reuse_errors_and_timeout_never_start_or_clean_up_external_resources() {
        for probe in ["exit 2", "no_such_meshfox_probe_command", "sleep 30"] {
            let (handle, _, cwd) = managed("reuse-error", &format!("oneshot: true\nreuse: {probe}\nstart: touch started\nstop: touch stopped\nrestart_on_fail: true\nstart_timeout: 100ms\nstop_timeout: 100ms"));
            assert!(handle.wait_ready(false).await.is_err());
            handle.stop_automatic().await.unwrap();
            assert!(!cwd.join("started").exists());
            assert!(!cwd.join("stopped").exists());
        }
    }

    #[tokio::test]
    async fn reused_resource_survives_readiness_failure_and_automatic_shutdown() {
        let (handle, _, cwd) = managed("reuse-ready", "oneshot: true\nreuse: 'true'\nstart: touch started\nstop: touch stopped\nready: sleep 30\nstart_timeout: 100ms\nstop_timeout: 100ms");
        assert!(handle.wait_ready(false).await.is_err());
        handle.stop_automatic().await.unwrap();
        assert!(!cwd.join("stopped").exists());
    }

    #[tokio::test]
    async fn borrowed_health_failure_never_restarts_or_stops_external_resource() {
        let (handle, _, cwd) = managed("reuse-health", "oneshot: true\nreuse: 'true'\nstart: touch started\nstop: touch stopped\ncheck: exit 1\ncheck_interval: 50ms\nrestart_on_fail: true\nrestart_delay: 50ms\nstop_timeout: 100ms");
        handle.wait_ready(false).await.unwrap();
        tokio::time::timeout(Duration::from_secs(3), async {
            while handle.is_live() {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        assert!(matches!(handle.status(), ServiceStatus::Crashed { .. }));
        assert!(!cwd.join("started").exists());
        assert!(!cwd.join("stopped").exists());
    }

    #[tokio::test]
    async fn graceful_stop_waits_for_child_cleanup_and_keeps_ledger_claimed() {
        let (handle, ledger, cwd) = managed(
            "graceful",
            r#"start: |
  bash -c '
    trap "echo stopping > stopping; sleep 0.3; echo done > done; exit 0" TERM
    echo ready > ready
    while :; do sleep 0.05; done
  ' &
  wait
stop_timeout: 2s"#,
        );
        wait_file(&cwd.join("ready")).await;
        let stopping = handle.clone();
        let task = tokio::spawn(async move { stopping.stop().await });
        wait_file(&cwd.join("stopping")).await;
        assert_eq!(handle.status(), ServiceStatus::Stopping);
        assert_eq!(ledger.active_running().unwrap().len(), 1);
        task.await.unwrap().unwrap();
        assert!(cwd.join("done").exists());
        assert!(ledger.active_running().unwrap().is_empty());
    }

    #[tokio::test]
    async fn failed_stop_retains_ownership_and_can_be_retried() {
        let (handle, ledger, cwd) = managed("stop-failure", "start: touch started\nmonitor: sleep 30\nstop: test -f allow-stop && touch stopped\nstop_timeout: 100ms");
        wait_file(&cwd.join("started")).await;
        assert!(handle.stop().await.is_err());
        assert!(matches!(handle.status(), ServiceStatus::StopFailed { .. }));
        assert_eq!(ledger.active_running().unwrap().len(), 1);
        assert!(restart(&handle).await.is_err());
        std::fs::write(cwd.join("allow-stop"), "").unwrap();
        handle.stop().await.unwrap();
        assert!(cwd.join("stopped").exists());
        assert!(ledger.active_running().unwrap().is_empty());
    }

    #[tokio::test]
    async fn monitor_exit_reconnects_without_restarting_start() {
        let (handle, _, cwd) = managed("monitor", "start: echo start >> starts\nmonitor: echo logs >> monitors\nrestart_delay: 50ms\nstop_timeout: 100ms\nrestart_on_fail: true");
        wait_file(&cwd.join("monitors")).await;
        tokio::time::sleep(std::time::Duration::from_millis(350)).await;
        handle.stop().await.unwrap();
        assert_eq!(
            std::fs::read_to_string(cwd.join("starts"))
                .unwrap()
                .lines()
                .count(),
            1
        );
        assert!(
            std::fs::read_to_string(cwd.join("monitors"))
                .unwrap()
                .lines()
                .count()
                > 1
        );
    }

    #[tokio::test]
    async fn checks_restart_service_and_change_generation() {
        let (handle, _, cwd) = managed("check-restart", "start: echo start >> starts\nmonitor: sleep 30\ncheck: test -f healthy\ncheck_interval: 50ms\nrestart_on_fail: true\nrestart_delay: 50ms\nstop_timeout: 100ms");
        let original = handle.generation();
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let starts = std::fs::read_to_string(cwd.join("starts")).unwrap_or_default();
                if starts.lines().count() >= 2 {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        std::fs::write(cwd.join("healthy"), "").unwrap();
        assert_ne!(original, handle.generation());
        handle.stop().await.unwrap();
        let count = std::fs::read_to_string(cwd.join("starts")).unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        assert_eq!(count, std::fs::read_to_string(cwd.join("starts")).unwrap());
    }

    #[tokio::test]
    async fn stop_interrupts_start_and_still_runs_stop_hook() {
        let (handle, _, cwd) = managed("cancel-start", "start: touch starting; sleep 30\nmonitor: sleep 30\nstop: touch stopped\nstop_timeout: 100ms");
        wait_file(&cwd.join("starting")).await;
        tokio::time::timeout(std::time::Duration::from_secs(3), handle.stop())
            .await
            .unwrap()
            .unwrap();
        assert!(cwd.join("stopped").exists());
    }

    #[tokio::test]
    async fn stop_timeout_is_reported_and_retryable() {
        let (handle, ledger, cwd) = managed("stop-timeout", "start: touch started\nmonitor: sleep 30\nstop: test -f allow-stop || sleep 30\nstop_timeout: 50ms");
        wait_file(&cwd.join("started")).await;
        assert!(
            tokio::time::timeout(std::time::Duration::from_secs(3), handle.stop())
                .await
                .unwrap()
                .is_err()
        );
        assert!(matches!(handle.status(), ServiceStatus::StopFailed { .. }));
        assert_eq!(ledger.active_running().unwrap().len(), 1);
        std::fs::write(cwd.join("allow-stop"), "").unwrap();
        handle.stop().await.unwrap();
    }
    #[tokio::test]
    async fn lifecycle_and_restart_preserve_the_original_process_environment() {
        let ledger = crate::run_ledger::RunLedger::open_in_memory().unwrap();
        let path = tmp_canvas_path(&format!("frozen-env-{}", uuid::Uuid::new_v4()));
        let cwd = path.parent().unwrap().to_path_buf();
        std::fs::create_dir_all(cwd.join(".meshfox")).unwrap();
        let config = cwd.join(".meshfox/config.toml");
        std::fs::write(&config, "[process_env]\nSERVICE_TOKEN = 'original'\n").unwrap();
        let block = managed_block("start: echo \"$SERVICE_TOKEN\" >> starts\nmonitor: sleep 30\nstop: echo \"$SERVICE_TOKEN\" >> stops\nstop_timeout: 100ms");
        let handle = spawn_locked(
            &ledger,
            "root".into(),
            "srv".into(),
            block,
            HashMap::new(),
            cwd.clone(),
            path,
            "cli",
        )
        .unwrap();
        wait_file(&cwd.join("starts")).await;
        std::fs::write(&config, "[process_env]\nSERVICE_TOKEN = 'edited'\n").unwrap();
        let next = restart(&handle).await.unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            while std::fs::read_to_string(cwd.join("starts"))
                .unwrap()
                .lines()
                .count()
                < 2
            {
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        next.stop().await.unwrap();
        assert_eq!(
            std::fs::read_to_string(cwd.join("starts")).unwrap(),
            "original\noriginal\n"
        );
        assert_eq!(
            std::fs::read_to_string(cwd.join("stops")).unwrap(),
            "original\noriginal\n"
        );
    }

    #[tokio::test]
    async fn unresponsive_foreground_is_killed_after_its_grace_period() {
        let (handle, ledger, cwd) = managed("hard-fallback", "start: |\n  trap '' TERM\n  echo $$ > pid\n  while :; do sleep 0.05; done\nstop_timeout: 100ms");
        wait_file(&cwd.join("pid")).await;
        let pid: u32 = std::fs::read_to_string(cwd.join("pid"))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let began = Instant::now();
        tokio::time::timeout(std::time::Duration::from_secs(3), handle.stop())
            .await
            .unwrap()
            .unwrap();
        assert!(began.elapsed() >= std::time::Duration::from_millis(100));
        assert!(!meshfox_core::service_lock::is_alive(pid));
        assert!(ledger.active_running().unwrap().is_empty());
    }
    #[tokio::test]
    async fn stop_during_failure_cleanup_does_not_restart_or_run_cleanup_twice() {
        let (handle, ledger, cwd) = managed("stop-during-cleanup", "start: exit 1\nmonitor: sleep 30\nstop: echo cleanup >> cleanups; touch cleaning; sleep 0.3\nrestart_on_fail: true\nrestart_delay: 50ms\nstop_timeout: 2s");
        wait_file(&cwd.join("cleaning")).await;
        tokio::time::timeout(std::time::Duration::from_secs(3), handle.stop())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(handle.status(), ServiceStatus::Stopped);
        assert_eq!(
            std::fs::read_to_string(cwd.join("cleanups"))
                .unwrap()
                .lines()
                .count(),
            1
        );
        assert!(ledger.active_running().unwrap().is_empty());
    }
}
