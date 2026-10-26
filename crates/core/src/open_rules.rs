//! How a `file` node's target is opened (`↗`/`o`), per client: the
//! `[tui.open]` / `[web.open]` tables of `.meshfox/config.toml`, with `[open]`
//! as the fallback for every client.
//!
//! ```toml
//! [tui.open]
//! "xls,xlsx,*.ods" = "sc-im {file}"   # TUI hands over its terminal
//! pdf              = "zathura {file} &" # trailing `&`: detached, TUI stays up
//! "*"              = "system"
//! ```
//!
//! A key is a comma-separated list of patterns, matched case-insensitively
//! against the file *name*: a pattern without `*`/`?` is an extension
//! (`ods`, `tar.gz`), one with them is a glob (`report-*.csv`), and `*` alone
//! is the catch-all. A value is `"system"` (the OS default application),
//! `false` (refuse), or a command template split shell-style without a shell:
//! `{file}`, `{dir}` and `{name}` are replaced inside each word, and a
//! template without `{file}` gets the file appended. A trailing `&` detaches
//! the command; otherwise a command runs in the client's own way — the TUI
//! gives it the terminal, the web client has no terminal and always detaches.
//!
//! When several entries match, the most specific wins: extension over glob
//! over `*`, then the longer pattern, then the client's own table over
//! `[open]`, then the nearer config file. Two different keys equally specific
//! in the same table are an error rather than a silent pick.

use std::path::Path;

/// The client asking to open a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Client {
    Tui,
    Web,
}

impl Client {
    fn table(self) -> &'static str {
        match self {
            Client::Tui => "tui",
            Client::Web => "web",
        }
    }
}

/// How a command is run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// On the client's terminal, waited for.
    Tty,
    /// In the background, no terminal, not waited for.
    Detach,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Whatever the OS associates with the file.
    System,
    Exec {
        argv: Vec<String>,
        mode: Mode,
    },
}

/// The action for `file`, from the configuration around `canvas_root`.
pub fn for_file(canvas_root: &Path, client: Client, file: &Path) -> Result<Action, String> {
    resolve(&crate::config::load_layers(canvas_root), client, file)
}

/// `for_file` over already-loaded config files, farthest first.
pub fn resolve(layers: &[toml::Table], client: Client, file: &Path) -> Result<Action, String> {
    let name = file
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let lower = name.to_lowercase();
    let mut best: Option<Candidate> = None;
    for (layer, table) in layers.iter().enumerate() {
        for own in [false, true] {
            let rules = if own {
                table
                    .get(client.table())
                    .and_then(|t| t.get("open"))
                    .map(|v| (v, format!("[{}.open]", client.table())))
            } else {
                table.get("open").map(|v| (v, "[open]".to_string()))
            };
            let Some((rules, where_)) = rules else {
                continue;
            };
            let rules = rules
                .as_table()
                .ok_or_else(|| format!("{where_} must be a table"))?;
            for (key, value) in rules {
                for token in key.split(',').map(str::trim).filter(|t| !t.is_empty()) {
                    let Some(kind) = matches(token, &lower)? else {
                        continue;
                    };
                    let candidate = Candidate {
                        rank: (kind, token.len(), own, layer),
                        where_: where_.clone(),
                        key: key.clone(),
                        value,
                    };
                    match &best {
                        Some(b) if b.rank > candidate.rank => {}
                        Some(b) if b.rank == candidate.rank && b.key != candidate.key => {
                            return Err(format!(
                                "{where_} has two equally specific entries for {name}: \
                                 {:?} and {:?}",
                                b.key, candidate.key
                            ));
                        }
                        _ => best = Some(candidate),
                    }
                }
            }
        }
    }
    let Some(best) = best else {
        return Ok(Action::System);
    };
    action(client, file, &name, &best)
}

struct Candidate<'a> {
    /// Compared as a whole; the largest wins.
    rank: (Kind, usize, bool, usize),
    where_: String,
    key: String,
    value: &'a toml::Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Kind {
    CatchAll,
    Glob,
    Extension,
}

/// Does `token` match the lowercased file name, and as which kind of entry.
fn matches(token: &str, lower_name: &str) -> Result<Option<Kind>, String> {
    if token == "*" {
        return Ok(Some(Kind::CatchAll));
    }
    if token.contains(['*', '?']) {
        let glob = globset::GlobBuilder::new(token)
            .case_insensitive(true)
            .build()
            .map_err(|e| format!("bad pattern {token:?}: {e}"))?;
        return Ok(glob
            .compile_matcher()
            .is_match(lower_name)
            .then_some(Kind::Glob));
    }
    let ext = token.trim_start_matches('.').to_lowercase();
    Ok((!ext.is_empty() && lower_name.ends_with(&format!(".{ext}"))).then_some(Kind::Extension))
}

fn action(client: Client, file: &Path, name: &str, best: &Candidate<'_>) -> Result<Action, String> {
    let Candidate {
        where_, key, value, ..
    } = best;
    let template = match value {
        toml::Value::Boolean(false) => {
            return Err(format!("opening {name} is disabled by {where_} {key:?}"));
        }
        toml::Value::String(s) => s.trim(),
        _ => {
            return Err(format!(
                "{where_} {key:?} must be a command string, \"system\" or false"
            ));
        }
    };
    if template == "system" {
        return Ok(Action::System);
    }
    let (template, detached) = match template.strip_suffix('&') {
        Some(rest) => (rest.trim_end(), true),
        None => (template, false),
    };
    let words = split_words(template).map_err(|e| format!("{where_} {key:?}: {e}"))?;
    if words.is_empty() {
        return Err(format!("{where_} {key:?}: empty command"));
    }
    let file_str = file.to_string_lossy();
    let dir = file
        .parent()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    let fill = |w: &str| {
        w.replace("{file}", &file_str)
            .replace("{dir}", &dir)
            .replace("{name}", name)
    };
    let mut argv: Vec<String> = words.iter().map(|w| fill(w)).collect();
    if !words.iter().any(|w| w.contains("{file}")) {
        argv.push(file_str.into_owned());
    }
    let mode = if detached || client == Client::Web {
        Mode::Detach
    } else {
        Mode::Tty
    };
    Ok(Action::Exec { argv, mode })
}

/// Splits on whitespace with shell-style quoting: `'…'` is literal, `"…"`
/// honours `\"` and `\\`, and a backslash outside quotes escapes the next
/// character. No expansion of any kind.
pub fn split_words(s: &str) -> Result<Vec<String>, String> {
    let mut words = Vec::new();
    let mut cur = String::new();
    let mut in_word = false;
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        match c {
            c if c.is_whitespace() => {
                if in_word {
                    words.push(std::mem::take(&mut cur));
                    in_word = false;
                }
            }
            '\'' => {
                in_word = true;
                loop {
                    match chars.next() {
                        Some('\'') => break,
                        Some(c) => cur.push(c),
                        None => return Err("unterminated ' quote".into()),
                    }
                }
            }
            '"' => {
                in_word = true;
                loop {
                    match chars.next() {
                        Some('"') => break,
                        Some('\\') => match chars.next() {
                            Some(c @ ('"' | '\\')) => cur.push(c),
                            Some(c) => {
                                cur.push('\\');
                                cur.push(c);
                            }
                            None => return Err("unterminated \" quote".into()),
                        },
                        Some(c) => cur.push(c),
                        None => return Err("unterminated \" quote".into()),
                    }
                }
            }
            '\\' => {
                in_word = true;
                match chars.next() {
                    Some(c) => cur.push(c),
                    None => return Err("trailing backslash".into()),
                }
            }
            c => {
                in_word = true;
                cur.push(c);
            }
        }
    }
    if in_word {
        words.push(cur);
    }
    Ok(words)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(config: &str, client: Client, file: &str) -> Result<Action, String> {
        let layers: Vec<toml::Table> = config
            .split("\n---\n")
            .map(|c| c.parse().unwrap())
            .collect();
        resolve(&layers, client, Path::new(file))
    }

    fn exec(argv: &[&str], mode: Mode) -> Action {
        Action::Exec {
            argv: argv.iter().map(|s| s.to_string()).collect(),
            mode,
        }
    }

    #[test]
    fn no_rule_means_the_system_default() {
        assert_eq!(run("", Client::Tui, "/d/a.ods"), Ok(Action::System));
        let cfg = "[tui.open]\ncsv = \"vd\"\n";
        assert_eq!(run(cfg, Client::Tui, "/d/a.ods"), Ok(Action::System));
    }

    #[test]
    fn a_key_lists_extensions_and_globs_and_matches_case_insensitively() {
        let cfg = "[tui.open]\n\"xls,xlsx,*.ODS\" = \"sc-im {file}\"\n";
        for f in ["/d/a.xls", "/d/a.XLSX", "/d/a.ods", "/d/dir.v2/a.Ods"] {
            assert_eq!(
                run(cfg, Client::Tui, f),
                Ok(exec(&["sc-im", f], Mode::Tty)),
                "{f}"
            );
        }
        assert_eq!(run(cfg, Client::Tui, "/d/xls"), Ok(Action::System));
        assert_eq!(run(cfg, Client::Tui, "/d/a.xlsm"), Ok(Action::System));
    }

    #[test]
    fn a_dotted_extension_matches_as_a_suffix() {
        let cfg = "[tui.open]\n\"tar.gz\" = \"tar-viewer\"\ngz = \"zless\"\n";
        assert_eq!(
            run(cfg, Client::Tui, "/d/a.tar.gz"),
            Ok(exec(&["tar-viewer", "/d/a.tar.gz"], Mode::Tty))
        );
        assert_eq!(
            run(cfg, Client::Tui, "/d/a.gz"),
            Ok(exec(&["zless", "/d/a.gz"], Mode::Tty))
        );
    }

    #[test]
    fn extension_beats_glob_beats_catch_all_whatever_the_order() {
        let cfg = "[tui.open]\n\"*\" = \"any\"\n\"*.csv\" = \"glob\"\ncsv = \"ext\"\n";
        assert_eq!(
            run(cfg, Client::Tui, "/d/a.csv"),
            Ok(exec(&["ext", "/d/a.csv"], Mode::Tty))
        );
        assert_eq!(
            run(cfg, Client::Tui, "/d/a.txt"),
            Ok(exec(&["any", "/d/a.txt"], Mode::Tty))
        );
        let cfg = "[tui.open]\n\"*\" = \"any\"\n\"data-*.bin\" = \"glob\"\n";
        assert_eq!(
            run(cfg, Client::Tui, "/d/data-1.bin"),
            Ok(exec(&["glob", "/d/data-1.bin"], Mode::Tty))
        );
    }

    #[test]
    fn the_client_table_beats_open_and_a_nearer_file_beats_a_farther_one() {
        let cfg = "[open]\nods = \"generic\"\n[tui.open]\nods = \"own\"\n";
        assert_eq!(
            run(cfg, Client::Tui, "/d/a.ods"),
            Ok(exec(&["own", "/d/a.ods"], Mode::Tty))
        );
        assert_eq!(
            run(cfg, Client::Web, "/d/a.ods"),
            Ok(exec(&["generic", "/d/a.ods"], Mode::Detach))
        );
        let cfg = "[tui.open]\n\"ods,xls\" = \"far\"\n---\n[tui.open]\nods = \"near\"\n";
        assert_eq!(
            run(cfg, Client::Tui, "/d/a.ods"),
            Ok(exec(&["near", "/d/a.ods"], Mode::Tty))
        );
        assert_eq!(
            run(cfg, Client::Tui, "/d/a.xls"),
            Ok(exec(&["far", "/d/a.xls"], Mode::Tty))
        );
        // A nearer catch-all does not hide a farther, more specific entry.
        let cfg = "[tui.open]\nods = \"far\"\n---\n[tui.open]\n\"*\" = \"system\"\n";
        assert_eq!(
            run(cfg, Client::Tui, "/d/a.ods"),
            Ok(exec(&["far", "/d/a.ods"], Mode::Tty))
        );
    }

    #[test]
    fn two_equally_specific_keys_in_one_table_are_an_error() {
        let cfg = "[tui.open]\n\"ods,csv\" = \"a\"\n\"ods,tsv\" = \"b\"\n";
        let err = run(cfg, Client::Tui, "/d/a.ods").unwrap_err();
        assert!(err.contains("equally specific"), "{err}");
        assert!(run(cfg, Client::Tui, "/d/a.csv").is_ok());
    }

    #[test]
    fn a_trailing_ampersand_detaches_and_the_web_always_does() {
        let cfg = "[tui.open]\npdf = \"zathura {file} &\"\nods = \"sc-im\"\n";
        assert_eq!(
            run(cfg, Client::Tui, "/d/a.pdf"),
            Ok(exec(&["zathura", "/d/a.pdf"], Mode::Detach))
        );
        assert_eq!(
            run(cfg, Client::Tui, "/d/a.ods"),
            Ok(exec(&["sc-im", "/d/a.ods"], Mode::Tty))
        );
        let cfg = "[web.open]\nods = \"libreoffice\"\n";
        assert_eq!(
            run(cfg, Client::Web, "/d/a.ods"),
            Ok(exec(&["libreoffice", "/d/a.ods"], Mode::Detach))
        );
    }

    #[test]
    fn placeholders_fill_inside_words_and_a_path_with_spaces_stays_whole() {
        let cfg = "[tui.open]\ntxt = \"ed --dir={dir} --name={name} '{file}'\"\n";
        assert_eq!(
            run(cfg, Client::Tui, "/a b/n.txt"),
            Ok(exec(
                &["ed", "--dir=/a b", "--name=n.txt", "/a b/n.txt"],
                Mode::Tty
            ))
        );
    }

    #[test]
    fn system_false_and_bad_values() {
        let cfg = "[tui.open]\nods = \"system\"\nexe = false\ntxt = 3\nbad = \"a 'b\"\n";
        assert_eq!(run(cfg, Client::Tui, "/d/a.ods"), Ok(Action::System));
        assert!(run(cfg, Client::Tui, "/d/a.exe")
            .unwrap_err()
            .contains("disabled"));
        assert!(run(cfg, Client::Tui, "/d/a.txt")
            .unwrap_err()
            .contains("must be a command string"));
        assert!(run(cfg, Client::Tui, "/d/a.bad")
            .unwrap_err()
            .contains("unterminated"));
        assert!(run("open = 1", Client::Tui, "/d/a.ods")
            .unwrap_err()
            .contains("must be a table"));
    }

    #[test]
    fn quoting_follows_the_shell_without_expanding_anything() {
        assert_eq!(
            split_words(r#"a "b \"c\" $HOME" 'd e' f\ g ''"#).unwrap(),
            ["a", "b \"c\" $HOME", "d e", "f g", ""]
        );
        assert!(split_words("a 'b").is_err());
        assert!(split_words("a \"b").is_err());
        assert!(split_words("a\\").is_err());
    }
}
