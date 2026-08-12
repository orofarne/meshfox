//! Terminal prompting for `meshfox:var` declarations — the interactive
//! side of `meshfox_core::vars`'s resolution precedence, used by both
//! `configure` and `run`'s lazy "ask only what's missing" path.

use meshfox_core::{VarDecl, VarType};
use std::io::{self, IsTerminal, Write};

/// Whether stdin is an interactive terminal — `configure`/`run` refuse to
/// prompt (erroring instead) when it isn't, rather than hang reading a
/// pipe/CI log that will never answer.
pub fn stdin_is_tty() -> bool {
    io::stdin().is_terminal()
}

/// Prompts on the terminal for `decl`'s value. `current` is shown as the
/// existing value — pressing Enter keeps it; pass `None` when there's
/// nothing to fall back to. `run`'s lazy-missing case passes `decl.default`
/// here (which may itself be `None`): a plain declaration only reaches
/// this function empty-handed when override/env/cache/default all came up
/// empty, but a `required` one reaches it whenever nothing but its own
/// `default` would have resolved it — passing that default through as
/// `current` is what lets it be confirmed with a bare Enter instead of
/// retyping it. A `secret` declaration ignores `current` entirely and
/// reads without echoing the input.
pub fn ask(decl: &VarDecl, current: Option<&str>) -> io::Result<String> {
    if decl.secret {
        return ask_secret(decl);
    }
    loop {
        print_prompt(decl, current)?;
        let input = read_line()?;

        if input.is_empty() {
            match current {
                Some(c) => return Ok(c.to_string()),
                None => {
                    println!("A value is required.");
                    continue;
                }
            }
        }

        match decl.var_type {
            VarType::Select => match resolve_select(decl, &input) {
                Some(choice) => return Ok(choice),
                None => {
                    println!("Please enter one of: {}", decl.choices.join(", "));
                    continue;
                }
            },
            VarType::Bool => match input.to_ascii_lowercase().as_str() {
                "y" | "yes" | "true" => return Ok("true".to_string()),
                "n" | "no" | "false" => return Ok("false".to_string()),
                _ => {
                    println!("Please answer y or n.");
                    continue;
                }
            },
            VarType::Int => match meshfox_core::validate_value(decl, &input) {
                Ok(()) => return Ok(input),
                Err(e) => {
                    println!("{e}");
                    continue;
                }
            },
            VarType::String => return Ok(input),
        }
    }
}

fn print_prompt(decl: &VarDecl, current: Option<&str>) -> io::Result<()> {
    if decl.var_type == VarType::Select {
        println!("{}", decl.prompt);
        for (i, choice) in decl.choices.iter().enumerate() {
            let marker = if current == Some(choice.as_str()) { "*" } else { " " };
            println!("  {marker} {}) {choice}", i + 1);
        }
        print!("> ");
    } else if decl.var_type == VarType::Bool {
        match current {
            Some(c) => print!("{} (y/n) [{c}]: ", decl.prompt),
            None => print!("{} (y/n): ", decl.prompt),
        }
    } else {
        match current {
            Some(c) => print!("{} [{c}]: ", decl.prompt),
            None => print!("{}: ", decl.prompt),
        }
    }
    io::stdout().flush()
}

fn resolve_select(decl: &VarDecl, input: &str) -> Option<String> {
    if let Ok(n) = input.parse::<usize>() {
        if n >= 1 && n <= decl.choices.len() {
            return Some(decl.choices[n - 1].clone());
        }
    }
    decl.choices.iter().find(|c| c.as_str() == input).cloned()
}

fn read_line() -> io::Result<String> {
    let mut line = String::new();
    io::stdin().read_line(&mut line)?;
    Ok(line.trim().to_string())
}

fn ask_secret(decl: &VarDecl) -> io::Result<String> {
    rpassword::prompt_password(format!("{}: ", decl.prompt))
}
