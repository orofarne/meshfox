//! `meshfox serve` — a persistent coordinator: the cross-platform Rust
//! counterpart of the macOS menu-bar daemon (`macos/MeshfoxDaemon`), speaking
//! the same `meshfox_server::watcher_protocol` on the socket `server_socket`
//! points at. Headless: no window, no menu; `meshfox cores list|open|kill`
//! is its control surface. The registry and request handling are
//! `crate::watcher`'s own, in its persistent mode (it also answers
//! `ListCores` and `Kill`, and never exits just because it went idle).
//!
//! The listening socket comes from, in order:
//! 1. systemd socket activation (`LISTEN_PID`/`LISTEN_FDS`, fd 3) — the
//!    `systemd --user` setup; systemd owns the socket, so it survives this
//!    process exiting and is never unlinked here;
//! 2. `--socket PATH`, or `server_socket` from the config — bound here, with
//!    mode `0600` (the socket permissions are the whole access control,
//!    `Kill` included), and unlinked again on exit.

use std::io;
use std::os::fd::FromRawFd;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use tokio::net::{UnixListener, UnixStream};

use crate::watcher::{accept_loop, wait_for_terminate, Registry};

/// First file descriptor systemd passes (`SD_LISTEN_FDS_START`).
const SD_LISTEN_FDS_START: i32 = 3;

enum Acquired {
    /// Handed over by systemd; never unlinked.
    Inherited(UnixListener),
    /// Bound by us at the path; unlinked on exit.
    Bound(UnixListener, PathBuf),
}

pub async fn run(exe: PathBuf, socket: Option<PathBuf>) -> io::Result<()> {
    let (listener, socket_path, owned) = match acquire(socket).await? {
        Acquired::Inherited(listener) => {
            let path = listener
                .local_addr()?
                .as_pathname()
                .map(Path::to_path_buf)
                .ok_or_else(|| {
                    io::Error::other(
                        "the socket handed over by systemd has no filesystem path \
                         (workers need one to report back to); use ListenStream=<path>",
                    )
                })?;
            (listener, path, false)
        }
        Acquired::Bound(listener, path) => (listener, path, true),
    };

    let registry = Arc::new(Registry::new_persistent());
    let accept_task = tokio::spawn(accept_loop(
        listener,
        Arc::clone(&registry),
        exe,
        socket_path.clone(),
    ));

    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = wait_for_terminate() => {}
    }

    registry.signal_shutdown();
    // Best-effort: give every worker task a moment to kill and reap its child.
    let _ = tokio::time::timeout(Duration::from_secs(5), registry.wait_until_empty()).await;
    accept_task.abort();
    if owned {
        let _ = std::fs::remove_file(&socket_path);
    }
    Ok(())
}

async fn acquire(socket: Option<PathBuf>) -> io::Result<Acquired> {
    if let Some(listener) = inherited_listener()? {
        return Ok(Acquired::Inherited(listener));
    }
    let path = socket
        .or_else(|| meshfox_core::config::server_socket(Path::new(".")))
        .ok_or_else(|| {
            io::Error::other(
                "no socket to listen on: pass --socket PATH, set `server_socket` in the config, \
                 or start under systemd socket activation",
            )
        })?;
    bind(&path).await.map(|l| Acquired::Bound(l, path))
}

/// Binds `path`, replacing a stale socket file but refusing to steal the
/// path from a coordinator that is still answering.
async fn bind(path: &Path) -> io::Result<UnixListener> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }
    if path.exists() {
        if UnixStream::connect(path).await.is_ok() {
            return Err(io::Error::new(
                io::ErrorKind::AddrInUse,
                format!("a coordinator is already listening on {}", path.display()),
            ));
        }
        std::fs::remove_file(path)?;
    }
    let listener = UnixListener::bind(path)?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}

/// The listener systemd passed us, if this process was socket-activated
/// (`sd_listen_fds(3)` protocol: `LISTEN_PID` is our pid and `LISTEN_FDS` is
/// at least 1). Only the first descriptor is used.
fn inherited_listener() -> io::Result<Option<UnixListener>> {
    let pid_matches = std::env::var("LISTEN_PID")
        .ok()
        .and_then(|v| v.parse::<u32>().ok())
        == Some(std::process::id());
    let fds = std::env::var("LISTEN_FDS")
        .ok()
        .and_then(|v| v.parse::<i32>().ok())
        .unwrap_or(0);
    if !pid_matches || fds < 1 {
        return Ok(None);
    }
    // SAFETY: systemd guarantees fd 3 is open and ours when LISTEN_PID
    // matches; nothing else in this process owns it.
    let std_listener =
        unsafe { std::os::unix::net::UnixListener::from_raw_fd(SD_LISTEN_FDS_START) };
    set_cloexec(SD_LISTEN_FDS_START)?;
    std_listener.set_nonblocking(true)?;
    UnixListener::from_std(std_listener).map(Some)
}

/// systemd leaves the descriptor inheritable; a worker spawned later must
/// not inherit the coordinator's listening socket.
fn set_cloexec(fd: i32) -> io::Result<()> {
    // SAFETY: plain fcntl on a descriptor we own.
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFD);
        if flags < 0 || libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) < 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_socket(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("mfx-serve-{name}-{}.sock", std::process::id()))
    }

    #[tokio::test]
    async fn bind_replaces_a_stale_socket_file() {
        let path = temp_socket("stale");
        // Bind then drop: the file stays, nothing listens behind it.
        drop(std::os::unix::net::UnixListener::bind(&path).unwrap());
        assert!(path.exists());

        let listener = bind(&path).await.unwrap();
        drop(listener);
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn bind_refuses_a_path_a_live_coordinator_owns() {
        let path = temp_socket("live");
        let _ = std::fs::remove_file(&path);
        let live = bind(&path).await.unwrap();

        let err = bind(&path).await.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AddrInUse);

        drop(live);
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn bound_socket_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let path = temp_socket("mode");
        let _ = std::fs::remove_file(&path);
        let listener = bind(&path).await.unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        drop(listener);
        let _ = std::fs::remove_file(&path);
    }
}
