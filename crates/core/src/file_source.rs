//! The filesystem-independent vocabulary for reading a canvas's `file` and
//! `include` targets. Kept out of `file_read` so the document model can name
//! these types without depending on any real I/O.

use std::path::PathBuf;
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
