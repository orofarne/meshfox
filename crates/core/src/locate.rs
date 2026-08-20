//! Finding which real on-disk file a node's content actually lives in —
//! itself (the primary document) or an `include` target elsewhere, however
//! deeply nested — shared by every consumer that needs to both *read* a
//! specific node's own text and, later, *write* an edit or a cached
//! block-output back to the right file. The server's mutating endpoints
//! were the first to need this (`crate::include`'s own doc comment used to
//! say only a "fully composed view" consumer ever calls `include::resolve`
//! at all); `meshfox run`/the TUI now do too, so a runnable block living
//! inside an included canvas is addressable and its `cache` lands in the
//! right file, not silently dropped or misattributed to the primary one.

use crate::canvas::Canvas;
use crate::include::{self, IncludeError};
use crate::mdcanvas::ParseError;
use std::path::{Path, PathBuf};
use thiserror::Error;

/// Where a node's real content lives, and its text as of the moment this
/// was called.
#[derive(Debug)]
pub struct LocatedNode {
    /// `None` for the primary document itself; `Some(path)` for an
    /// `include` target — `raw` in that case is read fresh from disk each
    /// call, not cached anywhere, since it isn't a file the caller
    /// necessarily already owns an in-memory copy of.
    pub origin: Option<PathBuf>,
    pub raw: String,
    /// `id`, translated to whatever it's actually called in `raw` — the
    /// primary document's own id verbatim when `origin` is `None`, or the
    /// include target's own local (pre-namespacing) id otherwise.
    pub local_id: String,
}

#[derive(Debug, Error)]
pub enum LocateError {
    #[error(transparent)]
    Parse(#[from] ParseError),
    #[error(transparent)]
    Include(#[from] IncludeError),
    #[error("no node {0:?}")]
    NotFound(String),
    /// The node was found, but only as a plain-Markdown `include`'s own
    /// body (shifted headings and all) — it has no real, separate on-disk
    /// node identity of its own to read/write back to (see
    /// `crate::canvas::Node::origin_path`'s own doc comment).
    #[error(
        "node {0:?} lives inside an included canvas with no node identity of its own \
         (it was included as plain Markdown rather than a .canvas.md) — open its own file directly"
    )]
    NoOwnIdentity(String),
    #[error("failed to read include target {0}: {1}")]
    Io(String, #[source] std::io::Error),
}

/// Finds `id` in `primary_raw` (`primary_path`'s own already-loaded text)
/// if it lives there directly; otherwise resolves every `include` reachable
/// from `primary_path` (however deeply nested) and looks it up in that
/// fully composed tree instead, reading its real origin file fresh off
/// disk. `primary_path` is only ever used to resolve `include` targets
/// relative to the primary document's own directory — never read itself
/// (`primary_raw` is assumed to already be its current content).
pub fn locate_node(
    primary_raw: &str,
    primary_path: &Path,
    id: &str,
) -> Result<LocatedNode, LocateError> {
    let primary = Canvas::from_markdown(primary_raw)?;
    if primary.node(id).is_some() {
        return Ok(LocatedNode {
            origin: None,
            raw: primary_raw.to_string(),
            local_id: id.to_string(),
        });
    }
    let resolved = include::resolve(&primary, primary_path)?;
    let node = resolved
        .node(id)
        .ok_or_else(|| LocateError::NotFound(id.to_string()))?;
    let origin_path = node
        .origin_path
        .clone()
        .ok_or_else(|| LocateError::NoOwnIdentity(id.to_string()))?;
    let local_id = node
        .origin_id
        .clone()
        .expect("origin_path is always set together with origin_id");
    let raw = std::fs::read_to_string(&origin_path)
        .map_err(|e| LocateError::Io(origin_path.clone(), e))?;
    Ok(LocatedNode {
        origin: Some(PathBuf::from(origin_path)),
        raw,
        local_id,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_base_and_child() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("meshfox-locate-test-{}", uuid_like()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("child.canvas.md"),
            concat!(
                "<!-- meshfox:canvas -->\n# Child\n<!-- meshfox:node id=\"root\" -->\n\n",
                "## Leaf\n<!-- meshfox:node id=\"leaf\" -->\n\nleaf body\n",
            ),
        )
        .unwrap();
        let base_path = dir.join("base.canvas.md");
        std::fs::write(
            &base_path,
            concat!(
                "<!-- meshfox:canvas -->\n# Base\n<!-- meshfox:node id=\"base\" -->\n\n",
                "## Child\n<!-- meshfox:node id=\"child\" type=\"include\" -->\n\n[child](./child.canvas.md)\n",
            ),
        )
        .unwrap();
        base_path
    }

    fn uuid_like() -> String {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        format!(
            "{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        )
    }

    #[test]
    fn finds_a_node_in_the_primary_document_directly() {
        let base_path = write_base_and_child();
        let raw = std::fs::read_to_string(&base_path).unwrap();
        let located = locate_node(&raw, &base_path, "base").unwrap();
        assert!(located.origin.is_none());
        assert_eq!(located.local_id, "base");
        assert_eq!(located.raw, raw);
        let _ = std::fs::remove_dir_all(base_path.parent().unwrap());
    }

    #[test]
    fn resolves_a_namespaced_id_to_its_own_include_target_file() {
        let base_path = write_base_and_child();
        let child_path = base_path
            .parent()
            .unwrap()
            .join("child.canvas.md")
            .canonicalize()
            .unwrap();
        let raw = std::fs::read_to_string(&base_path).unwrap();
        let located = locate_node(&raw, &base_path, "child/leaf").unwrap();
        assert_eq!(located.origin, Some(child_path.clone()));
        assert_eq!(located.local_id, "leaf");
        assert!(located.raw.contains("leaf body"));
        let _ = std::fs::remove_dir_all(base_path.parent().unwrap());
    }

    #[test]
    fn errors_on_an_unknown_id() {
        let base_path = write_base_and_child();
        let raw = std::fs::read_to_string(&base_path).unwrap();
        let err = locate_node(&raw, &base_path, "nope").unwrap_err();
        assert!(matches!(err, LocateError::NotFound(id) if id == "nope"));
        let _ = std::fs::remove_dir_all(base_path.parent().unwrap());
    }
}
