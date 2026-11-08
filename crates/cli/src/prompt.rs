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
    let _waiting = crate::terminal_status::waiting(&decl.name);
    if decl.secret {
        return ask_secret(decl);
    }
    // A `select` whose choices could not be computed has nothing to pick
    // from: keep what it has rather than loop on an unanswerable question.
    if let Some(source) = meshfox_core::pending_choices_source(decl) {
        return match current {
            Some(c) => Ok(c.to_string()),
            None => Err(io::Error::other(format!(
                "{}: no choices are available (they come from {source}, which has no value)",
                decl.name
            ))),
        };
    }
    // The entries a select shows right now (indices into `decl.choices`):
    // everything at first, then whatever the last typed filter left.
    let mut shown: Vec<usize> = (0..decl.choices.len()).collect();
    loop {
        if decl.var_type == VarType::Select {
            print_select(decl, &shown, current)?;
        } else {
            print_prompt(decl, current)?;
        }
        let input = read_line()?;

        if input.is_empty() {
            match current {
                Some(c) => return Ok(c.to_string()),
                // A `String` declaration accepts an empty value same as any
                // other (`validate_value` puts no constraint on it at all) —
                // an empty answer here is a deliberate "leave it blank", not
                // "didn't type anything yet". Every other type still needs a
                // real answer (`Int`/`Bool` can't parse "", `Select` has no
                // empty choice).
                None if decl.var_type == VarType::String => return Ok(String::new()),
                None => {
                    println!("A value is required.");
                    continue;
                }
            }
        }

        match decl.var_type {
            VarType::Select => match select_step(&decl.choices, &shown, &input) {
                SelectStep::Chosen(choice) => return Ok(choice),
                SelectStep::Narrowed(next) => {
                    shown = next;
                    continue;
                }
                SelectStep::NoMatch => {
                    println!("Nothing matches {input:?}; type part of a name, or a number.");
                    shown = (0..decl.choices.len()).collect();
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
            VarType::List => match list_from_prompt(decl, &input) {
                Ok(stored) => return Ok(stored),
                Err(e) => {
                    println!("{e}");
                    continue;
                }
            },
            VarType::String => return Ok(input),
            VarType::Date | VarType::DateTime => {
                let normalized = if decl.var_type == VarType::Date {
                    meshfox_core::datevars::normalize_date_input(&input)
                } else {
                    meshfox_core::datevars::normalize_datetime_input(&input)
                };
                match normalized {
                    Ok(v) => {
                        if v != input {
                            println!("→ {v}");
                        }
                        return Ok(v);
                    }
                    Err(e) => {
                        println!("{e}");
                        continue;
                    }
                }
            }
            VarType::Dir | VarType::File => match meshfox_core::validate_value(decl, &input) {
                Ok(()) => return Ok(input),
                Err(e) => {
                    println!("{e}");
                    continue;
                }
            },
        }
    }
}

/// How many choices a select lists in full; past that it asks for a filter.
const LIST_LIMIT: usize = 15;

fn print_select(decl: &VarDecl, shown: &[usize], current: Option<&str>) -> io::Result<()> {
    println!("{}", decl.prompt);
    let total = decl.choices.len();
    let narrowed = shown.len() < total;
    for (n, &i) in shown.iter().take(LIST_LIMIT).enumerate() {
        let choice = &decl.choices[i];
        let marker = if current == Some(choice.as_str()) {
            "*"
        } else {
            " "
        };
        println!("  {marker} {}) {choice}", n + 1);
    }
    if shown.len() > LIST_LIMIT {
        println!(
            "    ... and {} more: type part of a name to filter",
            shown.len() - LIST_LIMIT
        );
    } else if narrowed {
        println!("    ({} of {total} match)", shown.len());
    }
    if total > LIST_LIMIT {
        print!("filter, name or number > ");
    } else {
        print!("> ");
    }
    io::stdout().flush()
}

fn print_prompt(decl: &VarDecl, current: Option<&str>) -> io::Result<()> {
    if decl.var_type == VarType::List {
        if !decl.choices.is_empty() {
            for (i, choice) in decl.choices.iter().enumerate() {
                println!("  {}) {choice}", i + 1);
            }
        }
        let hint = if decl.choices.is_empty() {
            " (comma-separated, or a JSON array)"
        } else {
            " (numbers or names, comma-separated)"
        };
        match current {
            Some(c) => print!(
                "{}{hint} [{}]: ",
                decl.prompt,
                meshfox_core::listvars::to_user_text(c)
            ),
            None => print!("{}{hint}: ", decl.prompt),
        }
    } else if decl.var_type == VarType::Bool {
        match current {
            Some(c) => print!("{} (y/n) [{c}]: ", decl.prompt),
            None => print!("{} (y/n): ", decl.prompt),
        }
    } else {
        let hint = match decl.var_type {
            VarType::Date => " (YYYY-MM-DD)",
            VarType::DateTime => " (YYYY-MM-DD HH:MM[:SS][±HH:MM])",
            _ => "",
        };
        match current {
            Some(c) => print!("{}{hint} [{c}]: ", decl.prompt),
            None => print!("{}{hint}: ", decl.prompt),
        }
    }
    io::stdout().flush()
}

/// What one line typed at a select's prompt does.
#[derive(Debug, PartialEq, Eq)]
enum SelectStep {
    /// A choice was picked.
    Chosen(String),
    /// The text narrowed the list to these entries (indices into the choices).
    Narrowed(Vec<usize>),
    /// Nothing contains the text.
    NoMatch,
}

/// A number picks that row of the list as shown; an exact name picks it; any
/// other text is a filter (`meshfox_core::filter_choices`, the rule the TUI
/// picker and the web combobox use) that picks the only match, or narrows the
/// list to the matches.
fn select_step(choices: &[String], shown: &[usize], input: &str) -> SelectStep {
    if let Ok(n) = input.parse::<usize>() {
        if let Some(&i) = n.checked_sub(1).and_then(|n| shown.get(n)) {
            return SelectStep::Chosen(choices[i].clone());
        }
    }
    if let Some(choice) = choices.iter().find(|c| c.as_str() == input) {
        return SelectStep::Chosen(choice.clone());
    }
    let matches = meshfox_core::filter_choices(choices, input);
    match matches.as_slice() {
        [] => SelectStep::NoMatch,
        [only] => SelectStep::Chosen(choices[*only].clone()),
        _ => SelectStep::Narrowed(matches),
    }
}

/// What one line typed at a list's prompt means: a JSON array, or elements
/// separated by commas — for a list with `choices`, each one a number from the
/// shown menu or the name itself. Returns the stored JSON form, validated
/// against the declaration.
fn list_from_prompt(decl: &VarDecl, input: &str) -> Result<String, String> {
    let stored = if decl.choices.is_empty() || input.trim_start().starts_with('[') {
        meshfox_core::listvars::from_user_input(input)
            .map_err(|e| format!("{:?} {e}", decl.name))?
    } else {
        let mut items = Vec::new();
        for token in input.split(',').map(str::trim).filter(|t| !t.is_empty()) {
            let pick = token
                .parse::<usize>()
                .ok()
                .and_then(|n| n.checked_sub(1))
                .and_then(|i| decl.choices.get(i))
                .or_else(|| decl.choices.iter().find(|c| c.as_str() == token))
                .ok_or_else(|| format!("{token:?} is not one of {}", decl.choices.join(", ")))?;
            if !items.contains(pick) {
                items.push(pick.clone());
            }
        }
        meshfox_core::listvars::to_json(&items)
    };
    meshfox_core::validate_value(decl, &stored)?;
    Ok(stored)
}

fn read_line() -> io::Result<String> {
    let mut line = String::new();
    io::stdin().read_line(&mut line)?;
    Ok(line.trim().to_string())
}

fn ask_secret(decl: &VarDecl) -> io::Result<String> {
    rpassword::prompt_password(format!("{}: ", decl.prompt))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list_decl(choices: &[&str], of: Option<VarType>) -> VarDecl {
        VarDecl {
            of,
            name: "REGIONS".into(),
            var_type: VarType::List,
            prompt: "Regions".into(),
            default: None,
            choices: choices.iter().map(|s| s.to_string()).collect(),
            secret: false,
            required: false,
            from: None,
            session: false,
            default_var: None,
            choices_var: None,
            file_mode: None,
            ext: Vec::new(),
        }
    }

    #[test]
    fn a_list_prompt_takes_numbers_names_or_json() {
        let with = list_decl(&["us", "eu", "asia"], None);
        assert_eq!(list_from_prompt(&with, "1, 3").unwrap(), r#"["us","asia"]"#);
        assert_eq!(
            list_from_prompt(&with, "eu,us,eu").unwrap(),
            r#"["eu","us"]"#
        );
        assert_eq!(list_from_prompt(&with, r#"["eu"]"#).unwrap(), r#"["eu"]"#);
        assert_eq!(list_from_prompt(&with, "").unwrap(), "[]");
        let err = list_from_prompt(&with, "us, mars").unwrap_err();
        assert!(
            err.contains("mars") && err.contains("us, eu, asia"),
            "{err}"
        );
        assert!(list_from_prompt(&with, r#"["mars"]"#).is_err());
        let free = list_decl(&[], Some(VarType::Int));
        assert_eq!(
            list_from_prompt(&free, "3, 1,2").unwrap(),
            r#"["3","1","2"]"#
        );
        assert!(list_from_prompt(&free, "3, x").is_err());
    }

    fn fonts() -> Vec<String> {
        [
            "Arial",
            "Arial Black",
            "Helvetica",
            "Helvetica Neue",
            "Noto Sans",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect()
    }

    fn all(n: usize) -> Vec<usize> {
        (0..n).collect()
    }

    #[test]
    fn a_number_picks_the_row_as_shown() {
        let c = fonts();
        assert_eq!(
            select_step(&c, &all(5), "3"),
            SelectStep::Chosen("Helvetica".into())
        );
        // after narrowing, row 2 is the second match, not the second choice
        assert_eq!(
            select_step(&c, &[2, 3], "2"),
            SelectStep::Chosen("Helvetica Neue".into())
        );
        assert_eq!(select_step(&c, &all(5), "9"), SelectStep::NoMatch);
    }

    #[test]
    fn an_exact_name_wins_over_a_longer_match() {
        // "Arial" also matches "Arial Black" as a filter, but it is a choice itself
        assert_eq!(
            select_step(&fonts(), &all(5), "Arial"),
            SelectStep::Chosen("Arial".into())
        );
    }

    #[test]
    fn other_text_filters_and_a_single_match_is_picked() {
        let c = fonts();
        assert_eq!(
            select_step(&c, &all(5), "HELV"),
            SelectStep::Narrowed(vec![2, 3])
        );
        assert_eq!(
            select_step(&c, &all(5), "neue"),
            SelectStep::Chosen("Helvetica Neue".into())
        );
        assert_eq!(select_step(&c, &all(5), "zzz"), SelectStep::NoMatch);
    }

    #[test]
    fn a_select_with_pending_choices_keeps_its_value_or_errors() {
        let mut decl = VarDecl {
            of: None,
            name: "FONT".into(),
            var_type: VarType::Select,
            prompt: "Font".into(),
            default: None,
            choices: Vec::new(),
            secret: false,
            required: false,
            from: None,
            session: false,
            default_var: None,
            choices_var: Some("FONT_LIST".into()),
            file_mode: None,
            ext: Vec::new(),
        };
        assert_eq!(ask(&decl, Some("Arial")).unwrap(), "Arial");
        assert!(ask(&decl, None)
            .unwrap_err()
            .to_string()
            .contains("FONT_LIST"));
        decl.choices_var = None;
        assert!(meshfox_core::pending_choices_source(&decl).is_none());
    }
}
