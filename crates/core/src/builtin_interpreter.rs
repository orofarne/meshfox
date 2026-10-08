//! `@name` interpreters — a small, fixed set of macro scripts meshfox
//! embeds in its own binary. `interpreter="@agent"` (or `"@python_venv"`)
//! resolves, via `resolve_builtin_spec`, to a real on-disk script path
//! *before* `crate::exec::split_interpreter` ever sees it — from that
//! point on a builtin is completely indistinguishable from a real
//! `interpreter="/some/script"`: same shebang-style temp-file-for-code
//! convention (`crate::exec`'s own doc comment), same spawn path, no
//! special-cased execution model of its own.
//!
//! Deliberately "dumb": every builtin's *entire* behavior lives in its
//! embedded shell script text (`src/builtins/*.sh`), not in bespoke Rust
//! logic per name — so a canvas author can always "unbuiltin" one by
//! copying the script out and setting `interpreter="./my-agent.sh"`
//! instead, with no functional difference. See `crate::config` for the
//! `MESHFOX_CONFIG_*` values a script like `agent.sh` reads.
//!
//! Wired into both real resolution paths: `stream_exec::spawn_interpreter`
//! (captured/non-`tty` output) and `crate::exec::resolve_command` (`tty`,
//! real-terminal handoff) both call `resolve_with_env` below rather than
//! each separately re-implementing `@`-detection, so the two can't drift.

use std::io;
use std::path::{Path, PathBuf};

/// One embedded macro: `interpreter="@{name}"` resolves to `script`.
pub struct Builtin {
    pub name: &'static str,
    pub script: &'static str,
}

pub const BUILTINS: &[Builtin] = &[
    Builtin {
        name: "agent",
        script: include_str!("builtins/agent.sh"),
    },
    Builtin {
        name: "python_venv",
        script: include_str!("builtins/python_venv.sh"),
    },
];

fn lookup(name: &str) -> Option<&'static Builtin> {
    BUILTINS.iter().find(|b| b.name == name)
}

/// If `spec` names a builtin (`@name`, exactly — no arguments; a builtin's
/// own knobs come from config, not from text in the fence's own
/// `interpreter=` attribute), materializes its script to a stable on-disk
/// path (a no-op if already there from an earlier call with the same
/// binary) and returns that path — ready to be handed to
/// `crate::exec::split_interpreter` as an ordinary `interpreter=` value,
/// same as any real installed interpreter's own path would be. `Ok(None)`
/// for a spec that isn't `@`-prefixed at all — nothing for a caller to do
/// differently than today. An `@`-prefixed spec naming no known builtin is
/// an error, not a silent pass-through (there's no real program named
/// `@whatever` it could otherwise fall back to trying).
pub fn resolve_builtin_spec(spec: &str) -> io::Result<Option<String>> {
    let Some(name) = spec.strip_prefix('@') else {
        return Ok(None);
    };
    let builtin = lookup(name).ok_or_else(|| {
        let known: Vec<&str> = BUILTINS.iter().map(|b| b.name).collect();
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "interpreter=\"@{name}\": no builtin interpreter by that name (known: {})",
                known.join(", ")
            ),
        )
    })?;
    Ok(Some(materialize(builtin)?.display().to_string()))
}

/// The resolved interpreter path, plus every `(name, value)` env var pair
/// it should be spawned with — `resolve_with_env`'s own return shape,
/// pulled out to a named type only to keep that signature legible (a
/// nested tuple-in-tuple return type is otherwise exactly the kind of
/// thing `clippy::type_complexity` exists to flag).
pub type ResolvedInterpreter = (String, Vec<(String, String)>);

/// `resolve_builtin_spec` plus the `MESHFOX_*` env vars that resolution
/// should carry with it — the one place a caller actually spawning a
/// builtin (as opposed to just checking `is_builtin`) should go through.
/// `Ok(None)` for a non-`@` spec, same as `resolve_builtin_spec`.
///
/// Always includes the current config's `MESHFOX_CONFIG_*` flattening
/// (`crate::config`, loaded from `cwd` as the local config root — `None`
/// falls back to `.`, i.e. global config only). For `@python_venv`
/// specifically, includes `MESHFOX_BLOCK_LANG` when `lang` is given and,
/// when `canvas_path` is given, `MESHFOX_VENV_DIR` — a venv colocated at
/// `.meshfox/<canvas filename>.venv`, keyed by the *canvas's own* path rather than shared
/// per-directory `.venv`, so two unrelated canvases in the same directory
/// (e.g. this repo's own `examples/python-venv.canvas.md` and
/// `examples/pandas-dataframe.canvas.md`) each get their own venv instead
/// of silently sharing and colliding on one. `canvas_path: None`
/// leaves `MESHFOX_VENV_DIR` unset, so `python_venv.sh`'s own
/// `${MESHFOX_VENV_DIR:-.venv}` falls back to today's shared-per-directory
/// behavior rather than erroring.
pub fn resolve_with_env(
    spec: &str,
    lang: Option<&str>,
    cwd: Option<&Path>,
    canvas_path: Option<&Path>,
    env_names: &[String],
) -> io::Result<Option<ResolvedInterpreter>> {
    let Some(interpreter_path) = resolve_builtin_spec(spec)? else {
        return Ok(None);
    };
    let mut envs = Vec::new();
    let config = crate::config::load(cwd.unwrap_or_else(|| Path::new(".")));
    envs.extend(crate::config::flatten_to_env(&config));
    if spec == "@python_venv" {
        if let Some(lang) = lang {
            envs.push(("MESHFOX_BLOCK_LANG".to_string(), lang.to_string()));
        }
        if let Some(canvas_path) = canvas_path {
            envs.push((
                "MESHFOX_VENV_DIR".to_string(),
                venv_dir(canvas_path).display().to_string(),
            ));
        }
    }
    envs.push(("MESHFOX_ENV_NAMES".to_string(), env_names.join(",")));
    Ok(Some((interpreter_path, envs)))
}

/// `.meshfox/<canvas filename>.venv` next to the canvas file — same
/// colocation convention `crate::varcache::cache_path` already uses for
/// `<filename>.env`, just a directory instead of a dotenv file. For a canvas
/// that can't be written, a scratch directory under the system temp dir
/// instead (see `read_only_venv_dir`). See
/// `resolve_with_env`'s own doc comment for why this is keyed by the
/// canvas's own path rather than shared per-directory.
///
/// Absolute, deliberately: the child process this env var reaches
/// (`python_venv.sh`) is spawned with its *own* cwd already set to the
/// block's resolved node cwd (`crate::canvas::Node::cwd`) — a `canvas_path`
/// left relative to meshfox's own invocation directory would resolve
/// wrongly once re-interpreted from that different starting point (e.g.
/// double-joining `examples/` onto a path already inside it). Resolved
/// against this *process's* cwd, not the eventual child's — meshfox itself
/// hasn't `chdir`'d by the time this runs.
fn venv_dir(canvas_path: &Path) -> PathBuf {
    let absolute = if canvas_path.is_absolute() {
        canvas_path.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(canvas_path))
            .unwrap_or_else(|_| canvas_path.to_path_buf())
    };
    let dir = match absolute.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        _ => PathBuf::from("/"),
    };
    let file_name = absolute
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    if crate::worker_lock::is_read_only(&absolute) {
        return read_only_venv_dir(&absolute, &file_name);
    }
    dir.join(".meshfox").join(format!("{file_name}.venv"))
}

/// Where the venv of a canvas that can't be written lives instead:
/// `<system temp>/meshfox-readonly-<hash of the canvas path>/<filename>.venv`,
/// owner-only, so it's reused across runs but never needs the canvas's own
/// directory — and the system cleans it up, rather than meshfox leaving state
/// behind for canvases that are long gone. Keyed by the full path, since
/// unlike the colocated venv nothing here separates two canvases that happen
/// to share a file name.
fn read_only_venv_dir(absolute: &Path, file_name: &str) -> PathBuf {
    read_only_temp_root(absolute).join(format!("{file_name}.venv"))
}

/// `<system temp>/meshfox-readonly-<hash of the canvas path>`, created
/// owner-only (best effort) — the one place state for a canvas that can't be
/// written may live on disk (its venv, `display="table"` caches). `absolute`
/// is the canvas's absolute path.
pub fn read_only_temp_root(absolute: &Path) -> PathBuf {
    use std::os::unix::fs::DirBuilderExt;
    let key = fnv1a(absolute.to_string_lossy().as_bytes());
    let root = std::env::temp_dir().join(format!("meshfox-readonly-{key:08x}"));
    // Best-effort: if this fails, the caller's own create reports it.
    let _ = std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&root);
    root
}

/// True if `name` (without the `@`, e.g. `"agent"`) is a known builtin —
/// for a caller that wants to check without also materializing a script to
/// disk (e.g. deciding whether to bother loading config at all).
pub fn is_builtin(spec: &str) -> bool {
    spec.strip_prefix('@')
        .is_some_and(|name| lookup(name).is_some())
}

/// Writes `builtin.script` to a stable, content-addressed path under the
/// system temp dir (so a binary upgrade that changes the embedded script
/// gets a fresh path instead of silently reusing stale content) and marks
/// it executable. Reused across every call in the same meshfox version —
/// this is static, binary-embedded content, not a fresh temp file per run
/// the way the fence's own code gets (`crate::exec::resolve_command`'s own
/// per-run temp file, cleaned up after) — there's nothing to clean up
/// here, same as a real installed `python3` binary is never deleted after
/// use.
fn materialize(builtin: &Builtin) -> io::Result<PathBuf> {
    let dir = std::env::temp_dir().join("meshfox-builtins");
    materialize_in(builtin.name, builtin.script, &dir)
}

fn materialize_in(name: &str, script: &str, dir: &Path) -> io::Result<PathBuf> {
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    std::fs::create_dir_all(dir)?;
    let path = dir.join(format!(
        "{}-{}.sh",
        name,
        blake3::hash(script.as_bytes()).to_hex()
    ));
    if path.exists() {
        return Ok(path);
    }
    let temporary = dir.join(format!(
        ".{}-{}-{}-{}.tmp",
        name,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)?;
    let result = (|| {
        file.write_all(script.as_bytes())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(std::fs::Permissions::from_mode(0o755))?;
        }
        // Close the writable descriptor before publication: on Linux another
        // process executing it while still open for writing gets ETXTBSY.
        drop(file);
        // A hard link atomically publishes a complete, executable file without
        // replacing another process's already-published instance.
        match std::fs::hard_link(&temporary, &path) {
            Ok(()) => Ok(path.clone()),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Ok(path.clone()),
            Err(e) => Err(e),
        }
    })();
    let _ = std::fs::remove_file(&temporary);
    result
}

/// Execution identity of a builtin, excluding unrelated machine settings.
/// Pure counterpart used by tests without reading the developer's config.
pub fn execution_identity(spec: &str, config: &toml::Table) -> Option<String> {
    let builtin = lookup(spec.strip_prefix('@')?)?;
    let mut hash = blake3::Hasher::new();
    hash.update(builtin.script.as_bytes());
    if builtin.name == "agent" {
        // Project exactly the value exported to agent.sh (including key
        // normalization and the same last-value precedence as envs()).
        let provider = crate::config::flatten_to_env(config)
            .into_iter()
            .rev()
            .find(|(key, _)| key == "MESHFOX_CONFIG_INTERPRETERS_AGENT_PROVIDER")
            .map(|(_, value)| value)
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "claude".to_string());
        hash.update(provider.as_bytes());
    }
    Some(hash.finalize().to_hex().to_string())
}

/// Short key for the read-only scratch directory; builtin executable names
/// use the full BLAKE3 content digest instead.
fn fnv1a(bytes: &[u8]) -> u32 {
    const OFFSET: u32 = 0x811c_9dc5;
    const PRIME: u32 = 0x0100_0193;
    let mut hash = OFFSET;
    for &b in bytes {
        hash ^= b as u32;
        hash = hash.wrapping_mul(PRIME);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_builtin_spec_is_none_for_a_non_builtin_spec() {
        assert!(resolve_builtin_spec("python3 -u").unwrap().is_none());
    }

    #[test]
    fn resolve_builtin_spec_errors_on_an_unknown_builtin_name() {
        let err = resolve_builtin_spec("@nonexistent").unwrap_err();
        assert!(err.to_string().contains("@nonexistent"));
    }

    #[test]
    fn resolve_builtin_spec_materializes_a_readable_executable_script() {
        let path = resolve_builtin_spec("@agent").unwrap().unwrap();
        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(contents.contains("MESHFOX_CONFIG_INTERPRETERS_AGENT_PROVIDER"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o111, 0o111);
        }
    }

    #[test]
    fn resolve_builtin_spec_is_stable_across_calls() {
        let a = resolve_builtin_spec("@python_venv").unwrap().unwrap();
        let b = resolve_builtin_spec("@python_venv").unwrap().unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn is_builtin_true_only_for_a_known_at_prefixed_name() {
        assert!(is_builtin("@agent"));
        assert!(!is_builtin("@nonexistent"));
        assert!(!is_builtin("python3 -u"));
    }

    #[test]
    fn resolve_with_env_is_none_for_a_non_builtin_spec() {
        assert!(resolve_with_env("python3 -u", None, None, None, &[])
            .unwrap()
            .is_none());
    }

    #[test]
    fn resolve_with_env_sets_venv_dir_only_for_python_venv_with_a_canvas_path() {
        // Own writable fixture, including when the checkout is mounted read-only.
        let dir = std::env::temp_dir().join(format!(
            "meshfox-venv-env-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        let examples = dir.join("examples");
        std::fs::create_dir_all(&examples).unwrap();
        let canvas_file = examples.join("pandas-dataframe.canvas.md");
        std::fs::write(&canvas_file, "").unwrap();
        let canvas_path = canvas_file.as_path();

        let (_, envs) =
            resolve_with_env("@python_venv", Some("toml"), None, Some(canvas_path), &[])
                .unwrap()
                .unwrap();
        assert!(envs
            .iter()
            .any(|(k, v)| k == "MESHFOX_BLOCK_LANG" && v == "toml"));
        let venv_dir = envs
            .iter()
            .find(|(k, _)| k == "MESHFOX_VENV_DIR")
            .map(|(_, v)| v.as_str())
            .expect("MESHFOX_VENV_DIR set for @python_venv with a canvas_path");
        assert!(
            venv_dir.ends_with("examples/.meshfox/pandas-dataframe.canvas.md.venv"),
            "unexpected venv dir: {venv_dir}"
        );
        assert!(Path::new(venv_dir).is_absolute());

        // No canvas_path -> no MESHFOX_VENV_DIR at all (falls back to
        // python_venv.sh's own `.venv` default).
        let (_, envs) = resolve_with_env("@python_venv", None, None, None, &[])
            .unwrap()
            .unwrap();
        assert!(!envs.iter().any(|(k, _)| k == "MESHFOX_VENV_DIR"));

        // @agent never gets MESHFOX_VENV_DIR, even with a canvas_path — it
        // has no use for one.
        let (_, envs) = resolve_with_env("@agent", None, None, Some(canvas_path), &[])
            .unwrap()
            .unwrap();
        assert!(!envs.iter().any(|(k, _)| k == "MESHFOX_VENV_DIR"));
        assert!(!envs.iter().any(|(k, _)| k == "MESHFOX_BLOCK_LANG"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Regression test for a real bug: `agent.sh`'s `interpolate()` used
    /// `for name in "${names[@]}"` after `IFS=',' read -ra names <<< ...`.
    /// On bash 4+ that's fine even when `MESHFOX_ENV_NAMES` is unset/empty
    /// (no `env=` vars on the fence — the common case), but bash 3.2 —
    /// still what `#!/usr/bin/env bash` finds as a stock Mac's `/bin/bash`
    /// unless a newer bash sits earlier in `$PATH` — treats a zero-element
    /// array as unset under `set -u`, aborting the whole script with
    /// "names[@]: unbound variable" before ever reaching a real
    /// `claude`/`codex` invocation. Runs the materialized script with
    /// `/bin/bash` explicitly (bypassing `$PATH`/the shebang, which is
    /// exactly how this shipped unnoticed — dev shells here have a newer
    /// bash ahead of `/bin/bash`) and a bogus provider to force a
    /// deterministic, real-agent-free failure path.
    #[test]
    #[cfg(target_os = "macos")]
    fn agent_sh_interpolate_survives_bash_3_2_with_no_env_names() {
        let script = resolve_builtin_spec("@agent").unwrap().unwrap();
        let prompt =
            std::env::temp_dir().join(format!("meshfox-agent-sh-test-{}.txt", std::process::id()));
        std::fs::write(&prompt, "hello world").unwrap();

        let output = std::process::Command::new("/bin/bash")
            .arg(&script)
            .arg(&prompt)
            .env_remove("MESHFOX_ENV_NAMES")
            .env(
                "MESHFOX_CONFIG_INTERPRETERS_AGENT_PROVIDER",
                "bogus-provider",
            )
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap();

        std::fs::remove_file(&prompt).ok();

        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            !stderr.contains("unbound variable"),
            "agent.sh crashed on bash 3.2 with no MESHFOX_ENV_NAMES: {stderr}"
        );
        assert!(
            stderr.contains("bogus-provider"),
            "expected agent.sh's own unknown-provider message, got: {stderr}"
        );
    }

    #[test]
    fn resolve_with_env_always_sets_env_names() {
        let (_, envs) = resolve_with_env("@agent", None, None, None, &[])
            .unwrap()
            .unwrap();
        assert!(envs.iter().any(|(k, v)| k == "MESHFOX_ENV_NAMES" && v.is_empty()));

        let names = vec!["TOPIC".to_string(), "OTHER".to_string()];
        let (_, envs) = resolve_with_env("@agent", None, None, None, &names)
            .unwrap()
            .unwrap();
        assert_eq!(
            envs.iter()
                .find(|(k, _)| k == "MESHFOX_ENV_NAMES")
                .map(|(_, v)| v.as_str()),
            Some("TOPIC,OTHER")
        );
    }

    #[test]
    fn agent_identity_tracks_effective_provider_only() {
        let empty = toml::Table::new();
        let claude = "[interpreters.agent]\nprovider = 'claude'\n"
            .parse()
            .unwrap();
        let codex = "[interpreters.agent]\nprovider = 'codex'\n"
            .parse()
            .unwrap();
        let unrelated =
            "[interpreters.agent]\nprovider = 'codex'\n[tables]\ncache_max_bytes = 123\n"
                .parse()
                .unwrap();
        assert_eq!(
            execution_identity("@agent", &empty),
            execution_identity("@agent", &claude)
        );
        assert_ne!(
            execution_identity("@agent", &claude),
            execution_identity("@agent", &codex)
        );
        assert_eq!(
            execution_identity("@agent", &codex),
            execution_identity("@agent", &unrelated)
        );
        assert_eq!(
            execution_identity("@python_venv", &claude),
            execution_identity("@python_venv", &codex)
        );
        assert_eq!(execution_identity("python3", &empty), None);
        let non_scalar = "[interpreters.agent]\nprovider = ['codex']\n".parse().unwrap();
        assert_eq!(
            execution_identity("@agent", &non_scalar),
            execution_identity("@agent", &empty)
        );
    }

    #[test]
    #[cfg(unix)]
    fn agent_prompt_is_single_pass_and_provider_args_are_unrestricted() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!(
            "meshfox-agent-prompt-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let fake = "#!/bin/sh\nfor arg do printf '%s\\000' \"$arg\"; done\n";
        for provider in ["claude", "codex"] {
            let bin = dir.join(provider);
            std::fs::write(&bin, fake).unwrap();
            std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let prompt = dir.join("prompt.txt");
        std::fs::write(&prompt, "A=$A; braced=${A}; B=$B; prefix=$AB; escaped=$$A; unknown=$HOME; price=$5; bad=${A; shell=$(touch NO); unicode=ёж; ctrl=\u{1}").unwrap();
        for shell in ["/bin/bash", "bash"] {
            for provider in ["claude", "codex"] {
                for names in ["A,B", "B,A"] {
                    let output = std::process::Command::new(shell)
                        .arg(resolve_builtin_spec("@agent").unwrap().unwrap())
                        .arg(&prompt)
                        .env(
                            "PATH",
                            format!(
                                "{}:{}",
                                dir.display(),
                                std::env::var("PATH").unwrap_or_default()
                            ),
                        )
                        .env("MESHFOX_CONFIG_INTERPRETERS_AGENT_PROVIDER", provider)
                        .env("MESHFOX_ENV_NAMES", names)
                        .env("A", "$B\n\"quoted\" $$A")
                        .env("B", "VALUE")
                        .current_dir(&dir)
                        .stdin(std::process::Stdio::null())
                        .output()
                        .unwrap();
                    assert!(
                        output.status.success(),
                        "{}",
                        String::from_utf8_lossy(&output.stderr)
                    );
                    let args: Vec<_> = output
                        .stdout
                        .split(|b| *b == 0)
                        .filter(|b| !b.is_empty())
                        .collect();
                    assert_eq!(
                        args[0],
                        if provider == "claude" {
                            b"-p".as_slice()
                        } else {
                            b"exec".as_slice()
                        }
                    );
                    assert_eq!(args[1], b"--");
                    assert_eq!(args.len(), 3);
                    assert_eq!(args[2], "A=$B\n\"quoted\" $$A; braced=$B\n\"quoted\" $$A; B=VALUE; prefix=$AB; escaped=$A; unknown=$HOME; price=$5; bad=${A; shell=$(touch NO); unicode=ёж; ctrl=\u{1}".as_bytes());
                    assert!(!dir.join("NO").exists());
                }
            }
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    // Invoked by concurrent_materialization_publishes_complete_executables
    // in separate processes, without changing this process's environment.
    #[test]
    #[cfg(unix)]
    fn materialization_child() {
        use std::os::unix::fs::PermissionsExt;
        let Some(dir) = std::env::var_os("MESHFOX_TEST_BUILTIN_DIR") else {
            return;
        };
        for round in 0..16 {
            let script = format!("#!/bin/sh\n# {}\nexit 0\n", "x".repeat(65536 + round));
            let path = materialize_in("concurrent", &script, Path::new(&dir)).unwrap();
            assert_eq!(std::fs::read_to_string(&path).unwrap(), script);
            assert_ne!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o111,
                0
            );
            assert!(
                std::process::Command::new(&path)
                    .status()
                    .unwrap()
                    .success()
            );
        }
    }

    #[test]
    #[cfg(unix)]
    fn concurrent_materialization_publishes_complete_executables() {
        let dir = std::env::temp_dir().join(format!(
            "meshfox-materialize-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let mut children: Vec<_> = (0..8)
            .map(|_| {
                std::process::Command::new(std::env::current_exe().unwrap())
                    .args([
                        "--exact",
                        "builtin_interpreter::tests::materialization_child",
                    ])
                    .env("MESHFOX_TEST_BUILTIN_DIR", &dir)
                    .stdout(std::process::Stdio::null())
                    .spawn()
                    .unwrap()
            })
            .collect();
        for child in &mut children {
            assert!(child.wait().unwrap().success());
        }
        let files: Vec<_> = std::fs::read_dir(&dir).unwrap().collect();
        assert_eq!(files.len(), 16, "no unpublished temporary files remain");
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    #[cfg(unix)]
    fn python_venv_dispatches_text_and_toml_to_pip() {
        use std::os::unix::fs::PermissionsExt;

        let dir = std::env::temp_dir().join(format!(
            "meshfox-python-venv-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let bin = dir.join("venv/bin");
        std::fs::create_dir_all(&bin).unwrap();
        let fake_python = bin.join("python3");
        std::fs::write(&fake_python, "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$PIP_CALLS\"\nfor arg do :; done\nif [ -d \"$arg\" ]; then cat \"$arg/pyproject.toml\" >> \"$PIP_CALLS\"; fi\n").unwrap();
        std::fs::set_permissions(&fake_python, std::fs::Permissions::from_mode(0o755)).unwrap();
        let requirements = dir.join("requirements");
        let project = dir.join("project");
        let calls = dir.join("calls");
        let vars = dir.join("vars");
        std::fs::write(&requirements, "tabulate==0.9.0\n").unwrap();
        std::fs::write(
            &project,
            "[project]\nname = \"example\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        let script = resolve_builtin_spec("@python_venv").unwrap().unwrap();
        for (lang, input) in [("text", &requirements), ("toml", &project)] {
            let output = std::process::Command::new("bash")
                .arg(&script)
                .arg(input)
                .env("MESHFOX_BLOCK_LANG", lang)
                .env("MESHFOX_VENV_DIR", dir.join("venv"))
                .env("MESHFOX_VARS_OUT", &vars)
                .env("PIP_CALLS", &calls)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        let calls_text = std::fs::read_to_string(&calls).unwrap();
        assert!(calls_text.contains(&format!(
            "-m pip install --disable-pip-version-check -r {}",
            requirements.display()
        )));
        assert!(calls_text.contains("[project]\nname = \"example\""));
        assert!(calls_text.contains("-m pip install --disable-pip-version-check /"));
        assert_eq!(std::fs::read_to_string(&vars).unwrap().lines().count(), 2);
        let unsupported = std::process::Command::new("bash")
            .arg(&script)
            .arg(&project)
            .env("MESHFOX_BLOCK_LANG", "yaml")
            .env("MESHFOX_VENV_DIR", dir.join("venv"))
            .env("MESHFOX_VARS_OUT", &vars)
            .output()
            .unwrap();
        assert_eq!(unsupported.status.code(), Some(2));
        assert!(
            String::from_utf8_lossy(&unsupported.stderr).contains("expected a text or toml block")
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_read_only_canvas_gets_its_venv_under_the_system_temp_dir() {
        use std::os::unix::fs::PermissionsExt;
        // SAFETY: `geteuid` takes no arguments and can't fail.
        if unsafe { libc::geteuid() } == 0 {
            return;
        }
        let dir = std::env::temp_dir().join(format!("meshfox-ro-venv-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let canvas = dir.join("doc.canvas.md");
        std::fs::write(&canvas, "# Doc\n").unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o555)).unwrap();

        let venv = venv_dir(&canvas);
        assert!(
            venv.starts_with(std::env::temp_dir()),
            "unexpected venv dir: {venv:?}"
        );
        assert!(
            !venv.starts_with(&dir),
            "venv must not be inside the read-only directory"
        );
        assert!(venv.ends_with("doc.canvas.md.venv"));
        // Stable: the same canvas always maps to the same place.
        assert_eq!(venv, venv_dir(&canvas));
        assert!(!dir.join(".meshfox").exists());

        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::remove_dir_all(&dir).ok();
        if let Some(root) = venv.parent() {
            std::fs::remove_dir_all(root).ok();
        }
    }
}
