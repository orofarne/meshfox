//! Executors: running a code block's source for a given fence language.
//!
//! `bash`/`sh` fences run under `BashExecutor` with no further ado. Any
//! fence (regardless of `lang`) can instead carry its own `interpreter=`
//! attribute — a shebang-style command+flags string (`interpreter="python3
//! -u"`) — and run under `InterpreterExecutor` instead. Adding a *built-in*
//! language (no `interpreter=` needed) is additive — implement `Executor`
//! and wire it up in `executor_for`.

use crate::output::ExecOutput;
use std::io::{self, BufRead, BufReader, Read};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;

pub trait Executor {
    /// `cwd`, when given, is the directory the child process starts in —
    /// a node's own canvas file's directory (see `crate::canvas::Node::cwd`),
    /// not necessarily wherever the calling process itself happens to be
    /// running from. `None` inherits the caller's own cwd unchanged.
    fn run(&self, code: &str, cwd: Option<&Path>) -> io::Result<ExecOutput>;
}

pub struct BashExecutor;

impl Executor for BashExecutor {
    fn run(&self, code: &str, cwd: Option<&Path>) -> io::Result<ExecOutput> {
        let started = std::time::Instant::now();
        let mut command = Command::new("bash");
        command.arg("-e").arg("-c").arg(code);
        if let Some(cwd) = cwd {
            command.current_dir(cwd);
        }
        let mut child = command.stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()?;

        // stdout/stderr are separate pipes with no ordering between them —
        // draining one fully before the other (e.g. via `Command::output`)
        // would show every stdout line before every stderr line regardless
        // of when each was actually written. Reading both concurrently,
        // line by line, into one mutex-guarded buffer keeps them
        // interleaved in roughly the order they were really emitted.
        let buffer: Arc<Mutex<String>> = Arc::new(Mutex::new(String::new()));
        let stdout_thread = spawn_line_reader(child.stdout.take().expect("piped stdout"), &buffer);
        let stderr_thread = spawn_line_reader(child.stderr.take().expect("piped stderr"), &buffer);

        let status = child.wait()?;
        stdout_thread.join().ok();
        stderr_thread.join().ok();

        let output = Arc::try_unwrap(buffer)
            .map(|m| m.into_inner().unwrap())
            .unwrap_or_default();

        Ok(ExecOutput {
            exit_code: status.code().unwrap_or(-1),
            output,
            duration_ms: started.elapsed().as_millis() as u64,
        })
    }
}

fn spawn_line_reader<R>(reader: R, buffer: &Arc<Mutex<String>>) -> thread::JoinHandle<()>
where
    R: Read + Send + 'static,
{
    let buffer = Arc::clone(buffer);
    thread::spawn(move || {
        for line in BufReader::new(reader).lines() {
            let Ok(line) = line else { break };
            let mut buf = buffer.lock().unwrap();
            buf.push_str(&line);
            buf.push('\n');
        }
    })
}

/// Splits an `interpreter="..."` attribute value into a program name plus
/// its fixed argument list — shell-word syntax (quoting supported), the
/// same shape as a `#!/usr/bin/env -S ...` shebang line: `"python3 -u"` ->
/// `("python3", ["-u"])`. `None` on malformed quoting (e.g. an
/// unterminated `"`) or an empty/blank spec.
pub fn split_interpreter(spec: &str) -> Option<(String, Vec<String>)> {
    let mut words = shlex::split(spec)?;
    if words.is_empty() {
        return None;
    }
    let program = words.remove(0);
    Some((program, words))
}

/// What to actually spawn for `code` given an optional `interpreter=`
/// value — the shared decision behind every "run this block" path that
/// builds its own `Command`/`CommandBuilder` rather than going through
/// `InterpreterExecutor` directly (`crate` doesn't depend on `tokio` or
/// `portable-pty`, so those callers — `stream_exec`, `pty_exec`, the
/// CLI/TUI's own `tty`-handoff spawns — can't just reuse that `Executor`
/// impl itself, only the "what to run" part of it). `interpreter = None`
/// resolves to the implicit `bash -c code`; `Some(spec)` splits `spec`
/// (`split_interpreter`) and writes `code` to a fresh temp file, the same
/// shebang-script shape `InterpreterExecutor` uses.
pub struct ResolvedCommand {
    pub program: String,
    pub args: Vec<String>,
    /// A temp file holding `code`'s body, if one was created — remove it
    /// once the spawned child has exited (however the caller learns of
    /// that: an owned process handle's own `Drop`, or inline right after
    /// its own blocking `wait()`).
    pub cleanup: Option<std::path::PathBuf>,
}

pub fn resolve_command(code: &str, interpreter: Option<&str>) -> io::Result<ResolvedCommand> {
    match interpreter {
        None => Ok(ResolvedCommand {
            program: "bash".to_string(),
            args: vec!["-c".to_string(), code.to_string()],
            cleanup: None,
        }),
        Some(spec) => {
            let (program, mut args) = split_interpreter(spec).ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("interpreter={spec:?} isn't a valid shell-word command"),
                )
            })?;
            let path = std::env::temp_dir().join(format!("meshfox-{}.tmp", uuid_like_suffix()));
            std::fs::write(&path, code)?;
            args.push(path.display().to_string());
            Ok(ResolvedCommand {
                program,
                args,
                cleanup: Some(path),
            })
        }
    }
}

/// Runs a fence's code under an explicit `interpreter=` command — the
/// generalized, any-language counterpart to `BashExecutor`. The code is
/// written to a fresh temp file first (rather than passed via `-c`/stdin)
/// so it behaves like a real shebang script: works for interpreters that
/// care about `__file__`/script-relative paths, and sidesteps shell
/// quoting entirely. The temp file is removed once the child exits,
/// success or failure alike.
pub struct InterpreterExecutor {
    pub program: String,
    pub args: Vec<String>,
}

impl Executor for InterpreterExecutor {
    fn run(&self, code: &str, cwd: Option<&Path>) -> io::Result<ExecOutput> {
        let started = std::time::Instant::now();
        let path = std::env::temp_dir().join(format!("meshfox-{}.tmp", uuid_like_suffix()));
        std::fs::write(&path, code)?;
        let result = (|| {
            let mut command = Command::new(&self.program);
            command.args(&self.args).arg(&path);
            if let Some(cwd) = cwd {
                command.current_dir(cwd);
            }
            let mut child = command.stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()?;

            let buffer: Arc<Mutex<String>> = Arc::new(Mutex::new(String::new()));
            let stdout_thread =
                spawn_line_reader(child.stdout.take().expect("piped stdout"), &buffer);
            let stderr_thread =
                spawn_line_reader(child.stderr.take().expect("piped stderr"), &buffer);

            let status = child.wait()?;
            stdout_thread.join().ok();
            stderr_thread.join().ok();

            let output = Arc::try_unwrap(buffer)
                .map(|m| m.into_inner().unwrap())
                .unwrap_or_default();

            Ok(ExecOutput {
                exit_code: status.code().unwrap_or(-1),
                output,
                duration_ms: started.elapsed().as_millis() as u64,
            })
        })();
        let _ = std::fs::remove_file(&path);
        result
    }
}

/// A short, unique-enough suffix for a temp filename without pulling in a
/// UUID dependency just for this — process id plus a monotonic counter is
/// enough to avoid collisions between concurrently-running blocks in the
/// same process.
fn uuid_like_suffix() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{}-{}", std::process::id(), n)
}

/// Fence languages meshfox actually knows how to execute. Exposed
/// separately from `executor_for` so fence scanning (`crate::fence`) can
/// decide whether a fence is eligible to be treated as runnable *at all* —
/// named or implicitly named — before anything ever tries to execute it.
/// This is what keeps an ordinary Markdown doc's non-`bash`/`sh` example
/// fences (a `yaml` config sample, a `json` snippet, ...) from being
/// mistaken for "the" runnable block in a node that has no real meshfox
/// structure of its own.
pub fn is_supported_lang(lang: &str) -> bool {
    matches!(lang, "bash" | "sh")
}

/// Look up the executor for a fence language, e.g. `"bash"`.
pub fn executor_for(lang: &str) -> Option<Box<dyn Executor>> {
    if is_supported_lang(lang) {
        Some(Box::new(BashExecutor))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runs_bash_and_captures_stdout() {
        let exec = BashExecutor;
        let result = exec.run("echo hello", None).unwrap();
        assert_eq!(result.exit_code, 0);
        assert_eq!(result.output.trim(), "hello");
    }

    #[test]
    fn captures_nonzero_exit_code() {
        let exec = BashExecutor;
        let result = exec.run("exit 7", None).unwrap();
        assert_eq!(result.exit_code, 7);
    }

    #[test]
    fn unknown_language_has_no_executor() {
        assert!(executor_for("ruby").is_none());
    }

    #[test]
    fn is_supported_lang_matches_executor_for() {
        assert!(is_supported_lang("bash"));
        assert!(is_supported_lang("sh"));
        assert!(!is_supported_lang("yaml"));
        assert!(!is_supported_lang("ruby"));
    }

    #[test]
    fn split_interpreter_splits_program_and_flags() {
        assert_eq!(
            split_interpreter("python3 -u"),
            Some(("python3".to_string(), vec!["-u".to_string()]))
        );
    }

    #[test]
    fn split_interpreter_bare_program_has_no_args() {
        assert_eq!(
            split_interpreter("python3"),
            Some(("python3".to_string(), vec![]))
        );
    }

    #[test]
    fn split_interpreter_honors_quoting() {
        assert_eq!(
            split_interpreter(r#"env "my python" -u"#),
            Some((
                "env".to_string(),
                vec!["my python".to_string(), "-u".to_string()]
            ))
        );
    }

    #[test]
    fn split_interpreter_none_for_blank_or_malformed() {
        assert_eq!(split_interpreter(""), None);
        assert_eq!(split_interpreter("   "), None);
        assert_eq!(split_interpreter(r#"unterminated ""#), None);
    }

    #[test]
    fn resolve_command_with_no_interpreter_is_implicit_bash() {
        let resolved = resolve_command("echo hi", None).unwrap();
        assert_eq!(resolved.program, "bash");
        assert_eq!(resolved.args, vec!["-c".to_string(), "echo hi".to_string()]);
        assert!(resolved.cleanup.is_none());
    }

    #[test]
    fn resolve_command_with_interpreter_writes_a_temp_file_and_appends_its_path() {
        let resolved = resolve_command("print('hi')", Some("python3 -u")).unwrap();
        assert_eq!(resolved.program, "python3");
        let cleanup = resolved.cleanup.clone().expect("interpreter spawn sets cleanup");
        assert_eq!(
            resolved.args,
            vec!["-u".to_string(), cleanup.display().to_string()]
        );
        assert_eq!(std::fs::read_to_string(&cleanup).unwrap(), "print('hi')");
        std::fs::remove_file(&cleanup).unwrap();
    }

    #[test]
    fn resolve_command_rejects_a_malformed_interpreter() {
        assert!(resolve_command("code", Some(r#"unterminated ""#)).is_err());
    }

    #[test]
    fn interpreter_executor_runs_program_with_args_against_a_temp_file() {
        let exec = InterpreterExecutor {
            program: "python3".to_string(),
            args: vec!["-u".to_string()],
        };
        let Ok(result) = exec.run("print('hello')", None) else {
            return; // python3 not installed in this environment — skip
        };
        assert_eq!(result.exit_code, 0);
        assert_eq!(result.output.trim(), "hello");
    }

    #[test]
    fn interleaves_stdout_and_stderr_in_emission_order() {
        let exec = BashExecutor;
        // Slight delays force genuinely separate writes rather than both
        // streams filling their OS buffer before either is read.
        let result = exec
            .run(
                "echo out1; sleep 0.05; echo err1 >&2; sleep 0.05; echo out2; sleep 0.05; echo err2 >&2",
                None,
            )
            .unwrap();
        let lines: Vec<&str> = result.output.lines().collect();
        assert_eq!(lines, vec!["out1", "err1", "out2", "err2"]);
    }

    #[test]
    fn captures_output_with_no_trailing_newline() {
        let exec = BashExecutor;
        let result = exec.run("printf 'no newline'", None).unwrap();
        assert_eq!(result.output.trim_end(), "no newline");
    }

    #[test]
    fn bash_executor_runs_in_the_given_cwd() {
        let exec = BashExecutor;
        let dir = std::env::temp_dir();
        let result = exec.run("pwd -P", Some(&dir)).unwrap();
        assert_eq!(
            std::path::Path::new(result.output.trim()),
            dir.canonicalize().unwrap()
        );
    }

    #[test]
    fn interpreter_executor_runs_in_the_given_cwd() {
        let exec = InterpreterExecutor {
            program: "python3".to_string(),
            args: vec![],
        };
        let dir = std::env::temp_dir();
        let Ok(result) = exec.run("import os; print(os.getcwd())", Some(&dir)) else {
            return; // python3 not installed in this environment — skip
        };
        assert_eq!(
            std::path::Path::new(result.output.trim()),
            dir.canonicalize().unwrap()
        );
    }
}
