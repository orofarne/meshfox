//! Reading a `file`/`link` node's target off disk, confined to the
//! canvas's own directory — the one boundary every consumer that touches a
//! canvas-relative path shares: the server's `display="code"` preview and
//! run/open endpoints, `staticgen`'s static-export asset copying, and
//! `constraint`'s `.content()`/`.json()`/... (see `crate::constraint`).
//! Previously duplicated (with small behavioral drift) between
//! `crates/server` and `crates/core::staticgen`; this is the one copy both
//! — and the constraint sandbox — build on now.

use std::fs;
use std::path::{Path, PathBuf};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ConfineError {
    #[error("{0}: {1}")]
    DirNotFound(PathBuf, #[source] std::io::Error),
    #[error("{0}: {1}")]
    TargetNotFound(PathBuf, #[source] std::io::Error),
    #[error("{0} resolves outside the canvas directory")]
    Outside(PathBuf),
    /// The target names a `type="dir"` variable that has no trusted value
    /// yet (its out-of-canvas default hasn't been confirmed) — see SPEC.md,
    /// "Directory variables".
    #[error("${0} is a directory variable that hasn't been confirmed yet")]
    Unconfirmed(String),
}

/// Resolves `target` relative to `dir`, canonicalizing both, and confines
/// the result: a `../../etc/passwd` or an absolute path pointing outside
/// `dir` errors instead of resolving, since `target` comes from the
/// (possibly hand-edited) canvas file, not from a trusted source. `dir`
/// need not already be canonical — this canonicalizes it too, so every
/// caller can just pass the canvas's own directory as-is.
pub fn confine(dir: &Path, target: &str) -> Result<PathBuf, ConfineError> {
    confine_in(dir, &[], target)
}

/// Like `confine`, but a result that lands inside any of `roots` is also
/// accepted — the extra directories a canvas has been *trusted* with from
/// outside its own text (a `type="dir"` variable whose value the user
/// supplied or confirmed, or the worker-owned `@tmp`; see SPEC.md). The
/// check is still "canonicalize, then `starts_with`", now against each
/// root (canonicalized here too; a root that doesn't exist is skipped, it
/// can't contain an existing target anyway), so `..` and symlinks can't
/// step out of a root any more than out of `dir`.
pub fn confine_in(dir: &Path, roots: &[PathBuf], target: &str) -> Result<PathBuf, ConfineError> {
    let dir = dir
        .canonicalize()
        .map_err(|e| ConfineError::DirNotFound(dir.to_path_buf(), e))?;
    let candidate = dir.join(target);
    let resolved = candidate
        .canonicalize()
        .map_err(|e| ConfineError::TargetNotFound(candidate.clone(), e))?;
    let allowed = resolved.starts_with(&dir)
        || roots
            .iter()
            .filter_map(|r| r.canonicalize().ok())
            .any(|r| resolved.starts_with(r));
    if !allowed {
        return Err(ConfineError::Outside(resolved));
    }
    Ok(resolved)
}

#[derive(Debug, Error)]
pub enum PreviewError {
    #[error(transparent)]
    Confine(#[from] ConfineError),
    #[error("{0}: {1}")]
    Read(PathBuf, #[source] std::io::Error),
    #[error("target looks like a binary file, can't read it as text")]
    Binary,
}

pub struct FilePreview {
    pub content: String,
    /// `true` if `content` was cut off at `FILE_PREVIEW_MAX_BYTES` — callers
    /// that surface this to a user should say so rather than imply the
    /// preview is the whole file.
    pub truncated: bool,
}

/// Same cap the server's file-content preview and `staticgen`'s
/// `display="code"` export already used.
pub const FILE_PREVIEW_MAX_BYTES: usize = 1_000_000;

/// Reads `target` (relative to `dir`, confined to it — see `confine`)
/// fresh off disk. A null byte anywhere in a representative prefix is a
/// cheap, standard "this isn't text" heuristic (same one `file`/git use) —
/// good enough to keep an accidental image/binary target from getting
/// treated as text.
pub fn preview(dir: &Path, target: &str) -> Result<FilePreview, PreviewError> {
    preview_in(dir, &[], target)
}

/// `preview` with extra trusted `roots` (see `confine_in`).
pub fn preview_in(
    dir: &Path,
    roots: &[PathBuf],
    target: &str,
) -> Result<FilePreview, PreviewError> {
    let resolved = confine_in(dir, roots, target)?;
    let bytes = fs::read(&resolved).map_err(|e| PreviewError::Read(resolved.clone(), e))?;
    let sample_len = bytes.len().min(8000);
    if bytes[..sample_len].contains(&0) {
        return Err(PreviewError::Binary);
    }
    let truncated = bytes.len() > FILE_PREVIEW_MAX_BYTES;
    let slice = &bytes[..bytes.len().min(FILE_PREVIEW_MAX_BYTES)];
    let content = String::from_utf8_lossy(slice).into_owned();
    Ok(FilePreview { content, truncated })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_dir(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("meshfox-file-read-test-{tag}-{nanos}"));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn confine_reads_a_plain_relative_target() {
        let dir = tmp_dir("plain");
        fs::write(dir.join("a.txt"), "hello").unwrap();
        let resolved = confine(&dir, "a.txt").unwrap();
        assert_eq!(fs::read_to_string(resolved).unwrap(), "hello");
    }

    #[test]
    fn confine_rejects_a_target_that_escapes_the_dir() {
        let dir = tmp_dir("escape");
        let sibling = tmp_dir("escape-sibling");
        fs::write(sibling.join("secret.txt"), "nope").unwrap();
        let rel = pathdiff(&sibling.join("secret.txt"), &dir);
        assert!(matches!(
            confine(&dir, &rel),
            Err(ConfineError::Outside(_)) | Err(ConfineError::TargetNotFound(..))
        ));
    }

    #[test]
    fn confine_in_accepts_a_target_inside_a_trusted_root_only() {
        let dir = tmp_dir("root-canvas");
        let data = tmp_dir("root-data");
        let other = tmp_dir("root-other");
        fs::write(data.join("a.csv"), "x").unwrap();
        fs::write(other.join("b.csv"), "y").unwrap();
        let roots = vec![data.clone()];
        let abs = data.join("a.csv");
        assert!(confine_in(&dir, &roots, abs.to_str().unwrap()).is_ok());
        let abs_other = other.join("b.csv");
        assert!(matches!(
            confine_in(&dir, &roots, abs_other.to_str().unwrap()),
            Err(ConfineError::Outside(_))
        ));
        // `..` out of the root is judged on the canonical result.
        let sneaky = format!("{}/../{}/b.csv", data.display(), other.file_name().unwrap().to_str().unwrap());
        assert!(matches!(
            confine_in(&dir, &roots, &sneaky),
            Err(ConfineError::Outside(_))
        ));
        // No roots == plain `confine`.
        assert!(matches!(
            confine(&dir, abs.to_str().unwrap()),
            Err(ConfineError::Outside(_))
        ));
    }

    #[test]
    fn preview_reads_content_and_flags_binary() {
        let dir = tmp_dir("preview");
        fs::write(dir.join("ok.txt"), "text content").unwrap();
        let p = preview(&dir, "ok.txt").unwrap();
        assert_eq!(p.content, "text content");
        assert!(!p.truncated);

        fs::write(dir.join("bin.dat"), [0u8, 1, 2, 3]).unwrap();
        assert!(matches!(
            preview(&dir, "bin.dat"),
            Err(PreviewError::Binary)
        ));
    }

    /// Minimal `..`-based relative path from `base` to `target`, just
    /// enough for the escape test above (both are flat temp dirs, so one
    /// `..` always suffices).
    fn pathdiff(target: &Path, base: &Path) -> String {
        format!(
            "../{}/{}",
            base.file_name().unwrap().to_str().unwrap(),
            target.file_name().unwrap().to_str().unwrap()
        )
    }
}
