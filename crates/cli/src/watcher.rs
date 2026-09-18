//! `meshfox view <path>`'s coordinator — see TODO.canvas.md's "Ссылки и
//! навигация между канвасами". A top-level `meshfox view` invocation
//! (no `--watcher-socket` on its own `View` command — see `main.rs`)
//! *becomes* a watcher: it binds a private, per-invocation Unix socket,
//! spawns exactly one worker (a plain `meshfox view <path> --watcher-socket
//! <socket>`) for the file the user actually asked for, and from then on
//! is the single place that handles "a worker just bound a port, maybe open
//! a browser tab for it", "a worker wants another canvas opened", and "a
//! worker wants a plain file opened" — see
//! `meshfox_server::watcher_protocol`'s own doc comment for why this lives
//! behind a stable, language-agnostic wire protocol rather than in-process
//! state a worker could reach directly.
//!
//! Deliberately a plain parent-owns-children process tree, not a detached
//! system-wide daemon (contrast the earlier, now-removed
//! `meshfox_core::view_registry`): the watcher stays alive exactly as long
//! as it has at least one live worker, and killing it (Ctrl-C, `kill`,
//! closing the terminal) kills every worker it's tracking right along with
//! it. That's the whole point — "no dangling background processes once
//! you're done" is a property of an ordinary process tree, not something
//! that needs its own bookkeeping. A `.canvas.md` opened from another one
//! stays independent of *that* specific tab closing (its own worker
//! auto-exits on its own schedule, same as always — see
//! `meshfox_server::run`'s `TabGuard`), but the *watcher* itself only goes
//! away once every worker it ever spawned, across the whole session, is
//! gone.
//!
//! A persistent, detached, user-visible coordinator (a menu-bar app on
//! macOS, eventually) is a deliberately separate thing — same wire
//! protocol, entirely different lifecycle policy — not implemented here.

use meshfox_server::watcher_protocol::{AckResponse, Message};
use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::process::{Child, Command};
use tokio::sync::{broadcast, oneshot, Notify};

/// One tracked worker. `port` is `None` until its own `Ready` message
/// arrives. `pending_open` is `Some(fragment)` the moment somebody (the
/// initial invocation, or a later `Open` request) wants a browser tab
/// opened for it — carrying whichever `Open` request's own fragment
/// should be used once that happens (`Some(None)` for "no fragment, just
/// the root") — and is consumed (opened, cleared) the instant a port
/// becomes known, in `Registry::mark_ready`. `None` means nobody's
/// waiting on this one.
struct Entry {
    port: Option<u16>,
    pending_open: Option<Option<String>>,
    /// Every still-unanswered `Open` request waiting on this worker's own
    /// `Ready` (or its failure) — one per concurrent caller, each replied
    /// to exactly once, by `mark_ready` on success or `remove` on failure.
    /// Empty for a worker nobody's asked about over the socket (the
    /// top-level invocation's own primary worker, spawned directly by
    /// `run` rather than via an `Open` message).
    waiters: Vec<oneshot::Sender<Result<(), String>>>,
}

/// Shared state the accept loop and every per-worker watch task touch.
/// Never holds a `Child` — each worker's own task owns it exclusively
/// (needed to both `.wait()` on it *and* `.start_kill()` it from the same
/// place without fighting over `&mut` access) and only reports back here
/// via `remove`.
struct Registry {
    entries: Mutex<HashMap<PathBuf, Entry>>,
    /// Fired whenever `entries` transitions to empty — `wait_until_empty`
    /// loops on this rather than polling, so the watcher notices "nothing
    /// left to do" the instant it's true rather than on some delay.
    empty: Notify,
    /// Broadcasts once, when the watcher itself is shutting down (Ctrl-C,
    /// SIGTERM, or — belt and suspenders — natural emptiness) — every
    /// per-worker task holds its own `Receiver` and kills its own child on
    /// the first (and only) value that ever arrives.
    shutdown: broadcast::Sender<()>,
}

impl Registry {
    fn new() -> Self {
        let (shutdown, _) = broadcast::channel(1);
        Self {
            entries: Mutex::new(HashMap::new()),
            empty: Notify::new(),
            shutdown,
        }
    }

    fn is_empty(&self) -> bool {
        self.entries.lock().unwrap().is_empty()
    }

    async fn wait_until_empty(&self) {
        loop {
            if self.is_empty() {
                return;
            }
            self.empty.notified().await;
        }
    }

    fn signal_shutdown(&self) {
        let _ = self.shutdown.send(());
    }

    /// A worker's own `Ready` arrived — record its port, open a browser tab
    /// if anyone's waiting on that, and satisfy every `Open` caller
    /// currently waiting on this same worker with `Ok(())`. `path` is
    /// trusted as already the same canonical form this registry
    /// spawned/keys by (it's echoed straight back from what the watcher
    /// itself passed the worker as an argument).
    fn mark_ready(&self, path: &Path, port: u16) {
        let (pending_open, waiters) = {
            let mut entries = self.entries.lock().unwrap();
            // A `Ready` for something we're no longer tracking (already
            // killed?) — ignore.
            let Some(entry) = entries.get_mut(path) else {
                return;
            };
            entry.port = Some(port);
            (entry.pending_open.take(), std::mem::take(&mut entry.waiters))
        };
        if let Some(fragment) = pending_open {
            open_browser_tab(port, fragment.as_deref());
        }
        for waiter in waiters {
            let _ = waiter.send(Ok(()));
        }
    }

    /// Registers `waiter` on `path`'s own entry (which must already exist —
    /// callers create it via `spawn_worker` first) — for an `Open` request
    /// that arrived while this worker is still spawning.
    fn add_waiter(&self, path: &Path, waiter: oneshot::Sender<Result<(), String>>) {
        if let Some(entry) = self.entries.lock().unwrap().get_mut(path) {
            entry.waiters.push(waiter);
        } else {
            // The entry vanished between this being decided and now (the
            // worker already failed and was removed) — fail immediately
            // rather than leaving the caller waiting on a channel nothing
            // will ever signal.
            let _ = waiter.send(Err("worker exited before reporting ready".to_string()));
        }
    }

    /// Removes `path`'s entry (its worker task is the sole caller, once
    /// its child has actually exited) and wakes `wait_until_empty` if that
    /// was the last one. `failure_reason`, when the worker never reported
    /// `Ready` at all, fails every still-waiting `Open` caller with it —
    /// `None` for a worker that already had a port (nobody's left waiting;
    /// `mark_ready` already satisfied everyone) or that never had any
    /// waiters to begin with.
    fn remove(&self, path: &Path, failure_reason: Option<String>) {
        let (now_empty, waiters) = {
            let mut entries = self.entries.lock().unwrap();
            let waiters = entries.remove(path).map(|e| e.waiters).unwrap_or_default();
            (entries.is_empty(), waiters)
        };
        if let Some(reason) = failure_reason {
            for waiter in waiters {
                let _ = waiter.send(Err(reason.clone()));
            }
        }
        if now_empty {
            self.empty.notify_waiters();
        }
    }
}

/// `http://127.0.0.1:<port>/[#fragment]`, best-effort — same reasoning
/// `meshfox view`'s old direct `open::that` call always had: no browser,
/// no display, or an unsupported platform shouldn't be fatal to anything,
/// just means the user opens the URL by hand. Runs on a blocking thread
/// since `open::that` shells out synchronously.
fn open_browser_tab(port: u16, fragment: Option<&str>) {
    let mut url = format!("http://127.0.0.1:{port}/");
    if let Some(fragment) = fragment {
        url.push('#');
        url.push_str(fragment);
    }
    tokio::task::spawn_blocking(move || {
        if let Err(e) = open::that(&url) {
            eprintln!("meshfox: couldn't open a browser automatically ({e}) — open {url} yourself");
        }
    });
}

/// A plain file, opened however this platform's default association says
/// to (`open` on macOS, `xdg-open` on Linux, `start` on Windows) — this
/// watcher's own answer to `Message::OpenFile`, the same "hand it to the
/// OS" behavior `open_node_file` used to do itself before that moved up
/// here. Awaited (unlike the fire-and-forget `open_browser_tab`) so its
/// real success/failure can become this request's own `AckResponse` —
/// still off the async runtime's own worker threads, on a blocking one,
/// since `open::that` shells out synchronously.
async fn open_plain_file(path: PathBuf) -> Result<(), String> {
    tokio::task::spawn_blocking(move || open::that(&path).map_err(|e| e.to_string()))
        .await
        .unwrap_or_else(|e| Err(format!("couldn't open the file: {e}")))
}

/// How long an `Open` request waits for the worker it's spawning (or
/// already waiting on) to report `Ready` before giving up on *this
/// specific caller* — the worker itself is left running regardless (it
/// might still come up, and a later request for the same canvas benefits
/// from that), unlike the macOS daemon's own `SessionStore` (which kills a
/// worker that blows this same budget — see its own doc comment): that
/// more aggressive policy was built around one specific incident (a wedged
/// accept loop that made every worker look stuck), already fixed at its
/// own root (`UnixSocketServer.swift`'s `accept()` retry) rather than
/// worth re-defending against here too. Same value as `SessionStore`'s
/// `getPortTimeoutSeconds`/`openTimeoutSeconds` regardless, so a caller
/// waiting on either coordinator gives up on roughly the same schedule.
const WORKER_READY_TIMEOUT: Duration = Duration::from_secs(15);

/// A crashed worker's own captured stderr almost always starts with
/// `view_worker`'s own `eprintln!("meshfox view: {e}")` (`main.rs`) — a
/// sensible prefix when that's the *only* thing printed to a real
/// terminal, redundant once it's relayed as the reason inside an
/// `AckResponse::Error`/`PortResponse::Error` that the ultimate caller
/// (`view_or_hand_off`'s own `eprintln!("meshfox view: {e}")`) prefixes
/// itself. Stripped so a caller doesn't see it doubled up.
fn strip_meshfox_view_prefix(text: &str) -> &str {
    text.strip_prefix("meshfox view: ").unwrap_or(text)
}

/// Caps how much of a failed worker's own stderr gets relayed back over
/// the socket as an `AckResponse::Error` — enough for the one line that
/// actually matters (`meshfox view: <reason>`, or a Rust panic's own
/// message) without an unbounded/adversarial worker turning a JSON reply
/// line into a multi-megabyte one.
const CAPTURED_STDERR_LIMIT: usize = 4096;

/// Spawns a worker for `canonical_path` (already canonicalized by the
/// caller) and tracks it: inserts a `port: None` entry, then hands the
/// `Child` to its own dedicated task, which owns it for the rest of its
/// life — `.wait()`s for a natural exit, or kills it early on `shutdown`
/// — and removes its own registry entry once it's actually gone. The
/// child's own stderr is captured (and still echoed to this process's own,
/// so a locally-run `meshfox view` doesn't lose that visibility) so a
/// worker that dies before ever reporting `Ready` can fail any `Open`
/// caller waiting on it with the worker's own real reason, not just "it
/// exited" — see `watch_worker`.
fn spawn_worker(
    registry: &Arc<Registry>,
    exe: &Path,
    watcher_socket: &Path,
    canonical_path: PathBuf,
    port: u16,
    pending_open: Option<Option<String>>,
    auto_exit: bool,
    initial_waiter: Option<oneshot::Sender<Result<(), String>>>,
) -> io::Result<()> {
    let mut command = Command::new(exe);
    command
        .arg("view")
        .arg(&canonical_path)
        .arg("--port")
        .arg(port.to_string())
        .arg("--watcher-socket")
        .arg(watcher_socket);
    if !auto_exit {
        command.arg("--no-auto-exit");
    }
    let mut child = command
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()?;

    let captured_stderr = Arc::new(Mutex::new(String::new()));
    if let Some(stderr) = child.stderr.take() {
        let captured_stderr = Arc::clone(&captured_stderr);
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                eprintln!("{line}");
                let mut buf = captured_stderr.lock().unwrap();
                if buf.len() < CAPTURED_STDERR_LIMIT {
                    if !buf.is_empty() {
                        buf.push('\n');
                    }
                    buf.push_str(&line);
                }
            }
        });
    }

    registry.entries.lock().unwrap().insert(
        canonical_path.clone(),
        Entry {
            port: None,
            pending_open,
            waiters: initial_waiter.into_iter().collect(),
        },
    );

    let registry = Arc::clone(registry);
    let mut shutdown_rx = registry.shutdown.subscribe();
    tokio::spawn(async move {
        watch_worker(child, &mut shutdown_rx).await;
        let had_port = registry
            .entries
            .lock()
            .unwrap()
            .get(&canonical_path)
            .map(|e| e.port.is_some())
            .unwrap_or(false);
        let failure_reason = if had_port {
            None
        } else {
            let captured = captured_stderr.lock().unwrap().clone();
            Some(if captured.is_empty() {
                "worker exited before reporting ready".to_string()
            } else {
                strip_meshfox_view_prefix(&captured).to_string()
            })
        };
        registry.remove(&canonical_path, failure_reason);
    });
    Ok(())
}

/// Runs until `child` exits, one way or another: naturally, or because
/// `shutdown` fired (the watcher itself is going down) — in which case
/// this kills it and then still waits, so the caller never sees this
/// return before the process is actually gone (matters for a worker
/// holding real subprocess trees of its own, same reasoning
/// `stream_exec::SpawnedProcess::kill` documents).
async fn watch_worker(mut child: Child, shutdown_rx: &mut broadcast::Receiver<()>) {
    tokio::select! {
        _ = child.wait() => {}
        _ = shutdown_rx.recv() => {
            let _ = child.start_kill();
            let _ = child.wait().await;
        }
    }
}

/// A short, unique-enough-per-process private socket path — deliberately
/// terse: a Unix domain socket path has a tight length budget (`SUN_LEN`,
/// ~104 bytes on macOS/BSD), and `std::env::temp_dir()` alone can already
/// eat half of that (macOS's `/var/folders/.../T/`). One watcher per
/// process, so its own pid is already all the uniqueness this needs.
fn private_socket_path() -> PathBuf {
    std::env::temp_dir().join(format!("mfx-w-{}.sock", std::process::id()))
}

fn ack_json(result: &Result<(), String>) -> String {
    let reply = match result {
        Ok(()) => AckResponse::Ok {},
        Err(error) => AckResponse::Error { error: error.clone() },
    };
    let mut line = serde_json::to_string(&reply).expect("AckResponse always serializes");
    line.push('\n');
    line
}

/// Handles one already-accepted connection: reads exactly one
/// newline-delimited JSON `Message` (matches `watcher_protocol::send`'s/
/// `request_and_await_reply`'s own one-shot write side) and acts on it.
/// `Ready` stays fire-and-forget (nothing meaningful to reply with, and
/// nobody's waiting on a reply to it); `Open`/`OpenFile` now both write an
/// `AckResponse` back on this same connection before returning — see
/// `meshfox_server::watcher_protocol`'s own doc comment for why that
/// stopped being optional. A malformed or empty read is just dropped, same
/// as before.
async fn handle_connection(stream: UnixStream, registry: Arc<Registry>, exe: PathBuf, socket_path: PathBuf) {
    let (read_half, mut write_half) = stream.into_split();
    let mut line = String::new();
    if BufReader::new(read_half).read_line(&mut line).await.unwrap_or(0) == 0 {
        return;
    }
    let Ok(msg) = serde_json::from_str::<Message>(line.trim()) else {
        return;
    };
    match msg {
        Message::Ready { canvas_path, port } => {
            registry.mark_ready(&canvas_path, port);
        }
        Message::Open { canvas_path, fragment } => {
            let canonical = canvas_path.canonicalize().unwrap_or(canvas_path);

            // Three cases, matching exactly what was asked for: already
            // open (a port is known) → show it now, ack immediately;
            // already spawning (tracked, no port yet) → flag it wanted
            // (with this request's own fragment) and wait alongside
            // whoever else is already waiting; never seen at all → spawn
            // it, wanted from the start, and wait the same way.
            enum Action {
                OpenNow(u16),
                Wait,
                Spawn,
            }
            let action = {
                let mut entries = registry.entries.lock().unwrap();
                match entries.get_mut(&canonical) {
                    Some(entry) => match entry.port {
                        Some(port) => Action::OpenNow(port),
                        None => {
                            entry.pending_open = Some(fragment.clone());
                            Action::Wait
                        }
                    },
                    None => Action::Spawn,
                }
            };
            let result = match action {
                Action::OpenNow(port) => {
                    open_browser_tab(port, fragment.as_deref());
                    Ok(())
                }
                Action::Wait => {
                    let (tx, rx) = oneshot::channel();
                    registry.add_waiter(&canonical, tx);
                    await_ready(rx).await
                }
                Action::Spawn => {
                    // `port: 0` (let the OS pick) and `auto_exit: true`
                    // (exits on its own once its own tabs all close) —
                    // same defaults every navigated-to worker has always
                    // had.
                    let (tx, rx) = oneshot::channel();
                    match spawn_worker(&registry, &exe, &socket_path, canonical, 0, Some(fragment), true, Some(tx)) {
                        Ok(()) => await_ready(rx).await,
                        Err(e) => Err(format!("couldn't spawn a worker for the requested canvas: {e}")),
                    }
                }
            };
            let _ = write_half.write_all(ack_json(&result).as_bytes()).await;
        }
        Message::OpenFile { path } => {
            let result = open_plain_file(path).await;
            let _ = write_half.write_all(ack_json(&result).as_bytes()).await;
        }
        // Deliberately unsupported here: `GetPort` is a request-reply
        // message too, but this watcher is a private, per-`view`-invocation
        // process nobody's `server_socket` has a reason to point at (a
        // persistent, addressable coordinator — the macOS daemon, e.g. —
        // implements it instead) — see `crates/cli/src/coordinator.rs`'s
        // own doc comment. Closing without a reply here would leave a
        // real `GetPort` caller waiting out its own timeout for nothing,
        // but nothing in this codebase ever actually sends one here, so
        // that's a non-issue in practice, not a gap worth closing.
        Message::GetPort { .. } => {}
    }
}

/// Waits for `rx` to resolve (a worker this caller is waiting on either
/// reported `Ready` or failed), or gives up after `WORKER_READY_TIMEOUT` —
/// see that constant's own doc comment for why a timeout here doesn't also
/// kill the worker, unlike the macOS daemon's equivalent.
async fn await_ready(rx: oneshot::Receiver<Result<(), String>>) -> Result<(), String> {
    match tokio::time::timeout(WORKER_READY_TIMEOUT, rx).await {
        Ok(Ok(result)) => result,
        Ok(Err(_)) => Err("worker's own tracking task ended without reporting an outcome".to_string()),
        Err(_) => Err(format!(
            "worker didn't report ready within {}s",
            WORKER_READY_TIMEOUT.as_secs()
        )),
    }
}

/// Runs the watcher: binds its private socket, spawns the primary worker
/// (the file this `meshfox view` invocation actually asked for), then
/// blocks until either the whole family has exited on its own or
/// something asks the watcher to stop early (Ctrl-C, `SIGTERM`) — at
/// which point every still-live worker is killed too (see `watch_worker`)
/// before this returns. Never returns an `Err` for "a worker's own run
/// failed" (that's the worker's own problem, reported on its own stdout/
/// exit code, and relayed to any `Open`/`GetPort` caller waiting on it —
/// see `spawn_worker`) — only for something that stops the watcher itself
/// from standing up at all (can't bind its socket, can't spawn the primary
/// worker).
pub async fn run(
    exe: PathBuf,
    canvas_path: PathBuf,
    port: u16,
    open_browser: bool,
    auto_exit: bool,
) -> io::Result<()> {
    let canonical = canvas_path.canonicalize()?;
    let socket_path = private_socket_path();
    // A stale socket file (this exact pid reused since a previous, very
    // unclean exit) has nothing listening behind it — clear it so `bind`
    // doesn't fail with `AddrInUse` for no real reason.
    let _ = std::fs::remove_file(&socket_path);
    let listener = UnixListener::bind(&socket_path)?;

    let registry = Arc::new(Registry::new());
    let initial_open = if open_browser { Some(None) } else { None };
    spawn_worker(&registry, &exe, &socket_path, canonical, port, initial_open, auto_exit, None)?;

    let accept_registry = Arc::clone(&registry);
    let accept_exe = exe.clone();
    let accept_socket = socket_path.clone();
    let accept_task = tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                continue;
            };
            tokio::spawn(handle_connection(
                stream,
                Arc::clone(&accept_registry),
                accept_exe.clone(),
                accept_socket.clone(),
            ));
        }
    });

    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = wait_for_terminate() => {}
        _ = registry.wait_until_empty() => {}
    }

    registry.signal_shutdown();
    // Give every worker task a moment to actually kill+reap its child and
    // deregister — best-effort, not load-bearing for correctness (each
    // task keeps running to completion regardless of this timing out).
    let _ = tokio::time::timeout(Duration::from_secs(5), registry.wait_until_empty()).await;
    accept_task.abort();
    let _ = std::fs::remove_file(&socket_path);
    Ok(())
}

#[cfg(unix)]
async fn wait_for_terminate() {
    use tokio::signal::unix::{signal, SignalKind};
    match signal(SignalKind::terminate()) {
        Ok(mut stream) => {
            stream.recv().await;
        }
        Err(_) => std::future::pending().await,
    }
}

#[cfg(not(unix))]
async fn wait_for_terminate() {
    std::future::pending().await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn current_exe_or_skip() -> Option<PathBuf> {
        std::env::current_exe().ok()
    }

    fn empty_entry() -> Entry {
        Entry { port: None, pending_open: None, waiters: Vec::new() }
    }

    #[tokio::test]
    async fn registry_wait_until_empty_resolves_once_the_last_entry_is_removed() {
        let registry = Arc::new(Registry::new());
        registry
            .entries
            .lock()
            .unwrap()
            .insert(PathBuf::from("/tmp/a.canvas.md"), Entry { port: Some(1), ..empty_entry() });
        assert!(!registry.is_empty());

        let wait_registry = Arc::clone(&registry);
        let wait_task = tokio::spawn(async move { wait_registry.wait_until_empty().await });

        // Give the waiter a moment to actually start waiting before we
        // remove the only entry.
        tokio::time::sleep(Duration::from_millis(20)).await;
        registry.remove(&PathBuf::from("/tmp/a.canvas.md"), None);

        tokio::time::timeout(Duration::from_secs(2), wait_task)
            .await
            .expect("wait_until_empty should resolve promptly")
            .unwrap();
    }

    #[tokio::test]
    async fn mark_ready_opens_the_pending_fragment_and_satisfies_waiters() {
        let registry = Registry::new();
        let (tx, rx) = oneshot::channel();
        registry.entries.lock().unwrap().insert(
            PathBuf::from("/tmp/a.canvas.md"),
            Entry {
                pending_open: Some(Some("some-node".to_string())),
                waiters: vec![tx],
                ..empty_entry()
            },
        );

        registry.mark_ready(Path::new("/tmp/a.canvas.md"), 4242);

        let entries = registry.entries.lock().unwrap();
        let entry = entries.get(Path::new("/tmp/a.canvas.md")).unwrap();
        assert_eq!(entry.port, Some(4242));
        assert_eq!(entry.pending_open, None);
        assert!(entry.waiters.is_empty());
        drop(entries);

        assert_eq!(rx.await.unwrap(), Ok(()));
    }

    #[tokio::test]
    async fn mark_ready_for_an_untracked_path_is_a_harmless_no_op() {
        let registry = Registry::new();
        registry.mark_ready(Path::new("/tmp/nope.canvas.md"), 4242);
        assert!(registry.is_empty());
    }

    #[tokio::test]
    async fn remove_with_a_failure_reason_fails_every_waiter() {
        let registry = Registry::new();
        let (tx1, rx1) = oneshot::channel();
        let (tx2, rx2) = oneshot::channel();
        registry.entries.lock().unwrap().insert(
            PathBuf::from("/tmp/a.canvas.md"),
            Entry { waiters: vec![tx1, tx2], ..empty_entry() },
        );

        registry.remove(Path::new("/tmp/a.canvas.md"), Some("boom".to_string()));

        assert_eq!(rx1.await.unwrap(), Err("boom".to_string()));
        assert_eq!(rx2.await.unwrap(), Err("boom".to_string()));
        assert!(registry.is_empty());
    }

    #[tokio::test]
    async fn add_waiter_for_an_untracked_path_fails_immediately() {
        let registry = Registry::new();
        let (tx, rx) = oneshot::channel();
        registry.add_waiter(Path::new("/tmp/gone.canvas.md"), tx);
        assert!(rx.await.unwrap().is_err());
    }

    /// End-to-end: spawns this very test binary (standing in for `meshfox`
    /// — `current_exe()` inside `cargo test` is the test binary, not the
    /// real CLI, so this only proves the process-tree mechanics, not a
    /// real `meshfox view` handshake) as a "worker" that just reports
    /// ready over the socket and exits, and confirms the registry's own
    /// bookkeeping reacts correctly without ever touching `spawn_worker`'s
    /// own argument-shape (which does assume a real `meshfox` binary).
    #[tokio::test]
    async fn a_worker_reporting_ready_over_the_real_socket_updates_the_registry() {
        let Some(_exe) = current_exe_or_skip() else {
            return;
        };
        let socket_path = std::env::temp_dir().join(format!("mfx-w-test-{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&socket_path);
        let listener = UnixListener::bind(&socket_path).unwrap();

        let registry = Arc::new(Registry::new());
        let canonical = PathBuf::from("/tmp/reported.canvas.md");
        registry
            .entries
            .lock()
            .unwrap()
            .insert(canonical.clone(), empty_entry());

        let accept_registry = Arc::clone(&registry);
        let exe = PathBuf::from("/bin/true"); // never actually spawned in this test
        let socket_for_accept = socket_path.clone();
        let accept_task = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            handle_connection(stream, accept_registry, exe, socket_for_accept).await;
        });

        meshfox_server::watcher_protocol::notify_ready(&socket_path, &canonical, 9999)
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(2), accept_task).await.unwrap().unwrap();

        let entries = registry.entries.lock().unwrap();
        assert_eq!(entries.get(&canonical).unwrap().port, Some(9999));

        let _ = std::fs::remove_file(&socket_path);
    }

    /// The other end of the same real-socket path, now for `Open` itself:
    /// spawns a real `meshfox` process (this test binary standing in for
    /// it, same as above) is overkill here — this drives `handle_connection`
    /// directly against an already-tracked, already-ready entry, confirming
    /// the `OpenNow` branch acks immediately rather than waiting on
    /// anything.
    #[tokio::test]
    async fn open_for_an_already_ready_worker_acks_immediately() {
        let socket_path = std::env::temp_dir().join(format!("mfx-w-open-ready-{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&socket_path);
        let listener = UnixListener::bind(&socket_path).unwrap();

        let registry = Arc::new(Registry::new());
        let canvas_path = std::env::temp_dir().join("already-ready.canvas.md");
        let canonical = canvas_path.canonicalize().unwrap_or_else(|_| canvas_path.clone());
        registry
            .entries
            .lock()
            .unwrap()
            .insert(canonical.clone(), Entry { port: Some(7777), ..empty_entry() });

        let accept_registry = Arc::clone(&registry);
        let exe = PathBuf::from("/bin/true");
        let socket_for_accept = socket_path.clone();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            handle_connection(stream, accept_registry, exe, socket_for_accept).await;
        });

        let result = tokio::time::timeout(
            Duration::from_secs(2),
            meshfox_server::watcher_protocol::request_open(&socket_path, &canonical, None),
        )
        .await
        .expect("should ack promptly, not hang");
        assert!(result.is_ok());

        let _ = std::fs::remove_file(&socket_path);
    }
}
