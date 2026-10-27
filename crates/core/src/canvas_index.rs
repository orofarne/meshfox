//! Index of the canvases under a served root, for `meshfox serve --http`.
//!
//! Slugs ([`crate::slug`]) are not reversible, so the gateway keeps this
//! index to turn a URL segment back into a path. It also feeds the index
//! page. Building it only lists directories and reads metadata; no canvas
//! file is opened.
//!
//! What is walked: every `*.canvas.md` file below the root. Names starting
//! with `.` (files and directories) are skipped, and so are symlinks of any
//! kind: a directory symlink could loop or leave the root, and a file symlink
//! resolves to a target that is listed under its own name anyway.

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use crate::slug;

const CANVAS_SUFFIX: &str = ".canvas.md";

/// Deepest directory level scanned (the root is level 0).
pub const DEFAULT_MAX_DEPTH: usize = 1000;
/// Most canvases one scan collects before it stops and reports `truncated`.
pub const DEFAULT_MAX_ENTRIES: usize = 100_000;
/// Shortest time between two rescans triggered by unknown slugs.
pub const DEFAULT_MIN_RESCAN: Duration = Duration::from_secs(2);

#[derive(Debug, Clone)]
pub struct ScanOptions {
    pub max_depth: usize,
    pub max_entries: usize,
}

impl Default for ScanOptions {
    fn default() -> Self {
        Self {
            max_depth: DEFAULT_MAX_DEPTH,
            max_entries: DEFAULT_MAX_ENTRIES,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexEntry {
    /// Path relative to the root, as walked.
    pub rel: PathBuf,
    pub slug: String,
    pub mtime: Option<SystemTime>,
}

/// Two canvases whose slugs share a hash. The first (in walk order) keeps the
/// slug; the second is left out of the lookup table and reported here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Collision {
    pub kept: PathBuf,
    pub dropped: PathBuf,
}

#[derive(Debug, Default)]
pub struct CanvasIndex {
    /// Sorted by path, so output is deterministic.
    entries: Vec<IndexEntry>,
    by_hash: HashMap<String, usize>,
    /// The scan stopped at `max_entries`; there are more canvases than listed.
    pub truncated: bool,
    pub collisions: Vec<Collision>,
}

impl CanvasIndex {
    /// Walk `root` and index its canvases.
    pub fn scan(root: &Path, opts: &ScanOptions) -> io::Result<Self> {
        let mut index = Self::default();
        let mut stack: Vec<(PathBuf, usize)> = vec![(PathBuf::new(), 0)];
        'walk: while let Some((rel_dir, depth)) = stack.pop() {
            let mut names: Vec<(String, std::fs::FileType, PathBuf)> = Vec::new();
            let read = match std::fs::read_dir(root.join(&rel_dir)) {
                Ok(read) => read,
                // The root itself must be readable; a subdirectory that
                // vanished or is private just contributes nothing.
                Err(e) if depth == 0 => return Err(e),
                Err(_) => continue,
            };
            for dirent in read.flatten() {
                let Ok(file_type) = dirent.file_type() else {
                    continue;
                };
                let name = dirent.file_name().to_string_lossy().into_owned();
                if name.starts_with('.') || file_type.is_symlink() {
                    continue;
                }
                names.push((name, file_type, dirent.path()));
            }
            names.sort_by(|a, b| a.0.cmp(&b.0));

            // Directories go on the stack in reverse so they pop in name order.
            for (name, file_type, _) in names.iter().rev() {
                if file_type.is_dir() && depth < DEFAULT_MAX_DEPTH.min(opts.max_depth) {
                    stack.push((rel_dir.join(name), depth + 1));
                }
            }
            for (name, file_type, full) in &names {
                if !file_type.is_file() || !name.ends_with(CANVAS_SUFFIX) {
                    continue;
                }
                if index.entries.len() >= opts.max_entries {
                    index.truncated = true;
                    break 'walk;
                }
                let rel = rel_dir.join(name);
                let Ok(slug) = slug::slug_for_rel(&rel) else {
                    continue;
                };
                let mtime = std::fs::metadata(full).and_then(|m| m.modified()).ok();
                index.entries.push(IndexEntry { rel, slug, mtime });
            }
        }
        index.entries.sort_by(|a, b| a.rel.cmp(&b.rel));
        index.build_lookup();
        Ok(index)
    }

    fn build_lookup(&mut self) {
        let mut kept: Vec<IndexEntry> = Vec::with_capacity(self.entries.len());
        for entry in std::mem::take(&mut self.entries) {
            let hash = slug::hash_of(&entry.slug).unwrap_or_default().to_string();
            match self.by_hash.get(&hash) {
                Some(&at) => self.collisions.push(Collision {
                    kept: kept[at].rel.clone(),
                    dropped: entry.rel,
                }),
                None => {
                    self.by_hash.insert(hash, kept.len());
                    kept.push(entry);
                }
            }
        }
        self.entries = kept;
    }

    /// Every indexed canvas, sorted by path.
    pub fn entries(&self) -> &[IndexEntry] {
        &self.entries
    }

    /// The canvas a URL segment names, if it is exactly that canvas's slug.
    pub fn resolve(&self, segment: &str) -> Option<&IndexEntry> {
        let hash = slug::hash_of(segment)?;
        let entry = &self.entries[*self.by_hash.get(hash)?];
        slug::verify(&entry.rel, segment).then_some(entry)
    }
}

/// An index that rescans when asked about a slug it does not know, at most
/// once per `min_rescan`, so a new file is reachable without a restart but a
/// stream of bogus URLs cannot make the server walk the disk in a loop.
pub struct IndexCache {
    root: PathBuf,
    opts: ScanOptions,
    min_rescan: Duration,
    state: Mutex<CacheState>,
}

struct CacheState {
    index: Arc<CanvasIndex>,
    scanned_at: Instant,
}

impl IndexCache {
    pub fn new(root: PathBuf, opts: ScanOptions, min_rescan: Duration) -> io::Result<Self> {
        let index = Arc::new(CanvasIndex::scan(&root, &opts)?);
        Ok(Self {
            root,
            opts,
            min_rescan,
            state: Mutex::new(CacheState {
                index,
                scanned_at: Instant::now(),
            }),
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The current index, as of the last scan.
    pub fn snapshot(&self) -> Arc<CanvasIndex> {
        Arc::clone(&self.lock().index)
    }

    /// The index, rescanned first if the last scan is older than `min_rescan`
    /// (the index page uses this, so a new file shows up within seconds).
    pub fn current(&self) -> io::Result<Arc<CanvasIndex>> {
        let stale = self.lock().scanned_at.elapsed() >= self.min_rescan;
        if stale {
            self.refresh()
        } else {
            Ok(self.snapshot())
        }
    }

    /// Rescan now, regardless of throttling.
    pub fn refresh(&self) -> io::Result<Arc<CanvasIndex>> {
        let index = Arc::new(CanvasIndex::scan(&self.root, &self.opts)?);
        let mut state = self.lock();
        state.index = Arc::clone(&index);
        state.scanned_at = Instant::now();
        Ok(index)
    }

    /// Resolve a URL segment, rescanning once if it is unknown and the last
    /// scan is old enough.
    pub fn resolve(&self, segment: &str) -> Option<IndexEntry> {
        slug::hash_of(segment)?;
        let (index, stale) = {
            let state = self.lock();
            (
                Arc::clone(&state.index),
                state.scanned_at.elapsed() >= self.min_rescan,
            )
        };
        if let Some(entry) = index.resolve(segment) {
            return Some(entry.clone());
        }
        if !stale {
            return None;
        }
        self.refresh().ok()?.resolve(segment).cloned()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, CacheState> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tmp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("mfx-index-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir.canonicalize().unwrap()
    }

    fn touch(root: &Path, rel: &str) {
        let path = root.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, "# t\n").unwrap();
    }

    fn rels(index: &CanvasIndex) -> Vec<String> {
        index
            .entries()
            .iter()
            .map(|e| e.rel.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn lists_canvases_sorted_and_ignores_other_files() {
        let root = tmp("list");
        touch(&root, "b.canvas.md");
        touch(&root, "a.canvas.md");
        touch(&root, "notes/z.canvas.md");
        touch(&root, "notes/readme.md");
        touch(&root, "plain.txt");
        let index = CanvasIndex::scan(&root, &ScanOptions::default()).unwrap();
        assert_eq!(
            rels(&index),
            ["a.canvas.md", "b.canvas.md", "notes/z.canvas.md"]
        );
        assert!(!index.truncated);
        assert!(index.collisions.is_empty());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn dot_names_are_skipped_for_files_and_directories() {
        let root = tmp("dots");
        touch(&root, "ok.canvas.md");
        touch(&root, ".hidden.canvas.md");
        touch(&root, ".git/inner.canvas.md");
        touch(&root, ".meshfox/state.canvas.md");
        touch(&root, "sub/.secret/x.canvas.md");
        let index = CanvasIndex::scan(&root, &ScanOptions::default()).unwrap();
        assert_eq!(rels(&index), ["ok.canvas.md"]);
        let _ = fs::remove_dir_all(&root);
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_are_not_followed_or_listed() {
        use std::os::unix::fs::symlink;
        let root = tmp("symlinks");
        let outside = tmp("symlinks-outside");
        touch(&root, "real/a.canvas.md");
        touch(&outside, "far.canvas.md");
        symlink(&outside, root.join("out-dir")).unwrap();
        symlink(root.join("real"), root.join("loop-dir")).unwrap();
        symlink(root.join("real/a.canvas.md"), root.join("alias.canvas.md")).unwrap();
        let index = CanvasIndex::scan(&root, &ScanOptions::default()).unwrap();
        assert_eq!(rels(&index), ["real/a.canvas.md"]);
        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_dir_all(&outside);
    }

    #[test]
    fn depth_is_limited() {
        let root = tmp("depth");
        touch(&root, "top.canvas.md");
        touch(&root, "a/one.canvas.md");
        touch(&root, "a/b/two.canvas.md");
        let opts = ScanOptions {
            max_depth: 1,
            ..ScanOptions::default()
        };
        let index = CanvasIndex::scan(&root, &opts).unwrap();
        assert_eq!(rels(&index), ["a/one.canvas.md", "top.canvas.md"]);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn entry_limit_marks_the_index_truncated() {
        let root = tmp("limit");
        for i in 0..5 {
            touch(&root, &format!("c{i}.canvas.md"));
        }
        let opts = ScanOptions {
            max_entries: 3,
            ..ScanOptions::default()
        };
        let index = CanvasIndex::scan(&root, &opts).unwrap();
        assert_eq!(index.entries().len(), 3);
        assert!(index.truncated);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn unreadable_root_is_an_error() {
        let root = tmp("gone");
        let missing = root.join("nope");
        assert!(CanvasIndex::scan(&missing, &ScanOptions::default()).is_err());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn resolve_returns_the_entry_only_for_its_exact_slug() {
        let root = tmp("resolve");
        touch(&root, "notes/todo.canvas.md");
        let index = CanvasIndex::scan(&root, &ScanOptions::default()).unwrap();
        let entry = &index.entries()[0];
        assert_eq!(index.resolve(&entry.slug), Some(entry));
        let hash = slug::hash_of(&entry.slug).unwrap();
        assert_eq!(index.resolve(&format!("wrong-{hash}")), None);
        assert_eq!(index.resolve("nonsense"), None);
        assert_eq!(index.resolve("todo-aaaaaaaa"), None);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn colliding_hashes_keep_the_first_and_report_the_second() {
        // A real 40-bit collision cannot be constructed, so feed the lookup
        // builder two entries with the same slug directly.
        let mut index = CanvasIndex {
            entries: vec![
                IndexEntry {
                    rel: "a.canvas.md".into(),
                    slug: "x-aaaaaaaa".into(),
                    mtime: None,
                },
                IndexEntry {
                    rel: "b.canvas.md".into(),
                    slug: "y-aaaaaaaa".into(),
                    mtime: None,
                },
            ],
            ..CanvasIndex::default()
        };
        index.build_lookup();
        assert_eq!(rels(&index), ["a.canvas.md"]);
        assert_eq!(
            index.collisions,
            [Collision {
                kept: "a.canvas.md".into(),
                dropped: "b.canvas.md".into()
            }]
        );
    }

    #[test]
    fn cache_finds_a_new_file_after_the_throttle_window() {
        let root = tmp("cache");
        touch(&root, "old.canvas.md");
        let cache = IndexCache::new(root.clone(), ScanOptions::default(), Duration::ZERO).unwrap();
        touch(&root, "fresh.canvas.md");
        let slug = slug::slug_for_rel(Path::new("fresh.canvas.md")).unwrap();
        assert_eq!(
            cache.resolve(&slug).map(|e| e.rel),
            Some("fresh.canvas.md".into())
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn cache_does_not_rescan_inside_the_throttle_window() {
        let root = tmp("cache-throttle");
        touch(&root, "old.canvas.md");
        let cache = IndexCache::new(
            root.clone(),
            ScanOptions::default(),
            Duration::from_secs(3600),
        )
        .unwrap();
        touch(&root, "fresh.canvas.md");
        let slug = slug::slug_for_rel(Path::new("fresh.canvas.md")).unwrap();
        assert_eq!(cache.resolve(&slug), None);
        // An explicit refresh ignores the window.
        cache.refresh().unwrap();
        assert!(cache.resolve(&slug).is_some());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn cache_ignores_segments_that_cannot_be_slugs_without_scanning() {
        let root = tmp("cache-junk");
        touch(&root, "a.canvas.md");
        let cache = IndexCache::new(root.clone(), ScanOptions::default(), Duration::ZERO).unwrap();
        touch(&root, "later.canvas.md");
        assert_eq!(cache.resolve("not-a-slug"), None);
        // The junk lookup must not have refreshed the snapshot.
        assert_eq!(cache.snapshot().entries().len(), 1);
        let _ = fs::remove_dir_all(&root);
    }
}
