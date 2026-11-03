//! Packed canvases on disk (SPEC.md's "Packed canvases"): [`collect`] walks the
//! files a canvas reads, [`write_pack`] stores them as one `.canvas.tar.xz`,
//! [`unpack`] puts a pack's files into a new directory. The archive format,
//! the checks on it and the xz coding are `meshfox_model::pack`, shared with
//! the browser editor.
//!
//! Collecting never walks a directory. It starts at the entry canvas and
//! follows the references meshfox itself would read — includes, `file`
//! targets, local images and links in Markdown — through [`FileAccess`], the
//! same checks every other file read goes through. On top of those, every
//! reference must stay inside the directory of the file that holds it (see
//! the SPEC), which is the strictest rule any front end applies, so a pack
//! behaves the same wherever it is opened.

pub use meshfox_model::pack::*;

use std::collections::{BTreeMap, HashSet};
use std::fmt;
use std::io;
use std::path::{Component, Path, PathBuf};

use crate::canvas::{Canvas, NodeType};
use crate::dirvars::FileAccess;
use crate::file_source::ConfineError;

/// What [`collect`] was asked for.
#[derive(Debug, Clone, Copy, Default)]
pub struct CollectOptions {
    /// Do not follow references to other canvases: they stay out of the pack,
    /// and so does everything only they reference.
    pub shallow: bool,
}

/// Everything a pack is made of.
#[derive(Debug, Clone)]
pub struct Collected {
    /// Path of the entry canvas inside the pack.
    pub entry: String,
    /// The files, sorted by path.
    pub files: Vec<PackFile>,
    /// Things that were left out or that deserve a look, one line each.
    pub warnings: Vec<String>,
}

/// Why a pack cannot be made (or opened); the message names the canvas, the
/// node and the path involved.
#[derive(Debug)]
pub struct PackProblem(pub String);

impl fmt::Display for PackProblem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for PackProblem {}

impl From<PackError> for PackProblem {
    fn from(e: PackError) -> Self {
        PackProblem(e.to_string())
    }
}

fn problem<T>(message: impl Into<String>) -> Result<T, PackProblem> {
    Err(PackProblem(message.into()))
}

/// `path` made absolute and with `.`/`..` folded away, without touching the
/// filesystem (so a symbolic link stays what it is called).
fn normalize(path: &Path) -> PathBuf {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("/"))
            .join(path)
    };
    let mut out = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// The part of the walk that reads: who holds a reference and where from.
struct Holder<'a> {
    /// `doc.canvas.md`, as it will be called in the pack: for messages.
    canvas: &'a str,
    /// The node the text belongs to.
    node: &'a str,
    /// The directory references are resolved against, and may not leave.
    dir: &'a Path,
    access: &'a FileAccess,
    /// A reference from cached output: left out with a warning instead of
    /// failing the pack, since output is derived data the author cannot edit.
    soft: bool,
}

struct Walk {
    root: PathBuf,
    shallow: bool,
    limits: Limits,
    files: BTreeMap<String, PackFile>,
    canvases: Vec<PathBuf>,
    seen_canvases: HashSet<PathBuf>,
    warnings: Vec<String>,
}

/// Collects the files of the pack whose entry canvas is `entry`.
pub fn collect(entry: &Path, options: CollectOptions) -> Result<Collected, PackProblem> {
    let entry_path = normalize(entry);
    let Some(root) = entry_path.parent().map(Path::to_path_buf) else {
        return problem(format!("{}: not a file", entry.display()));
    };
    let bytes =
        std::fs::read(&entry_path).map_err(|e| PackProblem(format!("{}: {e}", entry.display())))?;
    let name = entry_path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| PackProblem(format!("{}: the name is not UTF-8", entry.display())))?
        .to_string();
    if !is_canvas_file(&name, &bytes) {
        return problem(format!(
            "{}: not a canvas (a `*.canvas.md`, or a `*.md` that opens with the canvas marker)",
            entry.display()
        ));
    }
    let mut walk = Walk {
        root,
        shallow: options.shallow,
        limits: Limits::default(),
        files: BTreeMap::new(),
        canvases: vec![entry_path.clone()],
        seen_canvases: HashSet::from([entry_path.clone()]),
        warnings: Vec::new(),
    };
    walk.add(
        name.clone(),
        PackFile {
            path: name.clone(),
            data: bytes,
            executable: is_executable(&entry_path),
        },
    );
    while let Some(canvas_path) = walk.canvases.pop() {
        walk.walk_canvas(&canvas_path)?;
    }
    Ok(Collected {
        entry: name,
        files: walk.files.into_values().collect(),
        warnings: walk.warnings,
    })
}

impl Walk {
    fn add(&mut self, rel: String, file: PackFile) {
        self.files.entry(rel).or_insert(file);
    }

    fn rel_of(&self, path: &Path) -> String {
        path.strip_prefix(&self.root)
            .map(|p| {
                p.components()
                    .map(|c| c.as_os_str().to_string_lossy().into_owned())
                    .collect::<Vec<_>>()
                    .join("/")
            })
            .unwrap_or_default()
    }

    fn walk_canvas(&mut self, canvas_path: &Path) -> Result<(), PackProblem> {
        let rel = self.rel_of(canvas_path);
        let text =
            std::fs::read_to_string(canvas_path).map_err(|e| PackProblem(format!("{rel}: {e}")))?;
        let canvas: Canvas = crate::mdcanvas::parse(&text)
            .map_err(|e| PackProblem(format!("{rel}: not a valid canvas: {e}")))?;
        let access = FileAccess::for_canvas_path(&canvas, canvas_path);
        let dir = canvas_path.parent().unwrap_or(Path::new("/")).to_path_buf();

        for node in &canvas.nodes {
            let holder = |soft| Holder {
                canvas: &rel,
                node: &node.id,
                dir: &dir,
                access: &access,
                soft,
            };
            match node.node_type {
                // A `link` node points at a URL, not a file.
                NodeType::Link => continue,
                NodeType::Include => {
                    if let Some(target) = &node.target {
                        let (path, _) = crate::mdcanvas::split_target_fragment(target);
                        if let Some(found) = self.reference(&holder(false), path)? {
                            self.scan_included(&rel, &node.id, &found, &access)?;
                        }
                    }
                }
                NodeType::File => {
                    if let Some(target) = &node.target {
                        let (path, _) = crate::mdcanvas::split_target_fragment(target);
                        self.reference(&holder(false), path)?;
                    }
                }
                _ => {}
            }
            for file_ref in markdown_file_refs(&node.text) {
                self.reference(&holder(file_ref.in_output), &file_ref.path)?;
            }
        }
        Ok(())
    }

    /// The Markdown an `include` pulls in is shown as part of the node, so
    /// its images and links are read too, resolved against its own directory.
    fn scan_included(
        &mut self,
        canvas: &str,
        node: &str,
        included: &Found,
        access: &FileAccess,
    ) -> Result<(), PackProblem> {
        let Ok(text) = std::str::from_utf8(&included.data) else {
            return Ok(());
        };
        let dir = included
            .lexical
            .parent()
            .unwrap_or(Path::new("/"))
            .to_path_buf();
        for file_ref in markdown_file_refs(text) {
            let holder = Holder {
                canvas,
                node,
                dir: &dir,
                access,
                soft: file_ref.in_output,
            };
            self.reference(&holder, &file_ref.path)?;
        }
        Ok(())
    }

    /// Resolves one reference, adds the file it names (and queues it if it is
    /// a canvas). `Ok(None)` means it was left out with a warning.
    fn reference(&mut self, holder: &Holder, target: &str) -> Result<Option<Found>, PackProblem> {
        match self.resolve(holder, target) {
            Ok(found) => Ok(found),
            Err(PackProblem(message)) if holder.soft => {
                self.warnings
                    .push(format!("{message} (in cached output, left out)"));
                Ok(None)
            }
            Err(error) => Err(error),
        }
    }

    fn resolve(&mut self, holder: &Holder, target: &str) -> Result<Option<Found>, PackProblem> {
        let at = format!("{}: node {:?}: {target:?}", holder.canvas, holder.node);
        if target.starts_with('/') {
            return problem(format!(
                "{at} is an absolute path; a pack holds only files below its canvas"
            ));
        }
        if target.starts_with('~') {
            return problem(format!(
                "{at} points into a home directory, which a pack cannot carry"
            ));
        }
        if target.starts_with("@tmp") || holder.access.expand(target) != target {
            return problem(format!(
                "{at} names a directory or file variable (or @tmp); a pack must not depend on the sender's environment"
            ));
        }
        let lexical = normalize(&holder.dir.join(target));
        if !lexical.starts_with(holder.dir) {
            let dir = self.rel_of(holder.dir);
            let dir = if dir.is_empty() {
                "the canvas's directory".to_string()
            } else {
                format!("{dir}/")
            };
            return problem(format!(
                "{at} leads outside {dir}; a reference may not leave the directory of the file that holds it"
            ));
        }
        let real = match holder.access.confine(holder.dir, target) {
            Ok(real) => real,
            Err(ConfineError::TargetNotFound(path, e)) => {
                return problem(format!("{at} does not exist ({}: {e})", path.display()));
            }
            Err(ConfineError::DirNotFound(path, e)) => {
                return problem(format!("{at} does not exist ({}: {e})", path.display()));
            }
            Err(ConfineError::Unconfirmed(name)) => {
                return problem(format!(
                    "{at} names the directory variable ${name}; a pack must not depend on the sender's environment"
                ));
            }
            Err(ConfineError::Outside(_)) => {
                return problem(format!(
                    "{at} leads outside what this canvas may read (a path outside its directory, or a symbolic link that goes there)"
                ));
            }
            Err(ConfineError::Config(e)) => return problem(format!("{at}: {e}")),
        };
        let floor_real = std::fs::canonicalize(holder.dir)
            .map_err(|e| PackProblem(format!("{}: {e}", holder.dir.display())))?;
        if !real.starts_with(&floor_real) {
            return problem(format!(
                "{at} is a symbolic link that leads outside its directory"
            ));
        }
        let metadata = std::fs::metadata(&real).map_err(|e| PackProblem(format!("{at}: {e}")))?;
        if !metadata.is_file() {
            return problem(format!(
                "{at} is a directory; a pack holds files, not directories"
            ));
        }
        if metadata.len() > self.limits.max_file_bytes {
            return problem(format!(
                "{at} is {} bytes, more than a pack may hold in one file ({})",
                metadata.len(),
                self.limits.max_file_bytes
            ));
        }
        let rel = self.rel_of(&lexical);
        check_path(&rel, &self.limits).map_err(|why| PackProblem(format!("{at}: {why}")))?;
        let data = std::fs::read(&real).map_err(|e| PackProblem(format!("{at}: {e}")))?;
        let found = Found {
            lexical: lexical.clone(),
            data: data.clone(),
        };
        if is_canvas_file(&rel, &data) {
            if self.shallow {
                self.warnings
                    .push(format!("{at} is a canvas and was left out (--shallow)"));
                return Ok(Some(found));
            }
            if self.seen_canvases.insert(lexical.clone()) {
                self.canvases.push(lexical.clone());
            }
        }
        self.add(
            rel.clone(),
            PackFile {
                path: rel,
                data,
                executable: is_executable(&real),
            },
        );
        Ok(Some(found))
    }
}

/// What a resolved reference turned out to be.
struct Found {
    lexical: PathBuf,
    data: Vec<u8>,
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|m| m.permissions().mode() & 0o100 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(_path: &Path) -> bool {
    false
}

/// Packs `collected` into the bytes of a `.canvas.tar.xz`. `created_by` names
/// the meshfox that writes it (informational, stored in the manifest).
pub fn pack_bytes(collected: &Collected, created_by: &str) -> Result<Vec<u8>, PackProblem> {
    let tar = build_tar(
        &collected.entry,
        created_by,
        &collected.files,
        &Limits::default(),
    )?;
    Ok(compress_xz(&tar)?)
}

/// Writes `bytes` to `out` so that a failure leaves no partial file: into a
/// sibling file first, then renamed over. An existing `out` is only replaced
/// with `force`.
pub fn write_file(out: &Path, bytes: &[u8], force: bool) -> Result<(), PackProblem> {
    if out.exists() && !force {
        return problem(format!(
            "{} already exists (use -f to replace it)",
            out.display()
        ));
    }
    let name = out
        .file_name()
        .ok_or_else(|| PackProblem(format!("{}: not a file name", out.display())))?
        .to_string_lossy()
        .into_owned();
    let partial = out.with_file_name(format!(".{name}.partial-{}", std::process::id()));
    let result = (|| {
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&partial)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        std::fs::rename(&partial, out)
    })();
    if let Err(e) = result {
        let _ = std::fs::remove_file(&partial);
        return problem(format!("{}: {e}", out.display()));
    }
    Ok(())
}

/// A pack's files, now on disk.
#[derive(Debug)]
pub struct Unpacked {
    pub dir: PathBuf,
    /// The entry canvas, inside `dir`.
    pub entry: PathBuf,
}

/// Checks `pack` (the bytes of a `.canvas.tar.xz`) completely, then puts its
/// files into `dir`, which must not exist or must be empty; nothing is
/// overwritten. A pack that fails a check writes nothing, and a write that
/// fails midway removes what it wrote.
pub fn unpack(pack: &[u8], dir: &Path) -> Result<Unpacked, PackProblem> {
    let parsed = read_xz(pack, &Limits::default())?;
    let existed = dir.exists();
    if existed {
        let empty = dir.is_dir()
            && std::fs::read_dir(dir)
                .map_err(|e| PackProblem(format!("{}: {e}", dir.display())))?
                .next()
                .is_none();
        if !empty {
            return problem(format!(
                "{} already exists and is not an empty directory",
                dir.display()
            ));
        }
    } else {
        std::fs::create_dir_all(dir).map_err(|e| PackProblem(format!("{}: {e}", dir.display())))?;
    }
    let mut written: Vec<PathBuf> = Vec::new();
    let result = (|| -> io::Result<()> {
        for file in &parsed.files {
            let target = dir.join(&file.path);
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }
            {
                use std::io::Write;
                let mut out = std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&target)?;
                written.push(target.clone());
                out.write_all(&file.data)?;
            }
            set_mode(&target, file.executable)?;
        }
        Ok(())
    })();
    if let Err(e) = result {
        if existed {
            for path in written.iter().rev() {
                let _ = std::fs::remove_file(path);
            }
            // The directories made on the way are empty again by now.
            if let Ok(entries) = std::fs::read_dir(dir) {
                for entry in entries.flatten() {
                    let _ = std::fs::remove_dir_all(entry.path());
                }
            }
        } else {
            let _ = std::fs::remove_dir_all(dir);
        }
        return problem(format!("{}: {e}", dir.display()));
    }
    Ok(Unpacked {
        dir: dir.to_path_buf(),
        entry: dir.join(&parsed.entry),
    })
}

#[cfg(unix)]
fn set_mode(path: &Path, executable: bool) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(
        path,
        std::fs::Permissions::from_mode(if executable { 0o755 } else { 0o644 }),
    )
}

#[cfg(not(unix))]
fn set_mode(_path: &Path, _executable: bool) -> io::Result<()> {
    Ok(())
}

/// A pack unpacked into a fresh private directory of its own, removed again
/// when this is dropped: what `meshfox view PACK` serves.
#[derive(Debug)]
pub struct TempUnpacked {
    pub unpacked: Unpacked,
}

impl Drop for TempUnpacked {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.unpacked.dir);
    }
}

/// [`unpack`] into a new directory under the system temporary directory
/// (readable by this user only).
pub fn unpack_to_temp(pack: &[u8]) -> Result<TempUnpacked, PackProblem> {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!("meshfox-pack-{}-{nanos}", std::process::id()));
    {
        let mut builder = std::fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder
            .create(&dir)
            .map_err(|e| PackProblem(format!("{}: {e}", dir.display())))?;
    }
    match unpack(pack, &dir) {
        Ok(unpacked) => Ok(TempUnpacked { unpacked }),
        Err(e) => {
            let _ = std::fs::remove_dir_all(&dir);
            Err(e)
        }
    }
}

/// The name `meshfox pack` gives its output by default: the canvas's file
/// name without `.canvas.md` (or `.md`), plus [`EXTENSION`].
pub fn default_pack_name(canvas: &Path) -> String {
    let name = canvas
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "canvas".to_string());
    let stem = name
        .strip_suffix(".canvas.md")
        .or_else(|| name.strip_suffix(".md"))
        .unwrap_or(&name);
    format!("{stem}{EXTENSION}")
}

/// The directory `meshfox unpack` fills by default: the pack's file name
/// without [`EXTENSION`].
pub fn default_unpack_dir(pack: &Path) -> PathBuf {
    let name = pack
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "pack".to_string());
    let stem = name.strip_suffix(EXTENSION).unwrap_or(&name);
    let stem = if stem.is_empty() || stem == name {
        format!("{name}.unpacked")
    } else {
        stem.to_string()
    };
    PathBuf::from(stem)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    fn temp() -> PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("mfx-pack-core-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.canonicalize().unwrap()
    }

    fn write(dir: &Path, rel: &str, text: &str) {
        let path = dir.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    /// A canvas whose root body is `body` and which has the given extra
    /// `(heading, node attributes, body)` nodes.
    fn canvas(body: &str, nodes: &[(&str, &str, &str)]) -> String {
        let mut text = format!(
            "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n\n{body}\n"
        );
        for (id, attrs, node_body) in nodes {
            text.push_str(&format!(
                "\n## {id}\n<!-- meshfox:node id=\"{id}\" {attrs} -->\n\n{node_body}\n"
            ));
        }
        text
    }

    fn paths(collected: &Collected) -> Vec<&str> {
        collected.files.iter().map(|f| f.path.as_str()).collect()
    }

    fn fail(dir: &Path, entry: &str) -> String {
        collect(&dir.join(entry), CollectOptions::default())
            .expect_err("the pack should be refused")
            .to_string()
    }

    #[test]
    fn it_follows_includes_images_file_nodes_and_nested_canvases() {
        let dir = temp();
        write(
            &dir,
            "doc.canvas.md",
            &canvas(
                "![pic](img/a.png) and [spec](docs/spec.pdf) and [web](https://example.com)",
                &[
                    ("inc", "type=\"include\"", "[notes](./docs/notes.md)"),
                    ("data", "type=\"file\"", "[data](./data/table.csv)"),
                    ("child", "type=\"file\"", "[child](./sub/child.canvas.md)"),
                ],
            ),
        );
        write(&dir, "img/a.png", "png");
        write(&dir, "docs/spec.pdf", "pdf");
        write(
            &dir,
            "docs/notes.md",
            "# Notes\n\n![inc image](fig/f.png)\n",
        );
        write(&dir, "docs/fig/f.png", "fig");
        write(&dir, "data/table.csv", "a,b\n1,2\n");
        write(
            &dir,
            "sub/child.canvas.md",
            &canvas("![deep](deep.png)", &[]),
        );
        write(&dir, "sub/deep.png", "deep");
        write(&dir, "unrelated.txt", "never referenced");
        write(&dir, ".meshfox/state.db", "state");

        let collected = collect(&dir.join("doc.canvas.md"), CollectOptions::default()).unwrap();
        assert_eq!(collected.entry, "doc.canvas.md");
        assert_eq!(
            paths(&collected),
            [
                "data/table.csv",
                "doc.canvas.md",
                "docs/fig/f.png",
                "docs/notes.md",
                "docs/spec.pdf",
                "img/a.png",
                "sub/child.canvas.md",
                "sub/deep.png",
            ]
        );
        assert!(collected.warnings.is_empty(), "{:?}", collected.warnings);
    }

    #[test]
    fn a_pack_unpacks_to_files_that_read_the_same() {
        let dir = temp();
        write(
            &dir,
            "doc.canvas.md",
            &canvas(
                "![pic](img/a.png)",
                &[("inc", "type=\"include\"", "[n](./n.md)")],
            ),
        );
        write(&dir, "img/a.png", "png bytes");
        write(&dir, "n.md", "# N\n");
        let first = collect(&dir.join("doc.canvas.md"), CollectOptions::default()).unwrap();
        let bytes = pack_bytes(&first, "meshfox test").unwrap();
        // The same files give the same bytes.
        let second = collect(&dir.join("doc.canvas.md"), CollectOptions::default()).unwrap();
        assert_eq!(bytes, pack_bytes(&second, "meshfox test").unwrap());

        let out = temp().join("out");
        let unpacked = unpack(&bytes, &out).unwrap();
        assert_eq!(unpacked.entry, out.join("doc.canvas.md"));
        for file in &first.files {
            assert_eq!(std::fs::read(out.join(&file.path)).unwrap(), file.data);
        }
        // The unpacked canvas packs to the same pack again.
        let again = collect(&unpacked.entry, CollectOptions::default()).unwrap();
        assert_eq!(bytes, pack_bytes(&again, "meshfox test").unwrap());
    }

    #[test]
    fn shallow_leaves_nested_canvases_and_what_only_they_use_out() {
        let dir = temp();
        write(
            &dir,
            "doc.canvas.md",
            &canvas(
                "",
                &[("child", "type=\"file\"", "[child](./child.canvas.md)")],
            ),
        );
        write(
            &dir,
            "child.canvas.md",
            &canvas("![x](only-child.png)", &[]),
        );
        write(&dir, "only-child.png", "x");
        let collected =
            collect(&dir.join("doc.canvas.md"), CollectOptions { shallow: true }).unwrap();
        assert_eq!(paths(&collected), ["doc.canvas.md"]);
        assert!(collected.warnings.iter().any(|w| w.contains("--shallow")));
    }

    #[test]
    fn a_canvas_reached_twice_or_in_a_cycle_is_packed_once() {
        let dir = temp();
        write(
            &dir,
            "a.canvas.md",
            &canvas("", &[("b", "type=\"file\"", "[b](./b.canvas.md)")]),
        );
        write(
            &dir,
            "b.canvas.md",
            &canvas("", &[("a", "type=\"file\"", "[a](./a.canvas.md)")]),
        );
        let collected = collect(&dir.join("a.canvas.md"), CollectOptions::default()).unwrap();
        assert_eq!(paths(&collected), ["a.canvas.md", "b.canvas.md"]);
    }

    #[test]
    fn a_reference_that_leaves_its_directory_fails_the_pack_and_says_where() {
        let dir = temp();
        write(&dir, "outside.png", "x");
        write(
            &dir.join("main"),
            "doc.canvas.md",
            &canvas("![up](../outside.png)", &[]),
        );
        let message = fail(&dir.join("main"), "doc.canvas.md");
        assert!(message.contains("doc.canvas.md"), "{message}");
        assert!(message.contains("\"root\""), "{message}");
        assert!(message.contains("../outside.png"), "{message}");
        assert!(message.contains("leads outside"), "{message}");
    }

    #[test]
    fn a_nested_canvas_may_not_reach_a_sibling_directory() {
        let dir = temp();
        write(
            &dir,
            "doc.canvas.md",
            &canvas(
                "",
                &[("child", "type=\"file\"", "[child](./sub/child.canvas.md)")],
            ),
        );
        write(
            &dir,
            "sub/child.canvas.md",
            &canvas("![x](../shared/x.png)", &[]),
        );
        write(&dir, "shared/x.png", "x");
        let message = fail(&dir, "doc.canvas.md");
        assert!(message.contains("sub/child.canvas.md"), "{message}");
        assert!(message.contains("../shared/x.png"), "{message}");
    }

    #[test]
    fn included_markdown_may_not_climb_out_of_its_own_directory() {
        let dir = temp();
        write(
            &dir,
            "doc.canvas.md",
            &canvas("", &[("inc", "type=\"include\"", "[n](./docs/n.md)")]),
        );
        write(&dir, "docs/n.md", "![x](../top.png)\n");
        write(&dir, "top.png", "x");
        assert!(fail(&dir, "doc.canvas.md").contains("../top.png"));
    }

    #[test]
    fn absolute_home_missing_directory_and_variable_targets_are_refused() {
        let dir = temp();
        write(&dir, "dir/inner.txt", "x");
        for (body, expect) in [
            ("![a](/etc/hostname)", "absolute"),
            ("![h](~/secret.png)", "home"),
            ("![m](nope.png)", "does not exist"),
            ("[d](dir)", "directory"),
            ("![t](@tmp/plot.png)", "environment"),
        ] {
            write(&dir, "doc.canvas.md", &canvas(body, &[]));
            let message = fail(&dir, "doc.canvas.md");
            assert!(message.contains(expect), "{body}: {message}");
        }
        let with_var = "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n\
             <!-- meshfox:var name=\"WORK\" type=\"dir\" default=\"@tmp/work\" -->\n\n[x]($WORK/x.csv)\n".to_string();
        write(&dir, "doc.canvas.md", &with_var);
        assert!(fail(&dir, "doc.canvas.md").contains("environment"));
    }

    #[test]
    fn the_state_directory_is_never_packed() {
        let dir = temp();
        write(&dir, ".meshfox/notes.md", "state");
        write(
            &dir,
            "doc.canvas.md",
            &canvas("[s](.meshfox/notes.md)", &[]),
        );
        assert!(fail(&dir, "doc.canvas.md").contains(".meshfox"));
    }

    #[cfg(unix)]
    #[test]
    fn a_symbolic_link_out_of_the_directory_is_refused_one_inside_is_not() {
        let dir = temp();
        let elsewhere = temp();
        write(&elsewhere, "secret.png", "secret");
        write(&dir, "real.png", "real");
        std::os::unix::fs::symlink(elsewhere.join("secret.png"), dir.join("out.png")).unwrap();
        std::os::unix::fs::symlink(dir.join("real.png"), dir.join("in.png")).unwrap();

        write(&dir, "doc.canvas.md", &canvas("![x](out.png)", &[]));
        assert!(fail(&dir, "doc.canvas.md").contains("symbolic link"));

        write(&dir, "doc.canvas.md", &canvas("![x](in.png)", &[]));
        let collected = collect(&dir.join("doc.canvas.md"), CollectOptions::default()).unwrap();
        // Called what the canvas calls it, holding the real file's bytes.
        let linked = collected.files.iter().find(|f| f.path == "in.png").unwrap();
        assert_eq!(linked.data, b"real");
    }

    #[test]
    fn a_broken_reference_in_cached_output_is_a_warning_not_a_failure() {
        let dir = temp();
        let body = concat!(
            "```bash name=\"go\"\necho hi\n```\n",
            "<!-- meshfox:output name=\"go\" -->\n![made](@tmp/made.png)\n<!-- /meshfox:output -->\n",
        );
        write(&dir, "doc.canvas.md", &canvas(body, &[]));
        let collected = collect(&dir.join("doc.canvas.md"), CollectOptions::default()).unwrap();
        assert_eq!(paths(&collected), ["doc.canvas.md"]);
        assert!(
            collected
                .warnings
                .iter()
                .any(|w| w.contains("cached output")),
            "{:?}",
            collected.warnings
        );
    }

    #[test]
    fn the_entry_must_be_a_canvas() {
        let dir = temp();
        write(&dir, "plain.md", "# just markdown\n");
        assert!(fail(&dir, "plain.md").contains("not a canvas"));
        assert!(fail(&dir, "missing.canvas.md").contains("missing.canvas.md"));
    }

    #[test]
    fn unpack_refuses_a_directory_that_has_something_in_it() {
        let dir = temp();
        write(&dir, "doc.canvas.md", &canvas("", &[]));
        let bytes = pack_bytes(
            &collect(&dir.join("doc.canvas.md"), CollectOptions::default()).unwrap(),
            "t",
        )
        .unwrap();
        let target = temp();
        write(&target, "mine.txt", "keep me");
        assert!(unpack(&bytes, &target).is_err());
        assert_eq!(
            std::fs::read_to_string(target.join("mine.txt")).unwrap(),
            "keep me"
        );
        // An empty directory is fine, and a second unpack into it is not.
        let empty = temp();
        unpack(&bytes, &empty).unwrap();
        assert!(unpack(&bytes, &empty).is_err());
    }

    #[test]
    fn a_pack_that_fails_its_checks_writes_nothing() {
        let out = temp().join("never");
        assert!(unpack(b"not a pack at all", &out).is_err());
        assert!(!out.exists());
    }

    #[cfg(unix)]
    #[test]
    fn the_executable_bit_travels() {
        use std::os::unix::fs::PermissionsExt;
        let dir = temp();
        write(
            &dir,
            "doc.canvas.md",
            &canvas("", &[("run", "type=\"file\"", "[go](./go.sh)")]),
        );
        write(&dir, "go.sh", "#!/bin/sh\necho hi\n");
        std::fs::set_permissions(dir.join("go.sh"), std::fs::Permissions::from_mode(0o755))
            .unwrap();
        let bytes = pack_bytes(
            &collect(&dir.join("doc.canvas.md"), CollectOptions::default()).unwrap(),
            "t",
        )
        .unwrap();
        let out = temp().join("out");
        unpack(&bytes, &out).unwrap();
        let mode = |p: &str| std::fs::metadata(out.join(p)).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode("go.sh"), 0o755);
        assert_eq!(mode("doc.canvas.md"), 0o644);
    }

    #[test]
    fn write_file_leaves_no_partial_file_and_respects_force() {
        let dir = temp();
        let out = dir.join("x.canvas.tar.xz");
        write_file(&out, b"one", false).unwrap();
        assert!(write_file(&out, b"two", false).is_err());
        assert_eq!(std::fs::read(&out).unwrap(), b"one");
        write_file(&out, b"two", true).unwrap();
        assert_eq!(std::fs::read(&out).unwrap(), b"two");
        let names: Vec<_> = std::fs::read_dir(&dir).unwrap().flatten().collect();
        assert_eq!(names.len(), 1, "{names:?}");
    }

    #[test]
    fn the_unpacked_temp_directory_goes_away_with_its_owner() {
        let dir = temp();
        write(&dir, "doc.canvas.md", &canvas("", &[]));
        let bytes = pack_bytes(
            &collect(&dir.join("doc.canvas.md"), CollectOptions::default()).unwrap(),
            "t",
        )
        .unwrap();
        let temp_dir = unpack_to_temp(&bytes).unwrap();
        let path = temp_dir.unpacked.dir.clone();
        assert!(path.join("doc.canvas.md").exists());
        drop(temp_dir);
        assert!(!path.exists());
    }

    #[test]
    fn default_names() {
        assert_eq!(
            default_pack_name(Path::new("a/doc.canvas.md")),
            "doc.canvas.tar.xz"
        );
        assert_eq!(
            default_pack_name(Path::new("README.md")),
            "README.canvas.tar.xz"
        );
        assert_eq!(
            default_unpack_dir(Path::new("x/doc.canvas.tar.xz")),
            PathBuf::from("doc")
        );
        assert_eq!(
            default_unpack_dir(Path::new("odd.bin")),
            PathBuf::from("odd.bin.unpacked")
        );
    }
}
