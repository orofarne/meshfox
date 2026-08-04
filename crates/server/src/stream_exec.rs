//! Async, cancellable counterpart to `meshfox_core::exec` — used only by
//! the server, for `meshfox view`'s live-output streaming and Kill button.
//!
//! The CLI keeps using `core::exec::BashExecutor` (`std::process`,
//! blocking `wait()`) unchanged — it never needs to show output before a
//! block finishes, or cancel a block mid-flight, so there's no reason to
//! pull `tokio` into `meshfox-core` (a deliberately light,
//! runtime-agnostic crate) just for this. This duplicates
//! `BashExecutor`'s small "spawn bash, merge stdout+stderr" body in
//! `tokio::process` form — justified by a genuinely different execution
//! model (cancellable/async vs blocking/sync), not accidental drift; keep
//! the two in sync by hand if the merging strategy ever changes.

use std::io;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::mpsc;

/// A running process plus its merged stdout+stderr, line by line, in
/// roughly the order they were actually emitted (same caveat as
/// `core::exec`'s sync reader threads: two separate pipes have no
/// ordering guarantee between them). The channel closes (yields `None`)
/// once the process has closed both streams — not necessarily the same
/// moment `child` is waitable, so callers still need `child.wait()`.
pub struct SpawnedProcess {
    pub child: Child,
    pub output_rx: mpsc::UnboundedReceiver<String>,
}

impl SpawnedProcess {
    /// Kills the *whole process group* this child leads (see
    /// `spawn_bash`'s `process_group(0)`), not just the `bash` PID itself
    /// — a hung script is exactly the case where `bash` has spawned (and
    /// is blocked waiting on) a child of its own, e.g. a bare `sleep 30`
    /// or a server it started; `Child::start_kill()` alone only kills
    /// `bash`, leaving that child running as an orphan.
    pub fn kill(&self) -> io::Result<()> {
        let Some(pid) = self.child.id() else {
            return Ok(()); // already reaped, nothing to signal
        };
        // SAFETY: `libc::kill` with a negative pid signals every process in
        // that process group; `pid` is this child's own pid, and it was
        // made the leader of its own group at spawn time, so `-pid` reaches
        // exactly this subtree and nothing else.
        let ret = unsafe { libc::kill(-(pid as libc::pid_t), libc::SIGKILL) };
        if ret != 0 && io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH) {
            // ESRCH ("no such process") just means it already exited on its
            // own between the caller deciding to kill it and this call —
            // not a real failure.
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

/// Whether `lang` has a streaming executor — same language set as
/// `core::exec::executor_for` (today: bash/sh only), just delegated rather
/// than duplicated, since it's pure data shared by both executors.
pub fn supports(lang: &str) -> bool {
    meshfox_core::is_supported_lang(lang)
}

/// Spawns `code` under `bash -c`, as the leader of a fresh process group
/// (`process_group(0)`) so `SpawnedProcess::kill` can reach every process
/// the script itself spawns, not just `bash`. `kill_on_drop(true)` means
/// dropping the returned `Child` without an explicit wait (e.g. the server
/// task itself getting cancelled) still reaps `bash` rather than leaking
/// it — it does not, on its own, reach the rest of the group; that's what
/// `kill` is for.
///
/// `envs` is added on top of the inherited environment (never replaces
/// it) — this is how resolved `meshfox:var` values reach a block, see
/// `meshfox_core::vars`.
pub fn spawn_bash<I, K, V>(code: &str, envs: I) -> io::Result<SpawnedProcess>
where
    I: IntoIterator<Item = (K, V)>,
    K: AsRef<std::ffi::OsStr>,
    V: AsRef<std::ffi::OsStr>,
{
    let mut child = Command::new("bash")
        .arg("-c")
        .arg(code)
        .envs(envs)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .process_group(0)
        .spawn()?;

    let (tx, output_rx) = mpsc::unbounded_channel();
    spawn_line_reader(child.stdout.take().expect("piped stdout"), tx.clone());
    spawn_line_reader(child.stderr.take().expect("piped stderr"), tx);

    Ok(SpawnedProcess { child, output_rx })
}

fn spawn_line_reader<R>(reader: R, tx: mpsc::UnboundedSender<String>)
where
    R: AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut lines = BufReader::new(reader).lines();
        loop {
            match lines.next_line().await {
                Ok(Some(line)) => {
                    if tx.send(line).is_err() {
                        break; // receiver gone — nobody left to read this
                    }
                }
                _ => break, // EOF or a read error either way ends this stream
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_envs() -> [(&'static str, &'static str); 0] {
        []
    }

    #[tokio::test]
    async fn merges_stdout_and_lets_the_child_be_waited_on() {
        let mut proc = spawn_bash("echo one; echo two", no_envs()).unwrap();
        let mut lines = Vec::new();
        while let Some(line) = proc.output_rx.recv().await {
            lines.push(line);
        }
        let status = proc.child.wait().await.unwrap();
        assert_eq!(lines, vec!["one", "two"]);
        assert_eq!(status.code(), Some(0));
    }

    #[tokio::test]
    async fn reports_nonzero_exit_code() {
        let mut proc = spawn_bash("exit 7", no_envs()).unwrap();
        while proc.output_rx.recv().await.is_some() {}
        let status = proc.child.wait().await.unwrap();
        assert_eq!(status.code(), Some(7));
    }

    #[tokio::test]
    async fn injects_extra_env_vars_on_top_of_the_inherited_ones() {
        let mut proc = spawn_bash("echo \"$INSTALL_PATH\"", [("INSTALL_PATH", "/opt/meshfox")]).unwrap();
        let mut lines = Vec::new();
        while let Some(line) = proc.output_rx.recv().await {
            lines.push(line);
        }
        assert_eq!(lines, vec!["/opt/meshfox"]);
    }

    #[tokio::test]
    async fn kill_terminates_a_long_running_process() {
        let mut proc = spawn_bash("sleep 30", no_envs()).unwrap();
        proc.kill().unwrap();
        let status = proc.child.wait().await.unwrap();
        assert!(!status.success());
    }

    #[tokio::test]
    async fn kill_reaches_the_whole_process_group_not_just_bash() {
        // A script with a preceding command forces bash to actually fork a
        // child for `sleep` rather than exec-replacing itself — the exact
        // shape of a real "hung block" (bash blocked waiting on something
        // it spawned). `Child::start_kill()` alone only kills `bash`,
        // leaving `sleep` running as an orphan — this is the bug `kill`
        // exists to avoid.
        let mut proc = spawn_bash("echo starting; sleep 30", no_envs()).unwrap();
        while let Some(line) = proc.output_rx.recv().await {
            if line == "starting" {
                break;
            }
        }
        let pgid = proc.child.id().expect("still running") as libc::pid_t;

        proc.kill().unwrap();
        proc.child.wait().await.unwrap();
        // Give the OS a moment to actually deliver/process the signal.
        std::thread::sleep(std::time::Duration::from_millis(200));

        // `kill(pid, 0)` sends no signal — it just checks whether anything
        // in that group still exists.
        let still_alive = unsafe { libc::kill(-pgid, 0) } == 0;
        assert!(!still_alive, "a process in the killed group is still running");
    }

    #[tokio::test]
    async fn unsupported_language_is_not_reported_as_supported() {
        assert!(!supports("ruby"));
        assert!(supports("bash"));
        assert!(supports("sh"));
    }
}
