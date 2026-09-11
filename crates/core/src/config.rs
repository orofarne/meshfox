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
