//! The `tangle` fence flag: instead of a throwaway file in the system temp
//! dir, a block's code is materialized as a hidden file with a **stable**
//! name in the block's own working directory, and the interpreter runs that.
//! Runtimes resolve imports, `package.json`, `tsconfig` and `__file__` from
//! where the script file lives, so a script next to the code it imports
//! behaves as if it had been written there by hand.
//!
//! The file is `<cwd>/.meshfox-tangle-<canvas file>__<block>.<ext>`:
//!
//! - both name components are [`sanitize`]d so `__` can only be the
//!   separator, which is what lets [`sweep_canvas`] tell one canvas's files
//!   from another's in a directory that holds many canvases;
//! - `<ext>` follows the fence language ([`crate::script_extension`]);
//! - it is rewritten only when the code changed (and replaced atomically),
//!   so re-running an unchanged block never touches the file — a dev server
//!   watching the directory sees a change only when the code changes.
//!
//! The file is a cache of the canvas, not a source: nothing reads edits made
//! to it. It stays on disk after a run; [`sweep_canvas`] removes the ones a
//! canvas no longer has a block for.

use std::collections::HashSet;
use std::io;
use std::path::{Path, PathBuf};

use crate::canvas::Canvas;

/// Every tangled file name starts with this, so one `.gitignore` line
/// (`.meshfox-tangle-*`) covers them all.
pub const PREFIX: &str = ".meshfox-tangle-";

/// Between the canvas component and the block component. [`sanitize`] never
/// lets `__` appear inside a component, so it appears exactly once.
const SEPARATOR: &str = "__";

/// A partially written file older than this is a leftover of a killed run.
const STALE_PART: std::time::Duration = std::time::Duration::from_secs(60);

/// Where a block's fence lives, for the spawn paths: its canvas file, and
/// the block's name when it carries `tangle`.
#[derive(Debug, Clone, Copy)]
pub struct BlockOrigin<'a> {
    pub canvas_path: &'a Path,
    pub tangle: Option<&'a str>,
}

impl<'a> BlockOrigin<'a> {
    pub fn new(canvas_path: &'a Path) -> Self {
        BlockOrigin {
            canvas_path,
            tangle: None,
        }
    }

    /// The same origin for a block that carries `tangle`.
    pub fn with_tangle(self, block_name: &'a str) -> Self {
        BlockOrigin {
            tangle: Some(block_name),
            ..self
        }
    }
}

/// One name component made safe for a file name: anything outside
/// `[A-Za-z0-9_.-]` becomes `_`, runs of `_` collapse to one, and leading and
/// trailing `_` are dropped (so a component can neither contain nor touch the
/// `__` separator). Never empty.
pub fn sanitize(component: &str) -> String {
    let mut out = String::with_capacity(component.len());
    for c in component.chars() {
        let c = if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
            c
        } else {
            '_'
        };
        if c == '_' && out.ends_with('_') {
            continue;
        }
        out.push(c);
    }
    let trimmed = out.trim_matches('_');
    if trimmed.is_empty() {
        "x".to_string()
    } else {
        trimmed.to_string()
    }
}

/// The start of every file name belonging to `canvas_path`'s canvas.
fn canvas_prefix(canvas_path: &Path) -> String {
    let file_name = canvas_path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    format!("{PREFIX}{}{SEPARATOR}", sanitize(&file_name))
}

/// The file name a tangled block is written under.
pub fn file_name(canvas_path: &Path, block_name: &str, lang: Option<&str>) -> String {
    format!(
        "{}{}.{}",
        canvas_prefix(canvas_path),
        sanitize(block_name),
        crate::script_extension(lang)
    )
}

/// Writes `code` to `dir`'s tangled file for this block (unless it already
/// holds exactly that) and returns its path. A failure is an error with the
/// path in it — there is deliberately no fallback to the temp dir: a block
/// that asked for `tangle` asked for its imports to resolve from `dir`.
pub fn materialize(
    canvas_path: &Path,
    block_name: &str,
    code: &str,
    lang: Option<&str>,
    dir: &Path,
) -> io::Result<PathBuf> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);

    let name = file_name(canvas_path, block_name, lang);
    let path = dir.join(&name);
    if std::fs::read(&path).is_ok_and(|existing| existing == code.as_bytes()) {
        return Ok(path);
    }
    let part = dir.join(format!(
        "{name}.{}-{}.part",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let written = std::fs::write(&part, code).and_then(|()| std::fs::rename(&part, &path));
    if let Err(e) = written {
        let _ = std::fs::remove_file(&part);
        return Err(io::Error::new(
            e.kind(),
            format!("tangle: can't write {}: {e}", path.display()),
        ));
    }
    Ok(path)
}

/// The tangled files of `canvas_path`'s canvas that it no longer has a
/// `tangle` block for — a deleted or renamed block, a block that dropped the
/// flag or changed language, or a `.part` file a killed run left behind.
/// Only this canvas's own files are looked at (`canvas_path`'s sanitized
/// file name is the prefix), in each directory a node of the canvas runs in;
/// other canvases' files, and files of a canvas that was renamed, are
/// `meshfox prune`'s job ([`canvas_component`] tells whose a file is).
/// Ignores every filesystem error.
pub fn orphans(canvas: &Canvas, canvas_path: &Path) -> Vec<PathBuf> {
    let canvas_dir = canvas_path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let prefix = canvas_prefix(canvas_path);

    let mut dirs: Vec<PathBuf> = Vec::new();
    let mut keep: HashSet<(PathBuf, String)> = HashSet::new();
    for node in &canvas.nodes {
        let cwd = node.cwd(canvas_dir);
        if !dirs.contains(&cwd) {
            dirs.push(cwd.clone());
        }
        for block in node.runnable_blocks() {
            if block.tangle() {
                if let Some(name) = &block.name {
                    keep.insert((cwd.clone(), file_name(canvas_path, name, Some(&block.lang))));
                }
            }
        }
    }

    let mut found = Vec::new();
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !name.starts_with(&prefix) {
                continue;
            }
            let orphan = if name.ends_with(".part") {
                entry
                    .metadata()
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|t| t.elapsed().ok())
                    .is_some_and(|age| age > STALE_PART)
            } else {
                !keep.contains(&(dir.clone(), name))
            };
            if orphan {
                found.push(entry.path());
            }
        }
    }
    found
}

/// Removes [`orphans`] and returns how many it removed — what a canvas's
/// worker does once when it starts. Ignores every filesystem error.
pub fn sweep_canvas(canvas: &Canvas, canvas_path: &Path) -> usize {
    orphans(canvas, canvas_path)
        .into_iter()
        .filter(|path| std::fs::remove_file(path).is_ok())
        .count()
}

/// The sanitized canvas file name a tangled file's name encodes — what sits
/// between [`PREFIX`] and the `__` separator — or `None` for any other file
/// name. Compare it with `sanitize(<canvas file name>)` to tell whose file
/// it is.
pub fn canvas_component(file_name: &str) -> Option<&str> {
    file_name
        .strip_prefix(PREFIX)?
        .split_once(SEPARATOR)
        .map(|(canvas, _)| canvas)
        .filter(|canvas| !canvas.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "meshfox-tangle-test-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn sanitize_keeps_safe_characters_and_never_touches_the_separator() {
        assert_eq!(sanitize("matching-check.v2"), "matching-check.v2");
        assert_eq!(sanitize("a b/c"), "a_b_c");
        assert_eq!(sanitize("a__b"), "a_b");
        assert_eq!(sanitize("__a__"), "a");
        assert_eq!(sanitize("привет"), "x");
        assert_eq!(sanitize(""), "x");
        for s in ["a  b", "a/ /b", "_x_", "a_ _b"] {
            let out = sanitize(s);
            assert!(!out.contains("__"), "{out:?}");
            assert!(!out.starts_with('_') && !out.ends_with('_'), "{out:?}");
        }
    }

    #[test]
    fn file_name_encodes_canvas_block_and_language() {
        assert_eq!(
            file_name(
                Path::new("data/catalog.canvas.md"),
                "check it",
                Some("python")
            ),
            ".meshfox-tangle-catalog.canvas.md__check_it.py"
        );
        assert_eq!(
            file_name(Path::new("README.md"), "b", None),
            ".meshfox-tangle-README.md__b.tmp"
        );
    }

    #[test]
    fn two_canvases_never_share_a_prefix() {
        let a = canvas_prefix(Path::new("a.canvas.md"));
        let b = canvas_prefix(Path::new("a_.canvas.md"));
        let name_of_b = file_name(Path::new("a_.canvas.md"), "x", None);
        assert!(!name_of_b.starts_with(&a), "{name_of_b} vs {a}");
        assert!(name_of_b.starts_with(&b));
    }

    #[test]
    fn materialize_writes_once_and_leaves_an_unchanged_file_alone() {
        let dir = temp_dir("write-once");
        let canvas = dir.join("doc.canvas.md");
        let path = materialize(&canvas, "b", "print(1)", Some("python"), &dir).unwrap();
        assert_eq!(path, dir.join(".meshfox-tangle-doc.canvas.md__b.py"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "print(1)");
        let first = std::fs::metadata(&path).unwrap().modified().unwrap();

        std::thread::sleep(std::time::Duration::from_millis(20));
        materialize(&canvas, "b", "print(1)", Some("python"), &dir).unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().modified().unwrap(),
            first,
            "unchanged code must not rewrite the file"
        );

        materialize(&canvas, "b", "print(2)", Some("python"), &dir).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "print(2)");
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().ends_with(".part"))
            .collect();
        assert!(leftovers.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn materialize_reports_the_path_when_it_cannot_write() {
        let missing = temp_dir("missing").join("nope");
        let err = materialize(Path::new("d.canvas.md"), "b", "x", None, &missing).unwrap_err();
        assert!(err.to_string().contains("tangle: can't write"), "{err}");
    }

    #[test]
    fn sweep_removes_only_this_canvas_files_of_blocks_that_are_gone() {
        let dir = temp_dir("sweep");
        let canvas_path = dir.join("doc.canvas.md");
        let md = "<!-- meshfox:canvas -->\n# Doc\n\n```python name=\"kept\" interpreter=\"python3\" tangle\nprint(1)\n```\n\n```python name=\"plain\" interpreter=\"python3\"\nprint(2)\n```\n";
        let canvas = crate::canvas::Canvas::from_markdown(md).unwrap();

        let kept = materialize(&canvas_path, "kept", "print(1)", Some("python"), &dir).unwrap();
        let gone = materialize(&canvas_path, "gone", "x", Some("python"), &dir).unwrap();
        let no_flag = materialize(&canvas_path, "plain", "x", Some("python"), &dir).unwrap();
        let other_lang = materialize(&canvas_path, "kept", "x", Some("ruby"), &dir).unwrap();
        let other_canvas = materialize(
            &dir.join("other.canvas.md"),
            "gone",
            "x",
            Some("python"),
            &dir,
        )
        .unwrap();
        let unrelated = dir.join("helper.py");
        std::fs::write(&unrelated, "x").unwrap();
        let fresh_part = dir.join(format!(
            "{}.1-1.part",
            file_name(&canvas_path, "kept", None)
        ));
        std::fs::write(&fresh_part, "x").unwrap();

        let removed = sweep_canvas(&canvas, &canvas_path);

        assert_eq!(removed, 3, "gone, plain (no flag), kept as ruby");
        assert!(kept.exists());
        assert!(!gone.exists());
        assert!(!no_flag.exists());
        assert!(!other_lang.exists());
        assert!(other_canvas.exists(), "another canvas's file is not ours");
        assert!(unrelated.exists());
        assert!(
            fresh_part.exists(),
            "a fresh .part may belong to a live run"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
