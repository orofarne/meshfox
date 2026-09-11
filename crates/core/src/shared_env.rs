//! Shared/global env-var inheritance for `meshfox:var` resolution —
//! project- and user-level defaults for values many canvases need in
//! common (DB credentials, etc.), so they don't have to be re-declared
//! per document. See `crate::vars::resolve_with_shared` for how this
//! participates in resolution, and SPEC.md's "Variables" section for the
//! user-facing writeup.
//!
//! Reuses the same two files `crate::config` already loads
//! (`~/.meshfox/config.toml` global, `<canvas_root>/.meshfox/config.toml`
//! local), under a new top-level `env` array-of-tables key — kept
//! separate from `config::load`'s own whole-table deep-merge (that's for
//! flat tool settings; this needs per-entry path scoping and a
//! project-always-wins-over-global tier order a blind table merge can't
//! express).

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};

/// Where a resolved shared value came from — attached to `SharedVar` and,
/// via `crate::vars::ResolvedVars::origins`/`BlockEnvResolution::origins`,
/// surfaced to a caller (web/TUI) that wants to show "this value was
/// inherited" next to a field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SharedOrigin {
    /// Won from `<canvas_root>/.meshfox/config.toml`.
    Project,
    /// Won from `~/.meshfox/config.toml`. `path` is the raw `path=` string
    /// as written in the matching `[[env]]` section (not canonicalized —
    /// this is for display only), `None` for an unscoped section.
    Global { path: Option<String> },
}

/// One resolved shared variable: its value plus where it came from.
#[derive(Debug, Clone, PartialEq)]
pub struct SharedVar {
    pub value: String,
    pub origin: SharedOrigin,
}

/// name -> resolved shared value, for one `canvas_root`.
pub type SharedEnv = HashMap<String, SharedVar>;

/// A section's own `path=` scoping, already resolved (see
/// `resolve_scope_path`) — kept as three explicit states rather than a
/// bare `Option<PathBuf>` so a malformed `path=` (wrong type, an empty
/// list, or a `~`-relative entry with no `$HOME`) fails *closed*
/// (`Unresolvable`, never matches) instead of being indistinguishable
/// from "no `path=` at all" and silently leaking a value meant to be
/// scoped into every canvas.
enum Scope {
    Unscoped,
    /// One or more resolved scope paths — `path=` can be written as
    /// either a single string or an array of strings (see
    /// `parse_env_sections`); the section matches if `normalized_root`
    /// falls under *any* of them (see `matching_specificity`). Always
    /// non-empty when this variant is used.
    Paths(Vec<PathBuf>),
    Unresolvable,
}

/// One `[[env]]` section as written in a config file, already resolved
/// against a base path but not yet matched against any particular
/// `canvas_root`.
struct EnvSection {
    /// The raw `path=` value as written, for display in a
    /// `SharedOrigin::Global` — joined with `", "` when `path=` was an
    /// array of more than one string. `None` when the section has no
    /// `path=` at all.
    raw_path: Option<String>,
    scope: Scope,
    vars: HashMap<String, String>,
}

/// Loads and precedence-resolves global + local `[[env]]` sections for a
/// canvas rooted at `canvas_root` into one flat `SharedEnv`. Re-reads both
/// config files from disk on every call, same "small file, cheap to
/// reread" tradeoff `config::load` already makes: editing
/// `~/.meshfox/config.toml` while a `meshfox tui`/`serve` is running takes
/// effect on the very next resolution, no restart needed.
pub fn load(canvas_root: &Path) -> SharedEnv {
    load_from(
        crate::config::global_config_path().as_deref(),
        canvas_root,
        home_dir().as_deref(),
    )
}

/// `load`'s own pure implementation, taking the global config path and
/// `$HOME` explicitly instead of resolving them from the environment
/// itself — split out so a test can exercise a specific global-
/// config/`$HOME` combination without depending on the developer's own
/// machine (same reasoning as `config::load_from`'s own doc comment).
fn load_from(global_path: Option<&Path>, canvas_root: &Path, home: Option<&Path>) -> SharedEnv {
    let global_table = global_path.map(crate::config::read_table).unwrap_or_default();
    let local_table = crate::config::read_table(&crate::config::local_config_path(canvas_root));

    // The global tier's own relative (non-`~`) paths have no anchor other
    // than `$HOME` — there's no "directory the file lives in" to resolve
    // against the way the local tier has `canvas_root`.
    let global_sections = parse_env_sections(&global_table, home, home);
    let local_sections = parse_env_sections(&local_table, home, Some(canvas_root));

    let normalized_root = normalize(canvas_root);

    let mut out: SharedEnv = HashMap::new();
    // Global tier first — the local tier is applied second so it
    // unconditionally overlays the global result regardless of
    // specificity: a project's own config always wins over whatever's in
    // the user's global registry, the same "override" escape hatch the
    // per-document cache already provides on top of both.
    apply_tier(&global_sections, &normalized_root, &mut out, |raw_path| SharedOrigin::Global {
        path: raw_path,
    });
    apply_tier(&local_sections, &normalized_root, &mut out, |_raw_path| SharedOrigin::Project);
    out
}

/// Resolves one tier's own `[[env]]` sections against `normalized_root`
/// and writes the winning value for each name into `out`. Within a single
/// tier, the *most specific* matching section wins for a given name (an
/// unscoped section is the least specific, specificity 0); on a
/// specificity tie the *later* section (in file/array order) wins — same
/// "later overlay wins" convention `config::deep_merge` already uses.
fn apply_tier(
    sections: &[EnvSection],
    normalized_root: &Path,
    out: &mut SharedEnv,
    origin_for: impl Fn(Option<String>) -> SharedOrigin,
) {
    // (specificity, section index) chosen so far, per variable name.
    // Comparing the tuple lexicographically gets both tie-break rules for
    // free: higher specificity always wins; on a tie, the higher (later)
    // index wins.
    let mut winners: HashMap<&str, (usize, usize)> = HashMap::new();
    for (idx, section) in sections.iter().enumerate() {
        let Some(specificity) = matching_specificity(section, normalized_root) else {
            continue;
        };
        for name in section.vars.keys() {
            let candidate = (specificity, idx);
            match winners.get(name.as_str()) {
                Some(&current) if current >= candidate => {}
                _ => {
                    winners.insert(name.as_str(), candidate);
                }
            }
        }
    }
    for (name, (_, idx)) in winners {
        let section = &sections[idx];
        if let Some(value) = section.vars.get(name) {
            out.insert(
                name.to_string(),
                SharedVar { value: value.clone(), origin: origin_for(section.raw_path.clone()) },
            );
        }
    }
}

/// `Some(specificity)` if `section` applies to a canvas rooted at
/// (already-normalized) `normalized_root` — `None` if it doesn't match at
/// all. Unscoped always matches (specificity 0); `Unresolvable` never
/// matches; a `Paths` scope matches if `normalized_root` falls under *any*
/// one of them, by path *component*, not raw string prefix (`.../projectA`
/// must not match a scope of `.../projectAB`) — specificity is the
/// *most specific* matching alternative's own component count (deeper =
/// more specific), so e.g. `path = ["~/work", "~/work/projectA/sub"]`
/// scores as the latter, more specific entry for a canvas under `sub`.
fn matching_specificity(section: &EnvSection, normalized_root: &Path) -> Option<usize> {
    match &section.scope {
        Scope::Unscoped => Some(0),
        Scope::Unresolvable => None,
        Scope::Paths(scope_paths) => scope_paths
            .iter()
            .filter_map(|scope_path| {
                let normalized_scope = normalize(scope_path);
                normalized_root
                    .starts_with(&normalized_scope)
                    .then(|| normalized_scope.components().count())
            })
            .max(),
    }
}

/// Resolves symlinks via `canonicalize` when possible; falls back to a
/// purely lexical collapse of `.`/`..` components when the path doesn't
/// exist on this machine (e.g. a synced dotfile referencing another
/// machine's project layout) — one dead scope entry shouldn't make every
/// other entry in the same file unmatchable.
fn normalize(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| lexical_normalize(path))
}

fn lexical_normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

/// Reads `table["env"]` as a list of `[[env]]` sections, resolving each
/// one's `path=` (if any — a single string, or an array of strings, see
/// `Scope::Paths`) against `home`/`relative_base` (see
/// `resolve_scope_path`) — tolerant of every malformed shape (`env` not
/// an array, an entry not a table, `vars` not a table, a non-scalar
/// `vars` leaf, a non-string element inside a `path=` array, silently
/// dropped) the same way the rest of `config.rs` treats a missing/
/// unparsable file as simply empty. A `path=` present but of the wrong
/// type entirely, an empty array, or one that resolves to nothing at all
/// (see `Scope::Unresolvable`), yields a section that parses fine but
/// never matches anything, rather than being dropped or treated as
/// unscoped.
fn parse_env_sections(
    table: &toml::Table,
    home: Option<&Path>,
    relative_base: Option<&Path>,
) -> Vec<EnvSection> {
    let Some(toml::Value::Array(entries)) = table.get("env") else {
        return Vec::new();
    };
    entries
        .iter()
        .filter_map(|entry| {
            let toml::Value::Table(entry) = entry else { return None };
            let (raw_path, scope) = parse_path_scope(entry.get("path"), home, relative_base);
            let vars = entry
                .get("vars")
                .and_then(|v| v.as_table())
                .map(flatten_vars)
                .unwrap_or_default();
            Some(EnvSection { raw_path, scope, vars })
        })
        .collect()
}

/// Parses one section's `path=` value (`None` if the key is absent) into
/// its display string and resolved `Scope` — accepts either a single
/// string (`path = "~/work/projectA"`) or an array of strings (`path =
/// ["~/work/projectA", "~/work/projectB"]`, matching if the canvas is
/// under *any* of them — see `Scope::Paths`). A non-string element inside
/// the array is silently skipped, same tolerance every other malformed
/// shape in this module gets; an array with nothing left after that (or a
/// `path=` of some other type entirely — a number, a bool, a table) fails
/// closed as `Scope::Unresolvable`, never `Unscoped`.
fn parse_path_scope(
    path_value: Option<&toml::Value>,
    home: Option<&Path>,
    relative_base: Option<&Path>,
) -> (Option<String>, Scope) {
    let raw_strings: Vec<String> = match path_value {
        None => return (None, Scope::Unscoped),
        Some(toml::Value::String(s)) => vec![s.clone()],
        Some(toml::Value::Array(items)) => {
            items.iter().filter_map(|v| v.as_str().map(str::to_string)).collect()
        }
        Some(_) => Vec::new(),
    };
    let raw_path = (!raw_strings.is_empty()).then(|| raw_strings.join(", "));
    let resolved: Vec<PathBuf> = raw_strings
        .iter()
        .filter_map(|s| resolve_scope_path(s, home, relative_base))
        .collect();
    let scope = if resolved.is_empty() { Scope::Unresolvable } else { Scope::Paths(resolved) };
    (raw_path, scope)
}

fn flatten_vars(table: &toml::Table) -> HashMap<String, String> {
    table.iter().filter_map(|(k, v)| scalar_to_string(v).map(|s| (k.clone(), s))).collect()
}

fn scalar_to_string(value: &toml::Value) -> Option<String> {
    match value {
        toml::Value::String(s) => Some(s.clone()),
        toml::Value::Integer(i) => Some(i.to_string()),
        toml::Value::Float(f) => Some(f.to_string()),
        toml::Value::Boolean(b) => Some(b.to_string()),
        toml::Value::Array(_) | toml::Value::Datetime(_) | toml::Value::Table(_) => None,
    }
}

/// Resolves one section's raw `path=` string to a `PathBuf` — `~`/`~/...`
/// expands against `home` (`None` there means this can never match, e.g.
/// no `$HOME`); any other relative path resolves against `relative_base`
/// (the global tier passes its own `home` as the base too — there's no
/// other natural anchor for a user-wide file; the local tier passes
/// `canvas_root`); an already-absolute path is used as-is.
fn resolve_scope_path(raw: &str, home: Option<&Path>, relative_base: Option<&Path>) -> Option<PathBuf> {
    if raw == "~" {
        return home.map(Path::to_path_buf);
    }
    if let Some(rest) = raw.strip_prefix("~/") {
        return home.map(|h| h.join(rest));
    }
    let path = Path::new(raw);
    if path.is_absolute() {
        return Some(path.to_path_buf());
    }
    relative_base.map(|b| b.join(path))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tempdir(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "meshfox-shared-env-test-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write(path: &Path, contents: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }

    #[test]
    fn load_is_empty_when_neither_file_exists() {
        let root = tempdir("empty");
        assert!(load_from(None, &root, None).is_empty());
    }

    #[test]
    fn unscoped_global_section_applies_everywhere() {
        let home = tempdir("home-unscoped");
        let global = home.join(".meshfox").join("config.toml");
        write(&global, "[[env]]\nvars = { DB_USER = \"alice\" }\n");
        let root = tempdir("project-unscoped");

        let shared = load_from(Some(&global), &root, Some(&home));
        assert_eq!(shared.get("DB_USER").map(|v| v.value.as_str()), Some("alice"));
        assert_eq!(shared.get("DB_USER").unwrap().origin, SharedOrigin::Global { path: None });
    }

    #[test]
    fn scoped_global_section_only_matches_under_its_path() {
        let home = tempdir("home-scoped");
        let project_a = home.join("work").join("projectA");
        std::fs::create_dir_all(&project_a).unwrap();
        let sibling = home.join("work").join("projectAB");
        std::fs::create_dir_all(&sibling).unwrap();

        let global = home.join(".meshfox").join("config.toml");
        write(
            &global,
            &format!(
                "[[env]]\npath = {:?}\nvars = {{ DB_URL = \"projA\" }}\n",
                project_a.to_string_lossy()
            ),
        );

        let matching = load_from(Some(&global), &project_a, Some(&home));
        assert_eq!(matching.get("DB_URL").map(|v| v.value.as_str()), Some("projA"));

        // A sibling directory whose name merely starts with the same
        // string must NOT match -- this is exactly why matching has to be
        // component-wise, not a raw string prefix.
        let non_matching = load_from(Some(&global), &sibling, Some(&home));
        assert_eq!(non_matching.get("DB_URL"), None);
    }

    #[test]
    fn more_specific_global_section_wins_over_less_specific() {
        let home = tempdir("home-specificity");
        let project_a = home.join("work").join("projectA");
        let sub = project_a.join("sub-service");
        std::fs::create_dir_all(&sub).unwrap();

        let global = home.join(".meshfox").join("config.toml");
        write(
            &global,
            &format!(
                concat!(
                    "[[env]]\npath = {:?}\nvars = {{ DB_URL = \"projA\" }}\n\n",
                    "[[env]]\npath = {:?}\nvars = {{ DB_URL = \"sub\" }}\n",
                ),
                project_a.to_string_lossy(),
                sub.to_string_lossy(),
            ),
        );

        let shared = load_from(Some(&global), &sub, Some(&home));
        assert_eq!(shared.get("DB_URL").map(|v| v.value.as_str()), Some("sub"));

        let shared_parent = load_from(Some(&global), &project_a, Some(&home));
        assert_eq!(shared_parent.get("DB_URL").map(|v| v.value.as_str()), Some("projA"));
    }

    #[test]
    fn local_project_config_overrides_global_regardless_of_specificity() {
        let home = tempdir("home-override");
        let root = tempdir("project-override");

        let global = home.join(".meshfox").join("config.toml");
        write(
            &global,
            &format!(
                "[[env]]\npath = {:?}\nvars = {{ DB_URL = \"global-scoped\" }}\n",
                root.to_string_lossy()
            ),
        );
        write(&root.join(".meshfox").join("config.toml"), "[[env]]\nvars = { DB_URL = \"local\" }\n");

        let shared = load_from(Some(&global), &root, Some(&home));
        assert_eq!(shared.get("DB_URL").map(|v| v.value.as_str()), Some("local"));
        assert_eq!(shared.get("DB_URL").unwrap().origin, SharedOrigin::Project);
    }

    #[test]
    fn tilde_path_expands_against_home() {
        let home = tempdir("home-tilde");
        let project = home.join("work").join("projectA");
        std::fs::create_dir_all(&project).unwrap();

        let global = home.join(".meshfox").join("config.toml");
        write(&global, "[[env]]\npath = \"~/work/projectA\"\nvars = { DB_URL = \"projA\" }\n");

        let shared = load_from(Some(&global), &project, Some(&home));
        assert_eq!(shared.get("DB_URL").map(|v| v.value.as_str()), Some("projA"));
    }

    #[test]
    fn tie_in_specificity_lets_the_later_section_win() {
        let home = tempdir("home-tie");
        let root = tempdir("project-tie");
        let global = home.join(".meshfox").join("config.toml");
        write(&global, "[[env]]\nvars = { X = \"first\" }\n\n[[env]]\nvars = { X = \"second\" }\n");

        let shared = load_from(Some(&global), &root, Some(&home));
        assert_eq!(shared.get("X").map(|v| v.value.as_str()), Some("second"));
    }

    #[test]
    fn a_malformed_path_fails_closed_instead_of_leaking_as_unscoped() {
        let home = tempdir("home-malformed");
        let root = tempdir("project-malformed");
        let global = home.join(".meshfox").join("config.toml");
        write(
            &global,
            concat!(
                "[[env]]\nvars = { LIST = [1, 2, 3], OK = \"kept\" }\n\n",
                "[[env]]\npath = 5\nvars = { LEAKED = \"no\" }\n",
            ),
        );

        let shared = load_from(Some(&global), &root, Some(&home));
        assert_eq!(shared.get("LIST"), None, "non-scalar vars leaf is dropped");
        assert_eq!(shared.get("OK").map(|v| v.value.as_str()), Some("kept"));
        assert_eq!(shared.get("LEAKED"), None, "a non-string path= must not fall back to unscoped");
    }

    #[test]
    fn path_accepts_an_array_matching_any_entry() {
        let home = tempdir("home-array");
        let project_a = home.join("work").join("projectA");
        let project_b = home.join("work").join("projectB");
        let unrelated = home.join("work").join("projectC");
        std::fs::create_dir_all(&project_a).unwrap();
        std::fs::create_dir_all(&project_b).unwrap();
        std::fs::create_dir_all(&unrelated).unwrap();

        let global = home.join(".meshfox").join("config.toml");
        write(
            &global,
            &format!(
                "[[env]]\npath = [{:?}, {:?}]\nvars = {{ DB_URL = \"shared\" }}\n",
                project_a.to_string_lossy(),
                project_b.to_string_lossy(),
            ),
        );

        for matching in [&project_a, &project_b] {
            let shared = load_from(Some(&global), matching, Some(&home));
            assert_eq!(shared.get("DB_URL").map(|v| v.value.as_str()), Some("shared"));
        }
        let shared = load_from(Some(&global), &unrelated, Some(&home));
        assert_eq!(shared.get("DB_URL"), None);
    }

    #[test]
    fn an_empty_path_array_fails_closed_instead_of_matching_everything() {
        let home = tempdir("home-empty-array");
        let root = tempdir("project-empty-array");
        let global = home.join(".meshfox").join("config.toml");
        write(&global, "[[env]]\npath = []\nvars = { LEAKED = \"no\" }\n");

        let shared = load_from(Some(&global), &root, Some(&home));
        assert_eq!(shared.get("LEAKED"), None);
    }

    #[test]
    fn a_non_string_element_inside_a_path_array_is_skipped_not_fatal() {
        let home = tempdir("home-mixed-array");
        let project_a = home.join("work").join("projectA");
        std::fs::create_dir_all(&project_a).unwrap();

        let global = home.join(".meshfox").join("config.toml");
        write(
            &global,
            &format!(
                "[[env]]\npath = [5, {:?}]\nvars = {{ DB_URL = \"shared\" }}\n",
                project_a.to_string_lossy(),
            ),
        );

        let shared = load_from(Some(&global), &project_a, Some(&home));
        assert_eq!(shared.get("DB_URL").map(|v| v.value.as_str()), Some("shared"));
    }
}
