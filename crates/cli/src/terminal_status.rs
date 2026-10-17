//! Presentation of a CLI run in the enclosing terminal. Never used by workers.
use std::io::{IsTerminal, Write};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, clap::ValueEnum)]
pub enum Mode {
    #[default]
    Auto,
    Off,
    Iterm2,
    Progress,
    Title,
}

pub fn default_mode() -> &'static str {
    static DEFAULT: OnceLock<String> = OnceLock::new();
    DEFAULT
        .get_or_init(|| std::env::var("MESHFOX_TERMINAL_STATUS").unwrap_or_else(|_| "auto".into()))
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum State {
    Running(String),
    Waiting(String),
    Done,
    Failed,
    Clear,
}

struct Status {
    mode: Mode,
    state: State,
}
static STATUS: OnceLock<Mutex<Option<Status>>> = OnceLock::new();
fn storage() -> &'static Mutex<Option<Status>> {
    STATUS.get_or_init(|| Mutex::new(None))
}

fn detect(tty: bool, program: &str, windows: bool, multiplexed: bool) -> Mode {
    if !tty || multiplexed {
        Mode::Off
    } else if program == "iTerm.app" {
        Mode::Iterm2
    } else if windows || matches!(program, "ghostty" | "contour") {
        Mode::Progress
    } else {
        Mode::Off
    }
}

pub fn start(mode: Mode) {
    let tty = std::io::stderr().is_terminal();
    let mode = if !tty {
        Mode::Off
    } else if mode == Mode::Auto {
        detect(
            tty,
            &std::env::var("TERM_PROGRAM").unwrap_or_default(),
            std::env::var_os("WT_SESSION").is_some(),
            std::env::var_os("TMUX").is_some() || std::env::var_os("STY").is_some(),
        )
    } else {
        mode
    };
    if mode == Mode::Off {
        return;
    }
    *storage().lock().unwrap() = Some(Status {
        mode,
        state: State::Running("preparing".into()),
    });
    running("preparing");
    // Ghostty expires progress without keep-alives. Keep the task on the CLI
    // runtime, including while a TTY child owns stdin; never read terminal input.
    tokio::spawn(async {
        let mut tick = tokio::time::interval(Duration::from_secs(1));
        loop {
            tick.tick().await;
            let status = storage().lock().unwrap();
            let Some(status) = status.as_ref() else { break };
            if status.mode == Mode::Progress {
                emit(status.mode, &status.state);
            }
        }
    });
}

// Labels come from documents: prevent embedded terminal controls and OSC fields.
fn label(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_control() && *c != ';')
        .take(100)
        .collect()
}
fn sequence(mode: Mode, state: &State) -> String {
    let (text, color) = match state {
        State::Running(name) => (format!("running: {}", label(name)), "#ffa500"),
        State::Waiting(name) => (format!("waiting: {}", label(name)), "#ffd700"),
        State::Done => ("done".into(), "#32cd32"),
        State::Failed => ("failed".into(), "#ff5555"),
        State::Clear => (String::new(), ""),
    };
    match mode {
        Mode::Iterm2 => {
            format!("\x1b]21337;status={text};indicator={color};status-color={color}\x1b\\")
        }
        Mode::Progress => {
            let state = match state {
                State::Running(_) => 3,
                State::Waiting(_) => 4,
                State::Failed => 2,
                _ => 0,
            };
            let value = if matches!(state, 2 | 4) { 100 } else { 0 };
            format!("\x1b]9;4;{state};{value}\x1b\\")
        }
        Mode::Title => format!("\x1b]2;meshfox {text}\x1b\\"),
        _ => String::new(),
    }
}
fn emit(mode: Mode, state: &State) {
    let mut stderr = std::io::stderr().lock();
    let _ = stderr.write_all(sequence(mode, state).as_bytes());
    let _ = stderr.flush();
}
fn set(state: State) {
    if let Some(status) = storage().lock().unwrap().as_mut() {
        emit(status.mode, &state);
        status.state = state;
    }
}
pub fn running(name: &str) {
    set(State::Running(name.into()));
}

pub struct Waiting(Option<State>);
pub fn waiting(name: &str) -> Waiting {
    let mut status = storage().lock().unwrap();
    let previous = status.as_mut().map(|status| {
        let previous = status.state.clone();
        status.state = State::Waiting(name.into());
        emit(status.mode, &status.state);
        previous
    });
    Waiting(previous)
}
impl Drop for Waiting {
    fn drop(&mut self) {
        if let Some(previous) = self.0.take() {
            set(previous);
        }
    }
}

pub fn finish(code: i32) {
    if let Some(status) = storage().lock().unwrap().take() {
        let state = match code {
            0 => State::Done,
            130 => State::Clear,
            _ => State::Failed,
        };
        emit(status.mode, &state);
    }
}
/// Explicit exits do not run destructors. Keep their terminal cleanup here.
pub fn exit(code: i32) -> ! {
    finish(code);
    std::process::exit(code)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn automatic_selection_is_conservative() {
        assert_eq!(detect(true, "iTerm.app", false, false), Mode::Iterm2);
        assert_eq!(detect(true, "ghostty", false, false), Mode::Progress);
        assert_eq!(detect(true, "", true, false), Mode::Progress);
        assert_eq!(detect(true, "unknown", false, false), Mode::Off);
        assert_eq!(detect(false, "iTerm.app", false, false), Mode::Off);
        assert_eq!(detect(true, "iTerm.app", false, true), Mode::Off);
    }
    #[test]
    fn progress_transitions_and_cleanup() {
        for (state, code) in [
            (State::Running("build".into()), 3),
            (State::Waiting("input".into()), 4),
            (State::Failed, 2),
            (State::Done, 0),
            (State::Clear, 0),
        ] {
            assert_eq!(
                sequence(Mode::Progress, &state),
                format!(
                    "\x1b]9;4;{code};{}\x1b\\",
                    if matches!(code, 2 | 4) { 100 } else { 0 }
                )
            );
        }
    }
    #[test]
    fn document_labels_cannot_inject_terminal_sequences() {
        let seq = sequence(
            Mode::Iterm2,
            &State::Running("build\x07\x1b;indicator=bad\n".into()),
        );
        assert_eq!(seq.matches(";indicator=").count(), 1);
        assert!(!seq.contains('\x07'));
        assert_eq!(seq.matches('\x1b').count(), 2);
        assert!(sequence(Mode::Iterm2, &State::Clear).contains("status=;indicator=;status-color="));
    }
}
