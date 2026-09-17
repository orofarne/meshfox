//! Resolves "who is *the* worker for this canvas" for every core-launch
//! operation (`view`, `tui`, `node <op>`, `run`, MCP `debug_*`) — one
//! function every one of them calls instead of reaching for
//! `meshfox_core::worker_lock`/`crate::worker_client::discover` directly.
//!
//! Two independent sources feed the same decision, tried in order:
//!
//! 1. [`meshfox_core::config::server_socket`] — an external, persistent
//!    coordinator (the macOS menu-bar daemon today; a future Linux/VS Code
//!    equivalent later — anything speaking `meshfox_server::watcher_protocol`)
//!    that this machine/project has been pointed at. When set, this is
//!    always used and never falls back to spawning locally — see
//!    `resolve`'s own doc comment for why a dead socket is a real error, not
//!    a silent local spawn.
//! 2. [`meshfox_core::worker_lock`] — the local, per-file `flock` a `view`/
//!    `tui` session already holds while it's running. This is what let
//!    `node body`/`node rm` join an already-open `view`/`tui` instead of
//!    racing it before this module existed; every other operation now gets
//!    the same treatment, unconditionally, not just when `server_socket`
//!    happens to be set.
//!
//! Net effect: `server_socket` is *one more way to find* a worker to become
//! a client of, layered on top of local discovery that already existed —
//! not a separate mechanism a caller has to special-case.

use meshfox_core::worker_lock::{self, Acquired, LockGuard};
use meshfox_server::watcher_protocol;
use std::io;
use std::path::Path;

/// Outcome of [`resolve`] — mirrors [`worker_lock::Acquired`] exactly
/// (same two cases, same caller contract: `Us` means bind/spawn and keep
/// the guard alive for the worker's whole lifetime, `Other` means become an
/// HTTP/WS client of `port` and never bind anything). Kept as a distinct
/// type rather than reusing `Acquired` itself only because `Us` can now
/// arise from a caller that never even touched `worker_lock` (a
/// `server_socket` client never returns `Us` at all — see `resolve`).
pub enum Resolved {
    /// No live worker anywhere (no `server_socket` configured, and no
    /// local `view`/`tui` session holds this file's lock) — caller becomes
    /// the worker itself.
    Us(LockGuard),
    /// A worker already exists — either a local sibling `view`/`tui`
    /// session, or one an external coordinator just get-or-spawned. Caller
    /// should not bind/spawn anything of its own, just talk HTTP/WS to this
    /// port.
    Other(u16),
}

/// Resolves a worker for `canvas_path`, trying `server_socket` first, then
/// local `flock` discovery — see this module's own doc comment for why in
/// that order and why both exist. A `server_socket` that's configured but
/// unreachable is a real, propagated error (never a silent fallback to
/// local discovery, matching the "no silent fallback to isolated embedded
/// mode" rule `TODO.canvas.md`'s own coordinator discussion settled on) —
/// the caller asked for an external coordinator specifically, so treating
/// it as absent instead would silently give a different guarantee (no
/// longer sharing state with whatever else that coordinator manages) than
/// what was configured.
pub async fn resolve(canvas_path: &Path) -> io::Result<Resolved> {
    let canvas_root = crate::canvas_root_dir(canvas_path);
    if let Some(socket) = meshfox_core::config::server_socket(canvas_root) {
        // Canonicalize before crossing the socket: the coordinator resolves
        // a relative path against *its own* cwd, not this process's — a
        // bare `meshfox run doc.canvas.md ...` from some other directory
        // would otherwise silently ask the daemon to open a same-named file
        // wherever it happens to have been launched from (confirmed live
        // against a real daemon process: "No such file or directory" from
        // the worker it spawned, for exactly this reason).
        let canonical = canvas_path
            .canonicalize()
            .map_err(|e| io::Error::new(e.kind(), format!("{}: {e}", canvas_path.display())))?;
        let port = watcher_protocol::request_port(&socket, &canonical).await?;
        return Ok(Resolved::Other(port));
    }
    match worker_lock::try_acquire(canvas_path)? {
        Acquired::Us(guard) => Ok(Resolved::Us(guard)),
        Acquired::Other { port } => Ok(Resolved::Other(port)),
    }
}

/// Hands `canvas_path` off to the `server_socket`-configured coordinator
/// instead of `view` becoming its own watcher — the replacement for the
/// now-removed `meshfox open` command, folded into `view`'s own top-level
/// dispatch (see `crate::main`'s `Command::View` arm).
///
/// No "find and launch the app if it's not running" fallback here (an
/// earlier version of this had one, mirroring the old `meshfox open`'s
/// `resolve_daemon_app` + spawn + poll-retry) — the coordinator's own
/// LaunchAgent now uses launchd socket activation (`macos/app.canvas.md`'s
/// `build` node registers it, `UnixSocketServer.swift` inherits the fd via
/// `launch_activate_socket`), so the well-known socket is *always* live
/// from the moment that LaunchAgent is loaded, whether or not the daemon
/// process happens to be running at this exact instant: launchd itself
/// spawns/wakes it on first connection. A [`watcher_protocol::request_open`]
/// failure here therefore means the LaunchAgent isn't installed at all
/// (not "temporarily not started") — a real, actionable error pointing at
/// `macos/app.canvas.md`, never a silent fallback to `view`'s own private
/// watcher (that would defeat the entire point of configuring
/// `server_socket` in the first place).
///
/// Returns `Ok(None)` if no `server_socket` is configured at all (the
/// common case — caller should fall through to `view`'s own watcher/worker
/// behavior unchanged), `Ok(Some(()))` once handed off successfully, `Err`
/// for a configured-but-unreachable coordinator.
pub async fn hand_off_to_configured_coordinator(
    canvas_path: &Path,
    fragment: Option<String>,
) -> Result<Option<()>, String> {
    let canvas_root = crate::canvas_root_dir(canvas_path);
    let Some(socket) = meshfox_core::config::server_socket(canvas_root) else {
        return Ok(None);
    };
    let canonical = canvas_path
        .canonicalize()
        .map_err(|e| format!("{}: {e}", canvas_path.display()))?;

    watcher_protocol::request_open(&socket, &canonical, fragment)
        .await
        .map(Some)
        .map_err(|e| {
            format!(
                "couldn't reach the coordinator at {}: {e} — is its LaunchAgent installed? \
                 see macos/app.canvas.md's \"Build & install\"",
                socket.display()
            )
        })
}

/// `resolve`'s own `Other(port)`-only view — the direct replacement for
/// `crate::worker_client::discover`'s old signature/contract: `Some(port)`
/// to route through, `None` (no live worker anywhere, *or* the lock itself
/// couldn't be read) to fall back to direct-file editing. Drops a winning
/// `Us` guard immediately, same non-blocking-peek posture `discover` always
/// had — this never becomes the worker itself, even momentarily.
///
/// Unlike `resolve`, a configured-but-unreachable `server_socket` collapses
/// to `None` here rather than propagating an error: every caller of this
/// function already has a working direct-file fallback for "no worker"
/// (that's the whole point of the ops it gates — `node body`/`node rm`),
/// so silently taking that fallback is strictly better than hard-failing a
/// plain file edit because a daemon happens to be down.
pub async fn discover(canvas_path: &Path) -> Option<u16> {
    match resolve(canvas_path).await {
        Ok(Resolved::Other(port)) => Some(port),
        Ok(Resolved::Us(_guard)) => None,
        Err(_) => None,
    }
}
