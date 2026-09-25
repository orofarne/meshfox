//! Pure `Canvas` -> static-site data transform: turns a parsed (and, if the
//! caller wants includes spliced in, already `include::resolve`d) canvas
//! into a plain, serializable `SiteData` a template can render — a
//! recursive tree mirroring the canvas's own node nesting (`SiteData.root`,
//! `NodeView.children`), every node's Markdown body already turned into
//! HTML.
//!
//! No I/O and no templating happens here (that's `meshfox_cli`'s job, via
//! Tera) — this module only computes data, so it can be unit-tested.
//!
//! Deliberately **not** a port of `layout.rs`/`web/src/autolayout.ts`: an
//! earlier version of this module pre-computed an x/y/width/height for
//! every node (estimating a Markdown body's rendered height from its raw
//! source text) the same way those two do, for the same "canvas" look —
//! but no heuristic can get a text-height guess right in general, and a
//! wrong one means a node's box just comes out the wrong size, either
//! cutting content off or leaving dead space below it. A browser already
//! does real layout for free, so a node with no *real*, authored
//! `x`/`y`/`width`/`height` in the source file now gets none of those
//! fields at all here (`NodeView.position` is `None`) — the template
//! renders it as an ordinary nested HTML element and lets CSS flow lay it
//! out and size it from its actual content. A node that *does* have all
//! four real values keeps them exactly (`NodeView.position` is `Some`) —
//! that's the author's own explicit canvas layout, untouched.
//!
//! `site-template/`'s own rendering strategy layers on top of this plain
//! tree (not computed here — it's pure CSS, no measurement needed): root's
//! own row stacks its direct children (depth 0/1) in one column with only a
//! small nudge per level, same document-outline convention
//! `web/src/autolayout.ts` calls `ROOT_CHILD_INDENT`; every deeper node
//! renders its own children beside itself in a flex row instead, branching
//! right the way `autolayout.ts`'s `placeRightward` does — but as plain CSS
//! flow, not a JS-measured position. `NodeView.depth` (real tree depth, not
//! to be confused with `level`, the Markdown heading level) is what lets the
//! template's CSS tell root/depth-1 (stacked below, small nudge) apart from
//! depth ≥2 (branches right of its own real parent).
//!
//! Every structural (`parent`→child) connector is drawn by that same CSS
//! (a border-based "twig and spine" pair of pseudo-elements — see
//! `site-template/style.css`), following the DOM nesting directly, so no
//! data about it needs to leave this module at all. Only `meshfox:edge`
//! cross-references — which can point anywhere, not just to a DOM sibling —
//! still need real endpoints from a browser and get drawn by a small JS pass
//! instead; `SiteData.edges` carries only those (`build_edges`).
//!
//! Local-file references (`build`'s `canvas_dir`/`links_base_url` parameters) get
//! three different treatments, matching how the web UI already resolves
//! them (`crates/server/src/lib.rs`'s `serve_canvas_relative_file`/
//! `get_node_file_content`) or, where a live server has no static
//! equivalent, the closest static substitute:
//!   - a Markdown `![image](relative/path)` is read from disk (confined to
//!     `canvas_dir`, same boundary check the server uses) and queued as an
//!     `Asset` for the caller to copy alongside the rendered HTML; its `src`
//!     is rewritten to that copy's path. This is what a browser hitting the
//!     live server got for free by just resolving the URL against the
//!     page — a static export has to bring the bytes with it instead.
//!   - a `file`-type node's `display="code"` preview — client-side
//!     `FileCodePreview.tsx`, fetching `GET /api/nodes/:id/file-content` —
//!     has no live route to fetch from once static, so its target's content
//!     is read once at build time and inlined directly into the HTML
//!     (same confinement boundary, same binary-sniff/size-cap as the
//!     server route — see `render_file_code`).
//!   - anything else relative (a plain Markdown link, or a `file`/`link`
//!     node's own target when *not* `display="code"`) is left untouched
//!     unless `links_base_url` is set, in which case it's prefixed with it —
//!     these were never resolved through the canvas's own directory to
//!     begin with (an ordinary link can point anywhere, including outside
//!     `canvas_dir`), so there's nothing here to copy; `links_base_url` is the
//!     escape hatch for "this needs to resolve against wherever the source
//!     lives" instead — distinct from the site's own `base_url`
//!     (`TemplateConfig::base_url`/`--sitemap`'s own `<loc>` prefix in
//!     `crates/cli/src/main.rs`, which never reaches this module at all):
//!     the two can differ, e.g. this repo's own canvases publish as plain
//!     Markdown on GitHub, not alongside the static site built from them.

use crate::canvas::{ArrowEnd, Canvas, FileDisplay, Node, NodeType};
use serde::Serialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize)]
pub struct SiteData {
    pub title: String,
    /// The canvas's own node tree — always exactly one root (a parsed
    /// `Canvas` guarantees this; see `mdcanvas::parse`'s "single root"
    /// check).
    pub root: NodeView,
    /// Every `meshfox:edge` cross-reference — see the module doc comment
    /// and `build_edges`. Structural (`parent`→child) edges aren't included
    /// here at all: they're drawn by pure CSS straight from `NodeView.
    /// children`'s own DOM nesting, so there's nothing for this list to add.
    pub edges: Vec<EdgeView>,
}

impl SiteData {
    /// Depth-first search for `id` in the tree — mainly for tests, but a
    /// generically reasonable thing for a caller to want too (this module
    /// doesn't keep a flat id->node index anywhere else).
    pub fn find(&self, id: &str) -> Option<&NodeView> {
        self.root.find(id)
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct NodeView {
    pub id: String,
    pub title: String,
    pub level: u8,
    /// Real tree depth (root is `0`), computed from the canvas's actual
    /// `parent` links — not the same as `level` above, which is only the
    /// Markdown heading level and can diverge from tree depth (an explicit
    /// `parent=` attribute can reparent a node past the heading ceiling; see
    /// `mdcanvas`'s `insert_child_node_past_heading_ceiling*` tests). Lets
    /// the template/JS tell "root or its direct children" (depth ≤1, plain
    /// CSS nesting, no repositioning) apart from "real rightward branching"
    /// (depth ≥2, measured and repositioned by JS) — see the module doc
    /// comment.
    pub depth: u32,
    /// `"text"` | `"file"` | `"link"` | `"group"` | `"include"` — see
    /// `NodeType::as_str`. In practice `"include"` never appears in a
    /// `SiteData` built from an already `include::resolve`d canvas (an
    /// include node becomes a `group` or `text` node once resolved — see
    /// `crate::include`'s module docs) — this module doesn't resolve
    /// includes itself, so it's left as a possible value for a caller that
    /// passes a raw, unresolved canvas.
    pub node_type: &'static str,
    /// `Some` only when the source had all four of `x`/`y`/`width`/
    /// `height` — the author's own explicit canvas layout, rendered at
    /// exactly those pixels. `None` (the common case) means: no inline
    /// position/size at all, render as an ordinary flowed element and let
    /// the browser size it from its real content. See the module doc
    /// comment.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub position: Option<Position>,
    /// The node's own explicit `color`, or a fallback derived from its
    /// tags against the document's `meshfox:tag-color` defaults — whatever
    /// `node.effective_color` already resolved to (`crate::tag_colors`).
    /// `None` when neither applies, or the caller never annotated it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    pub tags: Vec<String>,
    /// `node.text` rendered to HTML (GFM-flavored Markdown, meshfox's own
    /// fence attributes stripped down to a bare language token first — see
    /// `crate::fence::strip_fence_attrs`). Always empty for a `group` node,
    /// which never has a body.
    pub html_body: String,
    /// `file`/`link` node target (path or URL); `None` for every other type.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    /// Whether folding could hide anything about this node at all — a
    /// title-only node (see `is_title_only_node`) with no children has no
    /// body to hide and no subtree to collapse, so the template renders it
    /// as a plain element with no disclosure control. Every other node
    /// (real body content, or at least one child) is always `true` here
    /// regardless of its own `folded` state. Mirrors `web/src/App.tsx`'s
    /// `canFold`.
    pub foldable: bool,
    /// This node's default fold state on first render — `true` starts its
    /// `<details>` closed (see `site-template/_macros.html.tera`), hiding
    /// its own body and its whole `children` subtree until a reader clicks
    /// it open; purely a client-side starting point; a reader's own
    /// clicks aren't written back here (there's no build step to write
    /// them back into). Always `false` when `foldable` is `false` — see
    /// that field's own doc comment. Mirrors `web/src/App.tsx`'s
    /// `resolveDefaultFold`, computed once for the whole tree by
    /// `resolve_default_fold`.
    pub folded: bool,
    /// This node's direct structural children, in document order — the
    /// whole tree hangs off `SiteData.root` through this field; a template
    /// walks it with a recursive macro (see `site-template/`).
    pub children: Vec<NodeView>,
}

impl NodeView {
    /// Depth-first search for `id` in this node or any descendant — see
    /// `SiteData::find`, which this backs.
    pub fn find(&self, id: &str) -> Option<&NodeView> {
        if self.id == id {
            return Some(self);
        }
        self.children.iter().find_map(|c| c.find(id))
    }
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct Position {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// A `meshfox:edge` cross-reference — see the module doc comment for why
/// structural (`parent`→child) edges have no equivalent here.
#[derive(Debug, Clone, Serialize)]
pub struct EdgeView {
    pub from: String,
    pub to: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    pub style: &'static str,
    pub arrow_end: bool,
}

/// A local file `build` read off disk that the caller (`meshfox_cli`) needs
/// to copy alongside the rendered HTML for a relative image reference to
/// actually resolve — see the module doc comment. Not part of `SiteData`:
/// a `PathBuf` doesn't serialize usefully for Tera, and no template needs
/// to enumerate these itself, only the CLI's own file-copy step does.
#[derive(Debug, Clone)]
pub struct Asset {
    /// Absolute, canonicalized source path — confined to `canvas_dir` for
    /// an ordinary node, or to that node's own `asset_base` for one spliced
    /// in from an `include` target elsewhere on disk (see `include_asset`).
    pub source: PathBuf,
    /// Destination path relative to the site's output root, using forward
    /// slashes regardless of host OS — also what the rendered HTML's `src`
    /// now points to.
    pub dest_rel: String,
    /// Set only when `source` doesn't live under `canvas_dir` at all — an
    /// image referenced from inside an `include`-dumped body whose own
    /// directory (`Node::asset_base`) lies outside the primary canvas's own
    /// directory (`dest_rel` then carries a `_include-assets/`-namespaced
    /// path instead of a plain canvas-relative one — see
    /// `resolve_image_url`). `(dir, file)` is exactly the query-param pair
    /// the worker's own confined `GET /api/include-asset?dir=&file=` route
    /// expects (`crates/server/src/lib.rs::get_include_asset`) — the *only*
    /// worker route that can serve it, since the ordinary canvas-relative
    /// fallback route is confined to `canvas_dir` and 404s (well, falls
    /// through to the embedded web UI's own SPA shell — a 200, not a 404,
    /// so a worker-routed caller can't tell from the status code alone)
    /// for anything outside it. `dir` is the node's own `asset_base`
    /// string exactly as received, unmodified — the server's own `known`
    /// check there is a plain string `==` against every node's
    /// `asset_base`, so re-deriving/re-canonicalizing it client-side risks
    /// a canonically-equal-but-not-byte-identical mismatch.
    pub include_asset: Option<(String, String)>,
}

/// Builds a `SiteData` from `canvas` (its node tree, each node's body
/// rendered to HTML), plus every `Asset` (local image) it needs copied
/// alongside the rendered HTML.
///
/// `canvas_dir` is the directory the canvas file itself lives in — the same
/// root a relative image/file reference resolves against, confined the same
/// way the server confines it (see module docs). `links_base_url`, if set, is
/// prefixed onto whatever relative reference is left over (see module
/// docs) — pass `None` for a self-contained site meant to be opened as-is.
pub fn build(canvas: &Canvas, canvas_dir: &Path, links_base_url: Option<&str>) -> (SiteData, Vec<Asset>) {
    build_with_code_previews(canvas, canvas_dir, links_base_url, None)
}

/// Same as `build`, but every `display="code"` file-node's content is taken
/// from `code_previews` (keyed by node id) instead of read off local disk —
/// see `CodePreview`/`RenderCtx::code_previews`. Pass `None` for exactly
/// `build`'s own behavior (local disk, `crate::file_read::preview`); pass
/// `Some(map)` once every such node's content has already been fetched
/// through a worker (`meshfox static`'s own worker-routed path) — a node
/// with no entry in `map` renders the same fallback link `file_read::preview`
/// returning `Err` produces locally, never a silent disk read.
pub fn build_with_code_previews(
    canvas: &Canvas,
    canvas_dir: &Path,
    links_base_url: Option<&str>,
    code_previews: Option<&std::collections::HashMap<String, CodePreview>>,
) -> (SiteData, Vec<Asset>) {
    // `copy_files`/`recursive` both `false` means `root_canvas_path`/`""`
    // (the root's own slot) are never actually consulted (no `file`-node
    // target is ever treated as a possible canvas link at all — see
    // `build_node_view`'s own branch on `ctx.copy_files`), so any value
    // here is equally inert — `canvas_dir` itself is as good as any.
    let root_canvas_path = canvas_dir.to_path_buf();
    let (site, assets, _canvas_links) = build_for_static_export(
        canvas,
        canvas_dir,
        &root_canvas_path,
        "",
        links_base_url,
        code_previews,
        false,
        false,
    )
    .unwrap_or_else(|errors| {
        unreachable!("copy_files=false never produces a CopyFilesTargetError, got {errors:?}")
    });
    (site, assets)
}

/// Same as `build_with_code_previews`, but also handles `--copy-files`
/// (`copy_files: true`) and, on top of that, `--recursive` (`recursive:
/// true`, meaningless unless `copy_files` is also `true` — enforced by the
/// caller, not here):
///
/// - `copy_files: true` alone: a `file`-node's own `target` (never touched
///   by `build`/`build_with_code_previews` — see the module doc comment's
///   "Local-file references" section) is copied alongside the site the same
///   way a Markdown image already is. A target that resolves to a
///   `.canvas.md` file fails the whole build instead — every offending node
///   collected into the returned `Err` (see `CopyFilesTargetError`) — since
///   copying an inert canvas source file into `--out` wouldn't give a reader
///   a rendered page.
/// - `recursive: true` on top of that: a `.canvas.md` target is no longer an
///   error — its link is rewritten to point at that canvas's own rendered
///   page instead (a relative path computed from `current_slot`, this
///   canvas's own position in the overall `--out` tree, to that target's own
///   position — see `resolve_file_target_for_copy`/`canvas_link_subdir`),
///   and the target itself is queued into the returned `Vec<CanvasLinkTarget>`
///   for the caller to actually go build+render — this function only ever
///   handles one canvas at a time, it doesn't recurse itself (no I/O here at
///   all — see the module doc comment).
///
/// `root_canvas_path` is the *original* canvas the whole export started
/// from (its file, not just its directory) — fixed for every call across a
/// whole recursive export, distinct from `canvas_dir` (*this* call's own
/// canvas's directory, which differs from the root's for every nested
/// canvas). `current_slot` is this canvas's own position in the `--out`
/// tree relative to its root (`""` for the root canvas itself, otherwise
/// whatever `CanvasLinkTarget::subdir` the *caller* discovered this canvas
/// under) — both only matter when `recursive` is `true`.
///
/// `copy_files: false` is exactly `build_with_code_previews` (an empty
/// `Vec<CanvasLinkTarget>`, and can never itself produce an `Err`).
#[allow(clippy::too_many_arguments)]
pub fn build_for_static_export(
    canvas: &Canvas,
    canvas_dir: &Path,
    root_canvas_path: &Path,
    current_slot: &str,
    links_base_url: Option<&str>,
    code_previews: Option<&std::collections::HashMap<String, CodePreview>>,
    copy_files: bool,
    recursive: bool,
) -> Result<(SiteData, Vec<Asset>, Vec<CanvasLinkTarget>), Vec<CopyFilesTargetError>> {
    let canvas_dir = canvas_dir
        .canonicalize()
        .unwrap_or_else(|_| canvas_dir.to_path_buf());
    let root_canvas_path = root_canvas_path
        .canonicalize()
        .unwrap_or_else(|_| root_canvas_path.to_path_buf());

    let root_node = canvas
        .nodes
        .iter()
        .find(|n| n.parent.is_none())
        .expect("a parsed canvas always has a root");
    let (folded_ids, foldable_ids) = resolve_default_fold(canvas, &root_node.id);
    let tag_colors = crate::tag_colors::declared_tag_colors(canvas).unwrap_or_default();
    let ctx = RenderCtx {
        canvas_dir: &canvas_dir,
        root_canvas_path: &root_canvas_path,
        current_slot,
        links_base_url,
        folded_ids: &folded_ids,
        foldable_ids: &foldable_ids,
        tag_colors: &tag_colors,
        code_previews,
        copy_files,
        recursive,
    };

    let mut assets: Vec<Asset> = Vec::new();
    let mut errors: Vec<CopyFilesTargetError> = Vec::new();
    let mut canvas_links: Vec<CanvasLinkTarget> = Vec::new();
    let root = build_node_view(
        canvas,
        root_node,
        0,
        &ctx,
        &mut assets,
        &mut errors,
        &mut canvas_links,
    );
    if !errors.is_empty() {
        return Err(errors);
    }
    let edges = build_edges(canvas);
    let title = root.title.clone();

    // Same image referenced by more than one node (or more than once in the
    // same body) would otherwise queue a redundant copy — keep the first.
    let mut seen = std::collections::HashSet::new();
    assets.retain(|a| seen.insert(a.dest_rel.clone()));

    Ok((SiteData { title, root, edges }, assets, canvas_links))
}

fn build_node_view(
    canvas: &Canvas,
    node: &Node,
    depth: u32,
    ctx: &RenderCtx,
    assets: &mut Vec<Asset>,
    errors: &mut Vec<CopyFilesTargetError>,
    canvas_links: &mut Vec<CanvasLinkTarget>,
) -> NodeView {
    // The directory this node's own relative references (Markdown images
    // in its body, its caption) resolve against — `asset_base` when this
    // node's body actually came from an `include` target living elsewhere
    // on disk, `ctx.canvas_dir` otherwise. See `Node::cwd`'s own doc
    // comment and this module's own doc comment's "Local-file references"
    // section — a `file`-node's own `target` (the `display="code"` branch
    // below) never has this problem: a `file` node can't itself live
    // inside an `include`-dumped body (`crate::include::resolve` only ever
    // produces plain `Text` nodes, never new addressable structure), so
    // `node.asset_base` is always `None` there and `cwd` reduces to
    // `ctx.canvas_dir` — nothing to thread through for `render_file_code`.
    let base_dir = node.cwd(ctx.canvas_dir);
    let (html_body, target) = if node.node_type == NodeType::Group {
        (String::new(), None)
    } else if node.node_type == NodeType::File && node.display == Some(FileDisplay::Code) {
        // `render_file_code` replaces the node's own body with the
        // target's file content — but an optional caption (see
        // `Node::caption`) is still part of that body, and still worth
        // showing. Above the preview, not below (unlike the plain-link
        // display mode, which gets its caption for free below the link by
        // rendering `node.text` — link line + caption — whole) — it reads
        // as a heading/intro for the file content, not a footnote on it.
        let mut html = String::new();
        if let Some(caption) = &node.caption {
            html.push_str(&render_markdown(
                caption,
                &base_dir,
                node.asset_base.as_deref(),
                ctx,
                assets,
                None,
            ));
        }
        html.push_str(&render_file_code(node, ctx));
        (html, node.target.clone())
    } else {
        // `--copy-files` only ever applies here, to a `file`-node whose
        // rendered body still carries a plain link to its target (the
        // `display="code"` branch above already inlined the content for
        // anything else, nothing left to copy for) — never to a `link`
        // node (an external/preview reference, not a local file the way a
        // `file` node's target is treated) or a plain Markdown link inside
        // `node.text` (`resolve_link_url`, unchanged — see the module doc
        // comment's own reasoning for why those stay lighter-touch).
        //
        // Computed *before* `render_markdown` below, not after: SPEC.md's
        // grammar guarantees a `file`/`link` node's body starts with
        // exactly one Markdown link, so this resolved value is also the
        // *visible* link a reader actually clicks (`html_body`'s own first
        // `<a href>`) — `render_markdown`'s `first_link_override` is what
        // wires the two together, rather than leaving `NodeView.target`
        // correctly rewritten while the rendered page's own link still
        // silently points at the old, unresolved target (a real bug this
        // was caught by hand, rendering this repo's own `README.md` with
        // `--copy-files --recursive`: every `--out` file existed, but
        // nothing on the page actually linked to any of them).
        let target = node.target.as_deref().map(|t| {
            if ctx.copy_files && node.node_type == NodeType::File {
                resolve_file_target_for_copy(
                    &node.id,
                    t,
                    &base_dir,
                    node.asset_base.as_deref(),
                    ctx,
                    assets,
                    errors,
                    canvas_links,
                )
            } else {
                resolve_link_url(t, ctx)
            }
        });
        let first_link_override =
            (node.node_type == NodeType::File && ctx.copy_files).then_some(target.as_deref()).flatten();
        let html_body = render_markdown(
            &node.text,
            &base_dir,
            node.asset_base.as_deref(),
            ctx,
            assets,
            first_link_override,
        );
        (html_body, target)
    };
    // `canvas.resolve_absolute_position` is exactly `(node.x, node.y)` for
    // any node not nested under a `group` — it's only for a group member
    // that this differs, resolving the member's own group-relative
    // coordinate against its group's anchor (see
    // `Canvas::resolve_absolute_position`). `None` (no real position, or a
    // group ancestor with no anchor of its own) falls back to CSS flow same
    // as before this existed — no heuristic invented here either way, per
    // this module's own "all real or nothing" rule (see the module doc
    // comment).
    let position = match (
        canvas.resolve_absolute_position(&node.id),
        node.width,
        node.height,
    ) {
        (Some((x, y)), Some(width), Some(height)) => Some(Position {
            x,
            y,
            width,
            height,
        }),
        _ => None,
    };
    let children = canvas
        .children(&node.id)
        .into_iter()
        .map(|c| build_node_view(canvas, c, depth + 1, ctx, assets, errors, canvas_links))
        .collect();

    NodeView {
        id: node.id.clone(),
        title: node.title.clone(),
        level: node.level,
        node_type: node.node_type.as_str(),
        depth,
        position,
        color: crate::tag_colors::effective_color(node, ctx.tag_colors).map(str::to_string),
        tags: node.tags.clone(),
        html_body,
        target,
        foldable: ctx.foldable_ids.contains(node.id.as_str()),
        folded: ctx.folded_ids.contains(node.id.as_str()),
        children,
    }
}

/// Whether `n` renders as an already-title-only, empty-bodied row — folding
/// a node like this never changes its own row (there was never a body to
/// hide), so it can only be worth folding for its subtree's sake (see
/// `resolve_default_fold`, which is what actually decides foldability from
/// this). Mirrors `web/src/App.tsx`'s `isTitleOnlyNode`.
fn is_title_only_node(n: &Node) -> bool {
    n.node_type == NodeType::Text && n.text.trim().is_empty()
}

/// The default fold state for every node in `canvas` on a static export's
/// first render — a direct Rust port of `web/src/App.tsx`'s
/// `resolveDefaultFold`/`canFold`, computed once here up front (rather than
/// per node during the `build_node_view` walk, which doesn't have the whole
/// tree's parent-links in scope) and consulted from `RenderCtx` as each
/// `NodeView` is built. Returns `(folded_ids, foldable_ids)` — see
/// `NodeView::folded`/`NodeView::foldable` for what each set backs.
///
/// A static export has no `localStorage` (there's no return visit to persist
/// a reader's own fold clicks across, and no build-time reason to guess at
/// one) — so unlike the web UI, this *is* the whole story: what a reader
/// sees the moment the page loads, and the same every time.
fn resolve_default_fold(
    canvas: &Canvas,
    root_id: &str,
) -> (
    std::collections::HashSet<String>,
    std::collections::HashSet<String>,
) {
    let has_unfold_option = crate::options::declared_options(canvas)
        .map(|opts| opts.iter().any(|o| o == "unfold"))
        .unwrap_or(false);
    // Ids that are somebody's structural `parent` — mirrors
    // `web/src/App.tsx`'s `nodesWithChildren`.
    let with_children: std::collections::HashSet<&str> = canvas
        .nodes
        .iter()
        .filter_map(|n| n.parent.as_deref())
        .collect();

    let mut folded = std::collections::HashSet::new();
    let mut foldable = std::collections::HashSet::new();
    for n in &canvas.nodes {
        let can_fold = !is_title_only_node(n) || with_children.contains(n.id.as_str());
        if can_fold {
            foldable.insert(n.id.clone());
        }
        let has_explicit_size = n.width.is_some() || n.height.is_some();
        let resolved = n.fold.unwrap_or_else(|| {
            n.id != root_id && !has_unfold_option && !has_explicit_size && can_fold
        });
        if resolved {
            folded.insert(n.id.clone());
        }
    }
    (folded, foldable)
}

/// Every `meshfox:edge` cross-reference — see the module doc comment.
/// Structural (`parent`→child) edges are deliberately excluded: they're
/// drawn by pure CSS directly from `NodeView.children`'s own DOM nesting,
/// with no real endpoints for anything here to compute.
fn build_edges(canvas: &Canvas) -> Vec<EdgeView> {
    let mut edges = Vec::new();
    for node in &canvas.nodes {
        for extra in &node.extra_parents {
            if extra.from == node.id {
                continue; // defensively skip a self-loop; shouldn't occur post-`validate`
            }
            edges.push(EdgeView {
                from: extra.from.clone(),
                to: node.id.clone(),
                label: extra.label.clone(),
                color: extra.color.clone(),
                style: extra.style.map(|s| s.as_str()).unwrap_or("dashed"),
                arrow_end: extra
                    .arrow_end
                    .map(|a| matches!(a, ArrowEnd::Arrow))
                    .unwrap_or(true),
            });
        }
    }
    edges
}

/// Per-`build` context threaded into rendering: where relative references
/// resolve from, and how to treat whatever's left un-copied. See module
/// docs.
struct RenderCtx<'a> {
    canvas_dir: &'a Path,
    /// The *original* canvas the whole export started from (its file, not
    /// just its directory) — fixed across a whole recursive export, unlike
    /// `canvas_dir` (this call's own canvas's directory). Only consulted
    /// when `recursive` is `true` — see `canvas_link_subdir`/
    /// `resolve_file_target_for_copy`.
    root_canvas_path: &'a Path,
    /// This canvas's own position in the `--out` tree, relative to the
    /// root's — `""` for the root canvas itself, otherwise whatever
    /// `CanvasLinkTarget::subdir` the *caller* (the CLI's own recursive
    /// driver) discovered this canvas under. Only consulted when
    /// `recursive` is `true`.
    current_slot: &'a str,
    links_base_url: Option<&'a str>,
    /// See `resolve_default_fold` — computed once in `build`, consulted per
    /// node by `build_node_view`.
    folded_ids: &'a std::collections::HashSet<String>,
    foldable_ids: &'a std::collections::HashSet<String>,
    /// This document's own `meshfox:tag-color` defaults (see
    /// `crate::tag_colors`) — computed once in `build`, consulted per node
    /// by `build_node_view` via `tag_colors::effective_color`. A malformed
    /// declaration just resolves to empty here (no node falls back to a
    /// tag-derived color, same as if none were declared) — `meshfox
    /// validate` is what surfaces that loudly, not a site/PDF export.
    tag_colors: &'a std::collections::HashMap<String, String>,
    /// Pre-fetched `display="code"` file-node content, keyed by node id —
    /// see `CodePreview`/`build_with_code_previews`. `None` means "read the
    /// target off local disk instead" (`build`'s own default, and what
    /// `meshfox pdf` still uses); `Some(map)` means every `display="code"`
    /// node's content was already fetched through a worker ahead of time,
    /// and a missing entry means the fetch itself resulted in the same
    /// "can't preview this" outcome `file_read::preview`'s `Err` does
    /// locally — never a silent fall-through to a local disk read.
    code_previews: Option<&'a std::collections::HashMap<String, CodePreview>>,
    /// `--copy-files` (see `build_for_static_export`) — `false` for `build`/
    /// `build_with_code_previews` (unchanged default behavior: a `file`-
    /// node's own target is a plain link, never copied — see the module
    /// doc comment's "Local-file references" section). `true` copies a
    /// `file`-node's target alongside the site, same as a Markdown image
    /// already is — see `build_node_view`'s own handling of it.
    copy_files: bool,
    /// `--recursive` (see `build_for_static_export`) — only meaningful when
    /// `copy_files` is also `true` (enforced by the CLI, not here). `false`:
    /// a `.canvas.md` target is a `CopyFilesTargetError`. `true`: it's
    /// rewritten to a link at that canvas's own rendered page instead, and
    /// queued into `CanvasLinkTarget` for the caller to actually render.
    recursive: bool,
}

/// A `file`-node's target resolved to a `.canvas.md` file while
/// `--copy-files` is on, with no `--recursive` to render it as a page
/// instead (or `--recursive` without `--copy-files`, which the CLI refuses
/// before ever reaching here) — copying a canvas source file verbatim into
/// `--out` would sit there as inert Markdown, not the rendered page a
/// reader following the link expects. `build_for_static_export` collects
/// every one of these across the whole tree (not just the first) so a
/// caller can report every offending node in one pass rather than
/// fail-fix-refail once per node.
#[derive(Debug, Clone)]
pub struct CopyFilesTargetError {
    pub node_id: String,
    pub target: String,
}

impl std::fmt::Display for CopyFilesTargetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "node {:?}'s file target {:?} is a .canvas.md — --copy-files can't just copy it \
             verbatim (that would sit in --out as inert Markdown source, not a rendered page); \
             pass --recursive to render it as a page instead, or point this node somewhere else",
            self.node_id, self.target
        )
    }
}

/// A `file`-node's target resolved to a `.canvas.md` file with `--recursive`
/// on (see `build_for_static_export`) — this canvas's own build doesn't
/// recurse into it itself (no I/O happens in this module at all — see its
/// own doc comment), it only computes where the target's rendered page
/// *will* live and queues it here for the caller (the CLI's own driver,
/// which owns finding-or-spawning a worker for it, fetching its resolved
/// canvas, and calling `build_for_static_export` on it in turn) to actually
/// build. The corresponding `NodeView.target` is already rewritten to the
/// right relative link by the time this is returned — the caller doesn't
/// need to patch anything, just render what `subdir` names.
#[derive(Debug, Clone)]
pub struct CanvasLinkTarget {
    /// Absolute, canonicalized path to the linked `.canvas.md` file.
    pub resolved: PathBuf,
    /// This target's own position in the `--out` tree, relative to the
    /// root's (see `RenderCtx::current_slot`) — computed once, purely from
    /// `resolved` and the root's own path (`canvas_link_subdir`), so every
    /// node anywhere in the tree that links to the same target computes the
    /// same `subdir` independently, with no shared registry needed: the
    /// caller's own `visited`-style dedup (by `resolved`) is what makes sure
    /// it only actually gets rendered once.
    pub subdir: String,
}

/// A `display="code"` file-node's target content, fetched ahead of time by
/// the caller (`build_with_code_previews`) instead of read from local disk
/// during `build` itself — field-for-field what the worker's own
/// `GET /api/nodes/:id/file-content` returns, and what
/// `crate::file_read::FilePreview` holds for the local-disk case `render_file_code`
/// otherwise falls back to.
#[derive(Debug, Clone)]
pub struct CodePreview {
    pub content: String,
    pub truncated: bool,
}

/// GFM-flavored Markdown -> HTML, with meshfox's own fence attributes
/// stripped first (see `crate::fence::strip_fence_attrs`) so a runnable
/// block's `name=`/`cache`/`deps=` don't leak into the rendered
/// `class="language-..."`, and every image/link URL rewritten per `ctx`
/// (see `resolve_image_url`/`resolve_link_url`). Every link additionally
/// gets `target="_blank"` (see `add_target_blank`).
///
/// Also handles two of meshfox's own narrow Markdown extensions (see
/// SPEC.md's "Formal grammar" / TODO.canvas.md's `image-attrs` and
/// `sub-superscript` tasks) that `pulldown-cmark` has no built-in support
/// for — `{width=..}`/`{height=..}` right after an image (`image_attrs`)
/// and `~sub~`/`^sup^` (`subsup`) — plus GFM alert blockquotes
/// (`> [!NOTE]`/...), which `Options::ENABLE_GFM` *does* parse and render
/// natively (as `<blockquote class="markdown-alert-note">`, no marker
/// text left behind); `site-template/style.css`'s own `.markdown-alert-*`
/// rules are what actually style those, nothing more is needed here.
fn render_markdown(
    text: &str,
    base_dir: &Path,
    asset_base: Option<&str>,
    ctx: &RenderCtx,
    assets: &mut Vec<Asset>,
    first_link_override: Option<&str>,
) -> String {
    // Taken (once) by the very first `Tag::Link` event below, if set —
    // `--copy-files`'s own resolution for a `file`-node's own target (see
    // `build_node_view`'s own caller). SPEC.md's grammar guarantees a
    // `file`/`link` node's body starts with *exactly one* Markdown link, so
    // "the first link in this text" and "this node's own target" are the
    // same thing whenever this is `Some` — every link after that first one
    // (there normally isn't one — the rest is a plain-text caption) still
    // goes through the ordinary `resolve_link_url` below, unaffected.
    let mut first_link_override = first_link_override.map(str::to_string);
    use pulldown_cmark::{html, Event, Options, Parser, Tag, TagEnd};
    let stripped = crate::fence::strip_fence_attrs(text);
    let options = Options::ENABLE_TABLES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_FOOTNOTES
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_GFM;
    // Raw HTML (`Event::Html`/`Event::InlineHtml`) straight from a node's
    // own Markdown text — `<script>` and friends — is dropped here, not
    // sanitized: web UI's `ReactMarkdown` renders without `rehype-raw`,
    // so `mdast-util-to-hast` already drops any raw-HTML node outright
    // (no `rehype-sanitize`-style safe-subset passthrough either) rather
    // than showing it as escaped text. Matching that means dropping, not
    // allowlisting. This must run on `raw` (straight off the parser)
    // *before* the transform loop below, which synthesizes its own
    // legitimate `Event::Html` for spliced `<img>` attrs and `<sub>`/
    // `<sup>` (`subsup_events`) — filtering the final `events` list
    // instead would strip those too.
    let raw: Vec<Event> = Parser::new_ext(&stripped, options)
        .filter(|e| !matches!(e, Event::Html(_) | Event::InlineHtml(_)))
        .collect();
    let mut events: Vec<Event> = Vec::with_capacity(raw.len());
    let mut in_code = false;
    let mut i = 0;
    while i < raw.len() {
        match raw[i].clone() {
            Event::Start(Tag::Image {
                link_type,
                dest_url,
                title,
                id,
            }) => {
                let new_url = resolve_image_url(&dest_url, base_dir, asset_base, ctx, assets);
                let mut image_events = vec![Event::Start(Tag::Image {
                    link_type,
                    dest_url: new_url.into(),
                    title,
                    id,
                })];
                i += 1;
                loop {
                    let ev = raw[i].clone();
                    let is_end = matches!(ev, Event::End(TagEnd::Image));
                    image_events.push(ev);
                    i += 1;
                    if is_end {
                        break;
                    }
                }
                let mut attrs = crate::image_attrs::ImageAttrs::default();
                if let Some(Event::Text(t)) = raw.get(i) {
                    if let Some((parsed, consumed)) = crate::image_attrs::parse(t) {
                        attrs = parsed;
                        let rest = t[consumed..].to_string();
                        i += 1;
                        if !rest.is_empty() {
                            events.extend(subsup_events(&rest));
                        }
                    }
                }
                let mut img_html = String::new();
                html::push_html(&mut img_html, image_events.into_iter());
                if !attrs.is_empty() {
                    img_html = splice_img_attrs(&img_html, &attrs);
                }
                events.push(Event::Html(img_html.into()));
            }
            Event::Start(Tag::Link {
                link_type,
                dest_url,
                title,
                id,
            }) => {
                let new_url = first_link_override
                    .take()
                    .unwrap_or_else(|| resolve_link_url(&dest_url, ctx));
                events.push(Event::Start(Tag::Link {
                    link_type,
                    dest_url: new_url.into(),
                    title,
                    id,
                }));
                i += 1;
            }
            event @ Event::Start(Tag::CodeBlock(_)) => {
                in_code = true;
                events.push(event);
                i += 1;
            }
            event @ Event::End(TagEnd::CodeBlock) => {
                in_code = false;
                events.push(event);
                i += 1;
            }
            Event::Text(t) if !in_code => {
                events.extend(subsup_events(&t));
                i += 1;
            }
            event => {
                events.push(event);
                i += 1;
            }
        }
    }
    let mut out = String::new();
    html::push_html(&mut out, events.into_iter());
    add_target_blank(&out)
}

/// Splits `text` on `crate::subsup`'s `~sub~`/`^sup^` syntax and turns
/// each marked run into a raw `<sub>`/`<sup>` HTML event (content
/// HTML-escaped — these bypass `pulldown-cmark`'s own escaping, since
/// they're never real Markdown text nodes) alongside ordinary `Text`
/// events for everything in between. A single `Text` event in, in
/// general several events out — hence `extend` at each call site rather
/// than a plain `map`.
fn subsup_events(text: &str) -> Vec<pulldown_cmark::Event<'static>> {
    use crate::subsup::{scan, Piece, Script};
    use pulldown_cmark::Event;
    scan(text)
        .into_iter()
        .map(|piece| match piece {
            Piece::Text(t) => Event::Text(t.to_string().into()),
            Piece::Marked(Script::Sub, inner) => {
                Event::Html(format!("<sub>{}</sub>", escape_html_text(inner)).into())
            }
            Piece::Marked(Script::Sup, inner) => {
                Event::Html(format!("<sup>{}</sup>", escape_html_text(inner)).into())
            }
        })
        .collect()
}

/// Minimal HTML-escaping for text dropped into a raw `Event::Html` chunk
/// (`subsup_events` above) — `pulldown-cmark`'s own escaping only applies
/// to `Event::Text`, never to `Event::Html`, which is emitted verbatim.
fn escape_html_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(c),
        }
    }
    out
}

/// Inserts ` width=".."`/` height=".."` (see `image_attrs::html_attrs`)
/// into an already-rendered `<img ... />` tag, right before its trailing
/// `/>` — the one spot `pulldown-cmark`'s own HTML writer always ends an
/// image tag with, self-closing slash included, regardless of which of
/// `alt`/`title` it did or didn't have to write first.
fn splice_img_attrs(img_html: &str, attrs: &crate::image_attrs::ImageAttrs) -> String {
    match img_html.rfind("/>") {
        Some(idx) => {
            let prefix = img_html[..idx].trim_end();
            format!("{prefix}{} />", crate::image_attrs::html_attrs(attrs))
        }
        None => img_html.to_string(),
    }
}

/// Adds `target="_blank" rel="noopener noreferrer"` to every rendered
/// `<a href="...">` — a static page has no in-app navigation to protect,
/// so clicking a link (to GitHub, an external doc, ...) shouldn't lose the
/// page the reader was on. In-page anchors (`href="#..."` — a footnote
/// reference/back-reference, or a plain user-written `[jump](#heading)`
/// link) are left alone: those navigate *within* this same page, where a
/// new tab would be wrong, not right.
///
/// A plain string scan rather than an `Event`-level rewrite: pulldown-
/// cmark's own HTML writer (`html::push_html`) has no way to attach extra
/// attributes to a link through `Tag::Link`'s fields, but it *does* always
/// render one exactly as `<a href="..."` (see its own `src/html.rs`) —
/// including a footnote's own internally-generated anchors, which never
/// pass through `Tag::Link`/this module's own event-mapping at all and so
/// couldn't be reached that way regardless. `<a href="` is therefore an
/// unambiguous, exhaustive marker for "a link starts here" in whatever this
/// function ever produces.
fn add_target_blank(html: &str) -> String {
    const MARKER: &str = "<a href=\"";
    let mut out = String::with_capacity(html.len());
    let mut rest = html;
    while let Some(idx) = rest.find(MARKER) {
        out.push_str(&rest[..idx]);
        rest = &rest[idx + MARKER.len()..];
        if rest.starts_with('#') {
            out.push_str(MARKER);
        } else {
            out.push_str("<a target=\"_blank\" rel=\"noopener noreferrer\" href=\"");
        }
    }
    out.push_str(rest);
    out
}

/// A relative reference resolved under `canvas_dir` and confined to it —
/// `crate::file_read::confine` (also used by the server's own file-node
/// endpoints and `constraint`'s `.content()`/`.json()`/...), plus this
/// module's own two extra requirements: query/fragment stripped first (a
/// Markdown link can carry a `#section` a bare filesystem path never
/// would), and the result must be a real file, not a directory. `None` for
/// anything that doesn't resolve — an escaping target, a missing one, or
/// one that's a directory — same "leave it alone" fallback every caller
/// here already takes.
fn resolve_canvas_relative(url: &str, canvas_dir: &Path) -> Option<PathBuf> {
    let clean = url.split(['?', '#']).next().unwrap_or(url);
    if clean.is_empty() {
        return None;
    }
    let resolved = crate::file_read::confine(canvas_dir, clean).ok()?;
    resolved.is_file().then_some(resolved)
}

/// `resolved`'s output-relative path, using forward slashes regardless of
/// host OS (so it's a valid URL path component, not just a valid local
/// filesystem path), plus the `Asset::include_asset` pair a worker-routed
/// copy step needs to actually fetch it when it's not under `canvas_dir`
/// (see `Asset::include_asset`'s own doc comment for why the ordinary
/// canvas-relative fallback route can't serve it).
///
/// Relative to `canvas_dir` when possible — the common case, and the only
/// one before `asset_base` existed, so every existing `--out` layout is
/// unchanged (and `include_asset` is `None`). When `resolved` isn't under
/// `canvas_dir` at all — an image referenced from inside an `include`-
/// dumped body whose own directory (`base_dir`, from `Node::cwd`) isn't
/// nested under the primary canvas's directory (see
/// `crate::include::resolve`'s `asset_base`) — falls back to a path
/// relative to `base_dir` instead, namespaced under `_include-assets/` so
/// it can never collide with a primary-canvas-relative asset that happens
/// to share the same relative filename, and pairs it with `(asset_base,
/// relative-path)` for `include_asset`. `resolved` is always under one of
/// the two (it came out of `resolve_canvas_relative` confined to whichever
/// of them was passed as its own `dir`), so the final `resolved` fallback
/// only matters if a caller ever passes mismatched dirs — never valid
/// output, but still a real (if ugly) path rather than a panic; `asset_base`
/// being `None` there too (nothing to build an `include_asset` pair from)
/// is deliberate for the same reason.
fn dest_rel_for(
    resolved: &Path,
    canvas_dir: &Path,
    base_dir: &Path,
    asset_base: Option<&str>,
) -> (String, Option<(String, String)>) {
    if let Ok(rel) = resolved.strip_prefix(canvas_dir) {
        return (rel.to_string_lossy().replace('\\', "/"), None);
    }
    if let (Ok(rel), Some(asset_base)) = (resolved.strip_prefix(base_dir), asset_base) {
        let rel_str = rel.to_string_lossy().replace('\\', "/");
        return (
            format!("_include-assets/{rel_str}"),
            Some((asset_base.to_string(), rel_str)),
        );
    }
    (resolved.to_string_lossy().replace('\\', "/"), None)
}

/// True for anything this module leaves untouched no matter what: an
/// in-page anchor, a URL with a scheme (`https://`, `mailto:`, `data:`,
/// ...), or a root-absolute path (ambiguous once site root and repo root
/// are different things — not this module's call to resolve).
fn is_external_or_absolute(url: &str) -> bool {
    url.starts_with('#')
        || url.starts_with('/')
        || url.contains("://")
        || url.starts_with("mailto:")
        || url.starts_with("data:")
}

/// A Markdown image's `src`: copies the referenced local file (queuing an
/// `Asset`) and points `src` at that copy, or — external/unresolvable —
/// leaves the URL exactly as written (never `links_base_url`-prefixed: an image
/// that couldn't be resolved under `base_dir` was never going to be
/// bundled, and `links_base_url` is for the deliberately-left-alone case, not the
/// failed-to-copy one). `base_dir` is the *owning node's* own directory
/// (`Node::cwd(ctx.canvas_dir)` — `ctx.canvas_dir` itself for an ordinary
/// node, an `include` target's own directory for a node spliced in from
/// one), not always `ctx.canvas_dir`; `asset_base` is that same node's own
/// raw `Node::asset_base` (`None` for an ordinary node) — see the module
/// doc comment's "Local-file references" section and `dest_rel_for`.
fn resolve_image_url(
    url: &str,
    base_dir: &Path,
    asset_base: Option<&str>,
    ctx: &RenderCtx,
    assets: &mut Vec<Asset>,
) -> String {
    if is_external_or_absolute(url) {
        return url.to_string();
    }
    match resolve_canvas_relative(url, base_dir) {
        Some(resolved) => {
            // `resolved` came out of `confine`, which canonicalizes `dir`
            // before comparing — `ctx.canvas_dir` was already canonicalized
            // once, up front, in `build_with_code_previews`, but `base_dir`
            // (an `include`'s own `asset_base`, when set) never has been.
            // Comparing an uncanonicalized `base_dir` against a canonical
            // `resolved` would spuriously miss the `strip_prefix` below on
            // any platform where a temp/asset directory is itself a symlink
            // (macOS: `/var` -> `/private/var`), always falling through to
            // the last-resort absolute-path branch in `dest_rel_for`. Only
            // for that comparison — `asset_base` itself (passed separately)
            // stays exactly the raw string it came in as, for
            // `Asset::include_asset`'s own reasons.
            let canonical_base_dir = base_dir
                .canonicalize()
                .unwrap_or_else(|_| base_dir.to_path_buf());
            let (dest_rel, include_asset) =
                dest_rel_for(&resolved, ctx.canvas_dir, &canonical_base_dir, asset_base);
            assets.push(Asset {
                source: resolved,
                dest_rel: dest_rel.clone(),
                include_asset,
            });
            dest_rel
        }
        None => url.to_string(),
    }
}

/// A plain link's `href` (Markdown or a `file`/`link` node's own target):
/// left untouched unless it's relative and `links_base_url` is set, in which case
/// it's prefixed with it. Never copies anything — see the module doc
/// comment for why plain links get this lighter treatment than images.
fn resolve_link_url(url: &str, ctx: &RenderCtx) -> String {
    if is_external_or_absolute(url) {
        return url.to_string();
    }
    match ctx.links_base_url {
        Some(base) => format!(
            "{}/{}",
            base.trim_end_matches('/'),
            url.trim_start_matches("./")
        ),
        None => url.to_string(),
    }
}

/// This repo's own naming convention for a meshfox-structured document
/// (`TODO.canvas.md`, `memory.canvas.md`, ...) — used by `resolve_file_
/// target_for_copy` to tell a canvas target apart from an ordinary file. Not
/// a strict guarantee the target actually parses as one (nothing here reads
/// it to check), just the same suffix convention every canvas in this repo
/// already follows.
fn is_canvas_file(path: &Path) -> bool {
    path.to_string_lossy().to_ascii_lowercase().ends_with(".canvas.md")
}

/// `resolved`'s position in the `--out` tree relative to the *root*
/// canvas's own directory (`root_canvas_dir` — `RenderCtx::root_canvas_path`'s
/// own parent), stripped of its `.canvas.md` suffix (`other.canvas.md`
/// becomes `other`, a directory name — not `other.canvas.md`, which would
/// look like the source file sitting there un-rendered). A pure function of
/// `(resolved, root_canvas_dir)` alone, deliberately — every node anywhere
/// in the whole tree that links to the same target canvas calls this with
/// the same two inputs and must get the same answer back, with no shared
/// mutable registry to keep them in sync (see `CanvasLinkTarget`'s own doc
/// comment).
///
/// Nested under `root_canvas_dir` when possible — the common case (a
/// self-contained set of canvases all under one directory tree), giving a
/// readable path that mirrors the source layout. When `resolved` lives
/// outside `root_canvas_dir` entirely, falls back to a hash of the full
/// path instead — can't fall back to "relative to *this* node's own
/// directory" the way `dest_rel_for`'s own `_include-assets/` fallback does
/// (that only works there because an asset has one single owning include
/// directory; a canvas link has no such single "owner" — it's whatever
/// canvas happens to link to it, from wherever in the tree).
fn canvas_link_subdir(resolved: &Path, root_canvas_dir: &Path) -> String {
    let rel = match resolved.strip_prefix(root_canvas_dir) {
        Ok(rel) => rel.to_string_lossy().replace('\\', "/"),
        Err(_) => {
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            std::hash::Hash::hash(&resolved, &mut hasher);
            format!("external-pages/{:016x}", std::hash::Hasher::finish(&hasher))
        }
    };
    strip_canvas_md_suffix(&rel)
}

/// `rel` with a trailing `.canvas.md` (case-insensitive) removed — the
/// `canvas_link_subdir`/`is_canvas_file` counterpart to `dest_rel_for`'s own
/// plain pass-through for a non-canvas asset. Leaves `rel` untouched if it
/// doesn't actually end that way (defensive; every real call site here only
/// ever calls this after `is_canvas_file` already confirmed the suffix).
fn strip_canvas_md_suffix(rel: &str) -> String {
    const SUFFIX_LEN: usize = ".canvas.md".len();
    if rel.len() > SUFFIX_LEN && rel.to_ascii_lowercase().ends_with(".canvas.md") {
        rel[..rel.len() - SUFFIX_LEN].to_string()
    } else {
        rel.to_string()
    }
}

/// The relative path (a sequence of `/`-joined directory names, no leading
/// or trailing slash — `""` means "the same directory") from a page at
/// `from_slot` to a page at `to_slot`, both themselves relative to the same
/// `--out` root (`RenderCtx::current_slot`/`CanvasLinkTarget::subdir`) —
/// ordinary `..`-counting path-diffing, computed purely from the two slot
/// strings, no filesystem access. `from_slot`/`to_slot` `""` means the
/// `--out` root itself (the root canvas's own page). Used to build an
/// `<a href>` from *this* canvas's own rendered page to a linked canvas's
/// one, wherever each of them actually ends up under `--out` — critical for
/// a link back to an already-visited canvas (the root itself, most
/// commonly, via a cycle) to still resolve to wherever that canvas *really*
/// rendered, not to a location naively computed as if this were the first
/// time anyone had linked to it.
fn relative_slot_path(from_slot: &str, to_slot: &str) -> String {
    let from_parts: Vec<&str> = from_slot.split('/').filter(|s| !s.is_empty()).collect();
    let to_parts: Vec<&str> = to_slot.split('/').filter(|s| !s.is_empty()).collect();
    let common = from_parts
        .iter()
        .zip(to_parts.iter())
        .take_while(|(a, b)| a == b)
        .count();
    let mut parts: Vec<&str> = std::iter::repeat_n("..", from_parts.len() - common).collect();
    parts.extend(&to_parts[common..]);
    if parts.is_empty() {
        ".".to_string()
    } else {
        parts.join("/")
    }
}

/// `--copy-files`' own handling of a `file`-node's own `target` (never
/// touched by `resolve_link_url`, which every other node's target still
/// goes through) — copies the referenced local file alongside the site,
/// the same treatment a Markdown image already gets (`resolve_image_url`),
/// and points the rendered link at that copy instead of leaving it as a bare
/// relative reference. `base_dir`/`asset_base` are this node's own (see
/// `resolve_image_url`'s own doc comment) — a `file`-node can't itself live
/// inside an `include`-dumped body (see `build_node_view`'s own comment on
/// this), so `asset_base` is always `None` in practice here, but threaded
/// through anyway rather than special-cased away, in case that invariant
/// ever changes.
///
/// A target that resolves to a `.canvas.md` file, with `ctx.recursive`:
/// rewritten to a relative link at that target's own rendered page (see
/// `canvas_link_subdir`/`relative_slot_path`, and — for a
/// `other.canvas.md#node-id`-style deep link — the fragment is carried over
/// as `#node-<id>`, matching the bundled template's own per-node anchor
/// convention, `site-template/_macros.html.tera`'s `id="node-{{ n.id }}"`),
/// and queues the target into `canvas_links` for the caller to actually
/// build+render. Without `ctx.recursive`, pushes a `CopyFilesTargetError`
/// onto `errors` instead — see that type's own doc comment for why. A
/// target that's external/absolute, or doesn't resolve locally at all
/// (missing, a directory, outside confinement), falls back to
/// `resolve_link_url`'s own plain-link handling — the same "leave it alone"
/// outcome `resolve_image_url` already has for an unresolvable image, not a
/// hard error: `--copy-files` copies what it can find, it doesn't require
/// every `file`-node's target to resolve.
#[allow(clippy::too_many_arguments)]
fn resolve_file_target_for_copy(
    node_id: &str,
    url: &str,
    base_dir: &Path,
    asset_base: Option<&str>,
    ctx: &RenderCtx,
    assets: &mut Vec<Asset>,
    errors: &mut Vec<CopyFilesTargetError>,
    canvas_links: &mut Vec<CanvasLinkTarget>,
) -> String {
    if is_external_or_absolute(url) {
        return resolve_link_url(url, ctx);
    }
    match resolve_canvas_relative(url, base_dir) {
        Some(resolved) if is_canvas_file(&resolved) => {
            if !ctx.recursive {
                errors.push(CopyFilesTargetError {
                    node_id: node_id.to_string(),
                    target: url.to_string(),
                });
                // `errors` being non-empty fails the whole build in
                // `build_for_static_export` before this ever reaches a
                // template — what's returned here doesn't matter for
                // correctness, only for not panicking on the way there.
                return resolve_link_url(url, ctx);
            }
            let root_canvas_dir = ctx.root_canvas_path.parent().unwrap_or(ctx.root_canvas_path);
            // The root canvas itself always renders at the `--out` root
            // (`current_slot: ""`) by convention, not wherever
            // `canvas_link_subdir` would naively place it by its own
            // filename — a link back to it (directly, or via a longer
            // cycle) needs to land there too, not at a nonexistent
            // filename-derived subdirectory.
            let target_slot = if resolved == ctx.root_canvas_path {
                String::new()
            } else {
                canvas_link_subdir(&resolved, root_canvas_dir)
            };
            let href_dir = relative_slot_path(ctx.current_slot, &target_slot);
            let fragment = url.split_once('#').map(|(_, f)| f);
            let href = match fragment {
                Some(f) if !f.is_empty() => format!("{href_dir}/index.html#node-{f}"),
                _ => format!("{href_dir}/index.html"),
            };
            canvas_links.push(CanvasLinkTarget {
                resolved,
                subdir: target_slot,
            });
            href
        }
        Some(resolved) => {
            let canonical_base_dir = base_dir
                .canonicalize()
                .unwrap_or_else(|_| base_dir.to_path_buf());
            let (dest_rel, include_asset) =
                dest_rel_for(&resolved, ctx.canvas_dir, &canonical_base_dir, asset_base);
            assets.push(Asset {
                source: resolved,
                dest_rel: dest_rel.clone(),
                include_asset,
            });
            dest_rel
        }
        None => resolve_link_url(url, ctx),
    }
}

/// Inline `<pre><code>` replacement for a `file`-type node's `display="code"`
/// preview (`web/src/MeshNode.tsx`'s `FileCodePreview`, backed there by a
/// live `GET /api/nodes/:id/file-content` fetch — nothing to fetch from
/// once static). `ctx.code_previews`, when set, is consulted first (see its
/// own doc comment) — the worker-routed path, content already fetched
/// through that same `GET /api/nodes/:id/file-content` route ahead of time.
/// `None` (or no entry for this node) falls back to reading the target once
/// at build time instead, via `crate::file_read::preview` — same
/// confinement/binary-sniff/size-cap as that route. Falls back to a plain
/// link (same as the web UI falls back to an error message) when the target
/// is missing, unreadable, outside `canvas_dir`, or looks binary.
fn render_file_code(node: &Node, ctx: &RenderCtx) -> String {
    let Some(target) = node.target.as_deref() else {
        return "<p><em>no target</em></p>".to_string();
    };
    let fallback_link = || {
        format!(
            "<p><a target=\"_blank\" rel=\"noopener noreferrer\" href=\"{0}\">{0}</a></p>",
            html_escape(target)
        )
    };

    let (content, truncated) = match ctx.code_previews {
        Some(map) => {
            let Some(preview) = map.get(&node.id) else {
                return fallback_link();
            };
            (preview.content.clone(), preview.truncated)
        }
        None => {
            let Ok(preview) = crate::file_read::preview(ctx.canvas_dir, target) else {
                return fallback_link();
            };
            (preview.content, preview.truncated)
        }
    };
    let lang = node.lang.clone().unwrap_or_else(|| guess_lang(target));
    let class_attr = if lang.is_empty() {
        String::new()
    } else {
        format!(" class=\"language-{}\"", html_escape(&lang))
    };
    let note = if truncated {
        "<p class=\"file-preview-truncated\">(truncated)</p>"
    } else {
        ""
    };
    format!(
        "<pre><code{class_attr}>{}</code></pre>{note}",
        html_escape(&content)
    )
}

/// Extension-based language guess for a `display="code"` preview whose node
/// has no explicit `lang=` — the static-render substitute for the web UI's
/// `pickLanguage`/CodeMirror `LanguageDescription.matchFilename` (no
/// language-data table to match against here, just a small, common-case
/// map); an unrecognized extension gets no `class` at all, same as
/// CodeMirror returning no match there.
fn guess_lang(path: &str) -> String {
    let ext = Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "rs" => "rust",
        "py" => "python",
        "js" | "mjs" | "cjs" => "javascript",
        "ts" => "typescript",
        "tsx" => "tsx",
        "jsx" => "jsx",
        "go" => "go",
        "java" => "java",
        "c" | "h" => "c",
        "cpp" | "cc" | "cxx" | "hpp" => "cpp",
        "sh" | "bash" => "bash",
        "json" => "json",
        "yaml" | "yml" => "yaml",
        "toml" => "toml",
        "md" | "markdown" => "markdown",
        "html" | "htm" => "html",
        "css" => "css",
        "sql" => "sql",
        "rb" => "ruby",
        "php" => "php",
        "swift" => "swift",
        "kt" | "kts" => "kotlin",
        "xml" => "xml",
        _ => "",
    }
    .to_string()
}

fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mdcanvas::parse;
    use std::fs;

    fn canvas(md: &str) -> Canvas {
        parse(md).unwrap()
    }

    /// `build` with no real `canvas_dir` (nothing to resolve images/
    /// `display="code"` targets against) and no `links_base_url` — what every
    /// test that isn't specifically about those two features wants.
    fn build_site(c: &Canvas) -> SiteData {
        build(c, Path::new("/nonexistent-meshfox-test-dir"), None).0
    }

    /// A fresh temp directory for a test that needs real files on disk —
    /// same pattern `include.rs`'s own tests use, `tag` keeping concurrent
    /// tests in this module from colliding on the same path.
    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "meshfox-staticgen-test-{tag}-{}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write(dir: &Path, name: &str, contents: &[u8]) {
        fs::write(dir.join(name), contents).unwrap();
    }

    #[test]
    fn maps_basic_node_fields() {
        let c = canvas(
            "# Root\n<!-- meshfox:node id=\"root\" color=\"red\" tags=\"a,b\" -->\n\nhello\n",
        );
        let site = build_site(&c);
        let root = site.find("root").unwrap();
        assert_eq!(root.title, "Root");
        assert_eq!(root.level, 1);
        assert_eq!(root.node_type, "text");
        assert_eq!(root.color.as_deref(), Some("red"));
        assert_eq!(root.tags, vec!["a".to_string(), "b".to_string()]);
        assert!(root.html_body.contains("hello"));
        assert_eq!(site.title, "Root");
    }

    // TODO.canvas.md: "Node colour by tag" — `build`'s own resolution of
    // `NodeView.color`, end to end through `declared_tag_colors`/
    // `effective_color`.
    #[test]
    fn a_node_with_no_explicit_color_falls_back_to_its_first_matching_tags_color() {
        let c = canvas(concat!(
            "# Root\n<!-- meshfox:node id=\"root\" -->\n",
            "<!-- meshfox:tag-color tag=\"bug\" color=\"1\" -->\n\n",
            "## Child\n<!-- meshfox:node id=\"child\" tags=\"untagged,bug\" -->\n\nbody\n",
        ));
        let site = build_site(&c);
        assert_eq!(site.find("child").unwrap().color.as_deref(), Some("1"));
    }

    #[test]
    fn an_explicit_color_wins_over_a_tag_default() {
        let c = canvas(concat!(
            "# Root\n<!-- meshfox:node id=\"root\" -->\n",
            "<!-- meshfox:tag-color tag=\"bug\" color=\"1\" -->\n\n",
            "## Child\n<!-- meshfox:node id=\"child\" color=\"3\" tags=\"bug\" -->\n\nbody\n",
        ));
        let site = build_site(&c);
        assert_eq!(site.find("child").unwrap().color.as_deref(), Some("3"));
    }

    // TODO.canvas.md: "Base64 image" — a `data:` image `src` must pass
    // through `resolve_image_url` byte-for-byte, same as any other
    // external URL (`is_external_or_absolute`): a browser/headless-Chrome
    // decodes it natively, there's no local file to queue as an `Asset`.
    #[test]
    fn a_data_url_image_src_passes_through_unresolved_and_unqueued() {
        let c = canvas("# Root\n<!-- meshfox:node id=\"root\" -->\n\n![x](data:image/png;base64,iVBORw0KGgo=)\n");
        let (site, assets) = build(&c, Path::new("/nonexistent-meshfox-test-dir"), None);
        let body = &site.find("root").unwrap().html_body;
        assert!(
            body.contains("src=\"data:image/png;base64,iVBORw0KGgo=\""),
            "{body}"
        );
        assert!(assets.is_empty(), "a data: URL has no local file to copy");
    }

    // TODO.canvas.md: "Static export не санитайзит сырой HTML из тела нод"
    // — a static site has no `ReactMarkdown` deciding what's safe to
    // render at view time, unlike web UI, so raw HTML in a node's body
    // (however it got there — hand-typed, or spliced in via a runnable
    // fence's `output="markdown"`) must be dropped at render time here
    // instead, the same way web UI's `ReactMarkdown` (no `rehype-raw`)
    // already drops it rather than rendering or sanitizing it.
    #[test]
    fn raw_html_in_a_node_body_is_dropped_not_rendered() {
        let c = canvas(concat!(
            "# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
            "before <script>alert(1)</script> after\n\n",
            "<div onclick=\"evil()\">block html</div>\n",
        ));
        let site = build_site(&c);
        let body = &site.find("root").unwrap().html_body;
        assert!(!body.contains("<script"), "{body}");
        assert!(!body.contains("onclick"), "{body}");
        assert!(!body.contains("<div"), "{body}");
        assert!(body.contains("before"), "{body}");
        assert!(body.contains("after"), "{body}");
    }

    // Raw-HTML filtering (above) must only touch what the parser itself
    // read off the node's own Markdown text — not the `Event::Html` this
    // renderer synthesizes for its own spliced `<img>` attrs, or dropping
    // this would silently regress `image_size_attrs_become_html_width_height`.
    #[test]
    fn image_size_attrs_still_render_alongside_dropped_raw_html() {
        let c = canvas(concat!(
            "# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
            "<script>alert(1)</script>\n\n",
            "![alt](pic.png){width=300}\n",
        ));
        let site = build_site(&c);
        let body = &site.find("root").unwrap().html_body;
        assert!(!body.contains("<script"), "{body}");
        assert!(body.contains(r#"width="300""#), "{body}");
    }

    #[test]
    fn a_rendered_link_opens_in_a_new_tab() {
        let c = canvas("# Root\n<!-- meshfox:node id=\"root\" -->\n\n[meshfox](https://github.com/example/meshfox)\n");
        let site = build_site(&c);
        let body = &site.find("root").unwrap().html_body;
        assert!(
            body.contains("<a target=\"_blank\" rel=\"noopener noreferrer\" href=\"https://github.com/example/meshfox\">"),
            "{body}"
        );
    }

    // TODO.canvas.md: "Формальные граматики для meshfox:*" subtree ->
    // "Атрибуты картинок в markdown" — `{width=..}`/`{height=..}` right
    // after an image (`crate::image_attrs`), spliced into the rendered
    // `<img>` tag.
    #[test]
    fn image_size_attrs_become_html_width_height() {
        let c = canvas("# Root\n<!-- meshfox:node id=\"root\" -->\n\n![alt](pic.png){width=300 height=50%}\n");
        let (site, _assets) = build(&c, Path::new("/nonexistent-meshfox-test-dir"), None);
        let body = &site.find("root").unwrap().html_body;
        assert!(
            body.contains(r#"<img src="pic.png" alt="alt" width="300" height="50%" />"#),
            "{body}"
        );
    }

    #[test]
    fn text_right_after_an_image_with_no_size_attrs_is_left_alone() {
        let c = canvas("# Root\n<!-- meshfox:node id=\"root\" -->\n\n![alt](pic.png) not attrs\n");
        let (site, _assets) = build(&c, Path::new("/nonexistent-meshfox-test-dir"), None);
        let body = &site.find("root").unwrap().html_body;
        assert!(body.contains("not attrs"), "{body}");
        assert!(!body.contains("width="), "{body}");
    }

    #[test]
    fn leftover_text_after_a_partial_image_attrs_match_still_renders() {
        let c = canvas("# Root\n<!-- meshfox:node id=\"root\" -->\n\n![alt](pic.png){width=300} tail\n");
        let (site, _assets) = build(&c, Path::new("/nonexistent-meshfox-test-dir"), None);
        let body = &site.find("root").unwrap().html_body;
        assert!(body.contains(r#"width="300""#), "{body}");
        assert!(body.contains("tail"), "{body}");
    }

    // TODO.canvas.md: same subtree -> "Подстрочный/надстрочный".
    #[test]
    fn subscript_and_superscript_render_as_sub_sup_tags() {
        let c = canvas("# Root\n<!-- meshfox:node id=\"root\" -->\n\nH~2~O and x^n^\n");
        let site = build_site(&c);
        let body = &site.find("root").unwrap().html_body;
        assert!(body.contains("H<sub>2</sub>O"), "{body}");
        assert!(body.contains("x<sup>n</sup>"), "{body}");
    }

    #[test]
    fn subsup_markup_never_applies_inside_a_code_block() {
        let c = canvas("# Root\n<!-- meshfox:node id=\"root\" -->\n\n```text\nx~2~\n```\n");
        let site = build_site(&c);
        let body = &site.find("root").unwrap().html_body;
        assert!(body.contains("x~2~"), "{body}");
        assert!(!body.contains("<sub>"), "{body}");
    }

    #[test]
    fn doubled_tilde_strikethrough_is_unaffected_by_subsup() {
        let c = canvas("# Root\n<!-- meshfox:node id=\"root\" -->\n\n~~gone~~\n");
        let site = build_site(&c);
        let body = &site.find("root").unwrap().html_body;
        assert!(body.contains("<del>gone</del>"), "{body}");
        assert!(!body.contains("<sub>"), "{body}");
    }

    // TODO.canvas.md: same subtree -> "Admonition/callout-блоки" (GFM
    // variant) — `Options::ENABLE_GFM` parses `> [!NOTE]`/... natively,
    // stripping the marker line and giving the blockquote a
    // `markdown-alert-*` class; `site-template/style.css` styles it.
    #[test]
    fn a_gfm_alert_blockquote_gets_its_type_class_and_no_marker_text() {
        let c = canvas("# Root\n<!-- meshfox:node id=\"root\" -->\n\n> [!WARNING]\n> be careful\n");
        let site = build_site(&c);
        let body = &site.find("root").unwrap().html_body;
        assert!(
            body.contains(r#"<blockquote class="markdown-alert-warning">"#),
            "{body}"
        );
        assert!(body.contains("be careful"), "{body}");
        assert!(!body.contains("[!WARNING]"), "{body}");
    }

    #[test]
    fn an_ordinary_blockquote_is_unaffected_by_gfm_alerts() {
        let c = canvas("# Root\n<!-- meshfox:node id=\"root\" -->\n\n> just a quote\n");
        let site = build_site(&c);
        let body = &site.find("root").unwrap().html_body;
        assert!(body.contains("<blockquote>\n<p>just a quote"), "{body}");
    }

    #[test]
    fn an_in_page_anchor_link_does_not_open_in_a_new_tab() {
        // A plain user-written `#heading` link, and a footnote reference
        // (pulldown-cmark's own internally-generated `<a href="#...">`,
        // never passing through this module's own Tag::Link handling at
        // all) — both navigate within the same page, where a new tab is
        // wrong, not right.
        let c = canvas(
            "# Root\n<!-- meshfox:node id=\"root\" -->\n\n\
             [jump](#somewhere) and a footnote[^1].\n\n\
             [^1]: the footnote body\n",
        );
        let site = build_site(&c);
        let body = &site.find("root").unwrap().html_body;
        assert!(!body.contains("target=\"_blank\""), "{body}");
        assert!(body.contains("<a href=\"#somewhere\">"), "{body}");
    }

    #[test]
    fn a_fully_positioned_node_keeps_its_exact_authored_position() {
        let c = canvas("# Root\n<!-- meshfox:node id=\"root\" x=10 y=20 w=200 h=80 -->\n");
        let site = build_site(&c);
        let pos = site
            .find("root")
            .unwrap()
            .position
            .expect("all four values were set");
        assert_eq!(
            (pos.x, pos.y, pos.width, pos.height),
            (10.0, 20.0, 200.0, 80.0)
        );
    }

    #[test]
    fn an_unpositioned_node_gets_no_position_at_all() {
        let c = canvas("# Root\n\n## Child\n<!-- meshfox:node id=\"child\" -->\n\nbody\n");
        let site = build_site(&c);
        let child = site.find("child").unwrap();
        assert!(child.position.is_none());
    }

    #[test]
    fn a_partially_positioned_node_still_gets_no_position() {
        // Only x/y set, not width/height — same "all four or nothing" rule.
        let c = canvas("# Root\n<!-- meshfox:node id=\"root\" x=10 y=20 -->\n");
        let site = build_site(&c);
        assert!(site.find("root").unwrap().position.is_none());
    }

    #[test]
    fn a_group_members_position_resolves_relative_to_its_anchored_group() {
        let c = canvas(
            "# Root\n\n## Frame\n<!-- meshfox:node id=\"frame\" type=\"group\" x=1000 y=1000 -->\n\n\
             ### Member\n<!-- meshfox:node id=\"member\" x=20 y=20 w=100 h=80 -->\n\nbody\n",
        );
        let site = build_site(&c);
        let pos = site
            .find("member")
            .unwrap()
            .position
            .expect("group has a real anchor");
        assert_eq!(
            (pos.x, pos.y, pos.width, pos.height),
            (1020.0, 1020.0, 100.0, 80.0)
        );
    }

    #[test]
    fn a_group_members_position_is_none_when_its_group_has_no_anchor() {
        // Real x/y/w/h on the member itself, but the enclosing group has
        // never been dragged — nothing for the member's own coordinate to
        // be relative *to*, so this falls back to flowed CSS same as any
        // other unpositioned node, rather than misreading the member's
        // group-relative number as if it were absolute.
        let c = canvas(
            "# Root\n\n## Frame\n<!-- meshfox:node id=\"frame\" type=\"group\" -->\n\n\
             ### Member\n<!-- meshfox:node id=\"member\" x=20 y=20 w=100 h=80 -->\n\nbody\n",
        );
        let site = build_site(&c);
        assert!(site.find("member").unwrap().position.is_none());
    }

    #[test]
    fn root_never_folds_by_default() {
        let c = canvas("# Root\n<!-- meshfox:node id=\"root\" -->\n\nhello\n");
        let site = build_site(&c);
        assert!(!site.find("root").unwrap().folded);
    }

    #[test]
    fn a_plain_child_folds_by_default() {
        let c = canvas("# Root\n\n## Child\n<!-- meshfox:node id=\"child\" -->\n\nbody\n");
        let site = build_site(&c);
        let child = site.find("child").unwrap();
        assert!(child.foldable);
        assert!(child.folded);
    }

    #[test]
    fn the_unfold_option_flips_the_default_to_expanded() {
        let c = canvas(
            "# Root\n<!-- meshfox:node id=\"root\" -->\n<!-- meshfox:option name=\"unfold\" -->\n\nprose\n\n\
             ## Child\n<!-- meshfox:node id=\"child\" -->\n\nbody\n",
        );
        let site = build_site(&c);
        assert!(!site.find("child").unwrap().folded);
    }

    #[test]
    fn an_explicit_fold_override_always_wins() {
        // `fold="false"` keeps a child expanded despite the document's own
        // default; `fold="true"` folds root despite root normally never
        // folding by default.
        let c = canvas(
            "# Root\n<!-- meshfox:node id=\"root\" fold=\"true\" -->\n\n\
             ## Child\n<!-- meshfox:node id=\"child\" fold=\"false\" -->\n\nbody\n",
        );
        let site = build_site(&c);
        assert!(site.find("root").unwrap().folded);
        assert!(!site.find("child").unwrap().folded);
    }

    #[test]
    fn a_node_with_an_explicit_size_does_not_fold_by_default() {
        let c = canvas(
            "# Root\n\n## Child\n<!-- meshfox:node id=\"child\" x=10 y=20 w=200 h=80 -->\n\nbody\n",
        );
        let site = build_site(&c);
        assert!(!site.find("child").unwrap().folded);
    }

    #[test]
    fn a_childless_title_only_node_is_not_foldable_and_never_folds_by_default() {
        let c = canvas("# Root\n\n## Child\n<!-- meshfox:node id=\"child\" -->\n");
        let site = build_site(&c);
        let child = site.find("child").unwrap();
        assert!(!child.foldable);
        assert!(!child.folded);
    }

    #[test]
    fn a_title_only_node_with_children_is_foldable() {
        let c = canvas("# Root\n\n## Child\n<!-- meshfox:node id=\"child\" -->\n\n### Grandchild\n<!-- meshfox:node id=\"grandchild\" -->\n\nbody\n");
        let site = build_site(&c);
        let child = site.find("child").unwrap();
        assert!(child.foldable);
        assert!(child.folded);
    }

    #[test]
    fn children_nest_under_their_structural_parent() {
        let c = canvas(
            "# Root\n\n## A\n<!-- meshfox:node id=\"a\" -->\n\n\
             ### A1\n<!-- meshfox:node id=\"a1\" -->\n\nbody\n\n\
             ## B\n<!-- meshfox:node id=\"b\" -->\n",
        );
        let site = build_site(&c);
        assert_eq!(site.root.children.len(), 2);
        assert_eq!(site.root.children[0].id, "a");
        assert_eq!(site.root.children[1].id, "b");
        assert_eq!(site.root.children[0].children.len(), 1);
        assert_eq!(site.root.children[0].children[0].id, "a1");
        assert!(site.root.children[1].children.is_empty());
    }

    #[test]
    fn depth_is_the_real_tree_depth_not_the_heading_level() {
        // depth 0/1/2 mirror `web/src/autolayout.ts`'s own depth-based
        // regimes (root+its direct children stay put; depth >=2 is where
        // real rightward branching, JS-repositioned, starts) — see the
        // module doc comment.
        let c = canvas(
            "# Root\n\n## A\n<!-- meshfox:node id=\"a\" -->\n\n\
             ### A1\n<!-- meshfox:node id=\"a1\" -->\n\nbody\n\n\
             ## B\n<!-- meshfox:node id=\"b\" -->\n",
        );
        let site = build_site(&c);
        assert_eq!(site.find("root").unwrap().depth, 0);
        assert_eq!(site.find("a").unwrap().depth, 1);
        assert_eq!(site.find("b").unwrap().depth, 1);
        assert_eq!(site.find("a1").unwrap().depth, 2);
    }

    #[test]
    fn two_parents_own_children_stay_nested_under_their_own_parent() {
        // Two depth-1 parents, each with their own depth-2 children — each
        // set of children nests under its own parent, not merged into a
        // shared list the way an earlier columnar design grouped them (see
        // the module doc comment for why that was dropped: it decoupled a
        // child visually from its own real parent).
        let c = canvas(
            "# Root\n\n## A\n<!-- meshfox:node id=\"a\" -->\n\n\
             ### A1\n<!-- meshfox:node id=\"a1\" -->\n\n\
             ### A2\n<!-- meshfox:node id=\"a2\" -->\n\n\
             ## B\n<!-- meshfox:node id=\"b\" -->\n\n\
             ### B1\n<!-- meshfox:node id=\"b1\" -->\n",
        );
        let site = build_site(&c);
        let a = site.find("a").unwrap();
        let a_children: Vec<&str> = a.children.iter().map(|n| n.id.as_str()).collect();
        assert_eq!(a_children, vec!["a1", "a2"]);
        let b = site.find("b").unwrap();
        let b_children: Vec<&str> = b.children.iter().map(|n| n.id.as_str()).collect();
        assert_eq!(b_children, vec!["b1"]);
    }

    #[test]
    fn group_nodes_have_no_body() {
        let c = canvas("# Root\n\n## Frame\n<!-- meshfox:node id=\"frame\" type=\"group\" -->\n");
        let site = build_site(&c);
        let frame = site.find("frame").unwrap();
        assert_eq!(frame.node_type, "group");
        assert_eq!(frame.html_body, "");
    }

    #[test]
    fn fence_attrs_are_stripped_from_rendered_bodies() {
        let c = canvas(
            "# Root\n\n## Block\n<!-- meshfox:node id=\"block\" -->\n\n```bash name=\"build\" cache\necho hi\n```\n",
        );
        let site = build_site(&c);
        let block = site.find("block").unwrap();
        assert!(
            block.html_body.contains("language-bash"),
            "{}",
            block.html_body
        );
        assert!(!block.html_body.contains("name="), "{}", block.html_body);
    }

    #[test]
    fn structural_parent_child_links_are_not_in_site_edges() {
        // Structural edges are drawn by pure CSS straight from
        // `NodeView.children`'s own nesting — see the module doc comment —
        // so `site.edges` (JS-consumed) must never carry one.
        let c = canvas("# Root\n\n## Child\n<!-- meshfox:node id=\"child\" -->\n");
        let (site, _) = build(&c, Path::new("/nonexistent-meshfox-test-dir"), None);
        assert!(site.edges.is_empty());
    }

    #[test]
    fn extra_edge_is_always_included() {
        let c = canvas(
            "# Root\n<!-- meshfox:node id=\"root\" -->\n\n\
             ## A\n<!-- meshfox:node id=\"a\" -->\n\n\
             ## B\n<!-- meshfox:node id=\"b\" -->\n<!-- meshfox:edge from=\"a\" -->\n",
        );
        let site = build_site(&c);
        let extra = site.edges.first().unwrap();
        assert_eq!((extra.from.as_str(), extra.to.as_str()), ("a", "b"));
    }

    #[test]
    fn local_image_is_queued_as_an_asset_and_src_is_rewritten() {
        let dir = temp_dir("image");
        write(&dir, "shot.png", b"not a real png, just bytes");
        let c = canvas("# Root\n<!-- meshfox:node id=\"root\" -->\n\n![Screenshot](shot.png)\n");

        let (site, assets) = build(&c, &dir, None);
        assert!(
            site.find("root")
                .unwrap()
                .html_body
                .contains("src=\"shot.png\""),
            "{}",
            site.find("root").unwrap().html_body
        );
        assert_eq!(assets.len(), 1);
        assert_eq!(assets[0].dest_rel, "shot.png");
        assert_eq!(
            fs::read(&assets[0].source).unwrap(),
            b"not a real png, just bytes"
        );

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn image_outside_canvas_dir_is_left_unresolved_and_not_queued() {
        let dir = temp_dir("image-escape");
        let c = canvas("# Root\n<!-- meshfox:node id=\"root\" -->\n\n![x](../../etc/passwd)\n");

        let (site, assets) = build(&c, &dir, None);
        assert!(
            site.find("root")
                .unwrap()
                .html_body
                .contains("src=\"../../etc/passwd\""),
            "{}",
            site.find("root").unwrap().html_body
        );
        assert!(assets.is_empty());

        fs::remove_dir_all(&dir).ok();
    }

    // TODO.canvas.md: asset_base bug — `staticgen` used to always resolve a
    // node's own Markdown images against `canvas_dir`, even for a node
    // whose body actually came from an `include` target living in a
    // different directory (`Node::asset_base`/`cwd`) — so an image
    // referenced relative to *that* target's own directory either resolved
    // to the wrong file (one that coincidentally exists at the same
    // relative path under `canvas_dir`) or didn't resolve at all. Two
    // sibling temp dirs, each with their own same-named file with different
    // content, proves which directory actually got read.
    #[test]
    fn an_image_inside_an_include_dump_resolves_against_its_own_asset_base_not_canvas_dir() {
        let canvas_dir = temp_dir("asset-base-canvas-dir");
        let include_dir = temp_dir("asset-base-include-dir");
        write(&canvas_dir, "shot.png", b"wrong file: lives next to the canvas");
        write(&include_dir, "shot.png", b"right file: lives next to the include target");

        let mut c = canvas("# Root\n<!-- meshfox:node id=\"root\" -->\n\n![Screenshot](shot.png)\n");
        c.nodes[0].asset_base = Some(include_dir.to_string_lossy().into_owned());

        let (_site, assets) = build(&c, &canvas_dir, None);
        assert_eq!(assets.len(), 1, "{assets:?}");
        assert_eq!(
            fs::read(&assets[0].source).unwrap(),
            b"right file: lives next to the include target",
            "resolved against canvas_dir instead of the node's own asset_base"
        );

        fs::remove_dir_all(&canvas_dir).ok();
        fs::remove_dir_all(&include_dir).ok();
    }

    // Since the two temp dirs above are siblings (neither nested under the
    // other), `dest_rel` can't just be "relative to canvas_dir" for the
    // asset_base case — falls back to a `_include-assets/`-namespaced path
    // relative to the include's own directory instead (see `dest_rel_for`).
    #[test]
    fn an_asset_base_image_outside_canvas_dir_gets_a_namespaced_dest_rel() {
        let canvas_dir = temp_dir("asset-base-dest-rel-canvas-dir");
        let include_dir = temp_dir("asset-base-dest-rel-include-dir");
        write(&include_dir, "shot.png", b"bytes");

        let mut c = canvas("# Root\n<!-- meshfox:node id=\"root\" -->\n\n![Screenshot](shot.png)\n");
        c.nodes[0].asset_base = Some(include_dir.to_string_lossy().into_owned());

        let (_site, assets) = build(&c, &canvas_dir, None);
        assert_eq!(assets.len(), 1, "{assets:?}");
        assert_eq!(assets[0].dest_rel, "_include-assets/shot.png");
        // The ordinary canvas-relative fallback route can't serve this (it's
        // confined to `canvas_dir`, and this asset lives outside it) — a
        // worker-routed copy step needs `include_asset` instead, exactly the
        // `dir`/`file` pair `GET /api/include-asset` expects.
        assert_eq!(
            assets[0].include_asset,
            Some((include_dir.to_string_lossy().into_owned(), "shot.png".to_string()))
        );

        fs::remove_dir_all(&canvas_dir).ok();
        fs::remove_dir_all(&include_dir).ok();
    }

    // The common real-world shape (`README.md`/`SPEC.md` includes in this
    // very repo): the include target lives in the *same* directory as the
    // primary canvas, or a subdirectory of it — `asset_base` is set, but
    // `dest_rel` should stay a plain canvas-relative path, unnamespaced,
    // exactly as if `asset_base` had never been introduced.
    #[test]
    fn an_asset_base_image_nested_under_canvas_dir_gets_a_plain_dest_rel() {
        let canvas_dir = temp_dir("asset-base-nested-canvas-dir");
        let include_dir = canvas_dir.join("included");
        fs::create_dir_all(&include_dir).unwrap();
        write(&include_dir, "shot.png", b"bytes");

        let mut c = canvas("# Root\n<!-- meshfox:node id=\"root\" -->\n\n![Screenshot](shot.png)\n");
        c.nodes[0].asset_base = Some(include_dir.to_string_lossy().into_owned());

        let (_site, assets) = build(&c, &canvas_dir, None);
        assert_eq!(assets.len(), 1, "{assets:?}");
        assert_eq!(assets[0].dest_rel, "included/shot.png");
        // Nested under canvas_dir — the ordinary fallback route can serve
        // it directly, no `/api/include-asset` round trip needed.
        assert_eq!(assets[0].include_asset, None);

        fs::remove_dir_all(&canvas_dir).ok();
    }

    // TODO.canvas.md: "Опция: копировать file-таргет в dist; canvas-таргет
    // — ошибка" — `--copy-files`'s own core support, via
    // `build_for_static_export(..., copy_files: true)`.
    #[test]
    fn copy_files_queues_a_plain_file_nodes_target_as_an_asset() {
        let dir = temp_dir("copy-files-plain");
        write(&dir, "report.pdf", b"not a real pdf, just bytes");
        let c = canvas(concat!(
            "# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
            "## Report\n<!-- meshfox:node id=\"f\" type=\"file\" -->\n\n[report](report.pdf)\n",
        ));

        let (site, assets, canvas_links) =
            build_for_static_export(&c, &dir, &dir, "", None, None, true, false).unwrap();
        assert_eq!(
            site.find("f").unwrap().target.as_deref(),
            Some("report.pdf")
        );
        assert!(canvas_links.is_empty(), "{canvas_links:?}");
        assert_eq!(assets.len(), 1, "{assets:?}");
        assert_eq!(assets[0].dest_rel, "report.pdf");
        assert_eq!(
            fs::read(&assets[0].source).unwrap(),
            b"not a real pdf, just bytes"
        );

        fs::remove_dir_all(&dir).ok();
    }

    // Regression test for a real bug, caught by hand rendering this repo's
    // own README.md with `--copy-files --recursive`: `NodeView.target` was
    // correctly rewritten, every file actually landed in `--out` — but the
    // bundled `site-template/` never reads `target` at all, only
    // `html_body` (the node's own rendered Markdown) — which was still
    // going through the *old* `resolve_link_url`/`links_base_url` path,
    // completely unaware `--copy-files` existed. Every generated page
    // existed; nothing on the site actually linked to any of them.
    // `links_base_url` set here specifically so the two paths' outputs
    // provably differ if the fix ever regresses.
    #[test]
    fn copy_files_rewrites_the_visible_link_not_just_node_view_target() {
        let dir = temp_dir("copy-files-visible-link");
        write(&dir, "report.pdf", b"bytes");
        let c = canvas(concat!(
            "# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
            "## Report\n<!-- meshfox:node id=\"f\" type=\"file\" -->\n\n[report](report.pdf)\n",
        ));

        let (site, _assets, _canvas_links) = build_for_static_export(
            &c,
            &dir,
            &dir,
            "",
            Some("https://example.com/repo"),
            None,
            true,
            false,
        )
        .unwrap();
        let f = site.find("f").unwrap();
        assert_eq!(f.target.as_deref(), Some("report.pdf"));
        assert!(
            f.html_body.contains("href=\"report.pdf\""),
            "the visible link must match NodeView.target, not links_base_url: {}",
            f.html_body
        );
        assert!(
            !f.html_body.contains("https://example.com/repo"),
            "{}",
            f.html_body
        );

        fs::remove_dir_all(&dir).ok();
    }

    // Same regression, `--recursive` case: the visible link must point at
    // the rendered nested page, not the old links_base_url-prefixed target.
    #[test]
    fn recursive_rewrites_the_visible_link_not_just_node_view_target() {
        let dir = temp_dir("recursive-visible-link");
        write(&dir, "other.canvas.md", b"# Other\n<!-- meshfox:node id=\"root\" -->\n");
        write(&dir, "doc.canvas.md", b"unused: build() is given `c` directly, not read from disk");
        let root_path = dir.join("doc.canvas.md");
        let c = canvas(concat!(
            "# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
            "## Other\n<!-- meshfox:node id=\"f\" type=\"file\" -->\n\n[other](other.canvas.md)\n",
        ));

        let (site, _assets, _canvas_links) = build_for_static_export(
            &c,
            &dir,
            &root_path,
            "",
            Some("https://example.com/repo"),
            None,
            true,
            true,
        )
        .unwrap();
        let f = site.find("f").unwrap();
        assert_eq!(f.target.as_deref(), Some("other/index.html"));
        assert!(
            f.html_body.contains("href=\"other/index.html\""),
            "{}",
            f.html_body
        );

        fs::remove_dir_all(&dir).ok();
    }

    // User's own clarification: a `display="code"` target is already
    // inlined into the HTML — copying it alongside would just be dead
    // weight, nobody follows that link.
    #[test]
    fn copy_files_never_copies_a_display_code_targets_content() {
        let dir = temp_dir("copy-files-display-code");
        write(&dir, "snippet.txt", b"inlined already");
        let c = canvas(concat!(
            "# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
            "## Snippet\n<!-- meshfox:node id=\"f\" type=\"file\" display=\"code\" -->\n\n",
            "[snippet](snippet.txt)\n",
        ));

        let (_site, assets, _canvas_links) =
            build_for_static_export(&c, &dir, &dir, "", None, None, true, false).unwrap();
        assert!(assets.is_empty(), "{assets:?}");

        fs::remove_dir_all(&dir).ok();
    }

    // `--copy-files` only ever applies to a `file`-node's own target — a
    // `link`-type node (external/preview reference) is left exactly as
    // `resolve_link_url` already handles it, same as without the flag.
    #[test]
    fn copy_files_never_touches_a_link_type_nodes_target() {
        let dir = temp_dir("copy-files-link-node");
        write(&dir, "sibling.md", b"# irrelevant");
        let c = canvas(concat!(
            "# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
            "## Ref\n<!-- meshfox:node id=\"l\" type=\"link\" -->\n\n[sibling](sibling.md)\n",
        ));

        let (site, assets, _canvas_links) =
            build_for_static_export(&c, &dir, &dir, "", None, None, true, false).unwrap();
        assert_eq!(
            site.find("l").unwrap().target.as_deref(),
            Some("sibling.md")
        );
        assert!(assets.is_empty(), "{assets:?}");

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn copy_files_errors_on_a_canvas_md_target_instead_of_copying_it() {
        let dir = temp_dir("copy-files-canvas-target");
        write(&dir, "other.canvas.md", b"# Other\n<!-- meshfox:node id=\"root\" -->\n");
        let c = canvas(concat!(
            "# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
            "## Other\n<!-- meshfox:node id=\"f\" type=\"file\" -->\n\n[other](other.canvas.md)\n",
        ));

        let errors =
            build_for_static_export(&c, &dir, &dir, "", None, None, true, false).unwrap_err();
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert_eq!(errors[0].node_id, "f");
        assert_eq!(errors[0].target, "other.canvas.md");

        fs::remove_dir_all(&dir).ok();
    }

    // Without `--copy-files` (plain `build`), a `.canvas.md` target is just
    // an ordinary, untouched link — exactly today's pre-`--copy-files`
    // behavior, not a new error mode for anyone who never passes the flag.
    #[test]
    fn without_copy_files_a_canvas_md_target_is_left_as_a_plain_link() {
        let dir = temp_dir("no-copy-files-canvas-target");
        write(&dir, "other.canvas.md", b"# Other\n<!-- meshfox:node id=\"root\" -->\n");
        let c = canvas(concat!(
            "# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
            "## Other\n<!-- meshfox:node id=\"f\" type=\"file\" -->\n\n[other](other.canvas.md)\n",
        ));

        let (site, assets) = build(&c, &dir, None);
        assert_eq!(
            site.find("f").unwrap().target.as_deref(),
            Some("other.canvas.md")
        );
        assert!(assets.is_empty(), "{assets:?}");

        fs::remove_dir_all(&dir).ok();
    }

    // TODO.canvas.md: "Опция рекурсивного рендеринга canvas-таргетов" —
    // `--recursive`'s own core support (`copy_files: true, recursive: true`).
    #[test]
    fn recursive_rewrites_a_sibling_canvas_target_to_its_own_rendered_page() {
        let dir = temp_dir("recursive-sibling");
        write(&dir, "other.canvas.md", b"# Other\n<!-- meshfox:node id=\"root\" -->\n");
        // `root_canvas_path` must exist on disk to canonicalize — a
        // non-canonical fallback here would desync from `resolved` (always
        // canonical, via `confine`) and spuriously hit the
        // `external-pages/` fallback (see `canvas_link_subdir`'s own doc
        // comment) even though `other.canvas.md` genuinely lives right next
        // to it.
        write(&dir, "doc.canvas.md", b"unused: build() is given `c` directly, not read from disk");
        let root_path = dir.join("doc.canvas.md");
        let c = canvas(concat!(
            "# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
            "## Other\n<!-- meshfox:node id=\"f\" type=\"file\" -->\n\n[other](other.canvas.md)\n",
        ));

        let (site, _assets, canvas_links) =
            build_for_static_export(&c, &dir, &root_path, "", None, None, true, true).unwrap();
        assert_eq!(
            site.find("f").unwrap().target.as_deref(),
            Some("other/index.html")
        );
        assert_eq!(canvas_links.len(), 1, "{canvas_links:?}");
        assert_eq!(canvas_links[0].subdir, "other");
        assert_eq!(
            canvas_links[0].resolved,
            dir.canonicalize().unwrap().join("other.canvas.md")
        );

        fs::remove_dir_all(&dir).ok();
    }

    // `other.canvas.md#some-node` deep-links to a specific node — carried
    // over as `#node-some-node`, matching the bundled template's own
    // per-node anchor convention (`site-template/_macros.html.tera`'s
    // `id="node-{{ n.id }}"`), not just dropped.
    #[test]
    fn recursive_carries_a_deep_link_fragment_over_as_a_node_anchor() {
        let dir = temp_dir("recursive-fragment");
        write(&dir, "other.canvas.md", b"# Other\n<!-- meshfox:node id=\"root\" -->\n");
        write(&dir, "doc.canvas.md", b"unused: build() is given `c` directly, not read from disk");
        let root_path = dir.join("doc.canvas.md");
        let c = canvas(concat!(
            "# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
            "## Other\n<!-- meshfox:node id=\"f\" type=\"file\" -->\n\n",
            "[other](other.canvas.md#some-node)\n",
        ));

        let (site, _assets, _canvas_links) =
            build_for_static_export(&c, &dir, &root_path, "", None, None, true, true).unwrap();
        assert_eq!(
            site.find("f").unwrap().target.as_deref(),
            Some("other/index.html#node-some-node")
        );

        fs::remove_dir_all(&dir).ok();
    }

    // A link back to the root canvas itself (directly, or as the closing
    // edge of a longer cycle) must resolve to wherever the root *actually*
    // renders (`--out`'s own top level, `current_slot: ""`) — not to a
    // nonexistent filename-derived subdirectory `canvas_link_subdir` would
    // naively compute for it like any other target.
    #[test]
    fn recursive_a_link_back_to_the_root_canvas_resolves_to_the_out_root() {
        let dir = temp_dir("recursive-root-cycle");
        let root_path = dir.join("doc.canvas.md");
        let c = canvas(concat!(
            "# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
            "## Self\n<!-- meshfox:node id=\"f\" type=\"file\" -->\n\n[self](doc.canvas.md)\n",
        ));
        write(&dir, "doc.canvas.md", b"unused: build() is given `c` directly, not read from disk");

        let (site, _assets, canvas_links) =
            build_for_static_export(&c, &dir, &root_path, "", None, None, true, true).unwrap();
        assert_eq!(
            site.find("f").unwrap().target.as_deref(),
            Some("./index.html")
        );
        assert_eq!(canvas_links[0].subdir, "");

        fs::remove_dir_all(&dir).ok();
    }

    // The same link, followed from a canvas already nested one level deep
    // (`current_slot: "other"`) — the relative path back to the root needs
    // an extra `../` to still land on `--out`'s own top level, not on
    // `other/index.html` (its own page) or `other/../index.html` written
    // out literally.
    #[test]
    fn recursive_a_link_back_to_the_root_from_a_nested_canvas_climbs_out_correctly() {
        let dir = temp_dir("recursive-root-cycle-nested");
        write(&dir, "doc.canvas.md", b"unused: build() is given `c` directly, not read from disk");
        let root_path = dir.join("doc.canvas.md");
        let c = canvas(concat!(
            "# Other\n<!-- meshfox:node id=\"root\" -->\n\n",
            "## Back\n<!-- meshfox:node id=\"f\" type=\"file\" -->\n\n[back](doc.canvas.md)\n",
        ));

        let (site, _assets, _canvas_links) = build_for_static_export(
            &c, &dir, &root_path, "other", None, None, true, true,
        )
        .unwrap();
        assert_eq!(
            site.find("f").unwrap().target.as_deref(),
            Some("../index.html")
        );

        fs::remove_dir_all(&dir).ok();
    }

    // A canvas link is confined to *its own linking node's* directory, same
    // boundary an image/asset reference already has (`crate::file_read::
    // confine`, via `resolve_canvas_relative`) — a `../other.canvas.md` that
    // would have to climb out of the linking canvas's own directory doesn't
    // resolve there either, `--recursive` or not, so it's left as a plain,
    // untouched link (the `None` branch of `resolve_file_target_for_copy`'s
    // own match, same "leave it alone" outcome an out-of-bounds image gets)
    // rather than either an error or a silently-broken rewritten href.
    #[test]
    fn recursive_a_canvas_target_outside_the_linking_nodes_own_directory_is_left_untouched() {
        let root_dir = temp_dir("recursive-confinement-root");
        let nested_dir = root_dir.join("nested");
        fs::create_dir_all(&nested_dir).unwrap();
        write(&root_dir, "doc.canvas.md", b"unused: build() is given `c` directly, not read from disk");
        write(&root_dir, "sibling.canvas.md", b"# Sibling\n<!-- meshfox:node id=\"root\" -->\n");
        let root_path = root_dir.join("doc.canvas.md");
        // `c` here stands in for `nested/b.canvas.md`'s own resolved
        // content — `canvas_dir` (`nested_dir`) is what confinement is
        // checked against, matching its real location on disk.
        let c = canvas(concat!(
            "# B\n<!-- meshfox:node id=\"root\" -->\n\n",
            "## Escape\n<!-- meshfox:node id=\"f\" type=\"file\" -->\n\n",
            "[sibling](../sibling.canvas.md)\n",
        ));

        let (site, _assets, canvas_links) = build_for_static_export(
            &c, &nested_dir, &root_path, "nested", None, None, true, true,
        )
        .unwrap();
        assert_eq!(
            site.find("f").unwrap().target.as_deref(),
            Some("../sibling.canvas.md"),
            "escaping the linking node's own directory must leave the link untouched, not rewrite or error"
        );
        assert!(canvas_links.is_empty(), "{canvas_links:?}");

        fs::remove_dir_all(&root_dir).ok();
    }

    #[test]
    fn relative_slot_path_from_root_to_a_nested_slot_is_just_the_slot() {
        assert_eq!(relative_slot_path("", "sub/other"), "sub/other");
    }

    #[test]
    fn relative_slot_path_between_sibling_nested_slots_climbs_out_once() {
        assert_eq!(relative_slot_path("sub/b", "sub/other"), "../other");
    }

    #[test]
    fn relative_slot_path_from_a_nested_slot_to_root_climbs_out_fully() {
        assert_eq!(relative_slot_path("sub/b", ""), "../..");
    }

    #[test]
    fn relative_slot_path_between_identical_slots_is_dot() {
        assert_eq!(relative_slot_path("a/b", "a/b"), ".");
    }

    #[test]
    fn canvas_link_subdir_outside_the_root_dir_is_still_deterministic() {
        let root_dir = temp_dir("canvas-link-subdir-root");
        let outside = temp_dir("canvas-link-subdir-outside").join("other.canvas.md");
        let a = canvas_link_subdir(&outside, &root_dir);
        let b = canvas_link_subdir(&outside, &root_dir);
        assert_eq!(a, b, "same inputs must give the same subdir every time");
        assert!(a.starts_with("external-pages/"), "{a}");

        fs::remove_dir_all(&root_dir).ok();
        fs::remove_dir_all(outside.parent().unwrap()).ok();
    }

    #[test]
    fn external_image_url_is_never_queued_as_an_asset() {
        let dir = temp_dir("image-external");
        let c = canvas(
            "# Root\n<!-- meshfox:node id=\"root\" -->\n\n![x](https://example.com/x.png)\n",
        );

        let (site, assets) = build(&c, &dir, None);
        assert!(
            site.find("root")
                .unwrap()
                .html_body
                .contains("src=\"https://example.com/x.png\""),
            "{}",
            site.find("root").unwrap().html_body
        );
        assert!(assets.is_empty());

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn display_code_file_node_gets_its_target_inlined() {
        let dir = temp_dir("code-preview");
        write(&dir, "hello.rs", b"fn main() {\n    println!(\"hi\");\n}\n");
        let c = canvas(
            "# Root\n<!-- meshfox:node id=\"root\" -->\n\n\
             ## Src\n<!-- meshfox:node id=\"src\" type=\"file\" display=\"code\" -->\n\n[hello.rs](hello.rs)\n",
        );

        let (site, assets) = build(&c, &dir, None);
        let src = site.find("src").unwrap();
        assert!(src.html_body.contains("language-rust"), "{}", src.html_body);
        assert!(src.html_body.contains("println!"), "{}", src.html_body);
        // Inlined, not copied — no separate asset needed for a code preview.
        assert!(assets.is_empty());

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn display_code_file_node_puts_its_caption_above_the_preview() {
        let dir = temp_dir("code-preview-caption");
        write(&dir, "hello.rs", b"fn main() {}\n");
        let c = canvas(
            "# Root\n<!-- meshfox:node id=\"root\" -->\n\n\
             ## Src\n<!-- meshfox:node id=\"src\" type=\"file\" display=\"code\" -->\n\n\
             [hello.rs](hello.rs)\n\nA short **caption**.\n",
        );

        let (site, _assets) = build(&c, &dir, None);
        let src = site.find("src").unwrap();
        assert!(src.html_body.contains("language-rust"), "{}", src.html_body);
        let caption_at = src
            .html_body
            .find("<strong>caption</strong>")
            .unwrap_or_else(|| panic!("caption missing: {}", src.html_body));
        let preview_at = src
            .html_body
            .find("language-rust")
            .unwrap_or_else(|| panic!("preview missing: {}", src.html_body));
        assert!(
            caption_at < preview_at,
            "caption should render above the file preview: {}",
            src.html_body
        );

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn link_node_renders_its_caption_below_the_plain_link() {
        // No dedicated branch for this in `build_node_view` — a `link`
        // node's `html_body` is just `render_markdown(&node.text, ...)`
        // whole, so the caption (already part of `node.text`) comes along
        // for free; this is the regression test for that staying true.
        let c = canvas(
            "# Root\n<!-- meshfox:node id=\"root\" -->\n\n\
             ## Post\n<!-- meshfox:node id=\"post\" type=\"link\" -->\n\n\
             [example](https://example.com)\n\nA short note.\n",
        );

        let site = build_site(&c);
        let post = site.find("post").unwrap();
        assert!(post.html_body.contains("example.com"), "{}", post.html_body);
        assert!(post.html_body.contains("A short note."), "{}", post.html_body);
    }

    #[test]
    fn display_code_escapes_html_in_the_previewed_content() {
        let dir = temp_dir("code-preview-escape");
        write(&dir, "snippet.html", b"<script>alert(1)</script>");
        let c = canvas(
            "# Root\n<!-- meshfox:node id=\"root\" -->\n\n\
             ## Src\n<!-- meshfox:node id=\"src\" type=\"file\" display=\"code\" -->\n\n[snippet.html](snippet.html)\n",
        );

        let (site, _assets) = build(&c, &dir, None);
        let src = site.find("src").unwrap();
        assert!(
            src.html_body.contains("&lt;script&gt;"),
            "{}",
            src.html_body
        );
        assert!(
            !src.html_body.contains("<script>alert"),
            "{}",
            src.html_body
        );

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn display_code_falls_back_to_a_link_for_a_binary_target() {
        let dir = temp_dir("code-preview-binary");
        write(&dir, "blob.bin", &[0u8, 1, 2, 3, 0, 5]);
        let c = canvas(
            "# Root\n<!-- meshfox:node id=\"root\" -->\n\n\
             ## Src\n<!-- meshfox:node id=\"src\" type=\"file\" display=\"code\" -->\n\n[blob.bin](blob.bin)\n",
        );

        let (site, _assets) = build(&c, &dir, None);
        let src = site.find("src").unwrap();
        assert!(!src.html_body.contains("<pre>"), "{}", src.html_body);
        assert!(
            src.html_body.contains("href=\"blob.bin\""),
            "{}",
            src.html_body
        );

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn display_code_falls_back_to_a_link_when_the_target_is_missing() {
        let dir = temp_dir("code-preview-missing");
        let c = canvas(
            "# Root\n<!-- meshfox:node id=\"root\" -->\n\n\
             ## Src\n<!-- meshfox:node id=\"src\" type=\"file\" display=\"code\" -->\n\n[nope.rs](nope.rs)\n",
        );

        let (site, _assets) = build(&c, &dir, None);
        let src = site.find("src").unwrap();
        assert!(
            src.html_body.contains("href=\"nope.rs\""),
            "{}",
            src.html_body
        );

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn links_base_url_prefixes_a_relative_link_left_uncopied() {
        let dir = temp_dir("base-url-link");
        let c = canvas("# Root\n<!-- meshfox:node id=\"root\" -->\n\n[LICENSE](./LICENSE)\n");

        let (site, _assets) = build(&c, &dir, Some("https://example.com/repo"));
        assert!(
            site.find("root")
                .unwrap()
                .html_body
                .contains("href=\"https://example.com/repo/LICENSE\""),
            "{}",
            site.find("root").unwrap().html_body
        );

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn links_base_url_never_touches_a_copied_image() {
        let dir = temp_dir("base-url-image");
        write(&dir, "shot.png", b"bytes");
        let c = canvas("# Root\n<!-- meshfox:node id=\"root\" -->\n\n![Screenshot](shot.png)\n");

        let (site, assets) = build(&c, &dir, Some("https://example.com/repo"));
        assert!(
            site.find("root")
                .unwrap()
                .html_body
                .contains("src=\"shot.png\""),
            "{}",
            site.find("root").unwrap().html_body
        );
        assert_eq!(assets.len(), 1);

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn links_base_url_never_touches_an_external_link() {
        let dir = temp_dir("base-url-external");
        let c = canvas("# Root\n<!-- meshfox:node id=\"root\" -->\n\n[meshfox](https://github.com/example/meshfox)\n");

        let (site, _assets) = build(&c, &dir, Some("https://example.com/repo"));
        assert!(
            site.find("root")
                .unwrap()
                .html_body
                .contains("href=\"https://github.com/example/meshfox\""),
            "{}",
            site.find("root").unwrap().html_body
        );

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn links_base_url_prefixes_a_link_type_nodes_target_too() {
        let dir = temp_dir("base-url-link-node");
        let c = canvas(
            "# Root\n<!-- meshfox:node id=\"root\" -->\n\n\
             ## Homepage\n<!-- meshfox:node id=\"homepage\" type=\"link\" -->\n\n[home](./docs/home.html)\n",
        );

        let (site, _assets) = build(&c, &dir, Some("https://example.com/repo"));
        let homepage = site.find("homepage").unwrap();
        assert_eq!(
            homepage.target.as_deref(),
            Some("https://example.com/repo/docs/home.html")
        );

        fs::remove_dir_all(&dir).ok();
    }
}
