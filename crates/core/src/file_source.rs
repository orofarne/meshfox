//! The filesystem-independent vocabulary for reading a canvas's `file` and
//! `include` targets. Kept out of `file_read` so the document model can name
//! these types without depending on any real I/O.

use std::io;
use std::path::{Path, PathBuf};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ConfineError {
    #[error("{0}: {1}")]
    DirNotFound(PathBuf, #[source] std::io::Error),
    #[error("{0}: {1}")]
    TargetNotFound(PathBuf, #[source] std::io::Error),
    #[error("{0} resolves outside the canvas directory and its trusted roots")]
    Outside(PathBuf),
    /// The target names a `type="dir"` variable that has no trusted value
    /// yet (its out-of-canvas default hasn't been confirmed) — see SPEC.md,
    /// "Directory variables".
    #[error("${0} is a directory variable that hasn't been confirmed yet")]
    Unconfirmed(String),
    #[error("{0}")]
    Config(String),
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

/// Turns a file's raw bytes into a [`FilePreview`]. A null byte anywhere in
/// a representative prefix is a cheap, standard "this isn't text" heuristic
/// (same one `file`/git use) — good enough to keep an accidental
/// image/binary target from getting treated as text.
pub fn preview_from_bytes(bytes: &[u8]) -> Result<FilePreview, PreviewError> {
    let sample_len = bytes.len().min(8000);
    if bytes[..sample_len].contains(&0) {
        return Err(PreviewError::Binary);
    }
    let truncated = bytes.len() > FILE_PREVIEW_MAX_BYTES;
    let slice = &bytes[..bytes.len().min(FILE_PREVIEW_MAX_BYTES)];
    let content = String::from_utf8_lossy(slice).into_owned();
    Ok(FilePreview { content, truncated })
}

/// Where the document model reads a canvas's `file`/`include` targets from.
///
/// The model never touches a filesystem itself: everything it needs goes
/// through this trait. The native implementation (`dirvars::FileAccess`)
/// confines targets to the canvas directory and its trusted roots and reads
/// from disk; an embedder (a browser, a test) can supply its own.
pub trait FileSource {
    /// Resolves `target` (relative to `dir`) to a path this canvas is
    /// allowed to read, or says why not. The returned path is what the other
    /// methods take.
    fn confine(&self, dir: &Path, target: &str) -> Result<PathBuf, ConfineError>;

    /// The full contents of a path [`FileSource::confine`] returned.
    fn read(&self, path: &Path) -> io::Result<Vec<u8>>;

    /// [`FileSource::read`] as UTF-8 text.
    fn read_to_string(&self, path: &Path) -> io::Result<String> {
        String::from_utf8(self.read(path)?)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
    }

    /// The directory an included file's relative assets resolve against,
    /// for a path [`FileSource::confine`] returned (`None` if it has none).
    fn asset_base(&self, path: &Path) -> Option<String> {
        path.parent().map(|p| p.to_string_lossy().into_owned())
    }

    /// `target` read as a bounded text preview (see [`preview_from_bytes`]).
    fn preview(&self, dir: &Path, target: &str) -> Result<FilePreview, PreviewError> {
        let resolved = self.confine(dir, target)?;
        let bytes = self
            .read(&resolved)
            .map_err(|e| PreviewError::Read(resolved.clone(), e))?;
        preview_from_bytes(&bytes)
    }
}
