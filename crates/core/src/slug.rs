//! Stable, short URL slugs for canvases served by `meshfox serve --http`.
//!
//! A slug is `<prefix>-<hash>`, e.g. `todo-k7m2q5xa`:
//!
//! - `hash` is 8 base32 characters (40 bits of blake3) of the canvas path
//!   *relative to the served root*, so the same canvas keeps its slug wherever
//!   the root is mounted;
//! - `prefix` is the file name without `.canvas.md`, squeezed to `[a-z0-9-]`.
//!   It is derived from the same path, so it carries no information of its
//!   own: a URL whose prefix disagrees with the hash is a typo or tampering,
//!   and [`verify`] rejects it.
//!
//! Slugs are not reversible. The caller keeps an index of known paths and
//! looks the hash up in it ([`hash_of`]), then confirms with [`verify`].

use std::path::{Component, Path};

use unicode_normalization::UnicodeNormalization;

/// Length of the hash part, in base32 characters (8 × 5 bits = 40 bits).
pub const HASH_LEN: usize = 8;
/// Longest readable prefix kept; longer file names are cut.
pub const PREFIX_MAX: usize = 24;
/// Prefix used when the file name leaves nothing usable (e.g. Cyrillic).
pub const FALLBACK_PREFIX: &str = "canvas";

const ALPHABET: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";
const CANVAS_SUFFIX: &str = ".canvas.md";

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SlugError {
    #[error("{0} is outside the served root")]
    OutsideRoot(String),
    #[error("path has no file name")]
    NoFileName,
    #[error("cannot resolve {path}: {message}")]
    Resolve { path: String, message: String },
}

/// Slug of `path` under `root`. Both are canonicalised first, so symlinks
/// that leave the root are [`SlugError::OutsideRoot`], not silently followed.
pub fn slug(root: &Path, path: &Path) -> Result<String, SlugError> {
    let resolve = |p: &Path| {
        p.canonicalize().map_err(|e| SlugError::Resolve {
            path: p.display().to_string(),
            message: e.to_string(),
        })
    };
    let root = resolve(root)?;
    let path = resolve(path)?;
    let rel = path
        .strip_prefix(&root)
        .map_err(|_| SlugError::OutsideRoot(path.display().to_string()))?;
    slug_for_rel(rel)
}

/// Slug for a path that is already relative to the root (no filesystem
/// access). Anything but plain components (`..`, `.`, a prefix) is rejected as
/// outside the root.
pub fn slug_for_rel(rel: &Path) -> Result<String, SlugError> {
    let mut parts: Vec<String> = Vec::new();
    for component in rel.components() {
        match component {
            Component::Normal(name) => parts.push(name.to_string_lossy().nfc().collect()),
            _ => return Err(SlugError::OutsideRoot(rel.display().to_string())),
        }
    }
    let Some(file_name) = parts.last() else {
        return Err(SlugError::NoFileName);
    };
    let prefix = prefix_of(file_name);
    let hash = hash_of_rel(&parts.join("/"));
    Ok(format!("{prefix}-{hash}"))
}

/// The hash part of a slug-shaped URL segment: the [`HASH_LEN`] characters
/// after the last `-`. `None` when the segment cannot be a slug at all.
pub fn hash_of(segment: &str) -> Option<&str> {
    let (prefix, hash) = segment.rsplit_once('-')?;
    let valid =
        !prefix.is_empty() && hash.len() == HASH_LEN && hash.bytes().all(|b| ALPHABET.contains(&b));
    valid.then_some(hash)
}

/// Whether `segment` is exactly the slug of `rel`. Compares the whole
/// string, so a wrong or hand-edited prefix fails even with a right hash.
pub fn verify(rel: &Path, segment: &str) -> bool {
    slug_for_rel(rel).is_ok_and(|expected| expected == segment)
}

fn prefix_of(file_name: &str) -> String {
    let stem = file_name.strip_suffix(CANVAS_SUFFIX).unwrap_or(file_name);
    let mut out = String::new();
    let mut pending_dash = false;
    for c in stem.chars().flat_map(char::to_lowercase) {
        if c.is_ascii_alphanumeric() {
            if pending_dash && !out.is_empty() {
                out.push('-');
            }
            pending_dash = false;
            out.push(c);
        } else {
            pending_dash = true;
        }
    }
    out.truncate(PREFIX_MAX);
    while out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() {
        FALLBACK_PREFIX.to_string()
    } else {
        out
    }
}

fn hash_of_rel(rel: &str) -> String {
    let digest = blake3::hash(rel.as_bytes());
    let bytes = digest.as_bytes();
    // 5 bytes = 40 bits = exactly 8 base32 characters, no padding.
    let mut acc: u64 = 0;
    for b in &bytes[..5] {
        acc = (acc << 8) | u64::from(*b);
    }
    (0..HASH_LEN)
        .map(|i| {
            let shift = 5 * (HASH_LEN - 1 - i);
            char::from(ALPHABET[((acc >> shift) & 0x1f) as usize])
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    fn rel(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    fn tmp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("mfx-slug-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir.canonicalize().unwrap()
    }

    #[test]
    fn shape_is_prefix_dash_eight_base32() {
        let s = slug_for_rel(&rel("notes/TODO.canvas.md")).unwrap();
        let (prefix, hash) = s.rsplit_once('-').unwrap();
        assert_eq!(prefix, "todo");
        assert_eq!(hash.len(), HASH_LEN);
        assert!(hash.bytes().all(|b| ALPHABET.contains(&b)));
    }

    #[test]
    fn is_stable_across_calls() {
        let a = slug_for_rel(&rel("a/b.canvas.md")).unwrap();
        let b = slug_for_rel(&rel("a/b.canvas.md")).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn same_file_name_in_different_directories_differs() {
        let a = slug_for_rel(&rel("x/todo.canvas.md")).unwrap();
        let b = slug_for_rel(&rel("y/todo.canvas.md")).unwrap();
        assert_ne!(a, b);
        assert_eq!(hash_of(&a).map(str::len), Some(HASH_LEN));
        assert!(a.starts_with("todo-") && b.starts_with("todo-"));
    }

    #[test]
    fn nfc_and_nfd_spellings_give_one_slug() {
        // "é" precomposed vs "e" + combining acute.
        let nfc = slug_for_rel(&rel("caf\u{e9}/a.canvas.md")).unwrap();
        let nfd = slug_for_rel(&rel("cafe\u{301}/a.canvas.md")).unwrap();
        assert_eq!(nfc, nfd);
    }

    #[test]
    fn prefix_is_squeezed_lowercased_and_cut() {
        assert_eq!(prefix_of("My  Notes_v2!.canvas.md"), "my-notes-v2");
        assert_eq!(prefix_of("--x--.canvas.md"), "x");
        let long = format!("{}.canvas.md", "a".repeat(80));
        assert_eq!(prefix_of(&long).len(), PREFIX_MAX);
        // Cutting must not leave a dangling dash.
        let cut = format!("{}-b.canvas.md", "a".repeat(PREFIX_MAX - 1));
        assert_eq!(prefix_of(&cut), "a".repeat(PREFIX_MAX - 1));
    }

    #[test]
    fn unusable_names_fall_back() {
        assert_eq!(prefix_of("Заметки.canvas.md"), FALLBACK_PREFIX);
        assert_eq!(prefix_of(".canvas.md"), FALLBACK_PREFIX);
        let s = slug_for_rel(&rel("Заметки.canvas.md")).unwrap();
        assert!(s.starts_with("canvas-"));
        assert!(hash_of(&s).is_some());
    }

    #[test]
    fn a_name_with_dashes_still_yields_the_hash() {
        let s = slug_for_rel(&rel("my-long-name.canvas.md")).unwrap();
        assert!(s.starts_with("my-long-name-"));
        assert_eq!(hash_of(&s), Some(&s[s.len() - HASH_LEN..]));
    }

    #[test]
    fn hash_of_rejects_non_slugs() {
        assert_eq!(hash_of("nodash"), None);
        assert_eq!(hash_of("todo-short"), None);
        assert_eq!(hash_of("todo-UPPERCASE"), None);
        assert_eq!(hash_of("todo-k7m2q5x!"), None);
        assert_eq!(hash_of("-k7m2q5xa"), None);
        assert_eq!(hash_of("todo-k7m2q5xa"), Some("k7m2q5xa"));
    }

    #[test]
    fn verify_requires_the_exact_slug() {
        let p = rel("notes/todo.canvas.md");
        let good = slug_for_rel(&p).unwrap();
        assert!(verify(&p, &good));
        let hash = hash_of(&good).unwrap();
        // Right hash, wrong prefix: rejected.
        assert!(!verify(&p, &format!("other-{hash}")));
        // Right prefix, wrong hash (last character changed): rejected.
        let last = if good.ends_with('a') { 'b' } else { 'a' };
        let tampered = format!("{}{last}", &good[..good.len() - 1]);
        assert!(!verify(&p, &tampered));
        assert!(!verify(&p, ""));
    }

    #[test]
    fn rel_with_parent_or_absolute_components_is_outside() {
        assert!(matches!(
            slug_for_rel(&rel("../x.canvas.md")),
            Err(SlugError::OutsideRoot(_))
        ));
        assert!(matches!(
            slug_for_rel(&rel("/etc/x.canvas.md")),
            Err(SlugError::OutsideRoot(_))
        ));
        assert_eq!(slug_for_rel(&rel("")), Err(SlugError::NoFileName));
    }

    #[test]
    fn slug_does_not_depend_on_where_the_root_is() {
        let a = tmp("root-a");
        let b = tmp("root-b");
        for root in [&a, &b] {
            fs::create_dir_all(root.join("notes")).unwrap();
            fs::write(root.join("notes/todo.canvas.md"), "# t\n").unwrap();
        }
        let sa = slug(&a, &a.join("notes/todo.canvas.md")).unwrap();
        let sb = slug(&b, &b.join("notes/todo.canvas.md")).unwrap();
        assert_eq!(sa, sb);
        let _ = fs::remove_dir_all(&a);
        let _ = fs::remove_dir_all(&b);
    }

    #[cfg(unix)]
    #[test]
    fn symlink_leaving_the_root_is_outside_but_one_inside_is_fine() {
        use std::os::unix::fs::symlink;
        let root = tmp("symlink-root");
        let outside = tmp("symlink-outside");
        fs::write(outside.join("secret.canvas.md"), "# s\n").unwrap();
        fs::create_dir_all(root.join("real")).unwrap();
        fs::write(root.join("real/a.canvas.md"), "# a\n").unwrap();
        symlink(
            outside.join("secret.canvas.md"),
            root.join("leak.canvas.md"),
        )
        .unwrap();
        symlink(root.join("real/a.canvas.md"), root.join("alias.canvas.md")).unwrap();

        assert!(matches!(
            slug(&root, &root.join("leak.canvas.md")),
            Err(SlugError::OutsideRoot(_))
        ));
        // An in-root alias resolves to its target, so it shares the target's slug.
        assert_eq!(
            slug(&root, &root.join("alias.canvas.md")).unwrap(),
            slug(&root, &root.join("real/a.canvas.md")).unwrap()
        );
        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_dir_all(&outside);
    }

    #[test]
    fn missing_file_is_a_resolve_error() {
        let root = tmp("missing");
        assert!(matches!(
            slug(&root, &root.join("nope.canvas.md")),
            Err(SlugError::Resolve { .. })
        ));
        let _ = fs::remove_dir_all(&root);
    }
}
