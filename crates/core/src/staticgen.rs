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
//! Local-file references (`build`'s `canvas_dir`/`base_url` parameters) get
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
//!     unless `base_url` is set, in which case it's prefixed with it —
//!     these were never resolved through the canvas's own directory to
//!     begin with (an ordinary link can point anywhere, including outside
//!     `canvas_dir`), so there's nothing here to copy; `base_url` is the
//!     escape hatch for "this needs to resolve against wherever the source
//!     repo/site actually lives" instead.

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
    /// Absolute, canonicalized, `canvas_dir`-confined source path.
    pub source: PathBuf,
    /// Destination path relative to the site's output root, using forward
    /// slashes regardless of host OS — also what the rendered HTML's `src`
    /// now points to.
    pub dest_rel: String,
}

/// Builds a `SiteData` from `canvas` (its node tree, each node's body
/// rendered to HTML), plus every `Asset` (local image) it needs copied
/// alongside the rendered HTML.
///
/// `canvas_dir` is the directory the canvas file itself lives in — the same
/// root a relative image/file reference resolves against, confined the same
/// way the server confines it (see module docs). `base_url`, if set, is
/// prefixed onto whatever relative reference is left over (see module
/// docs) — pass `None` for a self-contained site meant to be opened as-is.
pub fn build(canvas: &Canvas, canvas_dir: &Path, base_url: Option<&str>) -> (SiteData, Vec<Asset>) {
    let canvas_dir = canvas_dir.canonicalize().unwrap_or_else(|_| canvas_dir.to_path_buf());

    let root_node = canvas.nodes.iter().find(|n| n.parent.is_none()).expect("a parsed canvas always has a root");
    let (folded_ids, foldable_ids) = resolve_default_fold(canvas, &root_node.id);
    let ctx = RenderCtx { canvas_dir: &canvas_dir, base_url, folded_ids: &folded_ids, foldable_ids: &foldable_ids };

    let mut assets: Vec<Asset> = Vec::new();
    let root = build_node_view(canvas, root_node, 0, &ctx, &mut assets);
    let edges = build_edges(canvas);
    let title = root.title.clone();

    // Same image referenced by more than one node (or more than once in the
    // same body) would otherwise queue a redundant copy — keep the first.
    let mut seen = std::collections::HashSet::new();
    assets.retain(|a| seen.insert(a.dest_rel.clone()));

    (SiteData { title, root, edges }, assets)
}

fn build_node_view(canvas: &Canvas, node: &Node, depth: u32, ctx: &RenderCtx, assets: &mut Vec<Asset>) -> NodeView {
    let (html_body, target) = if node.node_type == NodeType::Group {
        (String::new(), None)
    } else if node.node_type == NodeType::File && node.display == Some(FileDisplay::Code) {
        (render_file_code(node, ctx.canvas_dir), node.target.clone())
    } else {
        let html_body = render_markdown(&node.text, ctx, assets);
        let target = node.target.as_deref().map(|t| resolve_link_url(t, ctx));
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
    let position = match (canvas.resolve_absolute_position(&node.id), node.width, node.height) {
        (Some((x, y)), Some(width), Some(height)) => Some(Position { x, y, width, height }),
        _ => None,
    };
    let children = canvas
        .children(&node.id)
        .into_iter()
        .map(|c| build_node_view(canvas, c, depth + 1, ctx, assets))
        .collect();

    NodeView {
        id: node.id.clone(),
        title: node.title.clone(),
        level: node.level,
        node_type: node.node_type.as_str(),
        depth,
        position,
        color: node.color.clone(),
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
fn resolve_default_fold(canvas: &Canvas, root_id: &str) -> (std::collections::HashSet<String>, std::collections::HashSet<String>) {
    let has_unfold_option = crate::options::declared_options(canvas).map(|opts| opts.iter().any(|o| o == "unfold")).unwrap_or(false);
    // Ids that are somebody's structural `parent` — mirrors
    // `web/src/App.tsx`'s `nodesWithChildren`.
    let with_children: std::collections::HashSet<&str> = canvas.nodes.iter().filter_map(|n| n.parent.as_deref()).collect();

    let mut folded = std::collections::HashSet::new();
    let mut foldable = std::collections::HashSet::new();
    for n in &canvas.nodes {
        let can_fold = !is_title_only_node(n) || with_children.contains(n.id.as_str());
        if can_fold {
            foldable.insert(n.id.clone());
        }
        let has_explicit_size = n.width.is_some() || n.height.is_some();
        let resolved = n.fold.unwrap_or_else(|| n.id != root_id && !has_unfold_option && !has_explicit_size && can_fold);
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
                arrow_end: extra.arrow_end.map(|a| matches!(a, ArrowEnd::Arrow)).unwrap_or(true),
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
    base_url: Option<&'a str>,
    /// See `resolve_default_fold` — computed once in `build`, consulted per
    /// node by `build_node_view`.
    folded_ids: &'a std::collections::HashSet<String>,
    foldable_ids: &'a std::collections::HashSet<String>,
}

/// GFM-flavored Markdown -> HTML, with meshfox's own fence attributes
/// stripped first (see `crate::fence::strip_fence_attrs`) so a runnable
/// block's `name=`/`cache`/`deps=` don't leak into the rendered
/// `class="language-..."`, and every image/link URL rewritten per `ctx`
/// (see `resolve_image_url`/`resolve_link_url`). Every link additionally
/// gets `target="_blank"` (see `add_target_blank`).
fn render_markdown(text: &str, ctx: &RenderCtx, assets: &mut Vec<Asset>) -> String {
    use pulldown_cmark::{html, Event, Options, Parser, Tag};
    let stripped = crate::fence::strip_fence_attrs(text);
    let options =
        Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_FOOTNOTES | Options::ENABLE_TASKLISTS;
    let events: Vec<Event> = Parser::new_ext(&stripped, options)
        .map(|event| match event {
            Event::Start(Tag::Image { link_type, dest_url, title, id }) => {
                let new_url = resolve_image_url(&dest_url, ctx, assets);
                Event::Start(Tag::Image { link_type, dest_url: new_url.into(), title, id })
            }
            Event::Start(Tag::Link { link_type, dest_url, title, id }) => {
                let new_url = resolve_link_url(&dest_url, ctx);
                Event::Start(Tag::Link { link_type, dest_url: new_url.into(), title, id })
            }
            other => other,
        })
        .collect();
    let mut out = String::new();
    html::push_html(&mut out, events.into_iter());
    add_target_blank(&out)
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

/// A relative reference resolved under `canvas_dir` and confined to it,
/// mirroring `crates/server/src/lib.rs`'s `serve_canvas_relative_file`/
/// `get_node_file_content`: canonicalized, then rejected unless it still
/// starts with `canvas_dir` (so a `../../etc/passwd`-style target — or an
/// absolute path pointing outside the tree — resolves to `None` rather than
/// escaping it) and is a real file. `canvas_dir` is assumed already
/// canonicalized (see `build`).
fn resolve_canvas_relative(url: &str, canvas_dir: &Path) -> Option<PathBuf> {
    let clean = url.split(['?', '#']).next().unwrap_or(url);
    if clean.is_empty() {
        return None;
    }
    let resolved = canvas_dir.join(clean).canonicalize().ok()?;
    (resolved.starts_with(canvas_dir) && resolved.is_file()).then_some(resolved)
}

/// `resolved`'s path relative to `canvas_dir`, using forward slashes
/// regardless of host OS (so it's a valid URL path component, not just a
/// valid local filesystem path) — always succeeds for a `resolved` that
/// came out of `resolve_canvas_relative`, which already guarantees the
/// `canvas_dir` prefix.
fn dest_rel_for(resolved: &Path, canvas_dir: &Path) -> String {
    resolved.strip_prefix(canvas_dir).unwrap_or(resolved).to_string_lossy().replace('\\', "/")
}

/// True for anything this module leaves untouched no matter what: an
/// in-page anchor, a URL with a scheme (`https://`, `mailto:`, `data:`,
/// ...), or a root-absolute path (ambiguous once site root and repo root
/// are different things — not this module's call to resolve).
fn is_external_or_absolute(url: &str) -> bool {
    url.starts_with('#') || url.starts_with('/') || url.contains("://") || url.starts_with("mailto:") || url.starts_with("data:")
}

/// A Markdown image's `src`: copies the referenced local file (queuing an
/// `Asset`) and points `src` at that copy, or — external/unresolvable —
/// leaves the URL exactly as written (never `base_url`-prefixed: an image
/// that couldn't be resolved under `canvas_dir` was never going to be
/// bundled, and `base_url` is for the deliberately-left-alone case, not the
/// failed-to-copy one).
fn resolve_image_url(url: &str, ctx: &RenderCtx, assets: &mut Vec<Asset>) -> String {
    if is_external_or_absolute(url) {
        return url.to_string();
    }
    match resolve_canvas_relative(url, ctx.canvas_dir) {
        Some(resolved) => {
            let dest_rel = dest_rel_for(&resolved, ctx.canvas_dir);
            assets.push(Asset { source: resolved, dest_rel: dest_rel.clone() });
            dest_rel
        }
        None => url.to_string(),
    }
}

/// A plain link's `href` (Markdown or a `file`/`link` node's own target):
/// left untouched unless it's relative and `base_url` is set, in which case
/// it's prefixed with it. Never copies anything — see the module doc
/// comment for why plain links get this lighter treatment than images.
fn resolve_link_url(url: &str, ctx: &RenderCtx) -> String {
    if is_external_or_absolute(url) {
        return url.to_string();
    }
    match ctx.base_url {
        Some(base) => format!("{}/{}", base.trim_end_matches('/'), url.trim_start_matches("./")),
        None => url.to_string(),
    }
}

/// Same cap the server's `get_node_file_content` reads at most, for a
/// `display="code"` preview.
const FILE_CONTENT_MAX_BYTES: usize = 1_000_000;

/// Inline `<pre><code>` replacement for a `file`-type node's `display="code"`
/// preview (`web/src/MeshNode.tsx`'s `FileCodePreview`, backed there by a
/// live `GET /api/nodes/:id/file-content` fetch — nothing to fetch from
/// once static, so this reads the target once at build time instead, same
/// confinement/binary-sniff/size-cap as that route). Falls back to a plain
/// link (same as the web UI falls back to an error message) when the
/// target is missing, unreadable, outside `canvas_dir`, or looks binary.
fn render_file_code(node: &Node, canvas_dir: &Path) -> String {
    let Some(target) = node.target.as_deref() else {
        return "<p><em>no target</em></p>".to_string();
    };
    let fallback_link =
        || format!("<p><a target=\"_blank\" rel=\"noopener noreferrer\" href=\"{0}\">{0}</a></p>", html_escape(target));

    let Some(resolved) = resolve_canvas_relative(target, canvas_dir) else {
        return fallback_link();
    };
    let Ok(bytes) = std::fs::read(&resolved) else {
        return fallback_link();
    };
    // Same cheap "null byte in a representative prefix" heuristic the
    // server route uses to keep an accidental binary target from getting
    // shoved into a code block as mangled text.
    let sample_len = bytes.len().min(8000);
    if bytes[..sample_len].contains(&0) {
        return fallback_link();
    }

    let truncated = bytes.len() > FILE_CONTENT_MAX_BYTES;
    let slice = &bytes[..bytes.len().min(FILE_CONTENT_MAX_BYTES)];
    let content = String::from_utf8_lossy(slice);
    let lang = node.lang.clone().unwrap_or_else(|| guess_lang(target));
    let class_attr = if lang.is_empty() { String::new() } else { format!(" class=\"language-{}\"", html_escape(&lang)) };
    let note = if truncated { "<p class=\"file-preview-truncated\">(truncated)</p>" } else { "" };
    format!("<pre><code{class_attr}>{}</code></pre>{note}", html_escape(&content))
}

/// Extension-based language guess for a `display="code"` preview whose node
/// has no explicit `lang=` — the static-render substitute for the web UI's
/// `pickLanguage`/CodeMirror `LanguageDescription.matchFilename` (no
/// language-data table to match against here, just a small, common-case
/// map); an unrecognized extension gets no `class` at all, same as
/// CodeMirror returning no match there.
fn guess_lang(path: &str) -> String {
    let ext = Path::new(path).extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
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
    /// `display="code"` targets against) and no `base_url` — what every
    /// test that isn't specifically about those two features wants.
    fn build_site(c: &Canvas) -> SiteData {
        build(c, Path::new("/nonexistent-meshfox-test-dir"), None).0
    }

    /// A fresh temp directory for a test that needs real files on disk —
    /// same pattern `include.rs`'s own tests use, `tag` keeping concurrent
    /// tests in this module from colliding on the same path.
    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("meshfox-staticgen-test-{tag}-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write(dir: &Path, name: &str, contents: &[u8]) {
        fs::write(dir.join(name), contents).unwrap();
    }

    #[test]
    fn maps_basic_node_fields() {
        let c = canvas("# Root\n<!-- meshfox:node id=\"root\" color=\"red\" tags=\"a,b\" -->\n\nhello\n");
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
        let pos = site.find("root").unwrap().position.expect("all four values were set");
        assert_eq!((pos.x, pos.y, pos.width, pos.height), (10.0, 20.0, 200.0, 80.0));
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
        let pos = site.find("member").unwrap().position.expect("group has a real anchor");
        assert_eq!((pos.x, pos.y, pos.width, pos.height), (1020.0, 1020.0, 100.0, 80.0));
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
        let c = canvas("# Root\n\n## Child\n<!-- meshfox:node id=\"child\" x=10 y=20 w=200 h=80 -->\n\nbody\n");
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
        assert!(block.html_body.contains("language-bash"), "{}", block.html_body);
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
        assert!(site.find("root").unwrap().html_body.contains("src=\"shot.png\""), "{}", site.find("root").unwrap().html_body);
        assert_eq!(assets.len(), 1);
        assert_eq!(assets[0].dest_rel, "shot.png");
        assert_eq!(fs::read(&assets[0].source).unwrap(), b"not a real png, just bytes");

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn image_outside_canvas_dir_is_left_unresolved_and_not_queued() {
        let dir = temp_dir("image-escape");
        let c = canvas("# Root\n<!-- meshfox:node id=\"root\" -->\n\n![x](../../etc/passwd)\n");

        let (site, assets) = build(&c, &dir, None);
        assert!(site.find("root").unwrap().html_body.contains("src=\"../../etc/passwd\""), "{}", site.find("root").unwrap().html_body);
        assert!(assets.is_empty());

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn external_image_url_is_never_queued_as_an_asset() {
        let dir = temp_dir("image-external");
        let c = canvas("# Root\n<!-- meshfox:node id=\"root\" -->\n\n![x](https://example.com/x.png)\n");

        let (site, assets) = build(&c, &dir, None);
        assert!(site.find("root").unwrap().html_body.contains("src=\"https://example.com/x.png\""), "{}", site.find("root").unwrap().html_body);
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
    fn display_code_escapes_html_in_the_previewed_content() {
        let dir = temp_dir("code-preview-escape");
        write(&dir, "snippet.html", b"<script>alert(1)</script>");
        let c = canvas(
            "# Root\n<!-- meshfox:node id=\"root\" -->\n\n\
             ## Src\n<!-- meshfox:node id=\"src\" type=\"file\" display=\"code\" -->\n\n[snippet.html](snippet.html)\n",
        );

        let (site, _assets) = build(&c, &dir, None);
        let src = site.find("src").unwrap();
        assert!(src.html_body.contains("&lt;script&gt;"), "{}", src.html_body);
        assert!(!src.html_body.contains("<script>alert"), "{}", src.html_body);

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
        assert!(src.html_body.contains("href=\"blob.bin\""), "{}", src.html_body);

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
        assert!(src.html_body.contains("href=\"nope.rs\""), "{}", src.html_body);

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn base_url_prefixes_a_relative_link_left_uncopied() {
        let dir = temp_dir("base-url-link");
        let c = canvas("# Root\n<!-- meshfox:node id=\"root\" -->\n\n[LICENSE](./LICENSE)\n");

        let (site, _assets) = build(&c, &dir, Some("https://example.com/repo"));
        assert!(site.find("root").unwrap().html_body.contains("href=\"https://example.com/repo/LICENSE\""), "{}", site.find("root").unwrap().html_body);

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn base_url_never_touches_a_copied_image() {
        let dir = temp_dir("base-url-image");
        write(&dir, "shot.png", b"bytes");
        let c = canvas("# Root\n<!-- meshfox:node id=\"root\" -->\n\n![Screenshot](shot.png)\n");

        let (site, assets) = build(&c, &dir, Some("https://example.com/repo"));
        assert!(site.find("root").unwrap().html_body.contains("src=\"shot.png\""), "{}", site.find("root").unwrap().html_body);
        assert_eq!(assets.len(), 1);

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn base_url_never_touches_an_external_link() {
        let dir = temp_dir("base-url-external");
        let c = canvas("# Root\n<!-- meshfox:node id=\"root\" -->\n\n[meshfox](https://github.com/example/meshfox)\n");

        let (site, _assets) = build(&c, &dir, Some("https://example.com/repo"));
        assert!(site.find("root").unwrap().html_body.contains("href=\"https://github.com/example/meshfox\""), "{}", site.find("root").unwrap().html_body);

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn base_url_prefixes_a_link_type_nodes_target_too() {
        let dir = temp_dir("base-url-link-node");
        let c = canvas(
            "# Root\n<!-- meshfox:node id=\"root\" -->\n\n\
             ## Homepage\n<!-- meshfox:node id=\"homepage\" type=\"link\" -->\n\n[home](./docs/home.html)\n",
        );

        let (site, _assets) = build(&c, &dir, Some("https://example.com/repo"));
        let homepage = site.find("homepage").unwrap();
        assert_eq!(homepage.target.as_deref(), Some("https://example.com/repo/docs/home.html"));

        fs::remove_dir_all(&dir).ok();
    }
}
