//! Builds the two temp HTML pages `meshfox pdf` prints to PDF (see the
//! parent module's own doc comment for the overall two-page design):
//! `document.html.tera`, the full node tree in flow order, and
//! `diagram.html.tera`, a spatial overview of every node at its own box,
//! always showing its full body (never folded, regardless of the
//! document's own fold settings — a printed page has no click to unfold
//! later) — its real, authored `x`/`y`/`width`/`height` when it has one, or
//! else auto-laid-out.
//!
//! The auto-layout itself is *not* computed here in Rust: since a canvas
//! box now always shows real body content (tables, code fences, long
//! paragraphs, ...), its height is genuinely content-dependent, and no
//! heuristic can guess that right in general — the exact problem that sank
//! this repo's own earlier Rust auto-layout attempt (`layout.rs`, see
//! `web/src/autolayout.ts`'s own module doc comment for that history).
//! `diagram.html.tera`'s own inline script ports that same algorithm to
//! JS instead, run inside the real browser that's already rendering this
//! page, measuring each box's actual rendered height the same way
//! `autolayout.ts` does client-side for the live canvas view. This module
//! only ships that script the raw tree data (`DiagramTreeNode`) to walk —
//! see `crate::pdf::browser::print_diagram` for the other half: reading the
//! script's own final computed page size back out of the browser before
//! printing, since Rust has no way to know it up front any more either.
//!
//! Both templates are embedded in the binary at compile time
//! (`include_str!`) — unlike `meshfox static`, `pdf` isn't meant to be
//! user-templatable, so there's no runtime template directory to load.

use meshfox_core::staticgen::{Asset, EdgeView, NodeView, Position, SiteData};
use serde::Serialize;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

const DOCUMENT_TEMPLATE: &str = include_str!("templates/document.html.tera");
const DOCUMENT_MACROS_TEMPLATE: &str = include_str!("templates/_document_macros.html.tera");
const DIAGRAM_TEMPLATE: &str = include_str!("templates/diagram.html.tera");

/// A fresh, unique temp directory for one `meshfox pdf` run's HTML pages
/// and copied assets — same pattern `crates/cli/tests/site_template_layout.rs`'s
/// own `unique_dir` uses.
pub fn temp_work_dir() -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!("meshfox-pdf-{}-{nanos}", std::process::id()))
}

/// Copies every local image `Asset` `staticgen::build` queued into
/// `work_dir`, at the same relative path its HTML `src` already points to —
/// exactly what `static_cmd` does for a real static-site export.
pub fn copy_assets(assets: &[Asset], work_dir: &Path) -> Result<(), String> {
    for asset in assets {
        let bytes =
            std::fs::read(&asset.source).map_err(|e| format!("{}: {e}", asset.source.display()))?;
        write_file(&work_dir.join(&asset.dest_rel), &bytes)?;
    }
    Ok(())
}

/// Self-hosted Fira Code — same font family the main web UI uses
/// (`web/src/main.tsx`'s own `@fontsource/fira-code` imports), and, unlike
/// an earlier version of this module, the *exact same embedded bytes*:
/// `web/dist` is already built into whatever binary links `meshfox-server`
/// (via `rust-embed`) for the web UI's own sake, so a second,
/// `include_bytes!`-embedded copy just for `pdf` was pure duplication
/// within the same compiled `meshfox` binary — `meshfox_server::
/// find_web_asset` (see its own doc comment) pulls the bytes back out of
/// that already-embedded bundle instead, prefix/suffix-matched since Vite
/// content-hashes every built asset's filename. Written into
/// `work_dir/fonts/` at a *fixed* name (both templates' own `@font-face
/// src: url(fonts/fira-code-latin-400-normal.woff2)` etc. need a stable
/// path, unlike the hashed source name) — silently skipped, one file at a
/// time, if a given subset+weight isn't found (the web UI hasn't been
/// built into this binary yet, or was built with a different set of
/// `@fontsource/fira-code` imports than expected): the affected
/// `@font-face` just never resolves, and the page falls through to its own
/// CSS fallback stack — not a hard failure, matching the same "gap
/// degrades gracefully" shape `crates/server`'s own `serve_embedded`
/// already uses for an unbuilt `web/dist`.
///
/// Only `latin` (English) and `cyrillic` (Russian) — the two this
/// project's own content actually mixes — at the two weights either pdf
/// template's own CSS sets (400/700); `latin-ext`/`cyrillic-ext` (Central/
/// Eastern-European accented Latin, historic/extended Cyrillic) aren't
/// needed here either, same reasoning `site-template/fonts/`'s own
/// (separately, loose-file) trim already applied.
const FONT_LOOKUPS: &[(&str, &str)] = &[
    (
        "fira-code-cyrillic-400-normal-",
        "fira-code-cyrillic-400-normal.woff2",
    ),
    (
        "fira-code-cyrillic-700-normal-",
        "fira-code-cyrillic-700-normal.woff2",
    ),
    (
        "fira-code-latin-400-normal-",
        "fira-code-latin-400-normal.woff2",
    ),
    (
        "fira-code-latin-700-normal-",
        "fira-code-latin-700-normal.woff2",
    ),
];

pub fn copy_fonts(work_dir: &Path) -> Result<(), String> {
    for (prefix, dest_name) in FONT_LOOKUPS {
        if let Some(bytes) = meshfox_server::find_web_asset(prefix, ".woff2") {
            write_file(&work_dir.join("fonts").join(dest_name), &bytes)?;
        }
    }
    Ok(())
}

/// Renders `document.html.tera` (the full tree, flow order) into
/// `work_dir`, returning its path.
pub fn write_document_page(site: &SiteData, work_dir: &Path) -> Result<PathBuf, String> {
    let mut tera = tera::Tera::default();
    tera.add_raw_templates([
        ("_document_macros.html.tera", DOCUMENT_MACROS_TEMPLATE),
        ("document.html.tera", DOCUMENT_TEMPLATE),
    ])
    .map_err(|e| format!("pdf document template: {e}"))?;

    let mut context = tera::Context::new();
    context.insert("site", site);
    let html = tera
        .render("document.html.tera", &context)
        .map_err(|e| format!("pdf document template: {e}"))?;

    let path = work_dir.join("document.html");
    write_file(&path, html.as_bytes())?;
    Ok(path)
}

/// Renders `diagram.html.tera` into `work_dir`, returning its path. The
/// page's own layout/sizing happens client-side (see the module doc
/// comment) — this only ships the raw tree + edges as JSON for that script
/// to walk.
pub fn write_diagram_page(site: &SiteData, work_dir: &Path) -> Result<PathBuf, String> {
    let tree = build_diagram_tree(&site.root);

    let mut node_ids: HashSet<&str> = HashSet::new();
    collect_ids(&site.root, &mut node_ids);
    let mut edges: Vec<DiagramEdge> = Vec::new();
    collect_structural_edges(&site.root, &mut edges);
    edges.extend(cross_edges(&site.edges, &node_ids));
    for edge in &mut edges {
        if let Some(resolved) = edge.color.as_deref().and_then(resolve_color_hex) {
            edge.color = Some(resolved);
        }
    }

    let mut tera = tera::Tera::default();
    tera.add_raw_template("diagram.html.tera", DIAGRAM_TEMPLATE)
        .map_err(|e| format!("pdf diagram template: {e}"))?;

    let mut context = tera::Context::new();
    context.insert("tree", &tree);
    context.insert("edges", &edges);
    let html = tera
        .render("diagram.html.tera", &context)
        .map_err(|e| format!("pdf diagram template: {e}"))?;

    let path = work_dir.join("diagram.html");
    write_file(&path, html.as_bytes())?;
    Ok(path)
}

/// One node's own data for the canvas page's client-side layout script —
/// everything it needs to create the box, size/position it (real position
/// when authored, else the script's own auto-layout), and recurse into its
/// children. Always carries the node's full `html_body` regardless of the
/// document's own fold state (`NodeView::folded`) — unlike the document
/// page, there's no reader interaction to defer content behind here, so
/// nothing is ever shown collapsed.
#[derive(Debug, Clone, Serialize)]
struct DiagramTreeNode {
    id: String,
    title: String,
    tags: Vec<String>,
    node_type: &'static str,
    html_body: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    border_color: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    position: Option<Position>,
    children: Vec<DiagramTreeNode>,
}

fn build_diagram_tree(node: &NodeView) -> DiagramTreeNode {
    DiagramTreeNode {
        id: node.id.clone(),
        title: node.title.clone(),
        tags: node.tags.clone(),
        node_type: node.node_type,
        // A `group` never has a body (same rule `staticgen`/the document
        // page follow) — nothing for the script to show there either way.
        html_body: if node.node_type == "group" {
            String::new()
        } else {
            node.html_body.clone()
        },
        border_color: node.color.as_deref().and_then(resolve_color_hex),
        position: node.position,
        children: node.children.iter().map(build_diagram_tree).collect(),
    }
}

fn collect_ids<'a>(node: &'a NodeView, out: &mut HashSet<&'a str>) {
    out.insert(node.id.as_str());
    for child in &node.children {
        collect_ids(child, out);
    }
}

/// A node/edge's `color` attribute is either one of JSON Canvas's six
/// numbered presets or a literal `#rrggbb` hex string — same convention
/// `web/src/MeshNode.tsx`'s `resolveNodeColor`/`COLOR_PRESETS` uses, same
/// palette values. `None` for anything else (malformed input shouldn't fail
/// the whole export — it just renders with no explicit color).
fn resolve_color_hex(color: &str) -> Option<String> {
    const PRESETS: [(&str, &str); 6] = [
        ("1", "#c22b2b"),
        ("2", "#d9822b"),
        ("3", "#d9c02b"),
        ("4", "#3d9e4f"),
        ("5", "#3d6ef5"),
        ("6", "#a05dd1"),
    ];
    if let Some((_, hex)) = PRESETS.iter().find(|(preset, _)| *preset == color) {
        return Some((*hex).to_string());
    }
    let hex = color.strip_prefix('#')?;
    (hex.len() == 6 && hex.chars().all(|c| c.is_ascii_hexdigit())).then(|| color.to_string())
}

fn write_file(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("failed to create {}: {e}", parent.display()))?;
    }
    std::fs::write(path, bytes).map_err(|e| format!("failed to write {}: {e}", path.display()))
}

/// A connector for the diagram page's own SVG overlay script — structural
/// (parent -> child) and `meshfox:edge` cross-reference connectors are both
/// flattened into this one shape so the diagram page's inline script (a
/// close relative of `site-template/index.html.tera`'s own `drawEdges()`)
/// can draw both the same way, rather than needing two separate code paths.
#[derive(Debug, Clone, Serialize)]
struct DiagramEdge {
    from: String,
    to: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    color: Option<String>,
    style: &'static str,
    arrow_end: bool,
    /// `true` for a structural (parent -> child) connector, `false` for a
    /// `meshfox:edge` cross-reference — an explicit flag rather than
    /// inferring it from `style`, since a `meshfox:edge` can itself declare
    /// `style="solid"` (structural edges always do, but aren't the only
    /// ones that can). The diagram page's own script uses this to pick
    /// which routing/curve rules apply — see `web/src/App.tsx`'s own
    /// `type: "tree"` vs `type: "extra"` edge kinds, which this mirrors.
    structural: bool,
}

/// Every parent -> child connector — walks `NodeView::children` directly
/// (there's no flat structural-edge list; see `staticgen::SiteData::edges`'s
/// own doc comment).
fn collect_structural_edges(node: &NodeView, out: &mut Vec<DiagramEdge>) {
    for child in &node.children {
        // A group already shows containment spatially (its members render
        // inside its own box) — a connector on top of that would just be
        // clutter, and actively confusing, since a group's own box spans
        // well past any single member's edge. Mirrors `web/src/tree.ts`'s
        // `deriveEdges`'s own `isGroupParent` check.
        if node.node_type != "group" {
            out.push(DiagramEdge {
                from: node.id.clone(),
                to: child.id.clone(),
                label: None,
                color: None,
                style: "solid",
                arrow_end: true,
                structural: true,
            });
        }
        collect_structural_edges(child, out);
    }
}

/// A `meshfox:edge` cross-reference, kept only when both `from`/`to`
/// resolve to a node that actually exists in the tree — defensive (every id
/// a `meshfox:edge` can legally name already resolves to a real node by the
/// time a canvas parses; see `mdcanvas`).
fn cross_edges(edges: &[EdgeView], node_ids: &HashSet<&str>) -> Vec<DiagramEdge> {
    edges
        .iter()
        .filter(|e| node_ids.contains(e.from.as_str()) && node_ids.contains(e.to.as_str()))
        .map(|e| DiagramEdge {
            from: e.from.clone(),
            to: e.to.clone(),
            label: e.label.clone(),
            color: e.color.clone(),
            style: e.style,
            arrow_end: e.arrow_end,
            structural: false,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pos(x: f64, y: f64, w: f64, h: f64) -> Position {
        Position {
            x,
            y,
            width: w,
            height: h,
        }
    }

    fn node(
        id: &str,
        node_type: &'static str,
        position: Option<Position>,
        children: Vec<NodeView>,
    ) -> NodeView {
        NodeView {
            id: id.into(),
            title: id.into(),
            level: 1,
            depth: 0,
            node_type,
            position,
            color: None,
            tags: vec![],
            html_body: String::new(),
            target: None,
            foldable: false,
            folded: false,
            children,
        }
    }

    fn text_node(id: &str, position: Option<Position>, children: Vec<NodeView>) -> NodeView {
        node(id, "text", position, children)
    }

    #[test]
    fn the_diagram_tree_carries_every_node_and_its_real_position() {
        let root = text_node(
            "root",
            None,
            vec![text_node("a", Some(pos(500.0, 500.0, 90.0, 40.0)), vec![])],
        );
        let tree = build_diagram_tree(&root);
        assert_eq!(tree.id, "root");
        assert!(tree.position.is_none());
        assert_eq!(tree.children.len(), 1);
        let a = &tree.children[0];
        assert_eq!(a.id, "a");
        let a_pos = a.position.expect("a has a real position");
        assert_eq!(
            (a_pos.x, a_pos.y, a_pos.width, a_pos.height),
            (500.0, 500.0, 90.0, 40.0)
        );
    }

    #[test]
    fn the_diagram_tree_always_carries_full_body_regardless_of_fold_state() {
        let mut root = text_node("root", None, vec![]);
        root.html_body = "<p>full body text</p>".to_string();
        root.folded = true; // fold state must never hide anything here
        let tree = build_diagram_tree(&root);
        assert_eq!(tree.html_body, "<p>full body text</p>");
    }

    #[test]
    fn a_group_carries_no_body_even_if_the_source_node_somehow_has_one() {
        let mut group = node("frame", "group", None, vec![]);
        group.html_body = "should never appear".to_string();
        let tree = build_diagram_tree(&group);
        assert_eq!(tree.html_body, "");
    }

    #[test]
    fn structural_edges_cover_every_parent_child_pair() {
        let root = text_node(
            "root",
            None,
            vec![text_node("a", None, vec![]), text_node("b", None, vec![])],
        );
        let mut edges = Vec::new();
        collect_structural_edges(&root, &mut edges);
        assert_eq!(edges.len(), 2);
    }

    #[test]
    fn a_group_to_its_own_member_gets_no_structural_edge() {
        // Mirrors `web/src/tree.ts`'s own `isGroupParent` suppression: a
        // group already shows containment spatially, so a connector line
        // on top of it would just be clutter.
        let group = node(
            "frame",
            "group",
            None,
            vec![text_node("member", None, vec![])],
        );
        let root = text_node("root", None, vec![group]);
        let mut edges = Vec::new();
        collect_structural_edges(&root, &mut edges);
        // root -> frame survives (root isn't a group); frame -> member does not.
        assert_eq!(edges.len(), 1);
        assert_eq!(
            (edges[0].from.as_str(), edges[0].to.as_str()),
            ("root", "frame")
        );
    }

    #[test]
    fn cross_edge_is_dropped_when_either_end_is_unknown() {
        let node_ids: HashSet<&str> = ["a", "b"].into_iter().collect();
        let edges = vec![
            EdgeView {
                from: "a".into(),
                to: "b".into(),
                label: None,
                color: None,
                style: "dashed",
                arrow_end: true,
            },
            EdgeView {
                from: "a".into(),
                to: "c".into(),
                label: None,
                color: None,
                style: "dashed",
                arrow_end: true,
            },
        ];
        let out = cross_edges(&edges, &node_ids);
        assert_eq!(out.len(), 1);
        assert_eq!((out[0].from.as_str(), out[0].to.as_str()), ("a", "b"));
    }

    #[test]
    fn diagram_page_renders_the_full_tree_with_real_body_content() {
        let mut a = text_node("a", None, vec![]);
        a.html_body = "<p>hello from a</p>".to_string();
        let root = text_node("root", None, vec![a]);
        let site = SiteData {
            title: "t".into(),
            root,
            edges: vec![],
        };
        let dir =
            std::env::temp_dir().join(format!("meshfox-pdf-render-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        let path = write_diagram_page(&site, &dir).unwrap();
        let html = std::fs::read_to_string(&path).unwrap();
        assert!(html.contains("hello from a"), "{html}");
        assert!(html.contains("\"id\":\"root\""), "{html}");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn document_page_links_a_parent_to_each_child_with_no_indent_wrapper() {
        let child = text_node("child", None, vec![]);
        let root = text_node("root", None, vec![child]);
        let site = SiteData {
            title: "t".into(),
            root,
            edges: vec![],
        };
        let dir = std::env::temp_dir().join(format!(
            "meshfox-pdf-render-doc-test-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();

        let path = write_document_page(&site, &dir).unwrap();
        let html = std::fs::read_to_string(&path).unwrap();

        assert!(html.contains("id=\"node-root\""), "{html}");
        assert!(html.contains("id=\"node-child\""), "{html}");
        assert!(
            html.contains("href=\"#node-child\""),
            "the parent must link to its child: {html}"
        );
        assert!(
            !html.contains("class=\"node-children\""),
            "the old indent/left-border wrapper must be gone: {html}"
        );

        std::fs::remove_dir_all(&dir).ok();
    }
}
