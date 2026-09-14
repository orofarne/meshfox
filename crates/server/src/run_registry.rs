//! Address-scoped registry for a *plain* (non-`service`, non-`tty`) block's
//! most recent run — lets its output survive a client reconnecting or a
//! second tab watching the same block, the same way `crate::services`
//! already lets a `service` block's status/log survive across requests.
//! Unlike `services`, an entry here is transient: it exists only for the
//! run that's currently in flight, or the one that most recently finished
//! — a fresh run of the same address simply replaces it (see
//! `crate::AppState::runs_registry`'s own doc comment).
//!
//! The originating `/api/run` request and any later `/api/run/subscribe`
//! caller are both just subscribers to the same broadcast — starting a run
//! and watching it are two different things now, which is what lets a
//! second tab (or a reload of the first one) pick the live output back up
//! from wherever it left off instead of the old model's "the process dies
//! the moment the one connection that started it goes away".

use crate::stream_exec::{OutputStream, SpawnedProcess};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::{broadcast, oneshot};

/// How many recent output lines a run's log keeps — same figure
/// `services::LOG_CAPACITY` uses, for the same reasoning (generous enough
/// for real use, small enough to never be a real memory concern).
const LOG_CAPACITY: usize = 2000;

/// How a run ended — `Running` until it hasn't. Kept separate from
/// `services::ServiceStatus` (no `Crashed`/`Stopped` distinction here: a
/// plain block's own nonzero exit is just `Exited`, not a "crash" in the
/// service sense, and there's no "restart" concept for it either).
#[derive(Debug, Clone, PartialEq)]
pub enum RunOutcome {
    Running,
    Exited { exit_code: i32 },
    Killed,
}

/// One buffered/live output line, tagged with its own place in the
/// sequence — what a subscriber's `since` cursor advances past on each
/// reconnect, so it never re-fetches (or, worse, never sees) a line twice.
#[derive(Debug, Clone)]
pub struct SeqLine {
    pub seq: u64,
    pub stream: OutputStream,
    pub text: String,
}

struct RingBuffer {
    cap: usize,
    next_seq: u64,
    lines: VecDeque<SeqLine>,
}

impl RingBuffer {
    fn new(cap: usize) -> Self {
        RingBuffer { cap, next_seq: 0, lines: VecDeque::new() }
    }

    fn push(&mut self, stream: OutputStream, text: String) -> SeqLine {
        let seq = self.next_seq;
        self.next_seq += 1;
        let line = SeqLine { seq, stream, text };
        if self.lines.len() >= self.cap {
            self.lines.pop_front();
        }
        self.lines.push_back(line.clone());
        line
    }

    /// Every buffered line at or after `seq` — `since(0)` is every line
    /// still in the buffer, the closest equivalent to `services::
    /// RingBuffer::snapshot`'s own full-snapshot behavior.
    fn since(&self, seq: u64) -> Vec<SeqLine> {
        self.lines.iter().filter(|l| l.seq >= seq).cloned().collect()
    }
}

/// One event a subscriber (the request that started this run, or a later
/// `/api/run/subscribe` caller) receives — either a buffered-or-live
/// output line, or this run's own terminal outcome. `Done` is sent exactly
/// once, always last.
#[derive(Debug, Clone)]
pub enum RunEvent {
    Line(SeqLine),
    Done(RunOutcome),
}

pub struct RunHandle {
    // Identity that travels with the handle itself, same reasoning
    // `services::ServiceHandle` carries its own `node_id`/`block_name` for
    // — read by `GET /api/runs` (`crate::lib`'s `get_active_runs`) so it
    // doesn't have to separately thread the registry's own map key through.
    pub node_id: String,
    pub block_name: String,
    log: Arc<Mutex<RingBuffer>>,
    outcome: Arc<Mutex<RunOutcome>>,
    tx: broadcast::Sender<RunEvent>,
    // `Some` only until this run is killed or the drain task itself
    // consumes it on a normal exit — `Mutex<Option<..>>` rather than a
    // bare `oneshot::Sender` so `kill` can be called more than once
    // (a second caller racing the first, or calling it after the run
    // already finished on its own) without panicking on an already-
    // consumed sender.
    kill_tx: Mutex<Option<oneshot::Sender<()>>>,
    /// When this run started — same role `services::ServiceHandle::
    /// started_at` plays, for `GET /api/runs`'s own `uptimeMs` (lets a
    /// client that reconnects mid-run show a real elapsed time instead of
    /// restarting its own clock from the moment it happened to notice).
    /// For a handle that started life via `reserve` (see its own doc
    /// comment), this is when the *address* was reserved, not necessarily
    /// when a real process actually started under it.
    started_at: std::time::Instant,
    /// Set once (by `attach`) or resolved-without-ever-attaching (by
    /// `resolve_if_unreached`) — whichever happens first wins, and the
    /// other becomes a no-op. Only meaningful for a handle that started
    /// life via `reserve`; `track` (which attaches immediately) sets it
    /// right away, so it's always `true` for a handle nothing else ever
    /// needs to check it on.
    attached: AtomicBool,
}

impl RunHandle {
    pub fn outcome(&self) -> RunOutcome {
        self.outcome.lock().unwrap().clone()
    }

    pub fn uptime_ms(&self) -> u64 {
        self.started_at.elapsed().as_millis() as u64
    }

    /// Every buffered line at or after `since`, plus a receiver for
    /// whatever comes next (more lines, then exactly one `Done`) —
    /// subscribing *before* reading the buffered backlog (both happen
    /// under the same lock here) is what guarantees no line already in
    /// flight when this is called is ever missed or double-delivered.
    pub fn subscribe_from(&self, since: u64) -> (Vec<SeqLine>, broadcast::Receiver<RunEvent>) {
        let log = self.log.lock().unwrap();
        let rx = self.tx.subscribe();
        (log.since(since), rx)
    }

    /// Signals this run's own drain task to kill the process — a no-op
    /// (returns `false`) if it already finished on its own or was already
    /// killed by an earlier call. Every subscriber (this run's own
    /// originating connection *and* any later `/api/run/subscribe`
    /// watcher) sees the same `RunEvent::Done(Killed)` once the drain task
    /// actually notices, regardless of who called this.
    pub fn kill(&self) -> bool {
        self.kill_tx.lock().unwrap().take().map(|tx| tx.send(())).is_some()
    }

    /// Resolves this handle as `outcome` — but only if `attach` was never
    /// actually called on it (a no-op once a real process has taken over;
    /// that process's own eventual completion is authoritative, not
    /// whatever the caller passes here). Exists for a handle created via
    /// `reserve` and never followed up with `attach` at all — a chain that
    /// reserves its own requested block's identity up front (see
    /// `crate::run_block_impl`'s own use of this) but never actually
    /// reaches that step, an earlier dependency failing and breaking the
    /// chain, say. Without this, a subscriber that raced ahead and is
    /// already waiting on the reservation would see nothing further, ever
    /// — no line, no `Done` — since nothing else would ever resolve it.
    pub fn resolve_if_unreached(&self, outcome: RunOutcome) {
        if self.attached.swap(true, Ordering::SeqCst) {
            return;
        }
        *self.outcome.lock().unwrap() = outcome.clone();
        let _ = self.tx.send(RunEvent::Done(outcome));
    }
}

/// Reserves an address's *next* run identity synchronously, before any
/// process has actually spawned for it — an empty, still-`Running` handle
/// a caller can insert into a registry keyed by address right away. Exists
/// to close a race `crate::run_block_impl` documents at its own call site:
/// between "a request that will start a new run for this address has been
/// accepted" and "a process has actually spawned for it" there can be real
/// wall-clock time (an earlier step in the same chain still running, say)
/// — a subscriber that looks the address up in that window must find
/// *this*, not whatever the previous, already-finished run left behind.
/// `attach` below wires a real process into this exact handle once one
/// exists; `resolve_if_unreached` resolves it if that never happens.
pub fn reserve(node_id: String, block_name: String) -> Arc<RunHandle> {
    let (tx, _) = broadcast::channel(1024);
    Arc::new(RunHandle {
        node_id,
        block_name,
        log: Arc::new(Mutex::new(RingBuffer::new(LOG_CAPACITY))),
        outcome: Arc::new(Mutex::new(RunOutcome::Running)),
        tx,
        kill_tx: Mutex::new(None),
        started_at: std::time::Instant::now(),
        attached: AtomicBool::new(false),
    })
}

/// Wires an already-spawned process into `handle` (from `reserve`) — same
/// division of labor `services::spawn` already has (this doesn't spawn
/// `proc` itself, just drains it). A no-op (drops `proc` without draining
/// it) if `handle` was already attached or resolved — shouldn't happen in
/// practice (each handle's own chain step only ever calls this once), but
/// safe rather than double-spawning a drain task over the same handle.
///
/// `lock_path`, if given, is released *by this background task itself*
/// once the process actually exits or is killed — see `track`'s own doc
/// comment (still accurate here) for why that has to be tied to the
/// process's own completion rather than whatever caller happened to start
/// it.
pub fn attach(handle: &Arc<RunHandle>, mut proc: SpawnedProcess, lock_path: Option<std::path::PathBuf>) {
    if handle.attached.swap(true, Ordering::SeqCst) {
        return;
    }
    let (kill_tx, mut kill_rx) = oneshot::channel();
    *handle.kill_tx.lock().unwrap() = Some(kill_tx);

    let task_handle = Arc::clone(handle);
    tokio::spawn(async move {
        let outcome = loop {
            tokio::select! {
                line = proc.output_rx.recv() => match line {
                    Some((stream, text)) => {
                        let seq_line = task_handle.log.lock().unwrap().push(stream, text);
                        // No subscribers is a normal, common case (nobody's
                        // watching this exact run right now) — not an
                        // error, nothing to do about it either way, since
                        // the line is already durably in the ring buffer
                        // for whoever subscribes next.
                        let _ = task_handle.tx.send(RunEvent::Line(seq_line));
                    }
                    None => {
                        let status = proc.child.wait().await;
                        break RunOutcome::Exited {
                            exit_code: status.ok().and_then(|s| s.code()).unwrap_or(-1),
                        };
                    }
                },
                _ = &mut kill_rx => {
                    let _ = proc.kill();
                    let _ = proc.child.wait().await;
                    break RunOutcome::Killed;
                }
            }
        };
        *task_handle.outcome.lock().unwrap() = outcome.clone();
        let _ = task_handle.tx.send(RunEvent::Done(outcome));
        if let Some(path) = lock_path {
            let _ = meshfox_core::service_lock::release(&path);
        }
    });
}

/// Reserves and immediately attaches `proc` in one call — every caller
/// that doesn't need the gap between the two closed (see `reserve`'s own
/// doc comment for the one that does) just wants this.
pub fn track(
    node_id: String,
    block_name: String,
    proc: SpawnedProcess,
    lock_path: Option<std::path::PathBuf>,
) -> Arc<RunHandle> {
    let handle = reserve(node_id, block_name);
    attach(&handle, proc, lock_path);
    handle
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stream_exec;

    fn no_envs() -> [(&'static str, &'static str); 0] {
        []
    }

    #[tokio::test]
    async fn a_late_subscriber_gets_the_backlog_then_the_live_tail() {
        let proc = stream_exec::spawn_bash("echo one; sleep 0.3; echo two", no_envs(), None).unwrap();
        let handle = track("root".to_string(), "slow".to_string(), proc, None);

        // Poll briefly for the drain task to actually push "one" before
        // this "late" subscriber ever asks — proves it's replayed, not
        // missed. Polling (not a fixed sleep) so this isn't sensitive to
        // how loaded the machine running the test happens to be; re-
        // subscribing on each attempt is fine since nothing's been read
        // off any of the earlier, discarded receivers.
        let (backlog, mut rx) = {
            let mut result = None;
            for _ in 0..50 {
                let (b, rx) = handle.subscribe_from(0);
                if !b.is_empty() {
                    result = Some((b, rx));
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
            result.expect("\"one\" should have been buffered by now")
        };
        assert_eq!(backlog.len(), 1);
        assert_eq!(backlog[0].text, "one");

        let mut lines = vec![backlog[0].text.clone()];
        loop {
            match rx.recv().await.unwrap() {
                RunEvent::Line(l) => lines.push(l.text),
                RunEvent::Done(outcome) => {
                    assert_eq!(outcome, RunOutcome::Exited { exit_code: 0 });
                    break;
                }
            }
        }
        assert_eq!(lines, vec!["one".to_string(), "two".to_string()]);
    }

    #[tokio::test]
    async fn subscribing_from_a_later_seq_skips_earlier_backlog() {
        let proc = stream_exec::spawn_bash("echo one; echo two; echo three", no_envs(), None).unwrap();
        let handle = track("root".to_string(), "x".to_string(), proc, None);

        // Poll until the whole run has actually finished (three lines
        // buffered) so `since(2)` below is deterministic.
        loop {
            if matches!(handle.outcome(), RunOutcome::Exited { .. }) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let (backlog, _rx) = handle.subscribe_from(2);
        assert_eq!(backlog.iter().map(|l| l.text.as_str()).collect::<Vec<_>>(), vec!["three"]);
    }

    #[tokio::test]
    async fn kill_stops_the_process_and_reports_killed_to_every_subscriber() {
        let proc = stream_exec::spawn_bash("sleep 30", no_envs(), None).unwrap();
        let handle = track("root".to_string(), "slow".to_string(), proc, None);
        let (_, mut rx) = handle.subscribe_from(0);

        assert!(handle.kill());
        let event = rx.recv().await.unwrap();
        assert!(matches!(event, RunEvent::Done(RunOutcome::Killed)));
        assert_eq!(handle.outcome(), RunOutcome::Killed);

        // A second kill call, after the fact, is a harmless no-op — not a
        // panic on an already-consumed sender.
        assert!(!handle.kill());
    }
}
