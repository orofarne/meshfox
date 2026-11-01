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
    #[error(transparent)]
    Access(#[from] crate::file_source::ConfineError),
}

/// An `include` that could not be read, in a lenient resolve (see
/// [`resolve_lenient`]): the node stays, holding a notice in place of the
/// target's content.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IncludeWarning {
    pub node_id: String,
    /// The target as written, e.g. `./TEMPLATES.md`.
    pub target: String,
    /// Why it could not be read.
    pub reason: String,
}

/// Strict: the first include that cannot be read is an error.
pub fn resolve_with_access(
    canvas: &Canvas,
    base_path: &Path,
    access: &dyn crate::file_source::FileSource,
) -> Result<Canvas, IncludeError> {
    resolve_inner(canvas, base_path, access, false).map(|(canvas, _)| canvas)
}

/// Lenient, for a viewer that only has some of the files (a single `.canvas.md`
/// picked in a browser): an include whose target is missing, or outside what
/// may be read, does not fail the document. Its node becomes a text node with
/// a warning in place of the content, and is reported in the second result.
/// A node with no target at all is still an error.
pub fn resolve_lenient(
    canvas: &Canvas,
    base_path: &Path,
    access: &dyn crate::file_source::FileSource,
) -> Result<(Canvas, Vec<IncludeWarning>), IncludeError> {
    resolve_inner(canvas, base_path, access, true)
}

fn unreadable_notice(target: &str, reason: &str) -> String {
    format!(
        "> [!WARNING]\n> **Include not available.** `{target}` could not be read: {reason}.\n\
         > Its content is not shown here.\n"
    )
}

fn resolve_inner(
    canvas: &Canvas,
    base_path: &Path,
    access: &dyn crate::file_source::FileSource,
    lenient: bool,
) -> Result<(Canvas, Vec<IncludeWarning>), IncludeError> {
    let base_dir = base_path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let mut nodes = canvas.nodes.clone();
    let mut warnings = Vec::new();

    for node in nodes
        .iter_mut()
        .filter(|n| n.node_type == NodeType::Include)
    {
        let target = node
            .target
            .clone()
            .ok_or_else(|| IncludeError::MissingTarget(node.id.clone()))?;
        let read = (|| {
            let target_path = access
                .confine(&base_dir, &target)
                .map_err(|error| match error {
                    crate::file_source::ConfineError::TargetNotFound(path, source) => {
                        IncludeError::NotFound(path, source)
                    }
                    other => IncludeError::Access(other),
                })?;
            let contents = access
                .read_to_string(&target_path)
                .map_err(|e| IncludeError::NotFound(target_path.clone(), e))?;
            Ok((target_path, contents))
        })();
        let (target_path, contents) = match read {
            Ok(ok) => ok,
            Err(error) if lenient => {
                let reason = match &error {
                    IncludeError::NotFound(..) => "it is not among the files that were opened".to_string(),
                    other => other.to_string(),
                };
                node.node_type = NodeType::Text;
                node.target = None;
                node.text = unreadable_notice(&target, &reason);
                // A notice folded away behind a toggle is no notice.
                node.fold = Some(false);
                warnings.push(IncludeWarning {
                    node_id: node.id.clone(),
                    target,
                    reason,
                });
                continue;
            }
            Err(error) => return Err(error),
        };
        // Best-effort: an unreadable/relative-weirdness parent just leaves
        // `asset_base` unset, falling back to the canvas's own directory —
        // same graceful-degradation posture the rest of this crate takes
        // for display-only metadata.
        let asset_base = access.asset_base(&target_path);

        node.node_type = NodeType::Text;
        node.target = None;
        node.text = mdcanvas::shift_headings(&contents, node.level);
        node.asset_base = asset_base;
        node.plain_markdown_include = true;
    }

    Ok((
        Canvas {
            nodes,
            options: Vec::new(),
            artifact_root: base_path
                .parent()
                .unwrap_or_else(|| std::path::Path::new("."))
                .to_path_buf(),
            artifact_values: canvas.artifact_values.clone(),
        },
        warnings,
    ))
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

pub fn list_includes_with_access(
    canvas: &Canvas,
    base_path: &Path,
    access: &dyn crate::file_source::FileSource,
) -> Vec<IncludeInfo> {
    let base_dir = base_path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));

    canvas
        .nodes
        .iter()
        .filter(|n| n.node_type == NodeType::Include)
        .filter_map(|n| {
            let target = n.target.clone()?;
            let path = access.confine(&base_dir, &target).ok()?;
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
    use crate::test_disk::DiskFiles;

    fn resolve(c: &Canvas, p: &Path) -> Result<Canvas, IncludeError> {
        resolve_with_access(c, p, &DiskFiles)
    }

    fn list_includes(c: &Canvas, p: &Path) -> Vec<IncludeInfo> {
        list_includes_with_access(c, p, &DiskFiles)
    }

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
        let tmp =
            std::env::temp_dir().join(format!("meshfox-include-test-{}", std::process::id() + 1));
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
        let tmp =
            std::env::temp_dir().join(format!("meshfox-include-test-{}", std::process::id() + 2));
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
        let tmp =
            std::env::temp_dir().join(format!("meshfox-list-includes-test-{}", std::process::id()));
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
