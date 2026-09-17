//! Dynamic, read-only resolution of `include` nodes (see
//! `crate::canvas::NodeType::Include`): the target's own raw file content
//! gets dumped into the include node's own body, headings shifted down by
//! its level so the target's top-level `#` doesn't read as a second
//! document root. This never touches disk beyond *reading* the target
//! (never written back), and never recurses: the target's own bytes are
//! taken verbatim, even if it happens to be a `.canvas.md` with meshfox
//! structure of its own — an include never introduces a new addressable
//! node, so there's nothing to namespace and no cycle to detect.
//!
//! `resolve` takes a parsed `Canvas` and returns a new one with every
//! top-level `include` node expanded in memory. `check` never calls this —
//! it works on a single file's raw text directly, and sees a bare
//! `[label](target)` link, same as `file`/`link`. Consumers that want the
//! resolved view — the server (before serving `GET /api/canvas`),
//! `run`/the TUI (to run a block living in an include node's own dumped
//! body) — call it directly.

use crate::canvas::{Canvas, NodeType};
use crate::mdcanvas;
use std::path::{Path, PathBuf};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum IncludeError {
    #[error("include node {0:?} has no target")]
    MissingTarget(String),
    #[error("include target {0} not found: {1}")]
    NotFound(PathBuf, #[source] std::io::Error),
}

/// Returns a new `Canvas` with every `include` node's target dumped into
/// its own body text (see the module doc comment). `base_path` is the file
/// `canvas` was parsed from, used to resolve a relative include target
/// against its directory.
pub fn resolve(canvas: &Canvas, base_path: &Path) -> Result<Canvas, IncludeError> {
    let base_dir = base_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default();
    let mut nodes = canvas.nodes.clone();

    for node in nodes.iter_mut().filter(|n| n.node_type == NodeType::Include) {
        let target = node
            .target
            .clone()
            .ok_or_else(|| IncludeError::MissingTarget(node.id.clone()))?;
        let target_path = base_dir.join(&target);
        let contents = std::fs::read_to_string(&target_path)
            .map_err(|e| IncludeError::NotFound(target_path.clone(), e))?;
        // Best-effort: an unreadable/relative-weirdness parent just leaves
        // `asset_base` unset, falling back to the canvas's own directory —
        // same graceful-degradation posture the rest of this crate takes
        // for display-only metadata.
        let asset_base = target_path
            .canonicalize()
            .ok()
            .and_then(|p| p.parent().map(|p| p.to_string_lossy().into_owned()));

        node.node_type = NodeType::Text;
        node.target = None;
        node.text = mdcanvas::shift_headings(&contents, node.level);
        node.asset_base = asset_base;
        node.plain_markdown_include = true;
    }

    Ok(Canvas {
        nodes,
        options: Vec::new(),
    })
}

/// One `include` node declared directly in `canvas`, resolved to the
/// physical file it points at, but without dumping anything in — a
/// read-only companion to `resolve` for a consumer that wants to know
/// *what's there* (the web UI's Source-mode file picker) rather than the
/// resolved document.
#[derive(Debug, Clone)]
pub struct IncludeInfo {
    pub node_id: String,
    pub title: String,
    /// The literal link target as written (e.g. `./SPEC.md`).
    pub target: String,
    /// Absolute path it resolves to.
    pub path: PathBuf,
}

/// Lists every `include` node declared directly in `canvas` (see
/// `IncludeInfo`), without ever reading its target's content — `base_path`
/// is the file `canvas` was parsed from, same as `resolve`.
pub fn list_includes(canvas: &Canvas, base_path: &Path) -> Vec<IncludeInfo> {
    let base_dir = base_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default();

    canvas
        .nodes
        .iter()
        .filter(|n| n.node_type == NodeType::Include)
        .filter_map(|n| {
            let target = n.target.clone()?;
            let target_path = base_dir.join(&target);
            let path = target_path.canonicalize().unwrap_or(target_path);
            Some(IncludeInfo {
                node_id: n.id.clone(),
                title: n.title.clone(),
                target,
                path,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write(dir: &Path, name: &str, contents: &str) -> PathBuf {
        let path = dir.join(name);
        fs::write(&path, contents).unwrap();
        path
    }

    #[test]
    fn includes_plain_markdown_as_shifted_body_text() {
        let tmp = std::env::temp_dir().join(format!("meshfox-include-test-{}", std::process::id()));
        fs::create_dir_all(&tmp).unwrap();
        let _target = write(
            &tmp,
            "spec.md",
            "# Spec Title\n\nSome body.\n\n## Sub\nmore\n",
        );
        let base = write(
            &tmp,
            "root.canvas.md",
            "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node -->\n\n## Spec\n<!-- meshfox:node id=\"spec\" type=\"include\" -->\n\n[spec](./spec.md)\n",
        );

        let raw = fs::read_to_string(&base).unwrap();
        let canvas = Canvas::from_markdown(&raw).unwrap();
        let resolved = resolve(&canvas, &base).unwrap();

        let spec = resolved.node("spec").unwrap();
        assert_eq!(spec.node_type, NodeType::Text);
        assert!(spec.target.is_none());
        // Marks this node's body as belonging to the target file, not its
        // own — the signal a client (e.g. the web UI) uses to steer a
        // would-be per-node body editor toward Source mode instead.
        assert!(spec.plain_markdown_include);
        // Spec's own H1 (level 1) becomes level 1+2=3 (spec node is level 2).
        assert!(spec.text.contains("### Spec Title"));
        assert!(spec.text.contains("#### Sub"));
        // No new nodes: an include never parses its target's own structure.
        assert_eq!(resolved.nodes.len(), 2);
        // A relative asset (e.g. `![](fig.png)`) in the target's body
        // resolves against the target's own directory, not the including
        // document's — recorded here so a consumer (the server) can serve
        // it correctly instead of 404ing against the wrong directory.
        assert_eq!(
            spec.asset_base.as_deref(),
            Some(tmp.canonicalize().unwrap().to_str().unwrap())
        );

        fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn includes_a_canvas_target_as_plain_dumped_text_too() {
        let tmp = std::env::temp_dir().join(format!(
            "meshfox-include-test-{}",
            std::process::id() + 1
        ));
        fs::create_dir_all(&tmp).unwrap();
        write(
            &tmp,
            "child.canvas.md",
            "<!-- meshfox:canvas -->\n# Child Root\n<!-- meshfox:node id=\"root\" -->\n\nintro\n\n## Leaf\n<!-- meshfox:node id=\"leaf\" -->\n\nbody\n",
        );
        let base = write(
            &tmp,
            "root.canvas.md",
            "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node -->\n\n## Child\n<!-- meshfox:node id=\"child\" type=\"include\" -->\n\n[child](./child.canvas.md)\n",
        );

        let raw = fs::read_to_string(&base).unwrap();
        let canvas = Canvas::from_markdown(&raw).unwrap();
        let resolved = resolve(&canvas, &base).unwrap();

        // Dumped verbatim as this node's own body — no `child/root`,
        // `child/leaf`, or any other new node gets created.
        let child = resolved.node("child").unwrap();
        assert_eq!(child.node_type, NodeType::Text);
        assert!(child.plain_markdown_include);
        assert!(child.text.contains("Child Root"));
        assert!(child.text.contains("Leaf"));
        assert_eq!(resolved.nodes.len(), 2);

        fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn missing_include_target_is_a_clear_error() {
        let tmp = std::env::temp_dir().join(format!(
            "meshfox-include-test-{}",
            std::process::id() + 2
        ));
        fs::create_dir_all(&tmp).unwrap();
        let base = write(
            &tmp,
            "root.canvas.md",
            "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node -->\n\n## Spec\n<!-- meshfox:node id=\"spec\" type=\"include\" -->\n\n[spec](./nope.md)\n",
        );

        let raw = fs::read_to_string(&base).unwrap();
        let canvas = Canvas::from_markdown(&raw).unwrap();
        let err = resolve(&canvas, &base).unwrap_err();
        assert!(matches!(err, IncludeError::NotFound(_, _)));

        fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn list_includes_reports_a_top_level_include_without_reading_its_target() {
        let tmp = std::env::temp_dir().join(format!(
            "meshfox-list-includes-test-{}",
            std::process::id()
        ));
        fs::create_dir_all(&tmp).unwrap();
        write(&tmp, "child.md", "# Child\n\nbody\n");
        let base = write(
            &tmp,
            "root.canvas.md",
            "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node -->\n\n## Child\n<!-- meshfox:node id=\"child\" type=\"include\" -->\n\n[child](./child.md)\n",
        );

        let raw = fs::read_to_string(&base).unwrap();
        let canvas = Canvas::from_markdown(&raw).unwrap();
        let includes = list_includes(&canvas, &base);

        assert_eq!(includes.len(), 1);
        assert_eq!(includes[0].node_id, "child");
        assert_eq!(includes[0].title, "Child");
        assert_eq!(
            includes[0].path,
            tmp.join("child.md").canonicalize().unwrap()
        );

        fs::remove_dir_all(&tmp).ok();
    }
}
