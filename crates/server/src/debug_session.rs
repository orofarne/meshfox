//! A persistent `bash` shell kept alive across separate calls — the
//! primitive behind MCP's `debug_start`/`debug_send`/`debug_stop` tools
//! (`crates/cli/src/mcp.rs`). Lives here, not in `crates/cli`, so this
//! process (a `meshfox view`/daemon-managed worker) can own instances of it
//! too, reachable over `/api/debug/*` by a *different* process
//! (`crate::coordinator`-routed MCP/CLI) than the one that started them —
//! moved here verbatim from `mcp.rs`, no behavior change, when that routing
//! was added (see TODO.canvas.md's "Унификация webui/tui/cli/mcp..."
//! discussion).
//!
//! Not a pty: `bash --noprofile --norc` reading a script off its own stdin
//! pipe, stdout/stderr kept as two separate streams — a debug snippet's own
//! output shouldn't need ANSI-escape stripping or pty-line-buffering
//! quirks, and keeping the two streams distinct is what lets a caller tell
//! `stdout`/`stderr` apart in `SendOutcome`. `setsid()`'d on spawn so
//! `stop()`/`terminate_on_timeout()` can signal the whole process group,
//! not just this shell itself.

use std::collections::HashMap;
use std::io;
use std::process::Stdio;
use std::time::{Duration, Instant};

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::mpsc;

/// How long a timed-out `send` gives the hung command to exit after
/// `SIGTERM` before escalating to `SIGKILL` — see
/// `DebugSession::terminate_on_timeout`.
const TERM_GRACE: Duration = Duration::from_secs(2);

enum StreamLine {
    Out(String),
    Err(String),
}

pub struct DebugSession {
    child: Child,
    stdin: ChildStdin,
    lines_rx: mpsc::UnboundedReceiver<StreamLine>,
    last_used: Instant,
}

#[derive(Debug)]
pub struct SendOutcome {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
    pub timed_out: bool,
    pub session_ended: bool,
}

impl DebugSession {
    pub fn spawn(cwd: &std::path::Path, envs: HashMap<String, String>) -> io::Result<Self> {
        let mut command = Command::new("bash");
        // `--noprofile --norc`: a plain interactive-less shell, not a login
        // shell — nothing from the user's own `.bashrc` should silently
        // change how a debug snippet behaves. Reads commands from its own
        // stdin pipe, same as `bash < script.sh` — not a pty (see this
        // module's own doc comment for why: separate stdout/stderr, no
        // ANSI/terminal concerns to strip).
        command.arg("--noprofile").arg("--norc");
        command.current_dir(cwd);
        command.envs(envs);
        command.stdin(Stdio::piped());
        command.stdout(Stdio::piped());
        command.stderr(Stdio::piped());
        command.kill_on_drop(true);
        // `setsid()` — not the simpler `process_group(0)` this used to be —
        // for two reasons at once: it still makes this shell the leader of
        // its own new process group (pgid == its own pid), which is all
        // `stop()`/`terminate_on_timeout`/`signal_group` below actually need
        // to reach everything this shell spawns; but it *also* detaches
        // from any controlling
        // terminal, which `process_group(0)` alone does not do. That
        // detachment matters because libpq's password prompt
        // (`simple_prompt`) opens `/dev/tty` directly, bypassing stdin/
        // stdout entirely — if a controlling terminal is inherited from
        // whatever launched this process, that open succeeds and the
        // prompt blocks forever on a tty nobody will ever type into. With
        // no controlling terminal, `open("/dev/tty")` fails outright
        // (`ENXIO`) and libpq falls back to stdin, which fails fast instead
        // (see TODO.canvas.md's "PGPASSWORD-подстановка..." node). Not
        // combined with `process_group(0)`: `setsid()` requires the caller
        // not already be a process group leader, and `process_group(0)`'s
        // `setpgid` may run before this closure does.
        //
        // SAFETY: the only syscall here is `setsid()` itself, which is
        // async-signal-safe — safe to call between fork and exec.
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }

        let mut child = command.spawn()?;
        let stdin = child.stdin.take().expect("piped stdin");
        let stdout = child.stdout.take().expect("piped stdout");
        let stderr = child.stderr.take().expect("piped stderr");

        let (tx, rx) = mpsc::unbounded_channel();
        let tx_out = tx.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if tx_out.send(StreamLine::Out(line)).is_err() {
                    break;
                }
            }
        });
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if tx.send(StreamLine::Err(line)).is_err() {
                    break;
                }
            }
        });

        Ok(DebugSession {
            child,
            stdin,
            lines_rx: rx,
            last_used: Instant::now(),
        })
    }

    /// How long this session has sat idle since its last `send` — used by
    /// an idle-sweep to reclaim a forgotten session (see `mcp.rs`'s own
    /// `spawn_idle_sweep` for the in-process case).
    pub fn idle_for(&self) -> Duration {
        self.last_used.elapsed()
    }

    /// Whether the shell has already exited (and been reaped) — mainly so
    /// `send`'s own timeout/escalation behavior has something to assert
    /// against in tests (this crate's own and `meshfox-cli`'s, which can't
    /// share a `#[cfg(test)]` item across the crate boundary), not part of
    /// this type's otherwise-private internals.
    pub fn has_exited(&mut self) -> bool {
        self.child.try_wait().unwrap().is_some()
    }

    /// Runs `code` in this session's own shell and waits for it to finish —
    /// marked by a unique sentinel line this call appends after `code`,
    /// printed to *both* streams with the real exit code so completion is
    /// only declared once both stdout and stderr have delivered everything
    /// up to that point (they're unrelated pipes with no ordering
    /// guarantee between them). If `code` itself times out, this call can't
    /// tell that command's own trailing output apart from whatever a next
    /// call might get back — so instead of leaving it running and letting
    /// later `send`s queue up behind it forever, it kills the whole session
    /// (`terminate_on_timeout`) and reports `session_ended`; a fresh
    /// session is the clean way back in, same as after an explicit `stop`.
    pub async fn send(&mut self, code: &str, timeout: Duration) -> io::Result<SendOutcome> {
        self.last_used = Instant::now();
        let marker = format!("__meshfox_done_{}__", uuid::Uuid::new_v4().simple());
        let wrapped = format!(
            "{code}\n__mfx_rc=$?\nprintf '%s %s\\n' '{marker}' \"$__mfx_rc\" >&2\nprintf '%s %s\\n' '{marker}' \"$__mfx_rc\" >&1\n"
        );
        self.stdin.write_all(wrapped.as_bytes()).await?;
        self.stdin.flush().await?;

        let mut stdout_lines = Vec::new();
        let mut stderr_lines = Vec::new();
        let mut exit_code: i32 = -1;
        let mut stdout_done = false;
        let mut stderr_done = false;
        let deadline = tokio::time::Instant::now() + timeout;

        while !(stdout_done && stderr_done) {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                self.terminate_on_timeout().await;
                return Ok(SendOutcome {
                    stdout: stdout_lines.join("\n"),
                    stderr: stderr_lines.join("\n"),
                    exit_code,
                    timed_out: true,
                    session_ended: true,
                });
            }
            match tokio::time::timeout(remaining, self.lines_rx.recv()).await {
                Ok(Some(StreamLine::Out(line))) => match strip_marker(&line, &marker) {
                    Some(rc) => {
                        exit_code = rc;
                        stdout_done = true;
                    }
                    None => stdout_lines.push(line),
                },
                Ok(Some(StreamLine::Err(line))) => match strip_marker(&line, &marker) {
                    Some(rc) => {
                        exit_code = rc;
                        stderr_done = true;
                    }
                    None => stderr_lines.push(line),
                },
                // Both streams closed — the shell itself exited (e.g. the
                // debug code called `exit`) — nothing more will ever
                // arrive, so stop waiting rather than spin until the
                // timeout for no reason.
                Ok(None) => {
                    return Ok(SendOutcome {
                        stdout: stdout_lines.join("\n"),
                        stderr: stderr_lines.join("\n"),
                        exit_code,
                        timed_out: false,
                        session_ended: true,
                    });
                }
                Err(_) => {
                    self.terminate_on_timeout().await;
                    return Ok(SendOutcome {
                        stdout: stdout_lines.join("\n"),
                        stderr: stderr_lines.join("\n"),
                        exit_code,
                        timed_out: true,
                        session_ended: true,
                    });
                }
            }
        }
        Ok(SendOutcome {
            stdout: stdout_lines.join("\n"),
            stderr: stderr_lines.join("\n"),
            exit_code,
            timed_out: false,
            session_ended: false,
        })
    }

    pub async fn stop(&mut self) {
        let _ = self.signal_group(libc::SIGKILL);
        let _ = self.child.wait().await;
    }

    /// Escalated kill for a `code` that blew through its `send` timeout:
    /// `SIGTERM` first, so anything that traps it (or just needs a moment
    /// to flush/close a connection) can exit cleanly, then — only if it's
    /// still alive after `TERM_GRACE` — `SIGKILL`, which can't be caught or
    /// ignored. Whole group either way, same as `stop()`, since there's no
    /// way to tell this session's shell apart from whatever hung command
    /// it's still waiting on (see `send`'s own doc comment) — this ends the
    /// session, it doesn't try to save it.
    async fn terminate_on_timeout(&mut self) {
        let _ = self.signal_group(libc::SIGTERM);
        if tokio::time::timeout(TERM_GRACE, self.child.wait())
            .await
            .is_err()
        {
            let _ = self.signal_group(libc::SIGKILL);
            let _ = self.child.wait().await;
        }
    }

    fn signal_group(&self, signal: libc::c_int) -> io::Result<()> {
        let Some(pid) = self.child.id() else {
            return Ok(()); // already reaped
        };
        // SAFETY: `libc::kill` with a negative pid signals every process in
        // that process group; `pid` is this session's own leader pid (see
        // `spawn`'s `setsid()`).
        let ret = unsafe { libc::kill(-(pid as libc::pid_t), signal) };
        if ret != 0 && io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH) {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

/// `line` is the sentinel this session's own `send` appended, if it starts
/// with `marker` — returns the exit code it carries. Anything else is
/// real output from the code that ran, passed through unchanged.
fn strip_marker(line: &str, marker: &str) -> Option<i32> {
    line.strip_prefix(marker)?.trim().parse().ok()
}
