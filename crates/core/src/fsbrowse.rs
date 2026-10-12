//! Directory listing for the `file`/`dir` variable pickers (SPEC.md,
//! "Path pickers"). The worker owns the file system a canvas runs against,
//! so the web UI and the TUI both ask it — never read the disk themselves.
//!
//! Names only: nothing here reads a file's content. The listing is a
//! convenience for a person choosing a path; it grants no access (a chosen
//! value becomes a trusted root only through `vars::resolve`, see
//! `crate::dirvars`).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Most entries one listing returns; `FsListing::truncated` says when the
/// directory had more.
pub const MAX_ENTRIES: usize = 5000;

/// What the picker is choosing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickKind {
    /// Only directories are listed.
    Dir,
    /// Directories (to navigate) and files.
    File,
}

impl PickKind {
    pub fn parse(s: &str) -> Option<PickKind> {
        match s {
            "dir" => Some(PickKind::Dir),
            "file" => Some(PickKind::File),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FsEntry {
    pub name: String,
    pub is_dir: bool,
    /// Size in bytes; only for files.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub size: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FsListing {
    /// The directory listed, absolute and canonical.
    pub path: String,
    /// Its parent, `None` at the file system root.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub parent: Option<String>,
    /// The user's home directory, for a "Home" shortcut.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub home: Option<String>,
    /// The canvas's own directory, for a "Canvas" shortcut.
    pub canvas_dir: String,
    /// When the requested path named a file, its name inside `path` — the
    /// picker pre-selects it.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub selected: Option<String>,
    /// Directories first, then files, each group case-insensitively by name.
    pub entries: Vec<FsEntry>,
    /// More than [`MAX_ENTRIES`] entries existed.
    pub truncated: bool,
}

#[derive(Debug, Error)]
pub enum FsError {
    #[error("{0}: {1}")]
    Read(PathBuf, #[source] std::io::Error),
}

fn canvas_dir_of(canvas_path: &Path) -> PathBuf {
    match canvas_path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        _ => PathBuf::from("."),
    }
}

/// Where `requested` points, before any file-system check: `@tmp[/...]`,
/// `~[/...]`, an absolute path, or a path relative to the canvas directory.
/// An empty request means the canvas directory. Nothing is created.
fn locate(requested: &str, canvas_path: &Path) -> PathBuf {
    let requested = requested.trim();
    let canvas_dir = canvas_dir_of(canvas_path);
    if requested.is_empty() {
        return canvas_dir;
    }
    if crate::dirvars::is_tmp_ref(requested) {
        let rest = requested["@tmp".len()..].trim_start_matches('/');
        let base = crate::dirvars::tmp_dir(canvas_path);
        return if rest.is_empty() { base } else { base.join(rest) };
    }
    if requested == "~" || requested.starts_with("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            let rest = requested.trim_start_matches('~').trim_start_matches('/');
            let home = PathBuf::from(home);
            return if rest.is_empty() { home } else { home.join(rest) };
        }
    }
    let p = Path::new(requested);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        canvas_dir.join(p)
    }
}

/// Lists the directory `requested` names — or, when it names a file, the
/// directory containing it (with `selected` set). A path that doesn't exist
/// (the `save` output of a `file` variable, a stale cached value) falls
/// back to its nearest existing ancestor, so a picker can always open
/// somewhere useful.
pub fn list(
    requested: &str,
    kind: PickKind,
    show_hidden: bool,
    canvas_path: &Path,
) -> Result<FsListing, FsError> {
    let target = locate(requested, canvas_path);
    let mut selected = None;
    let mut dir = target.clone();
    loop {
        match std::fs::metadata(&dir) {
            Ok(md) if md.is_dir() => break,
            Ok(_) => {
                // A file: list its directory and pre-select it.
                selected = dir.file_name().map(|n| n.to_string_lossy().into_owned());
                match dir.parent() {
                    Some(p) => dir = p.to_path_buf(),
                    None => break,
                }
            }
            Err(_) => {
                // Missing (or unreadable): the picker opens at the nearest
                // ancestor that exists; the missing tail is just a name the
                // user may be about to create.
                match dir.parent() {
                    Some(p) if !p.as_os_str().is_empty() => dir = p.to_path_buf(),
                    _ => {
                        dir = canvas_dir_of(canvas_path);
                        break;
                    }
                }
                if selected.is_none() {
                    selected = target.file_name().map(|n| n.to_string_lossy().into_owned());
                }
            }
        }
    }
    // A `selected` that doesn't sit directly in `dir` (several missing
    // levels) is no use as a pre-selection.
    if selected.is_some() && target.parent() != Some(dir.as_path()) {
        selected = None;
    }
    let canonical = dir
        .canonicalize()
        .map_err(|e| FsError::Read(dir.clone(), e))?;
    let read = std::fs::read_dir(&canonical).map_err(|e| FsError::Read(canonical.clone(), e))?;

    let mut entries = Vec::new();
    let mut truncated = false;
    for item in read.flatten() {
        let name = item.file_name().to_string_lossy().into_owned();
        if !show_hidden && name.starts_with('.') {
            continue;
        }
        // `metadata` follows symlinks (a link to a directory is a
        // directory); a broken link has no metadata and is skipped.
        let Ok(md) = std::fs::metadata(item.path()) else {
            continue;
        };
        let is_dir = md.is_dir();
        if kind == PickKind::Dir && !is_dir {
            continue;
        }
        if entries.len() >= MAX_ENTRIES {
            truncated = true;
            break;
        }
        entries.push(FsEntry {
            name,
            is_dir,
            size: (!is_dir).then(|| md.len()),
        });
    }
    entries.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            .then_with(|| a.name.cmp(&b.name))
    });

    let canvas_dir = canvas_dir_of(canvas_path);
    let canvas_dir = canvas_dir.canonicalize().unwrap_or(canvas_dir);
    Ok(FsListing {
        parent: canonical.parent().map(|p| p.to_string_lossy().into_owned()),
        path: canonical.to_string_lossy().into_owned(),
        home: std::env::var_os("HOME").map(|h| h.to_string_lossy().into_owned()),
        canvas_dir: canvas_dir.to_string_lossy().into_owned(),
        selected,
        entries,
        truncated,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tmp(tag: &str) -> PathBuf {
        let n = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let d = std::env::temp_dir().join(format!("meshfox-fsbrowse-{tag}-{n}"));
        fs::create_dir_all(&d).unwrap();
        d.canonicalize().unwrap()
    }

    fn names(l: &FsListing) -> Vec<&str> {
        l.entries.iter().map(|e| e.name.as_str()).collect()
    }

    #[test]
    fn lists_dirs_first_then_files_hiding_dotfiles() {
        let d = tmp("sort");
        fs::create_dir(d.join("zeta")).unwrap();
        fs::create_dir(d.join("Alpha")).unwrap();
        fs::write(d.join("b.mp4"), "12345").unwrap();
        fs::write(d.join("A.txt"), "").unwrap();
        fs::write(d.join(".hidden"), "").unwrap();
        let canvas = d.join("c.canvas.md");

        let l = list(d.to_str().unwrap(), PickKind::File, false, &canvas).unwrap();
        assert_eq!(names(&l), ["Alpha", "zeta", "A.txt", "b.mp4"]);
        assert_eq!(l.entries[3].size, Some(5));
        assert_eq!(l.entries[0].size, None);

        let l = list(d.to_str().unwrap(), PickKind::File, true, &canvas).unwrap();
        assert!(names(&l).contains(&".hidden"));
    }

    #[test]
    fn dir_kind_lists_only_directories() {
        let d = tmp("dirkind");
        fs::create_dir(d.join("sub")).unwrap();
        fs::write(d.join("f.txt"), "").unwrap();
        let l = list(d.to_str().unwrap(), PickKind::Dir, false, &d.join("c.canvas.md")).unwrap();
        assert_eq!(names(&l), ["sub"]);
    }

    #[test]
    fn a_file_path_lists_its_directory_and_selects_it() {
        let d = tmp("file");
        fs::write(d.join("in.mp4"), "x").unwrap();
        fs::write(d.join("other.mp4"), "x").unwrap();
        let p = d.join("in.mp4");
        let l = list(p.to_str().unwrap(), PickKind::File, false, &d.join("c.canvas.md")).unwrap();
        assert_eq!(l.path, d.to_string_lossy());
        assert_eq!(l.selected.as_deref(), Some("in.mp4"));
    }

    #[test]
    fn a_missing_path_falls_back_to_its_nearest_existing_ancestor() {
        let d = tmp("missing");
        let p = d.join("out.mp4");
        let l = list(p.to_str().unwrap(), PickKind::File, false, &d.join("c.canvas.md")).unwrap();
        assert_eq!(l.path, d.to_string_lossy());
        assert_eq!(l.selected.as_deref(), Some("out.mp4"));

        let deep = d.join("a").join("b").join("out.mp4");
        let l = list(deep.to_str().unwrap(), PickKind::File, false, &d.join("c.canvas.md")).unwrap();
        assert_eq!(l.path, d.to_string_lossy());
        assert_eq!(l.selected, None);
    }

    #[test]
    fn empty_request_is_the_canvas_dir_and_relative_is_against_it() {
        let d = tmp("rel");
        fs::create_dir(d.join("data")).unwrap();
        let canvas = d.join("c.canvas.md");
        assert_eq!(list("", PickKind::Dir, false, &canvas).unwrap().path, d.to_string_lossy());
        assert_eq!(
            list("data", PickKind::Dir, false, &canvas).unwrap().path,
            d.join("data").to_string_lossy()
        );
    }

    #[test]
    fn parent_is_absent_only_at_the_root() {
        let d = tmp("parent");
        let l = list(d.to_str().unwrap(), PickKind::Dir, false, &d.join("c.canvas.md")).unwrap();
        assert_eq!(l.parent.as_deref(), d.parent().and_then(|p| p.to_str()));
        let root = list("/", PickKind::Dir, false, &d.join("c.canvas.md")).unwrap();
        assert_eq!(root.parent, None);
    }

    #[test]
    fn at_tmp_is_listed_without_being_created() {
        let d = tmp("attmp");
        let canvas = d.join("c.canvas.md");
        // Doesn't exist yet: falls back to an ancestor, creates nothing.
        let _ = list("@tmp/out", PickKind::Dir, false, &canvas).unwrap();
        assert!(!crate::dirvars::tmp_dir(&canvas).exists());
    }
}
