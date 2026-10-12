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
/// `@tmpfoo` is an ordinary relative path. A `..` component disqualifies
/// it: `@tmp/../..` would otherwise be taken silently as "inside the
/// worker's own directory" while denoting somewhere else entirely.
pub fn is_tmp_ref(value: &str) -> bool {
    value.strip_prefix("@tmp").is_some_and(|rest| {
        (rest.is_empty() || rest.starts_with('/')) && !rest.split('/').any(|c| c == "..")
    })
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

/// [`expand_dir_value`] for a `file` variable's value. Differs only for
/// `@tmp/...`: the worker creates the *parent* directory (so a block can
/// write the file there) but never the file's own path as a directory.
pub fn expand_file_value(value: &str, canvas_path: Option<&Path>) -> String {
    if is_tmp_ref(value) {
        let Some(canvas_path) = canvas_path else {
            return value.to_string();
        };
        let rest = value["@tmp".len()..].trim_start_matches('/');
        let base = tmp_dir(canvas_path);
        let full = if rest.is_empty() { base } else { base.join(rest) };
        if let Some(parent) = full.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        return absolutize(&full).to_string_lossy().into_owned();
    }
    expand_dir_value(value, canvas_path)
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
/// (the resolved `dir` and `file` variables); any other `$` is left alone, so an
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
    /// The values of resolved `file` variables: each is a trusted root for
    /// exactly that file. Kept apart from `roots` on purpose — roots match
    /// by prefix, and a file value that is (or later becomes) a directory
    /// must never turn into a whole-directory root.
    files: Vec<PathBuf>,
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
            files: Vec::new(),
            pending: Vec::new(),
        }
    }

    /// Adds the resolved `file` variable `values` (name -> expanded path):
    /// each is usable as `$NAME` in a target and grants exactly that file.
    pub fn with_files(mut self, values: HashMap<String, String>) -> FileAccess {
        for (name, value) in values {
            self.files.push(PathBuf::from(&value));
            self.vars.insert(name, value);
        }
        self
    }

    /// Resolves `canvas`'s declared `dir`/`file` variables the same way a run
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
            .filter(|d| matches!(d.var_type, VarType::Dir | VarType::File))
            .collect();
        let resolved = crate::vars::resolve_with_shared(&decls, overrides, cache, computed, shared);
        let pending: Vec<String> = resolved.missing.iter().map(|d| d.name.clone()).collect();
        let file_names: std::collections::HashSet<&str> = decls
            .iter()
            .filter(|d| d.var_type == VarType::File)
            .map(|d| d.name.as_str())
            .collect();
        let (files, dirs): (HashMap<String, String>, HashMap<String, String>) = resolved
            .values
            .into_iter()
            .partition(|(name, _)| file_names.contains(name.as_str()));
        let mut fa = FileAccess::from_values(canvas_path, dirs).with_files(files);
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

    /// The exact-file trusted roots (see [`FileAccess::with_files`]).
    pub fn files(&self) -> &[PathBuf] {
        &self.files
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
        file_read::confine_in_files(dir, &self.roots, &self.files, &self.expand(target))
    }

    pub fn preview(&self, dir: &Path, target: &str) -> Result<FilePreview, PreviewError> {
        self.confine(dir, target)?;
        file_read::preview_in_files(dir, &self.roots, &self.files, &self.expand(target))
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
    fn tmp_ref_with_a_dotdot_component_is_not_a_tmp_ref() {
        assert!(!is_tmp_ref("@tmp/.."));
        assert!(!is_tmp_ref("@tmp/a/../../b"));
        assert!(is_tmp_ref("@tmp/a..b"));
    }

    #[test]
    fn at_tmp_file_value_creates_only_the_parent() {
        let dir = tmp("file-tmp");
        let canvas = dir.join("c.canvas.md");
        let v = expand_file_value("@tmp/out/clip.mp4", Some(&canvas));
        assert_eq!(PathBuf::from(&v), tmp_dir(&canvas).join("out/clip.mp4"));
        assert!(Path::new(&v).parent().unwrap().is_dir());
        assert!(!Path::new(&v).exists());
    }

    #[test]
    fn a_file_value_grants_exactly_that_file() {
        let dir = tmp("ff-canvas");
        let canvas = dir.join("c.canvas.md");
        let media = tmp("ff-media");
        std::fs::write(media.join("in.mp4"), "x").unwrap();
        std::fs::write(media.join("in.mp4.txt"), "sibling").unwrap();
        std::fs::write(media.join("other.mp4"), "y").unwrap();
        std::fs::create_dir_all(media.join("sub")).unwrap();
        std::fs::write(media.join("sub").join("deep.mp4"), "z").unwrap();

        let mut files = HashMap::new();
        files.insert("INPUT".to_string(), media.join("in.mp4").to_string_lossy().into_owned());
        let fa = FileAccess::from_values(&canvas, HashMap::new()).with_files(files);
        assert!(fa.confine(&dir, "$INPUT").is_ok());
        let sibling = media.join("other.mp4");
        assert!(fa.confine(&dir, sibling.to_str().unwrap()).is_err());
        let text = media.join("in.mp4.txt");
        assert!(fa.confine(&dir, text.to_str().unwrap()).is_err());
        let deep = media.join("sub").join("deep.mp4");
        assert!(fa.confine(&dir, deep.to_str().unwrap()).is_err());
        assert!(fa.confine(&dir, "$INPUT/../other.mp4").is_err());
    }

    #[test]
    fn a_file_value_that_is_a_directory_grants_nothing() {
        let dir = tmp("fd-canvas");
        let canvas = dir.join("c.canvas.md");
        let media = tmp("fd-media");
        std::fs::write(media.join("a.mp4"), "x").unwrap();
        let mut files = HashMap::new();
        files.insert("OUT".to_string(), media.to_string_lossy().into_owned());
        let fa = FileAccess::from_values(&canvas, HashMap::new()).with_files(files);
        let inside = media.join("a.mp4");
        assert!(fa.confine(&dir, inside.to_str().unwrap()).is_err());
        assert!(fa.confine(&dir, "$OUT").is_err());
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
