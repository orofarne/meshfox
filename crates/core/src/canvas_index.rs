//! Index of the canvases under a served root, for `meshfox serve --http`.
//!
//! Slugs ([`crate::slug`]) are not reversible, so the gateway keeps this
//! index to turn a URL segment back into a path. It also feeds the index
//! page. Building it lists directories and reads metadata, and opens a plain
//! `*.md` file just far enough to see its first line; no canvas is parsed.
//!
//! What counts as a canvas is what the rest of meshfox calls one
//! ([`crate::mdcanvas::is_canvas`]): a `*.canvas.md` by name, or any other
//! `*.md` whose first line is the `meshfox:canvas` marker (a project's
//! `README.md` can be one).
//!
//! What is walked: everything below the root except
//! - names starting with `.` (files and directories), including `.git`;
//! - symlinks of any kind: a directory symlink could loop or leave the root,
//!   and a file symlink resolves to a target that is listed under its own name
//!   anyway;
//! - what a `.meshfoxignore` file excludes. It has `.gitignore` syntax (`#`
//!   comments, `dir/`, `*.log`, `/anchored`, `**`, `!negation`), lives in any
//!   directory and applies to that directory and everything below it, a
//!   deeper file overriding a shallower one. Nothing is excluded by name
//!   otherwise (not `node_modules`, not `target`): they may be what you serve.
//!
//! The tree is walked breadth first, so when a scan runs out of time or
//! entries the shallow canvases are the ones it found.

use std::collections::{HashMap, VecDeque};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant, SystemTime};

use ignore::gitignore::{Gitignore, GitignoreBuilder};

use crate::slug;

const CANVAS_SUFFIX: &str = ".canvas.md";
const MARKDOWN_SUFFIX: &str = ".md";

/// The file that excludes paths from the index (see the module docs).
pub const IGNORE_FILE: &str = ".meshfoxignore";

/// How much of a file is read to see whether it opens with the marker.
const MARKER_PROBE_BYTES: usize = 512;

/// Deepest directory level scanned (the root is level 0).
pub const DEFAULT_MAX_DEPTH: usize = 1000;
/// Most canvases one scan collects before it stops and reports `truncated`.
pub const DEFAULT_MAX_ENTRIES: usize = 100_000;
/// Shortest time between two rescans started by an unknown slug.
pub const DEFAULT_MIN_RESCAN: Duration = Duration::from_secs(2);
/// How old the index may get before looking at the index page starts a
/// rescan in the background.
pub const DEFAULT_MAX_AGE: Duration = Duration::from_secs(15);
/// Longest one scan runs; a bigger tree gives a partial (`truncated`) index.
pub const DEFAULT_MAX_DURATION: Duration = Duration::from_secs(60);
/// Longest a request waits for a scan that is already running.
pub const DEFAULT_WAIT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone)]
pub struct ScanOptions {
    pub max_depth: usize,
    pub max_entries: usize,
    pub max_duration: Duration,
}

impl Default for ScanOptions {
    fn default() -> Self {
        Self {
            max_depth: DEFAULT_MAX_DEPTH,
            max_entries: DEFAULT_MAX_ENTRIES,
            max_duration: DEFAULT_MAX_DURATION,
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

/// Whether the file `name` at `full` is a canvas: named `*.canvas.md`, or a
/// plain `*.md` that opens with the marker (only that is ever read, and only
/// its first line).
fn is_canvas_file(name: &str, full: &Path) -> bool {
    if name.ends_with(CANVAS_SUFFIX) {
        return true;
    }
    name.ends_with(MARKDOWN_SUFFIX) && opens_with_marker(full)
}

fn opens_with_marker(path: &Path) -> bool {
    use std::io::Read;
    let mut buf = [0u8; MARKER_PROBE_BYTES];
    let Ok(read) = std::fs::File::open(path).and_then(|mut f| f.read(&mut buf)) else {
        return false;
    };
    let head = String::from_utf8_lossy(&buf[..read]);
    crate::mdcanvas::has_marker(&head)
}

#[derive(Debug, Default)]
pub struct CanvasIndex {
    /// Sorted by path, so output is deterministic.
    entries: Vec<IndexEntry>,
    by_hash: HashMap<String, usize>,
    /// The scan stopped at `max_entries` or `max_duration`; there are more
    /// canvases than listed.
    pub truncated: bool,
    pub collisions: Vec<Collision>,
    /// Directory entries the scan looked at (what the walk cost).
    pub visited: usize,
    /// How long the walk took.
    pub scan_time: Duration,
}

/// The `.meshfoxignore` files above a directory, deepest first.
struct IgnoreLink {
    matcher: Gitignore,
    parent: Option<Arc<IgnoreLink>>,
}

type IgnoreChain = Option<Arc<IgnoreLink>>;

/// The ignore file in `dir` added to the chain above it. A file that cannot
/// be read, or lines in it that do not parse, exclude nothing rather than
/// stopping the scan.
fn with_ignore_file(dir: &Path, parent: IgnoreChain) -> IgnoreChain {
    let mut builder = GitignoreBuilder::new(dir);
    let _ = builder.add(dir.join(IGNORE_FILE));
    match builder.build() {
        Ok(matcher) => Some(Arc::new(IgnoreLink { matcher, parent })),
        Err(_) => parent,
    }
}

/// Whether the chain excludes `path`: the deepest file with an opinion wins,
/// and a `!` line in it brings back what a shallower one excluded.
fn is_ignored(chain: &IgnoreChain, path: &Path, is_dir: bool) -> bool {
    let mut link = chain.as_deref();
    while let Some(l) = link {
        match l.matcher.matched(path, is_dir) {
            ignore::Match::Ignore(_) => return true,
            ignore::Match::Whitelist(_) => return false,
            ignore::Match::None => link = l.parent.as_deref(),
        }
    }
    false
}

impl CanvasIndex {
    /// Walk `root` and index its canvases.
    pub fn scan(root: &Path, opts: &ScanOptions) -> io::Result<Self> {
        let mut index = Self::default();
        let started = Instant::now();
        let mut queue: VecDeque<(PathBuf, usize, IgnoreChain)> =
            VecDeque::from([(PathBuf::new(), 0, None)]);
        'walk: while let Some((rel_dir, depth, chain)) = queue.pop_front() {
            // A tree too big to walk in time (a mounted home directory) gives
            // what was found so far, marked truncated, instead of a scan that
            // never ends.
            if depth > 0 && started.elapsed() >= opts.max_duration {
                index.truncated = true;
                break;
            }
            let abs_dir = root.join(&rel_dir);
            let read = match std::fs::read_dir(&abs_dir) {
                Ok(read) => read,
                // The root itself must be readable; a subdirectory that
                // vanished or is private just contributes nothing.
                Err(e) if depth == 0 => return Err(e),
                Err(_) => continue,
            };
            let mut names: Vec<(String, std::fs::FileType, PathBuf)> = Vec::new();
            let mut has_ignore_file = false;
            for dirent in read.flatten() {
                let Ok(file_type) = dirent.file_type() else {
                    continue;
                };
                index.visited += 1;
                let name = dirent.file_name().to_string_lossy().into_owned();
                // The listing already says whether an ignore file is here, so
                // looking for one costs nothing extra.
                if name == IGNORE_FILE {
                    has_ignore_file = file_type.is_file();
                    continue;
                }
                if name.starts_with('.') || file_type.is_symlink() {
                    continue;
                }
                names.push((name, file_type, dirent.path()));
            }
            let chain = if has_ignore_file {
                with_ignore_file(&abs_dir, chain)
            } else {
                chain
            };
            names.sort_by(|a, b| a.0.cmp(&b.0));

            for (name, file_type, full) in &names {
                let is_dir = file_type.is_dir();
                if is_ignored(&chain, full, is_dir) {
                    continue;
                }
                if is_dir {
                    if depth < DEFAULT_MAX_DEPTH.min(opts.max_depth) {
                        queue.push_back((rel_dir.join(name), depth + 1, chain.clone()));
                    }
                    continue;
                }
                if !file_type.is_file() || !is_canvas_file(name, full) {
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
        index.scan_time = started.elapsed();
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

/// What [`IndexCache::resolve`] found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Lookup {
    Found(IndexEntry),
    NotFound,
    /// A scan is still running and did not finish within the wait: the slug
    /// may exist, the index just does not know yet.
    Indexing,
}

/// The index as last scanned, and whether a scan is running.
#[derive(Debug, Clone)]
pub struct Snapshot {
    pub index: Arc<CanvasIndex>,
    /// A scan has finished at least once (`index` is real, not the empty start).
    pub built: bool,
    pub scanning: bool,
    /// Why the last scan failed (an unreadable root), if it did.
    pub error: Option<String>,
}

/// The canvas index of a served root, kept fresh without ever making a request
/// wait for a walk of the tree.
///
/// A scan of a big tree (or a slow mount) takes seconds to minutes, so:
/// - scans run on their own thread, one at a time (a second request while one
///   runs shares it instead of starting another);
/// - the index page shows the last result at once and only *starts* a rescan
///   when the result is older than `max_age` ([`IndexCache::ensure_fresh`]);
/// - a slug the index does not know waits for a running scan, or starts one if
///   the last is older than `miss_throttle`, so a new file is reachable
///   without a restart but bogus URLs cannot make the server walk the disk in
///   a loop; a wait that outlasts `wait` ends as [`Lookup::Indexing`].
pub struct IndexCache {
    scanner: Scanner,
    miss_throttle: Duration,
    max_age: Duration,
    wait: Duration,
    state: Mutex<CacheState>,
    done: Condvar,
    scans: AtomicU64,
    on_scan: Mutex<Option<ScanHook>>,
}

/// What one scan does. The real one walks the root; tests substitute one that
/// is slow or blocks.
pub type Scanner = Box<dyn Fn() -> io::Result<CanvasIndex> + Send + Sync>;

/// How a finished scan went, for whoever wants to tell the user (a scan that
/// takes long is the user's cue to write a `.meshfoxignore`).
#[derive(Debug, Clone)]
pub struct ScanReport {
    pub duration: Duration,
    /// Directory entries looked at.
    pub visited: usize,
    /// It stopped early (too many canvases, or out of time).
    pub truncated: bool,
    pub error: Option<String>,
}

pub type ScanHook = Box<dyn Fn(&ScanReport) + Send + Sync>;

struct CacheState {
    index: Arc<CanvasIndex>,
    built: bool,
    finished_at: Option<Instant>,
    scanning: bool,
    error: Option<String>,
}

impl IndexCache {
    /// Nothing is scanned yet: call [`IndexCache::ensure_fresh`] to start the
    /// first scan in the background.
    pub fn new(root: PathBuf, opts: ScanOptions, miss_throttle: Duration) -> Arc<Self> {
        Self::with_timings(root, opts, miss_throttle, DEFAULT_MAX_AGE, DEFAULT_WAIT)
    }

    pub fn with_timings(
        root: PathBuf,
        opts: ScanOptions,
        miss_throttle: Duration,
        max_age: Duration,
        wait: Duration,
    ) -> Arc<Self> {
        let scanner: Scanner = Box::new(move || CanvasIndex::scan(&root, &opts));
        Self::with_scanner(scanner, miss_throttle, max_age, wait)
    }

    /// An index fed by `scanner` instead of a walk of a directory.
    pub fn with_scanner(
        scanner: Scanner,
        miss_throttle: Duration,
        max_age: Duration,
        wait: Duration,
    ) -> Arc<Self> {
        Arc::new(Self {
            scanner,
            miss_throttle,
            max_age,
            wait,
            state: Mutex::new(CacheState {
                index: Arc::new(CanvasIndex::default()),
                built: false,
                finished_at: None,
                scanning: false,
                error: None,
            }),
            done: Condvar::new(),
            scans: AtomicU64::new(0),
            on_scan: Mutex::new(None),
        })
    }

    /// Call `hook` after every scan that finishes (replacing a previous one).
    pub fn on_scan(&self, hook: impl Fn(&ScanReport) + Send + Sync + 'static) {
        *self.on_scan.lock().unwrap_or_else(|p| p.into_inner()) = Some(Box::new(hook));
    }

    /// How many scans have been started (for tests).
    pub fn scans_started(&self) -> u64 {
        self.scans.load(Ordering::SeqCst)
    }

    /// The last result, at once.
    pub fn snapshot(&self) -> Snapshot {
        let state = self.lock();
        Snapshot {
            index: Arc::clone(&state.index),
            built: state.built,
            scanning: state.scanning,
            error: state.error.clone(),
        }
    }

    /// Start a background scan if there is none yet or the last one is older
    /// than `max_age`, and none is running. Never waits.
    pub fn ensure_fresh(self: &Arc<Self>) {
        let due = {
            let state = self.lock();
            !state.scanning
                && state
                    .finished_at
                    .is_none_or(|at| at.elapsed() >= self.max_age)
        };
        if due {
            self.start_background_scan();
        }
    }

    fn start_background_scan(self: &Arc<Self>) {
        if !self.claim_scan() {
            return;
        }
        let cache = Arc::clone(self);
        std::thread::spawn(move || cache.run_claimed_scan());
    }

    /// Mark a scan as running; false if one already is.
    fn claim_scan(&self) -> bool {
        let mut state = self.lock();
        if state.scanning {
            return false;
        }
        state.scanning = true;
        self.scans.fetch_add(1, Ordering::SeqCst);
        true
    }

    /// Run the scan this thread claimed, store the result, wake the waiters.
    fn run_claimed_scan(&self) {
        let result = (self.scanner)();
        let report = match &result {
            Ok(index) => ScanReport {
                duration: index.scan_time,
                visited: index.visited,
                truncated: index.truncated,
                error: None,
            },
            Err(e) => ScanReport {
                duration: Duration::ZERO,
                visited: 0,
                truncated: false,
                error: Some(e.to_string()),
            },
        };
        let mut state = self.lock();
        match result {
            Ok(index) => {
                state.index = Arc::new(index);
                state.built = true;
                state.error = None;
            }
            Err(e) => state.error = Some(e.to_string()),
        }
        state.finished_at = Some(Instant::now());
        state.scanning = false;
        self.done.notify_all();
        drop(state);
        if let Some(hook) = self
            .on_scan
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
        {
            hook(&report);
        }
    }

    /// Resolve a URL segment. Blocks for at most the cache's wait, so call it
    /// from a blocking thread, not an async one.
    pub fn resolve(self: &Arc<Self>, segment: &str) -> Lookup {
        if slug::hash_of(segment).is_none() {
            return Lookup::NotFound;
        }
        let deadline = Instant::now() + self.wait;
        let mut state = self.lock();
        loop {
            if let Some(entry) = state.index.resolve(segment) {
                return Lookup::Found(entry.clone());
            }
            if state.scanning {
                // Share the scan that is running instead of starting another.
                let left = deadline.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    return Lookup::Indexing;
                }
                state = self
                    .done
                    .wait_timeout_while(state, left, |s| s.scanning)
                    .unwrap_or_else(|p| p.into_inner())
                    .0;
                continue;
            }
            // Unknown, and nothing is scanning. A scan that finished a moment
            // ago has already looked: the slug does not exist.
            if state
                .finished_at
                .is_some_and(|at| at.elapsed() < self.miss_throttle)
            {
                return Lookup::NotFound;
            }
            // Otherwise this thread scans, so the lookups that arrive while it
            // does share the result.
            state.scanning = true;
            self.scans.fetch_add(1, Ordering::SeqCst);
            drop(state);
            self.run_claimed_scan();
            // That scan has just looked: what it found is the answer.
            return match self.lock().index.resolve(segment) {
                Some(entry) => Lookup::Found(entry.clone()),
                None => Lookup::NotFound,
            };
        }
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
    fn a_plain_md_is_a_canvas_only_if_it_opens_with_the_marker() {
        let root = tmp("marker");
        fs::write(
            root.join("README.md"),
            "<!-- meshfox:canvas -->\n# Readme\n",
        )
        .unwrap();
        fs::write(root.join("notes.md"), "# just notes\n").unwrap();
        // The marker has to be the first line, as everywhere else in meshfox.
        fs::write(root.join("late.md"), "intro\n<!-- meshfox:canvas -->\n").unwrap();
        fs::write(root.join("empty.md"), "").unwrap();
        fs::write(root.join("notes.txt"), "<!-- meshfox:canvas -->\n").unwrap();
        touch(&root, "sub/real.canvas.md");
        fs::write(root.join("sub/doc.md"), "<!-- meshfox:canvas -->\n").unwrap();
        let index = CanvasIndex::scan(&root, &ScanOptions::default()).unwrap();
        assert_eq!(
            rels(&index),
            ["README.md", "sub/doc.md", "sub/real.canvas.md"]
        );
        let readme = index
            .entries()
            .iter()
            .find(|e| e.rel == Path::new("README.md"))
            .unwrap();
        assert!(readme.slug.starts_with("readme-"), "{}", readme.slug);
        assert_eq!(index.resolve(&readme.slug), Some(readme));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_marker_file_with_a_long_or_binary_head_does_not_break_the_probe() {
        let root = tmp("probe");
        fs::write(root.join("long.md"), format!("{}\n", "x".repeat(5000))).unwrap();
        fs::write(root.join("bin.md"), [0xff, 0xfe, 0x00, 0x01]).unwrap();
        fs::write(root.join("ok.md"), "<!-- meshfox:canvas -->\r\n# crlf\n").unwrap();
        let index = CanvasIndex::scan(&root, &ScanOptions::default()).unwrap();
        assert_eq!(rels(&index), ["ok.md"]);
        let _ = fs::remove_dir_all(&root);
    }

    fn ignore_file(root: &Path, rel_dir: &str, text: &str) {
        let dir = root.join(rel_dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(IGNORE_FILE), text).unwrap();
    }

    fn marker_md(root: &Path, rel: &str) {
        let path = root.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, "<!-- meshfox:canvas -->\n").unwrap();
    }

    #[test]
    fn nothing_is_skipped_by_name_unless_it_starts_with_a_dot_or_is_ignored() {
        let root = tmp("no-heuristics");
        // These are what a project might well serve: no guessing by name.
        touch(&root, "target/report.canvas.md");
        touch(&root, "node_modules/pkg/x.canvas.md");
        marker_md(&root, "node_modules/pkg/README.md");
        touch(&root, "__pycache__/y.canvas.md");
        touch(&root, ".hidden/z.canvas.md");
        let index = CanvasIndex::scan(&root, &ScanOptions::default()).unwrap();
        assert_eq!(
            rels(&index),
            [
                "__pycache__/y.canvas.md",
                "node_modules/pkg/README.md",
                "node_modules/pkg/x.canvas.md",
                "target/report.canvas.md",
            ]
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_root_ignore_file_excludes_what_it_names_with_gitignore_syntax() {
        let root = tmp("ignore-root");
        touch(&root, "keep.canvas.md");
        touch(&root, "target/a.canvas.md");
        touch(&root, "node_modules/pkg/b.canvas.md");
        touch(&root, "drafts/c.canvas.md");
        touch(&root, "sub/drafts/d.canvas.md");
        touch(&root, "sub/e.draft.canvas.md");
        touch(&root, "top-only.canvas.md");
        touch(&root, "sub/top-only.canvas.md");
        ignore_file(
            &root,
            "",
            "# build output and dependencies\ntarget/\nnode_modules/\n\n\
             /drafts/\n*.draft.canvas.md\n/top-only.canvas.md\n",
        );
        let index = CanvasIndex::scan(&root, &ScanOptions::default()).unwrap();
        assert_eq!(
            rels(&index),
            [
                "keep.canvas.md",
                "sub/drafts/d.canvas.md",
                "sub/top-only.canvas.md"
            ],
            "`/drafts/` and `/top-only…` are anchored to the root, `target/` is not"
        );
        // The ignore file itself starts with a dot, so it is never listed.
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn an_ignored_marker_md_is_not_even_opened() {
        let root = tmp("ignore-md");
        marker_md(&root, "vendor/README.md");
        marker_md(&root, "README.md");
        ignore_file(&root, "", "vendor/\n");
        let index = CanvasIndex::scan(&root, &ScanOptions::default()).unwrap();
        assert_eq!(rels(&index), ["README.md"]);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_nested_ignore_file_applies_to_its_directory_and_below_only() {
        let root = tmp("ignore-nested");
        touch(&root, "a/keep.canvas.md");
        touch(&root, "a/private/x.canvas.md");
        touch(&root, "a/deep/private/y.canvas.md");
        touch(&root, "b/private/z.canvas.md");
        ignore_file(&root, "a", "private/\n");
        let index = CanvasIndex::scan(&root, &ScanOptions::default()).unwrap();
        assert_eq!(rels(&index), ["a/keep.canvas.md", "b/private/z.canvas.md"]);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_deeper_file_can_bring_back_what_a_shallower_one_excluded() {
        let root = tmp("ignore-negate");
        touch(&root, "docs/a.canvas.md");
        touch(&root, "docs/api/b.canvas.md");
        touch(&root, "docs/api/c.canvas.md");
        ignore_file(&root, "", "*.canvas.md\n");
        ignore_file(&root, "docs", "!*.canvas.md\n");
        ignore_file(&root, "docs/api", "c.canvas.md\n");
        let index = CanvasIndex::scan(&root, &ScanOptions::default()).unwrap();
        assert_eq!(rels(&index), ["docs/a.canvas.md", "docs/api/b.canvas.md"]);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn an_ignore_file_inside_an_ignored_directory_is_never_read() {
        let root = tmp("ignore-inside");
        touch(&root, "out/x.canvas.md");
        touch(&root, "out/sub/y.canvas.md");
        ignore_file(&root, "", "out/\n");
        ignore_file(&root, "out", "!*\n");
        let index = CanvasIndex::scan(&root, &ScanOptions::default()).unwrap();
        assert!(rels(&index).is_empty());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_broken_ignore_file_excludes_nothing_and_does_not_stop_the_scan() {
        let root = tmp("ignore-broken");
        touch(&root, "a.canvas.md");
        touch(&root, "b/c.canvas.md");
        fs::write(root.join(IGNORE_FILE), b"\xff\xfe[unclosed\n\x00\n").unwrap();
        // A directory where the file should be is not a file at all.
        fs::create_dir_all(root.join("b").join(IGNORE_FILE)).unwrap();
        let index = CanvasIndex::scan(&root, &ScanOptions::default()).unwrap();
        assert_eq!(rels(&index), ["a.canvas.md", "b/c.canvas.md"]);
        let _ = fs::remove_dir_all(&root);
    }

    /// With a limit, the canvases found are the shallow ones, not whatever
    /// comes first alphabetically along one branch.
    #[test]
    fn a_limited_scan_finds_the_shallow_canvases_first() {
        let root = tmp("breadth");
        touch(&root, "a/b/c/deep.canvas.md");
        touch(&root, "z/shallow.canvas.md");
        let opts = ScanOptions {
            max_entries: 1,
            ..ScanOptions::default()
        };
        let index = CanvasIndex::scan(&root, &opts).unwrap();
        assert_eq!(rels(&index), ["z/shallow.canvas.md"]);
        assert!(index.truncated);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn the_scan_says_how_much_it_looked_at() {
        let root = tmp("visited");
        touch(&root, "a.canvas.md");
        touch(&root, "d/b.canvas.md");
        touch(&root, "d/notes.txt");
        let index = CanvasIndex::scan(&root, &ScanOptions::default()).unwrap();
        // a.canvas.md, d, b.canvas.md, notes.txt
        assert_eq!(index.visited, 4);
        let _ = fs::remove_dir_all(&root);
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

    fn slug_of(rel: &str) -> String {
        slug::slug_for_rel(Path::new(rel)).unwrap()
    }

    fn wait_built(cache: &IndexCache) {
        for _ in 0..200 {
            if cache.snapshot().built {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("the first scan never finished");
    }

    #[test]
    fn nothing_is_scanned_until_asked_and_the_start_is_empty() {
        let root = tmp("cache-start");
        touch(&root, "a.canvas.md");
        let cache = IndexCache::new(root.clone(), ScanOptions::default(), Duration::ZERO);
        let snap = cache.snapshot();
        assert!(!snap.built && !snap.scanning && snap.index.entries().is_empty());
        assert_eq!(cache.scans_started(), 0);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn ensure_fresh_scans_in_the_background_and_only_when_due() {
        let root = tmp("cache-fresh");
        touch(&root, "a.canvas.md");
        let cache = IndexCache::with_timings(
            root.clone(),
            ScanOptions::default(),
            Duration::from_secs(3600),
            Duration::from_secs(3600),
            DEFAULT_WAIT,
        );
        cache.ensure_fresh();
        wait_built(&cache);
        assert_eq!(rels(&cache.snapshot().index), ["a.canvas.md"]);
        // Fresh enough: asking again starts nothing.
        for _ in 0..5 {
            cache.ensure_fresh();
        }
        assert_eq!(cache.scans_started(), 1);
        // Old enough: it starts one.
        let old = IndexCache::with_timings(
            root.clone(),
            ScanOptions::default(),
            Duration::ZERO,
            Duration::ZERO,
            DEFAULT_WAIT,
        );
        old.ensure_fresh();
        wait_built(&old);
        old.ensure_fresh();
        for _ in 0..200 {
            if old.scans_started() >= 2 && !old.snapshot().scanning {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(old.scans_started() >= 2);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_lookup_waits_for_the_first_scan_and_finds_a_new_file_after_the_throttle() {
        let root = tmp("cache-new");
        touch(&root, "old.canvas.md");
        let cache = IndexCache::new(root.clone(), ScanOptions::default(), Duration::ZERO);
        // No scan yet: the lookup starts one itself and waits for it.
        assert!(
            matches!(cache.resolve(&slug_of("old.canvas.md")), Lookup::Found(e) if e.rel == Path::new("old.canvas.md"))
        );
        touch(&root, "fresh.canvas.md");
        assert!(matches!(
            cache.resolve(&slug_of("fresh.canvas.md")),
            Lookup::Found(_)
        ));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn an_unknown_slug_inside_the_throttle_window_does_not_rescan() {
        let root = tmp("cache-throttle");
        touch(&root, "old.canvas.md");
        let cache = IndexCache::new(
            root.clone(),
            ScanOptions::default(),
            Duration::from_secs(3600),
        );
        assert!(matches!(
            cache.resolve(&slug_of("old.canvas.md")),
            Lookup::Found(_)
        ));
        touch(&root, "fresh.canvas.md");
        assert_eq!(cache.resolve(&slug_of("fresh.canvas.md")), Lookup::NotFound);
        assert_eq!(cache.scans_started(), 1);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn junk_segments_never_scan() {
        let root = tmp("cache-junk");
        touch(&root, "a.canvas.md");
        let cache = IndexCache::new(root.clone(), ScanOptions::default(), Duration::ZERO);
        for junk in ["nope", "x-SHORT", "-aaaaaaaa", ""] {
            assert_eq!(cache.resolve(junk), Lookup::NotFound);
        }
        assert_eq!(cache.scans_started(), 0);
        let _ = fs::remove_dir_all(&root);
    }

    /// The reported hang: every request that needed the index started its own
    /// walk of the tree. Concurrent lookups must share one.
    #[test]
    fn concurrent_lookups_of_an_unknown_slug_share_one_scan() {
        let root = tmp("cache-herd");
        for i in 0..50 {
            touch(&root, &format!("d{i}/c.canvas.md"));
        }
        let cache = IndexCache::new(
            root.clone(),
            ScanOptions::default(),
            Duration::from_secs(3600),
        );
        let slug = slug_of("nobody-has-this.canvas.md");
        let barrier = Arc::new(std::sync::Barrier::new(16));
        let threads: Vec<_> = (0..16)
            .map(|_| {
                let (cache, barrier, slug) =
                    (Arc::clone(&cache), Arc::clone(&barrier), slug.clone());
                std::thread::spawn(move || {
                    barrier.wait();
                    cache.resolve(&slug)
                })
            })
            .collect();
        for t in threads {
            assert_eq!(t.join().unwrap(), Lookup::NotFound);
        }
        assert_eq!(cache.scans_started(), 1);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_lookup_that_outwaits_a_running_scan_says_indexing_not_not_found() {
        let root = tmp("cache-indexing");
        let cache = IndexCache::with_timings(
            root.clone(),
            ScanOptions::default(),
            Duration::ZERO,
            DEFAULT_MAX_AGE,
            Duration::from_millis(50),
        );
        // A scan that never finishes (a huge tree).
        cache.lock().scanning = true;
        let started = Instant::now();
        assert_eq!(cache.resolve(&slug_of("a.canvas.md")), Lookup::Indexing);
        assert!(started.elapsed() < Duration::from_secs(5));
        // The page still gets an answer at once, and says it is scanning.
        let snap = cache.snapshot();
        assert!(snap.scanning && !snap.built);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_scan_that_runs_out_of_time_returns_what_it_found_marked_truncated() {
        let root = tmp("cache-budget");
        touch(&root, "top.canvas.md");
        for i in 0..20 {
            touch(&root, &format!("d{i}/x.canvas.md"));
        }
        let opts = ScanOptions {
            max_duration: Duration::ZERO,
            ..ScanOptions::default()
        };
        let index = CanvasIndex::scan(&root, &opts).unwrap();
        assert!(index.truncated);
        assert_eq!(
            rels(&index),
            ["top.canvas.md"],
            "the root itself is always read"
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn an_unreadable_root_is_reported_and_nothing_is_found() {
        let root = tmp("cache-gone");
        let missing = root.join("nope");
        let cache = IndexCache::new(missing, ScanOptions::default(), Duration::ZERO);
        assert_eq!(cache.resolve(&slug_of("a.canvas.md")), Lookup::NotFound);
        let snap = cache.snapshot();
        assert!(!snap.built && snap.error.is_some());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn every_finished_scan_is_reported_with_its_cost() {
        let root = tmp("report");
        touch(&root, "a.canvas.md");
        let cache = IndexCache::new(root.clone(), ScanOptions::default(), Duration::ZERO);
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&seen);
        cache.on_scan(move |r| {
            sink.lock()
                .unwrap()
                .push((r.visited, r.truncated, r.error.clone()))
        });
        assert!(matches!(
            cache.resolve(&slug_of("a.canvas.md")),
            Lookup::Found(_)
        ));
        assert_eq!(*seen.lock().unwrap(), [(1, false, None)]);
        let _ = fs::remove_dir_all(&root);
    }
}
