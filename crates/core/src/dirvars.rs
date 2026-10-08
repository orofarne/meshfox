//! Directory variables (`<!-- meshfox:var type="dir" -->`) and the
//! trusted roots they grant — see SPEC.md, "Directory variables".
//!
//! A canvas file is untrusted input, so it can't widen its own file-access
//! boundary by what it *says*. A `dir` variable widens it by its *value*,
//! and only a value that did not come from the canvas text alone:
//!
//!   * `@tmp` / `@tmp/...` — a directory the worker owns, `<dir>/.meshfox/
//!     <canvas file>.tmp/`, created lazily and deleted by `session reset`;
//!   * anything the user supplied or confirmed (`--set`, the environment,
//!     shared config, a prompt answer in the cache) or a `from=` block
//!     produced — `vars::resolve` never puts a `dir` variable's own
//!     out-of-canvas `default` into `values` before it was confirmed (it
//!     behaves as `required`), so every `dir` value `resolve` returns is
//!     trusted.
//!
//! [`FileAccess`] bundles those values and is what every consumer that
//! resolves a `file`/`include` target on disk takes in place of a bare
//! `canvas_dir`.

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};

use crate::canvas::Canvas;
use crate::file_read::{self, ConfineError, FilePreview, PreviewError};
use crate::vars::{VarDecl, VarType};

/// Whether `value` is `@tmp` itself or a path under it (`@tmp/...`);
/// `@tmpfoo` is an ordinary relative path.
pub fn is_tmp_ref(value: &str) -> bool {
    value
        .strip_prefix("@tmp")
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
}

/// The canvas's worker-owned temporary directory, next to its other
/// `.meshfox/` state (`hello.canvas.md` -> `.meshfox/hello.canvas.md.tmp`).
pub fn tmp_dir(canvas_path: &Path) -> PathBuf {
    let dir = match canvas_path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };
    let name = canvas_path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    dir.join(".meshfox").join(format!("{name}.tmp"))
}

/// Size in bytes of everything under the canvas's `@tmp` (0 when absent) —
/// what `session reset` is about to delete, for the warning it shows.
pub fn tmp_size(canvas_path: &Path) -> u64 {
    fn walk(p: &Path) -> u64 {
        let Ok(md) = std::fs::symlink_metadata(p) else {
            return 0;
        };
        if md.is_dir() {
            std::fs::read_dir(p)
                .map(|rd| rd.flatten().map(|e| walk(&e.path())).sum())
                .unwrap_or(0)
        } else {
            md.len()
        }
    }
    walk(&tmp_dir(canvas_path))
}

/// Deletes the canvas's `@tmp` (session reset). Returns the bytes freed.
pub fn reset_tmp(canvas_path: &Path) -> io::Result<u64> {
    let size = tmp_size(canvas_path);
    match std::fs::remove_dir_all(tmp_dir(canvas_path)) {
        Ok(()) => Ok(size),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(0),
        Err(e) => Err(e),
    }
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

/// Turns a `dir` variable's value into the path it denotes: `@tmp[/...]`
/// -> under the canvas's `tmp_dir` (created, best effort — it is
/// worker-owned), `~[/...]` -> under `$HOME`, a relative path -> under the
/// canvas directory. `canvas_path` is `None` for an in-memory cache: `@tmp`
/// is then left as written.
pub fn expand_dir_value(value: &str, canvas_path: Option<&Path>) -> String {
    if is_tmp_ref(value) {
        let Some(canvas_path) = canvas_path else {
            return value.to_string();
        };
        let rest = value["@tmp".len()..].trim_start_matches('/');
        let base = tmp_dir(canvas_path);
        let full = if rest.is_empty() { base } else { base.join(rest) };
        let _ = std::fs::create_dir_all(&full);
        return absolutize(&full).to_string_lossy().into_owned();
    }
    if value == "~" || value.starts_with("~/") {
        if let Some(home) = home_dir() {
            let rest = value.trim_start_matches('~').trim_start_matches('/');
            return if rest.is_empty() {
                home
            } else {
                home.join(rest)
            }
            .to_string_lossy()
            .into_owned();
        }
        return value.to_string();
    }
    let p = Path::new(value);
    if p.is_absolute() {
        return value.to_string();
    }
    match canvas_path {
        Some(c) => {
            let dir = match c.parent() {
                Some(p) if !p.as_os_str().is_empty() => p,
                _ => Path::new("."),
            };
            absolutize(&dir.join(p)).to_string_lossy().into_owned()
        }
        None => value.to_string(),
    }
}

fn absolutize(p: &Path) -> PathBuf {
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|c| c.join(p))
            .unwrap_or_else(|_| p.to_path_buf())
    }
}

/// Replaces `$NAME` / `${NAME}` in `target` for every `NAME` in `vars`
/// (the resolved `dir` variables); any other `$` is left alone, so an
/// ordinary file name containing one still works. A leading `@tmp` is
/// expanded like a `dir` value.
pub fn expand_target(
    target: &str,
    vars: &HashMap<String, String>,
    canvas_path: Option<&Path>,
) -> String {
    if is_tmp_ref(target) {
        return expand_dir_value(target, canvas_path);
    }
    if !target.contains('$') {
        return target.to_string();
    }
    let b = target.as_bytes();
    let mut out = String::with_capacity(target.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'$' {
            let (name, end) = if b.get(i + 1) == Some(&b'{') {
                match target[i + 2..].find('}') {
                    Some(j) => (&target[i + 2..i + 2 + j], i + 3 + j),
                    None => ("", i),
                }
            } else {
                let mut j = i + 1;
                while j < b.len() && (b[j].is_ascii_alphanumeric() || b[j] == b'_') {
                    j += 1;
                }
                (&target[i + 1..j], j)
            };
            if let Some(v) = vars.get(name) {
                out.push_str(v);
                i = end;
                continue;
            }
        }
        // Not a known reference: copy one whole char.
        let ch = target[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

/// Everything a consumer needs to resolve a `file`/`include` target on disk
/// beyond the canvas's own directory: the resolved `dir` variables (for
/// `$NAME` in targets) and the trusted roots they and `@tmp` denote.
#[derive(Debug, Clone, Default)]
pub struct FileAccess {
    canvas_path: Option<PathBuf>,
    vars: HashMap<String, String>,
    roots: Vec<PathBuf>,
    /// `dir` variables with no trusted value yet (an unconfirmed default),
    /// so a target naming one can say so instead of just "not found".
    pending: Vec<String>,
}

impl FileAccess {
    /// Canvas directory only — the pre-`dir` behavior.
    pub fn none() -> FileAccess {
        FileAccess::default()
    }

    /// Builds from the already-resolved `dir` variable `values` (name ->
    /// expanded path, as `vars::resolve` returns them). The canvas's `@tmp`
    /// is always a root.
    pub fn from_values(canvas_path: &Path, values: HashMap<String, String>) -> FileAccess {
        let mut roots: Vec<PathBuf> = values.values().map(PathBuf::from).collect();
        roots.push(tmp_dir(canvas_path));
        FileAccess {
            canvas_path: Some(canvas_path.to_path_buf()),
            vars: values,
            roots,
            pending: Vec::new(),
        }
    }

    /// Resolves `canvas`'s declared `dir` variables the same way a run
    /// would (overrides -> env -> cache -> shared -> trusted default) and
    /// keeps the ones that resolved; an unconfirmed or unresolvable one
    /// grants nothing.
    pub fn resolve(
        canvas: &Canvas,
        canvas_path: &Path,
        overrides: &HashMap<String, String>,
        cache: &crate::varcache::VarCache,
        computed: &HashMap<String, String>,
        shared: &crate::shared_env::SharedEnv,
    ) -> FileAccess {
        let decls: Vec<VarDecl> = crate::vars::declared_vars(canvas)
            .unwrap_or_default()
            .into_iter()
            .filter(|d| d.var_type == VarType::Dir)
            .collect();
        let resolved = crate::vars::resolve_with_shared(&decls, overrides, cache, computed, shared);
        let pending: Vec<String> = resolved.missing.iter().map(|d| d.name.clone()).collect();
        let mut fa = FileAccess::from_values(canvas_path, resolved.values.into_iter().collect());
        fa.pending = pending;
        fa
    }

    /// [`FileAccess::resolve`] for a caller that has no live session state
    /// (a one-shot check, the constraint pass on `GET /api/canvas`): only
    /// the process environment, the on-disk answer cache, shared config
    /// and `@tmp` can grant anything. A `from=`-computed `dir` variable
    /// needs a run's output and grants nothing here.
    pub fn for_canvas_path(canvas: &Canvas, canvas_path: &Path) -> FileAccess {
        let cache = crate::varcache::VarCache::load_read_only(canvas_path)
            .unwrap_or_else(|_| crate::varcache::VarCache::in_memory());
        let root = match canvas_path.parent() {
            Some(p) if !p.as_os_str().is_empty() => p,
            _ => Path::new("."),
        };
        let shared = crate::shared_env::load(root);
        FileAccess::resolve(
            canvas,
            canvas_path,
            &HashMap::new(),
            &cache,
            &HashMap::new(),
            &shared,
        )
    }

    pub fn roots(&self) -> &[PathBuf] {
        &self.roots
    }

    pub fn vars(&self) -> &HashMap<String, String> {
        &self.vars
    }

    /// `target` with `$DIRVAR` / `@tmp` expanded.
    pub fn expand(&self, target: &str) -> String {
        expand_target(target, &self.vars, self.canvas_path.as_deref())
    }

    /// `file_read::confine_in` on the expanded target with the trusted roots.
    pub fn confine(&self, dir: &Path, target: &str) -> Result<PathBuf, ConfineError> {
        for name in &self.pending {
            if expand_target(target, &HashMap::from([(name.clone(), String::new())]), None)
                != target
            {
                return Err(ConfineError::Unconfirmed(name.clone()));
            }
        }
        file_read::confine_in(dir, &self.roots, &self.expand(target))
    }

    pub fn preview(&self, dir: &Path, target: &str) -> Result<FilePreview, PreviewError> {
        self.confine(dir, target)?;
        file_read::preview_in(dir, &self.roots, &self.expand(target))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let n = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let d = std::env::temp_dir().join(format!("meshfox-dirvars-{tag}-{n}"));
        std::fs::create_dir_all(&d).unwrap();
        d.canonicalize().unwrap()
    }

    #[test]
    fn tmp_ref_is_a_prefix_match_on_a_path_boundary() {
        assert!(is_tmp_ref("@tmp"));
        assert!(is_tmp_ref("@tmp/data"));
        assert!(!is_tmp_ref("@tmpfoo"));
        assert!(!is_tmp_ref("x/@tmp"));
    }

    #[test]
    fn at_tmp_expands_under_the_canvas_tmp_dir_and_is_created() {
        let dir = tmp("expand");
        let canvas = dir.join("c.canvas.md");
        let v = expand_dir_value("@tmp/data", Some(&canvas));
        assert_eq!(PathBuf::from(&v), tmp_dir(&canvas).join("data"));
        assert!(Path::new(&v).is_dir());
        // No canvas path -> left alone.
        assert_eq!(expand_dir_value("@tmp/x", None), "@tmp/x");
    }

    #[test]
    fn relative_values_resolve_against_the_canvas_dir() {
        let dir = tmp("rel");
        let canvas = dir.join("c.canvas.md");
        assert_eq!(
            PathBuf::from(expand_dir_value("data", Some(&canvas))),
            dir.join("data")
        );
    }

    #[test]
    fn expand_target_substitutes_only_known_names() {
        let mut vars = HashMap::new();
        vars.insert("WORK_DIR".to_string(), "/w".to_string());
        assert_eq!(expand_target("$WORK_DIR/a.csv", &vars, None), "/w/a.csv");
        assert_eq!(expand_target("${WORK_DIR}/a.csv", &vars, None), "/w/a.csv");
        assert_eq!(expand_target("$OTHER/a.csv", &vars, None), "$OTHER/a.csv");
        assert_eq!(expand_target("a$.csv", &vars, None), "a$.csv");
        assert_eq!(expand_target("ёж/$WORK_DIR", &vars, None), "ёж//w");
    }

    #[test]
    fn file_access_reads_a_dir_var_target_and_the_tmp_dir_only() {
        let dir = tmp("fa-canvas");
        let canvas = dir.join("c.canvas.md");
        let data = tmp("fa-data");
        let other = tmp("fa-other");
        std::fs::write(data.join("a.csv"), "x").unwrap();
        std::fs::write(other.join("b.csv"), "y").unwrap();
        let t = expand_dir_value("@tmp/out", Some(&canvas));
        std::fs::write(Path::new(&t).join("t.csv"), "z").unwrap();

        let mut vals = HashMap::new();
        vals.insert("DATA".to_string(), data.to_string_lossy().into_owned());
        let fa = FileAccess::from_values(&canvas, vals);
        assert!(fa.confine(&dir, "$DATA/a.csv").is_ok());
        assert!(fa.confine(&dir, "@tmp/out/t.csv").is_ok());
        let abs_other = other.join("b.csv");
        assert!(fa.confine(&dir, abs_other.to_str().unwrap()).is_err());
        assert!(fa.confine(&dir, "$DATA/../../x").is_err());
        assert!(FileAccess::none().confine(&dir, "$DATA/a.csv").is_err());
    }

    #[test]
    fn reset_tmp_removes_the_dir_and_reports_the_size() {
        let dir = tmp("reset");
        let canvas = dir.join("c.canvas.md");
        let t = expand_dir_value("@tmp/d", Some(&canvas));
        std::fs::write(Path::new(&t).join("f"), "12345").unwrap();
        assert_eq!(tmp_size(&canvas), 5);
        assert_eq!(reset_tmp(&canvas).unwrap(), 5);
        assert!(!tmp_dir(&canvas).exists());
        assert_eq!(reset_tmp(&canvas).unwrap(), 0);
    }
}
