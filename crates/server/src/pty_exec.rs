//! Async bridge to a real pseudo-terminal (`portable-pty`) — used only by
//! `run_block_tty` (see lib.rs) for a `tty` block's own step. Unlike
//! `stream_exec` (piped stdout/stderr, no stdin, no real terminal), this
//! gives the child a genuine pty: raw-mode input, cursor control, and
//! terminal-size queries all work the way they would in a real terminal —
//! see SPEC.md's "Interactive (`tty`) blocks".
//!
//! `portable-pty`'s API is entirely synchronous/blocking; every piece of it
//! (spawn, read, write, resize, wait) runs on a plain OS thread here,
//! bridged to async callers via channels — the same shape `stream_exec`
//! already uses for `std::process`'s blocking `Command`/pipes, just with
//! raw byte chunks instead of lines (a `tty` block's output can contain
//! control sequences that must reach the client byte-for-byte, never
//! split/buffered at `\n`).

use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use tokio::sync::{mpsc, oneshot};

pub struct PtyProcess {
    /// Raw output bytes from the pty, as they're produced.
    pub output_rx: mpsc::UnboundedReceiver<Vec<u8>>,
    input_tx: mpsc::UnboundedSender<Vec<u8>>,
    resize_tx: mpsc::UnboundedSender<(u16, u16)>,
    pid: i32,
    /// Resolves once the child has actually exited. `take()`n by `wait`,
    /// so it's only meaningful to await once.
    exit_rx: Option<oneshot::Receiver<i32>>,
    /// An `interpreter=` spawn's own temp file (see `spawn`) — removed by
    /// the exit-watching thread once the child is confirmed gone, not by
    /// this struct itself; kept here only so tests can observe that it
    /// actually happened.
    #[cfg_attr(not(test), allow(dead_code))]
    cleanup: Option<PathBuf>,
}

impl PtyProcess {
    /// Forwards `bytes` to the pty's stdin — what the client types.
    pub fn write(&self, bytes: Vec<u8>) {
        let _ = self.input_tx.send(bytes);
    }

    /// Tells the pty (and whatever's reading `$COLUMNS`/`$LINES` or
    /// polling `TIOCGWINSZ` inside it) its terminal has this many
    /// columns/rows — the browser's `xterm.js` + fit-addon size, kept in
    /// sync so a full-screen program (`vim`, `htop`, ...) draws correctly.
    pub fn resize(&self, cols: u16, rows: u16) {
        let _ = self.resize_tx.send((cols, rows));
    }

    /// Kills the *whole pty session* (every process in it, not just the
    /// immediate child) — same reasoning, and the same mechanism, as
    /// `stream_exec::SpawnedProcess::kill`: giving a child a controlling
    /// terminal via a pty requires it to become a new session/process-group
    /// leader, so `-pid` reaches everything it spawned too (e.g. a
    /// foreground job an interactive shell started).
    pub fn kill(&self) -> io::Result<()> {
        // SAFETY: `libc::kill` with a negative pid signals every process in
        // that process group; `pid` is this session's own leader pid.
        let ret = unsafe { libc::kill(-(self.pid as libc::pid_t), libc::SIGKILL) };
        if ret != 0 && io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH) {
            // ESRCH just means it already exited on its own — not a real
            // failure, same as `stream_exec::SpawnedProcess::kill`.
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// Waits for the child to exit, returning its exit code (`-1` if it
    /// couldn't be determined, e.g. killed by a signal). Only meaningful to
    /// call once — later calls return `-1` immediately.
    pub async fn wait(&mut self) -> i32 {
        match self.exit_rx.take() {
            Some(rx) => rx.await.unwrap_or(-1),
            None => -1,
        }
    }
}

/// Spawns `code` inside a fresh pty of size `cols`x`rows` — under `bash -c`
/// by default, or under an explicit `interpreter=` command (see
/// `meshfox_core::exec::resolve_command`) when the `tty` block carries one.
/// `envs` is added on top of the inherited environment, same convention
/// `stream_exec::spawn_bash` uses. `cwd`, when given, is the directory the
/// child starts in — a node's own canvas file's directory (see
/// `meshfox_core::canvas::Node::cwd`); `None` inherits the server's own
/// cwd unchanged.
pub fn spawn<I, K, V>(
    code: &str,
    interpreter: Option<&str>,
    envs: I,
    cwd: Option<&Path>,
    cols: u16,
    rows: u16,
) -> io::Result<PtyProcess>
where
    I: IntoIterator<Item = (K, V)>,
    K: AsRef<str>,
    V: AsRef<str>,
{
    let resolved = meshfox_core::resolve_command(code, interpreter)?;

    let pty_system = native_pty_system();
    let pair = pty_system
        .openpty(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(to_io_error)?;

    let mut cmd = CommandBuilder::new(&resolved.program);
    for arg in &resolved.args {
        cmd.arg(arg);
    }
    for (k, v) in envs {
        cmd.env(k.as_ref(), v.as_ref());
    }
    if let Some(cwd) = cwd {
        cmd.cwd(cwd);
    }

    let mut child = match pair.slave.spawn_command(cmd).map_err(to_io_error) {
        Ok(child) => child,
        Err(e) => {
            if let Some(path) = &resolved.cleanup {
                let _ = std::fs::remove_file(path);
            }
            return Err(e);
        }
    };
    // Only needed to spawn the child (which inherits it as its controlling
    // terminal) — dropping our own copy is what lets the master's reader
    // see EOF once the child (the only other holder) actually exits.
    drop(pair.slave);

    let pid = child
        .process_id()
        .ok_or_else(|| io::Error::other("pty child has no pid"))? as i32;

    let mut reader = pair.master.try_clone_reader().map_err(to_io_error)?;
    let mut writer = pair.master.take_writer().map_err(to_io_error)?;
    let master = pair.master;

    let (output_tx, output_rx) = mpsc::unbounded_channel::<Vec<u8>>();
    std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if output_tx.send(buf[..n].to_vec()).is_err() {
                        break; // receiver gone — nobody left to read this
                    }
                }
            }
        }
    });

    let (input_tx, mut input_rx) = mpsc::unbounded_channel::<Vec<u8>>();
    std::thread::spawn(move || {
        while let Some(bytes) = input_rx.blocking_recv() {
            if writer.write_all(&bytes).is_err() || writer.flush().is_err() {
                break;
            }
        }
    });

    let (resize_tx, mut resize_rx) = mpsc::unbounded_channel::<(u16, u16)>();
    std::thread::spawn(move || {
        while let Some((cols, rows)) = resize_rx.blocking_recv() {
            let _ = master.resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            });
        }
    });

    let (exit_tx, exit_rx) = oneshot::channel();
    let cleanup = resolved.cleanup.clone();
    std::thread::spawn(move || {
        let code = child
            .wait()
            .map(|status| status.exit_code() as i32)
            .unwrap_or(-1);
        // Removed here, once the child is actually confirmed gone, rather
        // than relying on the caller to `.wait()` on the returned
        // `PtyProcess` — `relay_tty_step`'s own `Disconnected` case
        // returns without ever awaiting it.
        if let Some(path) = cleanup {
            let _ = std::fs::remove_file(path);
        }
        let _ = exit_tx.send(code);
    });

    Ok(PtyProcess {
        output_rx,
        input_tx,
        resize_tx,
        pid,
        exit_rx: Some(exit_rx),
        cleanup: resolved.cleanup,
    })
}

fn to_io_error(e: anyhow::Error) -> io::Error {
    io::Error::other(e)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_envs() -> [(&'static str, &'static str); 0] {
        []
    }

    #[tokio::test]
    async fn runs_bash_and_captures_output() {
        let mut proc = spawn("echo hello", None, no_envs(), None, 80, 24).unwrap();
        let mut collected = Vec::new();
        while let Some(chunk) = proc.output_rx.recv().await {
            collected.extend_from_slice(&chunk);
            if String::from_utf8_lossy(&collected).contains("hello") {
                break;
            }
        }
        assert!(String::from_utf8_lossy(&collected).contains("hello"));
        assert_eq!(proc.wait().await, 0);
    }

    #[tokio::test]
    async fn reports_nonzero_exit_code() {
        let mut proc = spawn("exit 7", None, no_envs(), None, 80, 24).unwrap();
        while proc.output_rx.recv().await.is_some() {}
        assert_eq!(proc.wait().await, 7);
    }

    #[tokio::test]
    async fn stdin_is_writable() {
        let mut proc = spawn("read line; echo \"got: $line\"", None, no_envs(), None, 80, 24).unwrap();
        proc.write(b"hi there\n".to_vec());
        let mut collected = Vec::new();
        while let Some(chunk) = proc.output_rx.recv().await {
            collected.extend_from_slice(&chunk);
            if String::from_utf8_lossy(&collected).contains("got: hi there") {
                break;
            }
        }
        assert!(String::from_utf8_lossy(&collected).contains("got: hi there"));
    }

    #[tokio::test]
    async fn injects_extra_env_vars_on_top_of_the_inherited_ones() {
        let mut proc = spawn(
            "echo \"$INSTALL_PATH\"",
            None,
            [("INSTALL_PATH", "/opt/meshfox")],
            None,
            80,
            24,
        )
        .unwrap();
        let mut collected = Vec::new();
        while let Some(chunk) = proc.output_rx.recv().await {
            collected.extend_from_slice(&chunk);
            if String::from_utf8_lossy(&collected).contains("/opt/meshfox") {
                break;
            }
        }
        assert!(String::from_utf8_lossy(&collected).contains("/opt/meshfox"));
    }

    #[tokio::test]
    async fn kill_terminates_a_long_running_process() {
        let proc = spawn("sleep 30", None, no_envs(), None, 80, 24).unwrap();
        proc.kill().unwrap();
        // Draining output_rx (dropped instead here, deliberately) isn't
        // needed to confirm the kill worked — `wait` below is enough.
        let mut proc = proc;
        let code = proc.wait().await;
        assert_ne!(code, 0);
    }

    #[tokio::test]
    async fn runs_under_an_explicit_interpreter() {
        // `cat` as a stand-in "interpreter" — no assumption about python
        // being installed, just proves a `tty` block's own `interpreter=`
        // actually reaches the pty instead of always running under `bash`.
        let mut proc = spawn("hello from a tty interpreter", Some("cat"), no_envs(), None, 80, 24)
            .unwrap();
        let mut collected = Vec::new();
        while let Some(chunk) = proc.output_rx.recv().await {
            collected.extend_from_slice(&chunk);
            if String::from_utf8_lossy(&collected).contains("hello from a tty interpreter") {
                break;
            }
        }
        assert!(String::from_utf8_lossy(&collected).contains("hello from a tty interpreter"));
    }

    #[tokio::test]
    async fn interpreter_temp_file_is_removed_once_the_child_exits() {
        let mut proc = spawn("temp contents", Some("cat"), no_envs(), None, 80, 24).unwrap();
        let path = proc.cleanup.clone().expect("interpreter spawn sets cleanup");
        assert!(path.exists());
        while let Some(chunk) = proc.output_rx.recv().await {
            if String::from_utf8_lossy(&chunk).contains("temp contents") {
                break;
            }
        }
        proc.wait().await;
        assert!(!path.exists());
    }

    #[tokio::test]
    async fn runs_in_the_given_cwd() {
        let dir = std::env::temp_dir();
        let mut proc = spawn("pwd -P", None, no_envs(), Some(&dir), 80, 24).unwrap();
        let want = dir.canonicalize().unwrap().to_string_lossy().into_owned();
        let mut collected = Vec::new();
        while let Some(chunk) = proc.output_rx.recv().await {
            collected.extend_from_slice(&chunk);
            if String::from_utf8_lossy(&collected).contains(&want) {
                break;
            }
        }
        assert!(String::from_utf8_lossy(&collected).contains(&want));
    }
}
