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
use std::path::{Path, PathBuf};
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
    /// A temp file (`spawn_interpreter`'s materialized fence body) to
    /// remove once this process is done with it — removed on drop so it's
    /// cleaned up regardless of which path the caller takes to get there
    /// (normal completion, `kill`, or the run future just getting dropped).
    cleanup: Option<PathBuf>,
}

impl Drop for SpawnedProcess {
    fn drop(&mut self) {
        if let Some(path) = self.cleanup.take() {
            let _ = std::fs::remove_file(path);
        }
    }
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

/// Whether `block` has a streaming executor — either its `lang` is one
/// `core::exec::executor_for` already knows (today: bash/sh), or it
/// carries its own `interpreter=` attribute naming one explicitly. See
/// `spawn_block`.
pub fn supports(block: &meshfox_core::CodeBlock) -> bool {
    block.interpreter.is_some() || meshfox_core::is_supported_lang(&block.lang)
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
/// `meshfox_core::vars`. `cwd`, when given, is the directory the child
/// starts in — a node's own canvas file's directory (see
/// `meshfox_core::canvas::Node::cwd`), not necessarily wherever the
/// server process itself happens to be running from; `None` inherits the
/// server's own cwd unchanged.
pub fn spawn_bash<I, K, V>(code: &str, envs: I, cwd: Option<&Path>) -> io::Result<SpawnedProcess>
where
    I: IntoIterator<Item = (K, V)>,
    K: AsRef<std::ffi::OsStr>,
    V: AsRef<std::ffi::OsStr>,
{
    let mut command = Command::new("bash");
    command.arg("-c").arg(code).envs(envs);
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .process_group(0)
        .spawn()?;

    let (tx, output_rx) = mpsc::unbounded_channel();
    spawn_line_reader(child.stdout.take().expect("piped stdout"), tx.clone());
    spawn_line_reader(child.stderr.take().expect("piped stderr"), tx);

    Ok(SpawnedProcess {
        child,
        output_rx,
        cleanup: None,
    })
}

/// Spawns `program` (found via `PATH`, no shell involved) with `args`, as
/// the leader of a fresh process group — same cancellable/streamed shape as
/// `spawn_bash`, just without a shell or extra environment variables in
/// between. Used to run a `file` node's `interpreter target` (see
/// `meshfox_core::Node::is_runnable_file`), where `target` is a plain path
/// string that has no business being interpreted by a shell.
pub fn spawn_process<I, S>(program: &str, args: I, cwd: Option<&Path>) -> io::Result<SpawnedProcess>
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    let mut command = Command::new(program);
    command.args(args);
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .process_group(0)
        .spawn()?;

    let (tx, output_rx) = mpsc::unbounded_channel();
    spawn_line_reader(child.stdout.take().expect("piped stdout"), tx.clone());
    spawn_line_reader(child.stderr.take().expect("piped stderr"), tx);

    Ok(SpawnedProcess {
        child,
        output_rx,
        cleanup: None,
    })
}

/// Spawns `code` under an explicit `interpreter=` command (see
/// `meshfox_core::exec::split_interpreter`) — the streaming counterpart to
/// `meshfox_core::exec::InterpreterExecutor`: `code` is written to a fresh
/// temp file first (shebang-style, same reasoning as the sync version),
/// then run as `program args... tmpfile`, as the leader of a fresh process
/// group like every other spawn function here. The temp file is removed
/// once the returned `SpawnedProcess` is dropped (see its `Drop` impl).
pub fn spawn_interpreter<I, K, V>(
    interpreter: &str,
    code: &str,
    envs: I,
    cwd: Option<&Path>,
) -> io::Result<SpawnedProcess>
where
    I: IntoIterator<Item = (K, V)>,
    K: AsRef<std::ffi::OsStr>,
    V: AsRef<std::ffi::OsStr>,
{
    let (program, args) = meshfox_core::split_interpreter(interpreter).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("interpreter={interpreter:?} isn't a valid shell-word command"),
        )
    })?;

    let path = std::env::temp_dir().join(format!(
        "meshfox-{}-{}.tmp",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    std::fs::write(&path, code)?;

    let mut command = Command::new(&program);
    command.args(&args).arg(&path).envs(envs);
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    let spawned = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .process_group(0)
        .spawn();

    let mut child = match spawned {
        Ok(child) => child,
        Err(e) => {
            let _ = std::fs::remove_file(&path);
            return Err(e);
        }
    };

    let (tx, output_rx) = mpsc::unbounded_channel();
    spawn_line_reader(child.stdout.take().expect("piped stdout"), tx.clone());
    spawn_line_reader(child.stderr.take().expect("piped stderr"), tx);

    Ok(SpawnedProcess {
        child,
        output_rx,
        cleanup: Some(path),
    })
}

/// Spawns `block`'s code under its own `interpreter=` if it has one,
/// otherwise under plain `bash` — the single dispatch point every caller
/// running a fenced code block (as opposed to a `file` node's
/// `interpreter target`, which always has an interpreter and goes through
/// `spawn_process` directly) should use, so a new interpreter-aware caller
/// never has to duplicate this branch. See `supports` for the matching
/// eligibility check.
pub fn spawn_block<I, K, V>(
    block: &meshfox_core::CodeBlock,
    envs: I,
    cwd: Option<&Path>,
) -> io::Result<SpawnedProcess>
where
    I: IntoIterator<Item = (K, V)>,
    K: AsRef<std::ffi::OsStr>,
    V: AsRef<std::ffi::OsStr>,
{
    match &block.interpreter {
        Some(interpreter) => spawn_interpreter(interpreter, &block.code, envs, cwd),
        None => spawn_bash(&block.code, envs, cwd),
    }
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
        let mut proc = spawn_bash("echo one; echo two", no_envs(), None).unwrap();
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
        let mut proc = spawn_bash("exit 7", no_envs(), None).unwrap();
        while proc.output_rx.recv().await.is_some() {}
        let status = proc.child.wait().await.unwrap();
        assert_eq!(status.code(), Some(7));
    }

    #[tokio::test]
    async fn spawn_interpreter_runs_code_via_the_named_program() {
        // `cat` as a stand-in "interpreter" — no assumption about python
        // being installed, just proves the temp-file-plus-args plumbing.
        let mut proc = spawn_interpreter("cat", "hello from a temp file", no_envs(), None).unwrap();
        let mut lines = Vec::new();
        while let Some(line) = proc.output_rx.recv().await {
            lines.push(line);
        }
        let status = proc.child.wait().await.unwrap();
        assert_eq!(lines, vec!["hello from a temp file"]);
        assert_eq!(status.code(), Some(0));
    }

    #[tokio::test]
    async fn spawn_interpreter_rejects_malformed_command() {
        // `SpawnedProcess` (the `Ok` side) doesn't implement `Debug` — it
        // owns a live child process/channel, nothing worth debug-printing
        // — so this matches instead of `.unwrap_err()`.
        match spawn_interpreter(r#"unterminated ""#, "code", no_envs(), None) {
            Err(e) => assert_eq!(e.kind(), io::ErrorKind::InvalidInput),
            Ok(_) => panic!("expected an error for a malformed interpreter command"),
        }
    }

    #[tokio::test]
    async fn spawn_interpreter_cleans_up_its_temp_file_on_drop() {
        let mut proc = spawn_interpreter("cat", "temp contents", no_envs(), None).unwrap();
        let path = proc.cleanup.clone().expect("interpreter spawn sets cleanup");
        while proc.output_rx.recv().await.is_some() {}
        proc.child.wait().await.unwrap();
        drop(proc);
        assert!(!path.exists());
    }

    #[tokio::test]
    async fn spawn_block_dispatches_to_interpreter_when_set() {
        let block = test_block("cat", Some("cat"), "via interpreter");
        let mut proc = spawn_block(&block, no_envs(), None).unwrap();
        let mut lines = Vec::new();
        while let Some(line) = proc.output_rx.recv().await {
            lines.push(line);
        }
        assert_eq!(lines, vec!["via interpreter"]);
    }

    #[tokio::test]
    async fn spawn_block_falls_back_to_bash_with_no_interpreter() {
        let block = test_block("bash", None, "echo via bash");
        let mut proc = spawn_block(&block, no_envs(), None).unwrap();
        let mut lines = Vec::new();
        while let Some(line) = proc.output_rx.recv().await {
            lines.push(line);
        }
        assert_eq!(lines, vec!["via bash"]);
    }

    fn test_block(lang: &str, interpreter: Option<&str>, code: &str) -> meshfox_core::CodeBlock {
        let md = match interpreter {
            Some(i) => format!("```{lang} name=\"x\" interpreter=\"{i}\"\n{code}\n```\n"),
            None => format!("```{lang} name=\"x\"\n{code}\n```\n"),
        };
        meshfox_core::scan_code_blocks(&md).remove(0)
    }

    #[test]
    fn supports_true_for_bash_or_interpreter_regardless_of_lang() {
        assert!(supports(&test_block("bash", None, "echo hi")));
        assert!(supports(&test_block("python", Some("python3"), "print(1)")));
    }

    #[tokio::test]
    async fn injects_extra_env_vars_on_top_of_the_inherited_ones() {
        let mut proc =
            spawn_bash("echo \"$INSTALL_PATH\"", [("INSTALL_PATH", "/opt/meshfox")], None).unwrap();
        let mut lines = Vec::new();
        while let Some(line) = proc.output_rx.recv().await {
            lines.push(line);
        }
        assert_eq!(lines, vec!["/opt/meshfox"]);
    }

    #[tokio::test]
    async fn kill_terminates_a_long_running_process() {
        let mut proc = spawn_bash("sleep 30", no_envs(), None).unwrap();
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
        let mut proc = spawn_bash("echo starting; sleep 30", no_envs(), None).unwrap();
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
        assert!(
            !still_alive,
            "a process in the killed group is still running"
        );
    }

    #[tokio::test]
    async fn unsupported_language_is_not_reported_as_supported() {
        // `ruby` with no `interpreter=` would never actually reach here in
        // practice — the fence scanner already excludes it as a candidate
        // (see `fence::candidate_fences`) — but `supports` is tested here
        // in isolation regardless, hence a hand-built `CodeBlock` rather
        // than one scanned out of markdown.
        assert!(!supports(&block_with_lang("ruby")));
        assert!(supports(&test_block("bash", None, "echo hi")));
        assert!(supports(&test_block("sh", None, "echo hi")));
    }

    fn block_with_lang(lang: &str) -> meshfox_core::CodeBlock {
        meshfox_core::CodeBlock {
            lang: lang.to_string(),
            name: Some("x".to_string()),
            cache: false,
            default: false,
            tty: false,
            deps: Vec::new(),
            env: Vec::new(),
            interpreter: None,
            attrs: std::collections::HashMap::new(),
            code: "echo hi".to_string(),
            span: 0..0,
        }
    }

    #[tokio::test]
    async fn spawn_process_runs_a_program_directly_with_args() {
        let mut proc = spawn_process("echo", ["one", "two"], None).unwrap();
        let mut lines = Vec::new();
        while let Some(line) = proc.output_rx.recv().await {
            lines.push(line);
        }
        let status = proc.child.wait().await.unwrap();
        assert_eq!(lines, vec!["one two"]);
        assert_eq!(status.code(), Some(0));
    }

    #[tokio::test]
    async fn spawn_process_kill_terminates_it() {
        let mut proc = spawn_process("sleep", ["30"], None).unwrap();
        proc.kill().unwrap();
        let status = proc.child.wait().await.unwrap();
        assert!(!status.success());
    }

    #[tokio::test]
    async fn spawn_bash_runs_in_the_given_cwd() {
        let dir = std::env::temp_dir();
        let mut proc = spawn_bash("pwd -P", no_envs(), Some(&dir)).unwrap();
        let mut lines = Vec::new();
        while let Some(line) = proc.output_rx.recv().await {
            lines.push(line);
        }
        assert_eq!(lines, vec![dir.canonicalize().unwrap().to_string_lossy()]);
    }

    #[tokio::test]
    async fn spawn_interpreter_runs_in_the_given_cwd() {
        // `bash` as the "interpreter", run against a temp file containing
        // `pwd -P` — proves `cwd` reaches the actual spawned child, not
        // just whatever `bash -c` would've inherited.
        let dir = std::env::temp_dir();
        let mut proc = spawn_interpreter("bash", "pwd -P", no_envs(), Some(&dir)).unwrap();
        let mut lines = Vec::new();
        while let Some(line) = proc.output_rx.recv().await {
            lines.push(line);
        }
        assert_eq!(lines, vec![dir.canonicalize().unwrap().to_string_lossy()]);
    }

    #[tokio::test]
    async fn spawn_process_runs_in_the_given_cwd() {
        let dir = std::env::temp_dir();
        let mut proc = spawn_process("pwd", ["-P"], Some(&dir)).unwrap();
        let mut lines = Vec::new();
        while let Some(line) = proc.output_rx.recv().await {
            lines.push(line);
        }
        assert_eq!(lines, vec![dir.canonicalize().unwrap().to_string_lossy()]);
    }
}
