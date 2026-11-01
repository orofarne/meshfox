//! Which paths of a tree meshfox leaves out: the same rules `meshfox serve
//! --http` uses to index canvases (see `meshfox-core`'s `canvas_index`), as a
//! pure function over text, so a browser picking a folder applies them too.
//!
//! - a name starting with `.` is left out, and with it everything below;
//! - so is whatever a `.meshfoxignore` excludes. It has `.gitignore` syntax
//!   (`#` comments, `dir/`, `*.log`, `/anchored`, `**`, `!negation`), lives in
//!   any directory and applies to that directory and everything below it; a
//!   deeper file overrides a shallower one.
//!
//! Nothing is left out by name otherwise (not `node_modules`, not `target`).
//! Symlinks are the host's business: the model never sees them.

use std::collections::HashMap;
use std::path::Path;

use ignore::gitignore::{Gitignore, GitignoreBuilder};

/// The file that holds the rules.
pub const IGNORE_FILE: &str = ".meshfoxignore";

/// The `.meshfoxignore` files found so far, by the directory they sit in
/// (`""` is the root). Paths are `/`-separated and relative to the root.
#[derive(Default)]
pub struct IgnoreRules {
    by_dir: HashMap<String, Gitignore>,
}

fn parts(path: &str) -> Vec<&str> {
    path.split('/')
        .filter(|p| !p.is_empty() && *p != ".")
        .collect()
}

impl IgnoreRules {
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers the text of the `.meshfoxignore` that sits in `dir`. Lines that
    /// do not parse exclude nothing, rather than failing.
    pub fn add(&mut self, dir: &str, text: &str) {
        let dir = parts(dir).join("/");
        let mut builder = GitignoreBuilder::new(Path::new("/").join(&dir));
        for line in text.lines() {
            let _ = builder.add_line(None, line);
        }
        if let Ok(matcher) = builder.build() {
            self.by_dir.insert(dir, matcher);
        }
    }

    /// Whether `path` (a file, or a directory when `is_dir`) is left out: it,
    /// or any directory above it, is hidden or excluded.
    pub fn is_ignored(&self, path: &str, is_dir: bool) -> bool {
        let parts = parts(path);
        for i in 0..parts.len() {
            if parts[i].starts_with('.') {
                return true;
            }
            let is_dir_here = i + 1 < parts.len() || is_dir;
            if self.excluded(&parts[..=i], is_dir_here) {
                return true;
            }
        }
        false
    }

    /// The deepest ignore file with an opinion on `entry` wins; a `!` line in it
    /// brings back what a shallower one excluded.
    fn excluded(&self, entry: &[&str], is_dir: bool) -> bool {
        let abs = Path::new("/").join(entry.join("/"));
        for depth in (0..entry.len()).rev() {
            let Some(matcher) = self.by_dir.get(&entry[..depth].join("/")) else {
                continue;
            };
            match matcher.matched(&abs, is_dir) {
                ignore::Match::Ignore(_) => return true,
                ignore::Match::Whitelist(_) => return false,
                ignore::Match::None => {}
            }
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules(files: &[(&str, &str)]) -> IgnoreRules {
        let mut r = IgnoreRules::new();
        for (dir, text) in files {
            r.add(dir, text);
        }
        r
    }

    #[test]
    fn nothing_is_left_out_by_name_alone() {
        let r = IgnoreRules::new();
        assert!(!r.is_ignored("node_modules/x/y.md", false));
        assert!(!r.is_ignored("target", true));
        assert!(!r.is_ignored("a/b.canvas.md", false));
    }

    #[test]
    fn dot_names_are_left_out_with_everything_below() {
        let r = IgnoreRules::new();
        assert!(r.is_ignored(".git", true));
        assert!(r.is_ignored(".notes/x.md", false));
        assert!(r.is_ignored("a/.hidden.md", false));
        assert!(r.is_ignored("a/.b/c/d.md", false));
        assert!(!r.is_ignored("a/b.md", false));
    }

    #[test]
    fn gitignore_syntax() {
        let r = rules(&[("", "# comment\nnode_modules/\n*.log\n/drafts/\n!keep.log\n")]);
        assert!(r.is_ignored("node_modules", true));
        assert!(r.is_ignored("a/node_modules/x.md", false));
        assert!(r.is_ignored("run.log", false));
        assert!(r.is_ignored("a/run.log", false));
        assert!(!r.is_ignored("keep.log", false));
        assert!(r.is_ignored("drafts/x.md", false));
        assert!(
            !r.is_ignored("a/drafts/x.md", false),
            "anchored to the root"
        );
        // `dir/` only matches directories.
        assert!(!r.is_ignored("node_modules", false));
    }

    #[test]
    fn a_file_applies_to_its_own_directory_and_below_only() {
        let r = rules(&[("sub", "secret.md\n")]);
        assert!(r.is_ignored("sub/secret.md", false));
        assert!(r.is_ignored("sub/deeper/secret.md", false));
        assert!(!r.is_ignored("secret.md", false));
        assert!(!r.is_ignored("other/secret.md", false));
    }

    #[test]
    fn a_deeper_file_overrides_a_shallower_one() {
        let r = rules(&[("", "*.md\n"), ("docs", "!*.md\n")]);
        assert!(r.is_ignored("a.md", false));
        assert!(!r.is_ignored("docs/a.md", false));
        assert!(!r.is_ignored("docs/deeper/a.md", false));
        assert!(r.is_ignored("other/a.md", false));
    }

    #[test]
    fn unparseable_lines_exclude_nothing() {
        let r = rules(&[("", "[unclosed\nreal.md\n")]);
        assert!(r.is_ignored("real.md", false));
        assert!(!r.is_ignored("other.md", false));
    }

    #[test]
    fn dot_slash_and_empty_segments_are_folded() {
        let r = rules(&[("./", "x.md\n")]);
        assert!(r.is_ignored("./a//x.md", false));
    }
}
