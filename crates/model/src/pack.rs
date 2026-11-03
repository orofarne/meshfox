//! Packed canvases (`.canvas.tar.xz`, SPEC.md's "Packed canvases"): reading
//! and writing the archive itself, with no filesystem or operating system
//! behind it, so the command line and the browser editor share one reader and
//! one set of rules.
//!
//! A pack is a tar archive compressed as one xz stream. Its first entry is the
//! manifest ([`MANIFEST_NAME`]): the format version, the entry canvas and every
//! file's size and BLAKE3 hash. [`read_xz`] turns the bytes into a verified
//! [`Pack`] or refuses the whole file, naming the first problem; it never
//! trusts a size the archive claims, only the bytes actually produced.
//! [`build_tar`] writes the uncompressed tar deterministically; compressing it
//! (which needs a real LZMA encoder, not part of this crate) is the caller's
//! job.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, Read, Write};

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// The manifest's name: the first entry of every pack, reserved.
pub const MANIFEST_NAME: &str = ".meshfox-pack.json";

/// The format version this module reads and writes.
pub const FORMAT_VERSION: u32 = 1;

/// What every pack's name ends with.
pub const EXTENSION: &str = ".canvas.tar.xz";

/// The first bytes of an xz stream.
pub const XZ_MAGIC: [u8; 6] = [0xFD, b'7', b'z', b'X', b'Z', 0x00];

/// The directory of a canvas's own state; never part of a pack.
const STATE_DIR: &str = ".meshfox";

/// Bytes a tar entry can add around its data: a header, a long-name entry for
/// a path of up to 1024 bytes, and the padding of both. Used to size the cap
/// on decompressed bytes, which has to allow for the tar framing too.
const TAR_OVERHEAD_PER_ENTRY: u64 = 4096;

/// What a reader refuses to go beyond. Part of the format version, and the
/// same for the command line and the browser (SPEC.md, "Reading and limits").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Entries of the archive, the manifest included.
    pub max_entries: usize,
    /// One file's size.
    pub max_file_bytes: u64,
    /// All files' sizes together.
    pub max_total_bytes: u64,
    /// The manifest's size.
    pub max_manifest_bytes: u64,
    /// A path's length in bytes.
    pub max_path_bytes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            max_entries: 20_000,
            max_file_bytes: 256 * 1024 * 1024,
            max_total_bytes: 512 * 1024 * 1024,
            max_manifest_bytes: 8 * 1024 * 1024,
            max_path_bytes: 1024,
        }
    }
}

impl Limits {
    /// The most bytes an xz stream may unpack to before it is cut off: the
    /// files, the manifest and the tar framing around them.
    fn max_unpacked_bytes(&self) -> u64 {
        self.max_total_bytes
            .saturating_add(self.max_manifest_bytes)
            .saturating_add((self.max_entries as u64 + 2).saturating_mul(TAR_OVERHEAD_PER_ENTRY))
            .saturating_add(1024)
    }
}

/// Why a pack was refused (or could not be written).
#[derive(Debug, Error, PartialEq, Eq)]
pub enum PackError {
    #[error("not a packed canvas: it does not start like an xz file")]
    NotXz,
    #[error("the xz data is damaged or cut off: {0}")]
    Xz(String),
    #[error("the tar data is damaged: {0}")]
    Tar(String),
    #[error("{0}")]
    TooLarge(String),
    #[error("{path:?}: unsupported entry type ({kind}); a pack holds only files and directories")]
    BadEntryType { path: String, kind: String },
    #[error("{path:?}: not allowed in a pack: {why}")]
    BadPath { path: String, why: &'static str },
    #[error("{a:?} and {b:?} clash: {why}")]
    Conflict {
        a: String,
        b: String,
        why: &'static str,
    },
    #[error("bad manifest: {0}")]
    Manifest(String),
    #[error("this pack is format version {0}; this meshfox reads version {FORMAT_VERSION}")]
    UnsupportedFormat(u64),
    #[error("{0:?} is in the archive but not in the manifest")]
    NotListed(String),
    #[error("{0:?} is in the manifest but not in the archive")]
    Missing(String),
    #[error("{0:?} appears twice")]
    Duplicate(String),
    #[error("{path:?} is {found} bytes, the manifest says {listed}")]
    SizeMismatch {
        path: String,
        found: u64,
        listed: u64,
    },
    #[error("{0:?} does not match the hash in the manifest")]
    HashMismatch(String),
    #[error("entry canvas {0:?}: {1}")]
    BadEntry(String, &'static str),
}

/// The manifest, as stored in [`MANIFEST_NAME`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    pub format: u32,
    /// Path of the canvas to open.
    pub entry: String,
    /// Which meshfox wrote the pack (informational).
    #[serde(default)]
    pub created_by: String,
    /// Every file but the manifest, sorted by path bytewise.
    pub files: Vec<ManifestFile>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestFile {
    pub path: String,
    pub size: u64,
    /// 64 hex digits.
    pub blake3: String,
}

/// One file of a pack.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackFile {
    /// Relative, `/`-separated.
    pub path: String,
    pub data: Vec<u8>,
    /// The source was executable by its owner (`0755` instead of `0644`).
    pub executable: bool,
}

/// A pack that passed every check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pack {
    pub entry: String,
    pub created_by: String,
    /// Sorted by path bytewise.
    pub files: Vec<PackFile>,
}

impl Pack {
    /// The bytes of the file at `path`, if the pack has it.
    pub fn file(&self, path: &str) -> Option<&PackFile> {
        self.files
            .binary_search_by(|f| f.path.as_str().cmp(path))
            .ok()
            .map(|i| &self.files[i])
    }
}

/// Whether `bytes` open like an xz stream (what tells a pack from anything
/// else, whatever the file is called).
pub fn is_xz(bytes: &[u8]) -> bool {
    bytes.starts_with(&XZ_MAGIC)
}

/// Why `path` cannot be a pack path, if it cannot: the rules of SPEC.md's
/// "Layout". Case and prefix clashes between several paths are
/// [`check_paths`]'s business.
pub fn check_path(path: &str, limits: &Limits) -> Result<(), &'static str> {
    if path.is_empty() {
        return Err("the path is empty");
    }
    if path.len() > limits.max_path_bytes {
        return Err("the path is too long");
    }
    if path.starts_with('/') {
        return Err("the path is absolute");
    }
    if path.contains('\\') {
        return Err("the path has a backslash");
    }
    if path.chars().any(|c| c < ' ' || c == '\u{7f}') {
        return Err("the path has a control character");
    }
    for (i, part) in path.split('/').enumerate() {
        match part {
            "" => return Err("the path has an empty component"),
            "." | ".." => return Err("the path has a `.` or `..` component"),
            _ => {}
        }
        if i == 0 {
            let bytes = part.as_bytes();
            if bytes.len() == 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
                return Err("the path starts with a drive letter");
            }
            if part == STATE_DIR {
                return Err("`.meshfox/` is a canvas's own state and is never packed");
            }
        }
    }
    if path == MANIFEST_NAME {
        return Err("that name belongs to the manifest");
    }
    Ok(())
}

/// Checks a set of file paths against each other: no path twice, none that
/// differs from another only in letter case (so a pack unpacks the same way on
/// a case-insensitive filesystem), none that is a file and also the directory
/// another path lives in. `dirs` are directory paths from the archive, which a
/// file may not share a name with either.
fn check_paths<'a>(
    files: impl Iterator<Item = &'a str>,
    dirs: &BTreeSet<String>,
) -> Result<(), PackError> {
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    let mut lower: BTreeMap<String, &str> = BTreeMap::new();
    let files: Vec<&str> = files.collect();
    for &path in &files {
        if !seen.insert(path) {
            return Err(PackError::Duplicate(path.to_string()));
        }
        if let Some(other) = lower.insert(path.to_lowercase(), path) {
            return Err(PackError::Conflict {
                a: other.to_string(),
                b: path.to_string(),
                why: "they differ only in letter case",
            });
        }
        if dirs.contains(path) {
            return Err(PackError::Conflict {
                a: path.to_string(),
                b: path.to_string(),
                why: "it is both a file and a directory",
            });
        }
    }
    for &path in &files {
        let mut prefix_end = 0;
        while let Some(i) = path[prefix_end..].find('/') {
            prefix_end += i;
            let ancestor = &path[..prefix_end];
            if seen.contains(ancestor) {
                return Err(PackError::Conflict {
                    a: ancestor.to_string(),
                    b: path.to_string(),
                    why: "a file cannot also be a directory",
                });
            }
            prefix_end += 1;
        }
    }
    Ok(())
}

/// Whether `contents` at `path` is a canvas: named `*.canvas.md`, or a `*.md`
/// that opens with the canvas marker.
pub fn is_canvas_file(path: &str, contents: &[u8]) -> bool {
    if path.ends_with(".canvas.md") {
        return true;
    }
    path.ends_with(".md")
        && std::str::from_utf8(contents)
            .map(crate::mdcanvas::has_marker)
            .unwrap_or(false)
}

/// A reference from Markdown text to a local file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileRef {
    /// The path as written, with URL escapes decoded and any `?query` or
    /// `#fragment` cut off.
    pub path: String,
    /// It sits inside a cached output region, so it is derived data rather
    /// than something its author wrote.
    pub in_output: bool,
}

/// Every local file a Markdown text points at, by an image or a link:
/// what `pack` has to carry along. Remote URLs (anything with a scheme),
/// `#anchors` and protocol-relative `//` links are not files, and code
/// fences hold no links. Absolute paths are returned like any other, for the
/// caller to refuse.
pub fn markdown_file_refs(markdown: &str) -> Vec<FileRef> {
    use pulldown_cmark::{Event, Parser, Tag};
    let outputs = crate::output::output_byte_ranges(markdown);
    let mut refs = Vec::new();
    for (event, range) in Parser::new(markdown).into_offset_iter() {
        let url = match event {
            Event::Start(Tag::Image { dest_url, .. })
            | Event::Start(Tag::Link { dest_url, .. }) => dest_url,
            _ => continue,
        };
        let url = url.as_ref();
        if url.is_empty() || url.starts_with('#') || url.starts_with("//") || url.contains(':') {
            continue;
        }
        let path =
            crate::image_attrs::decode_image_path(url.split(['?', '#']).next().unwrap_or(url));
        if path.is_empty() {
            continue;
        }
        let in_output = crate::fence::in_output_region(&outputs, range.start);
        refs.push(FileRef { path, in_output });
    }
    refs
}

fn blake3_hex(data: &[u8]) -> String {
    blake3::hash(data).to_hex().to_string()
}

fn tar_err(e: io::Error) -> PackError {
    PackError::Tar(e.to_string())
}

/// What one xz block may ask the decoder to allocate (its dictionary and
/// working memory), in KiB. `xz -9` needs about 65 MiB; a file asking for far
/// more is either hostile or not meant for a browser.
const MAX_DECODER_MEMORY_KIB: u32 = 192 * 1024;

/// Reads a pack from `xz` bytes: decompresses within the limits, then
/// [`read_tar`].
pub fn read_xz(xz: &[u8], limits: &Limits) -> Result<Pack, PackError> {
    if !is_xz(xz) {
        return Err(PackError::NotXz);
    }
    let mut input = io::Cursor::new(xz);
    let cap = limits.max_unpacked_bytes();
    let mut unpacked: Vec<u8> = Vec::new();
    {
        let mut decoder =
            lzma_rust2::XzReader::new_mem_limit(&mut input, false, MAX_DECODER_MEMORY_KIB);
        let mut chunk = vec![0u8; 64 * 1024];
        loop {
            let n = decoder
                .read(&mut chunk)
                .map_err(|e| PackError::Xz(e.to_string()))?;
            if n == 0 {
                break;
            }
            if unpacked.len() as u64 + n as u64 > cap {
                return Err(PackError::TooLarge(format!(
                    "the pack unpacks to more than the limit ({} bytes of files)",
                    limits.max_total_bytes
                )));
            }
            unpacked.extend_from_slice(&chunk[..n]);
        }
    }
    // One stream, then at most the zero padding xz allows.
    let rest = &xz[input.position() as usize..];
    if rest.iter().any(|&b| b != 0) {
        return Err(PackError::Xz(
            "there is data after the end of the xz stream".to_string(),
        ));
    }
    read_tar(&unpacked, limits)
}

/// Compresses `tar` (from [`build_tar`]) into the xz stream a pack is stored
/// as: one stream, one block, the same bytes for the same input. The encoder is
/// pure Rust, so the command line and the browser editor write packs with the
/// same code.
pub fn compress_xz(tar: &[u8]) -> Result<Vec<u8>, PackError> {
    let options = lzma_rust2::XzOptions::with_preset(6);
    let mut writer =
        lzma_rust2::XzWriter::new(Vec::new(), options).map_err(|e| PackError::Xz(e.to_string()))?;
    writer
        .write_all(tar)
        .map_err(|e| PackError::Xz(e.to_string()))?;
    writer.finish().map_err(|e| PackError::Xz(e.to_string()))
}

/// Reads and verifies a pack from uncompressed tar bytes.
pub fn read_tar(tar: &[u8], limits: &Limits) -> Result<Pack, PackError> {
    let mut archive = tar::Archive::new(tar);
    let mut manifest: Option<Manifest> = None;
    let mut listed: BTreeMap<String, (u64, String)> = BTreeMap::new();
    let mut files: Vec<PackFile> = Vec::new();
    let mut dirs: BTreeSet<String> = BTreeSet::new();
    let mut total: u64 = 0;
    let mut count: usize = 0;

    for (index, entry) in archive.entries().map_err(tar_err)?.enumerate() {
        let mut entry = entry.map_err(tar_err)?;
        count += 1;
        if count > limits.max_entries {
            return Err(PackError::TooLarge(format!(
                "the pack has more than {} entries",
                limits.max_entries
            )));
        }
        let path =
            String::from_utf8(entry.path_bytes().into_owned()).map_err(|e| PackError::BadPath {
                path: String::from_utf8_lossy(e.as_bytes()).into_owned(),
                why: "the path is not UTF-8",
            })?;
        let kind = entry.header().entry_type();
        let is_dir = kind.is_dir();
        if !(kind.is_file() || is_dir) {
            return Err(PackError::BadEntryType {
                path,
                kind: format!("{kind:?}"),
            });
        }
        let size = entry.size();

        if index == 0 {
            if is_dir || path != MANIFEST_NAME {
                return Err(PackError::Manifest(format!(
                    "the first entry must be {MANIFEST_NAME}, not {path:?}"
                )));
            }
            if size > limits.max_manifest_bytes {
                return Err(PackError::TooLarge(format!(
                    "the manifest is larger than {} bytes",
                    limits.max_manifest_bytes
                )));
            }
            let data = read_entry(&mut entry, size)?;
            let parsed = parse_manifest(&data, limits)?;
            for file in &parsed.files {
                listed.insert(file.path.clone(), (file.size, file.blake3.clone()));
            }
            manifest = Some(parsed);
            continue;
        }

        let path = if is_dir {
            path.strip_suffix('/').unwrap_or(&path).to_string()
        } else {
            path
        };
        check_path(&path, limits).map_err(|why| PackError::BadPath {
            path: path.clone(),
            why,
        })?;
        if is_dir {
            dirs.insert(path);
            continue;
        }
        if size > limits.max_file_bytes {
            return Err(PackError::TooLarge(format!(
                "{path:?} is larger than {} bytes",
                limits.max_file_bytes
            )));
        }
        total = total.saturating_add(size);
        if total > limits.max_total_bytes {
            return Err(PackError::TooLarge(format!(
                "the files add up to more than {} bytes",
                limits.max_total_bytes
            )));
        }
        let Some((listed_size, listed_hash)) = listed.get(&path) else {
            return Err(PackError::NotListed(path));
        };
        if *listed_size != size {
            return Err(PackError::SizeMismatch {
                path,
                found: size,
                listed: *listed_size,
            });
        }
        let executable = entry
            .header()
            .mode()
            .map(|m| m & 0o100 != 0)
            .unwrap_or(false);
        let data = read_entry(&mut entry, size)?;
        if blake3_hex(&data) != *listed_hash {
            return Err(PackError::HashMismatch(path));
        }
        files.push(PackFile {
            path,
            data,
            executable,
        });
    }

    let manifest = manifest.ok_or_else(|| PackError::Manifest("the archive is empty".into()))?;
    check_paths(files.iter().map(|f| f.path.as_str()), &dirs)?;
    let present: BTreeSet<&str> = files.iter().map(|f| f.path.as_str()).collect();
    if let Some(missing) = manifest
        .files
        .iter()
        .find(|f| !present.contains(f.path.as_str()))
    {
        return Err(PackError::Missing(missing.path.clone()));
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));

    let entry_file = files
        .binary_search_by(|f| f.path.as_str().cmp(&manifest.entry))
        .ok()
        .map(|i| &files[i])
        .ok_or_else(|| {
            PackError::BadEntry(manifest.entry.clone(), "it is not a file of the pack")
        })?;
    if !is_canvas_file(&entry_file.path, &entry_file.data) {
        return Err(PackError::BadEntry(
            manifest.entry.clone(),
            "it is not a canvas",
        ));
    }
    Ok(Pack {
        entry: manifest.entry,
        created_by: manifest.created_by,
        files,
    })
}

/// The `size` bytes of an entry; a tar cut short is an error, not a short file.
fn read_entry(entry: &mut impl Read, size: u64) -> Result<Vec<u8>, PackError> {
    let mut data = Vec::with_capacity(size.min(1 << 20) as usize);
    entry
        .by_ref()
        .take(size)
        .read_to_end(&mut data)
        .map_err(tar_err)?;
    if data.len() as u64 != size {
        return Err(PackError::Tar("the archive ends inside a file".to_string()));
    }
    Ok(data)
}

/// Parses and checks the manifest on its own: the version first (so a newer
/// format is reported as that, not as a field this reader lacks), then the
/// fields, then the list of files.
fn parse_manifest(data: &[u8], limits: &Limits) -> Result<Manifest, PackError> {
    let value: serde_json::Value =
        serde_json::from_slice(data).map_err(|e| PackError::Manifest(e.to_string()))?;
    match value.get("format").and_then(|v| v.as_u64()) {
        Some(n) if n == u64::from(FORMAT_VERSION) => {}
        Some(n) => return Err(PackError::UnsupportedFormat(n)),
        None => return Err(PackError::Manifest("`format` is missing".to_string())),
    }
    let manifest: Manifest =
        serde_json::from_value(value).map_err(|e| PackError::Manifest(e.to_string()))?;
    check_path(&manifest.entry, limits)
        .map_err(|why| PackError::BadEntry(manifest.entry.clone(), why))?;
    let mut previous: Option<&str> = None;
    for file in &manifest.files {
        check_path(&file.path, limits).map_err(|why| PackError::BadPath {
            path: file.path.clone(),
            why,
        })?;
        if file.blake3.len() != 64 || !file.blake3.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(PackError::Manifest(format!(
                "{:?}: `blake3` is not 64 hex digits",
                file.path
            )));
        }
        if previous.is_some_and(|p| p >= file.path.as_str()) {
            return Err(PackError::Manifest(format!(
                "the files are not sorted by path (or {:?} is listed twice)",
                file.path
            )));
        }
        previous = Some(&file.path);
    }
    check_paths(
        manifest.files.iter().map(|f| f.path.as_str()),
        &BTreeSet::new(),
    )?;
    Ok(manifest)
}

/// Writes the uncompressed tar of a pack: the manifest first, then the files
/// sorted by path, with no time, owner or group, so the same input gives the
/// same bytes. Refuses what a reader would refuse (bad or clashing paths, a
/// limit, an entry that is not a listed canvas).
pub fn build_tar(
    entry: &str,
    created_by: &str,
    files: &[PackFile],
    limits: &Limits,
) -> Result<Vec<u8>, PackError> {
    if files.len() + 1 > limits.max_entries {
        return Err(PackError::TooLarge(format!(
            "the pack would have more than {} entries",
            limits.max_entries
        )));
    }
    let mut sorted: Vec<&PackFile> = files.iter().collect();
    sorted.sort_by(|a, b| a.path.cmp(&b.path));
    let mut total: u64 = 0;
    for file in &sorted {
        check_path(&file.path, limits).map_err(|why| PackError::BadPath {
            path: file.path.clone(),
            why,
        })?;
        let size = file.data.len() as u64;
        if size > limits.max_file_bytes {
            return Err(PackError::TooLarge(format!(
                "{:?} is larger than {} bytes",
                file.path, limits.max_file_bytes
            )));
        }
        total = total.saturating_add(size);
    }
    if total > limits.max_total_bytes {
        return Err(PackError::TooLarge(format!(
            "the files add up to more than {} bytes",
            limits.max_total_bytes
        )));
    }
    check_paths(sorted.iter().map(|f| f.path.as_str()), &BTreeSet::new())?;
    let entry_file = sorted
        .iter()
        .find(|f| f.path == entry)
        .ok_or_else(|| PackError::BadEntry(entry.to_string(), "it is not among the files"))?;
    if !is_canvas_file(&entry_file.path, &entry_file.data) {
        return Err(PackError::BadEntry(entry.to_string(), "it is not a canvas"));
    }

    let manifest = Manifest {
        format: FORMAT_VERSION,
        entry: entry.to_string(),
        created_by: created_by.to_string(),
        files: sorted
            .iter()
            .map(|f| ManifestFile {
                path: f.path.clone(),
                size: f.data.len() as u64,
                blake3: blake3_hex(&f.data),
            })
            .collect(),
    };
    let mut manifest_bytes =
        serde_json::to_vec_pretty(&manifest).map_err(|e| PackError::Manifest(e.to_string()))?;
    manifest_bytes.push(b'\n');
    if manifest_bytes.len() as u64 > limits.max_manifest_bytes {
        return Err(PackError::TooLarge(format!(
            "the manifest would be larger than {} bytes",
            limits.max_manifest_bytes
        )));
    }

    let mut builder = tar::Builder::new(Vec::new());
    append(&mut builder, MANIFEST_NAME, &manifest_bytes, false)?;
    for file in sorted {
        append(&mut builder, &file.path, &file.data, file.executable)?;
    }
    builder.into_inner().map_err(tar_err)
}

fn append(
    builder: &mut tar::Builder<Vec<u8>>,
    path: &str,
    data: &[u8],
    executable: bool,
) -> Result<(), PackError> {
    let mut header = tar::Header::new_gnu();
    header.set_entry_type(tar::EntryType::Regular);
    header.set_size(data.len() as u64);
    header.set_mode(if executable { 0o755 } else { 0o644 });
    header.set_mtime(0);
    header.set_uid(0);
    header.set_gid(0);
    builder
        .append_data(&mut header, path, data)
        .map_err(tar_err)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CANVAS: &str = "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n";

    fn file(path: &str, data: &str) -> PackFile {
        PackFile {
            path: path.to_string(),
            data: data.as_bytes().to_vec(),
            executable: false,
        }
    }

    fn sample() -> Vec<PackFile> {
        vec![
            file("doc.canvas.md", CANVAS),
            file("img/sensor.webp", "not really an image"),
            file("notes.md", "# Notes\n"),
        ]
    }

    /// A real xz stream, from the same library the command line writes with.
    fn xz(tar: &[u8]) -> Vec<u8> {
        let mut encoder = liblzma::write::XzEncoder::new(Vec::new(), 6);
        encoder.write_all(tar).unwrap();
        encoder.finish().unwrap()
    }

    fn pack_bytes(files: &[PackFile]) -> Vec<u8> {
        xz(&build_tar("doc.canvas.md", "meshfox test", files, &Limits::default()).unwrap())
    }

    #[test]
    fn markdown_refs_find_local_images_and_links_only() {
        let md = concat!(
            "![a](img/a.png) and [b](notes/b.md#part) and [c](docs/My%20File.pdf?x=1)\n\n",
            "[web](https://example.com/x) [mail](mailto:a@b.c) [top](#top) [pr](//cdn/x.js)\n\n",
            "```\n![fenced](nope.png)\n```\n\n",
            "[abs](/etc/passwd) [up](../outside.md)\n",
        );
        let paths: Vec<String> = markdown_file_refs(md).into_iter().map(|r| r.path).collect();
        assert_eq!(
            paths,
            [
                "img/a.png",
                "notes/b.md",
                "docs/My File.pdf",
                "/etc/passwd",
                "../outside.md"
            ]
        );
    }

    #[test]
    fn markdown_refs_say_when_they_sit_in_cached_output() {
        let md = concat!(
            "![own](own.png)\n\n```bash name=\"go\"\necho hi\n```\n",
            "<!-- meshfox:output name=\"go\" -->\n![made](made.png)\n<!-- /meshfox:output -->\n",
        );
        let refs = markdown_file_refs(md);
        assert_eq!(
            refs,
            [
                FileRef {
                    path: "own.png".into(),
                    in_output: false
                },
                FileRef {
                    path: "made.png".into(),
                    in_output: true
                },
            ]
        );
    }

    // --- hand-made tar, for archives `build_tar` would never write ---

    fn octal(field: &mut [u8], value: u64) {
        let text = format!("{value:0width$o}", width = field.len() - 1);
        field[..text.len()].copy_from_slice(text.as_bytes());
        field[text.len()] = 0;
    }

    fn raw_entry(name: &[u8], typeflag: u8, data: &[u8]) -> Vec<u8> {
        let mut h = [0u8; 512];
        h[..name.len()].copy_from_slice(name);
        octal(&mut h[100..108], 0o644);
        octal(&mut h[108..116], 0);
        octal(&mut h[116..124], 0);
        octal(&mut h[124..136], data.len() as u64);
        octal(&mut h[136..148], 0);
        h[148..156].copy_from_slice(b"        ");
        h[156] = typeflag;
        h[257..263].copy_from_slice(b"ustar\0");
        h[263..265].copy_from_slice(b"00");
        let sum: u32 = h.iter().map(|&b| u32::from(b)).sum();
        let text = format!("{sum:06o}\0 ");
        h[148..156].copy_from_slice(text.as_bytes());
        let mut out = h.to_vec();
        out.extend_from_slice(data);
        out.resize(out.len().div_ceil(512) * 512, 0);
        out
    }

    fn tar_of(entries: Vec<Vec<u8>>) -> Vec<u8> {
        let mut out: Vec<u8> = entries.concat();
        out.extend_from_slice(&[0u8; 1024]);
        out
    }

    fn manifest_for(entry: &str, files: &[(&str, &[u8])]) -> Vec<u8> {
        let manifest = Manifest {
            format: FORMAT_VERSION,
            entry: entry.to_string(),
            created_by: String::new(),
            files: files
                .iter()
                .map(|(p, d)| ManifestFile {
                    path: p.to_string(),
                    size: d.len() as u64,
                    blake3: blake3_hex(d),
                })
                .collect(),
        };
        serde_json::to_vec(&manifest).unwrap()
    }

    /// A tar with a correct manifest for `files` (sorted as given), followed
    /// by `entries` as they are.
    fn tar_with(entry: &str, files: &[(&str, &[u8])], entries: Vec<Vec<u8>>) -> Vec<u8> {
        let mut all = vec![raw_entry(
            MANIFEST_NAME.as_bytes(),
            b'0',
            &manifest_for(entry, files),
        )];
        all.extend(entries);
        tar_of(all)
    }

    fn canvas_entry() -> Vec<u8> {
        raw_entry(b"doc.canvas.md", b'0', CANVAS.as_bytes())
    }

    fn read(tar: &[u8]) -> Result<Pack, PackError> {
        read_tar(tar, &Limits::default())
    }

    // --- round trips ---

    #[test]
    fn a_pack_round_trips_through_real_xz() {
        let files = sample();
        let pack = read_xz(&pack_bytes(&files), &Limits::default()).unwrap();
        assert_eq!(pack.entry, "doc.canvas.md");
        assert_eq!(pack.created_by, "meshfox test");
        assert_eq!(pack.files, files);
        assert_eq!(pack.file("notes.md").unwrap().data, b"# Notes\n");
        assert!(pack.file("missing").is_none());
    }

    #[test]
    fn what_compress_xz_writes_is_a_real_xz_file_any_reader_takes() {
        let tar = build_tar("doc.canvas.md", "", &sample(), &Limits::default()).unwrap();
        let packed = compress_xz(&tar).unwrap();
        assert!(is_xz(&packed));
        // Our reader takes it back...
        assert_eq!(
            read_xz(&packed, &Limits::default()).unwrap().files,
            sample()
        );
        // ...and so does the reference implementation (liblzma), byte for byte.
        let mut decoded = Vec::new();
        liblzma::read::XzDecoder::new(&packed[..])
            .read_to_end(&mut decoded)
            .unwrap();
        assert_eq!(decoded, tar);
    }

    #[test]
    fn compress_xz_is_deterministic_and_really_compresses() {
        let text = "meshfox canvases are mostly text, and text compresses well. ".repeat(2000);
        let files = vec![file("doc.canvas.md", CANVAS), file("notes.md", &text)];
        let tar = build_tar("doc.canvas.md", "", &files, &Limits::default()).unwrap();
        let a = compress_xz(&tar).unwrap();
        assert_eq!(a, compress_xz(&tar).unwrap());
        assert!(
            a.len() * 20 < tar.len(),
            "{} of {} bytes",
            a.len(),
            tar.len()
        );
    }

    #[test]
    fn packs_written_by_liblzma_at_any_preset_are_read() {
        let tar = build_tar("doc.canvas.md", "", &sample(), &Limits::default()).unwrap();
        for preset in [0, 1, 6, 9] {
            let mut encoder = liblzma::write::XzEncoder::new(Vec::new(), preset);
            encoder.write_all(&tar).unwrap();
            let packed = encoder.finish().unwrap();
            assert_eq!(
                read_xz(&packed, &Limits::default()).unwrap().files,
                sample(),
                "preset {preset}"
            );
        }
    }

    #[test]
    fn the_same_input_gives_the_same_bytes_whatever_the_order() {
        let mut shuffled = sample();
        shuffled.reverse();
        let a = build_tar("doc.canvas.md", "x", &sample(), &Limits::default()).unwrap();
        let b = build_tar("doc.canvas.md", "x", &shuffled, &Limits::default()).unwrap();
        assert_eq!(a, b);
        assert_eq!(xz(&a), xz(&b));
    }

    #[test]
    fn the_tar_is_plain_and_has_no_time_owner_or_group() {
        let tar = build_tar("doc.canvas.md", "x", &sample(), &Limits::default()).unwrap();
        let mut archive = tar::Archive::new(&tar[..]);
        let mut names = Vec::new();
        for entry in archive.entries().unwrap() {
            let entry = entry.unwrap();
            let h = entry.header();
            assert_eq!(
                (h.mtime().unwrap(), h.uid().unwrap(), h.gid().unwrap()),
                (0, 0, 0)
            );
            assert!(h.username().unwrap().unwrap_or("").is_empty());
            assert_eq!(h.mode().unwrap(), 0o644);
            names.push(entry.path().unwrap().to_string_lossy().into_owned());
        }
        assert_eq!(
            names,
            [
                MANIFEST_NAME,
                "doc.canvas.md",
                "img/sensor.webp",
                "notes.md"
            ]
        );
    }

    #[test]
    fn an_executable_bit_survives_and_other_modes_do_not_matter() {
        let mut files = sample();
        files[2].executable = true;
        let pack = read_xz(&pack_bytes(&files), &Limits::default()).unwrap();
        assert!(pack.file("notes.md").unwrap().executable);
        assert!(!pack.file("doc.canvas.md").unwrap().executable);
    }

    #[test]
    fn long_paths_and_non_ascii_names_survive() {
        let long = format!("{}/{}.md", "d".repeat(400), "é".repeat(200));
        let mut files = sample();
        files.push(file(&long, "long"));
        files.push(file("папка/файл.md", "юникод"));
        let pack = read_xz(&pack_bytes(&files), &Limits::default()).unwrap();
        assert_eq!(pack.file(&long).unwrap().data, b"long");
        assert_eq!(
            pack.file("папка/файл.md").unwrap().data,
            "юникод".as_bytes()
        );
    }

    #[test]
    fn an_entry_that_is_a_plain_md_needs_the_canvas_marker() {
        let with = vec![file("README.md", CANVAS)];
        build_tar("README.md", "", &with, &Limits::default()).unwrap();
        let without = vec![file("README.md", "# just markdown\n")];
        assert!(matches!(
            build_tar("README.md", "", &without, &Limits::default()),
            Err(PackError::BadEntry(..))
        ));
    }

    #[test]
    fn the_writer_refuses_what_the_reader_would() {
        let limits = Limits::default();
        for (path, why) in [
            ("../x.md", "`.` or `..`"),
            ("/abs.md", "absolute"),
            ("a\\b.md", "backslash"),
            ("a//b.md", "empty component"),
            ("C:/x.md", "drive"),
            (".meshfox/state.db", "own state"),
            (MANIFEST_NAME, "manifest"),
            ("a\u{0}b.md", "control"),
        ] {
            let mut files = sample();
            files.push(file(path, "x"));
            match build_tar("doc.canvas.md", "", &files, &limits) {
                Err(PackError::BadPath { .. }) => {}
                other => panic!("{path:?} ({why}): {other:?}"),
            }
        }
        let mut files = sample();
        files.push(file("IMG/Sensor.webp", "x"));
        assert!(matches!(
            build_tar("doc.canvas.md", "", &files, &limits),
            Err(PackError::Conflict { .. })
        ));
        let mut files = sample();
        files.push(file("notes.md/inner.md", "x"));
        assert!(matches!(
            build_tar("doc.canvas.md", "", &files, &limits),
            Err(PackError::Conflict { .. })
        ));
        let mut files = sample();
        files.push(file("notes.md", "again"));
        assert!(matches!(
            build_tar("doc.canvas.md", "", &files, &limits),
            Err(PackError::Duplicate(_))
        ));
        assert!(matches!(
            build_tar("nope.canvas.md", "", &sample(), &limits),
            Err(PackError::BadEntry(..))
        ));
    }

    // --- hostile archives ---

    #[test]
    fn not_xz_is_refused_by_its_bytes_not_its_name() {
        assert_eq!(
            read_xz(b"PK\x03\x04 a zip file", &Limits::default()),
            Err(PackError::NotXz)
        );
        assert_eq!(read_xz(b"", &Limits::default()), Err(PackError::NotXz));
    }

    #[test]
    fn a_truncated_or_damaged_xz_is_refused() {
        let good = pack_bytes(&sample());
        let cut = &good[..good.len() - 12];
        assert!(matches!(
            read_xz(cut, &Limits::default()),
            Err(PackError::Xz(_))
        ));
        let mut flipped = good.clone();
        let middle = flipped.len() / 2;
        flipped[middle] ^= 0xFF;
        assert!(read_xz(&flipped, &Limits::default()).is_err());
    }

    #[test]
    fn data_after_the_xz_stream_is_refused() {
        let mut bytes = pack_bytes(&sample());
        bytes.extend_from_slice(b"trailing garbage");
        assert!(matches!(
            read_xz(&bytes, &Limits::default()),
            Err(PackError::Xz(_))
        ));
    }

    #[test]
    fn a_tiny_xz_that_expands_without_end_stops_at_the_limit() {
        // 16 MiB of zeros is a few hundred bytes of xz.
        let mut tar = raw_entry(MANIFEST_NAME.as_bytes(), b'0', b"{}");
        tar.extend(raw_entry(b"big", b'0', &vec![0u8; 16 << 20]));
        let bomb = xz(&tar);
        assert!(bomb.len() < 4096, "{} bytes", bomb.len());
        // Every limit small, so the cap on unpacked bytes (files, manifest and
        // tar framing) is about a megabyte, far below what the stream holds.
        let limits = Limits {
            max_entries: 4,
            max_total_bytes: 1 << 20,
            max_file_bytes: 1 << 20,
            max_manifest_bytes: 1024,
            ..Limits::default()
        };
        assert!(matches!(
            read_xz(&bomb, &limits),
            Err(PackError::TooLarge(_))
        ));
        // Not a bomb to a reader whose limits allow it, merely not a pack.
        let roomy = Limits {
            max_total_bytes: 64 << 20,
            max_file_bytes: 64 << 20,
            ..Limits::default()
        };
        assert!(matches!(
            read_xz(&bomb, &roomy),
            Err(PackError::Manifest(_))
        ));
    }

    #[test]
    fn a_file_bigger_than_the_limit_is_refused_before_it_is_read() {
        let data = vec![b'x'; 2048];
        let tar = tar_with(
            "doc.canvas.md",
            &[("big.bin", &data), ("doc.canvas.md", CANVAS.as_bytes())],
            vec![raw_entry(b"big.bin", b'0', &data), canvas_entry()],
        );
        let limits = Limits {
            max_file_bytes: 1024,
            ..Limits::default()
        };
        assert!(matches!(
            read_tar(&tar, &limits),
            Err(PackError::TooLarge(_))
        ));
    }

    #[test]
    fn too_many_entries_are_refused() {
        let limits = Limits {
            max_entries: 3,
            ..Limits::default()
        };
        let files = [
            ("a.md", &b"a"[..]),
            ("b.md", b"b"),
            ("doc.canvas.md", CANVAS.as_bytes()),
        ];
        let tar = tar_with(
            "doc.canvas.md",
            &files,
            vec![
                raw_entry(b"a.md", b'0', b"a"),
                raw_entry(b"b.md", b'0', b"b"),
                canvas_entry(),
            ],
        );
        assert!(matches!(
            read_tar(&tar, &limits),
            Err(PackError::TooLarge(_))
        ));
    }

    #[test]
    fn an_oversized_manifest_is_refused() {
        let limits = Limits {
            max_manifest_bytes: 16,
            ..Limits::default()
        };
        let tar = tar_with(
            "doc.canvas.md",
            &[("doc.canvas.md", CANVAS.as_bytes())],
            vec![canvas_entry()],
        );
        assert!(matches!(
            read_tar(&tar, &limits),
            Err(PackError::TooLarge(_))
        ));
    }

    #[test]
    fn links_devices_and_fifos_are_refused() {
        for (flag, name) in [
            (b'2', "symlink"),
            (b'1', "hardlink"),
            (b'3', "char"),
            (b'4', "block"),
            (b'6', "fifo"),
        ] {
            let tar = tar_with(
                "doc.canvas.md",
                &[("doc.canvas.md", CANVAS.as_bytes())],
                vec![canvas_entry(), raw_entry(b"evil", flag, b"")],
            );
            match read(&tar) {
                Err(PackError::BadEntryType { path, .. }) => assert_eq!(path, "evil", "{name}"),
                other => panic!("{name}: {other:?}"),
            }
        }
    }

    #[test]
    fn paths_that_climb_or_are_absolute_are_refused_on_reading() {
        for name in [
            "../escape.md",
            "a/../../escape.md",
            "/etc/passwd",
            "a\\b.md",
            "C:/x.md",
            "a//b.md",
            ".meshfox/db",
            ".meshfox-pack.json",
        ] {
            let tar = tar_with(
                "doc.canvas.md",
                &[("doc.canvas.md", CANVAS.as_bytes())],
                vec![canvas_entry(), raw_entry(name.as_bytes(), b'0', b"x")],
            );
            assert!(
                matches!(read(&tar), Err(PackError::BadPath { .. })),
                "{name:?}: {:?}",
                read(&tar)
            );
        }
        // A path that is not UTF-8 at all.
        let tar = tar_with(
            "doc.canvas.md",
            &[("doc.canvas.md", CANVAS.as_bytes())],
            vec![
                canvas_entry(),
                raw_entry(&[b'a', 0xFF, b'.', b'm', b'd'], b'0', b"x"),
            ],
        );
        assert!(matches!(read(&tar), Err(PackError::BadPath { .. })));
    }

    #[test]
    fn the_manifest_must_come_first() {
        let tar = tar_of(vec![
            canvas_entry(),
            raw_entry(
                MANIFEST_NAME.as_bytes(),
                b'0',
                &manifest_for("doc.canvas.md", &[("doc.canvas.md", CANVAS.as_bytes())]),
            ),
        ]);
        assert!(matches!(read(&tar), Err(PackError::Manifest(_))));
        assert!(matches!(read(&tar_of(vec![])), Err(PackError::Manifest(_))));
    }

    #[test]
    fn a_file_the_manifest_does_not_list_is_refused() {
        let tar = tar_with(
            "doc.canvas.md",
            &[("doc.canvas.md", CANVAS.as_bytes())],
            vec![canvas_entry(), raw_entry(b"extra.md", b'0', b"x")],
        );
        assert_eq!(read(&tar), Err(PackError::NotListed("extra.md".into())));
    }

    #[test]
    fn a_file_the_archive_lacks_is_refused() {
        let tar = tar_with(
            "doc.canvas.md",
            &[("doc.canvas.md", CANVAS.as_bytes()), ("gone.md", b"x")],
            vec![canvas_entry()],
        );
        assert_eq!(read(&tar), Err(PackError::Missing("gone.md".into())));
    }

    #[test]
    fn a_changed_file_is_refused_by_size_and_by_hash() {
        let tar = tar_with(
            "doc.canvas.md",
            &[("a.md", b"original"), ("doc.canvas.md", CANVAS.as_bytes())],
            vec![raw_entry(b"a.md", b'0', b"tampered!!"), canvas_entry()],
        );
        assert!(matches!(read(&tar), Err(PackError::SizeMismatch { .. })));
        let tar = tar_with(
            "doc.canvas.md",
            &[("a.md", b"original"), ("doc.canvas.md", CANVAS.as_bytes())],
            vec![raw_entry(b"a.md", b'0', b"0riginal"), canvas_entry()],
        );
        assert_eq!(read(&tar), Err(PackError::HashMismatch("a.md".into())));
    }

    #[test]
    fn a_file_twice_in_the_archive_is_refused() {
        let tar = tar_with(
            "doc.canvas.md",
            &[("doc.canvas.md", CANVAS.as_bytes())],
            vec![canvas_entry(), canvas_entry()],
        );
        assert_eq!(
            read(&tar),
            Err(PackError::Duplicate("doc.canvas.md".into()))
        );
    }

    #[test]
    fn names_that_differ_only_in_case_are_refused() {
        let tar = tar_with(
            "doc.canvas.md",
            &[
                ("A.md", b"a"),
                ("a.md", b"b"),
                ("doc.canvas.md", CANVAS.as_bytes()),
            ],
            vec![
                raw_entry(b"A.md", b'0', b"a"),
                raw_entry(b"a.md", b'0', b"b"),
                canvas_entry(),
            ],
        );
        assert!(matches!(read(&tar), Err(PackError::Conflict { .. })));
    }

    #[test]
    fn a_file_that_is_also_a_directory_is_refused() {
        let tar = tar_with(
            "doc.canvas.md",
            &[
                ("doc.canvas.md", CANVAS.as_bytes()),
                ("x", b"1"),
                ("x/y", b"2"),
            ],
            vec![
                canvas_entry(),
                raw_entry(b"x", b'0', b"1"),
                raw_entry(b"x/y", b'0', b"2"),
            ],
        );
        assert!(matches!(read(&tar), Err(PackError::Conflict { .. })));
        let tar = tar_with(
            "doc.canvas.md",
            &[("doc.canvas.md", CANVAS.as_bytes()), ("x", b"1")],
            vec![
                canvas_entry(),
                raw_entry(b"x", b'0', b"1"),
                raw_entry(b"x/", b'5', b""),
            ],
        );
        assert!(matches!(read(&tar), Err(PackError::Conflict { .. })));
    }

    #[test]
    fn directories_are_accepted_and_hold_nothing_by_themselves() {
        let tar = tar_with(
            "doc.canvas.md",
            &[("doc.canvas.md", CANVAS.as_bytes())],
            vec![raw_entry(b"empty/", b'5', b""), canvas_entry()],
        );
        assert_eq!(read(&tar).unwrap().files.len(), 1);
    }

    #[test]
    fn a_newer_format_is_named_as_such() {
        let manifest = br#"{"format": 2, "entry": "doc.canvas.md", "files": [], "new_field": 1}"#;
        let tar = tar_of(vec![
            raw_entry(MANIFEST_NAME.as_bytes(), b'0', manifest),
            canvas_entry(),
        ]);
        assert_eq!(read(&tar), Err(PackError::UnsupportedFormat(2)));
        let tar = tar_of(vec![raw_entry(
            MANIFEST_NAME.as_bytes(),
            b'0',
            br#"{"entry": "x"}"#,
        )]);
        assert!(matches!(read(&tar), Err(PackError::Manifest(_))));
        let tar = tar_of(vec![raw_entry(MANIFEST_NAME.as_bytes(), b'0', b"not json")]);
        assert!(matches!(read(&tar), Err(PackError::Manifest(_))));
    }

    #[test]
    fn an_unsorted_manifest_or_a_bad_hash_is_refused() {
        let list = |files: &str| {
            let manifest =
                format!(r#"{{"format": 1, "entry": "doc.canvas.md", "files": [{files}]}}"#);
            tar_of(vec![
                raw_entry(MANIFEST_NAME.as_bytes(), b'0', manifest.as_bytes()),
                canvas_entry(),
            ])
        };
        let h = blake3_hex(CANVAS.as_bytes());
        let a = format!(
            r#"{{"path":"doc.canvas.md","size":{},"blake3":"{h}"}}"#,
            CANVAS.len()
        );
        let b = format!(r#"{{"path":"a.md","size":1,"blake3":"{h}"}}"#);
        assert!(matches!(
            read(&list(&format!("{a},{b}"))),
            Err(PackError::Manifest(_))
        ));
        let bad = format!(
            r#"{{"path":"doc.canvas.md","size":{},"blake3":"zz"}}"#,
            CANVAS.len()
        );
        assert!(matches!(read(&list(&bad)), Err(PackError::Manifest(_))));
    }

    #[test]
    fn the_entry_must_be_a_listed_canvas() {
        let tar = tar_with(
            "notes.md",
            &[
                ("doc.canvas.md", CANVAS.as_bytes()),
                ("notes.md", b"# Notes\n"),
            ],
            vec![canvas_entry(), raw_entry(b"notes.md", b'0', b"# Notes\n")],
        );
        assert!(matches!(read(&tar), Err(PackError::BadEntry(..))));
        let tar = tar_with(
            "elsewhere.canvas.md",
            &[("doc.canvas.md", CANVAS.as_bytes())],
            vec![canvas_entry()],
        );
        assert!(matches!(read(&tar), Err(PackError::BadEntry(..))));
        let tar = tar_with(
            "../doc.canvas.md",
            &[("doc.canvas.md", CANVAS.as_bytes())],
            vec![canvas_entry()],
        );
        assert!(matches!(read(&tar), Err(PackError::BadEntry(..))));
    }

    #[test]
    fn the_writer_enforces_the_limits_too() {
        let tiny = Limits {
            max_entries: 2,
            ..Limits::default()
        };
        assert!(matches!(
            build_tar("doc.canvas.md", "", &sample(), &tiny),
            Err(PackError::TooLarge(_))
        ));
        let small = Limits {
            max_file_bytes: 4,
            ..Limits::default()
        };
        assert!(matches!(
            build_tar("doc.canvas.md", "", &sample(), &small),
            Err(PackError::TooLarge(_))
        ));
    }
}
