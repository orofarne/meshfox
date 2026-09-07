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

/// `resolve_builtin_spec` plus the `MESHFOX_*` env vars that resolution
/// should carry with it — the one place a caller actually spawning a
/// builtin (as opposed to just checking `is_builtin`) should go through.
/// `Ok(None)` for a non-`@` spec, same as `resolve_builtin_spec`.
///
/// Always includes the current config's `MESHFOX_CONFIG_*` flattening
/// (`crate::config`, loaded from `cwd` as the local config root — `None`
/// falls back to `.`, i.e. global config only). For `@python_venv`
/// specifically, when `canvas_path` is given, also includes
/// `MESHFOX_VENV_DIR` — a venv colocated at `.meshfox/<canvas
/// filename>.venv`, keyed by the *canvas's own* path rather than shared
/// per-directory `.venv`, so two unrelated canvases in the same directory
/// (e.g. this repo's own `examples/python-venv.canvas.md` and
/// `examples/pandas-dataframe.canvas.md`) each get their own venv instead
/// of silently sharing and colliding on one. `canvas_path: None`
/// leaves `MESHFOX_VENV_DIR` unset, so `python_venv.sh`'s own
/// `${MESHFOX_VENV_DIR:-.venv}` falls back to today's shared-per-directory
/// behavior rather than erroring.
pub fn resolve_with_env(
    spec: &str,
    cwd: Option<&Path>,
    canvas_path: Option<&Path>,
    env_names: &[String],
) -> io::Result<Option<(String, Vec<(String, String)>)>> {
    let Some(interpreter_path) = resolve_builtin_spec(spec)? else {
        return Ok(None);
    };
    let mut envs = Vec::new();
    let config = crate::config::load(cwd.unwrap_or_else(|| Path::new(".")));
    envs.extend(crate::config::flatten_to_env(&config));
    if spec == "@python_venv" {
        if let Some(canvas_path) = canvas_path {
            envs.push((
                "MESHFOX_VENV_DIR".to_string(),
                venv_dir(canvas_path).display().to_string(),
            ));
        }
    }
    if !env_names.is_empty() {
        envs.push(("MESHFOX_ENV_NAMES".to_string(), env_names.join(",")));
    }
    Ok(Some((interpreter_path, envs)))
}

/// `.meshfox/<canvas filename>.venv` next to the canvas file — same
/// colocation convention `crate::varcache::cache_path` already uses for
/// `<filename>.env`, just a directory instead of a dotenv file. See
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
    dir.join(".meshfox").join(format!("{file_name}.venv"))
}

/// True if `name` (without the `@`, e.g. `"agent"`) is a known builtin —
/// for a caller that wants to check without also materializing a script to
/// disk (e.g. deciding whether to bother loading config at all).
pub fn is_builtin(spec: &str) -> bool {
    spec.strip_prefix('@').is_some_and(|name| lookup(name).is_some())
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
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{}-{:08x}.sh", builtin.name, fnv1a(builtin.script.as_bytes())));
    if !path.exists() {
        std::fs::write(&path, builtin.script)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))?;
        }
    }
    Ok(path)
}

/// Same tiny non-cryptographic hash `crate::fence::fingerprint` uses, for
/// the same reason: just enough to make a builtin's materialized filename
/// change when its embedded content does, a collision costing nothing
/// worse than reusing/overwriting a stale-but-harmless temp file.
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
        assert!(resolve_with_env("python3 -u", None, None, &[]).unwrap().is_none());
    }

    #[test]
    fn resolve_with_env_sets_venv_dir_only_for_python_venv_with_a_canvas_path() {
        let canvas_path = Path::new("examples/pandas-dataframe.canvas.md");

        let (_, envs) = resolve_with_env("@python_venv", None, Some(canvas_path), &[])
            .unwrap()
            .unwrap();
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
        let (_, envs) = resolve_with_env("@python_venv", None, None, &[]).unwrap().unwrap();
        assert!(!envs.iter().any(|(k, _)| k == "MESHFOX_VENV_DIR"));

        // @agent never gets MESHFOX_VENV_DIR, even with a canvas_path — it
        // has no use for one.
        let (_, envs) = resolve_with_env("@agent", None, Some(canvas_path), &[])
            .unwrap()
            .unwrap();
        assert!(!envs.iter().any(|(k, _)| k == "MESHFOX_VENV_DIR"));
    }

    #[test]
    fn resolve_with_env_sets_env_names_only_when_non_empty() {
        let (_, envs) = resolve_with_env("@agent", None, None, &[]).unwrap().unwrap();
        assert!(!envs.iter().any(|(k, _)| k == "MESHFOX_ENV_NAMES"));

        let names = vec!["TOPIC".to_string(), "OTHER".to_string()];
        let (_, envs) = resolve_with_env("@agent", None, None, &names).unwrap().unwrap();
        assert_eq!(
            envs.iter().find(|(k, _)| k == "MESHFOX_ENV_NAMES").map(|(_, v)| v.as_str()),
            Some("TOPIC,OTHER")
        );
    }
}
