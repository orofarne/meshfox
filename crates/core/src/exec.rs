//! Parsing/resolving an `interpreter="..."` fence attribute, and deciding
//! what to actually spawn for a block's code — the shared logic every real
//! executor (`crate::server::stream_exec` and `pty_exec`'s async/streaming
//! spawns; the CLI/TUI's own `tty`-handoff spawns) builds its own
//! `Command`/`CommandBuilder` around, rather than owning this parsing
//! itself. Nothing in this module runs a process on its own — that's
//! `stream_exec`'s job (see its own module doc for why: this crate stays
//! `tokio`-free).

use std::io;

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

/// True if `word` is a bare `$NAME` token in its entirety — a whole shell
/// word, not a prefix/suffix of a longer one (`$PYTHON` matches,
/// `/opt/$PYTHON/bin` and `$PYTHON-3.11` don't) — see
/// `interpreter_var_refs`/`resolve_interpreter` for why only this shape is
/// recognized.
fn whole_token_var_name(word: &str) -> Option<&str> {
    let name = word.strip_prefix('$')?;
    let mut chars = name.chars();
    let first_ok = chars.next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_');
    if !first_ok || !chars.all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return None;
    }
    Some(name)
}

/// Every declared-variable name an `interpreter=` attribute's raw value
/// references as a whole shell word (`interpreter="$PYTHON -u"` ->
/// `["PYTHON"]`), in first-occurrence order, deduplicated — what a caller
/// needs to have resolved (and, if a `from=`-computed one, already run the
/// source block for — see `crate::deps::visit`) before calling
/// `resolve_interpreter`.
///
/// Unlike `env=`'s entries (always a name reference, so a leading `$` is
/// purely cosmetic there — see `crate::fence::strip_dollar`'s own doc
/// comment), `interpreter=` mixes literal tokens (`python3`, `-u`) with
/// possibly-a-reference ones, so the `$` can't be optional here: a bare
/// `PYTHON` token is a literal program/argument, never treated as a
/// variable reference. Only a *whole* `$NAME` word counts — a reference
/// embedded partway through a token (`/opt/$NAME/bin`) is deliberately not
/// recognized, so there's never any ambiguity about where a name starts
/// and ends. Malformed shell-word syntax (unterminated quotes, same as
/// `split_interpreter` itself would reject) yields no references at all —
/// `resolve_command`'s own error handling is what surfaces that to a user,
/// not this.
pub fn interpreter_var_refs(spec: &str) -> Vec<String> {
    let Some(words) = shlex::split(spec) else {
        return Vec::new();
    };
    let mut seen = std::collections::HashSet::new();
    let mut names = Vec::new();
    for word in &words {
        if let Some(name) = whole_token_var_name(word) {
            if seen.insert(name.to_string()) {
                names.push(name.to_string());
            }
        }
    }
    names
}

/// Substitutes every whole-token `$NAME` reference in `spec` (see
/// `interpreter_var_refs`) with its value from `resolved_vars`, leaving
/// every other token exactly as written — the runtime counterpart callers
/// use right before actually spawning anything, once every name
/// `interpreter_var_refs` reported has a real value (from ordinary
/// `meshfox:var` resolution — prompted, `--set`, cached, `from=`-computed,
/// whatever the usual precedence supplies). Substitution happens at the
/// shell-word level, not by splicing text into the raw string and
/// re-parsing it — so a resolved value containing spaces (an unusual but
/// legal interpreter path) becomes exactly one argument, never
/// word-split. A name `interpreter_var_refs` would have reported but that's
/// missing from `resolved_vars` is a caller bug (every such name is
/// supposed to already be resolved by this point, same contract `env=`
/// resolution already has) — degrades by leaving that one token literal
/// (`$NAME`) rather than panicking, the same "don't crash on an
/// unrecognized/unresolved case" fallback used throughout this format.
pub fn resolve_interpreter(spec: &str, resolved_vars: &std::collections::HashMap<String, String>) -> String {
    let Some(words) = shlex::split(spec) else {
        return spec.to_string();
    };
    let substituted: Vec<String> = words
        .into_iter()
        .map(|word| match whole_token_var_name(&word) {
            Some(name) => resolved_vars.get(name).cloned().unwrap_or(word),
            None => word,
        })
        .collect();
    shlex::try_join(substituted.iter().map(String::as_str)).unwrap_or(spec.to_string())
}

/// What to actually spawn for `code` given an optional `interpreter=`
/// value — the shared decision behind every "run this block" path (each
/// builds its own `Command`/`CommandBuilder` around the result; `crate`
/// doesn't depend on `tokio` or `portable-pty` itself, so it can only hand
/// back "what to run", not run it). `interpreter = None` resolves to the
/// implicit `bash -c code`; `Some(spec)` splits `spec` (`split_interpreter`)
/// and writes `code` to a fresh temp file, the same shebang-script shape
/// every real spawner uses for an explicit `interpreter=`.
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

/// Fence languages meshfox actually knows how to execute — what every real
/// spawner (`stream_exec::supports`) checks before treating a fence with no
/// `interpreter=` as runnable at all. Exposed here (not just inlined at
/// each call site) so fence scanning (`crate::fence`) can decide whether a
/// fence is eligible to be treated as runnable — named or implicitly named
/// — before anything ever tries to execute it. This is what keeps an
/// ordinary Markdown doc's non-`bash`/`sh` example fences (a `yaml` config
/// sample, a `json` snippet, ...) from being mistaken for "the" runnable
/// block in a node that has no real meshfox structure of its own.
pub fn is_supported_lang(lang: &str) -> bool {
    matches!(lang, "bash" | "sh")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_supported_lang_matches_known_languages() {
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
    fn interpreter_var_refs_finds_a_whole_token_reference() {
        assert_eq!(interpreter_var_refs("$PYTHON -u"), vec!["PYTHON".to_string()]);
    }

    #[test]
    fn interpreter_var_refs_ignores_a_mid_token_reference() {
        assert!(interpreter_var_refs("/opt/$PYTHON/bin").is_empty());
        assert!(interpreter_var_refs("$PYTHON-3.11").is_empty());
    }

    #[test]
    fn interpreter_var_refs_ignores_a_bare_word_with_no_dollar() {
        assert!(interpreter_var_refs("python3 -u").is_empty());
    }

    #[test]
    fn interpreter_var_refs_dedupes_and_preserves_first_occurrence_order() {
        assert_eq!(
            interpreter_var_refs("$PYTHON $FLAGS $PYTHON"),
            vec!["PYTHON".to_string(), "FLAGS".to_string()]
        );
    }

    #[test]
    fn interpreter_var_refs_is_empty_for_malformed_shell_syntax() {
        assert!(interpreter_var_refs(r#"unterminated ""#).is_empty());
    }

    #[test]
    fn resolve_interpreter_substitutes_a_whole_token_reference() {
        let mut vars = std::collections::HashMap::new();
        vars.insert("PYTHON".to_string(), ".venv/bin/python3".to_string());
        assert_eq!(resolve_interpreter("$PYTHON -u", &vars), ".venv/bin/python3 -u");
    }

    #[test]
    fn resolve_interpreter_leaves_literal_tokens_untouched() {
        let vars = std::collections::HashMap::new();
        assert_eq!(resolve_interpreter("python3 -u", &vars), "python3 -u");
    }

    #[test]
    fn resolve_interpreter_keeps_a_value_containing_spaces_as_one_argument() {
        let mut vars = std::collections::HashMap::new();
        vars.insert("PYTHON".to_string(), "/opt/my python/bin/python3".to_string());
        let resolved = resolve_interpreter("$PYTHON -u", &vars);
        assert_eq!(
            split_interpreter(&resolved),
            Some((
                "/opt/my python/bin/python3".to_string(),
                vec!["-u".to_string()]
            ))
        );
    }

    #[test]
    fn resolve_interpreter_leaves_an_unresolved_reference_literal_rather_than_panicking() {
        // Re-joining via `shlex` can legitimately re-quote an unresolved
        // `$NAME` token (it contains a shell-special `$`) rather than
        // returning it byte-for-byte unchanged — round-trip through
        // `split_interpreter` instead of comparing raw strings, same as
        // `resolve_interpreter_keeps_a_value_containing_spaces_as_one_argument`
        // already does.
        let vars = std::collections::HashMap::new();
        let resolved = resolve_interpreter("$PYTHON -u", &vars);
        assert_eq!(
            split_interpreter(&resolved),
            Some(("$PYTHON".to_string(), vec!["-u".to_string()]))
        );
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

}
