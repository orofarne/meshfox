//! The read pipeline: raw `.canvas.md` text in, the fully annotated
//! [`Canvas`] a viewer needs out — what `GET /api/canvas` serves, minus the
//! HTTP. Everything it needs from outside comes through one [`FileSource`].
//!
//! The passes run in a fixed order, and [`annotate_resolved`] is the shared
//! tail of it so an embedder that resolves `include`s its own way (the
//! native server builds a different file access before and after splicing
//! them in) still gets exactly the same annotations.

use crate::body_rev::annotate_body_revs;
use crate::canvas::Canvas;
use crate::file_source::FileSource;
use crate::include::{self, IncludeError};
use crate::mdcanvas::{self, ParseError};
use crate::options::declared_options;
use crate::tag_colors::annotate_effective_colors;
use std::path::Path;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum RenderError {
    #[error(transparent)]
    Parse(#[from] ParseError),
    #[error(transparent)]
    Include(#[from] IncludeError),
}

/// The directory a canvas's `file`/`include` targets resolve against: its
/// own, or `.` for a bare file name.
pub fn canvas_root_dir(canvas_path: &Path) -> &Path {
    canvas_path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
}

/// Parses `raw`, splices in every `include`, and annotates the result (see
/// [`annotate_resolved`]). `canvas_path` is where `raw` came from, used only
/// to resolve relative targets; `files` is how any of them are read.
pub fn render_canvas(
    raw: &str,
    canvas_path: &Path,
    files: &dyn FileSource,
) -> Result<Canvas, RenderError> {
    let parsed = mdcanvas::parse(raw)?;
    let mut canvas = include::resolve_with_access(&parsed, canvas_path, files)?;
    // A runnable file node's block fingerprint covers its target's content.
    canvas.hydrate_target_digests_with(canvas_path, files);
    annotate_resolved(&mut canvas, canvas_root_dir(canvas_path), files);
    Ok(canvas)
}

/// The annotation passes over an already include-resolved canvas, in order:
/// constraint pass/fail badges, computed Starlark views, declared options,
/// tag-derived colors, body revisions.
pub fn annotate_resolved(canvas: &mut Canvas, base_dir: &Path, files: &dyn FileSource) {
    // Every embedded constraint fence's script itself is cheap and pure
    // (tick/heap/callstack-bounded — see `constraint::evaluate`); the only
    // I/O it can trigger is a `file`-type node's own already-declared
    // target, read through `files` and capped the same way the
    // `display="code"` preview is. Running every constraint on every fetch
    // keeps a viewer's pass/fail badges current without a separate
    // endpoint or a stale cache to invalidate.
    crate::constraint::annotate_status_with_access(canvas, Some(base_dir), files);
    crate::starlark_render::annotate(canvas, Some(base_dir), files);
    // Best-effort: a malformed `meshfox:option` declaration shouldn't break
    // *viewing* the canvas (falls back to no options declared, same as if
    // there were none at all) — validation is what surfaces that loudly,
    // the same split `vars`/constraint fences already have between "parses
    // enough to view" and "fully valid".
    canvas.options = declared_options(canvas).unwrap_or_default();
    // Same best-effort split for `meshfox:tag-color` — a malformed
    // declaration just means no node falls back to a tag-derived color,
    // not a broken canvas view.
    annotate_effective_colors(canvas);
    annotate_body_revs(canvas);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_disk::DiskFiles;
    use std::fs;
    use std::path::PathBuf;

    fn tmp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("mfx-render-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir.canonicalize().unwrap()
    }

    const DOC: &str = concat!(
        "# Root\n<!-- meshfox:node id=\"root\" -->\n",
        "<!-- meshfox:tag-color tag=\"bug\" color=\"1\" -->\n",
        "<!-- meshfox:option name=\"auto-timestamps\" -->\n\n",
        "```starlark constraint\nif len(doc.descendants()) < 1:\n    fail(\"no nodes\")\n```\n\n",
        "## Child\n<!-- meshfox:node id=\"child\" tags=\"bug\" -->\n\nbody\n",
    );

    #[test]
    fn every_pass_runs_on_a_plain_document() {
        let canvas = render_canvas(DOC, Path::new("x.canvas.md"), &DiskFiles).unwrap();
        let child = canvas.node("child").unwrap();
        assert_eq!(child.effective_color.as_deref(), Some("1"));
        assert!(child.body_rev.is_some());
        assert_eq!(canvas.options, ["auto-timestamps"]);
        let root = canvas.node("root").unwrap();
        assert_eq!(root.constraint_results.len(), 1);
        assert!(
            root.constraint_results[0].ok,
            "{:?}",
            root.constraint_results[0].messages
        );
    }

    #[test]
    fn includes_are_spliced_in_through_the_file_source() {
        let dir = tmp("include");
        fs::write(dir.join("notes.md"), "# Notes\n\nhello from disk\n").unwrap();
        let raw = concat!(
            "# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
            "## Notes\n<!-- meshfox:node id=\"notes\" type=\"include\" -->\n\n[notes](./notes.md)\n",
        );
        let canvas = render_canvas(raw, &dir.join("x.canvas.md"), &DiskFiles).unwrap();
        let notes = canvas.node("notes").unwrap();
        assert!(notes.text.contains("hello from disk"));
        assert!(notes.plain_markdown_include);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_include_target_is_an_include_error() {
        let dir = tmp("missing");
        let raw = concat!(
            "# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
            "## Gone\n<!-- meshfox:node id=\"gone\" type=\"include\" -->\n\n[gone](./gone.md)\n",
        );
        let err = render_canvas(raw, &dir.join("x.canvas.md"), &DiskFiles).unwrap_err();
        assert!(matches!(err, RenderError::Include(_)), "{err}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_document_without_a_root_is_a_parse_error() {
        let err = render_canvas("no heading\n", Path::new("x.canvas.md"), &DiskFiles).unwrap_err();
        assert!(matches!(err, RenderError::Parse(_)), "{err}");
    }
}
