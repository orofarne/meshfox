//! Executors: running a code block's source for a given fence language.
//!
//! Only `bash` is implemented today; `gherkin` is planned (see README
//! roadmap). Adding a language is additive — implement `Executor` and wire
//! it up in `executor_for`.

use crate::output::ExecOutput;
use std::io::{self, BufRead, BufReader, Read};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;

pub trait Executor {
    fn run(&self, code: &str) -> io::Result<ExecOutput>;
}

pub struct BashExecutor;

impl Executor for BashExecutor {
    fn run(&self, code: &str) -> io::Result<ExecOutput> {
        let mut child = Command::new("bash")
            .arg("-e")
            .arg("-c")
            .arg(code)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;

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
        let result = exec.run("echo hello").unwrap();
        assert_eq!(result.exit_code, 0);
        assert_eq!(result.output.trim(), "hello");
    }

    #[test]
    fn captures_nonzero_exit_code() {
        let exec = BashExecutor;
        let result = exec.run("exit 7").unwrap();
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
    fn interleaves_stdout_and_stderr_in_emission_order() {
        let exec = BashExecutor;
        // Slight delays force genuinely separate writes rather than both
        // streams filling their OS buffer before either is read.
        let result = exec
            .run("echo out1; sleep 0.05; echo err1 >&2; sleep 0.05; echo out2; sleep 0.05; echo err2 >&2")
            .unwrap();
        let lines: Vec<&str> = result.output.lines().collect();
        assert_eq!(lines, vec!["out1", "err1", "out2", "err2"]);
    }

    #[test]
    fn captures_output_with_no_trailing_newline() {
        let exec = BashExecutor;
        let result = exec.run("printf 'no newline'").unwrap();
        assert_eq!(result.output.trim_end(), "no newline");
    }
}
