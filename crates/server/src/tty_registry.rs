//! Multi-viewer registry for a `tty` block's own live pty session —
//! generalizes `run_registry`'s "detach execution from whichever
//! connection started it" model to an interactive session: any number of
//! WebSocket connections (the one that started it, plus any later
//! `/api/run/tty/attach` caller) can watch the same session's output,
//! type into it, and contribute to its terminal size, all at once.
//! Closing one viewer's tab no longer ends the session — only an explicit
//! kill or the process exiting on its own does.
//!
//! Byte-buffered history only (no vt100/screen-state emulation) — a late
//! attacher replays the whole still-buffered byte history and lets
//! xterm.js's own parser reconstruct the screen from it. A buffer that's
//! been truncated (the cap was hit) can occasionally hand a viewer a
//! stream that starts mid-escape-sequence, showing briefly garbled state
//! until the next real redraw — an accepted, deliberately simple starting
//! point (see this feature's own design discussion).

use crate::pty_exec::PtyProcess;
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::{broadcast, mpsc, oneshot};

/// How many recent bytes a session's history keeps for a late/reconnecting
/// viewer to replay — generous for real terminal use (a screenful of a
/// busy build log, say), small enough to never be a real memory concern.
const BYTE_CAPACITY: usize = 262_144; // 256 KiB

/// How long a killed session gets to end on its own before `SIGKILL`: the
/// time for a program to run its cleanup (Claude Code, for one, records on
/// exit that its fullscreen start was healthy and counts a hard kill within
/// the first seconds as a failed start).
pub const STOP_GRACE: std::time::Duration = std::time::Duration::from_secs(5);

#[derive(Debug, Clone, PartialEq)]
pub enum RunOutcome {
    Running,
    Exited { exit_code: i32 },
    Killed,
}

/// One event a viewer (this session's originating connection, or a later
/// `/api/run/tty/attach` caller) receives — either a chunk of raw pty
/// output, or this session's own terminal outcome, sent exactly once,
/// always last.
#[derive(Debug, Clone)]
pub enum TtyEvent {
    Bytes(Arc<[u8]>),
    Done(RunOutcome),
}

struct ByteRing {
    cap: usize,
    buf: VecDeque<u8>,
}

impl ByteRing {
    fn new(cap: usize) -> Self {
        ByteRing {
            cap,
            buf: VecDeque::new(),
        }
    }

    fn push(&mut self, bytes: &[u8]) {
        self.buf.extend(bytes.iter().copied());
        let overflow = self.buf.len().saturating_sub(self.cap);
        if overflow > 0 {
            self.buf.drain(..overflow);
        }
    }

    fn snapshot(&self) -> Vec<u8> {
        self.buf.iter().copied().collect()
    }
}

pub struct TtySessionHandle {
    // Identity that travels with the handle itself — read by `GET
    // /api/runs` (see `run_registry::RunHandle`'s identical pair of
    // fields, and its own doc comment).
    pub node_id: String,
    pub block_name: String,
    log: Mutex<ByteRing>,
    outcome: Mutex<RunOutcome>,
    tx: broadcast::Sender<TtyEvent>,
    input_tx: mpsc::UnboundedSender<Vec<u8>>,
    resize_tx: mpsc::UnboundedSender<(u16, u16)>,
    kill_tx: Mutex<Option<oneshot::Sender<()>>>,
    /// Every currently-attached viewer's own last-reported terminal size,
    /// keyed by an id `register_viewer` hands out — the pty's actual size
    /// is always the componentwise minimum across these (tmux's own model
    /// for multiple attached clients), recomputed on every attach/detach/
    /// resize so nobody's view is ever clipped below what they can
    /// actually see.
    viewer_sizes: Mutex<HashMap<u64, (u16, u16)>>,
    next_viewer_id: AtomicU64,
    /// Same role `run_registry::RunHandle::started_at` plays — real
    /// elapsed time for `GET /api/runs`'s own `uptimeMs`, not reset just
    /// because a client only just noticed this session.
    started_at: std::time::Instant,
}

impl TtySessionHandle {
    pub fn outcome(&self) -> RunOutcome {
        self.outcome.lock().unwrap().clone()
    }

    pub fn uptime_ms(&self) -> u64 {
        self.started_at.elapsed().as_millis() as u64
    }

    /// The full still-buffered byte history, plus a receiver for whatever
    /// comes next (more bytes, then exactly one `Done`) — subscribing
    /// *before* reading the buffered backlog (both under the same lock
    /// here) is what guarantees no chunk already in flight when this is
    /// called is ever missed or double-delivered, same reasoning
    /// `run_registry::RunHandle::subscribe_from` documents.
    pub fn attach(&self) -> (Vec<u8>, broadcast::Receiver<TtyEvent>) {
        let log = self.log.lock().unwrap();
        let rx = self.tx.subscribe();
        (log.snapshot(), rx)
    }

    /// Forwards `bytes` to the pty's stdin — what a viewer typed. No
    /// exclusivity between viewers: whichever one's input arrives goes
    /// straight through, same as multiple attached `tmux` clients typing
    /// into the same pane.
    pub fn write(&self, bytes: Vec<u8>) {
        let _ = self.input_tx.send(bytes);
    }

    /// Registers a new viewer's own reported terminal size and recomputes
    /// the session's actual pty size. Returns an id to hand back to
    /// `resize_viewer`/`forget_viewer` for this same viewer later.
    pub fn register_viewer(&self, cols: u16, rows: u16) -> u64 {
        let id = self.next_viewer_id.fetch_add(1, Ordering::Relaxed);
        self.viewer_sizes.lock().unwrap().insert(id, (cols, rows));
        self.recompute_size();
        id
    }

    /// Updates one already-registered viewer's own size (its own window/
    /// panel was resized) and recomputes the shared minimum.
    pub fn resize_viewer(&self, id: u64, cols: u16, rows: u16) {
        self.viewer_sizes.lock().unwrap().insert(id, (cols, rows));
        self.recompute_size();
    }

    /// Removes a disconnected viewer from the size negotiation — the
    /// shared size may grow back toward whoever's left once this runs.
    /// Does *not* kill the session — a session outlives every one of its
    /// viewers now, on purpose (see this module's own doc comment).
    pub fn forget_viewer(&self, id: u64) {
        self.viewer_sizes.lock().unwrap().remove(&id);
        self.recompute_size();
    }

    fn recompute_size(&self) {
        let sizes = self.viewer_sizes.lock().unwrap();
        let min = sizes
            .values()
            .fold(None, |acc: Option<(u16, u16)>, &(c, r)| {
                Some(match acc {
                    None => (c, r),
                    Some((mc, mr)) => (mc.min(c), mr.min(r)),
                })
            });
        // No viewers left at all — leave the pty at whatever size it last
        // had rather than resizing to nothing.
        if let Some((cols, rows)) = min {
            let _ = self.resize_tx.send((cols, rows));
        }
    }

    /// Signals this session's drain task to kill the pty — a no-op
    /// (returns `false`) if it already finished or was already killed by
    /// an earlier call. Every attached viewer sees the same
    /// `TtyEvent::Done(Killed)` once the drain task actually notices,
    /// regardless of who called this.
    pub fn kill(&self) -> bool {
        self.kill_tx
            .lock()
            .unwrap()
            .take()
            .map(|tx| tx.send(()))
            .is_some()
    }
}

impl TtySessionHandle {
    /// Appends `bytes` to the log and hands them to every attached viewer. No
    /// viewer attached right now is normal and common: the bytes are already
    /// in the ring buffer for whoever attaches next.
    fn publish(&self, bytes: Vec<u8>) {
        self.log.lock().unwrap().push(&bytes);
        let _ = self
            .tx
            .send(TtyEvent::Bytes(Arc::from(bytes.into_boxed_slice())));
    }
}

/// Ends the session the way closing its terminal would, with `SIGKILL` only
/// as the fallback (the same shape as a service's stop): `SIGHUP` at once,
/// `SIGTERM` if it is still there halfway through `grace`, `SIGKILL` once
/// `grace` is over. Output keeps flowing to the viewers meanwhile, so the
/// program's last words are not lost. Returns when the session is gone.
async fn stop_gracefully(
    pty: &mut PtyProcess,
    handle: &TtySessionHandle,
    grace: std::time::Duration,
) {
    const TICK: std::time::Duration = std::time::Duration::from_millis(50);
    let started = std::time::Instant::now();
    let _ = pty.hangup();
    let mut terminated = false;
    while started.elapsed() < grace {
        tokio::select! {
            chunk = pty.output_rx.recv() => match chunk {
                Some(bytes) => handle.publish(bytes),
                // Every holder of the terminal is gone: the session ended.
                None => {
                    let _ = pty.wait().await;
                    return;
                }
            },
            _ = tokio::time::sleep(TICK) => {
                if !terminated && started.elapsed() >= grace / 2 && pty.group_alive() {
                    terminated = true;
                    let _ = pty.terminate();
                }
            }
        }
    }
    let _ = pty.kill();
    let _ = pty.wait().await;
}

/// Registers an already-spawned pty session and hands back a handle whose
/// log/outcome stay live in the background regardless of how many viewers
/// (if any) are currently attached. The caller is expected to insert the
/// returned handle into a registry keyed by address before anyone else can
/// look it up.
///
/// `ledger_row`, if given, is resolved (`RunLedger::finish`) *by this
/// background task itself* once the session actually exits or is killed —
/// see `run_registry::track`'s identical parameter and its own doc comment
/// for why: the whole point of this registry is that a session outlives
/// whatever connection started it, so the ledger row's own lifetime has to
/// track the session's real end, not whenever the originating connection
/// happens to drop.
pub fn track(
    node_id: String,
    block_name: String,
    pty: PtyProcess,
    ledger_row: Option<(crate::run_ledger::RunLedger, i64)>,
) -> Arc<TtySessionHandle> {
    track_with_grace(node_id, block_name, pty, ledger_row, STOP_GRACE)
}

/// [`track`] with an explicit stop grace period (tests use a short one).
fn track_with_grace(
    node_id: String,
    block_name: String,
    mut pty: PtyProcess,
    ledger_row: Option<(crate::run_ledger::RunLedger, i64)>,
    grace: std::time::Duration,
) -> Arc<TtySessionHandle> {
    let (tx, _) = broadcast::channel(1024);
    let (kill_tx, mut kill_rx) = oneshot::channel();
    let handle = Arc::new(TtySessionHandle {
        node_id,
        block_name,
        log: Mutex::new(ByteRing::new(BYTE_CAPACITY)),
        outcome: Mutex::new(RunOutcome::Running),
        tx,
        input_tx: pty.input_sender(),
        resize_tx: pty.resize_sender(),
        kill_tx: Mutex::new(Some(kill_tx)),
        viewer_sizes: Mutex::new(HashMap::new()),
        next_viewer_id: AtomicU64::new(0),
        started_at: std::time::Instant::now(),
    });

    let task_handle = Arc::clone(&handle);
    tokio::spawn(async move {
        let outcome = loop {
            tokio::select! {
                chunk = pty.output_rx.recv() => match chunk {
                    Some(bytes) => task_handle.publish(bytes),
                    None => {
                        let exit_code = pty.wait().await;
                        break RunOutcome::Exited { exit_code };
                    }
                },
                _ = &mut kill_rx => {
                    stop_gracefully(&mut pty, &task_handle, grace).await;
                    break RunOutcome::Killed;
                }
            }
        };
        *task_handle.outcome.lock().unwrap() = outcome.clone();
        let _ = task_handle.tx.send(TtyEvent::Done(outcome.clone()));
        if let Some((ledger, id)) = ledger_row {
            let finish_outcome = match outcome {
                RunOutcome::Exited { exit_code } => {
                    crate::run_ledger::FinishOutcome::Exited(exit_code)
                }
                RunOutcome::Killed => crate::run_ledger::FinishOutcome::Killed,
                RunOutcome::Running => {
                    unreachable!("the loop above only ever breaks with Exited/Killed")
                }
            };
            let _ = ledger.finish(id, finish_outcome);
        }
    });

    handle
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pty_exec;

    fn no_envs() -> [(&'static str, &'static str); 0] {
        []
    }

    #[tokio::test]
    async fn a_late_attacher_gets_the_byte_history_then_the_live_tail() {
        let pty =
            pty_exec::spawn("echo hello", None, None, no_envs(), None, None, (80, 24)).unwrap();
        let handle = track("root".to_string(), "shell".to_string(), pty, None);

        // Poll until the session has actually produced (and this handle
        // has buffered) some output, same robust-under-load pattern
        // `run_registry`'s own equivalent test uses instead of a fixed
        // sleep.
        let mut backlog = Vec::new();
        for _ in 0..50 {
            let (b, _) = handle.attach();
            if !b.is_empty() {
                backlog = b;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert!(
            String::from_utf8_lossy(&backlog).contains("hello"),
            "expected buffered output to contain the echoed text, got: {:?}",
            String::from_utf8_lossy(&backlog)
        );

        // Drain to completion via a fresh attach — proves outcome resolves
        // even for an attacher that only shows up after the fact.
        loop {
            if matches!(handle.outcome(), RunOutcome::Exited { .. }) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert_eq!(handle.outcome(), RunOutcome::Exited { exit_code: 0 });
    }

    #[tokio::test]
    async fn pty_size_tracks_the_minimum_across_every_attached_viewer() {
        let pty = pty_exec::spawn("sleep 30", None, None, no_envs(), None, None, (80, 24)).unwrap();
        let handle = track("root".to_string(), "shell".to_string(), pty, None);

        let a = handle.register_viewer(100, 40);
        let b = handle.register_viewer(80, 24);
        // No direct way to read the pty's own current size back through
        // this handle (it only ever pushes resize requests one-way,
        // exactly like a real pty) — this test exercises that `recompute_
        // size` doesn't panic and that removing the smaller viewer lets a
        // later resize widen again, via the size-tracking map itself
        // rather than the pty's own internals.
        assert_eq!(
            *handle.viewer_sizes.lock().unwrap().get(&a).unwrap(),
            (100, 40)
        );
        assert_eq!(
            *handle.viewer_sizes.lock().unwrap().get(&b).unwrap(),
            (80, 24)
        );

        handle.forget_viewer(b);
        assert!(!handle.viewer_sizes.lock().unwrap().contains_key(&b));
        assert!(handle.viewer_sizes.lock().unwrap().contains_key(&a));

        handle.kill();
    }

    /// Polls until the session's log contains `needle`.
    async fn wait_for_output(handle: &TtySessionHandle, needle: &str) {
        for _ in 0..200 {
            let (backlog, _) = handle.attach();
            if String::from_utf8_lossy(&backlog).contains(needle) {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        panic!("never saw {needle:?} in the session output");
    }

    /// Polls until the session has an outcome and returns how long that took.
    async fn wait_for_end(handle: &TtySessionHandle) -> std::time::Duration {
        let started = std::time::Instant::now();
        while matches!(handle.outcome(), RunOutcome::Running) {
            assert!(
                started.elapsed() < std::time::Duration::from_secs(15),
                "the session never ended"
            );
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        started.elapsed()
    }

    #[tokio::test]
    async fn a_killed_session_gets_to_clean_up_before_any_sigkill() {
        let marker = std::env::temp_dir().join(format!(
            "mfx-tty-cleanup-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let code = format!(
            "trap 'echo bye; echo cleaned > {}; exit 0' HUP TERM\necho ready\nwhile :; do sleep 0.1; done",
            marker.display()
        );
        let pty = pty_exec::spawn(&code, None, None, no_envs(), None, None, (80, 24)).unwrap();
        let handle = track("root".to_string(), "shell".to_string(), pty, None);
        wait_for_output(&handle, "ready").await;

        assert!(handle.kill());
        let took = wait_for_end(&handle).await;

        assert!(matches!(handle.outcome(), RunOutcome::Killed));
        assert!(marker.exists(), "the cleanup handler never ran");
        // Far less than the grace period: it ended as soon as it was asked.
        assert!(took < STOP_GRACE, "took {took:?}");
        // What it printed while stopping reached the log.
        let (backlog, _) = handle.attach();
        assert!(String::from_utf8_lossy(&backlog).contains("bye"));
        let _ = std::fs::remove_file(marker);
    }

    #[tokio::test]
    async fn a_session_that_ignores_hangup_and_terminate_is_killed_after_the_grace() {
        let code = "trap '' HUP TERM\necho ready\nwhile :; do sleep 0.1; done";
        let pty = pty_exec::spawn(code, None, None, no_envs(), None, None, (80, 24)).unwrap();
        let pid = pty.pid();
        let grace = std::time::Duration::from_millis(1000);
        let handle = track_with_grace("root".to_string(), "shell".to_string(), pty, None, grace);
        wait_for_output(&handle, "ready").await;

        assert!(handle.kill());
        let took = wait_for_end(&handle).await;

        assert!(matches!(handle.outcome(), RunOutcome::Killed));
        assert!(
            took >= grace,
            "SIGKILL came before the grace ran out: {took:?}"
        );
        assert!(took < grace * 4, "took {took:?}");
        // SAFETY: signal 0 only checks that the group exists.
        let alive = unsafe { libc::kill(-pid, 0) } == 0;
        assert!(!alive, "the session's process group is still alive");
    }

    #[tokio::test]
    async fn kill_stops_the_session_and_every_viewer_sees_it() {
        let pty = pty_exec::spawn("sleep 30", None, None, no_envs(), None, None, (80, 24)).unwrap();
        let handle = track("root".to_string(), "shell".to_string(), pty, None);
        let (_, mut rx1) = handle.attach();
        let (_, mut rx2) = handle.attach();

        assert!(handle.kill());
        for rx in [&mut rx1, &mut rx2] {
            let event = rx.recv().await.unwrap();
            assert!(matches!(event, TtyEvent::Done(RunOutcome::Killed)));
        }
        assert_eq!(handle.outcome(), RunOutcome::Killed);
    }
}
