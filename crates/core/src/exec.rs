//! Parsing/resolving an `interpreter="..."` fence attribute, and deciding
//! what to actually spawn for a block's code — the shared logic every real
//! executor (`crate::server::stream_exec` and `pty_exec`'s async/streaming
//! spawns; the CLI/TUI's own `tty`-handoff spawns) builds its own
//! `Command`/`CommandBuilder` around, rather than owning this parsing
//! itself. Nothing in this module runs a process on its own — that's
//! `stream_exec`'s job (see its own module doc for why: this crate stays
//! `tokio`-free).

use crate::block_refs::whole_token_var_name;
pub use crate::block_refs::{interpreter_var_refs, is_supported_lang, BUTTON_LANG, FORM_LANG};
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
pub fn resolve_interpreter(
    spec: &str,
    resolved_vars: &std::collections::HashMap<String, String>,
) -> String {
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
    /// `MESHFOX_*` env vars an `@name` builtin (see
    /// `crate::builtin_interpreter`) needs on top of whatever the caller
    /// already builds for the block — empty for a plain, non-`@`
    /// `interpreter=`. The caller merges these into its own env before
    /// spawning `program`, same as `stream_exec::spawn_interpreter` does
    /// for the non-`tty` path.
    pub extra_envs: Vec<(String, String)>,
}

/// `lang`/`cwd`/`origin`/`env_names` are only consulted for an `@name`
/// builtin spec (see `crate::builtin_interpreter::resolve_with_env`) —
/// `cwd` as the local config root, `origin`'s canvas path to key
/// `@python_venv`'s own venv directory, `env_names` (the block's own `env=`
/// locals) for `@agent`'s prompt interpolation — with one exception: an
/// `origin` that carries `tangle` makes the code a stable file in `cwd`
/// (`crate::tangle`) instead of a temp file, for any interpreter. All of
/// them can be left empty/`None` for a plain `interpreter=`.
pub fn resolve_command(
    code: &str,
    interpreter: Option<&str>,
    lang: Option<&str>,
    cwd: Option<&std::path::Path>,
    origin: Option<crate::tangle::BlockOrigin<'_>>,
    env_names: &[String],
) -> io::Result<ResolvedCommand> {
    match interpreter {
        None => Ok(ResolvedCommand {
            program: "bash".to_string(),
            args: vec!["-c".to_string(), code.to_string()],
            cleanup: None,
            extra_envs: Vec::new(),
        }),
        Some(spec) => {
            let resolved_spec;
            let extra_envs;
            let spec = match crate::builtin_interpreter::resolve_with_env(
                spec,
                lang,
                cwd,
                origin.map(|o| o.canvas_path),
                env_names,
            )? {
                Some((path, envs)) => {
                    resolved_spec = path;
                    extra_envs = envs;
                    resolved_spec.as_str()
                }
                None => {
                    extra_envs = Vec::new();
                    spec
                }
            };
            let (program, mut args) = split_interpreter(spec).ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("interpreter={spec:?} isn't a valid shell-word command"),
                )
            })?;
            let (path, cleanup) = script_file(code, lang, cwd, origin)?;
            args.push(path.display().to_string());
            Ok(ResolvedCommand {
                program,
                args,
                cleanup,
                extra_envs,
            })
        }
    }
}

/// The file extension a block's code is written under for `lang`, without
/// the dot. Runtimes pick a mode from it — Node reads `.tmp` as CommonJS
/// only (an ESM `import` fails with `ERR_UNKNOWN_FILE_EXTENSION`), `deno`
/// and `tsx` need `.ts` — so the extension follows the fence language.
/// `tmp` for a language with no entry (and for `@name` builtins' `text`/
/// `toml` bodies, which are data for a script, not code).
pub fn script_extension(lang: Option<&str>) -> &'static str {
    match lang.map(str::to_ascii_lowercase).as_deref() {
        Some("python" | "python3" | "py") => "py",
        Some("javascript" | "js" | "node" | "nodejs" | "mjs") => "js",
        Some("typescript" | "ts") => "ts",
        Some("ruby" | "rb") => "rb",
        Some("perl" | "pl") => "pl",
        Some("php") => "php",
        Some("lua") => "lua",
        Some("sql") => "sql",
        Some("go" | "golang") => "go",
        Some("bash" | "sh" | "shell" | "zsh") => "sh",
        _ => "tmp",
    }
}

/// The file an `interpreter=` runs `code` from, and the path the caller must
/// remove once the child has exited (`None` for a tangled file, which stays):
/// the block's stable file in `cwd` when `origin` carries `tangle`
/// (`crate::tangle::materialize`), a fresh temp file otherwise. The one
/// place both spawn paths (captured and `tty`) choose between the two.
pub fn script_file(
    code: &str,
    lang: Option<&str>,
    cwd: Option<&std::path::Path>,
    origin: Option<crate::tangle::BlockOrigin<'_>>,
) -> io::Result<(std::path::PathBuf, Option<std::path::PathBuf>)> {
    if let Some(origin) = origin {
        if let Some(block_name) = origin.tangle {
            let dir = cwd.unwrap_or(std::path::Path::new("."));
            let path = crate::tangle::materialize(origin.canvas_path, block_name, code, lang, dir)?;
            return Ok((path, None));
        }
    }
    let path = write_script_file(code, lang)?;
    Ok((path.clone(), Some(path)))
}

/// Writes `code` to a fresh `meshfox-<pid>-<n>.<ext>` file under the system
/// temp dir (`ext` from [`script_extension`]) and returns its path — the one
/// place a block's code becomes a file for an `interpreter=`, shared by the
/// captured (`stream_exec`) and `tty` spawn paths. The caller removes it once
/// the child has exited.
///
/// The name is guessable (pid and a counter) and the temp dir may be shared
/// with other users, so the file is created exclusively: a path somebody
/// already put there — a file, or a symlink to one of ours — is skipped, never
/// written through or run.
pub fn write_script_file(code: &str, lang: Option<&str>) -> io::Result<std::path::PathBuf> {
    use std::io::Write;
    let ext = script_extension(lang);
    let (path, mut file) = create_exclusive(|| {
        std::env::temp_dir().join(format!("meshfox-{}.{ext}", uuid_like_suffix()))
    })?;
    if let Err(e) = file.write_all(code.as_bytes()) {
        let _ = std::fs::remove_file(&path);
        return Err(e);
    }
    Ok(path)
}

/// Creates a file that did not exist (`O_CREAT | O_EXCL`: a symlink in the
/// way counts as existing and is not followed) at the first name `next`
/// offers that is free, and returns it open for writing. Callers whose names
/// are guessable and live in a directory others can write to use this instead
/// of `std::fs::write`, which would write through a planted symlink or into a
/// file another user owns.
pub(crate) fn create_exclusive(
    mut next: impl FnMut() -> std::path::PathBuf,
) -> io::Result<(std::path::PathBuf, std::fs::File)> {
    for _ in 0..32 {
        let path = next();
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(file) => return Ok((path, file)),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "no free temporary file name",
    ))
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

/// True if `lang` is the `button` pseudo-language — see `BUTTON_LANG`.
pub fn is_button(lang: &str) -> bool {
    lang == BUTTON_LANG
}

/// True if `lang` is the `form` pseudo-language — see `FORM_LANG`.
pub fn is_form(lang: &str) -> bool {
    lang == FORM_LANG
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_supported_lang_matches_known_languages() {
        assert!(is_supported_lang("bash"));
        assert!(is_supported_lang("sh"));
        assert!(is_supported_lang("button"));
        assert!(is_supported_lang("form"));
        assert!(!is_supported_lang("yaml"));
        assert!(!is_supported_lang("ruby"));
    }

    #[test]
    fn is_button_matches_only_the_button_lang() {
        assert!(is_button("button"));
        assert!(!is_button("bash"));
        assert!(!is_button("form"));
    }

    #[test]
    fn is_form_matches_only_the_form_lang() {
        assert!(is_form("form"));
        assert!(!is_form("bash"));
        assert!(!is_form("button"));
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
        assert_eq!(
            interpreter_var_refs("$PYTHON -u"),
            vec!["PYTHON".to_string()]
        );
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
        assert_eq!(
            resolve_interpreter("$PYTHON -u", &vars),
            ".venv/bin/python3 -u"
        );
    }

    #[test]
    fn resolve_interpreter_leaves_literal_tokens_untouched() {
        let vars = std::collections::HashMap::new();
        assert_eq!(resolve_interpreter("python3 -u", &vars), "python3 -u");
    }

    #[test]
    fn resolve_interpreter_keeps_a_value_containing_spaces_as_one_argument() {
        let mut vars = std::collections::HashMap::new();
        vars.insert(
            "PYTHON".to_string(),
            "/opt/my python/bin/python3".to_string(),
        );
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
        let resolved = resolve_command("echo hi", None, None, None, None, &[]).unwrap();
        assert_eq!(resolved.program, "bash");
        assert_eq!(resolved.args, vec!["-c".to_string(), "echo hi".to_string()]);
        assert!(resolved.cleanup.is_none());
    }

    #[test]
    fn resolve_command_with_interpreter_writes_a_temp_file_and_appends_its_path() {
        let resolved =
            resolve_command("print('hi')", Some("python3 -u"), None, None, None, &[]).unwrap();
        assert_eq!(resolved.program, "python3");
        let cleanup = resolved
            .cleanup
            .clone()
            .expect("interpreter spawn sets cleanup");
        assert_eq!(
            resolved.args,
            vec!["-u".to_string(), cleanup.display().to_string()]
        );
        assert_eq!(std::fs::read_to_string(&cleanup).unwrap(), "print('hi')");
        std::fs::remove_file(&cleanup).unwrap();
    }

    #[test]
    fn script_extension_follows_the_fence_language() {
        assert_eq!(script_extension(Some("python")), "py");
        assert_eq!(script_extension(Some("JavaScript")), "js");
        assert_eq!(script_extension(Some("ts")), "ts");
        assert_eq!(script_extension(Some("sql")), "sql");
        // No entry, or no language at all: the old extension.
        assert_eq!(script_extension(Some("text")), "tmp");
        assert_eq!(script_extension(Some("toml")), "tmp");
        assert_eq!(script_extension(None), "tmp");
    }

    #[test]
    fn resolve_command_names_the_temp_file_after_the_language() {
        let resolved = resolve_command(
            "console.log(1)",
            Some("node"),
            Some("javascript"),
            None,
            None,
            &[],
        )
        .unwrap();
        let cleanup = resolved
            .cleanup
            .clone()
            .expect("interpreter spawn sets cleanup");
        assert_eq!(cleanup.extension().and_then(|e| e.to_str()), Some("js"));
        std::fs::remove_file(&cleanup).unwrap();
    }

    #[test]
    #[cfg(unix)]
    fn create_exclusive_skips_taken_names_and_never_writes_through_a_symlink() {
        use std::io::Write;
        let dir = std::env::temp_dir().join(format!("meshfox-excl-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let victim = dir.join("victim");
        std::fs::write(&victim, "keep").unwrap();
        let link = dir.join("a");
        std::os::unix::fs::symlink(&victim, &link).unwrap();
        let dangling = dir.join("b");
        std::os::unix::fs::symlink(dir.join("nowhere"), &dangling).unwrap();
        let taken = dir.join("c");
        std::fs::write(&taken, "theirs").unwrap();
        let free = dir.join("d");

        let mut names = vec![link, dangling, taken.clone(), free.clone()].into_iter();
        let (path, mut file) = create_exclusive(|| names.next().unwrap()).unwrap();
        file.write_all(b"mine").unwrap();
        drop(file);

        assert_eq!(path, free);
        assert_eq!(std::fs::read_to_string(&free).unwrap(), "mine");
        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "keep");
        assert_eq!(std::fs::read_to_string(&taken).unwrap(), "theirs");
        assert!(
            !dir.join("nowhere").exists(),
            "a dangling link is not followed"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn create_exclusive_gives_up_when_every_name_is_taken() {
        let dir = std::env::temp_dir().join(format!("meshfox-excl-full-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let taken = dir.join("only");
        std::fs::write(&taken, "x").unwrap();
        let err = create_exclusive(|| taken.clone()).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn write_script_file_writes_the_code_to_a_new_file_each_time() {
        let a = write_script_file("one", Some("python")).unwrap();
        let b = write_script_file("two", Some("python")).unwrap();
        assert_ne!(a, b);
        assert_eq!(a.extension().and_then(|e| e.to_str()), Some("py"));
        assert_eq!(std::fs::read_to_string(&a).unwrap(), "one");
        assert_eq!(std::fs::read_to_string(&b).unwrap(), "two");
        std::fs::remove_file(a).unwrap();
        std::fs::remove_file(b).unwrap();
    }

    #[test]
    fn resolve_command_rejects_a_malformed_interpreter() {
        assert!(resolve_command("code", Some(r#"unterminated ""#), None, None, None, &[]).is_err());
    }

    #[test]
    fn resolve_command_resolves_an_at_builtin_interpreter_to_bash_running_its_script() {
        let resolved = resolve_command("a prompt", Some("@agent"), None, None, None, &[]).unwrap();
        assert_eq!(resolved.program, "bash");
        // `bash -c <script> agent <the fence body's file>`: nothing written
        // for the script itself, and the body's file is the last argument.
        assert_eq!(resolved.args[0], "-c");
        assert!(resolved.args[1].contains("MESHFOX_CONFIG_INTERPRETERS_AGENT_PROVIDER"));
        assert_eq!(resolved.args[2], "agent");
        assert_eq!(resolved.args.len(), 4);
        // No canvas_path given -> no MESHFOX_VENV_DIR, but @agent never
        // sets that anyway (only @python_venv does) — this just confirms
        // resolve_command's own config-env plumbing ran at all, same as
        // stream_exec's spawn_interpreter test covers for the non-tty path.
        let cleanup = resolved
            .cleanup
            .clone()
            .expect("interpreter spawn sets cleanup");
        assert_eq!(std::fs::read_to_string(&cleanup).unwrap(), "a prompt");
        std::fs::remove_file(&cleanup).unwrap();
    }

    #[test]
    fn resolve_command_rejects_an_unknown_at_builtin_name() {
        assert!(resolve_command("code", Some("@nonexistent"), None, None, None, &[]).is_err());
    }
}
