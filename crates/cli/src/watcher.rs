//! `meshfox view <path>`'s coordinator — see TODO.canvas.md's "Ссылки и
//! навигация между канвасами". A top-level `meshfox view` invocation
//! (no `--watcher-socket` on its own `View` command — see `main.rs`)
//! *becomes* a watcher: it binds a private, per-invocation Unix socket,
//! spawns exactly one worker (a plain `meshfox view <path> --watcher-socket
//! <socket>`) for the file the user actually asked for, and from then on
//! is the single place that handles both "a worker just bound a port,
//! maybe open a browser tab for it" and "a worker wants another canvas
//! opened" — see `meshfox_server::watcher_protocol`'s own doc comment for
//! why this lives behind a stable, language-agnostic wire protocol rather
//! than in-process state a worker could reach directly.
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

use meshfox_server::watcher_protocol::Message;
use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::process::{Child, Command};
use tokio::sync::{broadcast, Notify};

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
}

/// Shared state the accept loop and every per-worker watch task touch.
/// Never holds a `Child` — each worker's own task owns its `Child`
/// exclusively (needed to both `.wait()` on it *and* `.start_kill()` it
/// from the same place without fighting over `&mut` access) and only
/// reports back here via `remove`.
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

    /// A worker's own `Ready` arrived — record its port, and open a
    /// browser tab now (with whichever fragment the pending request
    /// asked for) if anyone's waiting on it. `path` is trusted as already
    /// the same canonical form this registry spawned/keys by (it's echoed
    /// straight back from what the watcher itself passed the worker as an
    /// argument).
    fn mark_ready(&self, path: &Path, port: u16) {
        let pending = {
            let mut entries = self.entries.lock().unwrap();
            let Some(entry) = entries.get_mut(path) else {
                return; // a Ready for something we're no longer tracking (already killed?) — ignore
            };
            entry.port = Some(port);
            entry.pending_open.take()
        };
        if let Some(fragment) = pending {
            open_browser_tab(port, fragment.as_deref());
        }
    }

    /// Removes `path`'s entry (its worker task is the sole caller, once
    /// its child has actually exited) and wakes `wait_until_empty` if that
    /// was the last one.
    fn remove(&self, path: &Path) {
        let now_empty = {
            let mut entries = self.entries.lock().unwrap();
            entries.remove(path);
            entries.is_empty()
        };
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

/// Spawns a worker for `canonical_path` (already canonicalized by the
/// caller) and tracks it: inserts a `port: None` entry, then hands the
/// `Child` to its own dedicated task, which owns it for the rest of its
/// life — `.wait()`s for a natural exit, or kills it early on `shutdown`
/// — and removes its own registry entry once it's actually gone.
fn spawn_worker(
    registry: &Arc<Registry>,
    exe: &Path,
    watcher_socket: &Path,
    canonical_path: PathBuf,
    port: u16,
    pending_open: Option<Option<String>>,
    auto_exit: bool,
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
    let child = command
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()?;

    registry.entries.lock().unwrap().insert(
        canonical_path.clone(),
        Entry {
            port: None,
            pending_open,
        },
    );

    let registry = Arc::clone(registry);
    let mut shutdown_rx = registry.shutdown.subscribe();
    tokio::spawn(async move {
        watch_worker(child, &mut shutdown_rx).await;
        registry.remove(&canonical_path);
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

/// Handles one already-accepted connection: reads exactly one
/// newline-delimited JSON `Message` (matches `watcher_protocol::send`'s
/// own one-shot-then-shutdown write side) and acts on it. A malformed or
/// empty read is just dropped — nothing meaningful to reply with over
/// this one-way protocol, and a worker that fails to report in just
/// leaves its own entry pending/never-ready rather than wedging anything
/// else.
async fn handle_connection(stream: UnixStream, registry: Arc<Registry>, exe: PathBuf, socket_path: PathBuf) {
    let mut line = String::new();
    if BufReader::new(stream).read_line(&mut line).await.unwrap_or(0) == 0 {
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
            // open (a port is known) → show it now; already spawning
            // (tracked, no port yet) → just flag it wanted (with this
            // request's own fragment) — `mark_ready` opens it once the
            // port lands; never seen at all → spawn it, wanted from the
            // start. Resolved and dropped before `spawn_worker` (which
            // takes the same lock itself, to insert its own entry).
            enum Action {
                OpenNow(u16),
                AlreadyPending,
                Spawn,
            }
            let action = {
                let mut entries = registry.entries.lock().unwrap();
                match entries.get_mut(&canonical) {
                    Some(entry) => match entry.port {
                        Some(port) => Action::OpenNow(port),
                        None => {
                            entry.pending_open = Some(fragment.clone());
                            Action::AlreadyPending
                        }
                    },
                    None => Action::Spawn,
                }
            };
            match action {
                Action::OpenNow(port) => open_browser_tab(port, fragment.as_deref()),
                Action::AlreadyPending => {}
                Action::Spawn => {
                    // `port: 0` (let the OS pick) and `auto_exit: true`
                    // (exits on its own once its own tabs all close) —
                    // same defaults every navigated-to worker has always
                    // had.
                    if let Err(e) =
                        spawn_worker(&registry, &exe, &socket_path, canonical, 0, Some(fragment), true)
                    {
                        eprintln!("meshfox: couldn't spawn a worker for the requested canvas: {e}");
                    }
                }
            }
        }
    }
}

/// Runs the watcher: binds its private socket, spawns the primary worker
/// (the file this `meshfox view` invocation actually asked for), then
/// blocks until either the whole family has exited on its own or
/// something asks the watcher to stop early (Ctrl-C, `SIGTERM`) — at
/// which point every still-live worker is killed too (see `watch_worker`)
/// before this returns. Never returns an `Err` for "a worker's own run
/// failed" (that's the worker's own problem, reported on its own stdout/
/// exit code) — only for something that stops the watcher itself from
/// standing up at all (can't bind its socket, can't spawn the primary
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
    spawn_worker(&registry, &exe, &socket_path, canonical, port, initial_open, auto_exit)?;

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

    #[tokio::test]
    async fn registry_wait_until_empty_resolves_once_the_last_entry_is_removed() {
        let registry = Arc::new(Registry::new());
        registry.entries.lock().unwrap().insert(
            PathBuf::from("/tmp/a.canvas.md"),
            Entry { port: Some(1), pending_open: None },
        );
        assert!(!registry.is_empty());

        let wait_registry = Arc::clone(&registry);
        let wait_task = tokio::spawn(async move { wait_registry.wait_until_empty().await });

        // Give the waiter a moment to actually start waiting before we
        // remove the only entry.
        tokio::time::sleep(Duration::from_millis(20)).await;
        registry.remove(&PathBuf::from("/tmp/a.canvas.md"));

        tokio::time::timeout(Duration::from_secs(2), wait_task)
            .await
            .expect("wait_until_empty should resolve promptly")
            .unwrap();
    }

    #[tokio::test]
    async fn mark_ready_opens_a_pending_entry_and_clears_it() {
        let registry = Registry::new();
        registry.entries.lock().unwrap().insert(
            PathBuf::from("/tmp/a.canvas.md"),
            Entry { port: None, pending_open: Some(Some("some-node".to_string())) },
        );

        registry.mark_ready(Path::new("/tmp/a.canvas.md"), 4242);

        let entries = registry.entries.lock().unwrap();
        let entry = entries.get(Path::new("/tmp/a.canvas.md")).unwrap();
        assert_eq!(entry.port, Some(4242));
        assert_eq!(entry.pending_open, None);
    }

    #[tokio::test]
    async fn mark_ready_for_an_untracked_path_is_a_harmless_no_op() {
        let registry = Registry::new();
        registry.mark_ready(Path::new("/tmp/nope.canvas.md"), 4242);
        assert!(registry.is_empty());
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
            .insert(canonical.clone(), Entry { port: None, pending_open: None });

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
}
