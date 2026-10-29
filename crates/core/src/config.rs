//! meshfox's own settings — distinct from a document's `meshfox:var`s
//! (`crate::vars`): a value here is a fact about the machine/environment
//! (which agent CLI is installed and preferred, say), read from optional
//! `.meshfox/config.toml` files rather than declared or prompted for per canvas.
//!
//! The global config is overlaid by every ancestor's `.meshfox/config.toml`,
//! from the filesystem root to the canonical canvas directory. Nearer scalar
//! keys/arrays win and nested tables merge. `shared_env` resolves `[[env]]`
//! separately per source; `file_roots` preserves owners for relative grants.

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

/// Loads and deep-merges global + ancestor configs (nearest wins
/// key-by-key at every level — see module doc). A file being absent,
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
    let mut merged = toml::Table::new();
    for layer in layers_from(global_path, canvas_root) {
        deep_merge(&mut merged, layer);
    }
    merged
}

/// The configuration files `load` merges, one table each, farthest first
/// (global, then every ancestor down to the canvas directory). For a
/// setting whose entries must be compared across files instead of merged
/// key by key (`crate::open_rules`).
pub fn load_layers(canvas_root: &Path) -> Vec<toml::Table> {
    layers_from(global_config_path().as_deref(), canvas_root)
}

fn layers_from(global_path: Option<&Path>, canvas_root: &Path) -> Vec<toml::Table> {
    global_path
        .map(read_table)
        .into_iter()
        .chain(
            ancestor_config_paths(canvas_root, global_path)
                .iter()
                .map(|path| read_table(path)),
        )
        .collect()
}

/// Parent configurations, from filesystem root to the canvas directory.
/// Resolve symlinks first so opening the same canvas via an alias cannot
/// change its configuration. The global configuration is applied only once.
pub(crate) fn ancestor_config_paths(canvas_root: &Path, global: Option<&Path>) -> Vec<PathBuf> {
    let root = canvas_root.canonicalize().unwrap_or_else(|_| {
        if canvas_root.is_absolute() {
            canvas_root.to_path_buf()
        } else {
            std::env::current_dir()
                .unwrap_or_default()
                .join(canvas_root)
        }
    });
    let global = global.map(|p| p.canonicalize().unwrap_or_else(|_| p.to_path_buf()));
    let mut paths: Vec<_> = root
        .ancestors()
        .map(local_config_path)
        .filter(|p| {
            let canonical = p.canonicalize().unwrap_or_else(|_| p.clone());
            Some(&canonical) != global.as_ref()
        })
        .collect();
    paths.reverse();
    paths
}

/// Explicit filesystem grants. A nearer `files.allow` replaces the entire
/// inherited list (including with `[]`). Paths are relative to the owning
/// directory, not `.meshfox` or the consuming canvas. Invalid configuration
/// fails closed rather than silently falling back to a broader ancestor.
pub fn file_roots(canvas_root: &Path) -> Result<Vec<PathBuf>, String> {
    file_roots_from(global_config_path().as_deref(), canvas_root)
}

fn file_roots_from(global: Option<&Path>, canvas_root: &Path) -> Result<Vec<PathBuf>, String> {
    let mut roots = Vec::new();
    for path in global
        .into_iter()
        .map(Path::to_path_buf)
        .chain(ancestor_config_paths(canvas_root, global))
    {
        let table = read_table_strict(&path)?;
        let Some(files) = table.get("files") else {
            continue;
        };
        let files = files
            .as_table()
            .ok_or_else(|| format!("{}: files must be a table", path.display()))?;
        let Some(allow) = files.get("allow") else {
            continue;
        };
        let allow = allow
            .as_array()
            .ok_or_else(|| format!("{}: files.allow must be an array of paths", path.display()))?;
        let owner = path
            .parent()
            .and_then(Path::parent)
            .unwrap_or(Path::new("/"));
        roots = allow
            .iter()
            .map(|value| {
                let value = value.as_str().filter(|s| !s.is_empty()).ok_or_else(|| {
                    format!(
                        "{}: files.allow entries must be nonempty strings",
                        path.display()
                    )
                })?;
                let expanded = if value == "~" || value.starts_with("~/") {
                    let home = std::env::var_os("HOME").ok_or("cannot expand ~ without HOME")?;
                    PathBuf::from(home).join(value.strip_prefix("~/").unwrap_or(""))
                } else {
                    owner.join(value)
                };
                Ok(expanded)
            })
            .collect::<Result<_, String>>()?;
    }
    Ok(roots)
}

pub(crate) fn read_table(path: &Path) -> toml::Table {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| s.parse::<toml::Table>().ok())
        .unwrap_or_default()
}

/// Like `read_table`, but tells "no such file" (fine: empty) apart from a
/// file that exists and can't be read or parsed (an `Err` with the reason) —
/// for callers that must not treat a typo as "declares nothing".
pub(crate) fn read_table_strict(path: &Path) -> Result<toml::Table, String> {
    match std::fs::read_to_string(path) {
        Ok(s) => s
            .parse::<toml::Table>()
            .map_err(|e| format!("{}: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(toml::Table::new()),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
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
/// doc comment for its fallback. An empty local value disables a global
/// socket for that canvas root.
///
/// `MESHFOX_SERVER_SOCKET` overrides the config file when set, without
/// touching `.meshfox/config.toml`: a path points at a different (or
/// throwaway) daemon, an empty string forces this off even if the config
/// file has a `server_socket` set. A debug-only escape hatch (same spirit
/// as `MESHFOX_TEST_UNTOUCHED_TIMEOUT_SECS` in `meshfox_server`) — not a
/// third config source alongside global/local, so it isn't merged or
/// documented as a regular setting.
pub fn server_socket(canvas_root: &Path) -> Option<PathBuf> {
    if let Ok(over) = std::env::var("MESHFOX_SERVER_SOCKET") {
        return if over.is_empty() {
            None
        } else {
            Some(PathBuf::from(over))
        };
    }
    server_socket_from_table(&load(canvas_root))
}

fn server_socket_from_table(table: &toml::Table) -> Option<PathBuf> {
    table
        .get("server_socket")
        .and_then(|v| v.as_str())
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

/// How the terminal client delivers copied text. Default is OSC 52.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TuiClipboard {
    #[default]
    Terminal,
    System,
    Text,
}

pub fn tui_clipboard(canvas_root: &Path) -> Result<TuiClipboard, String> {
    tui_clipboard_from_table(&load(canvas_root))
}

fn tui_clipboard_from_table(table: &toml::Table) -> Result<TuiClipboard, String> {
    match table.get("tui").and_then(|t| t.get("clipboard")) {
        None => Ok(TuiClipboard::Terminal),
        Some(toml::Value::String(s)) => match s.as_str() {
            "terminal" => Ok(TuiClipboard::Terminal),
            "system" => Ok(TuiClipboard::System),
            "text" => Ok(TuiClipboard::Text),
            _ => Err(format!(
                "invalid tui.clipboard={s:?}; expected terminal, system, or text"
            )),
        },
        Some(_) => Err("tui.clipboard must be a string: terminal, system, or text".into()),
    }
}

/// Configured TUI graphics protocol. Environment overrides belong to the
/// terminal client, whose environment may differ from the worker's.
pub fn tui_image_protocol(canvas_root: &Path) -> Result<Option<String>, String> {
    tui_image_protocol_from_table(&load(canvas_root))
}

fn tui_image_protocol_from_table(table: &toml::Table) -> Result<Option<String>, String> {
    let Some(value) = table.get("tui").and_then(|tui| tui.get("image_protocol")) else {
        return Ok(None);
    };
    let value = value
        .as_str()
        .ok_or("tui.image_protocol must be a string")?;
    match value {
        "auto" | "iterm2" | "sixel" | "kitty" | "halfblocks" => Ok(Some(value.to_owned())),
        _ => Err(format!("invalid tui.image_protocol={value:?}; expected auto, iterm2, sixel, kitty, or halfblocks")),
    }
}

/// Configured `[tui] output_height_pct`: the Output pane's initial height as a
/// percentage of the terminal. Anything but an integer in `1..=90` is ignored,
/// so the TUI falls back to its own default instead of refusing to start.
pub fn tui_output_height_pct(canvas_root: &Path) -> Option<u16> {
    tui_output_height_pct_from_table(&load(canvas_root))
}

fn tui_output_height_pct_from_table(table: &toml::Table) -> Option<u16> {
    let pct = table.get("tui")?.get("output_height_pct")?.as_integer()?;
    u16::try_from(pct).ok().filter(|p| (1..=90).contains(p))
}

/// Default for [`session_max_output_bytes`] — 256 KiB per block, same
/// figure `tty_registry::BYTE_CAPACITY` uses for a pty's own scrollback.
pub const DEFAULT_SESSION_MAX_OUTPUT_BYTES: usize = 256 * 1024;

/// Default for [`session_max_runs_per_block`].
pub const DEFAULT_SESSION_MAX_RUNS_PER_BLOCK: usize = 10;

/// Number of complete chain transcripts retained independently of block history.
pub fn session_max_chain_runs(root: &Path) -> usize {
    session_limit(root, "max_chain_runs", 100).max(1)
}
/// Byte budget for output in one chain transcript, including effective output.
pub fn session_max_chain_output_bytes(root: &Path) -> usize {
    if session_max_output_bytes(root) == 0 {
        return 0;
    }
    session_limit(root, "max_chain_output_bytes", 1024 * 1024)
}
fn session_limit(root: &Path, key: &str, fallback: usize) -> usize {
    load(root)
        .get("session")
        .and_then(|v| v.get(key))
        .and_then(|v| v.as_integer())
        .and_then(|v| usize::try_from(v).ok())
        .unwrap_or(fallback)
}

/// `[session] max_runs_per_block = N` in `.meshfox/config.toml` (same merge
/// as every other setting) — how many finished runs of one block the core
/// keeps in the session database before the oldest are rotated out, run row
/// and stored output together. At least `1`: a `0` (or a value that isn't a
/// non-negative integer) is treated as `1` / the default respectively.
pub fn session_max_runs_per_block(canvas_root: &Path) -> usize {
    session_max_runs_per_block_from_table(&load(canvas_root))
}

fn session_max_runs_per_block_from_table(table: &toml::Table) -> usize {
    table
        .get("session")
        .and_then(|v| v.as_table())
        .and_then(|t| t.get("max_runs_per_block"))
        .and_then(|v| v.as_integer())
        .and_then(|n| usize::try_from(n).ok())
        .map(|n| n.max(1))
        .unwrap_or(DEFAULT_SESSION_MAX_RUNS_PER_BLOCK)
}

/// `[session] max_output_bytes = N` in `.meshfox/config.toml` (local or
/// global, same merge as every other setting here) — how many bytes of *each*
/// finished plain run's output the core keeps in the session database
/// (`.meshfox/<canvas>.session.sqlite3`) so it survives a core restart;
/// past the limit the *oldest* lines are dropped, keeping the tail. How many
/// runs per block are kept is [`session_max_runs_per_block`]. `0`
/// turns persisting run output off entirely. Unset or not a non-negative
/// integer falls back to [`DEFAULT_SESSION_MAX_OUTPUT_BYTES`].
pub fn session_max_output_bytes(canvas_root: &Path) -> usize {
    session_max_output_bytes_from_table(&load(canvas_root))
}

fn session_max_output_bytes_from_table(table: &toml::Table) -> usize {
    table
        .get("session")
        .and_then(|v| v.as_table())
        .and_then(|t| t.get("max_output_bytes"))
        .and_then(|v| v.as_integer())
        .and_then(|n| usize::try_from(n).ok())
        .unwrap_or(DEFAULT_SESSION_MAX_OUTPUT_BYTES)
}

/// Default for [`tables_cache_max_bytes`] — 4 GiB.
pub const DEFAULT_TABLES_CACHE_MAX_BYTES: u64 = 4 * 1024 * 1024 * 1024;

/// `[tables] cache_max_bytes = N` in `.meshfox/config.toml` (local or
/// global, same merge as every other setting here) — the most disk the
/// worker lets `display="table"` caches (imported `.duckdb` files plus
/// DuckDB's spill files) take up; the least recently used unopened tables
/// are evicted past it, and a single table that alone won't fit is refused
/// with an error naming this key. Unset or not a non-negative integer falls
/// back to [`DEFAULT_TABLES_CACHE_MAX_BYTES`].
pub fn tables_cache_max_bytes(canvas_root: &Path) -> u64 {
    tables_cache_max_bytes_from_table(&load(canvas_root))
}

fn tables_cache_max_bytes_from_table(table: &toml::Table) -> u64 {
    table
        .get("tables")
        .and_then(|v| v.as_table())
        .and_then(|t| t.get("cache_max_bytes"))
        .and_then(|v| v.as_integer())
        .and_then(|n| u64::try_from(n).ok())
        .unwrap_or(DEFAULT_TABLES_CACHE_MAX_BYTES)
}

/// `[tables] duckdb_path = "..."` in `.meshfox/config.toml` — the `duckdb`
/// executable `display="table"` drives, when it isn't on `PATH` (see
/// `meshfox_server::tables::locate_duckdb` for the full lookup order).
pub fn tables_duckdb_path(canvas_root: &Path) -> Option<PathBuf> {
    load(canvas_root)
        .get("tables")
        .and_then(|v| v.as_table())
        .and_then(|t| t.get("duckdb_path"))
        .and_then(|v| v.as_str())
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
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
                    if !name.is_empty()
                        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
                    {
                        out.push_str(&std::env::var(name).unwrap_or_default());
                        i += 2 + rel_close + 1; // past the closing '}'
                        continue;
                    }
                }
            } else {
                let name_start = i + 1;
                let mut name_end = name_start;
                while name_end < len
                    && (bytes[name_end].is_ascii_alphanumeric() || bytes[name_end] == b'_')
                {
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

    /// `MESHFOX_SERVER_SOCKET` is process-wide, and tests run on parallel
    /// threads: every test that sets, clears or relies on it being unset
    /// holds this for its whole body, so one can't see another's value.
    static SERVER_SOCKET_ENV: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn lock_server_socket_env() -> std::sync::MutexGuard<'static, ()> {
        SERVER_SOCKET_ENV.lock().unwrap_or_else(|e| e.into_inner())
    }

    #[test]
    fn tui_clipboard_defaults_validates_and_merges() {
        assert_eq!(
            tui_clipboard_from_table(&toml::Table::new()).unwrap(),
            TuiClipboard::Terminal
        );
        for (value, expected) in [
            ("terminal", TuiClipboard::Terminal),
            ("system", TuiClipboard::System),
            ("text", TuiClipboard::Text),
        ] {
            let table = format!("[tui]\nclipboard = \"{value}\"").parse().unwrap();
            assert_eq!(tui_clipboard_from_table(&table).unwrap(), expected);
        }
        for value in ["42", "true", "\"auto\"", "[]"] {
            let table = format!("[tui]\nclipboard = {value}").parse().unwrap();
            assert!(tui_clipboard_from_table(&table).is_err());
        }
        let mut global: toml::Table = "[tui]\nclipboard = \"system\"\ntheme = \"lightfox\""
            .parse()
            .unwrap();
        let local = "[tui]\nclipboard = \"text\"".parse().unwrap();
        deep_merge(&mut global, local);
        assert_eq!(
            tui_clipboard_from_table(&global).unwrap(),
            TuiClipboard::Text
        );
        assert_eq!(global["tui"]["theme"].as_str(), Some("lightfox"));
    }

    #[test]
    fn tui_output_height_pct_accepts_only_sane_integers() {
        let pct = |v: &str| {
            tui_output_height_pct_from_table(
                &format!("[tui]\noutput_height_pct = {v}").parse().unwrap(),
            )
        };
        assert_eq!(pct("35"), Some(35));
        assert_eq!(pct("0"), None);
        assert_eq!(pct("95"), None);
        assert_eq!(pct("-5"), None);
        assert_eq!(pct("\"20\""), None);
        assert_eq!(tui_output_height_pct_from_table(&toml::Table::new()), None);
    }

    #[test]
    fn tui_protocol_uses_merged_config_and_validates_values() {
        let mut global: toml::Table =
            "[tui]\nimage_protocol = \"sixel\"\ntheme = \"base16-mocha.dark\""
                .parse()
                .unwrap();
        let local: toml::Table = "[tui]\nimage_protocol = \"iterm2\"".parse().unwrap();
        assert_eq!(
            tui_image_protocol_from_table(&global).unwrap().as_deref(),
            Some("sixel")
        );
        deep_merge(&mut global, local);
        assert_eq!(
            tui_image_protocol_from_table(&global).unwrap().as_deref(),
            Some("iterm2")
        );
        assert_eq!(global["tui"]["theme"].as_str(), Some("base16-mocha.dark"));
        assert_eq!(
            tui_image_protocol_from_table(&toml::Table::new()).unwrap(),
            None
        );
        for invalid in ["true", "7", "\"unknown\""] {
            let table = format!("[tui]\nimage_protocol = {invalid}")
                .parse()
                .unwrap();
            assert!(tui_image_protocol_from_table(&table).is_err());
        }
    }

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
        let table: toml::Table = "server_socket = \"/tmp/coordinator.sock\"\n"
            .parse()
            .unwrap();
        assert_eq!(
            server_socket_from_table(&table),
            Some(PathBuf::from("/tmp/coordinator.sock"))
        );
    }

    #[test]
    fn session_max_output_bytes_reads_override_and_defaults_otherwise() {
        assert_eq!(
            session_max_output_bytes_from_table(&toml::Table::new()),
            DEFAULT_SESSION_MAX_OUTPUT_BYTES
        );
        let table: toml::Table = "[session]\nmax_output_bytes = 1024".parse().unwrap();
        assert_eq!(session_max_output_bytes_from_table(&table), 1024);
        let table: toml::Table = "[session]\nmax_output_bytes = 0".parse().unwrap();
        assert_eq!(session_max_output_bytes_from_table(&table), 0);
        for bad in [
            "[session]\nmax_output_bytes = -5",
            "[session]\nmax_output_bytes = \"big\"",
        ] {
            let table: toml::Table = bad.parse().unwrap();
            assert_eq!(
                session_max_output_bytes_from_table(&table),
                DEFAULT_SESSION_MAX_OUTPUT_BYTES
            );
        }
    }

    #[test]
    fn session_max_runs_per_block_reads_override_and_clamps_to_one() {
        assert_eq!(
            session_max_runs_per_block_from_table(&toml::Table::new()),
            DEFAULT_SESSION_MAX_RUNS_PER_BLOCK
        );
        let table: toml::Table = "[session]\nmax_runs_per_block = 3".parse().unwrap();
        assert_eq!(session_max_runs_per_block_from_table(&table), 3);
        let table: toml::Table = "[session]\nmax_runs_per_block = 0".parse().unwrap();
        assert_eq!(session_max_runs_per_block_from_table(&table), 1);
        let table: toml::Table = "[session]\nmax_runs_per_block = -2".parse().unwrap();
        assert_eq!(
            session_max_runs_per_block_from_table(&table),
            DEFAULT_SESSION_MAX_RUNS_PER_BLOCK
        );
    }

    #[test]
    fn server_socket_from_table_is_none_when_unset_or_wrong_type() {
        assert_eq!(server_socket_from_table(&toml::Table::new()), None);
        let table: toml::Table = "server_socket = 4\n".parse().unwrap();
        assert_eq!(server_socket_from_table(&table), None);
        let table: toml::Table = "server_socket = \"\"\n".parse().unwrap();
        assert_eq!(server_socket_from_table(&table), None);
    }

    #[test]
    fn server_socket_env_override_takes_a_path_over_the_config_file() {
        let _env = lock_server_socket_env();
        let dir = tempfile_dir();
        std::fs::create_dir_all(dir.join(".meshfox")).unwrap();
        std::fs::write(
            dir.join(".meshfox").join("config.toml"),
            "server_socket = \"/tmp/from-config.sock\"\n",
        )
        .unwrap();
        std::env::set_var("MESHFOX_SERVER_SOCKET", "/tmp/from-env.sock");
        let result = server_socket(&dir);
        std::env::remove_var("MESHFOX_SERVER_SOCKET");
        assert_eq!(result, Some(PathBuf::from("/tmp/from-env.sock")));
    }

    #[test]
    fn server_socket_env_override_empty_string_forces_it_off() {
        let _env = lock_server_socket_env();
        let dir = tempfile_dir();
        std::fs::create_dir_all(dir.join(".meshfox")).unwrap();
        std::fs::write(
            dir.join(".meshfox").join("config.toml"),
            "server_socket = \"/tmp/from-config.sock\"\n",
        )
        .unwrap();
        std::env::set_var("MESHFOX_SERVER_SOCKET", "");
        let result = server_socket(&dir);
        std::env::remove_var("MESHFOX_SERVER_SOCKET");
        assert_eq!(result, None);
    }

    #[test]
    fn server_socket_falls_back_to_config_file_when_env_unset() {
        let _env = lock_server_socket_env();
        let dir = tempfile_dir();
        std::fs::create_dir_all(dir.join(".meshfox")).unwrap();
        std::fs::write(
            dir.join(".meshfox").join("config.toml"),
            "server_socket = \"/tmp/from-config.sock\"\n",
        )
        .unwrap();
        std::env::remove_var("MESHFOX_SERVER_SOCKET");
        assert_eq!(
            server_socket(&dir),
            Some(PathBuf::from("/tmp/from-config.sock"))
        );
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
            vec![(
                "MESHFOX_TEST_ENV_EXPAND_E".to_string(),
                "/base:/extra".to_string()
            )]
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

    #[test]
    fn ancestors_merge_and_file_grants_keep_their_owner_and_replace() {
        let dir = tempfile_dir().canonicalize().unwrap();
        let project = dir.join("project");
        let child = project.join("nested/child");
        std::fs::create_dir_all(project.join(".meshfox")).unwrap();
        std::fs::create_dir_all(child.join(".meshfox")).unwrap();
        let global = dir.join("global.toml");
        std::fs::write(
            &global,
            "[tui]\ntheme = \"global\"\n[files]\nallow = [\"/global\"]\n",
        )
        .unwrap();
        std::fs::write(
            project.join(".meshfox/config.toml"),
            "[tui]\ntheme = \"parent\"\n[files]\nallow = [\"docs\"]\n",
        )
        .unwrap();
        let local = child.join(".meshfox/config.toml");
        std::fs::write(&local, "[tui]\nimage_protocol = \"sixel\"\n").unwrap();
        let merged = load_from(Some(&global), &child);
        assert_eq!(merged["tui"]["theme"].as_str(), Some("parent"));
        assert_eq!(merged["tui"]["image_protocol"].as_str(), Some("sixel"));
        assert_eq!(
            file_roots_from(Some(&global), &child).unwrap(),
            vec![project.join("docs")]
        );
        std::fs::write(&local, "[files]\nallow = [\"own\"]\n").unwrap();
        assert_eq!(
            file_roots_from(Some(&global), &child).unwrap(),
            vec![child.join("own")]
        );
        std::fs::write(&local, "[files]\nallow = []\n").unwrap();
        assert!(file_roots_from(Some(&global), &child).unwrap().is_empty());
        for invalid in [
            "[files]\nallow = true",
            "[files]\nallow = [7]",
            "[files]\nallow = [\"\"]",
            "broken = [",
        ] {
            std::fs::write(&local, invalid).unwrap();
            assert!(file_roots_from(Some(&global), &child).is_err(), "{invalid}");
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn global_config_is_not_applied_twice_through_ancestor_search() {
        let dir = tempfile_dir().canonicalize().unwrap();
        let global = local_config_path(&dir);
        std::fs::create_dir_all(global.parent().unwrap()).unwrap();
        std::fs::write(&global, "[files]\nallow = [\"docs\"]").unwrap();
        let child = dir.join("child");
        std::fs::create_dir_all(&child).unwrap();
        assert!(!ancestor_config_paths(&child, Some(&global)).contains(&global));
        assert_eq!(
            file_roots_from(Some(&global), &child).unwrap(),
            vec![dir.join("docs")]
        );
        #[cfg(unix)]
        {
            let alias = dir.join("alias");
            std::os::unix::fs::symlink(&child, &alias).unwrap();
            assert_eq!(
                ancestor_config_paths(&alias, Some(&global)),
                ancestor_config_paths(&child, Some(&global))
            );
        }
        std::fs::remove_dir_all(dir).unwrap();
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
