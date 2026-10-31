//! A plain-disk [`FileSource`] for the model's own tests. The real thing
//! (`meshfox_core::dirvars::FileAccess`, with trusted roots and variables)
//! lives in `meshfox-core`; tests here only need "this directory, nothing
//! outside it".

use crate::file_source::{ConfineError, FileSource};
use std::io;
use std::path::{Path, PathBuf};

pub struct DiskFiles;

impl FileSource for DiskFiles {
    fn confine(&self, dir: &Path, target: &str) -> Result<PathBuf, ConfineError> {
        let dir = dir
            .canonicalize()
            .map_err(|e| ConfineError::DirNotFound(dir.to_path_buf(), e))?;
        let candidate = dir.join(target);
        let resolved = candidate
            .canonicalize()
            .map_err(|e| ConfineError::TargetNotFound(candidate.clone(), e))?;
        if !resolved.starts_with(&dir) {
            return Err(ConfineError::Outside(resolved));
        }
        Ok(resolved)
    }

    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        std::fs::read(path)
    }

    fn asset_base(&self, path: &Path) -> Option<String> {
        path.canonicalize()
            .ok()
            .and_then(|p| p.parent().map(|p| p.to_string_lossy().into_owned()))
    }
}
