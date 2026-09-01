//! The "open a canvas from another canvas" navigation daemon (see
//! TODO.canvas.md's "Ссылки и навигация между канвасами"). When the web
//! UI's "↗ open" button (or the TUI's `o`) targets another `.canvas.md`
//! file, that's not something to hand to the OS's default-application
//! opener (there's no "open a canvas" association to hand off to) —
//! instead it needs its own `meshfox view` server, reused across repeat
//! clicks on the same target and independent of whichever canvas' own
//! server the click came from (closing the README that linked to it
//! shouldn't kill the detailed canvas someone opened from it).
//!
//! This module is the client half (`get_or_spawn`, called from
//! `meshfox-server`'s `open_node_file` handler and the TUI) and the daemon
//! half (`serve`, run by the CLI's hidden `view-registry-serve`
//! subcommand) of a tiny flat registry that bridges the two: one
//! long-lived, self-detaching daemon process holding `canonical path ->
//! spawned worker` (`meshfox view <path> --port 0 --no-open --port-file
//! <tmp>` — plain `Child`, not itself tracked by anything but this map),
//! reachable over a Unix socket. No health/`whoami` endpoint is needed to
//! tell a stale entry from a live one — the daemon holds each worker's
//! `Child` directly, so `try_wait()` answers that outright.
//!
//! Deliberately synchronous/thread-per-connection rather than async: this
//! is the only thing in `meshfox-core` that would need a `tokio`
//! dependency, for a handful of infrequent, tiny requests — not worth
//! pulling the runtime into a crate that otherwise has none.
//!
//! Unix-only for now (`std::os::unix::net`, `libc::daemon` for detaching)
//! — a Windows equivalent (a named pipe, `DETACHED_PROCESS`/
//! `CREATE_NO_WINDOW` process-creation flags instead of `daemon()`) is a
//! separate, not-yet-scheduled follow-up; `get_or_spawn` returns a plain
//! "unsupported" error on non-unix so callers (the "↗ open" handler, the
//! TUI) have one place to fall back to the old direct-open behavior.

use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// How long the daemon stays up with an empty registry before exiting on
/// its own — same shape (and same duration) as `crates/cli/src/mcp.rs`'s
/// own `IDLE_TIMEOUT`/`spawn_idle_sweep` for the multi-canvas MCP root
/// process, which this mirrors closely: a lazily-started, self-reaping
/// background process nobody has to remember to stop by hand.
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// `~/.meshfox/view-registry.sock` — global (not per-project, unlike
/// `varcache::cache_path`), since the whole point is reuse across whatever
/// canvases happen to be open at once, regardless of which project each
/// belongs to. `None` only when `HOME` itself isn't set (unusual enough
/// that callers just treat it as "registry unavailable").
pub fn socket_path() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|home| Path::new(&home).join(".meshfox").join("view-registry.sock"))
}

#[cfg(unix)]
mod unix {
    use super::*;
    use std::collections::HashMap;
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::process::{Child, Command, Stdio};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Instant;

    /// The registry's tiny wire protocol — one request/response pair,
    /// newline-delimited JSON over the Unix socket (see `socket_path`).
    /// Not a `#[cfg(not(unix))]` concern: nothing on that side ever
    /// constructs one.
    #[derive(Debug, serde::Serialize, serde::Deserialize)]
    #[serde(tag = "op", rename_all = "snake_case")]
    enum Request {
        GetOrSpawn { path: String },
    }

    #[derive(Debug, serde::Serialize, serde::Deserialize)]
    #[serde(untagged)]
    enum Response {
        Ok { port: u16 },
        Err { error: String },
    }

    /// Asks the registry daemon for a worker serving `target_path`,
    /// starting the daemon first (if nothing answers the socket yet) and
    /// spawning the worker itself (if the daemon has no live one for this
    /// path yet) as needed. Returns the worker's bound port. `target_path`
    /// is canonicalized before being used as the registry key, so two
    /// different (e.g. relative vs. `..`-laden) spellings of the same file
    /// share one worker.
    pub fn get_or_spawn(target_path: &Path) -> io::Result<u16> {
        let socket = super::socket_path().ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotFound, "HOME is not set — no view-registry socket path")
        })?;
        let canonical = target_path.canonicalize()?;
        let exe = std::env::current_exe()?;
        let mut stream = connect_or_bootstrap(&exe, &socket)?;

        let req = Request::GetOrSpawn {
            path: canonical.to_string_lossy().into_owned(),
        };
        let mut line = serde_json::to_string(&req)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        line.push('\n');
        stream.write_all(line.as_bytes())?;

        let mut reply = String::new();
        BufReader::new(stream).read_line(&mut reply)?;
        match serde_json::from_str::<Response>(reply.trim())
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?
        {
            Response::Ok { port } => Ok(port),
            Response::Err { error } => Err(io::Error::other(error)),
        }
    }

    /// Connects to an already-running daemon, or — if nothing's listening
    /// yet — races to become the one that starts it. Losing that race is
    /// harmless: `UnixListener::bind` on a path someone else is about to
    /// (or already did) bind fails with `AddrInUse`, so a loser just backs
    /// off and retries the plain connect instead. The daemon process
    /// itself has the same fallback on the *real* bind it does once
    /// running (see `serve` below) — the socket file this function briefly
    /// binds-then-drops to claim the "spawn a daemon" role is not the same
    /// bind the daemon serves on, so at most a couple of harmless
    /// short-lived daemon processes race to serve and all but one exit
    /// immediately once they lose that second bind.
    fn connect_or_bootstrap(exe: &Path, socket: &Path) -> io::Result<UnixStream> {
        if let Ok(s) = UnixStream::connect(socket) {
            return Ok(s);
        }
        if let Some(parent) = socket.parent() {
            std::fs::create_dir_all(parent)?;
        }
        for attempt in 0..40u64 {
            if let Ok(s) = UnixStream::connect(socket) {
                return Ok(s);
            }
            // Nobody answered — try to claim the "I'll start the daemon"
            // role. A stale socket file left behind by a crashed daemon
            // has no listener behind it, so it's cleared first; if another
            // process is doing this exact thing at the exact same moment,
            // one of the two `remove_file`+`bind` sequences below just
            // loses to the other's — see the doc comment above.
            let _ = std::fs::remove_file(socket);
            if let Ok(claim) = UnixListener::bind(socket) {
                drop(claim); // release the path — `serve`'s own real bind reclaims it
                spawn_daemon(exe)?;
            }
            std::thread::sleep(Duration::from_millis(50 + attempt * 20));
        }
        UnixStream::connect(socket)
    }

    /// Spawns `exe view-registry-serve` and forgets about it — it detaches
    /// itself (`libc::daemon`, called at the top of `serve`) almost
    /// immediately, so the direct child this creates exits fast; a
    /// background reaper thread just cleans up that short-lived zombie
    /// without making the caller wait for it.
    fn spawn_daemon(exe: &Path) -> io::Result<()> {
        let mut child = Command::new(exe)
            .arg("view-registry-serve")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        std::thread::spawn(move || {
            let _ = child.wait();
        });
        Ok(())
    }

    /// A registry-tracked worker: a plain `meshfox view` child process and
    /// the port `run`'s `--port-file` reported it bound.
    struct Worker {
        child: Child,
        port: u16,
    }

    /// Runs the daemon's accept loop until it decides to exit on its own
    /// (idle timeout) — never returns otherwise. Binds the *real* socket
    /// (distinct from `connect_or_bootstrap`'s claim-then-drop bind above);
    /// losing that bind to another daemon that got there first isn't an
    /// error, just nothing left for this process to do.
    pub fn serve() -> io::Result<()> {
        let socket = super::socket_path()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "HOME is not set"))?;
        let _ = std::fs::remove_file(&socket);
        let listener = match UnixListener::bind(&socket) {
            Ok(l) => l,
            Err(e) if e.kind() == io::ErrorKind::AddrInUse => return Ok(()),
            Err(e) => return Err(e),
        };

        // Detach from whatever spawned us (a `view` process that should
        // stay free to exit on its own, e.g. its last browser tab
        // closing, without taking this registry down with it) — forks,
        // makes the child a new session leader, and redirects its stdio
        // to `/dev/null`. The listener's fd survives the fork untouched.
        // SAFETY: called once, before any other threads exist in this
        // process (no worker/sweep threads have been spawned yet).
        // `daemon(3)` is deprecated on macOS (10.5+) in favor of
        // launchd-managed services, but there's no replacement for "just
        // detach this ad-hoc process" outside of hand-rolling the same
        // fork/setsid/reopen-stdio sequence — it still works.
        #[allow(deprecated)]
        unsafe {
            libc::daemon(1, 0);
        }

        let exe = std::env::current_exe()?;
        let workers: Arc<Mutex<HashMap<PathBuf, Worker>>> = Arc::new(Mutex::new(HashMap::new()));
        let last_activity = Arc::new(Mutex::new(Instant::now()));
        spawn_idle_sweep(Arc::clone(&workers), Arc::clone(&last_activity));

        let port_file_seq = AtomicU64::new(0);
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            *last_activity.lock().unwrap() = Instant::now();
            let workers = Arc::clone(&workers);
            let exe = exe.clone();
            let seq = port_file_seq.fetch_add(1, Ordering::Relaxed);
            std::thread::spawn(move || handle_connection(stream, &workers, &exe, seq));
        }
        Ok(())
    }

    fn handle_connection(
        stream: UnixStream,
        workers: &Mutex<HashMap<PathBuf, Worker>>,
        exe: &Path,
        seq: u64,
    ) {
        let mut reader = BufReader::new(stream.try_clone().expect("clone unix stream"));
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            return;
        }
        let response = match serde_json::from_str::<Request>(line.trim()) {
            Ok(Request::GetOrSpawn { path }) => match get_or_spawn_worker(workers, exe, &path, seq) {
                Ok(port) => Response::Ok { port },
                Err(e) => Response::Err { error: e.to_string() },
            },
            Err(e) => Response::Err {
                error: format!("bad request: {e}"),
            },
        };
        let mut out = serde_json::to_string(&response).unwrap_or_else(|_| {
            r#"{"error":"failed to encode response"}"#.to_string()
        });
        out.push('\n');
        let _ = (&stream).write_all(out.as_bytes());
    }

    fn get_or_spawn_worker(
        workers: &Mutex<HashMap<PathBuf, Worker>>,
        exe: &Path,
        path: &str,
        seq: u64,
    ) -> io::Result<u16> {
        let canonical = PathBuf::from(path);
        let mut workers = workers.lock().unwrap();
        if let Some(worker) = workers.get_mut(&canonical) {
            if matches!(worker.child.try_wait(), Ok(None)) {
                return Ok(worker.port);
            }
            workers.remove(&canonical);
        }

        let port_file = std::env::temp_dir().join(format!(
            "meshfox-view-registry-port-{}-{seq}",
            std::process::id()
        ));
        let child = Command::new(exe)
            .arg("view")
            .arg(&canonical)
            .arg("--port")
            .arg("0")
            .arg("--no-open")
            .arg("--port-file")
            .arg(&port_file)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;

        let port = wait_for_port_file(&port_file, Duration::from_secs(10));
        let _ = std::fs::remove_file(&port_file);
        let mut child = child;
        let port = match port {
            Some(port) => port,
            None => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!("worker for {path} never reported its port"),
                ));
            }
        };

        workers.insert(canonical, Worker { child, port });
        Ok(port)
    }

    /// Polls for `port_file` to show up with parseable contents — written
    /// by the worker's own `run` right after it binds its listener (see
    /// `crates/server/src/lib.rs::run`'s `port_file` handling). A plain
    /// poll rather than e.g. `inotify`: this fires once per newly-spawned
    /// worker, not on any hot path, and the file is expected within
    /// milliseconds.
    fn wait_for_port_file(path: &Path, timeout: Duration) -> Option<u16> {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if let Ok(contents) = std::fs::read_to_string(path) {
                if let Ok(port) = contents.trim().parse() {
                    return Some(port);
                }
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        None
    }

    fn spawn_idle_sweep(
        workers: Arc<Mutex<HashMap<PathBuf, Worker>>>,
        last_activity: Arc<Mutex<Instant>>,
    ) {
        std::thread::spawn(move || loop {
            std::thread::sleep(Duration::from_secs(60));
            let mut workers_guard = workers.lock().unwrap();
            workers_guard.retain(|_, w| matches!(w.child.try_wait(), Ok(None)));
            let empty = workers_guard.is_empty();
            drop(workers_guard);
            if empty && last_activity.lock().unwrap().elapsed() > super::IDLE_TIMEOUT {
                std::process::exit(0);
            }
        });
    }
}

#[cfg(unix)]
pub use unix::{get_or_spawn, serve};

#[cfg(not(unix))]
pub fn get_or_spawn(_target_path: &Path) -> io::Result<u16> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "cross-canvas navigation isn't implemented on this platform yet",
    ))
}

#[cfg(not(unix))]
pub fn serve() -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "meshfox view-registry-serve isn't implemented on this platform yet",
    ))
}
