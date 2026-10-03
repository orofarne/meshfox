//! `meshfox cores ls|open|kill` — inspect and control the workers ("cores") of
//! the persistent coordinator `server_socket` points at (the macOS daemon, or
//! `meshfox serve`). Speaks `meshfox_server::watcher_protocol`'s `ListCores`,
//! `Open` and `Kill`. Without a configured `server_socket` there is nothing
//! to ask, so every op fails loudly rather than falling back to a private
//! watcher.

use std::io;
use std::path::{Path, PathBuf};

use meshfox_server::watcher_protocol;

#[derive(clap::Subcommand, Debug)]
pub enum CoresOp {
    /// List the coordinator's live cores: canvas path, port, pid.
    Ls,
    /// Show a canvas in the browser, spawning its core first if needed.
    Open {
        /// The canvas to open.
        canvas: PathBuf,
    },
    /// Stop the core serving a canvas. Fails with `no such core` if none is
    /// running for it.
    Kill {
        /// The canvas whose core to stop.
        canvas: PathBuf,
    },
}

pub fn run(op: CoresOp) -> Result<(), String> {
    let socket = configured_socket()?;
    let runtime = tokio::runtime::Runtime::new()
        .map_err(|e| format!("failed to start async runtime: {e}"))?;
    runtime.block_on(run_op(op, &socket))
}

fn configured_socket() -> Result<PathBuf, String> {
    meshfox_core::config::server_socket(Path::new("."))
        .ok_or_else(|| "no `server_socket` configured; there is no coordinator to ask".to_string())
}

async fn run_op(op: CoresOp, socket: &Path) -> Result<(), String> {
    match op {
        CoresOp::Ls => {
            let cores = watcher_protocol::request_list_cores(socket)
                .await
                .map_err(|e| describe(socket, e))?;
            print!("{}", format_cores(&cores));
            Ok(())
        }
        CoresOp::Open { canvas } => {
            let canonical = canonical(&canvas)?;
            watcher_protocol::request_open(socket, &canonical, None)
                .await
                .map_err(|e| describe(socket, e))
        }
        CoresOp::Kill { canvas } => {
            // A core whose canvas file was deleted can't be canonicalized
            // any more; the coordinator matches by the path it recorded, so
            // fall back to the path as given.
            let path = canvas.canonicalize().unwrap_or(canvas);
            watcher_protocol::request_kill(socket, &path)
                .await
                .map_err(|e| describe(socket, e))
        }
    }
}

fn canonical(path: &Path) -> Result<PathBuf, String> {
    path.canonicalize()
        .map_err(|e| format!("{}: {e}", path.display()))
}

/// A reply the coordinator itself sent (`io::ErrorKind::Other`, see
/// `parse_ack`) passes through verbatim; anything else means the
/// coordinator couldn't be reached or answered badly.
fn describe(socket: &Path, e: io::Error) -> String {
    if e.kind() == io::ErrorKind::Other {
        e.to_string()
    } else {
        format!(
            "couldn't reach the coordinator at {}: {e}",
            socket.display()
        )
    }
}

fn format_cores(cores: &[watcher_protocol::CoreInfo]) -> String {
    if cores.is_empty() {
        return "no cores running\n".to_string();
    }
    let mut out = String::new();
    for c in cores {
        let port = c
            .port
            .map_or_else(|| "starting".to_string(), |p| p.to_string());
        out.push_str(&format!(
            "{}\t{}\t{}\n",
            c.canvas_path.display(),
            port,
            c.pid
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use watcher_protocol::CoreInfo;

    #[test]
    fn format_cores_is_tab_separated_with_starting_placeholder() {
        let cores = vec![
            CoreInfo {
                canvas_path: PathBuf::from("/a.canvas.md"),
                port: Some(4242),
                pid: 7,
            },
            CoreInfo {
                canvas_path: PathBuf::from("/b.canvas.md"),
                port: None,
                pid: 8,
            },
        ];
        assert_eq!(
            format_cores(&cores),
            "/a.canvas.md\t4242\t7\n/b.canvas.md\tstarting\t8\n"
        );
    }

    #[test]
    fn format_cores_empty() {
        assert_eq!(format_cores(&[]), "no cores running\n");
    }

    #[test]
    fn describe_passes_coordinator_errors_through() {
        let e = io::Error::other("no such core: /x.canvas.md");
        assert_eq!(
            describe(Path::new("/s.sock"), e),
            "no such core: /x.canvas.md"
        );
        let e = io::Error::from(io::ErrorKind::NotFound);
        assert!(describe(Path::new("/s.sock"), e).starts_with("couldn't reach the coordinator"));
    }
}
