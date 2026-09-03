//! Wire protocol between a `meshfox view` worker and whichever process is
//! coordinating it — the private, per-invocation watcher a top-level
//! `meshfox view <path>` spawns (see `crates/cli/src/watcher.rs`), or,
//! eventually, a persistent GUI daemon (a menu-bar app, quite possibly not
//! even Rust — see TODO.canvas.md's "Ссылки и навигация между
//! канвасами"). Deliberately a plain newline-delimited JSON message over a
//! *named* Unix socket (a path, not an inherited file descriptor/
//! `socketpair()`) precisely so any implementation, in any language, on
//! any platform (a Windows named pipe is the direct analog) can speak it —
//! a worker never needs to know or care whether the process on the other
//! end is this crate's own watcher or something else entirely.
//!
//! Three messages, all one-way — no response payload a caller needs to act
//! on, since opening a browser tab (or a plain file) is the *coordinator's*
//! job now, not something a caller waits for a port back to do itself (see
//! `crate::open_node_file`):
//! - [`Message::Ready`] — sent once by a freshly-spawned worker, right
//!   after it binds its listener. Replaces the old `--port-file` polling
//!   entirely: the coordinator just gets told, instead of having to notice.
//! - [`Message::Open`] — sent by a worker whenever its own `open_node_file`
//!   handler needs to show the user some *other* canvas (a "↗ open" click
//!   on a `.canvas.md` target) — get-or-spawn-and-show is entirely the
//!   coordinator's problem from here.
//! - [`Message::OpenFile`] — sent by that same handler for a "↗ open" on a
//!   plain (non-canvas) file node's target. Deliberately a separate variant
//!   from `Open` rather than an optional/reused field on it: a plain file
//!   has no fragment, no port, no spawn-and-wait lifecycle — just "hand
//!   this path to whatever this coordinator does with a file", which is
//!   exactly the hook that lets each coordinator implementation give it
//!   different behavior (the OS's default application for `crate::view`'s
//!   own watcher and the macOS menu-bar daemon, a fresh editor tab for the
//!   VS Code extension's coordinator) without the worker itself knowing or
//!   caring which one it's talking to.
//!
//! A connection failure (the coordinator's socket doesn't exist, or
//! nothing answers) is surfaced as a plain `io::Error` to the caller — see
//! each function's own doc comment for how its caller treats that.

use serde::{Deserialize, Serialize};
use std::io;
use std::path::{Path, PathBuf};
use tokio::io::AsyncWriteExt;
use tokio::net::UnixStream;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Message {
    /// A worker's own listener just bound `port` for `canvas_path`
    /// (canonicalized). Sent exactly once, right after `run`'s
    /// `TcpListener::bind` succeeds.
    Ready { canvas_path: PathBuf, port: u16 },
    /// "Show the user `canvas_path` (canonicalized) in a browser tab" —
    /// get-or-spawn-and-open, entirely the coordinator's decision. Sent by
    /// `open_node_file` for a `.canvas.md` (or marker-carrying `.md`)
    /// target. `fragment` is a deep link's own `#node-id` (from
    /// `[label](other.canvas.md#node-id)` — see
    /// `meshfox_core::mdcanvas::split_target_fragment`), appended to the
    /// URL the coordinator actually opens once the target worker's port is
    /// known; `None` opens the target's own root.
    Open {
        canvas_path: PathBuf,
        fragment: Option<String>,
    },
    /// "Open `path` — a plain file, not a canvas — however this
    /// coordinator opens plain files." Sent by `open_node_file` for a
    /// file-node target that isn't a `.canvas.md` (or marker-carrying
    /// `.md`). No fragment, no port to wait for: fire-and-forget, same as
    /// `Open` once a worker's already running.
    OpenFile { path: PathBuf },
}

/// Sends `msg` to the coordinator listening at `socket_path` as one
/// newline-delimited JSON line, then closes the connection — a fresh
/// short-lived connection per message rather than a kept-open one, so
/// neither side has to manage reconnects if the other restarts.
async fn send(socket_path: &Path, msg: &Message) -> io::Result<()> {
    let mut stream = UnixStream::connect(socket_path).await?;
    let mut line =
        serde_json::to_string(msg).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    line.push('\n');
    stream.write_all(line.as_bytes()).await?;
    stream.shutdown().await?;
    Ok(())
}

/// [`Message::Ready`] — called by `run` once its listener is bound. A
/// failure here (no watcher, or it's gone) is deliberately non-fatal to
/// the caller: a worker with nobody to report to still serves its own
/// canvas fine, it just won't get a browser tab auto-opened for it (and
/// its own future `request_open` calls will fail too, degrading cross-
/// canvas navigation only, not this worker's own operation) — see `run`'s
/// own call site for how it logs rather than propagates this.
pub async fn notify_ready(socket_path: &Path, canvas_path: &Path, port: u16) -> io::Result<()> {
    send(
        socket_path,
        &Message::Ready {
            canvas_path: canvas_path.to_path_buf(),
            port,
        },
    )
    .await
}

/// [`Message::Open`] — called by `open_node_file` for a canvas target. A
/// failure here (no watcher reachable) *is* meant to surface to the
/// caller as a real error — see `open_node_file`'s own handling — since
/// unlike `notify_ready`, this is the one thing cross-canvas navigation
/// actually depends on succeeding.
pub async fn request_open(
    socket_path: &Path,
    canvas_path: &Path,
    fragment: Option<String>,
) -> io::Result<()> {
    send(
        socket_path,
        &Message::Open {
            canvas_path: canvas_path.to_path_buf(),
            fragment,
        },
    )
    .await
}

/// [`Message::OpenFile`] — called by `open_node_file` for a plain-file
/// target. Same failure contract as `request_open`: no coordinator
/// reachable is a real error the caller surfaces, since without one there's
/// nobody left to open the file at all (see `open_node_file`'s own
/// handling of that case for both message kinds).
pub async fn request_open_file(socket_path: &Path, path: &Path) -> io::Result<()> {
    send(
        socket_path,
        &Message::OpenFile {
            path: path.to_path_buf(),
        },
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncReadExt;
    use tokio::net::UnixListener;

    /// Short and unique, not descriptive — a Unix domain socket path has a
    /// tight length budget (`SUN_LEN`, ~104 bytes on macOS/BSD), and
    /// `std::env::temp_dir()` alone can already eat half of that (macOS's
    /// `/var/folders/.../T/`). `std::process::id()` is enough uniqueness on
    /// its own for anything sharing a single test binary's process, so
    /// `name` just disambiguates the handful of sockets one test opens,
    /// not concurrent test runs.
    fn temp_socket_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("mfx-{name}-{}.sock", std::process::id()))
    }

    #[tokio::test]
    async fn notify_ready_sends_a_single_parseable_ready_line() {
        let socket_path = temp_socket_path("ready");
        let listener = UnixListener::bind(&socket_path).unwrap();

        let canvas_path = PathBuf::from("/tmp/some.canvas.md");
        let send_task = tokio::spawn({
            let socket_path = socket_path.clone();
            let canvas_path = canvas_path.clone();
            async move { notify_ready(&socket_path, &canvas_path, 4242).await }
        });

        let (mut stream, _) = listener.accept().await.unwrap();
        let mut buf = String::new();
        stream.read_to_string(&mut buf).await.unwrap();
        send_task.await.unwrap().unwrap();

        let msg: Message = serde_json::from_str(buf.trim()).unwrap();
        match msg {
            Message::Ready { canvas_path: p, port } => {
                assert_eq!(p, canvas_path);
                assert_eq!(port, 4242);
            }
            other => panic!("expected Ready, got {other:?}"),
        }

        let _ = std::fs::remove_file(&socket_path);
    }

    #[tokio::test]
    async fn request_open_sends_a_single_parseable_open_line() {
        let socket_path = temp_socket_path("open");
        let listener = UnixListener::bind(&socket_path).unwrap();

        let canvas_path = PathBuf::from("/tmp/other.canvas.md");
        let send_task = tokio::spawn({
            let socket_path = socket_path.clone();
            let canvas_path = canvas_path.clone();
            async move { request_open(&socket_path, &canvas_path, Some("some-node".to_string())).await }
        });

        let (mut stream, _) = listener.accept().await.unwrap();
        let mut buf = String::new();
        stream.read_to_string(&mut buf).await.unwrap();
        send_task.await.unwrap().unwrap();

        let msg: Message = serde_json::from_str(buf.trim()).unwrap();
        match msg {
            Message::Open { canvas_path: p, fragment } => {
                assert_eq!(p, canvas_path);
                assert_eq!(fragment.as_deref(), Some("some-node"));
            }
            other => panic!("expected Open, got {other:?}"),
        }

        let _ = std::fs::remove_file(&socket_path);
    }

    #[tokio::test]
    async fn request_open_file_sends_a_single_parseable_open_file_line() {
        let socket_path = temp_socket_path("open-file");
        let listener = UnixListener::bind(&socket_path).unwrap();

        let path = PathBuf::from("/tmp/some.txt");
        let send_task = tokio::spawn({
            let socket_path = socket_path.clone();
            let path = path.clone();
            async move { request_open_file(&socket_path, &path).await }
        });

        let (mut stream, _) = listener.accept().await.unwrap();
        let mut buf = String::new();
        stream.read_to_string(&mut buf).await.unwrap();
        send_task.await.unwrap().unwrap();

        let msg: Message = serde_json::from_str(buf.trim()).unwrap();
        match msg {
            Message::OpenFile { path: p } => assert_eq!(p, path),
            other => panic!("expected OpenFile, got {other:?}"),
        }

        let _ = std::fs::remove_file(&socket_path);
    }

    #[tokio::test]
    async fn request_open_fails_when_nothing_is_listening() {
        let socket_path = temp_socket_path("nobody-home");
        let err = request_open(&socket_path, &PathBuf::from("/tmp/x.canvas.md"), None)
            .await
            .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
    }
}
