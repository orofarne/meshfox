//! Formatting shared by plain runs and the raw-terminal tty transcript.

use std::io::IsTerminal;

fn color_enabled() -> bool {
    std::io::stdout().is_terminal()
        && std::env::var_os("NO_COLOR").is_none_or(|value| value.is_empty())
        && std::env::var_os("TERM").is_none_or(|value| value != "dumb")
}

fn line(text: &str, color: &str, enabled: bool, newline: &str) -> String {
    if enabled {
        format!("\x1b[{color}m{text}\x1b[0m{newline}")
    } else {
        format!("{text}{newline}")
    }
}

pub(crate) fn banner(text: &str, newline: &str) {
    print!(
        "{newline}{}",
        line(&format!("==> {text}"), "1;36", color_enabled(), newline)
    );
}

pub(crate) fn step_end(exit_code: i32, duration_ms: u64, newline: &str) {
    let text = format!(
        "(exit {exit_code} · {})",
        meshfox_core::format_duration_ms(duration_ms)
    );
    print!(
        "{}",
        line(
            &text,
            if exit_code == 0 { "32" } else { "31" },
            color_enabled(),
            newline
        )
    );
}
