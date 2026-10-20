//! Async lifecycle owner. No registry mutex is held while running user commands.
use super::{descendant_pids, tree_refresh, RingBuffer, ServiceStatus};
use crate::{
    run_ledger::{FinishOutcome, RunLedger},
    stream_exec::{self, OutputStream, SpawnedProcess},
};
use meshfox_core::service::ServiceConfig;
use std::{
    collections::HashMap,
    io,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicI64, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};
use tokio::sync::watch;

type Log = Arc<Mutex<RingBuffer>>;
const TICK: Duration = Duration::from_millis(50);
/// How often a running process's descendants are looked for: often while it
/// is young — when a service forks, daemonizes and has its parent exit, which
/// reparents the child out of reach — and ever less once it has been running
/// a while. Each look walks the machine's whole process table, so a service
/// that runs for days must not pay the young rate for all of them; at stop
/// its process group is signalled whatever the scans found.
const SCAN_YOUNG: Duration = Duration::from_millis(250);
const SCAN_SETTLING: Duration = Duration::from_secs(1);
const SCAN_SETTLED: Duration = Duration::from_secs(5);
const SCAN_YOUNG_FOR: Duration = Duration::from_secs(10);
const SCAN_SETTLING_FOR: Duration = Duration::from_secs(60);
const DEFAULT_STOP: Duration = Duration::from_secs(30);
static NEXT_EPOCH: AtomicI64 = AtomicI64::new(-1);

struct Process {
    proc: SpawnedProcess,
    pid: u32,
    descendants: HashMap<sysinfo::Pid, u64>,
    stopping: bool,
    cleaned: bool,
    track_descendants: bool,
    started: Instant,
    scanned: Instant,
    exit: Option<i32>,
}
impl Process {
    fn new(proc: SpawnedProcess) -> Self {
        Self {
            pid: proc.child.id().unwrap_or(0),
            proc,
            descendants: HashMap::new(),
            stopping: false,
            cleaned: false,
            track_descendants: true,
            started: Instant::now(),
            scanned: Instant::now() - Duration::from_secs(1),
            exit: None,
        }
    }
    fn pump(&mut self, log: &Log) -> io::Result<Option<i32>> {
        while let Ok((stream, text)) = self.proc.output_rx.try_recv() {
            log.lock().unwrap().push(stream, text);
        }
        if self.exit.is_some() {
            return Ok(self.exit);
        }
        if self.scanned.elapsed() >= self.scan_interval() {
            self.scan();
            self.scanned = Instant::now();
        }
        if let Some(status) = self.proc.child.try_wait()? {
            self.exit = Some(status.code().unwrap_or(-1));
            if !self.stopping {
                self.kill_remaining();
                self.cleaned = true;
            }
        }
        Ok(self.exit)
    }
    fn scan_interval(&self) -> Duration {
        let age = self.started.elapsed();
        if age < SCAN_YOUNG_FOR {
            SCAN_YOUNG
        } else if age < SCAN_SETTLING_FOR {
            SCAN_SETTLING
        } else {
            SCAN_SETTLED
        }
    }
    fn scan(&mut self) {
        // A reaped leader PID may have been reused since startup.
        if self.exit.is_some() {
            return;
        }
        let descendants = descendant_pids(self.pid);
        if descendants.is_empty() {
            return;
        }
        let mut sys = sysinfo::System::new();
        sys.refresh_processes_specifics(
            sysinfo::ProcessesToUpdate::Some(&descendants),
            true,
            tree_refresh(),
        );
        for pid in descendants {
            if let Some(process) = sys.process(pid) {
                self.descendants.entry(pid).or_insert(process.start_time());
            }
        }
    }
    fn live_descendants(&self) -> Vec<sysinfo::Pid> {
        let pids: Vec<_> = self.descendants.keys().copied().collect();
        if pids.is_empty() {
            return pids;
        }
        let mut sys = sysinfo::System::new();
        sys.refresh_processes_specifics(
            sysinfo::ProcessesToUpdate::Some(&pids),
            true,
            tree_refresh(),
        );
        pids.into_iter()
            .filter(|pid| {
                sys.process(*pid).is_some_and(|p| {
                    p.start_time() == self.descendants[pid]
                        && p.status() != sysinfo::ProcessStatus::Zombie
                })
            })
            .collect()
    }
    fn signal(&self, signal: i32) {
        let descendants = self.live_descendants();
        if self.pid != 0 && (self.exit.is_none() || !descendants.is_empty()) {
            unsafe {
                libc::kill(-(self.pid as libc::pid_t), signal);
            }
        }
        for pid in descendants {
            // Group members already received the signal. Delivering TERM
            // twice can interrupt a trap while it is doing cleanup.
            if unsafe { libc::getpgid(pid.as_u32() as libc::pid_t) } != self.pid as libc::pid_t {
                unsafe {
                    libc::kill(pid.as_u32() as libc::pid_t, signal);
                }
            }
        }
    }
    fn kill_remaining(&mut self) {
        self.scan();
        self.signal(libc::SIGKILL);
    }
    async fn terminate(&mut self, timeout: Duration, log: &Log) {
        self.stopping = true;
        self.scan();
        self.signal(libc::SIGTERM);
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            let exited = self.pump(log).ok().flatten().is_some();
            let group_alive = unsafe { libc::kill(-(self.pid as libc::pid_t), 0) } == 0;
            if exited && !group_alive && self.live_descendants().is_empty() {
                break;
            }
            tokio::time::sleep(TICK).await;
        }
        self.kill_remaining();
        let _ = self.proc.child.wait().await;
        // A daemon may retain a pipe after escaping a scan. Never let log
        // draining turn a bounded stop into an unbounded wait.
        let _ = tokio::time::timeout(Duration::from_millis(250), async {
            while let Some((stream, text)) = self.proc.output_rx.recv().await {
                log.lock().unwrap().push(stream, text);
            }
        })
        .await;
        self.cleaned = true;
    }
}
impl Drop for Process {
    fn drop(&mut self) {
        if !self.cleaned {
            self.kill_remaining();
        }
    }
}

#[derive(Debug)]
enum End {
    Exit(i32),
    Stop,
    Timeout,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Ownership {
    Pending,
    Owned,
    Borrowed,
}

struct Supervisor {
    config: Option<ServiceConfig>,
    env: Arc<Vec<(std::ffi::OsString, std::ffi::OsString)>>,
    cwd: PathBuf,
    status: Arc<Mutex<ServiceStatus>>,
    log: Log,
    stop: watch::Receiver<u64>,
    completed: watch::Sender<(u64, Result<(), String>)>,
    handled: u64,
    processes: Vec<Process>,
    ownership: Ownership,
    force_stop: Arc<AtomicBool>,
}
impl Supervisor {
    fn message(&self, text: impl Into<String>) {
        self.log.lock().unwrap().push(
            OutputStream::Stderr,
            format!("[meshfox service] {}", text.into()),
        );
    }
    fn set_status(&self, status: ServiceStatus) {
        *self.status.lock().unwrap() = status;
    }
    fn requested(&self) -> bool {
        *self.stop.borrow() > self.handled
    }
    fn command(&self, code: &str) -> io::Result<Process> {
        stream_exec::spawn_bash_frozen(code, &self.env, &self.cwd).map(Process::new)
    }
    // Bounded checks/stop commands keep draining the service's own log too.
    async fn command_until(
        &mut self,
        code: &str,
        timeout: Option<Duration>,
        interruptible: bool,
        retain: bool,
    ) -> io::Result<End> {
        let mut command = self.command(code)?;
        command.stopping = retain;
        let deadline = timeout.map(|t| Instant::now() + t);
        loop {
            for process in &mut self.processes {
                let _ = process.pump(&self.log);
            }
            if interruptible && self.requested() {
                command.terminate(Duration::from_secs(1), &self.log).await;
                return Ok(End::Stop);
            }
            if let Some(code) = command.pump(&self.log)? {
                if retain {
                    self.processes.push(command);
                } else {
                    command.terminate(Duration::ZERO, &self.log).await;
                }
                return Ok(End::Exit(code));
            }
            if deadline.is_some_and(|d| Instant::now() >= d) {
                command.terminate(Duration::ZERO, &self.log).await;
                return Ok(End::Timeout);
            }
            tokio::time::sleep(TICK).await;
        }
    }
    async fn cleanup(&mut self) -> Result<(), String> {
        let timeout = self
            .config
            .as_ref()
            .map_or(DEFAULT_STOP, |c| c.stop_timeout);
        let hook = if self.ownership == Ownership::Owned || self.force_stop.load(Ordering::SeqCst) {
            self.config.as_ref().and_then(|c| c.stop.clone())
        } else {
            None
        };
        let has_hook = hook.is_some();
        let result = if let Some(hook) = hook {
            match self.command_until(&hook, Some(timeout), false, false).await {
                Ok(End::Exit(0)) => Ok(()),
                Ok(end) => Err(format!("stop command failed: {end:?}")),
                Err(e) => Err(format!("stop command failed: {e}")),
            }
        } else {
            Ok(())
        };
        // Do not spend another full grace period per process after a hook:
        // it has already had the configured time to stop the external service.
        let grace = if has_hook {
            Duration::from_secs(1)
        } else {
            timeout
        };
        let deadline = Instant::now() + grace;
        for process in &mut self.processes {
            process
                .terminate(
                    deadline.saturating_duration_since(Instant::now()),
                    &self.log,
                )
                .await;
        }
        self.processes.clear();
        result
    }
    async fn stop_attempt(&mut self) -> bool {
        let request = *self.stop.borrow();
        self.set_status(ServiceStatus::Stopping);
        let result = self.cleanup().await;
        self.handled = request;
        match &result {
            Ok(()) => {}
            Err(message) => {
                self.message(message.clone());
                self.set_status(ServiceStatus::StopFailed {
                    message: message.clone(),
                });
            }
        }
        if result.is_err() {
            let _ = self.completed.send_replace((request, result.clone()));
        }
        result.is_ok()
    }
    fn finish_stop(&self, ledger: &RunLedger, row: i64) {
        let _ = ledger.finish(row, FinishOutcome::Killed);
        self.set_status(ServiceStatus::Stopped);
        let _ = self.completed.send_replace((*self.stop.borrow(), Ok(())));
    }
    fn finish_exit(&self, ledger: &RunLedger, row: i64, code: i32) {
        let _ = ledger.finish(row, FinishOutcome::Exited(code));
        let mut status = self.status.lock().unwrap();
        if self.requested() || matches!(*status, ServiceStatus::Stopping) {
            *status = ServiceStatus::Stopped;
            let _ = self.completed.send_replace((*self.stop.borrow(), Ok(())));
        } else {
            *status = ServiceStatus::Crashed { exit_code: code };
        }
    }
    async fn wait_retry_stop(&mut self, identity: &mut Process) {
        while !self.requested() {
            if self.stop.has_changed().is_err() || identity.pump(&self.log).ok().flatten().is_some()
            {
                // The owner was dropped or force-taken-over. Avoid a late
                // hook touching a new owner's external resources.
                self.config.as_mut().unwrap().stop = None;
                return;
            }
            tokio::time::sleep(TICK).await;
        }
    }

    async fn attempt(&mut self, anchor: &mut Process) -> io::Result<End> {
        let config = self.config.clone().expect("declarative service");
        if self.requested() {
            return Ok(End::Stop);
        }
        self.set_status(ServiceStatus::Starting);
        let deadline = Instant::now() + config.start_timeout;
        if self.ownership == Ownership::Pending {
            if let Some(reuse) = &config.reuse {
                match self
                    .command_until(reuse, Some(config.start_timeout), true, false)
                    .await?
                {
                    End::Exit(0) => {
                        self.ownership = Ownership::Borrowed;
                        self.message(
                            "reusing external resource; automatic cleanup will only detach",
                        );
                    }
                    End::Exit(1) => {}
                    End::Stop => return Ok(End::Stop),
                    end => {
                        self.message(format!("reuse probe failed: {end:?}"));
                        return Ok(match end {
                            End::Exit(_) => End::Exit(2),
                            other => other,
                        });
                    }
                }
            }
        }
        let oneshot = config.oneshot || config.monitor.is_some();
        let mut watched = None;
        if self.ownership != Ownership::Borrowed {
            if Instant::now() >= deadline {
                return Ok(End::Timeout);
            }
            self.ownership = Ownership::Owned;
            if oneshot {
                match self
                    .command_until(
                        &config.start,
                        Some(deadline.saturating_duration_since(Instant::now())),
                        true,
                        true,
                    )
                    .await?
                {
                    End::Exit(0) => {}
                    end => return Ok(end),
                }
            } else {
                watched = Some(self.processes.len());
                self.processes.push(self.command(&config.start)?);
            }
        }
        if let Some(monitor) = &config.monitor {
            watched = Some(self.processes.len());
            self.processes.push(self.command(monitor)?);
        }
        if let Some(ready) = &config.ready {
            loop {
                if self.requested() {
                    return Ok(End::Stop);
                }
                if anchor.pump(&self.log)?.is_some() {
                    return Ok(End::Stop);
                }
                if !oneshot {
                    if let Some(process) = watched.and_then(|index| self.processes.get_mut(index)) {
                        if let Some(code) = process.pump(&self.log)? {
                            return Ok(End::Exit(code));
                        }
                    }
                }
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    self.message("service readiness timed out");
                    return Ok(End::Timeout);
                }
                match self
                    .command_until(ready, Some(remaining), true, false)
                    .await?
                {
                    End::Exit(0) => break,
                    End::Stop => return Ok(End::Stop),
                    End::Timeout => {
                        self.message("service readiness timed out");
                        return Ok(End::Timeout);
                    }
                    _ => tokio::time::sleep(TICK).await,
                }
            }
        }
        self.set_status(ServiceStatus::Running);
        let mut next_check = Instant::now() + config.check_interval;
        let mut next_monitor = Instant::now();
        loop {
            if self.requested() {
                return Ok(End::Stop);
            }
            if anchor.pump(&self.log)?.is_some() {
                return Ok(End::Stop);
            }
            if let Some(process) = watched.and_then(|index| self.processes.get_mut(index)) {
                if let Some(code) = process.pump(&self.log)? {
                    if config.monitor.is_none() {
                        return Ok(End::Exit(code));
                    }
                    self.message(format!(
                        "monitor exited ({code}); reconnecting without restarting service"
                    ));
                    self.processes.pop();
                    watched = None;
                    next_monitor = Instant::now() + config.restart_delay;
                }
            } else if config.monitor.is_some() && Instant::now() >= next_monitor {
                watched = Some(self.processes.len());
                self.processes
                    .push(self.command(config.monitor.as_ref().unwrap())?);
            }
            if Instant::now() >= next_check {
                if let Some(check) = &config.check {
                    match self
                        .command_until(check, Some(config.check_interval), true, false)
                        .await?
                    {
                        End::Exit(0) => {}
                        End::Stop => return Ok(End::Stop),
                        end => {
                            self.message(format!("check failed: {end:?}"));
                            return Ok(End::Exit(1));
                        }
                    }
                }
                next_check = Instant::now() + config.check_interval;
            }
            tokio::time::sleep(TICK).await;
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn spawn(
    proc: SpawnedProcess,
    config: Option<ServiceConfig>,
    env: Arc<Vec<(std::ffi::OsString, std::ffi::OsString)>>,
    cwd: PathBuf,
    status: Arc<Mutex<ServiceStatus>>,
    log: Log,
    ledger: RunLedger,
    row: i64,
    stop: watch::Receiver<u64>,
    completed: watch::Sender<(u64, Result<(), String>)>,
    epoch: Arc<AtomicI64>,
    force_stop: Arc<AtomicBool>,
) {
    tokio::spawn(async move {
        let mut supervisor = Supervisor {
            config,
            env,
            cwd,
            status,
            log,
            stop,
            completed,
            handled: 0,
            processes: Vec::new(),
            ownership: Ownership::Pending,
            force_stop,
        };
        let mut identity = Process::new(proc);
        identity.track_descendants = supervisor.config.is_none();
        if supervisor.config.is_none() {
            supervisor.processes.push(identity);
            loop {
                if supervisor.requested() {
                    supervisor.stop_attempt().await;
                    supervisor.finish_stop(&ledger, row);
                    return;
                }
                match supervisor.processes[0].pump(&supervisor.log) {
                    Ok(Some(code)) => {
                        supervisor.processes[0]
                            .terminate(Duration::ZERO, &supervisor.log)
                            .await;
                        supervisor.finish_exit(&ledger, row, code);
                        return;
                    }
                    Err(e) => supervisor.message(e.to_string()),
                    _ => {}
                }
                tokio::time::sleep(TICK).await;
            }
        }
        let mut failures = 0u32;
        loop {
            let began = Instant::now();
            let end = match supervisor.attempt(&mut identity).await {
                Ok(end) => end,
                Err(e) => {
                    supervisor.message(e.to_string());
                    End::Exit(1)
                }
            };
            if identity.exit.is_some() {
                // Force-takeover killed this identity. Never run a delayed
                // external stop against the new owner's resources.
                supervisor.config.as_mut().unwrap().stop = None;
            }
            if matches!(end, End::Stop) || supervisor.requested() {
                loop {
                    if supervisor.stop_attempt().await {
                        break;
                    }
                    supervisor.wait_retry_stop(&mut identity).await;
                }
                identity.terminate(Duration::ZERO, &supervisor.log).await;
                supervisor.finish_stop(&ledger, row);
                return;
            }
            supervisor.set_status(ServiceStatus::Restarting);
            epoch.store(
                NEXT_EPOCH.fetch_sub(1, Ordering::Relaxed),
                Ordering::Relaxed,
            );
            if let Err(message) = supervisor.cleanup().await {
                supervisor.message(message.clone());
                supervisor.set_status(ServiceStatus::StopFailed { message });
                loop {
                    supervisor.wait_retry_stop(&mut identity).await;
                    if supervisor.stop_attempt().await {
                        break;
                    }
                }
                identity.terminate(Duration::ZERO, &supervisor.log).await;
                supervisor.finish_stop(&ledger, row);
                return;
            }
            if supervisor.requested() {
                identity.terminate(Duration::ZERO, &supervisor.log).await;
                supervisor.finish_stop(&ledger, row);
                return;
            }
            let config = supervisor.config.as_ref().unwrap();
            let code = match end {
                End::Exit(code) => code,
                _ => 1,
            };
            if supervisor.ownership != Ownership::Owned
                || !config.restart_on_fail
                || code == 0
                || matches!(end, End::Timeout)
            {
                identity.terminate(Duration::ZERO, &supervisor.log).await;
                supervisor.finish_exit(&ledger, row, code);
                return;
            }
            if began.elapsed() >= Duration::from_secs(60) {
                failures = 0;
            }
            let delay = config
                .restart_delay
                .mul_f64(2f64.powi(failures.min(8) as i32))
                .min(config.restart_delay.max(Duration::from_secs(60)));
            failures += 1;
            supervisor.set_status(ServiceStatus::Restarting);
            supervisor.message(format!("restarting after failure ({code}) in {delay:?}"));
            let deadline = Instant::now() + delay;
            while Instant::now() < deadline && !supervisor.requested() {
                if identity.pump(&supervisor.log).ok().flatten().is_some() {
                    break;
                }
                tokio::time::sleep(TICK).await;
            }
            if identity.exit.is_some() {
                supervisor.config.as_mut().unwrap().stop = None;
            }
            if supervisor.requested() || identity.exit.is_some() {
                // The next attempt observes the stop before spawning anything.
                while !supervisor.stop_attempt().await {
                    supervisor.wait_retry_stop(&mut identity).await;
                }
                identity.terminate(Duration::ZERO, &supervisor.log).await;
                supervisor.finish_stop(&ledger, row);
                return;
            }
        }
    });
}

/// Probe an already running service without taking over its lifecycle.
pub(super) async fn ready_probe(
    code: &str,
    timeout: Duration,
    env: &[(std::ffi::OsString, std::ffi::OsString)],
    cwd: &std::path::Path,
    status: &Arc<Mutex<ServiceStatus>>,
    log: &Log,
) -> io::Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        if !matches!(*status.lock().unwrap(), ServiceStatus::Running) {
            return Err(io::Error::other(
                "service stopped while waiting for readiness",
            ));
        }
        let mut probe = Process::new(stream_exec::spawn_bash_frozen(code, env, cwd)?);
        let result = loop {
            if let Some(code) = probe.pump(log)? {
                break Some(code);
            }
            if Instant::now() >= deadline
                || !matches!(*status.lock().unwrap(), ServiceStatus::Running)
            {
                break None;
            }
            tokio::time::sleep(TICK).await;
        };
        probe.terminate(Duration::ZERO, log).await;
        if result == Some(0) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "service readiness timed out",
            ));
        }
        tokio::time::sleep(TICK).await;
    }
}
