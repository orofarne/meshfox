//! `meshfox prune`: finds the regenerable files meshfox leaves behind and
//! removes them on request. Everything here can be rebuilt by running the
//! canvas again, which is what makes it safe to delete:
//!
//! - **tangled files** (`.meshfox-tangle-<canvas>__<block>.<ext>`, see
//!   [`crate::tangle`]) whose block, or whose whole canvas, is gone;
//! - **state directories** (`.meshfox/<canvas file>.venv` — an `@python_venv`
//!   environment — and `.meshfox/<canvas file>.node` — an `@node_env` one)
//!   whose canvas file is gone;
//! - **stale temp files** (`meshfox-<pid>-<n>.<ext>` in the system temp dir,
//!   which a killed run leaves behind) older than a day.
//!
//! It deliberately never touches a canvas's run history
//! (`.meshfox/<canvas>.session.sqlite3`) or its variable cache
//! (`.meshfox/<canvas>.env`, which can hold secret values): those are the
//! user's to delete by hand. A canvas with a running worker is skipped, and
//! a symlink is never followed. [`scan`] only looks; [`remove`] deletes one
//! [`Candidate`], re-checking that it is the kind of file it claims to be.

use std::collections::{BTreeMap, HashSet};
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::canvas::Canvas;
use crate::tangle;

/// A temp file older than this is a leftover, not a run in progress.
pub const STALE_TEMP_AGE: Duration = Duration::from_secs(24 * 60 * 60);

/// Directories a recursive scan never descends into.
const SKIPPED_DIRS: &[&str] = &[".git", ".meshfox", "node_modules", "target", "__pycache__"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    TangledFile,
    StateDirectory,
    StaleTempFile,
}

impl Kind {
    pub fn heading(self) -> &'static str {
        match self {
            Kind::TangledFile => "tangled files whose block or canvas is gone",
            Kind::StateDirectory => "environments whose canvas is gone",
            Kind::StaleTempFile => "stale temp files",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Candidate {
    pub path: PathBuf,
    pub kind: Kind,
    pub reason: String,
    pub bytes: u64,
}

/// Something that looks prunable but is left alone, and why.
#[derive(Debug, Clone)]
pub struct Skipped {
    pub path: PathBuf,
    pub reason: String,
}

#[derive(Debug, Default)]
pub struct Report {
    pub candidates: Vec<Candidate>,
    pub skipped: Vec<Skipped>,
}

pub struct Scan<'a> {
    /// The directory to look at.
    pub root: &'a Path,
    /// Descend into subdirectories as well.
    pub recursive: bool,
    /// Where stale temp files are looked for (the system temp dir).
    pub temp_dir: &'a Path,
    /// What "a day ago" is measured from.
    pub now: SystemTime,
}

/// Looks for prunable files; changes nothing.
pub fn scan(scan: &Scan<'_>) -> Report {
    let mut report = Report::default();
    for dir in directories(scan.root, scan.recursive) {
        scan_directory(&dir, &mut report);
    }
    scan_temp(scan.temp_dir, scan.now, &mut report);
    report
        .candidates
        .sort_by(|a, b| (a.kind, &a.path).cmp(&(b.kind, &b.path)));
    report.candidates.dedup_by(|a, b| a.path == b.path);
    report.skipped.sort_by(|a, b| a.path.cmp(&b.path));
    report
}

/// Deletes one candidate. Refuses anything that isn't, right now, the kind
/// of file or directory the candidate says it is (a symlink, a path that
/// changed type since the scan, a name that doesn't fit its kind).
pub fn remove(candidate: &Candidate) -> io::Result<()> {
    let refuse = |why: &str| {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("won't remove {}: {why}", candidate.path.display()),
        ))
    };
    let name = candidate
        .path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let Ok(meta) = std::fs::symlink_metadata(&candidate.path) else {
        return refuse("it is gone");
    };
    match candidate.kind {
        Kind::TangledFile => {
            if !name.starts_with(tangle::PREFIX) || !meta.is_file() {
                return refuse("not a tangled file");
            }
            std::fs::remove_file(&candidate.path)
        }
        Kind::StateDirectory => {
            let in_meshfox = candidate
                .path
                .parent()
                .and_then(Path::file_name)
                .is_some_and(|n| n == ".meshfox");
            if !in_meshfox
                || !(name.ends_with(".venv") || name.ends_with(".node"))
                || !meta.is_dir()
            {
                return refuse("not an environment directory under .meshfox");
            }
            std::fs::remove_dir_all(&candidate.path)
        }
        Kind::StaleTempFile => {
            if !is_run_temp_name(&name) || !meta.is_file() {
                return refuse("not a meshfox temp file");
            }
            std::fs::remove_file(&candidate.path)
        }
    }
}

/// `root` and, when `recursive`, every directory under it (never through a
/// symlink, never into [`SKIPPED_DIRS`]).
fn directories(root: &Path, recursive: bool) -> Vec<PathBuf> {
    let mut found = vec![root.to_path_buf()];
    if !recursive {
        return found;
    }
    let mut next = 0;
    while next < found.len() {
        let dir = found[next].clone();
        next += 1;
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if entry.file_type().is_ok_and(|t| t.is_dir()) && !SKIPPED_DIRS.contains(&name.as_str())
            {
                found.push(entry.path());
            }
        }
    }
    found
}

fn scan_directory(dir: &Path, report: &mut Report) {
    // Markdown files in the directory, by the sanitized name a tangled file
    // would carry for them.
    let mut canvases: BTreeMap<String, String> = BTreeMap::new();
    let mut tangled: Vec<String> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !entry.file_type().is_ok_and(|t| t.is_file()) {
                continue;
            }
            if name.ends_with(".md") {
                canvases.insert(tangle::sanitize(&name), name);
            } else if name.starts_with(tangle::PREFIX) {
                tangled.push(name);
            }
        }
    }

    scan_state_directories(dir, report);

    // Tangled files whose canvas is not among the scanned markdown files.
    for name in &tangled {
        let Some(component) = tangle::canvas_component(name) else {
            continue;
        };
        if !canvases.contains_key(component) {
            report.candidates.push(Candidate {
                path: dir.join(name),
                kind: Kind::TangledFile,
                reason: "no canvas of that name in the scanned directories".to_string(),
                bytes: file_len(&dir.join(name)),
            });
        }
    }

    // Tangled files of a canvas that still exists but no longer has the block.
    for (component, canvas_name) in &canvases {
        let has_tangled = tangled
            .iter()
            .any(|name| tangle::canvas_component(name) == Some(component.as_str()));
        if !has_tangled {
            continue;
        }
        let canvas_path = dir.join(canvas_name);
        let Ok(markdown) = std::fs::read_to_string(&canvas_path) else {
            continue;
        };
        let Ok(canvas) = Canvas::from_markdown(&markdown) else {
            // An unparseable document says nothing about which blocks exist.
            continue;
        };
        let orphans = tangle::orphans(&canvas, &canvas_path);
        if orphans.is_empty() {
            continue;
        }
        if crate::worker_lock::is_held(&canvas_path) {
            report.skipped.push(Skipped {
                path: canvas_path,
                reason: "a worker is running for it".to_string(),
            });
            continue;
        }
        for path in orphans {
            report.candidates.push(Candidate {
                bytes: file_len(&path),
                path,
                kind: Kind::TangledFile,
                reason: format!("{canvas_name} has no tangle block for it any more"),
            });
        }
    }
}

/// `.meshfox/<canvas file>.venv` / `.node` whose canvas file is not there.
fn scan_state_directories(dir: &Path, report: &mut Report) {
    let Ok(entries) = std::fs::read_dir(dir.join(".meshfox")) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(canvas_name) = name
            .strip_suffix(".venv")
            .or_else(|| name.strip_suffix(".node"))
            .filter(|n| !n.is_empty())
        else {
            continue;
        };
        if !entry.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let canvas_path = dir.join(canvas_name);
        if std::fs::symlink_metadata(&canvas_path).is_ok() {
            continue;
        }
        if crate::worker_lock::is_held(&canvas_path) {
            report.skipped.push(Skipped {
                path: entry.path(),
                reason: "a worker is still running for the canvas".to_string(),
            });
            continue;
        }
        report.candidates.push(Candidate {
            bytes: dir_size(&entry.path()),
            path: entry.path(),
            kind: Kind::StateDirectory,
            reason: format!("{canvas_name} is gone"),
        });
    }
}

fn scan_temp(temp_dir: &Path, now: SystemTime, report: &mut Report) {
    let Ok(entries) = std::fs::read_dir(temp_dir) else {
        return;
    };
    let mut seen = HashSet::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !is_run_temp_name(&name) || !entry.file_type().is_ok_and(|t| t.is_file()) {
            continue;
        }
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        let old = meta
            .modified()
            .ok()
            .and_then(|t| now.duration_since(t).ok())
            .is_some_and(|age| age > STALE_TEMP_AGE);
        if old && seen.insert(entry.path()) {
            report.candidates.push(Candidate {
                path: entry.path(),
                kind: Kind::StaleTempFile,
                reason: "left by a run that never cleaned up".to_string(),
                bytes: meta.len(),
            });
        }
    }
}

/// `meshfox-<pid>-<suffix>.<ext>` — the name [`crate::write_script_file`]
/// (and the older `.tmp` scheme) give a block's temp file. Anything else in
/// the temp dir, including other `meshfox-*` entries, is not ours to remove.
fn is_run_temp_name(name: &str) -> bool {
    let Some(rest) = name.strip_prefix("meshfox-") else {
        return false;
    };
    let Some((stem, ext)) = rest.rsplit_once('.') else {
        return false;
    };
    let Some((pid, suffix)) = stem.split_once('-') else {
        return false;
    };
    let word = |s: &str| {
        !s.is_empty()
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    };
    !pid.is_empty() && pid.chars().all(|c| c.is_ascii_digit()) && word(suffix) && word(ext)
}

fn file_len(path: &Path) -> u64 {
    std::fs::symlink_metadata(path)
        .map(|m| m.len())
        .unwrap_or(0)
}

/// Total size of the regular files under `dir`, without following symlinks.
fn dir_size(dir: &Path) -> u64 {
    let mut total = 0;
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            total += dir_size(&entry.path());
        } else if file_type.is_file() {
            total += entry.metadata().map(|m| m.len()).unwrap_or(0);
        }
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "meshfox-prune-test-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    const CANVAS: &str = concat!(
        "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
        "```python name=\"kept\" interpreter=\"python3\" tangle\nprint(1)\n```\n",
    );

    fn tangled(dir: &Path, canvas: &str, block: &str) -> PathBuf {
        tangle::materialize(&dir.join(canvas), block, "x", Some("python"), dir).unwrap()
    }

    fn paths(report: &Report) -> Vec<PathBuf> {
        report.candidates.iter().map(|c| c.path.clone()).collect()
    }

    fn scan_of(root: &Path, recursive: bool, temp: &Path, now: SystemTime) -> Report {
        scan(&Scan {
            root,
            recursive,
            temp_dir: temp,
            now,
        })
    }

    #[test]
    fn finds_only_what_is_regenerable_and_orphaned() {
        let dir = temp_dir("finds");
        let temp = temp_dir("finds-temp");
        std::fs::write(dir.join("a.canvas.md"), CANVAS).unwrap();
        let meshfox = dir.join(".meshfox");

        // Environments: one with its canvas, two without.
        std::fs::create_dir_all(meshfox.join("a.canvas.md.venv")).unwrap();
        std::fs::create_dir_all(meshfox.join("gone.canvas.md.venv/bin")).unwrap();
        std::fs::write(meshfox.join("gone.canvas.md.venv/bin/python3"), "12345").unwrap();
        std::fs::create_dir_all(meshfox.join("gone.canvas.md.node")).unwrap();
        // State that must never be offered, canvas or not.
        for keep in [
            "gone.canvas.md.session.sqlite3",
            "gone.canvas.md.env",
            "gone.canvas.md.worker.lock",
        ] {
            std::fs::write(meshfox.join(keep), "x").unwrap();
        }
        std::fs::create_dir_all(dir.join(".venv")).unwrap();

        // Tangled files: kept, removed block, vanished canvas.
        let kept = tangled(&dir, "a.canvas.md", "kept");
        let removed_block = tangled(&dir, "a.canvas.md", "removed-block");
        let vanished = tangled(&dir, "old.canvas.md", "b");

        let report = scan_of(&dir, false, &temp, SystemTime::now());
        let found = paths(&report);
        assert!(found.contains(&meshfox.join("gone.canvas.md.venv")));
        assert!(found.contains(&meshfox.join("gone.canvas.md.node")));
        assert!(found.contains(&removed_block));
        assert!(found.contains(&vanished));
        assert_eq!(found.len(), 4, "{found:?}");
        assert!(!found.contains(&kept));
        assert_eq!(
            report
                .candidates
                .iter()
                .find(|c| c.path == meshfox.join("gone.canvas.md.venv"))
                .unwrap()
                .bytes,
            5
        );
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&temp);
    }

    #[test]
    fn a_running_worker_keeps_its_canvas_state() {
        let dir = temp_dir("worker");
        let temp = temp_dir("worker-temp");
        let canvas = dir.join("held.canvas.md");
        std::fs::create_dir_all(dir.join(".meshfox/held.canvas.md.venv")).unwrap();
        // The canvas file is gone, but a worker still holds its lock.
        let guard = match crate::worker_lock::try_acquire(&canvas).unwrap() {
            crate::worker_lock::Acquired::Us(guard) => guard,
            _ => panic!("nobody else holds this lock"),
        };

        let report = scan_of(&dir, false, &temp, SystemTime::now());
        assert!(report.candidates.is_empty(), "{:?}", report.candidates);
        assert_eq!(report.skipped.len(), 1);
        assert!(report.skipped[0].reason.contains("worker"));

        drop(guard);
        let report = scan_of(&dir, false, &temp, SystemTime::now());
        assert_eq!(report.candidates.len(), 1, "free once the worker is gone");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&temp);
    }

    #[test]
    fn recursion_is_opt_in_and_skips_vcs_and_dependency_directories() {
        let dir = temp_dir("recursive");
        let temp = temp_dir("recursive-temp");
        let sub = dir.join("sub");
        std::fs::create_dir_all(sub.join(".meshfox/gone.canvas.md.venv")).unwrap();
        let ignored = dir.join("node_modules/pkg");
        std::fs::create_dir_all(ignored.join(".meshfox/gone.canvas.md.venv")).unwrap();

        assert!(scan_of(&dir, false, &temp, SystemTime::now())
            .candidates
            .is_empty());
        let report = scan_of(&dir, true, &temp, SystemTime::now());
        assert_eq!(
            paths(&report),
            vec![sub.join(".meshfox/gone.canvas.md.venv")]
        );
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&temp);
    }

    #[test]
    fn stale_temp_files_are_only_meshfoxs_own_and_only_old_ones() {
        let dir = temp_dir("temp-root");
        let temp = temp_dir("temp");
        for name in [
            "meshfox-123-4.tmp",
            "meshfox-123-5.py",
            "meshfox-123-6f2a-9c1.tmp",
        ] {
            std::fs::write(temp.join(name), "x").unwrap();
        }
        // Not ours to remove: other meshfox entries and unrelated files.
        std::fs::write(temp.join("meshfox-vars-1-2.env"), "x").unwrap();
        std::fs::write(temp.join("meshfox-abc-1.tmp"), "x").unwrap();
        std::fs::write(temp.join("notes.tmp"), "x").unwrap();
        std::fs::create_dir_all(temp.join("meshfox-builtins")).unwrap();
        std::fs::create_dir_all(temp.join("meshfox-99-1.tmp.d")).unwrap();

        let fresh = scan_of(&dir, false, &temp, SystemTime::now());
        assert!(
            fresh.candidates.is_empty(),
            "a fresh temp file is a live run"
        );

        let later = SystemTime::now() + STALE_TEMP_AGE + Duration::from_secs(60);
        let report = scan_of(&dir, false, &temp, later);
        let mut names: Vec<String> = report
            .candidates
            .iter()
            .map(|c| c.path.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(
            names,
            [
                "meshfox-123-4.tmp",
                "meshfox-123-5.py",
                "meshfox-123-6f2a-9c1.tmp"
            ]
        );
        assert!(report
            .candidates
            .iter()
            .all(|c| c.kind == Kind::StaleTempFile));
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&temp);
    }

    #[test]
    fn remove_deletes_a_candidate_and_refuses_anything_else() {
        let dir = temp_dir("remove");
        let temp = temp_dir("remove-temp");
        std::fs::create_dir_all(dir.join(".meshfox/gone.canvas.md.venv/bin")).unwrap();
        let file = tangled(&dir, "old.canvas.md", "b");
        let report = scan_of(&dir, false, &temp, SystemTime::now());
        assert_eq!(report.candidates.len(), 2);
        for candidate in &report.candidates {
            remove(candidate).unwrap();
            assert!(!candidate.path.exists());
        }
        assert!(!file.exists());

        // A candidate that isn't what it says, or a symlink, is refused.
        let victim = dir.join("precious.txt");
        std::fs::write(&victim, "keep me").unwrap();
        let bogus = |path: PathBuf, kind| Candidate {
            path,
            kind,
            reason: String::new(),
            bytes: 0,
        };
        assert!(remove(&bogus(victim.clone(), Kind::TangledFile)).is_err());
        assert!(remove(&bogus(victim.clone(), Kind::StaleTempFile)).is_err());
        assert!(remove(&bogus(dir.clone(), Kind::StateDirectory)).is_err());
        let link = dir.join(format!("{}x__y.py", tangle::PREFIX));
        std::os::unix::fs::symlink(&victim, &link).unwrap();
        assert!(remove(&bogus(link, Kind::TangledFile)).is_err());
        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "keep me");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&temp);
    }
}
