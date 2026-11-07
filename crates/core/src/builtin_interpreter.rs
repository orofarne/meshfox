//! `@name` interpreters — a small, fixed set of macro scripts meshfox
//! embeds in its own binary. `interpreter="@agent"` (or `"@python_venv"`)
//! resolves, via `resolve_builtin_spec`, to a `bash -c '<script>' <name>`
//! command *before* `crate::exec::split_interpreter` ever sees it — from
//! that point on a builtin is indistinguishable from a real
//! `interpreter="bash -c ..."`: same shebang-style temp-file-for-code
//! convention (`crate::exec`'s own doc comment), same spawn path, no
//! special-cased execution model of its own. The script is never written to
//! disk, so there is no file in a shared directory to plant or replace.
//!
//! Deliberately "dumb": every builtin's *entire* behavior lives in its
//! embedded shell script text (`src/builtins/*.sh`), not in bespoke Rust
//! logic per name — so a canvas author can always "unbuiltin" one by
//! copying the script out and setting `interpreter="./my-agent.sh"`
//! instead, with no functional difference. See `crate::config` for the
//! `MESHFOX_CONFIG_*` values a script like `agent.sh` reads.
//!
//! `@service` is a native lifecycle interpreter handled by the service
//! executor; it never materializes a script through this module.
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
    Builtin {
        name: "node_env",
        script: include_str!("builtins/node_env.sh"),
    },
];

fn lookup(name: &str) -> Option<&'static Builtin> {
    BUILTINS.iter().find(|b| b.name == name)
}

/// Parse the optional, deliberately small environment-builtin option surface.
fn environment_options(spec: &str) -> io::Result<(String, Option<String>, Option<String>)> {
    let words = shlex::split(spec).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid builtin interpreter quoting",
        )
    })?;
    let mut words = words.into_iter();
    let base = words.next().unwrap_or_default();
    let mut name = None;
    let mut var = None;
    for word in words {
        let (slot, value, variable) = match word.split_once('=') {
            Some(("name", value)) => (&mut name, value, false),
            Some(("var", value)) => (&mut var, value, true),
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "builtin options must be name=NAME or var=VARIABLE",
                ))
            }
        };
        if base != "@python_venv" && base != "@node_env" {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "only environment builtins accept name= and var=",
            ));
        }
        let valid = !value.is_empty()
            && value.bytes().enumerate().all(|(i, c)| {
                c.is_ascii_alphabetic()
                    || c == b'_'
                    || (i > 0 && (c.is_ascii_digit() || (!variable && c == b'-')))
            });
        if !valid || slot.replace(value.to_string()).is_some() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("invalid or duplicate builtin option: {word}"),
            ));
        }
    }
    Ok((base, name, var))
}

/// If `spec` names a builtin (environment builtins also accept `name=`
/// and `var=` options), returns the interpreter command that runs its
/// embedded script — `bash -c '<script>' <name>`, shell-quoted — ready to be
/// handed to `crate::exec::split_interpreter` as an ordinary `interpreter=`
/// value, same as any real installed interpreter's own command would be.
/// Nothing is written to disk. `Ok(None)` for a spec that isn't
/// `@`-prefixed at all — nothing for a caller to do differently than today.
/// An `@`-prefixed spec naming no known builtin is an error, not a silent
/// pass-through (there's no real program named `@whatever` it could
/// otherwise fall back to trying).
pub fn resolve_builtin_spec(spec: &str) -> io::Result<Option<String>> {
    if !shlex::split(spec)
        .and_then(|words| words.into_iter().next())
        .is_some_and(|word| word.starts_with('@'))
    {
        return Ok(None);
    }
    let (spec, _, _) = environment_options(spec)?;
    if spec == "@service" {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "@service requires the managed service executor",
        ));
    }
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
    // The script travels as bash's own `-c` argument: no file is written
    // anywhere, so there is nothing in a shared directory for another user to
    // plant or swap. `crate::exec::split_interpreter` takes this apart again
    // as program `bash` with the arguments `-c <script> <name>`, and appends
    // the fence body's file, which the script sees as `$1` (`$0` is the name).
    shlex::try_join(["bash", "-c", builtin.script, builtin.name])
        .map(Some)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e.to_string()))
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
    let (spec, environment_name, output_var) = environment_options(spec)?;
    let mut envs = Vec::new();
    let config = crate::config::load(cwd.unwrap_or_else(|| Path::new(".")));
    envs.extend(crate::config::flatten_to_env(&config));
    // The two environment builtins: both get the fence kind, and — when the
    // canvas's own path is known — a directory keyed by that path.
    let environment = match spec.as_str() {
        "@python_venv" => Some(("MESHFOX_VENV_DIR", "venv")),
        "@node_env" => Some(("MESHFOX_NODE_DIR", "node")),
        _ => None,
    };
    if let Some((dir_var, suffix)) = environment {
        if let Some(lang) = lang {
            envs.push(("MESHFOX_BLOCK_LANG".to_string(), lang.to_string()));
        }
        if let Some(canvas_path) = canvas_path {
            envs.push((
                dir_var.to_string(),
                {
                    let base = canvas_state_dir(canvas_path, suffix);
                    environment_name
                        .as_deref()
                        .map_or_else(|| base.clone(), |name| base.join(".named").join(name))
                }
                .display()
                .to_string(),
            ));
        }
    }
    if let Some(name) = environment_name {
        envs.push(("MESHFOX_ENVIRONMENT_NAME".to_string(), name.to_string()));
    }
    if let Some(var) = output_var {
        envs.push(("MESHFOX_ENVIRONMENT_VAR".to_string(), var.to_string()));
    }
    envs.push(("MESHFOX_ENV_NAMES".to_string(), env_names.join(",")));
    Ok(Some((interpreter_path, envs)))
}

/// `.meshfox/<canvas filename>.<suffix>` (`venv` for `@python_venv`, `node` for
/// `@node_env`) next to the canvas file — same
/// colocation convention `crate::varcache::cache_path` already uses for
/// `<filename>.env`, just a directory instead of a dotenv file. For a canvas
/// that can't be written, a scratch directory under the system temp dir
/// instead (see below). See
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
pub(crate) fn canvas_state_dir(canvas_path: &Path, suffix: &str) -> PathBuf {
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
        return read_only_temp_root(&absolute).join(format!("{file_name}.{suffix}"));
    }
    dir.join(".meshfox").join(format!("{file_name}.{suffix}"))
}

/// (A canvas that can't be written keeps its venv/node environment instead
/// under `<system temp>/meshfox-readonly-<hash of the canvas path>/
/// <filename>.<suffix>`, owner-only, so it's reused across runs but never
/// needs the canvas's own directory — and the system cleans it up, rather
/// than meshfox leaving state behind for canvases that are long gone. Keyed
/// by the full path, since unlike the colocated directory nothing here
/// separates two canvases that happen to share a file name.)
/// `<system temp>/meshfox-readonly-<uid>-<hash of the canvas path>`, created
/// owner-only and checked to be this user's own — the one place state for a
/// canvas that can't be written may live on disk (its venv, `display="table"` caches). `absolute`
/// is the canvas's absolute path.
pub fn read_only_temp_root(absolute: &Path) -> PathBuf {
    let key = fnv1a(absolute.to_string_lossy().as_bytes());
    read_only_root_in(&std::env::temp_dir(), key)
}

/// `read_only_temp_root` under `temp`. The system temp dir can be shared with
/// other users and the name is guessable, so a directory already there is
/// used only if it is a real directory (not a symlink) of this user that
/// nobody else can enter; anything else — somebody else's, or open to
/// others — is left alone and a fresh private directory is used instead
/// (this canvas then does not share its environment with an earlier run).
fn read_only_root_in(temp: &Path, key: u32) -> PathBuf {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt};
    // SAFETY: `geteuid` takes no arguments and can't fail.
    let uid = unsafe { libc::geteuid() };
    let private = |path: &Path| match std::fs::DirBuilder::new().mode(0o700).create(path) {
        Ok(()) => true,
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => std::fs::symlink_metadata(path)
            .is_ok_and(|m| m.is_dir() && m.uid() == uid && m.mode() & 0o077 == 0),
        Err(_) => false,
    };
    let stable = temp.join(format!("meshfox-readonly-{uid}-{key:08x}"));
    if private(&stable) {
        return stable;
    }
    let mut fresh = stable;
    for attempt in 0..8u32 {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        fresh = temp.join(format!(
            "meshfox-readonly-{uid}-{key:08x}-{}-{nanos}-{attempt}",
            std::process::id()
        ));
        if private(&fresh) {
            break;
        }
    }
    fresh
}

/// True if `name` (without the `@`, e.g. `"agent"`) is a known builtin —
/// for a caller that wants to check without also materializing a script to
/// disk (e.g. deciding whether to bother loading config at all).
pub fn is_builtin(spec: &str) -> bool {
    let Ok((spec, _, _)) = environment_options(spec) else {
        return false;
    };
    spec == "@service"
        || spec
            .strip_prefix('@')
            .is_some_and(|name| lookup(name).is_some())
}

/// Execution identity of a builtin, excluding unrelated machine settings.
/// Pure counterpart used by tests without reading the developer's config.
pub fn execution_identity(spec: &str, config: &toml::Table) -> Option<String> {
    let (spec, _, _) = environment_options(spec).ok()?;
    let builtin = lookup(spec.strip_prefix('@')?)?;
    let mut hash = blake3::Hasher::new();
    hash.update(builtin.script.as_bytes());
    if builtin.name == "agent" {
        // Project exactly the value exported to agent.sh (including key
        // normalization and the same last-value precedence as envs()).
        let flat = crate::config::flatten_to_env(config);
        let get = |name: &str| {
            flat.iter()
                .rev()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.clone())
                .filter(|s| !s.is_empty())
        };
        let provider = get("MESHFOX_CONFIG_INTERPRETERS_AGENT_PROVIDER")
            .unwrap_or_else(|| "claude".to_string());
        hash.update(provider.as_bytes());
        if provider == "custom" {
            // The free-form commands are part of what the block runs.
            for name in [
                "MESHFOX_CONFIG_INTERPRETERS_AGENT_COMMAND",
                "MESHFOX_CONFIG_INTERPRETERS_AGENT_TTY_COMMAND",
            ] {
                hash.update(b"\0");
                hash.update(get(name).unwrap_or_default().as_bytes());
            }
        }
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

/// Runtime cache identity, including the resolved builtin and the configuration
/// loaded from the same cwd as execution. The authored output hash stays static.
pub fn execution_fingerprint(
    block: &crate::fence::CodeBlock,
    resolved_vars: &std::collections::HashMap<String, String>,
    cwd: &std::path::Path,
) -> String {
    let base = crate::fence::session_fingerprint(block, resolved_vars);
    let Some(spec) = crate::args::resolve_interpreter(block, resolved_vars) else {
        return base;
    };
    if !is_builtin(&spec) {
        return base;
    }
    let config = crate::config::load(cwd);
    match execution_identity(&spec, &config) {
        Some(identity) => crate::fence::combine_fingerprints([base, identity]),
        None => base,
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn suggest_builtin_names_match_the_embedded_macros() {
        let names: Vec<&str> = super::BUILTINS.iter().map(|b| b.name).collect();
        assert_eq!(names, crate::suggest::BUILTIN_NAMES);
    }

    use super::*;

    #[test]
    fn named_environments_are_isolated_and_validate_options() {
        let canvas = std::env::temp_dir().join("named-environments.canvas.md");
        for (builtin, directory, suffix) in [
            ("@python_venv", "MESHFOX_VENV_DIR", "venv"),
            ("@node_env", "MESHFOX_NODE_DIR", "node"),
        ] {
            let spec = format!("{builtin} name=xls var=XLS_RUNTIME");
            let spec = crate::exec::resolve_interpreter(&spec, &Default::default());
            assert!(is_builtin(&spec));
            assert!(execution_identity(&spec, &toml::Table::new()).is_some());
            let (_, env) = resolve_with_env(&spec, Some("text"), None, Some(&canvas), &[])
                .unwrap()
                .unwrap();
            let env: std::collections::HashMap<_, _> = env.into_iter().collect();
            assert_eq!(
                env[directory],
                canvas_state_dir(&canvas, suffix)
                    .join(".named")
                    .join("xls")
                    .display()
                    .to_string()
            );
            assert_eq!(env["MESHFOX_ENVIRONMENT_VAR"], "XLS_RUNTIME");
            let (_, plain) = resolve_with_env(builtin, None, None, Some(&canvas), &[])
                .unwrap()
                .unwrap();
            assert_ne!(
                plain.iter().find(|(key, _)| key == directory).unwrap().1,
                env[directory]
            );
        }
        for spec in [
            "@python_venv name=../oops",
            "@python_venv var=X-Y",
            "@python_venv name=",
            "@python_venv name=a name=b",
            "@python_venv unknown=x",
            "@agent name=x",
        ] {
            assert!(resolve_builtin_spec(spec).is_err(), "{spec}");
        }
    }

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
    fn resolve_builtin_spec_is_stable_across_calls() {
        let a = resolve_builtin_spec("@python_venv").unwrap().unwrap();
        let b = resolve_builtin_spec("@python_venv").unwrap().unwrap();
        assert_eq!(a, b);
    }

    /// The command a builtin resolves to, run by `shell` over `input` the way
    /// `crate::exec::resolve_command` does: split into program and arguments,
    /// then the fence body's file last.
    fn builtin_command(spec: &str, shell: &str, input: &Path) -> std::process::Command {
        let resolved = resolve_builtin_spec(spec).unwrap().unwrap();
        let (_, args) = crate::exec::split_interpreter(&resolved).unwrap();
        let mut command = std::process::Command::new(shell);
        command.args(args).arg(input);
        command
    }

    #[test]
    fn a_builtin_resolves_to_bash_dash_c_with_its_embedded_script() {
        for builtin in BUILTINS {
            let spec = format!("@{}", builtin.name);
            let resolved = resolve_builtin_spec(&spec).unwrap().unwrap();
            let (program, args) = crate::exec::split_interpreter(&resolved).unwrap();
            assert_eq!(program, "bash", "{spec}");
            assert_eq!(args, ["-c", builtin.script, builtin.name], "{spec}");
        }
    }

    /// The script is one argv entry (a Linux limit of 128 KiB each, and the
    /// environment shares the total): keep the embedded ones well below it.
    #[test]
    fn every_builtin_script_fits_comfortably_in_one_argument() {
        for builtin in BUILTINS {
            assert!(
                builtin.script.len() < 64 * 1024,
                "{} is {} bytes",
                builtin.name,
                builtin.script.len()
            );
            assert!(!builtin.script.contains('\0'), "{}", builtin.name);
        }
    }

    /// The scripts run as `bash -c`, where `$0` is the builtin's name and
    /// not a path: none may rely on its own location.
    #[test]
    fn no_builtin_script_depends_on_its_own_path() {
        for builtin in BUILTINS {
            assert!(!builtin.script.contains("BASH_SOURCE"), "{}", builtin.name);
            assert!(!builtin.script.contains("$0"), "{}", builtin.name);
            assert!(!builtin.script.contains("${0"), "{}", builtin.name);
        }
    }

    #[test]
    fn resolving_a_builtin_leaves_nothing_in_the_shared_temp_dir() {
        let shared = std::env::temp_dir().join("meshfox-builtins");
        let before = std::fs::metadata(&shared).ok().map(|m| m.modified().ok());
        for builtin in BUILTINS {
            resolve_builtin_spec(&format!("@{}", builtin.name)).unwrap();
        }
        let after = std::fs::metadata(&shared).ok().map(|m| m.modified().ok());
        assert_eq!(
            before,
            after,
            "resolving must not touch {}",
            shared.display()
        );
    }

    /// The directory `read_only_root_in` starts from, for one test: its own
    /// temp dir, and the name an attacker would plant (the key is public).
    #[cfg(unix)]
    fn read_only_fixture(tag: &str) -> (PathBuf, PathBuf) {
        // SAFETY: `geteuid` takes no arguments and can't fail.
        let uid = unsafe { libc::geteuid() };
        let temp =
            std::env::temp_dir().join(format!("meshfox-ro-root-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&temp);
        std::fs::create_dir_all(&temp).unwrap();
        let stable = temp.join(format!("meshfox-readonly-{uid}-{:08x}", 0xabcd_u32));
        (temp, stable)
    }

    #[test]
    #[cfg(unix)]
    fn the_read_only_root_is_a_private_directory_reused_across_calls() {
        use std::os::unix::fs::PermissionsExt;
        let (temp, stable) = read_only_fixture("own");
        let first = read_only_root_in(&temp, 0xabcd);
        assert_eq!(first, stable);
        assert_eq!(
            std::fs::metadata(&first).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(read_only_root_in(&temp, 0xabcd), first);
        std::fs::remove_dir_all(&temp).unwrap();
    }

    #[test]
    #[cfg(unix)]
    fn a_read_only_root_open_to_others_is_not_used() {
        use std::os::unix::fs::PermissionsExt;
        let (temp, stable) = read_only_fixture("open");
        std::fs::create_dir(&stable).unwrap();
        std::fs::set_permissions(&stable, std::fs::Permissions::from_mode(0o777)).unwrap();

        let root = read_only_root_in(&temp, 0xabcd);
        assert_ne!(root, stable);
        assert_eq!(
            std::fs::metadata(&root).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert!(
            std::fs::read_dir(&stable).unwrap().next().is_none(),
            "untouched"
        );
        std::fs::remove_dir_all(&temp).unwrap();
    }

    #[test]
    #[cfg(unix)]
    fn a_read_only_root_that_is_a_symlink_is_not_followed() {
        let (temp, stable) = read_only_fixture("link");
        let elsewhere = temp.join("elsewhere");
        std::fs::create_dir(&elsewhere).unwrap();
        std::os::unix::fs::symlink(&elsewhere, &stable).unwrap();

        let root = read_only_root_in(&temp, 0xabcd);
        assert_ne!(root, stable);
        assert!(root.is_dir() && !root.starts_with(&elsewhere));
        assert!(
            std::fs::read_dir(&elsewhere).unwrap().next().is_none(),
            "untouched"
        );
        std::fs::remove_dir_all(&temp).unwrap();
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
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
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
        let prompt =
            std::env::temp_dir().join(format!("meshfox-agent-sh-test-{}.txt", std::process::id()));
        std::fs::write(&prompt, "hello world").unwrap();

        let output = builtin_command("@agent", "/bin/bash", &prompt)
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
        assert!(envs
            .iter()
            .any(|(k, v)| k == "MESHFOX_ENV_NAMES" && v.is_empty()));

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
        let non_scalar = "[interpreters.agent]\nprovider = ['codex']\n"
            .parse()
            .unwrap();
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
        for provider in ["claude", "codex", "opencode"] {
            let bin = dir.join(provider);
            std::fs::write(&bin, fake).unwrap();
            std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let prompt = dir.join("prompt.txt");
        std::fs::write(&prompt, "A=$A; braced=${A}; B=$B; prefix=$AB; escaped=$$A; unknown=$HOME; price=$5; bad=${A; shell=$(touch NO); unicode=ёж; ctrl=\u{1}").unwrap();
        for shell in ["/bin/bash", "bash"] {
            for provider in ["claude", "codex", "opencode"] {
                for names in ["A,B", "B,A"] {
                    let output = builtin_command("@agent", shell, &prompt)
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
                        match provider {
                            "claude" => b"-p".as_slice(),
                            "codex" => b"exec".as_slice(),
                            _ => b"run".as_slice(),
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

    #[test]
    fn agent_identity_tracks_custom_commands_only_for_custom() {
        let cfg = |s: &str| -> toml::Table { s.parse().unwrap() };
        let a = cfg("[interpreters.agent]\nprovider='custom'\ncommand='a \"$1\"'\n");
        let b = cfg("[interpreters.agent]\nprovider='custom'\ncommand='b \"$1\"'\n");
        let t = cfg(
            "[interpreters.agent]\nprovider='custom'\ncommand='a \"$1\"'\ntty_command='x'\n",
        );
        assert_ne!(execution_identity("@agent", &a), execution_identity("@agent", &b));
        assert_ne!(execution_identity("@agent", &a), execution_identity("@agent", &t));
        let c1 = cfg("[interpreters.agent]\nprovider='codex'\ncommand='a'\n");
        let c2 = cfg("[interpreters.agent]\nprovider='codex'\ncommand='b'\n");
        assert_eq!(execution_identity("@agent", &c1), execution_identity("@agent", &c2));
    }

    #[test]
    #[cfg(unix)]
    fn agent_custom_command_gets_prompt_as_argument_without_injection() {
        let dir = std::env::temp_dir().join(format!("meshfox-agent-custom-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let prompt = dir.join("prompt.txt");
        std::fs::write(&prompt, "hi $(touch NO) \"q\" 'x'").unwrap();
        let run = |cmd: Option<&str>| {
            let mut c = builtin_command("@agent", "bash", &prompt);
            c.env("MESHFOX_CONFIG_INTERPRETERS_AGENT_PROVIDER", "custom")
                .env("MESHFOX_ENV_NAMES", "")
                .current_dir(&dir)
                .stdin(std::process::Stdio::null());
            if let Some(cmd) = cmd {
                c.env("MESHFOX_CONFIG_INTERPRETERS_AGENT_COMMAND", cmd);
            }
            c.output().unwrap()
        };
        let ok = run(Some("printf '%s|%s' \"$1\" \"$MESHFOX_PROMPT\""));
        assert!(ok.status.success());
        let want = "hi $(touch NO) \"q\" 'x'";
        assert_eq!(String::from_utf8_lossy(&ok.stdout), format!("{want}|{want}"));
        assert!(!dir.join("NO").exists());
        let missing = run(None);
        assert!(!missing.status.success());
        assert!(String::from_utf8_lossy(&missing.stderr).contains("interpreters.agent.command"));
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
        for (lang, input) in [("text", &requirements), ("toml", &project)] {
            let output = builtin_command("@python_venv", "bash", input)
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
        let launcher = dir.join("venv/bin/python3-mf");
        assert_eq!(
            std::fs::read_to_string(&vars).unwrap(),
            format!("PYTHON={0}\nPYTHON={0}\n", launcher.display()),
            "the computed PYTHON is the launcher, not the bare venv python"
        );
        assert!(
            std::fs::metadata(&launcher).unwrap().permissions().mode() & 0o111 != 0,
            "launcher is executable"
        );
        let renamed = builtin_command("@python_venv", "bash", &requirements)
            .env("MESHFOX_BLOCK_LANG", "text")
            .env("MESHFOX_VENV_DIR", dir.join("venv"))
            .env("MESHFOX_ENVIRONMENT_VAR", "XLS_PYTHON")
            .env("MESHFOX_VARS_OUT", &vars)
            .env("PIP_CALLS", &calls)
            .output()
            .unwrap();
        assert!(renamed.status.success());
        assert!(std::fs::read_to_string(&vars)
            .unwrap()
            .contains(&format!("XLS_PYTHON={}\n", launcher.display())));
        // The launcher puts the caller's cwd on PYTHONPATH and runs the venv python.
        let probe = dir.join("probe");
        std::fs::create_dir_all(&probe).unwrap();
        std::fs::write(
            &fake_python,
            "#!/bin/sh\nprintf '%s|%s\\n' \"$PYTHONPATH\" \"$*\" > \"$PIP_CALLS\"\n",
        )
        .unwrap();
        let status = std::process::Command::new(&launcher)
            .current_dir(&probe)
            .env("PYTHONPATH", "/already/here")
            .env("PIP_CALLS", &calls)
            .args(["-u", "script.py"])
            .status()
            .unwrap();
        assert!(status.success());
        assert_eq!(
            std::fs::read_to_string(&calls).unwrap().trim_end(),
            format!(
                "{}:/already/here|-u script.py",
                probe.canonicalize().unwrap().display()
            )
        );
        let unsupported = builtin_command("@python_venv", "bash", &project)
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

    /// `@node_env`: package specs from a `text` fence or a whole `json`
    /// package.json, `npm install` only when the declared input changed, and
    /// the `NODE` launcher — run with a fake `npm` so no network is needed.
    #[test]
    #[cfg(unix)]
    fn node_env_installs_only_when_the_input_changed_and_reports_a_launcher() {
        use std::os::unix::fs::PermissionsExt;

        let dir = std::env::temp_dir().join(format!(
            "meshfox-node-env-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let fake_bin = dir.join("bin");
        std::fs::create_dir_all(&fake_bin).unwrap();
        let npm = fake_bin.join("npm");
        std::fs::write(
            &npm,
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$NPM_CALLS\"\nmkdir -p node_modules\n",
        )
        .unwrap();
        std::fs::set_permissions(&npm, std::fs::Permissions::from_mode(0o755)).unwrap();
        let node = fake_bin.join("node");
        std::fs::write(&node, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&node, std::fs::Permissions::from_mode(0o755)).unwrap();

        let env_dir = dir.join("env");
        let calls = dir.join("npm-calls");
        let vars = dir.join("vars");
        let path = format!("{}:{}", fake_bin.display(), std::env::var("PATH").unwrap());
        let run = |lang: &str, body: &str| {
            let input = dir.join("input");
            std::fs::write(&input, body).unwrap();
            let output = builtin_command("@node_env", "bash", &input)
                .env("PATH", &path)
                .env("MESHFOX_BLOCK_LANG", lang)
                .env("MESHFOX_NODE_DIR", &env_dir)
                .env("MESHFOX_VARS_OUT", &vars)
                .env("NPM_CALLS", &calls)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        };
        let npm_calls = || std::fs::read_to_string(&calls).unwrap_or_default();

        run("text", "# comment\npele@1.6.0\n\n  @scope/pkg@^2  \n");
        assert_eq!(
            npm_calls(),
            "install --no-audit --no-fund --save pele@1.6.0 @scope/pkg@^2\n"
        );
        assert_eq!(
            std::fs::read_to_string(env_dir.join("package.json")).unwrap(),
            "{\"private\": true}\n"
        );

        run("text", "# comment\npele@1.6.0\n\n  @scope/pkg@^2  \n");
        assert_eq!(
            npm_calls().lines().count(),
            1,
            "unchanged input: no install"
        );

        run("json", "{\"dependencies\": {\"pele\": \"1.6.0\"}}\n");
        assert_eq!(npm_calls().lines().count(), 2, "changed input: install");
        assert_eq!(
            std::fs::read_to_string(env_dir.join("package.json")).unwrap(),
            "{\"dependencies\": {\"pele\": \"1.6.0\"}}\n"
        );

        let launcher = env_dir.join("node");
        assert_eq!(
            std::fs::read_to_string(&vars).unwrap(),
            format!("NODE={0}\nNODE={0}\nNODE={0}\n", launcher.display())
        );
        let launcher_text = std::fs::read_to_string(&launcher).unwrap();
        assert!(launcher_text.contains(&format!("NODE_PATH=\"{}/node_modules", env_dir.display())));
        assert!(launcher_text.contains("--import"));
        assert!(launcher.metadata().unwrap().permissions().mode() & 0o111 != 0);
        for file in ["register.mjs", "hooks.mjs", "anchor.mjs"] {
            assert!(env_dir.join(file).exists(), "{file}");
        }

        let unsupported = builtin_command("@node_env", "bash", &dir.join("input"))
            .env("MESHFOX_BLOCK_LANG", "yaml")
            .env("MESHFOX_VARS_OUT", &vars)
            .output()
            .unwrap();
        assert_eq!(unsupported.status.code(), Some(2));
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

        let venv = canvas_state_dir(&canvas, "venv");
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
        assert_eq!(venv, canvas_state_dir(&canvas, "venv"));
        assert!(!dir.join(".meshfox").exists());

        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::remove_dir_all(&dir).ok();
        if let Some(root) = venv.parent() {
            std::fs::remove_dir_all(root).ok();
        }
    }
}
