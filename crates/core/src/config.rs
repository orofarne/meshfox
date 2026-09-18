//! meshfox's own settings — distinct from a document's `meshfox:var`s
//! (`crate::vars`): a value here is a fact about the machine/environment
//! (which agent CLI is installed and preferred, say), resolved once per
//! invocation from a `.meshfox/config.toml`, not declared or prompted for
//! per-canvas. Currently consumed by `crate::builtin_interpreter`'s `@name`
//! macros only — nothing else reads this yet.
//!
//! Local (`<canvas_root>/.meshfox/config.toml`) merges over global
//! (`~/.meshfox/config.toml`), same locality split `crate::syntax_dirs`
//! already established for custom grammars: local wins key-by-key, at
//! every nesting level, not just the top one — so a global `[agent]
//! provider = "claude"` plus a local file containing only `[agent]
//! max_budget_usd = 0.05` ends up with both fields, not the local file's
//! table wholesale replacing the global one.
//!
//! Skeleton status: loaded and flattened (`flatten_to_env`) here, but only
//! wired into one real caller so far (`stream_exec::spawn_interpreter`'s
//! `@`-builtin path) — see that function's own doc comment.

use std::path::{Path, PathBuf};

/// `~/.meshfox/config.toml` — global, every project. `None` isn't an
/// error, just "no global config this run" (e.g. no `HOME` set).
pub fn global_config_path() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|home| Path::new(&home).join(".meshfox").join("config.toml"))
}

/// `<canvas_root>/.meshfox/config.toml` — local to one project, sibling of
/// `crate::varcache`'s own `<filename>.env` and `crate::syntax_dirs`'s
/// `syntax/` in the same `.meshfox/` directory.
pub fn local_config_path(canvas_root: &Path) -> PathBuf {
    canvas_root.join(".meshfox").join("config.toml")
}

/// Loads and deep-merges global + local config into one table (local wins
/// key-by-key at every level — see module doc). Either file being absent,
/// or unreadable/unparsable, is treated the same as "empty" for that one
/// file rather than an error: this is optional configuration a project may
/// never have, not a required manifest.
pub fn load(canvas_root: &Path) -> toml::Table {
    load_from(global_config_path().as_deref(), canvas_root)
}

/// `load`'s own pure implementation, taking the global path explicitly
/// instead of resolving it from `$HOME` itself — split out so a test can
/// exercise "no global config at all" (`global_path: None`) without
/// depending on whether *this* machine happens to have a real
/// `~/.meshfox/config.toml` (`load_is_empty_when_neither_file_exists`
/// used to, and would spuriously fail on a developer's own machine that
/// has one — see that test's own doc comment).
fn load_from(global_path: Option<&Path>, canvas_root: &Path) -> toml::Table {
    let mut merged = global_path.map(read_table).unwrap_or_default();
    let local = read_table(&local_config_path(canvas_root));
    deep_merge(&mut merged, local);
    merged
}

pub(crate) fn read_table(path: &Path) -> toml::Table {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| s.parse::<toml::Table>().ok())
        .unwrap_or_default()
}

fn deep_merge(base: &mut toml::Table, overlay: toml::Table) {
    for (key, overlay_value) in overlay {
        match (base.get_mut(&key), overlay_value) {
            (Some(toml::Value::Table(base_table)), toml::Value::Table(overlay_table)) => {
                deep_merge(base_table, overlay_table);
            }
            (_, overlay_value) => {
                base.insert(key, overlay_value);
            }
        }
    }
}

/// `server_socket = "/path/to/socket"` in `.meshfox/config.toml` (local or
/// global, same local-wins load every other setting here goes through) —
/// the control-socket path of an external coordinator (see
/// `meshfox_server::watcher_protocol`) that every core-launch operation
/// (`view`, `tui`, `node <op>`, `run`, MCP `debug_*`) should become a pure
/// client of, instead of spawning/discovering a worker of its own. Unset
/// (the default) changes nothing for any caller — see each call site's own
/// doc comment for its fallback.
pub fn server_socket(canvas_root: &Path) -> Option<PathBuf> {
    server_socket_from_table(&load(canvas_root))
}

fn server_socket_from_table(table: &toml::Table) -> Option<PathBuf> {
    table.get("server_socket").and_then(|v| v.as_str()).map(PathBuf::from)
}

/// `[process_env]` in `.meshfox/config.toml` (local or global, same merge
/// as every other setting here) — extra environment variables applied to
/// every spawned block/interpreter process (`stream_exec::spawn_bash`/
/// `spawn_process`/`spawn_interpreter`, and `pty_exec::spawn` for a `tty`
/// block — every real spawn path this crate has, now), on top of whatever
/// that process already inherited. Named distinctly from `crate::shared_env`'s
/// `[[env]]`/`vars=` (an unrelated, older feature — per-path-scoped
/// *defaults for `meshfox:var` resolution*, only reaching a block that
/// explicitly declares `env="NAME"` on its own fence) — this instead
/// always applies, to every spawned process's real OS environment,
/// regardless of anything a block declares. Exists specifically for the
/// case where "whatever it inherited" is missing something a block
/// actually needs — a `meshfox view`/`run` worker spawned by the macOS
/// daemon inherits *launchd's* env, not a login shell's, so
/// `npm`/`cargo`/anything installed under `~/.cargo/bin` or Homebrew is
/// invisible to it even though it's on the user's own interactive `$PATH`.
///
/// A value may reference *any* variable's current value via `$NAME`/
/// `${NAME}` (see `expand_env_refs`) — most usefully its own, to extend
/// rather than replace it (blindly overwriting `PATH` would lose whatever
/// the worker already had), but a plain shell-style export list referring
/// to other variables too (`$HOME`, say) works exactly as it reads:
///
/// ```toml
/// [process_env]
/// PATH = "$HOME/.cargo/bin:$HOME/.local/bin:$PATH:$HOME/bin"
/// ```
///
/// — the same left-to-right accumulation four separate
/// `export PATH="...:$PATH"` lines in a `.zshrc` would produce, just
/// collapsed into the one final value (`[process_env]` isn't itself
/// sequential — there's only one `PATH` key — so a multi-line shell config
/// extending `$PATH` several times over needs its author to fold those
/// into one value by hand, same as it would if written as a single shell
/// assignment).
///
/// Every key becomes a real env var unconditionally otherwise (no implicit
/// `PATH`-only special-casing) — a plain `FOO = "bar"` just sets `FOO=bar`.
/// Only string values are supported; anything else in the table is
/// silently skipped, same as `flatten_to_env`'s own array/datetime skip —
/// there's no established convention for what a non-string
/// `[process_env]` entry would even mean.
pub fn env_overrides(canvas_root: &Path) -> Vec<(String, String)> {
    env_overrides_from_table(&load(canvas_root))
}

fn env_overrides_from_table(table: &toml::Table) -> Vec<(String, String)> {
    let Some(toml::Value::Table(env_table)) = table.get("process_env") else {
        return Vec::new();
    };
    env_table
        .iter()
        .filter_map(|(name, value)| Some((name.clone(), expand_env_refs(value.as_str()?))))
        .collect()
}

/// Replaces every `$NAME`/`${NAME}` reference in `value` with that
/// variable's current value in *this* process's own environment (empty
/// string if unset) — the one substitution an `[process_env]` value gets, letting
/// it read like an ordinary shell export (`PATH = "$HOME/bin:$PATH"`)
/// instead of only ever being able to extend itself. `NAME` follows shell
/// identifier rules (letters/digits/underscore, not starting with a
/// digit); `${NAME}` needs a matching `}` and a valid identifier between
/// the braces or it's left untouched, and the bare `$NAME` form only
/// consumes the longest valid identifier that follows (so `$PATH2`
/// resolves `PATH2`, not `PATH` followed by a literal `2` — same "whole
/// token" reasoning `crate::exec::interpreter_var_refs` already applies
/// elsewhere, just now bounded by where the identifier actually ends
/// rather than by a single expected name). Deliberately not general shell
/// expansion — no command substitution, no quoting rules, nothing else in
/// `value` is touched.
fn expand_env_refs(value: &str) -> String {
    let bytes = value.as_bytes();
    let len = value.len();
    let mut out = String::with_capacity(len);
    let mut i = 0;
    while i < len {
        if bytes[i] == b'$' {
            if i + 1 < len && bytes[i + 1] == b'{' {
                if let Some(rel_close) = value[i + 2..].find('}') {
                    let name = &value[i + 2..i + 2 + rel_close];
                    if !name.is_empty() && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
                        out.push_str(&std::env::var(name).unwrap_or_default());
                        i += 2 + rel_close + 1; // past the closing '}'
                        continue;
                    }
                }
            } else {
                let name_start = i + 1;
                let mut name_end = name_start;
                while name_end < len && (bytes[name_end].is_ascii_alphanumeric() || bytes[name_end] == b'_') {
                    name_end += 1;
                }
                let name = &value[name_start..name_end];
                let starts_with_digit = name.as_bytes().first().is_some_and(u8::is_ascii_digit);
                if !name.is_empty() && !starts_with_digit {
                    out.push_str(&std::env::var(name).unwrap_or_default());
                    i = name_end;
                    continue;
                }
            }
        }
        let ch = value[i..].chars().next().expect("i < len");
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

/// Flattens a config table into `MESHFOX_CONFIG_<PATH>` env-var pairs —
/// `[interpreters.agent] provider = "codex"` becomes
/// `MESHFOX_CONFIG_INTERPRETERS_AGENT_PROVIDER=codex`, dashes in a key
/// becoming underscores same as everything else. Only scalar leaves
/// (string/integer/float/boolean) are exported; a nested table recurses,
/// an array or datetime is silently skipped — nothing any `@name` macro
/// reads today needs either shape, and there's no established `env=`-style
/// convention yet for what a list-valued config entry should even look
/// like as one env var.
pub fn flatten_to_env(table: &toml::Table) -> Vec<(String, String)> {
    let mut out = Vec::new();
    flatten_into(table, "MESHFOX_CONFIG", &mut out);
    out
}

fn flatten_into(table: &toml::Table, prefix: &str, out: &mut Vec<(String, String)>) {
    for (key, value) in table {
        let var_name = format!("{prefix}_{}", key.to_uppercase().replace('-', "_"));
        match value {
            toml::Value::Table(nested) => flatten_into(nested, &var_name, out),
            toml::Value::String(s) => out.push((var_name, s.clone())),
            toml::Value::Integer(i) => out.push((var_name, i.to_string())),
            toml::Value::Float(f) => out.push((var_name, f.to_string())),
            toml::Value::Boolean(b) => out.push((var_name, b.to_string())),
            toml::Value::Array(_) | toml::Value::Datetime(_) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flatten_to_env_joins_nested_keys_with_underscores() {
        let table: toml::Table = "[interpreters.agent]\nprovider = \"codex\"\n"
            .parse()
            .unwrap();
        assert_eq!(
            flatten_to_env(&table),
            vec![(
                "MESHFOX_CONFIG_INTERPRETERS_AGENT_PROVIDER".to_string(),
                "codex".to_string()
            )]
        );
    }

    #[test]
    fn flatten_to_env_stringifies_non_string_scalars() {
        let table: toml::Table = "[a]\ncount = 3\nverbose = true\nratio = 0.5\n"
            .parse()
            .unwrap();
        let mut pairs = flatten_to_env(&table);
        pairs.sort();
        assert_eq!(
            pairs,
            vec![
                ("MESHFOX_CONFIG_A_COUNT".to_string(), "3".to_string()),
                ("MESHFOX_CONFIG_A_RATIO".to_string(), "0.5".to_string()),
                ("MESHFOX_CONFIG_A_VERBOSE".to_string(), "true".to_string()),
            ]
        );
    }

    #[test]
    fn flatten_to_env_skips_arrays() {
        let table: toml::Table = "list = [1, 2, 3]\n".parse().unwrap();
        assert!(flatten_to_env(&table).is_empty());
    }

    #[test]
    fn server_socket_from_table_reads_the_top_level_string_key() {
        let table: toml::Table = "server_socket = \"/tmp/coordinator.sock\"\n".parse().unwrap();
        assert_eq!(
            server_socket_from_table(&table),
            Some(PathBuf::from("/tmp/coordinator.sock"))
        );
    }

    #[test]
    fn server_socket_from_table_is_none_when_unset_or_wrong_type() {
        assert_eq!(server_socket_from_table(&toml::Table::new()), None);
        let table: toml::Table = "server_socket = 4\n".parse().unwrap();
        assert_eq!(server_socket_from_table(&table), None);
    }

    #[test]
    fn expand_env_refs_extends_an_existing_value_via_bare_dollar_name() {
        std::env::set_var("MESHFOX_TEST_ENV_EXPAND_A", "/base");
        let out = expand_env_refs("$MESHFOX_TEST_ENV_EXPAND_A:/extra");
        std::env::remove_var("MESHFOX_TEST_ENV_EXPAND_A");
        assert_eq!(out, "/base:/extra");
    }

    #[test]
    fn expand_env_refs_extends_via_the_braced_form() {
        std::env::set_var("MESHFOX_TEST_ENV_EXPAND_B", "/base");
        let out = expand_env_refs("${MESHFOX_TEST_ENV_EXPAND_B}:/extra");
        std::env::remove_var("MESHFOX_TEST_ENV_EXPAND_B");
        assert_eq!(out, "/base:/extra");
    }

    #[test]
    fn expand_env_refs_is_empty_string_when_the_variable_is_unset() {
        std::env::remove_var("MESHFOX_TEST_ENV_EXPAND_UNSET_C");
        let out = expand_env_refs("$MESHFOX_TEST_ENV_EXPAND_UNSET_C:/extra");
        assert_eq!(out, ":/extra");
    }

    #[test]
    fn expand_env_refs_consumes_the_longest_valid_identifier_not_a_fixed_name() {
        std::env::set_var("MESHFOX_TEST_ENV_EXPAND_D", "short");
        std::env::set_var("MESHFOX_TEST_ENV_EXPAND_D2", "long");
        let out = expand_env_refs("$MESHFOX_TEST_ENV_EXPAND_D2/x");
        std::env::remove_var("MESHFOX_TEST_ENV_EXPAND_D");
        std::env::remove_var("MESHFOX_TEST_ENV_EXPAND_D2");
        assert_eq!(out, "long/x");
    }

    /// The motivating real-world case: a `~/.zshrc` with several
    /// `export PATH="...:$PATH"`-style lines collapsed into one `[process_env]`
    /// value, referencing *other* variables (`$HOME`) as well as itself.
    #[test]
    fn expand_env_refs_resolves_multiple_distinct_variables_in_one_value() {
        std::env::set_var("MESHFOX_TEST_ENV_EXPAND_HOME", "/Users/test");
        std::env::set_var("MESHFOX_TEST_ENV_EXPAND_PATH", "/usr/bin");
        let out = expand_env_refs(
            "$MESHFOX_TEST_ENV_EXPAND_HOME/.cargo/bin:$MESHFOX_TEST_ENV_EXPAND_PATH:$MESHFOX_TEST_ENV_EXPAND_HOME/bin",
        );
        std::env::remove_var("MESHFOX_TEST_ENV_EXPAND_HOME");
        std::env::remove_var("MESHFOX_TEST_ENV_EXPAND_PATH");
        assert_eq!(out, "/Users/test/.cargo/bin:/usr/bin:/Users/test/bin");
    }

    #[test]
    fn env_overrides_from_table_expands_and_skips_non_string_values() {
        std::env::set_var("MESHFOX_TEST_ENV_EXPAND_E", "/base");
        let table: toml::Table = "[process_env]\nMESHFOX_TEST_ENV_EXPAND_E = \"$MESHFOX_TEST_ENV_EXPAND_E:/extra\"\ncount = 3\n"
            .parse()
            .unwrap();
        let overrides = env_overrides_from_table(&table);
        std::env::remove_var("MESHFOX_TEST_ENV_EXPAND_E");
        assert_eq!(
            overrides,
            vec![("MESHFOX_TEST_ENV_EXPAND_E".to_string(), "/base:/extra".to_string())]
        );
    }

    #[test]
    fn env_overrides_from_table_is_empty_without_an_env_table() {
        let table: toml::Table = "server_socket = \"/tmp/x.sock\"\n".parse().unwrap();
        assert!(env_overrides_from_table(&table).is_empty());
    }

    #[test]
    fn deep_merge_overlays_a_leaf_without_dropping_sibling_keys() {
        let mut base: toml::Table = "[agent]\nprovider = \"claude\"\ntimeout = 30\n"
            .parse()
            .unwrap();
        let overlay: toml::Table = "[agent]\nprovider = \"codex\"\n".parse().unwrap();
        deep_merge(&mut base, overlay);
        let table: toml::Table = "[agent]\nprovider = \"codex\"\ntimeout = 30\n"
            .parse()
            .unwrap();
        assert_eq!(base, table);
    }

    /// Exercises `load_from` directly with `global_path: None` — plain
    /// `load` always *also* merges this machine's real
    /// `~/.meshfox/config.toml` (via `global_config_path`), so a developer
    /// who actually has one (not unusual — this repo's own `CLAUDE.md`
    /// tells an agent to keep one updated) would otherwise make this test
    /// spuriously fail on their own machine for a reason that has nothing
    /// to do with what it's meant to check: a directory with no *local*
    /// config either really does load empty.
    #[test]
    fn load_is_empty_when_neither_file_exists() {
        let dir = tempfile_dir();
        assert!(load_from(None, &dir).is_empty());
    }

    fn tempfile_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "meshfox-config-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }
}
