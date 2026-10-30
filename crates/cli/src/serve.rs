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
//!
//! With `--http ADDR` the same process also runs the gateway
//! (`crate::gateway`): an index of the canvases under `--root` and a reverse
//! proxy to their cores, behind an access token unless ADDR is loopback.

use std::io;
use std::os::fd::FromRawFd;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use tokio::net::{UnixListener, UnixStream};

use crate::gateway::{CanvasResolver, Gateway, GatewayConfig, PortFuture};
use crate::watcher::{accept_loop, ensure_core, wait_for_terminate, Announce, Registry};

/// First file descriptor systemd passes (`SD_LISTEN_FDS_START`).
const SD_LISTEN_FDS_START: i32 = 3;

enum Acquired {
    /// Handed over by systemd; never unlinked.
    Inherited(UnixListener),
    /// Bound by us at the path; unlinked on exit.
    Bound(UnixListener, PathBuf),
}

/// `meshfox serve --http`, as parsed from the command line.
pub struct HttpOptions {
    pub addr: String,
    pub root: Option<PathBuf>,
    pub token_file: Option<PathBuf>,
    pub allowed_hosts: Vec<String>,
    pub public_url: Option<String>,
    pub trust_proxy: bool,
}

/// The gateway's view of this process's own registry.
struct LocalResolver {
    registry: Arc<Registry>,
    exe: PathBuf,
    socket_path: PathBuf,
}

impl CanvasResolver for LocalResolver {
    fn port_for(&self, canvas: PathBuf) -> PortFuture<'_> {
        Box::pin(ensure_core(
            &self.registry,
            &self.exe,
            &self.socket_path,
            canvas,
            None,
        ))
    }

    fn running(&self) -> Vec<PathBuf> {
        self.registry
            .list_cores()
            .into_iter()
            .map(|core| core.canvas_path)
            .collect()
    }
}

pub async fn run(
    exe: PathBuf,
    socket: Option<PathBuf>,
    http: Option<HttpOptions>,
) -> io::Result<()> {
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
    // Set up the gateway before accepting anything, so a bad `--http`
    // (non-existent root, port taken) fails the whole start.
    let gateway_task = match http {
        Some(options) => {
            let resolver = Arc::new(LocalResolver {
                registry: Arc::clone(&registry),
                exe: exe.clone(),
                socket_path: socket_path.clone(),
            });
            Some(start_gateway(options, resolver).await?)
        }
        None => None,
    };
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
    if let Some(task) = gateway_task {
        task.abort();
    }
    if owned {
        let _ = tokio::fs::remove_file(&socket_path).await;
    }
    Ok(())
}

/// Where the access token comes from: `MESHFOX_TOKEN`, then `--token-file`
/// (first line). `None` when neither is set.
fn configured_token(token_file: Option<&Path>) -> io::Result<Option<String>> {
    if let Some(token) = std::env::var("MESHFOX_TOKEN")
        .ok()
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
    {
        return Ok(Some(token));
    }
    let Some(file) = token_file else {
        return Ok(None);
    };
    let text = std::fs::read_to_string(file)
        .map_err(|e| io::Error::other(format!("cannot read {}: {e}", file.display())))?;
    match text.lines().next().map(str::trim).filter(|t| !t.is_empty()) {
        Some(token) => Ok(Some(token.to_string())),
        None => Err(io::Error::other(format!(
            "{} has no token on its first line",
            file.display()
        ))),
    }
}

/// 256 bits from the OS, as hex.
fn generate_token() -> String {
    format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}

/// A scan this slow gets a hint on stderr.
const SLOW_SCAN: Duration = Duration::from_secs(5);

fn slow_scan_hint(root: &Path, report: &meshfox_core::canvas_index::ScanReport) -> String {
    let what = if report.truncated {
        "stopped early, so some canvases are missing from the index"
    } else {
        "finished"
    };
    format!(
        "meshfox serve: scanning {} {what} after {:.1}s ({} entries looked at); \
         list directories you do not serve (build output, dependencies, data) in {} \
         (`.gitignore` syntax, e.g. `node_modules/`) to make it quick",
        root.display(),
        report.duration.as_secs_f64(),
        report.visited,
        root.join(meshfox_core::canvas_index::IGNORE_FILE).display()
    )
}

async fn start_gateway(
    options: HttpOptions,
    resolver: Arc<LocalResolver>,
) -> io::Result<tokio::task::JoinHandle<()>> {
    let root = options.root.unwrap_or_else(|| PathBuf::from("."));
    let root = root
        .canonicalize()
        .map_err(|e| io::Error::other(format!("--root {}: {e}", root.display())))?;
    let listener = tokio::net::TcpListener::bind(&options.addr)
        .await
        .map_err(|e| io::Error::new(e.kind(), format!("--http {}: {e}", options.addr)))?;
    let local = listener.local_addr()?;
    let loopback = local.ip().is_loopback();

    let mut token = configured_token(options.token_file.as_deref())?;
    if token.is_none() && !loopback {
        // A reachable gateway runs code for whoever holds the token, so it
        // is never started open: make one and say so.
        let generated = generate_token();
        eprintln!("meshfox serve: generated access token (set MESHFOX_TOKEN to choose your own):");
        eprintln!("  {generated}");
        token = Some(generated);
    }
    let gateway = Gateway::new(
        GatewayConfig {
            root: root.clone(),
            token,
            allowed_hosts: options.allowed_hosts,
            loopback,
            trust_proxy: options.trust_proxy,
        },
        Arc::clone(&resolver) as Arc<dyn CanvasResolver>,
    )?;
    // A scan that takes long is the user's cue to exclude what they do not
    // serve: say so once, with the file to write.
    let hint_root = root.clone();
    let hinted = std::sync::atomic::AtomicBool::new(false);
    gateway.index().on_scan(move |report| {
        let slow = report.duration >= SLOW_SCAN || report.truncated;
        if slow && !hinted.swap(true, std::sync::atomic::Ordering::SeqCst) {
            eprintln!("{}", slow_scan_hint(&hint_root, report));
        }
    });
    // Where a canvas "opened" by `meshfox view`/`cores open` is shown to the
    // user. A wildcard bind (a container) is reached as localhost through the
    // published port unless `--public-url` says otherwise.
    let base = options
        .public_url
        .map(|u| u.trim_end_matches('/').to_string())
        .unwrap_or_else(|| {
            if local.ip().is_unspecified() {
                format!("http://localhost:{}", local.port())
            } else {
                format!("http://{local}")
            }
        });
    resolver.registry.set_announce(Announce {
        base,
        root: root.clone(),
        open_locally: loopback,
    });
    eprintln!(
        "meshfox serve: http on http://{local}/ (root {})",
        root.display()
    );
    let app = gateway.router();
    Ok(tokio::spawn(async move {
        let service = app.into_make_service_with_connect_info::<std::net::SocketAddr>();
        if let Err(e) = axum::serve(listener, service).await {
            eprintln!("meshfox serve: http server stopped: {e}");
        }
    }))
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

    #[test]
    fn the_slow_scan_hint_names_the_file_to_write_and_what_happened() {
        use meshfox_core::canvas_index::ScanReport;
        let report = |duration, truncated| ScanReport {
            duration,
            visited: 342_196,
            truncated,
            error: None,
        };
        let slow = slow_scan_hint(Path::new("/work"), &report(Duration::from_secs(31), false));
        assert!(slow.contains("finished after 31.0s") && slow.contains("342196 entries"));
        assert!(slow.contains("/work/.meshfoxignore") && slow.contains("node_modules/"));
        let cut = slow_scan_hint(Path::new("/work"), &report(Duration::from_secs(60), true));
        assert!(cut.contains("stopped early") && cut.contains("missing"));
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
