//! Wire protocol between a `meshfox view` worker and whichever process is
//! coordinating it — the private, per-invocation watcher a top-level
//! `meshfox view <path>` spawns (see `crates/cli/src/watcher.rs`), or a
//! persistent GUI daemon (the macOS menu-bar app, `macos/MeshfoxDaemon`;
//! quite possibly not even Rust — see TODO.canvas.md's "Ссылки и навигация
//! между канвасами"). Deliberately a plain newline-delimited JSON message
//! over a *named* Unix socket (a path, not an inherited file descriptor/
//! `socketpair()`) precisely so any implementation, in any language, on
//! any platform (a Windows named pipe is the direct analog) can speak it —
//! a worker never needs to know or care whether the process on the other
//! end is this crate's own watcher or something else entirely.
//!
//! Four messages. One is genuinely one-way — no response payload a caller
//! needs to act on:
//! - [`Message::Ready`] — sent once by a freshly-spawned worker, right
//!   after it binds its listener. Replaces the old `--port-file` polling
//!   entirely: the coordinator just gets told, instead of having to notice.
//!
//! The other three all get a reply on the same connection — every
//! coordinator implementation that wants to be usable at all (not just
//! `server_socket` clients — see [`request_open`]'s own doc comment on why
//! this stopped being optional) needs to answer each of them:
//! - [`Message::Open`] — sent by a worker whenever its own `open_node_file`
//!   handler needs to show the user some *other* canvas (a "↗ open" click
//!   on a `.canvas.md` target), or by `meshfox view` itself handing a
//!   top-level invocation off to a configured coordinator instead of
//!   becoming its own watcher — get-or-spawn-and-show is entirely the
//!   coordinator's problem from here. Used to be fire-and-forget (a caller
//!   just trusted the coordinator to get on with it); a real, whole-file
//!   incident (`TODO.canvas.md`: a wedged coordinator accepted every
//!   connection into its own kernel backlog and reported success while
//!   never actually opening anything) is exactly why that trust turned out
//!   to be worth replacing with a real acknowledgement.
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
//! - [`Message::GetPort`] — sent not by a worker but by any *other* client
//!   (`tui`, `run`, `node <op>`, MCP — see `crates/cli/src/coordinator.rs`,
//!   and the VS Code extension) that wants to become an HTTP/WS client of
//!   whichever worker the coordinator manages for a canvas, without opening
//!   a browser tab for it. After get-or-spawning a worker exactly like
//!   `Open` would, the coordinator writes one JSON response line back on
//!   the *same* connection before closing it — `{"port": u16}` on success,
//!   `{"error": string}` on failure — see [`request_port`].
//!
//! `Open`/`OpenFile` share [`Message::Ready`]'s pre-existing "unreachable
//! coordinator" failure contract (a plain `io::Error`), just now also
//! covering "reachable, but it (or the worker it spawned) reported a real
//! failure" the exact same way `GetPort` already did — see
//! [`request_open`]/[`request_open_file`]'s own doc comments for the reply
//! shape ([`AckResponse`]) and timeout behavior.

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
    /// `.md`). No fragment, no port to wait for: get-or-spawn doesn't apply
    /// here, but (since `AckResponse`, below) the coordinator still
    /// confirms it actually got opened.
    OpenFile { path: PathBuf },
    /// "Get-or-spawn a worker for `canvas_path`, don't open a browser tab,
    /// just tell me its port" — see this module's own doc comment.
    GetPort { canvas_path: PathBuf },
}

/// The one JSON line a coordinator writes back after a [`Message::GetPort`]
/// — see [`request_port`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
enum PortResponse {
    Port { port: u16 },
    Error { error: String },
}

/// The one JSON line a coordinator writes back after a [`Message::Open`]/
/// [`Message::OpenFile`] — `{}` on success, `{"error": "..."}` on failure.
/// `pub` so `crates/cli/src/watcher.rs` (a coordinator implementation
/// itself, not just a client of one) can construct and serialize this
/// directly rather than duplicating the shape. `Error` is declared before
/// `Ok` deliberately: `#[serde(untagged)]` tries variants in declared
/// order, and an empty struct like `Ok` would otherwise happily deserialize
/// *any* object (including `{"error": "..."}`, since an unrecognized field
/// is just ignored) — trying `Error` first is what makes this actually
/// discriminate on the field's presence.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum AckResponse {
    Error { error: String },
    Ok {},
}

/// Sends `msg` to the coordinator listening at `socket_path` as one
/// newline-delimited JSON line, then closes the connection — used only by
/// [`notify_ready`], the one message nobody ever replies to. Every other
/// message here keeps the connection open afterward to read a reply — see
/// [`request_and_await_reply`].
async fn send(socket_path: &Path, msg: &Message) -> io::Result<()> {
    let mut stream = UnixStream::connect(socket_path).await?;
    let mut line =
        serde_json::to_string(msg).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    line.push('\n');
    stream.write_all(line.as_bytes()).await?;
    stream.shutdown().await?;
    Ok(())
}

/// Connects to `socket_path`, writes `msg` as one line, then reads exactly
/// one JSON reply line back — the shared mechanics every reply-expecting
/// message ([`request_port`], [`request_open`], [`request_open_file`])
/// builds on. Keeps the write half open afterward (unlike [`send`]) so a
/// reply can actually arrive: the Rust client here keeps reading rather
/// than shutting its own write side down waiting for one, exactly the
/// asymmetry `handleClient`'s own doc comment on the Swift side already
/// has to account for. A reply that never comes (connection closes with an
/// empty read) is `InvalidData`, same failure kind every caller here
/// already surfaced for that case before this was factored out.
async fn request_and_await_reply(socket_path: &Path, msg: &Message) -> io::Result<String> {
    use tokio::io::{AsyncBufReadExt, BufReader};

    let mut stream = UnixStream::connect(socket_path).await?;
    let mut line =
        serde_json::to_string(msg).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    line.push('\n');
    stream.write_all(line.as_bytes()).await?;

    let (read_half, _write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);
    let mut reply = String::new();
    reader.read_line(&mut reply).await?;
    if reply.trim().is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "coordinator closed the connection without answering",
        ));
    }
    Ok(reply)
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

/// How long [`request_port`]/[`request_open`]/[`request_open_file`] each
/// wait for a reply before giving up — a backstop for a coordinator that's
/// wedged rather than just slow (a real worker spawn is fast; this is
/// generous specifically so it never fires under normal load). Longer than
/// the macOS daemon's own `SessionStore.getPortTimeoutSeconds`/
/// `openTimeoutSeconds` (15s each) so that side's own timeout-and-kill-the-
/// worker path is what normally answers first — this is only reached if
/// the coordinator itself never gets to run that logic at all (the
/// launchd-socket-backlog incident this pair of timeouts was added for: a
/// completely wedged accept loop, not merely a slow worker — see
/// `UnixSocketServer.swift`'s own fix for the specific `accept()` failure
/// that incident turned out to be).
const COORDINATOR_REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// [`Message::GetPort`] — get-or-spawn a worker for `canvas_path` and learn
/// its port, without opening a browser tab. A coordinator-reported
/// `{"error": ...}` comes back as `io::ErrorKind::Other`; a connection
/// failure or a malformed/missing reply as `io::ErrorKind::InvalidData`; no
/// reply at all within [`COORDINATOR_REQUEST_TIMEOUT`] as
/// `io::ErrorKind::TimedOut` — a coordinator that's completely wedged (not
/// just slow) shouldn't be able to hang every client that ever asks it for
/// a port forever. Used by `crates/cli/src/coordinator.rs` whenever
/// `server_socket` is configured — see that module for why every other
/// core-launch operation (`tui`, `run`, `node <op>`, MCP) goes through this
/// instead of `worker_lock`.
pub async fn request_port(socket_path: &Path, canvas_path: &Path) -> io::Result<u16> {
    request_port_with_timeout(socket_path, canvas_path, COORDINATOR_REQUEST_TIMEOUT).await
}

/// `request_port`'s own implementation, taking the timeout explicitly so a
/// test can use a short one instead of waiting out the real
/// [`COORDINATOR_REQUEST_TIMEOUT`].
async fn request_port_with_timeout(
    socket_path: &Path,
    canvas_path: &Path,
    timeout: std::time::Duration,
) -> io::Result<u16> {
    match tokio::time::timeout(timeout, request_port_inner(socket_path, canvas_path)).await {
        Ok(result) => result,
        Err(_) => Err(io::Error::new(
            io::ErrorKind::TimedOut,
            format!(
                "coordinator at {} didn't answer get_port within {}s",
                socket_path.display(),
                timeout.as_secs_f64()
            ),
        )),
    }
}

async fn request_port_inner(socket_path: &Path, canvas_path: &Path) -> io::Result<u16> {
    let msg = Message::GetPort {
        canvas_path: canvas_path.to_path_buf(),
    };
    let reply = request_and_await_reply(socket_path, &msg).await?;
    match serde_json::from_str::<PortResponse>(reply.trim()) {
        Ok(PortResponse::Port { port }) => Ok(port),
        Ok(PortResponse::Error { error }) => Err(io::Error::other(error)),
        Err(e) => Err(io::Error::new(io::ErrorKind::InvalidData, e)),
    }
}

fn parse_ack(reply: &str) -> io::Result<()> {
    match serde_json::from_str::<AckResponse>(reply.trim()) {
        Ok(AckResponse::Ok {}) => Ok(()),
        Ok(AckResponse::Error { error }) => Err(io::Error::other(error)),
        Err(e) => Err(io::Error::new(io::ErrorKind::InvalidData, e)),
    }
}

/// [`Message::Open`] — called by `open_node_file` for a canvas target, and
/// by `meshfox view` itself (`crate::coordinator::hand_off_to_configured_coordinator`
/// on the CLI side) when handing a top-level invocation off to a configured
/// coordinator. A connection failure (no coordinator reachable at all) is a
/// real error, same as it always was; what's new is that a *reachable*
/// coordinator that failed to actually open anything (the worker it spawned
/// crashed — a bad canvas file, say — or its own accept loop is wedged and
/// never even got to try) now surfaces the same way instead of this
/// resolving successfully regardless. See [`AckResponse`] for the reply
/// shape and [`COORDINATOR_REQUEST_TIMEOUT`] for how long this waits.
pub async fn request_open(
    socket_path: &Path,
    canvas_path: &Path,
    fragment: Option<String>,
) -> io::Result<()> {
    request_open_with_timeout(socket_path, canvas_path, fragment, COORDINATOR_REQUEST_TIMEOUT).await
}

async fn request_open_with_timeout(
    socket_path: &Path,
    canvas_path: &Path,
    fragment: Option<String>,
    timeout: std::time::Duration,
) -> io::Result<()> {
    let msg = Message::Open {
        canvas_path: canvas_path.to_path_buf(),
        fragment,
    };
    match tokio::time::timeout(timeout, request_and_await_reply(socket_path, &msg)).await {
        Ok(reply) => parse_ack(&reply?),
        Err(_) => Err(io::Error::new(
            io::ErrorKind::TimedOut,
            format!(
                "coordinator at {} didn't answer open within {}s",
                socket_path.display(),
                timeout.as_secs_f64()
            ),
        )),
    }
}

/// [`Message::OpenFile`] — called by `open_node_file` for a plain-file
/// target. Same failure contract as [`request_open`] now (used to be the
/// same fire-and-forget contract [`notify_ready`] still has — see this
/// module's own doc comment for why that changed).
pub async fn request_open_file(socket_path: &Path, path: &Path) -> io::Result<()> {
    request_open_file_with_timeout(socket_path, path, COORDINATOR_REQUEST_TIMEOUT).await
}

async fn request_open_file_with_timeout(
    socket_path: &Path,
    path: &Path,
    timeout: std::time::Duration,
) -> io::Result<()> {
    let msg = Message::OpenFile {
        path: path.to_path_buf(),
    };
    match tokio::time::timeout(timeout, request_and_await_reply(socket_path, &msg)).await {
        Ok(reply) => parse_ack(&reply?),
        Err(_) => Err(io::Error::new(
            io::ErrorKind::TimedOut,
            format!(
                "coordinator at {} didn't answer open_file within {}s",
                socket_path.display(),
                timeout.as_secs_f64()
            ),
        )),
    }
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

    /// Reads back a request line and writes `reply` on the same connection
    /// — the shared test-side stand-in for a coordinator answering any of
    /// the three reply-expecting messages.
    async fn accept_and_reply(listener: &UnixListener, reply: &str) -> Message {
        use tokio::io::{AsyncBufReadExt, BufReader};
        let (stream, _) = listener.accept().await.unwrap();
        let (read_half, mut write_half) = stream.into_split();
        let mut reader = BufReader::new(read_half);
        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();
        let msg: Message = serde_json::from_str(line.trim()).unwrap();
        write_half.write_all(reply.as_bytes()).await.unwrap();
        msg
    }

    #[tokio::test]
    async fn request_open_sends_a_single_parseable_open_line_and_reads_back_ok() {
        let socket_path = temp_socket_path("open");
        let listener = UnixListener::bind(&socket_path).unwrap();

        let canvas_path = PathBuf::from("/tmp/other.canvas.md");
        let send_task = tokio::spawn({
            let socket_path = socket_path.clone();
            let canvas_path = canvas_path.clone();
            async move { request_open(&socket_path, &canvas_path, Some("some-node".to_string())).await }
        });

        let msg = accept_and_reply(&listener, "{}\n").await;
        send_task.await.unwrap().unwrap();

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
    async fn request_open_surfaces_a_coordinator_reported_error() {
        let socket_path = temp_socket_path("open-error");
        let listener = UnixListener::bind(&socket_path).unwrap();

        let canvas_path = PathBuf::from("/tmp/bad.canvas.md");
        let send_task = tokio::spawn({
            let socket_path = socket_path.clone();
            let canvas_path = canvas_path.clone();
            async move { request_open(&socket_path, &canvas_path, None).await }
        });

        accept_and_reply(&listener, "{\"error\":\"file node \\\"code-uv\\\" ...\"}\n").await;
        let err = send_task.await.unwrap().unwrap_err();
        assert!(err.to_string().contains("code-uv"));

        let _ = std::fs::remove_file(&socket_path);
    }

    #[tokio::test]
    async fn request_open_file_sends_a_single_parseable_open_file_line_and_reads_back_ok() {
        let socket_path = temp_socket_path("open-file");
        let listener = UnixListener::bind(&socket_path).unwrap();

        let path = PathBuf::from("/tmp/some.txt");
        let send_task = tokio::spawn({
            let socket_path = socket_path.clone();
            let path = path.clone();
            async move { request_open_file(&socket_path, &path).await }
        });

        let msg = accept_and_reply(&listener, "{}\n").await;
        send_task.await.unwrap().unwrap();

        match msg {
            Message::OpenFile { path: p } => assert_eq!(p, path),
            other => panic!("expected OpenFile, got {other:?}"),
        }

        let _ = std::fs::remove_file(&socket_path);
    }

    #[tokio::test]
    async fn request_port_reads_back_the_coordinators_reply() {
        let socket_path = temp_socket_path("get-port");
        let listener = UnixListener::bind(&socket_path).unwrap();

        let canvas_path = PathBuf::from("/tmp/some.canvas.md");
        let send_task = tokio::spawn({
            let socket_path = socket_path.clone();
            let canvas_path = canvas_path.clone();
            async move { request_port(&socket_path, &canvas_path).await }
        });

        let msg = accept_and_reply(&listener, "{\"port\":4242}\n").await;
        match msg {
            Message::GetPort { canvas_path: p } => assert_eq!(p, canvas_path),
            other => panic!("expected GetPort, got {other:?}"),
        }

        assert_eq!(send_task.await.unwrap().unwrap(), 4242);
        let _ = std::fs::remove_file(&socket_path);
    }

    #[tokio::test]
    async fn request_port_surfaces_a_coordinator_reported_error() {
        let socket_path = temp_socket_path("get-port-error");
        let listener = UnixListener::bind(&socket_path).unwrap();

        let canvas_path = PathBuf::from("/tmp/bad.canvas.md");
        let send_task = tokio::spawn({
            let socket_path = socket_path.clone();
            let canvas_path = canvas_path.clone();
            async move { request_port(&socket_path, &canvas_path).await }
        });

        accept_and_reply(&listener, "{\"error\":\"couldn't spawn a worker\"}\n").await;
        let err = send_task.await.unwrap().unwrap_err();
        assert!(err.to_string().contains("couldn't spawn a worker"));
        let _ = std::fs::remove_file(&socket_path);
    }

    /// The client-side backstop from the launchd-socket-backlog incident:
    /// a coordinator that accepts the connection but then never answers at
    /// all (as opposed to replying with `{"error": ...}`) shouldn't hang
    /// this forever — see `COORDINATOR_REQUEST_TIMEOUT`'s own doc comment
    /// for why this exists *in addition to* the daemon's own timeout.
    #[tokio::test]
    async fn request_port_times_out_when_the_coordinator_never_replies() {
        let socket_path = temp_socket_path("get-port-never-replies");
        let listener = UnixListener::bind(&socket_path).unwrap();

        let canvas_path = PathBuf::from("/tmp/wedged.canvas.md");
        let send_task = tokio::spawn({
            let socket_path = socket_path.clone();
            let canvas_path = canvas_path.clone();
            async move {
                request_port_with_timeout(&socket_path, &canvas_path, std::time::Duration::from_millis(200)).await
            }
        });

        // Accept the connection (so the client's own `connect()` and
        // `write_all` both succeed) and just hold it open, never writing a
        // reply — exactly what a coordinator wedged before ever reaching
        // its own reply logic looks like from a client's perspective.
        let (_stream, _) = listener.accept().await.unwrap();

        let err = send_task.await.unwrap().unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::TimedOut);
        let _ = std::fs::remove_file(&socket_path);
    }

    /// Same backstop, for `request_open` — the whole reason `Open` gained a
    /// reply at all (see this module's own doc comment).
    #[tokio::test]
    async fn request_open_times_out_when_the_coordinator_never_replies() {
        let socket_path = temp_socket_path("open-never-replies");
        let listener = UnixListener::bind(&socket_path).unwrap();

        let canvas_path = PathBuf::from("/tmp/wedged.canvas.md");
        let send_task = tokio::spawn({
            let socket_path = socket_path.clone();
            let canvas_path = canvas_path.clone();
            async move {
                request_open_with_timeout(&socket_path, &canvas_path, None, std::time::Duration::from_millis(200))
                    .await
            }
        });

        let (_stream, _) = listener.accept().await.unwrap();

        let err = send_task.await.unwrap().unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::TimedOut);
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
