//! meshfox's primary on-disk format: a single Markdown document where
//! heading nesting doubles as the node tree, and meshfox's own bookkeeping
//! (stable id, canvas position, extra graph edges) rides along in HTML
//! comments right after each heading. See README.md for the full writeup.
//!
//! A heading is only a node if it's immediately followed by a
//! `<!-- meshfox:node ... -->` comment (the document's first heading is
//! the sole exception — it's always the root). An unmarked heading is just
//! Markdown formatting and stays part of its enclosing node's body, so
//! headings can be used freely inside prose without turning into canvas
//! nodes by accident.
//!
//! ```text
//! # Root
//! <!-- meshfox:node id="root" x=0 y=0 w=250 h=60 -->
//!
//! body text for the root node
//!
//! ## Section
//! <!-- meshfox:node id="section" x=280 y=0 w=250 h=60 -->
//! <!-- meshfox:edge from="some-other-node" -->
//!
//! body text for "section" — it has two parents: Root (from nesting) and
//! some-other-node (from the extra edge line).
//! ```
//!
//! `parse` builds the in-memory `Canvas`. Prefer the surgical
//! `set_node_body` / `set_node_meta` over a full `render` when patching an
//! *existing* file — they touch only the target node's own lines, leaving
//! everything else in the document byte-for-byte untouched. `render` is for
//! producing a document from scratch.

use crate::attrs::parse_attrs;
use crate::canvas::{
    slugify, ArrowEnd, Canvas, EdgeLineStyle, ExtraEdge, FileDisplay, Node, NodeType,
};
use crate::fence::{scan_runnable_blocks, BlockRef, EnvRef};
use std::collections::{HashMap, HashSet};
use std::ops::Range;
use thiserror::Error;

/// Optional first-line marker identifying a `.md` file as a meshfox canvas
/// even when it isn't named `*.canvas.md` (e.g. this project's own
/// README.md). Purely a hint for discovery tooling — `parse` never
/// requires it, since heading structure alone is enough to parse a
/// document.
pub const CANVAS_MARKER: &str = "<!-- meshfox:canvas -->";

/// Whether `markdown` opens with the `meshfox:canvas` marker line.
pub fn has_marker(markdown: &str) -> bool {
    let first_line = markdown.lines().next().unwrap_or("");
    parse_comment(first_line, "meshfox:canvas").is_some()
}

/// Whether `path`'s own name marks it as a canvas without having to read
/// it — the cheap half of the "is this a canvas" check, split out so a
/// caller that only has a `*.canvas.md` candidate (or wants to avoid a read
/// for a file it's about to reject anyway) doesn't have to read the file
/// just to find out. A plain `*.md` file still needs [`has_marker`] on its
/// actual contents — see [`is_canvas`].
pub fn is_canvas_path(path: &std::path::Path) -> bool {
    path.to_string_lossy().ends_with(".canvas.md")
}

/// Whether `path` (whose `contents` the caller already has in hand) counts
/// as a canvas: either its name ends in `.canvas.md`, or it's a plain `.md`
/// file that opens with the `meshfox:canvas` marker (see [`has_marker`]) —
/// this is how e.g. README.md itself can be treated as a canvas without
/// being renamed. The one rule used, up to now, independently by
/// `include::resolve`/`include::list_includes` and the CLI's own
/// `find_canvas()`.
pub fn is_canvas(path: &std::path::Path, contents: &str) -> bool {
    is_canvas_path(path) || has_marker(contents)
}

/// Splits a `file`/`link` node's `target` on its first `#` into the path
/// portion and an optional fragment — e.g. `other.canvas.md#some-node` deep
/// links to node `some-node` in that canvas. The fragment must be stripped
/// before the path portion is resolved against the filesystem (`confine`/
/// `resolve_confined_target`), since neither knows about node ids.
pub fn split_target_fragment(target: &str) -> (&str, Option<&str>) {
    match target.split_once('#') {
        Some((path, fragment)) => (path, Some(fragment)),
        None => (target, None),
    }
}

#[derive(Debug, Error, PartialEq)]
pub enum ParseError {
    #[error("document has no top-level (#) heading to serve as the root node")]
    NoRoot,
    #[error("document has more than one top-level (#) heading; meshfox expects exactly one root")]
    MultipleRoots,
    #[error("duplicate node id {0:?}")]
    DuplicateId(String),
    #[error(
        "node {0:?} declares an edge from={1:?} but no node with that id exists in the document"
    )]
    UnknownParent(String, String),
    #[error("node {0:?} declares parent={1:?} but no node with that id exists in the document")]
    UnknownExplicitParent(String, String),
    #[error("node {0:?}'s parent chain (via explicit parent= overrides) cycles back to itself")]
    CyclicParent(String),
    #[error("node {0:?} has unknown type={1:?} (expected text, file, link, group, or include)")]
    UnknownNodeType(String, String),
    #[error("node {0:?} sets preview= but only link nodes support a social preview")]
    PreviewRequiresLinkType(String),
    #[error("group node {0:?} must have an empty body (groups are purely organizational)")]
    GroupHasBody(String),
    #[error(
        "{1} node {0:?} must have a body that is exactly one Markdown link, e.g. [label](target)"
    )]
    InvalidLinkBody(String, &'static str),
    #[error("id {0:?} contains a forbidden character (a `\"`, a `,`, or a control character)")]
    InvalidId(String),
    #[error(
        "node {0:?} has an invalid {1}={2:?} — expected RFC3339, e.g. \"2026-08-29T10:15:00Z\" \
         or with an explicit offset like \"2026-08-29T13:15:00+03:00\""
    )]
    InvalidTimestamp(String, &'static str, String),
}

/// Characters an id can never contain, whether typed by hand in an explicit
/// `id="..."` attribute or passed to `rename_node_id`/`clear_node_id`: `"`
/// would corrupt the attribute's own quoting (see `attrs.rs`, which has no
/// escaping), and `,` would be silently torn into multiple ids wherever an
/// id list is comma-split (`meshfox:edge from="a,b"`, `deps="id/block,..."`
/// — see `rewrite_deps_node_id` below). Control characters (tab, newline,
/// ...) are rejected too, out of caution — nothing legitimate needs one in
/// an id. Plain spaces and any non-Latin script are fine.
fn id_has_forbidden_char(id: &str) -> bool {
    id.chars().any(|c| c == '"' || c == ',' || c.is_control())
}

impl Canvas {
    pub fn from_markdown(markdown: &str) -> Result<Canvas, ParseError> {
        parse(markdown)
    }

    /// Full regeneration from the in-memory model. Prefer `set_node_body`
    /// / `set_node_meta` when patching a file that already exists.
    pub fn to_markdown(&self) -> String {
        render(self)
    }
}

/// Canvas-position/style fields settable via `set_node_meta`. `None` means
/// "leave unset" (the attribute is omitted) for `x`/`y`/`width`/`height`/
/// `color`, or "preserve whatever's already on the line" for `node_type` —
/// pass the previous value through for any field you don't want to change.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct NodeMeta {
    pub x: Option<f64>,
    pub y: Option<f64>,
    pub width: Option<f64>,
    pub height: Option<f64>,
    pub color: Option<String>,
    /// `Some(t)` writes/overwrites `type="..."` (omitted when `t` is the
    /// default `Text`); `None` carries over whatever `type=` (if any) was
    /// already on the line, same as every other field here left unset.
    pub node_type: Option<NodeType>,
    /// `file`-node display mode (see `FileDisplay`). `None` means "leave
    /// unset" (omitted, same as `x`/`y`/`width`/`height`/`color`) — the
    /// caller is responsible for passing the existing value through when it
    /// wants it preserved rather than dropped.
    pub display: Option<FileDisplay>,
    /// `file`-node syntax-highlighting language hint. Same "omitted unless
    /// set" contract as `display`.
    pub lang: Option<String>,
    /// `file`-node interpreter (see `Node::is_runnable_file`). Same
    /// "omitted unless set" contract as `display`/`lang`.
    pub interpreter: Option<String>,
    /// `link`-node social preview toggle (see `Node::preview`). `None`
    /// omits `preview=` entirely (off); same "caller passes through the
    /// existing value to keep it" contract as `display`/`lang`/
    /// `interpreter` — `Some(false)` also omits (it's the default), only
    /// `Some(true)` writes `preview="true"`.
    pub preview: Option<bool>,
    /// Structural-edge label (see `Node::edge_label`). Same "omitted unless
    /// set" contract as `display`/`lang`/`interpreter`.
    pub edge_label: Option<String>,
    /// Free-form tags. Empty means "omitted" — same "caller passes through
    /// the existing value to keep it" contract as every other field here.
    pub tags: Vec<String>,
    /// `createdAt=` override (see `Node::created_at`) — `node meta
    /// --created-at` is the only writer; validated as RFC3339 before it
    /// ever reaches this struct (`crate::timestamp::is_valid_rfc3339`).
    /// Same "omitted unless set" contract as `display`/`lang`/`interpreter`.
    /// `updatedAt=` has no equivalent field here — it's never
    /// caller-settable, only auto-touched by `set_node_body` (see its own
    /// doc comment) via a dedicated raw-attribute rewrite, not this struct.
    pub created_at: Option<String>,
    /// Per-node fold-state override (see `Node::fold`). Same "omitted
    /// unless set" contract as `display`/`lang`/`interpreter` — `None`
    /// omits `fold=` entirely (no override; follows the document
    /// default), `Some(true)`/`Some(false)` write an explicit
    /// `fold="true"`/`fold="false"`.
    pub fold: Option<bool>,
}

/// Parses the `"true"`/`"false"`/`"default"` string sentinel used
/// wherever a caller needs to distinguish "not touching `fold` at all"
/// (simply not calling this — see `NodeMeta::fold`'s own doc comment)
/// from "explicitly clearing it back to no override" (`"default"`,
/// `Ok(None)`) — shared by the CLI's `node meta --fold` and the server's
/// node-update endpoint so both interpret the same three spellings
/// identically.
pub fn parse_fold_override(raw: &str) -> Result<Option<bool>, String> {
    match raw {
        "true" => Ok(Some(true)),
        "false" => Ok(Some(false)),
        "default" => Ok(None),
        other => Err(format!(
            "fold must be \"true\", \"false\", or \"default\", got {other:?}"
        )),
    }
}

/// Every attribute name `parse` actually reads off a `meshfox:node`
/// comment (`NodeMeta`'s own fields plus the explicit `parent=` override)
/// — the canonical vocabulary `unknown_node_edge_attr` (below, `validate`-
/// only) diffs a comment's own attribute keys against. Also what
/// `crates/cli/src/tui/source_editor.rs`'s `Ctrl-p` popup mirrors, for the
/// same reason that file's own copy explains: no single list survived
/// there to import before this one existed to import from.
pub const NODE_ATTRS: &[&str] = &[
    "id",
    "type",
    "x",
    "y",
    "w",
    "h",
    "color",
    "tags",
    "parent",
    "display",
    "lang",
    "interpreter",
    "preview",
    "fold",
    "edgeLabel",
    "createdAt",
    "updatedAt",
];
/// `meshfox:edge`'s own attribute vocabulary — `from` is required, the
/// rest optional styling (`canvas.rs`'s `ExtraEdge`/style enums).
pub const EDGE_ATTRS: &[&str] = &["from", "label", "color", "style", "arrowStart", "arrowEnd", "tags"];

/// `meshfox validate`-only (see `attrs::UnknownAttrError`'s own doc
/// comment for why this is a separate pass rather than part of `parse`
/// itself): the first `meshfox:node`/`meshfox:edge` comment attribute
/// anywhere in `markdown` that isn't in `NODE_ATTRS`/`EDGE_ATTRS`.
pub fn unknown_node_edge_attr(markdown: &str) -> Option<crate::attrs::UnknownAttrError> {
    for seg in scan(markdown) {
        if let Some(attr) = crate::attrs::first_unknown(&seg.node_attrs, NODE_ATTRS) {
            return Some(crate::attrs::UnknownAttrError {
                context: format!("the meshfox:node comment under heading {:?}", seg.title),
                attr: attr.to_string(),
            });
        }
        for edge in &seg.edge_attrs {
            if let Some(attr) = crate::attrs::first_unknown(edge, EDGE_ATTRS) {
                return Some(crate::attrs::UnknownAttrError {
                    context: format!("a meshfox:edge comment under heading {:?}", seg.title),
                    attr: attr.to_string(),
                });
            }
        }
    }
    None
}

struct Segment {
    level: u8,
    title: String,
    heading_span: Range<usize>,
    node_line_span: Option<Range<usize>>,
    node_attrs: HashMap<String, String>,
    edge_attrs: Vec<HashMap<String, String>>,
    /// Byte span covering every consecutive `meshfox:edge` line right after
    /// this node's heading/meta line (empty/`None` if it has none) — lets
    /// `set_node_edges` replace the whole block in one splice.
    edge_block_span: Option<Range<usize>>,
    body_span: Range<usize>,
}

pub fn parse(markdown: &str) -> Result<Canvas, ParseError> {
    let segments = scan(markdown);
    if segments.is_empty() || segments[0].level != 1 {
        return Err(ParseError::NoRoot);
    }
    if segments.iter().skip(1).any(|s| s.level == 1) {
        return Err(ParseError::MultipleRoots);
    }
    let ids = assign_ids(&segments)?;
    let parents = resolve_parent_ids(&segments, &ids)?;

    let mut nodes = Vec::with_capacity(segments.len());
    for ((seg, id), parent) in segments.iter().zip(ids.iter()).zip(parents) {
        let node_type = match seg.node_attrs.get("type").map(String::as_str) {
            None | Some("text") => NodeType::Text,
            Some("file") => NodeType::File,
            Some("link") => NodeType::Link,
            Some("group") => NodeType::Group,
            Some("include") => NodeType::Include,
            Some(other) => return Err(ParseError::UnknownNodeType(id.clone(), other.to_string())),
        };
        if node_type != NodeType::Link && seg.node_attrs.contains_key("preview") {
            return Err(ParseError::PreviewRequiresLinkType(id.clone()));
        }

        let body = crate::comment::strip(&markdown[seg.body_span.clone()])
            .trim()
            .to_string();
        let target = match node_type {
            NodeType::Group => {
                if !body.is_empty() {
                    return Err(ParseError::GroupHasBody(id.clone()));
                }
                None
            }
            NodeType::File | NodeType::Link | NodeType::Include => match parse_single_link(&body) {
                Some((_, target)) => Some(target),
                None => return Err(ParseError::InvalidLinkBody(id.clone(), node_type.as_str())),
            },
            NodeType::Text => None,
        };

        let created_at = match seg.node_attrs.get("createdAt") {
            Some(v) if crate::timestamp::is_valid_rfc3339(v) => Some(v.clone()),
            Some(v) => {
                return Err(ParseError::InvalidTimestamp(
                    id.clone(),
                    "createdAt",
                    v.clone(),
                ))
            }
            None => None,
        };
        let updated_at = match seg.node_attrs.get("updatedAt") {
            Some(v) if crate::timestamp::is_valid_rfc3339(v) => Some(v.clone()),
            Some(v) => {
                return Err(ParseError::InvalidTimestamp(
                    id.clone(),
                    "updatedAt",
                    v.clone(),
                ))
            }
            None => None,
        };

        nodes.push(Node {
            id: id.clone(),
            title: seg.title.clone(),
            level: seg.level,
            node_type,
            parent,
            extra_parents: Vec::new(),
            x: seg.node_attrs.get("x").and_then(|v| v.parse().ok()),
            y: seg.node_attrs.get("y").and_then(|v| v.parse().ok()),
            width: seg.node_attrs.get("w").and_then(|v| v.parse().ok()),
            height: seg.node_attrs.get("h").and_then(|v| v.parse().ok()),
            color: seg.node_attrs.get("color").cloned(),
            fold: seg.node_attrs.get("fold").and_then(|v| match v.as_str() {
                "true" => Some(true),
                "false" => Some(false),
                _ => None,
            }),
            tags: parse_tags(seg.node_attrs.get("tags")),
            target,
            display: seg
                .node_attrs
                .get("display")
                .and_then(|v| match v.as_str() {
                    "link" => Some(FileDisplay::Link),
                    "code" => Some(FileDisplay::Code),
                    _ => None,
                }),
            lang: seg.node_attrs.get("lang").cloned(),
            interpreter: seg.node_attrs.get("interpreter").cloned(),
            preview: seg.node_attrs.get("preview").map(|v| v == "true").unwrap_or(false),
            edge_label: seg.node_attrs.get("edgeLabel").cloned(),
            text: body,
            created_at,
            updated_at,
            constraint_results: Vec::new(),
            effective_color: None,
            asset_base: None,
            origin_path: None,
            origin_id: None,
            plain_markdown_include: false,
        });
    }

    let id_set: HashSet<&str> = ids.iter().map(String::as_str).collect();
    for (seg, id) in segments.iter().zip(ids.iter()) {
        for edge in &seg.edge_attrs {
            let Some(from) = edge.get("from") else {
                continue;
            };
            for parent_id in from.split(',').map(str::trim).filter(|s| !s.is_empty()) {
                if !id_set.contains(parent_id) {
                    return Err(ParseError::UnknownParent(id.clone(), parent_id.to_string()));
                }
                if let Some(node) = nodes.iter_mut().find(|n| &n.id == id) {
                    node.extra_parents
                        .push(extra_edge_from_attrs(edge, parent_id));
                }
            }
        }
    }

    Ok(Canvas {
        nodes,
        options: Vec::new(),
    })
}

/// Builds an `ExtraEdge` for `from_id` out of a `meshfox:edge` line's raw
/// attribute map — shared by `parse` and `delete_node`'s dangling-reference
/// sweep. When one line declares several parents via a comma-separated
/// `from="a,b"` (see `parse`), every resulting edge shares that one line's
/// label/color/style/arrow/tags attributes.
fn extra_edge_from_attrs(attrs: &HashMap<String, String>, from_id: &str) -> ExtraEdge {
    ExtraEdge {
        from: from_id.to_string(),
        label: attrs.get("label").cloned(),
        color: attrs.get("color").cloned(),
        style: attrs.get("style").and_then(|v| EdgeLineStyle::parse(v)),
        arrow_start: attrs.get("arrowStart").and_then(|v| ArrowEnd::parse(v)),
        arrow_end: attrs.get("arrowEnd").and_then(|v| ArrowEnd::parse(v)),
        tags: parse_tags(attrs.get("tags")),
    }
}

/// Splits a `tags="a, b, c"` attribute value into its individual tags,
/// trimming whitespace and dropping empty entries — shared by node and
/// edge parsing, and by the CLI's `node meta --tags` (same comma-separated
/// spelling on the command line as in the file itself). `None` (attribute
/// absent) is just an empty list.
pub fn parse_tags(v: Option<&String>) -> Vec<String> {
    v.map(|s| {
        s.split(',')
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(str::to_string)
            .collect()
    })
    .unwrap_or_default()
}

pub fn render(canvas: &Canvas) -> String {
    let Some(root) = canvas.nodes.iter().find(|n| n.parent.is_none()) else {
        return String::new();
    };
    let mut out = String::new();
    out.push_str(CANVAS_MARKER);
    out.push('\n');
    out.push('\n');
    render_node(canvas, root, &mut out);
    out
}

fn render_node(canvas: &Canvas, node: &Node, out: &mut String) {
    out.push_str(&"#".repeat(node.level as usize));
    out.push(' ');
    out.push_str(&node.title);
    out.push('\n');
    out.push_str(&render_node_line(canvas, node));
    out.push('\n');
    for edge in &node.extra_parents {
        out.push_str(&render_edge_line(edge));
    }
    let trimmed = node.text.trim();
    if !trimmed.is_empty() {
        out.push('\n');
        out.push_str(trimmed);
        out.push('\n');
    }
    out.push('\n');

    for child in canvas
        .nodes
        .iter()
        .filter(|n| n.parent.as_deref() == Some(node.id.as_str()))
    {
        render_node(canvas, child, out);
    }
}

fn render_node_line(canvas: &Canvas, node: &Node) -> String {
    let mut parts = vec![format!("id=\"{}\"", node.id)];
    if !node.node_type.is_text() {
        parts.push(format!("type=\"{}\"", node.node_type.as_str()));
    }
    // Needed whenever heading depth alone can't express the real parent —
    // in practice, once this subtree is already `######` (CommonMark's
    // level-6 ceiling) and a deeper child has nowhere left to go but
    // *another* `######` heading (see `insert_child_node`'s doc comment).
    // `node.level <= parent.level` is exactly the condition under which
    // plain heading-outline inference (`resolve_parent_ids`) would get this
    // wrong on the next parse.
    if let Some(parent_id) = &node.parent {
        let parent_level = canvas.node(parent_id).map(|p| p.level).unwrap_or(0);
        if node.level <= parent_level {
            parts.push(format!("parent=\"{parent_id}\""));
        }
    }
    if let Some(x) = node.x {
        parts.push(format!("x={}", fmt_num(x)));
    }
    if let Some(y) = node.y {
        parts.push(format!("y={}", fmt_num(y)));
    }
    if let Some(w) = node.width {
        parts.push(format!("w={}", fmt_num(w)));
    }
    if let Some(h) = node.height {
        parts.push(format!("h={}", fmt_num(h)));
    }
    if let Some(c) = &node.color {
        parts.push(format!("color=\"{c}\""));
    }
    if let Some(d) = node.display {
        if !d.is_link() {
            parts.push(format!("display=\"{}\"", d.as_str()));
        }
    }
    if let Some(l) = &node.lang {
        parts.push(format!("lang=\"{l}\""));
    }
    if let Some(i) = &node.interpreter {
        parts.push(format!("interpreter=\"{i}\""));
    }
    if node.preview {
        parts.push("preview=\"true\"".to_string());
    }
    if let Some(l) = &node.edge_label {
        parts.push(format!("edgeLabel=\"{l}\""));
    }
    if let Some(c) = &node.created_at {
        parts.push(format!("createdAt=\"{c}\""));
    }
    if let Some(u) = &node.updated_at {
        parts.push(format!("updatedAt=\"{u}\""));
    }
    if !node.tags.is_empty() {
        parts.push(format!("tags=\"{}\"", node.tags.join(",")));
    }
    format!("<!-- meshfox:node {} -->", parts.join(" "))
}

fn fmt_num(n: f64) -> String {
    if n.fract() == 0.0 {
        format!("{}", n as i64)
    } else {
        format!("{n}")
    }
}

/// Byte offset in `markdown` where node `node_id`'s own body starts —
/// right after its heading and `meshfox:node` comment (if any), same
/// `body_span.start` `set_node_body` below patches in place. `None` if
/// `node_id` doesn't exist. For a consumer that wants to place a cursor
/// at "the start of this node's content" (e.g. a source-code editor
/// jumping to the selected node) without needing to know anything about
/// `Segment`/`scan` themselves, which stay private to this module.
pub fn node_body_offset(markdown: &str, node_id: &str) -> Option<usize> {
    let segments = scan(markdown);
    let ids = assign_ids(&segments).ok()?;
    let idx = ids.iter().position(|id| id == node_id)?;
    Some(segments[idx].body_span.start)
}

/// Replace just the body of node `node_id` with `new_body`, leaving the
/// rest of the document untouched. Used to write cached block output back
/// into the source file without reformatting anything else.
///
/// When the new body actually differs from what was there before, and the
/// document has declared the `auto-timestamps` option (see SPEC.md's
/// "Options" — off by default, meshfox is first and foremost a
/// documentation format), this also bumps the node's own `updatedAt=` to
/// now — see `Node::updated_at`. A body that comes out byte-identical
/// (re-running a `cache`d block whose output didn't change, say) never
/// touches `updatedAt`, so re-running an unchanged block doesn't
/// manufacture a git diff on its own.
pub fn set_node_body(markdown: &str, node_id: &str, new_body: &str) -> Option<String> {
    let segments = scan(markdown);
    let ids = assign_ids(&segments).ok()?;
    let idx = ids.iter().position(|id| id == node_id)?;
    let seg = &segments[idx];

    let trimmed = new_body.trim();
    let body_changed = markdown[seg.body_span.clone()].trim() != trimmed;

    let mut replacement = String::new();
    if !trimmed.is_empty() {
        replacement.push('\n');
        replacement.push_str(trimmed);
        replacement.push('\n');
    }
    replacement.push('\n');

    let mut out = String::with_capacity(markdown.len() + replacement.len());
    out.push_str(&markdown[..seg.body_span.start]);
    out.push_str(&replacement);
    out.push_str(&markdown[seg.body_span.end..]);

    if !body_changed || !timestamps_enabled(markdown, &segments) {
        return Some(out);
    }
    let Some(line_span) = &seg.node_line_span else {
        // Only the root can lack a `meshfox:node` comment line at all —
        // nowhere to stamp `updatedAt` onto without inventing one, which
        // `node meta` (not a body write) is the deliberate way to do.
        return Some(out);
    };
    let mut attrs = seg.node_attrs.clone();
    attrs.insert(
        "updatedAt".to_string(),
        crate::timestamp::now_utc_rfc3339(),
    );
    let new_line = rebuild_node_line_from_raw_attrs(node_id, &attrs);
    // `line_span` was measured against the original `markdown`, but nothing
    // between it and `seg.body_span.start` (where the edit above starts)
    // moved, so the same byte offsets still apply to `out`.
    let mut restamped = String::with_capacity(out.len() + new_line.len());
    restamped.push_str(&out[..line_span.start]);
    restamped.push_str(&new_line);
    restamped.push_str(&out[line_span.end..]);
    Some(restamped)
}

/// Appends `addition` to the end of node `node_id`'s existing prose body —
/// after whatever's already there, still before its first child's own
/// heading (a node's `body_span` already ends there, same boundary
/// `set_node_body` itself patches in place) — without the caller having to
/// read the current body first just to hand it back unchanged. A plain
/// wrapper over `set_node_body` (so it gets the same `updatedAt`/
/// `auto-timestamps` handling for free): reads the current raw body,
/// concatenates, replaces.
pub fn append_node_body(markdown: &str, node_id: &str, addition: &str) -> Option<String> {
    let segments = scan(markdown);
    let ids = assign_ids(&segments).ok()?;
    let idx = ids.iter().position(|id| id == node_id)?;
    let seg = &segments[idx];

    let existing = markdown[seg.body_span.clone()].trim();
    let addition = addition.trim();
    let combined = match (existing.is_empty(), addition.is_empty()) {
        (true, _) => addition.to_string(),
        (false, true) => existing.to_string(),
        (false, false) => format!("{existing}\n\n{addition}"),
    };
    set_node_body(markdown, node_id, &combined)
}

/// Whether `markdown`'s root node declares `<!-- meshfox:option
/// name="auto-timestamps" -->` (see SPEC.md's "Options") — checked by
/// `insert_child_node`/`set_node_body` before ever stamping `createdAt`/
/// `updatedAt` automatically. Timestamps are off unless this is declared —
/// meshfox is first and foremost a documentation format, and most
/// documents don't want bookkeeping churn on every regeneration. Fails
/// closed (assumes timestamps are *not* enabled, the same as the
/// undeclared case) on anything that doesn't scan cleanly — this is a soft
/// internal check, not `meshfox validate`'s job (which already separately
/// rejects a misplaced or malformed `meshfox:option` declaration), and the
/// safe outcome when uncertain is simply not to have a side effect.
fn timestamps_enabled(markdown: &str, segments: &[Segment]) -> bool {
    let Some(root) = segments.iter().find(|s| s.level == 1) else {
        return false;
    };
    crate::options::scan_option_decls(&markdown[root.body_span.clone()])
        .unwrap_or_default()
        .iter()
        .any(|name| name == "auto-timestamps")
}

/// Every `meshfox:node` attribute key that isn't `id`, in the same
/// canonical order `render_node_line`/`set_node_meta` already write them
/// in, alongside whether its value is quoted (every attribute except the
/// bare-numeric `x`/`y`/`w`/`h`). Shared only by
/// `rebuild_node_line_from_raw_attrs` below — unlike `set_node_meta`
/// (which rebuilds a line from typed, individually-validated fields), that
/// function rewrites a line from whatever raw strings were already there,
/// for a caller (`set_node_body`'s own `updatedAt` auto-touch) that only
/// wants to touch *one* attribute and pass every other one through
/// untouched, without re-parsing each of them into a typed value first.
const NODE_LINE_RAW_ORDER: &[(&str, bool)] = &[
    ("type", true),
    ("parent", true),
    ("x", false),
    ("y", false),
    ("w", false),
    ("h", false),
    ("color", true),
    ("display", true),
    ("lang", true),
    ("interpreter", true),
    ("preview", true),
    ("edgeLabel", true),
    ("fold", true),
    ("createdAt", true),
    ("updatedAt", true),
    ("tags", true),
];

/// Rebuilds a `<!-- meshfox:node ... -->` line from `attrs` (raw strings,
/// as already parsed off the existing line) in canonical order — see
/// `NODE_LINE_RAW_ORDER`. Only `set_node_body`'s `updatedAt` auto-touch
/// uses this; every other mutator that rewrites this line
/// (`set_node_meta`) goes through typed fields instead, since it may also
/// need to validate or transform a value, not just pass it through.
fn rebuild_node_line_from_raw_attrs(id: &str, attrs: &HashMap<String, String>) -> String {
    let mut parts = vec![format!("id=\"{id}\"")];
    for (key, quoted) in NODE_LINE_RAW_ORDER {
        if let Some(v) = attrs.get(*key) {
            if *quoted {
                parts.push(format!("{key}=\"{v}\""));
            } else {
                parts.push(format!("{key}={v}"));
            }
        }
    }
    format!("<!-- meshfox:node {} -->", parts.join(" "))
}

/// Insert or update just node `node_id`'s `meshfox:node` comment line with
/// new position/style fields, leaving everything else (including its
/// `meshfox:edge` lines and body) untouched.
pub fn set_node_meta(markdown: &str, node_id: &str, meta: &NodeMeta) -> Option<String> {
    let segments = scan(markdown);
    let ids = assign_ids(&segments).ok()?;
    let idx = ids.iter().position(|id| id == node_id)?;
    let seg = &segments[idx];

    let mut parts = vec![format!("id=\"{node_id}\"")];
    let type_str = match &meta.node_type {
        Some(t) if !t.is_text() => Some(t.as_str().to_string()),
        Some(_) => None, // explicitly set back to the default — omit the attribute
        None => seg.node_attrs.get("type").cloned(),
    };
    if let Some(t) = type_str {
        parts.push(format!("type=\"{t}\""));
    }
    // `NodeMeta` has no field for this — it's never something a caller
    // *sets*, only a structural fact `insert_child_node` may have recorded
    // once (see its doc comment) — so always carry over whatever was
    // already on the line. Dropping it here would silently corrupt the
    // tree on the next unrelated edit (a drag, a resize, a color change):
    // this node would re-parse one level higher than it actually belongs,
    // right back to the bug this attribute exists to avoid.
    if let Some(p) = seg.node_attrs.get("parent") {
        parts.push(format!("parent=\"{p}\""));
    }
    if let Some(x) = meta.x {
        parts.push(format!("x={}", fmt_num(x)));
    }
    if let Some(y) = meta.y {
        parts.push(format!("y={}", fmt_num(y)));
    }
    if let Some(w) = meta.width {
        parts.push(format!("w={}", fmt_num(w)));
    }
    if let Some(h) = meta.height {
        parts.push(format!("h={}", fmt_num(h)));
    }
    if let Some(c) = &meta.color {
        parts.push(format!("color=\"{c}\""));
    }
    if let Some(d) = &meta.display {
        if !d.is_link() {
            parts.push(format!("display=\"{}\"", d.as_str()));
        }
    }
    if let Some(l) = &meta.lang {
        parts.push(format!("lang=\"{l}\""));
    }
    if let Some(i) = &meta.interpreter {
        parts.push(format!("interpreter=\"{i}\""));
    }
    if let Some(true) = meta.preview {
        parts.push("preview=\"true\"".to_string());
    }
    if let Some(l) = &meta.edge_label {
        parts.push(format!("edgeLabel=\"{l}\""));
    }
    if let Some(f) = meta.fold {
        parts.push(format!("fold=\"{f}\""));
    }
    if let Some(c) = &meta.created_at {
        parts.push(format!("createdAt=\"{c}\""));
    }
    // `updatedAt=` isn't part of `NodeMeta` at all (see its own doc
    // comment) — unlike every other field here, there's no caller-supplied
    // value that could carry it forward, so (same as `parent=` above) it's
    // always read straight off the existing line instead.
    if let Some(u) = seg.node_attrs.get("updatedAt") {
        parts.push(format!("updatedAt=\"{u}\""));
    }
    if !meta.tags.is_empty() {
        parts.push(format!("tags=\"{}\"", meta.tags.join(",")));
    }
    let line = format!("<!-- meshfox:node {} -->", parts.join(" "));

    let mut out = String::with_capacity(markdown.len() + line.len() + 1);
    match &seg.node_line_span {
        Some(span) => {
            out.push_str(&markdown[..span.start]);
            out.push_str(&line);
            out.push('\n');
            out.push_str(&markdown[span.end..]);
        }
        None => {
            out.push_str(&markdown[..seg.heading_span.end]);
            out.push_str(&line);
            out.push('\n');
            out.push_str(&markdown[seg.heading_span.end..]);
        }
    }
    Some(out)
}

/// Fence attributes settable via `set_fence_attrs` — same "`None` leaves it
/// untouched" contract as `NodeMeta`, field by field. `deps`/`env` follow
/// `NodeMeta::tags`'s own convention too: `Some(vec![])` (from
/// `--clear-deps`/`--clear-env`) clears the list outright, distinct from
/// `None` (not touched at all). `interpreter` is tri-state for the same
/// reason: `None` leaves it alone, `Some(None)` clears it
/// (`--clear-interpreter`), `Some(Some(spec))` sets it — a plain
/// `Option<String>` can't tell "clear" apart from "not given" on its own.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FenceAttrsPatch {
    /// New `name=` — a fence's own runnable identity isn't addressable any
    /// other way (unlike a node's `id`), so this *is* the rename, not a
    /// separate operation.
    pub name: Option<String>,
    /// The bare language word right after the opening delimiter (`bash` in
    /// `` ```bash ``).
    pub lang: Option<String>,
    pub cache: Option<bool>,
    pub always: Option<bool>,
    pub default: Option<bool>,
    pub tty: Option<bool>,
    pub autoclose: Option<bool>,
    pub deps: Option<Vec<BlockRef>>,
    pub env: Option<Vec<EnvRef>>,
    pub interpreter: Option<Option<String>>,
    /// New code, replacing everything between the opening and closing
    /// delimiter lines outright — `None` leaves the code untouched.
    pub code: Option<String>,
}

/// Rewrites just one runnable fence's own info-string attributes (and,
/// optionally, its code — see `FenceAttrsPatch::code`) inside node
/// `node_id` — every other fence in the node, the rest of its body, and
/// the rest of the document are left byte-for-byte untouched. Unlike
/// `set_node_body`, a caller never has to reconstruct surrounding prose or
/// sibling blocks just to flip one flag on one of them.
///
/// `block_name` is resolved via `scan_runnable_blocks` — the same name/
/// `default`-flag/implicit-self-naming address `meshfox run` and `meshfox
/// list` already understand, see SPEC.md's "Runnable code fences".
///
/// The original delimiter (`` ``` `` vs `~~~`, and its exact run length,
/// e.g. four backticks so a fence can safely contain a literal ` ``` `)
/// and any leading indentation are read straight off the source and
/// carried over verbatim — this only ever rewrites what `patch` actually
/// asks it to. `None` (not `Some` with an empty string) when `node_id` or
/// `block_name` doesn't resolve to a real fence.
pub fn set_fence_attrs(
    markdown: &str,
    node_id: &str,
    block_name: &str,
    patch: &FenceAttrsPatch,
) -> Option<String> {
    let segments = scan(markdown);
    let ids = assign_ids(&segments).ok()?;
    let idx = ids.iter().position(|id| id == node_id)?;
    let seg = &segments[idx];
    let body = &markdown[seg.body_span.clone()];

    let blocks = scan_runnable_blocks(node_id, body);
    let block = blocks.iter().find(|b| b.name.as_deref() == Some(block_name))?;

    let abs_start = seg.body_span.start + block.span.start;
    let abs_end = seg.body_span.start + block.span.end;
    let fence_text = &markdown[abs_start..abs_end];

    // A fence found by `scan_runnable_blocks` always has both a real
    // opening and closing line, even when empty-bodied (`` ```bash\n``` ``)
    // — that's exactly what makes it a fence rather than an unclosed run.
    let mut lines: Vec<&str> = fence_text.lines().collect();
    let closing_line = lines.pop()?;
    let opening_line = lines.remove(0);
    let existing_code_lines = lines;

    let indent_len = opening_line.len() - opening_line.trim_start().len();
    let indent = &opening_line[..indent_len];
    let delim: String = opening_line[indent_len..]
        .chars()
        .take_while(|&c| c == '`' || c == '~')
        .collect();

    let lang = patch.lang.clone().unwrap_or_else(|| block.lang.clone());

    let mut parts = Vec::new();
    if let Some(n) = patch.name.clone().or_else(|| block.name.clone()) {
        parts.push(format!("name=\"{n}\""));
    }
    let deps = patch.deps.clone().unwrap_or_else(|| block.deps.clone());
    if !deps.is_empty() {
        let rendered = deps
            .iter()
            .map(|d| {
                let mut part = match &d.node_id {
                    Some(n) => format!("{n}/{}", d.block_name),
                    None => d.block_name.clone(),
                };
                if d.sync {
                    part.push('!');
                }
                part
            })
            .collect::<Vec<_>>()
            .join(",");
        parts.push(format!("deps=\"{rendered}\""));
    }
    let env = patch.env.clone().unwrap_or_else(|| block.env.clone());
    if !env.is_empty() {
        let rendered = env
            .iter()
            .map(|e| {
                if e.local_name == e.var_name {
                    format!("${}", e.var_name)
                } else {
                    format!("{}=${}", e.local_name, e.var_name)
                }
            })
            .collect::<Vec<_>>()
            .join(",");
        parts.push(format!("env=\"{rendered}\""));
    }
    if patch.cache.unwrap_or(block.cache) {
        parts.push("cache".to_string());
    }
    if patch.tty.unwrap_or(block.tty) {
        parts.push("tty".to_string());
    }
    if patch.autoclose.unwrap_or(block.autoclose) {
        parts.push("autoclose".to_string());
    }
    if patch.always.unwrap_or(block.always) {
        parts.push("always".to_string());
    }
    if patch.default.unwrap_or(block.default) {
        parts.push("default".to_string());
    }
    let interpreter = match &patch.interpreter {
        Some(new) => new.clone(),
        None => block.interpreter.clone(),
    };
    if let Some(i) = &interpreter {
        parts.push(format!("interpreter=\"{i}\""));
    }

    let attrs_suffix = if parts.is_empty() {
        String::new()
    } else {
        format!(" {}", parts.join(" "))
    };
    let new_opening = format!("{indent}{delim}{lang}{attrs_suffix}");

    let new_code_lines: Vec<&str> = match &patch.code {
        Some(code) => code.lines().collect(),
        None => existing_code_lines,
    };

    let mut new_fence = new_opening;
    new_fence.push('\n');
    for line in &new_code_lines {
        new_fence.push_str(line);
        new_fence.push('\n');
    }
    new_fence.push_str(closing_line);

    let mut out = String::with_capacity(markdown.len() + new_fence.len());
    out.push_str(&markdown[..abs_start]);
    out.push_str(&new_fence);
    out.push_str(&markdown[abs_end..]);
    Some(out)
}

/// Rewrites just node `node_id`'s heading *text*, preserving its `#` level
/// and leaving its `meshfox:node`/`meshfox:edge` lines and body untouched.
pub fn set_node_title(markdown: &str, node_id: &str, new_title: &str) -> Option<String> {
    let segments = scan(markdown);
    let ids = assign_ids(&segments).ok()?;
    let idx = ids.iter().position(|id| id == node_id)?;
    let seg = &segments[idx];

    let mut heading = "#".repeat(seg.level as usize);
    heading.push(' ');
    heading.push_str(new_title);
    heading.push('\n');

    let mut out = String::with_capacity(markdown.len() + heading.len());
    out.push_str(&markdown[..seg.heading_span.start]);
    out.push_str(&heading);
    out.push_str(&markdown[seg.heading_span.end..]);
    Some(out)
}

#[derive(Debug, Error, PartialEq)]
pub enum RenameIdError {
    #[error("no node {0:?}")]
    NotFound(String),
    #[error("id {0:?} is already used by another node")]
    AlreadyExists(String),
    #[error("id can't be empty")]
    Empty,
    #[error("id can't contain a `\"`, a `,`, or a control character")]
    InvalidChar,
}

/// Renames node `old_id` to `new_id`: rewrites its own `meshfox:node id=`
/// attribute, every other node's `parent="old_id"` attribute (only ever
/// present once a subtree is past CommonMark's level-6 heading ceiling —
/// see `insert_child_node`) and `meshfox:edge from="old_id"` line that
/// reference it. Also best-effort rewrites `deps="old_id/..."` fence
/// references anywhere in the document (`rewrite_deps_node_id`) — those
/// aren't structurally validated by `parse` (see `crate::deps`), so this
/// step can't itself fail; a reference that was already stale before the
/// rename stays stale, surfaced separately by `deps::validate`/`meshfox
/// check`. A no-op (`Ok`, document unchanged) if `new_id == old_id`.
pub fn rename_node_id(markdown: &str, old_id: &str, new_id: &str) -> Result<String, RenameIdError> {
    if new_id.is_empty() {
        return Err(RenameIdError::Empty);
    }
    if id_has_forbidden_char(new_id) {
        return Err(RenameIdError::InvalidChar);
    }
    if new_id == old_id {
        return Ok(markdown.to_string());
    }

    let canvas =
        Canvas::from_markdown(markdown).map_err(|_| RenameIdError::NotFound(old_id.to_string()))?;
    if canvas.node(new_id).is_some() {
        return Err(RenameIdError::AlreadyExists(new_id.to_string()));
    }

    let mut result = set_node_id_attr(markdown, old_id, Some(new_id))
        .ok_or_else(|| RenameIdError::NotFound(old_id.to_string()))?;

    // Sweep explicit `parent="old_id"` attributes (structural, but only
    // ever written once a subtree hits the level-6 heading ceiling —
    // everyone else's parent is inferred from heading nesting, and doesn't
    // need touching). Re-scan `result`, not `markdown`: only the renamed
    // node's own line has changed so far, so every other node's `parent=`
    // attribute still literally says `old_id` at this point.
    let segments = scan(&result);
    let ids = assign_ids(&segments).map_err(|_| RenameIdError::NotFound(old_id.to_string()))?;
    let explicit_parent_children: Vec<String> = segments
        .iter()
        .zip(ids.iter())
        .filter(|(seg, _)| seg.node_attrs.get("parent").map(String::as_str) == Some(old_id))
        .map(|(_, id)| id.clone())
        .collect();
    for child_id in explicit_parent_children {
        result = set_node_parent_attr(&result, &child_id, Some(new_id))
            .ok_or_else(|| RenameIdError::NotFound(child_id.clone()))?;
    }

    // Sweep `meshfox:edge from="old_id"` lines — same dangling-reference
    // cleanup `delete_node` already does, but rewriting instead of
    // dropping. Uses the original `canvas`'s `extra_parents` (unaffected
    // by the id-attribute rewrite above).
    for node in &canvas.nodes {
        if node.id == old_id || !node.extra_parents.iter().any(|e| e.from == old_id) {
            continue;
        }
        let updated_edges: Vec<ExtraEdge> = node
            .extra_parents
            .iter()
            .map(|e| {
                if e.from == old_id {
                    ExtraEdge {
                        from: new_id.to_string(),
                        ..e.clone()
                    }
                } else {
                    e.clone()
                }
            })
            .collect();
        result = set_node_edges(&result, &node.id, &updated_edges)
            .ok_or_else(|| RenameIdError::NotFound(node.id.clone()))?;
    }

    result = rewrite_deps_node_id(&result, old_id, new_id);

    Ok(result)
}

#[derive(Debug, Error, PartialEq)]
pub enum ClearIdError {
    #[error("no node {0:?}")]
    NotFound(String),
}

/// Removes node `id`'s explicit `id="..."` attribute, handing it back to
/// the parser's own title-slug fallback (`assign_ids`: no `id=` attribute
/// means "derive one from a slug of the title", the same rule a hand-
/// written `meshfox:node` comment with no `id` gets for free). Returns the
/// document plus the id the node actually has afterward — usually a slug
/// of its current title, but re-derived fresh each time rather than
/// memoized, so it stays correct if the title's since diverged from
/// whatever the id used to be.
///
/// If that derived slug happens to already equal `id` (the common case: an
/// explicit id nobody's touched since the node was created, still just
/// `slug(title)`), this only drops the now-redundant attribute — nothing
/// refers to the id by name, so there's nothing else to rewrite. Otherwise
/// this is `rename_node_id(markdown, id, &derived)` (same reference sweep
/// across `parent=`/`meshfox:edge from=`/`deps=`) with the `id=` attribute
/// additionally omitted rather than written out — *if* that's actually
/// safe (see below); falls back to leaving the attribute explicit
/// otherwise, rather than ever risk writing back a document that no
/// longer parses.
///
/// `derived_id` is computed against every *other* id already in the
/// document, but `assign_ids` itself resolves implicit (no `id=`) ids
/// sequentially, in document order, each one only checked against implicit
/// ids resolved *earlier* in the same pass — not the whole document up
/// front. So an id-less node can end up "stealing" a slug some other,
/// still-explicit node later in the file also needs, turning what looked
/// like a safe dedup into a hard duplicate-id parse error once this node
/// no longer has an explicit id pinning it in place. Rare (needs a
/// specific ordering plus a title-slug collision), but real, so this
/// always re-parses the id-less candidate and only accepts it if the
/// document still parses with this node resolving to exactly
/// `derived_id`.
pub fn clear_node_id(markdown: &str, id: &str) -> Result<(String, String), ClearIdError> {
    let canvas =
        Canvas::from_markdown(markdown).map_err(|_| ClearIdError::NotFound(id.to_string()))?;
    let node = canvas
        .node(id)
        .ok_or_else(|| ClearIdError::NotFound(id.to_string()))?;

    let segments = scan(markdown);
    let ids = assign_ids(&segments).map_err(|_| ClearIdError::NotFound(id.to_string()))?;
    let used: HashSet<String> = ids.iter().filter(|i| i.as_str() != id).cloned().collect();
    let derived_id = unique_slug(&node.title, &used);

    let renamed = if derived_id == id {
        markdown.to_string()
    } else {
        rename_node_id(markdown, id, &derived_id).map_err(|_| ClearIdError::NotFound(id.to_string()))?
    };

    if let Some(candidate) = set_node_id_attr(&renamed, &derived_id, None) {
        if Canvas::from_markdown(&candidate)
            .is_ok_and(|c| c.node(&derived_id).is_some())
        {
            return Ok((candidate, derived_id));
        }
    }
    Ok((renamed, derived_id))
}

/// Rewrites just node `old_id`'s own `meshfox:node` comment line's `id=`
/// attribute to `new_id`, leaving every other attribute (`type`/`parent`/
/// `x`/`y`/`w`/`h`/`color`/`display`/`lang`/`interpreter`) exactly as it
/// already was.
/// Doesn't touch anything that *refers* to `old_id` elsewhere in the
/// document — see `rename_node_id`, which sweeps those separately.
///
/// `new_id: None` omits the `id=` attribute entirely instead of writing
/// one — used by `clear_node_id` to let the parser's own title-slug
/// fallback (`assign_ids`) take back over.
fn set_node_id_attr(markdown: &str, old_id: &str, new_id: Option<&str>) -> Option<String> {
    let segments = scan(markdown);
    let ids = assign_ids(&segments).ok()?;
    let idx = ids.iter().position(|id| id == old_id)?;
    let seg = &segments[idx];

    let mut parts = Vec::new();
    if let Some(new_id) = new_id {
        parts.push(format!("id=\"{new_id}\""));
    }
    if let Some(t) = seg.node_attrs.get("type") {
        parts.push(format!("type=\"{t}\""));
    }
    if let Some(p) = seg.node_attrs.get("parent") {
        parts.push(format!("parent=\"{p}\""));
    }
    for key in ["x", "y", "w", "h"] {
        if let Some(v) = seg.node_attrs.get(key) {
            parts.push(format!("{key}={v}"));
        }
    }
    if let Some(c) = seg.node_attrs.get("color") {
        parts.push(format!("color=\"{c}\""));
    }
    if let Some(d) = seg.node_attrs.get("display") {
        parts.push(format!("display=\"{d}\""));
    }
    if let Some(l) = seg.node_attrs.get("lang") {
        parts.push(format!("lang=\"{l}\""));
    }
    if let Some(i) = seg.node_attrs.get("interpreter") {
        parts.push(format!("interpreter=\"{i}\""));
    }
    if let Some(t) = seg.node_attrs.get("tags") {
        parts.push(format!("tags=\"{t}\""));
    }
    let line = if parts.is_empty() {
        "<!-- meshfox:node -->".to_string()
    } else {
        format!("<!-- meshfox:node {} -->", parts.join(" "))
    };

    let mut out = String::with_capacity(markdown.len() + line.len() + 1);
    match &seg.node_line_span {
        Some(span) => {
            out.push_str(&markdown[..span.start]);
            out.push_str(&line);
            out.push('\n');
            out.push_str(&markdown[span.end..]);
        }
        None => {
            out.push_str(&markdown[..seg.heading_span.end]);
            out.push_str(&line);
            out.push('\n');
            out.push_str(&markdown[seg.heading_span.end..]);
        }
    }
    Some(out)
}

/// Best-effort textual rewrite of every `deps="old_id/...", ...` reference
/// (see `crate::fence::BlockRef`) to `new_id/...`, across every fence in
/// the document — used by `rename_node_id` to keep cross-node `deps=`
/// references pointing at the right node after a rename. Scoped to each
/// fence's own opening (info-string) line, never touching the code body or
/// any other attribute on that line. Unlike `parent=`/`meshfox:edge from=`,
/// `deps=` isn't structurally validated by `parse` (see `crate::deps`), so
/// this can't fail — a reference that doesn't match `old_id/` exactly (e.g.
/// already broken) is left untouched.
fn rewrite_deps_node_id(markdown: &str, old_id: &str, new_id: &str) -> String {
    let prefix = format!("{old_id}/");
    let fences = crate::fence::scan_raw_fences(markdown);
    let mut out = String::with_capacity(markdown.len());
    let mut last = 0;
    for f in &fences {
        let attrs = crate::attrs::parse_attrs(&f.info);
        let Some(deps) = attrs.get("deps") else {
            continue;
        };
        if !deps
            .split(',')
            .map(str::trim)
            .any(|s| s.starts_with(&prefix))
        {
            continue;
        }
        let new_deps = deps
            .split(',')
            .map(|entry| {
                let trimmed = entry.trim();
                match trimmed.strip_prefix(&prefix) {
                    Some(rest) => format!("{new_id}/{rest}"),
                    None => trimmed.to_string(),
                }
            })
            .collect::<Vec<_>>()
            .join(",");

        let info_line_end = markdown[f.span.start..]
            .find('\n')
            .map(|i| f.span.start + i)
            .unwrap_or(f.span.start + f.info.len());
        let old_line = &markdown[f.span.start..info_line_end];
        let new_line = if old_line.contains(&format!("deps=\"{deps}\"")) {
            old_line.replacen(
                &format!("deps=\"{deps}\""),
                &format!("deps=\"{new_deps}\""),
                1,
            )
        } else {
            old_line.replacen(&format!("deps={deps}"), &format!("deps={new_deps}"), 1)
        };

        out.push_str(&markdown[last..f.span.start]);
        out.push_str(&new_line);
        last = info_line_end;
    }
    out.push_str(&markdown[last..]);
    out
}

/// Renders one `meshfox:edge` line for `e`, including whichever of
/// `label`/`color`/`style`/`arrowStart`/`arrowEnd` are actually set — shared
/// by `render_node` and `set_node_edges`.
fn render_edge_line(e: &ExtraEdge) -> String {
    let mut parts = vec![format!("from=\"{}\"", e.from)];
    if let Some(l) = &e.label {
        parts.push(format!("label=\"{l}\""));
    }
    if let Some(c) = &e.color {
        parts.push(format!("color=\"{c}\""));
    }
    if let Some(s) = e.style {
        parts.push(format!("style=\"{}\"", s.as_str()));
    }
    if let Some(a) = e.arrow_start {
        parts.push(format!("arrowStart=\"{}\"", a.as_str()));
    }
    if let Some(a) = e.arrow_end {
        parts.push(format!("arrowEnd=\"{}\"", a.as_str()));
    }
    if !e.tags.is_empty() {
        parts.push(format!("tags=\"{}\"", e.tags.join(",")));
    }
    format!("<!-- meshfox:edge {} -->\n", parts.join(" "))
}

/// Replaces node `node_id`'s whole set of extra incoming edges
/// (`meshfox:edge from="..."` lines) with one line per entry in
/// `extra_parents`, in the given order — an empty slice removes the block
/// entirely. Leaves the heading, `meshfox:node` line, and body untouched.
pub fn set_node_edges(
    markdown: &str,
    node_id: &str,
    extra_parents: &[ExtraEdge],
) -> Option<String> {
    let segments = scan(markdown);
    let ids = assign_ids(&segments).ok()?;
    let idx = ids.iter().position(|id| id == node_id)?;
    let seg = &segments[idx];

    let mut replacement = String::new();
    for edge in extra_parents {
        replacement.push_str(&render_edge_line(edge));
    }

    let mut out = String::with_capacity(markdown.len() + replacement.len());
    match &seg.edge_block_span {
        Some(span) => {
            out.push_str(&markdown[..span.start]);
            out.push_str(&replacement);
            out.push_str(&markdown[span.end..]);
        }
        None => {
            // Insert right after whatever's already there (meta line, or
            // the heading itself if even that's missing — root's case) —
            // directly, with no blank line in between: `scan()` only
            // recognizes `meshfox:edge` lines immediately following the
            // node's own heading/meta line.
            let insert_at = seg.node_line_span.as_ref().unwrap_or(&seg.heading_span).end;
            out.push_str(&markdown[..insert_at]);
            out.push_str(&replacement);
            out.push_str(&markdown[insert_at..]);
        }
    }
    Some(out)
}

/// Replaces the document's whole set of declared `meshfox:option` names
/// (see `crate::options`, SPEC.md's "Options" — always root-only) with
/// exactly one line per entry in `desired`, in the given order,
/// consolidated at the very top of the root node's own body (right after
/// its `meshfox:node`/`meshfox:edge` lines) regardless of where any
/// previous declaration lived in the file. An empty slice removes every
/// declaration. This is the write path behind the web UI's "document
/// options" modal; hand-editing the plain comment syntax directly still
/// works fine, this just gives the UI an equivalent way to do the same
/// edit. Returns `None` only if the document has no root heading at all
/// (never true for anything `parse` already accepted).
pub fn set_document_options(markdown: &str, desired: &[String]) -> Option<String> {
    let segments = scan(markdown);
    let root = segments.first()?;
    let body_start = root.body_span.start;
    let body_end = root.body_span.end;
    let body = &markdown[body_start..body_end];

    let fence_ranges = crate::fence::fenced_byte_ranges(body);
    let mut fi = 0;
    let mut offset = 0;
    let mut kept = String::with_capacity(body.len());
    for line in body.split_inclusive('\n') {
        let start = offset;
        offset += line.len();
        while fi < fence_ranges.len() && fence_ranges[fi].end <= start {
            fi += 1;
        }
        let in_fence = fi < fence_ranges.len() && fence_ranges[fi].start <= start;
        let is_option_line = !in_fence
            && crate::options::parse_option_comment(line.trim_end_matches('\n')).is_some();
        if !is_option_line {
            kept.push_str(line);
        }
    }

    let mut prefix = String::new();
    for name in desired {
        prefix.push_str(&format!("<!-- meshfox:option name=\"{name}\" -->\n"));
    }

    let mut out = String::with_capacity(markdown.len() + prefix.len() + kept.len());
    out.push_str(&markdown[..body_start]);
    out.push_str(&prefix);
    out.push_str(&kept);
    out.push_str(&markdown[body_end..]);
    Some(out)
}

/// Inserts a new child heading node under `parent_id`, titled `title`, with
/// an empty body — as the *last* item in `parent_id`'s entire existing
/// subtree (after all of its current children/grandchildren), so it reads
/// as the newest child without disturbing anything already there. Returns
/// the updated document and the new node's (uniquely slugged) id. No
/// position is set, so the web client's own client-side auto-layout places
/// it using the same tree-aware default every other position-less node
/// gets, until it's dragged (or hand-authored/`node meta`-set) into a real
/// one.
///
/// `parent_id` already at CommonMark's level-6 heading ceiling (i.e. it's
/// `######`) is handled, not rejected: the new node is still written as
/// `######` (there's nowhere deeper to go), but its `meshfox:node` comment
/// carries an explicit `parent="{parent_id}"` attribute so `parse` — via
/// `resolve_parent_ids` — knows it's a *child*, not a sibling, of
/// `parent_id`. Without that attribute two consecutive `######` headings
/// are indistinguishable from one another under plain heading-outline
/// rules, and the new node would silently attach one level too high (to
/// `parent_id`'s own parent) instead.
pub fn insert_child_node(markdown: &str, parent_id: &str, title: &str) -> Option<(String, String)> {
    insert_child_node_impl(markdown, parent_id, title, |used| unique_slug(title, used))
}

/// Same as [`insert_child_node`], except the new node's id is always a
/// random base36-encoded 64-bit number (see `random_base36_id`), never a
/// slug of `title` — the web UI's "add child" button uses this, not
/// `insert_child_node` itself. That button no longer opens a settings
/// dialog before creating the node (TODO.canvas.md: "Позволить
/// редактировать заголовок прямо на канвасе"), so `title` here is just a
/// placeholder the user is about to overwrite inline, not a real title —
/// slugging it would only reproduce the confusing "new-node"/"new-node-2"
/// ids this exists to replace (TODO.canvas.md: "Id-хэши вместо new-node-X
/// по умолчанию"). `insert_child_node` itself (CLI/MCP `node add`, where
/// the caller's title is the real, final one) is untouched — its
/// human-readable, title-derived id is exactly the point there.
pub fn insert_child_node_random_id(markdown: &str, parent_id: &str, title: &str) -> Option<(String, String)> {
    insert_child_node_impl(markdown, parent_id, title, |used| loop {
        let candidate = random_base36_id();
        if !used.contains(&candidate) {
            return candidate;
        }
    })
}

fn insert_child_node_impl(
    markdown: &str,
    parent_id: &str,
    title: &str,
    gen_id: impl FnOnce(&HashSet<String>) -> String,
) -> Option<(String, String)> {
    let segments = scan(markdown);
    let ids = assign_ids(&segments).ok()?;
    let parents = resolve_parent_ids(&segments, &ids).ok()?;
    let parent_idx = ids.iter().position(|id| id == parent_id)?;
    let parent_level = segments[parent_idx].level;
    let child_level = (parent_level + 1).min(6);
    let needs_explicit_parent = child_level <= parent_level;

    let insert_at = subtree_end_idx(&ids, &parents, parent_idx)
        .map(|j| segments[j].heading_span.start)
        .unwrap_or(markdown.len());

    let used: HashSet<String> = ids.iter().cloned().collect();
    let new_id = gen_id(&used);

    let mut node_parts = vec![format!("id=\"{new_id}\"")];
    if needs_explicit_parent {
        node_parts.push(format!("parent=\"{parent_id}\""));
    }
    if timestamps_enabled(markdown, &segments) {
        let now = crate::timestamp::now_utc_rfc3339();
        node_parts.push(format!("createdAt=\"{now}\""));
        node_parts.push(format!("updatedAt=\"{now}\""));
    }

    let mut block = String::new();
    block.push_str(&"#".repeat(child_level as usize));
    block.push(' ');
    block.push_str(title);
    block.push('\n');
    block.push_str(&format!("<!-- meshfox:node {} -->\n", node_parts.join(" ")));
    block.push('\n');

    let mut out = String::with_capacity(markdown.len() + block.len() + 1);
    out.push_str(&markdown[..insert_at]);
    if !out.ends_with("\n\n") {
        if !out.ends_with('\n') {
            out.push('\n');
        }
        out.push('\n');
    }
    out.push_str(&block);
    out.push_str(&markdown[insert_at..]);
    Some((out, new_id))
}

/// Deletes node `node_id` only, promoting each of its *direct* children (and
/// their own subtrees, untouched otherwise) to be children of `node_id`'s
/// former parent instead — the "move children up a level" counterpart to
/// [`delete_node`]'s "delete the whole subtree". Grandchildren and beyond
/// aren't reparented themselves (they stay under whichever direct child they
/// already had); only the direct children move. Returns `None` if
/// `node_id` doesn't exist or is the root (no parent to promote children
/// to — callers should reject deleting the root themselves, same as
/// `delete_node`).
///
/// Since a node's children already sit immediately after it in the file
/// (nesting is what heading depth encodes), promoting them in place just
/// means: cut `node_id`'s own heading/meta/edges/body (nothing past it),
/// then re-level each child's whole subtree down to sit directly under the
/// grandparent — one level shallower, same rule `insert_child_node` uses
/// for the reverse (an explicit `parent=` attribute once that hits
/// CommonMark's level-6 ceiling).
pub fn delete_node_reparent_children(markdown: &str, node_id: &str) -> Option<String> {
    let segments = scan(markdown);
    let ids = assign_ids(&segments).ok()?;
    let parents = resolve_parent_ids(&segments, &ids).ok()?;
    let idx = ids.iter().position(|id| id == node_id)?;
    let grandparent_id = parents[idx].clone()?;
    let grandparent_idx = ids.iter().position(|id| id == &grandparent_id)?;
    let grandparent_level = segments[grandparent_idx].level;

    let child_ids: Vec<String> = (0..segments.len())
        .filter(|&i| parents[i].as_deref() == Some(node_id))
        .map(|i| ids[i].clone())
        .collect();

    // node_id's own "self" span ends where its first child begins (or, if
    // it has none, where its subtree — just itself, in that case — ends).
    let self_start = segments[idx].heading_span.start;
    let self_end = if let Some(first_child_id) = child_ids.first() {
        let ci = ids.iter().position(|x| x == first_child_id)?;
        segments[ci].heading_span.start
    } else {
        subtree_end_idx(&ids, &parents, idx)
            .map(|j| segments[j].heading_span.start)
            .unwrap_or(markdown.len())
    };

    let mut result = String::with_capacity(markdown.len());
    result.push_str(&markdown[..self_start]);
    result.push_str(&markdown[self_end..]);

    // Same dangling-edge cleanup delete_node does: node_id is gone, so any
    // `meshfox:edge from="node_id"` elsewhere would no longer parse. The
    // promoted children themselves keep their ids, so edges pointing at
    // *them* stay valid untouched.
    let rescanned = scan(&result);
    let rescanned_ids = assign_ids(&rescanned).ok()?;
    for (seg, id) in rescanned.iter().zip(rescanned_ids.iter()) {
        let edges: Vec<ExtraEdge> = seg
            .edge_attrs
            .iter()
            .flat_map(|attrs| {
                let from = attrs.get("from").map(String::as_str).unwrap_or("");
                from.split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(|parent_id| extra_edge_from_attrs(attrs, parent_id))
                    .collect::<Vec<_>>()
            })
            .collect();
        if edges.iter().any(|e| e.from == node_id) {
            let filtered: Vec<ExtraEdge> =
                edges.into_iter().filter(|e| e.from != node_id).collect();
            result = set_node_edges(&result, id, &filtered)?;
        }
    }

    if child_ids.is_empty() {
        return Some(result);
    }

    let new_level = (grandparent_level + 1).min(6);
    let needs_explicit_parent = new_level <= grandparent_level;

    // Pass 1: fix up each direct child's own explicit `parent=` attribute
    // first, for every child, before touching any heading levels — a child
    // that was itself past the heading ceiling under `node_id` still
    // literally says `parent="node_id"` at this point, which must be
    // resolved (to the grandparent, or dropped) before the document parses
    // again enough to compute subtree boundaries in pass 2.
    for child_id in &child_ids {
        result = set_node_parent_attr(
            &result,
            child_id,
            needs_explicit_parent.then_some(grandparent_id.as_str()),
        )?;
    }

    // Pass 2: now that the document parses, re-level each child's whole
    // subtree (itself plus every descendant) to its new depth.
    for child_id in &child_ids {
        let segs = scan(&result);
        let seg_ids = assign_ids(&segs).ok()?;
        let seg_parents = resolve_parent_ids(&segs, &seg_ids).ok()?;
        let ci = seg_ids.iter().position(|x| x == child_id)?;
        let old_level = segs[ci].level;
        let delta = new_level as i16 - old_level as i16;
        if delta != 0 {
            let start = segs[ci].heading_span.start;
            let end = subtree_end_idx(&seg_ids, &seg_parents, ci)
                .map(|j| segs[j].heading_span.start)
                .unwrap_or(result.len());
            result = shift_headings_range(&result, start..end, delta as i8);
        }
    }

    Some(result)
}

/// Rewrites just node `node_id`'s own `meshfox:node` comment line's
/// `parent=` attribute — set to `new_parent` if `Some`, or dropped entirely
/// if `None` — leaving every other attribute (`id`/`type`/`x`/`y`/`w`/`h`/
/// `color`) exactly as it already was. Used by `delete_node_reparent_children`
/// to retarget a promoted child once its new heading depth alone either can
/// (`None`) or can't (`Some`, past the level-6 ceiling) express its new
/// parent — same escape hatch `insert_child_node` writes on the way down.
fn set_node_parent_attr(markdown: &str, node_id: &str, new_parent: Option<&str>) -> Option<String> {
    let segments = scan(markdown);
    let ids = assign_ids(&segments).ok()?;
    let idx = ids.iter().position(|id| id == node_id)?;
    let seg = &segments[idx];

    let mut parts = vec![format!("id=\"{node_id}\"")];
    if let Some(t) = seg.node_attrs.get("type") {
        parts.push(format!("type=\"{t}\""));
    }
    if let Some(p) = new_parent {
        parts.push(format!("parent=\"{p}\""));
    }
    for (attr, key) in [("x", "x"), ("y", "y"), ("w", "w"), ("h", "h")] {
        if let Some(v) = seg.node_attrs.get(key) {
            parts.push(format!("{attr}={v}"));
        }
    }
    if let Some(c) = seg.node_attrs.get("color") {
        parts.push(format!("color=\"{c}\""));
    }
    let line = format!("<!-- meshfox:node {} -->", parts.join(" "));

    let mut out = String::with_capacity(markdown.len() + line.len() + 1);
    match &seg.node_line_span {
        Some(span) => {
            out.push_str(&markdown[..span.start]);
            out.push_str(&line);
            out.push('\n');
            out.push_str(&markdown[span.end..]);
        }
        None => {
            out.push_str(&markdown[..seg.heading_span.end]);
            out.push_str(&line);
            out.push('\n');
            out.push_str(&markdown[seg.heading_span.end..]);
        }
    }
    Some(out)
}

/// Deletes `node_id`'s structural (nesting) parent edge, promoting one of
/// its existing *extra* edges to take its place instead — `new_parent_id`
/// must already be one of `node_id`'s `extra_parents` (see
/// `Canvas::extra_parents`); it stops being one, since the tree nesting now
/// expresses that same relationship instead. This is deliberately not a
/// general "move this node anywhere" operation: `new_parent_id` has to be a
/// relationship the document already declares, just not the structural one
/// yet — the web UI's counterpart to `delete_node_reparent_children` for
/// the *incoming* side of a node's edges rather than its children's.
///
/// Moves `node_id`'s whole subtree (re-leveled to fit, same rule
/// `insert_child_node` uses once past CommonMark's level-6 heading ceiling)
/// to the end of `new_parent_id`'s existing children, wherever in the
/// document that is — nesting is what heading depth encodes, so an actual
/// relocation (not just a level change) is unavoidable here, unlike
/// `delete_node_reparent_children`'s children, which never leave their
/// existing spot in the file.
///
/// Returns `None` if `node_id` or `new_parent_id` doesn't exist, `node_id`
/// is the root (no structural edge to delete in the first place),
/// `new_parent_id` isn't currently one of `node_id`'s extra parents, or
/// `new_parent_id` is `node_id` itself or one of its own descendants (would
/// make the tree cyclic).
pub fn reparent_node(markdown: &str, node_id: &str, new_parent_id: &str) -> Option<String> {
    if node_id == new_parent_id {
        return None;
    }
    let canvas = Canvas::from_markdown(markdown).ok()?;
    let node = canvas.node(node_id)?;
    node.parent.as_ref()?;
    canvas.node(new_parent_id)?;
    if !node.extra_parents.iter().any(|e| e.from == new_parent_id) {
        return None;
    }
    // Cycle guard: walk new_parent_id's own ancestor chain — if it reaches
    // node_id, new_parent_id is currently a *descendant* of node_id, and
    // moving node_id under it would make both each other's ancestor.
    let mut cur = canvas.node(new_parent_id)?.parent.clone();
    while let Some(p) = cur {
        if p == node_id {
            return None;
        }
        cur = canvas.node(&p)?.parent.clone();
    }

    let segments = scan(markdown);
    let ids = assign_ids(&segments).ok()?;
    let parents = resolve_parent_ids(&segments, &ids).ok()?;
    let idx = ids.iter().position(|id| id == node_id)?;

    let start = segments[idx].heading_span.start;
    let end = subtree_end_idx(&ids, &parents, idx)
        .map(|j| segments[j].heading_span.start)
        .unwrap_or(markdown.len());
    let mut fragment = markdown[start..end].to_string();

    let mut without_node = String::with_capacity(markdown.len() - (end - start));
    without_node.push_str(&markdown[..start]);
    without_node.push_str(&markdown[end..]);

    // Re-level the whole fragment (node_id plus every descendant, as one
    // rigid unit) to sit under new_parent_id, and fix node_id's own
    // explicit `parent=` attribute to match — same two concerns
    // `delete_node_reparent_children` handles for its promoted children.
    let rescanned = scan(&without_node);
    let rescanned_ids = assign_ids(&rescanned).ok()?;
    let new_parent_idx = rescanned_ids.iter().position(|x| x == new_parent_id)?;
    let new_parent_level = rescanned[new_parent_idx].level;
    let old_level = segments[idx].level;
    let new_level = (new_parent_level + 1).min(6);
    let needs_explicit_parent = new_level <= new_parent_level;
    let delta = new_level as i16 - old_level as i16;

    fragment = set_node_parent_attr(
        &fragment,
        node_id,
        needs_explicit_parent.then_some(new_parent_id),
    )?;
    if delta != 0 {
        let flen = fragment.len();
        fragment = shift_headings_range(&fragment, 0..flen, delta as i8);
    }

    // Insert at the end of new_parent_id's existing subtree, in the
    // now-node_id-less document.
    let rescanned_parents = resolve_parent_ids(&rescanned, &rescanned_ids).ok()?;
    let insert_at = subtree_end_idx(&rescanned_ids, &rescanned_parents, new_parent_idx)
        .map(|j| rescanned[j].heading_span.start)
        .unwrap_or(without_node.len());

    let mut result = String::with_capacity(without_node.len() + fragment.len() + 2);
    result.push_str(&without_node[..insert_at]);
    if !result.ends_with("\n\n") {
        if !result.ends_with('\n') {
            result.push('\n');
        }
        result.push('\n');
    }
    result.push_str(&fragment);
    if !result.ends_with('\n') {
        result.push('\n');
    }
    result.push_str(&without_node[insert_at..]);

    // The promoted relationship is now expressed structurally — drop the
    // now-redundant `meshfox:edge from="new_parent_id"` line from
    // node_id's own extra parents; every other extra parent is untouched.
    let remaining: Vec<ExtraEdge> = node
        .extra_parents
        .iter()
        .filter(|e| e.from != new_parent_id)
        .cloned()
        .collect();
    result = set_node_edges(&result, node_id, &remaining)?;

    Some(result)
}

/// Deletes node `node_id` and its entire subtree (every descendant), plus
/// drops any `meshfox:edge from="..."` reference *elsewhere* in the
/// document that pointed at something in the deleted subtree — otherwise
/// the result could fail to parse (`ParseError::UnknownParent`) once that
/// id no longer exists. Returns `None` if `node_id` doesn't exist.
/// Deleting the root isn't special-cased here (it would just produce a
/// document with no root, `ParseError::NoRoot`) — callers should reject
/// that themselves, since "no root" isn't a useful error for this specific
/// action.
pub fn delete_node(markdown: &str, node_id: &str) -> Option<String> {
    let segments = scan(markdown);
    let ids = assign_ids(&segments).ok()?;
    let parents = resolve_parent_ids(&segments, &ids).ok()?;
    let idx = ids.iter().position(|id| id == node_id)?;

    // Same subtree-end rule `insert_child_node` uses: everything up to the
    // first following segment that isn't one of `node_id`'s descendants
    // (see `subtree_end_idx`), or EOF.
    let start = segments[idx].heading_span.start;
    let end = subtree_end_idx(&ids, &parents, idx)
        .map(|j| segments[j].heading_span.start)
        .unwrap_or(markdown.len());

    let deleted_ids: HashSet<String> = ids
        .iter()
        .zip(segments.iter())
        .filter(|(_, seg)| seg.heading_span.start >= start && seg.heading_span.start < end)
        .map(|(id, _)| id.clone())
        .collect();

    let mut result = String::with_capacity(markdown.len());
    result.push_str(&markdown[..start]);
    result.push_str(&markdown[end..]);

    let remaining_segments = scan(&result);
    let remaining_ids = assign_ids(&remaining_segments).ok()?;
    for (seg, id) in remaining_segments.iter().zip(remaining_ids.iter()) {
        let edges: Vec<ExtraEdge> = seg
            .edge_attrs
            .iter()
            .flat_map(|attrs| {
                let from = attrs.get("from").map(String::as_str).unwrap_or("");
                from.split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(|parent_id| extra_edge_from_attrs(attrs, parent_id))
                    .collect::<Vec<_>>()
            })
            .collect();
        if edges.iter().any(|e| deleted_ids.contains(&e.from)) {
            let filtered: Vec<ExtraEdge> = edges
                .into_iter()
                .filter(|e| !deleted_ids.contains(&e.from))
                .collect();
            result = set_node_edges(&result, id, &filtered)?;
        }
    }

    Some(result)
}

/// Reorders every parent's direct (structural) children to match their
/// canvas layout — sorted by `y`, then `x` among ties — without touching
/// heading depth, any node's own content, or extra (`meshfox:edge`) parents,
/// which don't define a document order in the first place. A child with no
/// recorded position sorts after every positioned sibling, keeping its
/// relative order among other unpositioned siblings stable.
///
/// Since siblings never change depth or parent here, this is just a
/// rearrangement of whole subtree byte ranges (each already contiguous and
/// self-contained, same as [`reparent_node`]'s fragment) — no releveling
/// needed, unlike an actual move to a new parent.
///
/// Called by the server on every canvas save so the on-disk heading order
/// always matches what's drawn, not just each node's individually-patched
/// `x`/`y`. Returns `None` if `markdown` doesn't parse.
pub fn reorder_by_position(markdown: &str) -> Option<String> {
    let segments = scan(markdown);
    let ids = assign_ids(&segments).ok()?;
    let parents = resolve_parent_ids(&segments, &ids).ok()?;
    let canvas = parse(markdown).ok()?;

    let id_index: HashMap<&str, usize> = ids
        .iter()
        .enumerate()
        .map(|(i, id)| (id.as_str(), i))
        .collect();
    let mut children: HashMap<usize, Vec<usize>> = HashMap::new();
    for (i, parent) in parents.iter().enumerate() {
        if let Some(&pidx) = parent.as_deref().and_then(|p| id_index.get(p)) {
            children.entry(pidx).or_default().push(i);
        }
    }

    fn build(
        idx: usize,
        markdown: &str,
        segments: &[Segment],
        ids: &[String],
        parents: &[Option<String>],
        nodes: &[Node],
        children: &HashMap<usize, Vec<usize>>,
    ) -> String {
        let own_start = segments[idx].heading_span.start;
        let mut kids = children.get(&idx).cloned().unwrap_or_default();
        let own_end = match kids.first() {
            Some(&ci) => segments[ci].heading_span.start,
            None => subtree_end_idx(ids, parents, idx)
                .map(|j| segments[j].heading_span.start)
                .unwrap_or(markdown.len()),
        };

        let mut out = markdown[own_start..own_end].to_string();
        kids.sort_by(|&a, &b| {
            let ya = nodes[a].y.unwrap_or(f64::INFINITY);
            let yb = nodes[b].y.unwrap_or(f64::INFINITY);
            ya.partial_cmp(&yb)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| {
                    let xa = nodes[a].x.unwrap_or(f64::INFINITY);
                    let xb = nodes[b].x.unwrap_or(f64::INFINITY);
                    xa.partial_cmp(&xb).unwrap_or(std::cmp::Ordering::Equal)
                })
        });
        for ci in kids {
            out.push_str(&build(
                ci, markdown, segments, ids, parents, nodes, children,
            ));
        }
        out
    }

    let mut out = String::with_capacity(markdown.len());
    out.push_str(&markdown[..segments.first()?.heading_span.start]);
    out.push_str(&build(
        0,
        markdown,
        &segments,
        &ids,
        &parents,
        &canvas.nodes,
        &children,
    ));
    Some(out)
}

#[derive(Debug, Error, PartialEq)]
pub enum MoveSiblingError {
    #[error("no node {0:?}")]
    NotFound(String),
    #[error("node {0:?} and target {1:?} don't share the same structural parent")]
    NotSiblings(String, String),
    #[error("a node can't be moved relative to itself")]
    SameNode,
}

/// Which side of `target_id` `move_sibling` places `id` on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MoveSiblingPosition {
    Before,
    After,
}

/// Moves node `id`'s whole structural subtree to sit immediately before or
/// after `target_id`'s subtree, among the same parent's children — the
/// mechanism `insert_child_node` doesn't have (it only ever appends a new
/// child as the *last* sibling) and unpositioned nodes otherwise have no
/// way to get at all: their document order (heading order in the file) is
/// their only sibling order, and nothing short of hand-editing the raw
/// file could change it before this (see TODO.canvas.md: "Механизм
/// изменения порядка нод при авторазмещении"). A positioned node can
/// already get a new relative order indirectly, by dragging to a new
/// `x`/`y` and letting `reorder_by_position` resync — this is the same
/// underlying rearrangement, just addressed by sibling id instead of by
/// coordinate, so it works for positioned and unpositioned siblings alike
/// (position itself, if either has one, is left untouched either way).
///
/// `id` and `target_id` must already be siblings (same structural parent)
/// — moving to sit among a *different* parent's children is
/// `reparent_node`'s job; combining the two would mean this also has to
/// relevel the moved fragment, the exact extra complexity `reparent_node`
/// already carries on its own. Every descendant of `id` moves with it, as
/// one rigid unit, same as `reparent_node`'s own fragment.
pub fn move_sibling(
    markdown: &str,
    id: &str,
    target_id: &str,
    position: MoveSiblingPosition,
) -> Result<String, MoveSiblingError> {
    if id == target_id {
        return Err(MoveSiblingError::SameNode);
    }
    let segments = scan(markdown);
    let ids = assign_ids(&segments).map_err(|_| MoveSiblingError::NotFound(id.to_string()))?;
    let parents =
        resolve_parent_ids(&segments, &ids).map_err(|_| MoveSiblingError::NotFound(id.to_string()))?;

    let idx = ids
        .iter()
        .position(|x| x == id)
        .ok_or_else(|| MoveSiblingError::NotFound(id.to_string()))?;
    let target_idx = ids
        .iter()
        .position(|x| x == target_id)
        .ok_or_else(|| MoveSiblingError::NotFound(target_id.to_string()))?;
    if parents[idx] != parents[target_idx] {
        return Err(MoveSiblingError::NotSiblings(
            id.to_string(),
            target_id.to_string(),
        ));
    }

    let start = segments[idx].heading_span.start;
    let end = subtree_end_idx(&ids, &parents, idx)
        .map(|j| segments[j].heading_span.start)
        .unwrap_or(markdown.len());
    let fragment = &markdown[start..end];

    let mut without_node = String::with_capacity(markdown.len() - (end - start));
    without_node.push_str(&markdown[..start]);
    without_node.push_str(&markdown[end..]);

    // Re-scan the id-less document to find target_id's own (possibly
    // shifted, if it came after id) position in it.
    let rescanned = scan(&without_node);
    let rescanned_ids =
        assign_ids(&rescanned).map_err(|_| MoveSiblingError::NotFound(target_id.to_string()))?;
    let rescanned_parents = resolve_parent_ids(&rescanned, &rescanned_ids)
        .map_err(|_| MoveSiblingError::NotFound(target_id.to_string()))?;
    let new_target_idx = rescanned_ids
        .iter()
        .position(|x| x == target_id)
        .ok_or_else(|| MoveSiblingError::NotFound(target_id.to_string()))?;

    let insert_at = match position {
        MoveSiblingPosition::Before => rescanned[new_target_idx].heading_span.start,
        MoveSiblingPosition::After => {
            subtree_end_idx(&rescanned_ids, &rescanned_parents, new_target_idx)
                .map(|j| rescanned[j].heading_span.start)
                .unwrap_or(without_node.len())
        }
    };

    let mut result = String::with_capacity(without_node.len() + fragment.len());
    result.push_str(&without_node[..insert_at]);
    result.push_str(fragment);
    result.push_str(&without_node[insert_at..]);
    Ok(result)
}

/// Resolves each segment's structural parent id — normally the nearest
/// preceding segment with a strictly smaller heading level (the classic
/// Markdown-outline rule that `insert_child_node` relies on too), but an
/// explicit `parent="id"` attribute on the `meshfox:node` comment overrides
/// that. That override exists only to escape CommonMark's level-6 heading
/// ceiling: once a subtree is already `######`, a deeper child has nowhere
/// left to go but *another* `######` heading, which the outline rule alone
/// would read as a sibling rather than a child — see `insert_child_node`,
/// which is the only place that actually writes the attribute. The root
/// (the very first segment) never gets an override: it's the one node
/// whose parent must stay `None` no matter what a stray attribute says.
fn resolve_parent_ids(
    segments: &[Segment],
    ids: &[String],
) -> Result<Vec<Option<String>>, ParseError> {
    let id_index: HashMap<&str, usize> = ids
        .iter()
        .enumerate()
        .map(|(i, id)| (id.as_str(), i))
        .collect();

    let mut stack: Vec<(u8, String)> = Vec::new();
    let mut parents = Vec::with_capacity(segments.len());
    for (i, (seg, id)) in segments.iter().zip(ids.iter()).enumerate() {
        while stack.last().is_some_and(|(lvl, _)| *lvl >= seg.level) {
            stack.pop();
        }
        let inferred = stack.last().map(|(_, pid)| pid.clone());
        let parent = if i == 0 {
            None
        } else {
            match seg.node_attrs.get("parent") {
                Some(explicit) => {
                    if !id_index.contains_key(explicit.as_str()) {
                        return Err(ParseError::UnknownExplicitParent(
                            id.clone(),
                            explicit.clone(),
                        ));
                    }
                    Some(explicit.clone())
                }
                None => inferred,
            }
        };
        stack.push((seg.level, id.clone()));
        parents.push(parent);
    }

    // An explicit override can, in principle, point anywhere — including
    // back into its own descendants. Walk each chain to the root and bail
    // if it revisits a node instead of terminating, rather than let a
    // malformed document send some other consumer into an infinite loop.
    for (i, id) in ids.iter().enumerate() {
        let mut seen = HashSet::from([id.as_str()]);
        let mut cur = parents[i].as_deref();
        while let Some(p) = cur {
            if !seen.insert(p) {
                return Err(ParseError::CyclicParent(id.clone()));
            }
            cur = id_index.get(p).and_then(|&idx| parents[idx].as_deref());
        }
    }

    Ok(parents)
}

/// Byte offset (into the segment list, as an index) where `id`'s full
/// structural subtree ends: the first following segment that isn't one of
/// its descendants per `parents` (a sibling, an uncle, or further out) —
/// `None` if the subtree runs to the end of the document. Generalizes the
/// old "next segment at this heading level or shallower" rule (still
/// correct as a special case, since without any `parent=` override,
/// "descendant" and "strictly deeper heading level, transitively" are the
/// same relation) so it also works once a subtree relies on `parent=`
/// overrides — where heading level alone can no longer tell a child from a
/// sibling. Shared by `insert_child_node` (finds where to append a new
/// last child) and `delete_node` (finds where a deleted subtree ends).
fn subtree_end_idx(ids: &[String], parents: &[Option<String>], idx: usize) -> Option<usize> {
    let id_index: HashMap<&str, usize> = ids
        .iter()
        .enumerate()
        .map(|(i, x)| (x.as_str(), i))
        .collect();
    let id = ids[idx].as_str();
    let is_descendant = |mut i: usize| -> bool {
        loop {
            let Some(p) = parents[i].as_deref() else {
                return false;
            };
            if p == id {
                return true;
            }
            let Some(&pi) = id_index.get(p) else {
                return false;
            };
            i = pi;
        }
    };
    ((idx + 1)..ids.len()).find(|&j| !is_descendant(j))
}

fn assign_ids(segments: &[Segment]) -> Result<Vec<String>, ParseError> {
    let mut used = HashSet::new();
    let mut ids = Vec::with_capacity(segments.len());
    for seg in segments {
        let id = match seg.node_attrs.get("id") {
            Some(v) => {
                if id_has_forbidden_char(v) {
                    return Err(ParseError::InvalidId(v.clone()));
                }
                v.clone()
            }
            None => unique_slug(&seg.title, &used),
        };
        if !used.insert(id.clone()) {
            return Err(ParseError::DuplicateId(id));
        }
        ids.push(id);
    }
    Ok(ids)
}

fn unique_slug(title: &str, used: &HashSet<String>) -> String {
    let base = slugify(title);
    let base = if base.is_empty() {
        "node".to_string()
    } else {
        base
    };
    if !used.contains(&base) {
        return base;
    }
    let mut n = 2;
    loop {
        let candidate = format!("{base}-{n}");
        if !used.contains(&candidate) {
            return candidate;
        }
        n += 1;
    }
}

/// A random 64-bit number, base36-encoded (`0-9a-z`, lowercase, no
/// padding) — see `insert_child_node_random_id`. Uses `RandomState`'s own
/// OS-seeded key rather than pulling in a `rand` dependency just for this;
/// not cryptographic, only needs to not collide, which the caller's `used`
/// check already guards regardless.
fn random_base36_id() -> String {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};
    let mut n = RandomState::new().build_hasher().finish();
    const DIGITS: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    if n == 0 {
        return "0".to_string();
    }
    let mut buf = Vec::with_capacity(13);
    while n > 0 {
        buf.push(DIGITS[(n % 36) as usize]);
        n /= 36;
    }
    buf.reverse();
    String::from_utf8(buf).expect("base36 digits are ASCII")
}

/// (start_offset, line_including_its_own_trailing_newline_if_any)
fn lines_with_offsets(s: &str) -> Vec<(usize, &str)> {
    let mut result = Vec::new();
    let mut offset = 0;
    for line in s.split_inclusive('\n') {
        result.push((offset, line));
        offset += line.len();
    }
    result
}

fn parse_heading(line: &str) -> Option<(u8, String)> {
    let trimmed = line.trim_start();
    let hashes = trimmed.chars().take_while(|&c| c == '#').count();
    if hashes == 0 || hashes > 6 {
        return None;
    }
    let rest = &trimmed[hashes..];
    if !rest.is_empty() && !rest.starts_with(' ') && !rest.starts_with('\t') {
        return None;
    }
    let title = rest.trim().to_string();
    if title.is_empty() {
        return None;
    }
    Some((hashes as u8, title))
}

/// Rewrites every top-level (fence-aware, see `fence::fenced_byte_ranges`)
/// Markdown heading in `markdown`, adding `shift` to its level and clamping
/// to 6 (CommonMark's ceiling — headings can't nest any deeper). Used by
/// `crate::include` to nest an included plain-Markdown document's own
/// heading hierarchy under the node that includes it, so e.g. its top-level
/// `#` doesn't collide with the including document's actual root.
pub fn shift_headings(markdown: &str, shift: u8) -> String {
    shift_headings_range(markdown, 0..markdown.len(), shift as i8)
}

/// `shift_headings`'s general form: adds `delta` (which may be negative, for
/// the reverse — see `delete_node_reparent_children`) to the level of every
/// top-level heading whose line *starts* within `range`, clamped to
/// CommonMark's 1–6 bounds. A no-op outside `range`, so a caller can re-level
/// just one subtree without disturbing heading depths anywhere else in the
/// document.
fn shift_headings_range(markdown: &str, range: Range<usize>, delta: i8) -> String {
    if delta == 0 {
        return markdown.to_string();
    }
    // Both use byte offsets into the same `markdown`, but note the two
    // `lines_with_offsets` helpers differ: this module's keeps each line's
    // trailing '\n' attached (see below), fence.rs's doesn't — irrelevant
    // here since we only compare offsets, never the line text itself,
    // across the two.
    let fence_ranges = crate::fence::fenced_byte_ranges(markdown);
    let lines = lines_with_offsets(markdown);
    let mut fi = 0;
    let mut out = String::with_capacity(markdown.len());
    for &(start_off, line) in &lines {
        while fi < fence_ranges.len() && fence_ranges[fi].end <= start_off {
            fi += 1;
        }
        let in_fence = fi < fence_ranges.len() && fence_ranges[fi].start <= start_off;
        let in_range = start_off >= range.start && start_off < range.end;
        if !in_fence && in_range {
            if let Some((level, _)) = parse_heading(line) {
                let indent_len = line.len() - line.trim_start().len();
                let (indent, rest) = line.split_at(indent_len);
                let hash_len = rest.chars().take_while(|&c| c == '#').count();
                let after = &rest[hash_len..];
                let new_level = (level as i16 + delta as i16).clamp(1, 6) as usize;
                out.push_str(indent);
                out.push_str(&"#".repeat(new_level));
                out.push_str(after);
                continue;
            }
        }
        out.push_str(line);
    }
    out
}

fn parse_comment(line: &str, tag: &str) -> Option<String> {
    let trimmed = line.trim();
    let inner = trimmed.strip_prefix("<!--")?.strip_suffix("-->")?.trim();
    inner.strip_prefix(tag).map(|rest| rest.trim().to_string())
}

/// Parses a body that is *exactly* one Markdown link — `[label](target)`
/// and nothing else — as required for `file`/`link` nodes. Returns
/// `(label, target)`.
fn parse_single_link(body: &str) -> Option<(String, String)> {
    let rest = body.trim().strip_prefix('[')?;
    let close_bracket = rest.find(']')?;
    let label = &rest[..close_bracket];
    let rest = rest[close_bracket + 1..].strip_prefix('(')?;
    let target = rest.strip_suffix(')')?;
    if target.is_empty() {
        return None;
    }
    Some((label.to_string(), target.to_string()))
}

fn scan(markdown: &str) -> Vec<Segment> {
    let lines = lines_with_offsets(markdown);

    // Skip any line that starts inside a real (CommonMark-matched) fence —
    // see `fence::scan_raw_fences` — rather than naively toggling on every
    // ``` -like line. That naive approach would treat a nested/longer outer
    // fence, or arbitrary cached command output that happens to contain a
    // ``` run, as closing the fence early and expose its content (which
    // might itself contain `#` lines) as if it were real document structure.
    //
    // Same treatment for a `<!-- meshfox:output ... --> ...
    // <!-- /meshfox:output -->` region (`crate::output::output_byte_ranges`)
    // — needed on top of the fence check above because `output="markdown"`
    // mode (see `crate::output::render_output_block_markdown`) splices a
    // command's own stdout in as real Markdown rather than inside a fence,
    // so without this a forged `# Heading` + `<!-- meshfox:node ... -->`
    // pair printed by the command could otherwise become a real,
    // adversarial canvas node on the next parse.
    let fence_ranges = crate::fence::fenced_byte_ranges(markdown);
    let output_ranges = crate::output::output_byte_ranges(markdown);
    let mut fi = 0;
    let mut oi = 0;
    let mut all_headings = Vec::new();
    for (i, &(start_off, line)) in lines.iter().enumerate() {
        while fi < fence_ranges.len() && fence_ranges[fi].end <= start_off {
            fi += 1;
        }
        while oi < output_ranges.len() && output_ranges[oi].end <= start_off {
            oi += 1;
        }
        let in_fence = fi < fence_ranges.len() && fence_ranges[fi].start <= start_off;
        let in_output = oi < output_ranges.len() && output_ranges[oi].start <= start_off;
        if in_fence || in_output {
            continue;
        }
        let content = line.trim_end_matches('\n');
        if parse_heading(content).is_some() {
            all_headings.push(i);
        }
    }

    // A heading only becomes a node if it's immediately followed by a
    // `meshfox:node` comment — except the document's very first heading,
    // which is always the root whether or not it's marked. An unmarked
    // heading is just formatting: it, and everything under it, stays part
    // of the nearest real node's body like any other Markdown text, so
    // headings can be used freely inside prose without fragmenting the
    // canvas.
    let is_marked = |line_idx: usize| -> bool {
        matches!(
            lines.get(line_idx + 1),
            Some(&(_, l)) if parse_comment(l.trim_end_matches('\n'), "meshfox:node").is_some()
        )
    };
    let heading_line_idx: Vec<usize> = all_headings
        .into_iter()
        .enumerate()
        .filter(|&(pos, line_idx)| pos == 0 || is_marked(line_idx))
        .map(|(_, line_idx)| line_idx)
        .collect();

    let mut segments = Vec::with_capacity(heading_line_idx.len());
    for (pos, &line_idx) in heading_line_idx.iter().enumerate() {
        let (heading_start, heading_line) = lines[line_idx];
        let (level, title) = parse_heading(heading_line.trim_end_matches('\n')).unwrap();
        let heading_span = heading_start..(heading_start + heading_line.len());

        let mut cursor = line_idx + 1;
        let mut node_line_span = None;
        let mut node_attrs = HashMap::new();
        if let Some(&(off, l)) = lines.get(cursor) {
            if let Some(inner) = parse_comment(l.trim_end_matches('\n'), "meshfox:node") {
                node_attrs = parse_attrs(&inner);
                node_line_span = Some(off..(off + l.len()));
                cursor += 1;
            }
        }

        let mut edge_attrs = Vec::new();
        let mut edge_block_span: Option<Range<usize>> = None;
        while let Some(&(off, l)) = lines.get(cursor) {
            match parse_comment(l.trim_end_matches('\n'), "meshfox:edge") {
                Some(inner) => {
                    edge_attrs.push(parse_attrs(&inner));
                    let end = off + l.len();
                    edge_block_span = Some(match edge_block_span {
                        Some(span) => span.start..end,
                        None => off..end,
                    });
                    cursor += 1;
                }
                None => break,
            }
        }

        let body_start = lines
            .get(cursor)
            .map(|&(off, _)| off)
            .unwrap_or(markdown.len());
        let body_end = heading_line_idx
            .get(pos + 1)
            .map(|&next_idx| lines[next_idx].0)
            .unwrap_or(markdown.len());

        segments.push(Segment {
            level,
            title,
            heading_span,
            node_line_span,
            node_attrs,
            edge_attrs,
            edge_block_span,
            body_span: body_start..body_end.max(body_start),
        });
    }
    segments
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOC: &str = r#"# Hello Project
<!-- meshfox:node id="root" x=0 y=0 w=250 h=60 -->

Root body text.

## Tests
<!-- meshfox:node id="tests" x=0 y=160 w=250 h=60 -->

### Smoke Test
<!-- meshfox:node id="smoke-test" x=0 y=320 w=420 h=240 -->

A trivial check.

```bash name="smoke" cache
echo hi
```

## Examples
<!-- meshfox:node id="examples" x=560 y=160 w=250 h=60 -->

### Shared Smoke Check
<!-- meshfox:node id="shared-smoke" x=560 y=320 w=420 h=200 -->
<!-- meshfox:edge from="tests" -->

Reused from Tests as well.
"#;

    #[test]
    fn parses_tree_and_positions() {
        let c = parse(DOC).unwrap();
        assert_eq!(c.nodes.len(), 5);
        let root = c.root().unwrap();
        assert_eq!(root.id, "root");
        assert_eq!(root.text, "Root body text.");

        let tests = c.node("tests").unwrap();
        assert_eq!(tests.parent.as_deref(), Some("root"));
        assert_eq!(tests.x, Some(0.0));
        assert_eq!(tests.y, Some(160.0));

        let smoke = c.node("smoke-test").unwrap();
        assert_eq!(smoke.parent.as_deref(), Some("tests"));
        assert!(smoke.text.contains("```bash name=\"smoke\" cache"));
    }

    #[test]
    fn extra_edge_from_comment() {
        let c = parse(DOC).unwrap();
        let shared = c.node("shared-smoke").unwrap();
        assert_eq!(shared.parent.as_deref(), Some("examples"));
        assert_eq!(shared.extra_parents, vec![ExtraEdge::new("tests")]);
    }

    #[test]
    fn parses_node_and_edge_tags() {
        let doc = "# Root\n<!-- meshfox:node id=\"root\" tags=\"a, b ,c\" -->\n\n## Child\n<!-- meshfox:node id=\"child\" -->\n<!-- meshfox:edge from=\"root\" tags=\"x,y\" -->\n";
        let c = parse(doc).unwrap();
        assert_eq!(c.node("root").unwrap().tags, vec!["a", "b", "c"]);
        assert!(c.node("child").unwrap().tags.is_empty());
        assert_eq!(
            c.node("child").unwrap().extra_parents[0].tags,
            vec!["x", "y"]
        );
    }

    #[test]
    fn tags_round_trip_through_render_and_set_node_meta() {
        let doc = "# Root\n<!-- meshfox:node id=\"root\" tags=\"a,b\" -->\n";
        let c = parse(doc).unwrap();
        let rendered = render(&c);
        assert_eq!(
            parse(&rendered).unwrap().node("root").unwrap().tags,
            vec!["a", "b"]
        );

        // set_node_meta with a different tag list overwrites it...
        let meta = NodeMeta {
            tags: vec!["only".to_string()],
            ..Default::default()
        };
        let updated = set_node_meta(doc, "root", &meta).unwrap();
        assert_eq!(
            parse(&updated).unwrap().node("root").unwrap().tags,
            vec!["only"]
        );

        // ...and an empty list drops the attribute entirely, not just empties it.
        let cleared = set_node_meta(doc, "root", &NodeMeta::default()).unwrap();
        assert!(!cleared.contains("tags="));
        assert!(parse(&cleared)
            .unwrap()
            .node("root")
            .unwrap()
            .tags
            .is_empty());
    }

    #[test]
    fn set_fence_attrs_renames_a_block_leaving_everything_else_untouched() {
        let doc = concat!(
            "# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
            "Some prose above.\n\n",
            "```bash name=\"build\" cache\necho hi\n```\n\n",
            "```bash name=\"other\"\necho untouched\n```\n\n",
            "Some prose below.\n",
        );
        let patch = FenceAttrsPatch {
            name: Some("built".to_string()),
            ..Default::default()
        };
        let updated = set_fence_attrs(doc, "root", "build", &patch).unwrap();
        let canvas = parse(&updated).unwrap();
        let node = canvas.node("root").unwrap();
        let blocks = scan_runnable_blocks("root", &node.text);
        assert!(blocks.iter().any(|b| b.name.as_deref() == Some("built")));
        assert!(!blocks.iter().any(|b| b.name.as_deref() == Some("build")));
        // Every other line — prose, the untouched sibling block, its code —
        // survives byte-for-byte.
        assert!(updated.contains("Some prose above."));
        assert!(updated.contains("Some prose below."));
        assert!(updated.contains("```bash name=\"other\"\necho untouched\n```"));
        assert!(updated.contains("echo hi"));
    }

    #[test]
    fn set_fence_attrs_toggles_boolean_flags_on_and_off() {
        let doc = "# Root\n<!-- meshfox:node id=\"root\" -->\n\n```bash name=\"b\"\necho hi\n```\n";
        let set = set_fence_attrs(
            doc,
            "root",
            "b",
            &FenceAttrsPatch {
                cache: Some(true),
                always: Some(true),
                ..Default::default()
            },
        )
        .unwrap();
        let block = scan_runnable_blocks("root", &parse(&set).unwrap().node("root").unwrap().text)
            .into_iter()
            .find(|b| b.name.as_deref() == Some("b"))
            .unwrap();
        assert!(block.cache);
        assert!(block.always);

        // Flipping back off drops the flags entirely, same as a fresh doc.
        let unset = set_fence_attrs(
            &set,
            "root",
            "b",
            &FenceAttrsPatch {
                cache: Some(false),
                always: Some(false),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(!unset.contains("cache"));
        assert!(!unset.contains("always"));
    }

    #[test]
    fn set_fence_attrs_replaces_deps_and_clears_them() {
        let doc = concat!(
            "# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
            "```bash name=\"b\" deps=\"a\"\necho hi\n```\n",
        );
        let replaced = set_fence_attrs(
            doc,
            "root",
            "b",
            &FenceAttrsPatch {
                deps: Some(crate::fence::parse_deps_list("x,y/z")),
                ..Default::default()
            },
        )
        .unwrap();
        let block =
            scan_runnable_blocks("root", &parse(&replaced).unwrap().node("root").unwrap().text)
                .into_iter()
                .find(|b| b.name.as_deref() == Some("b"))
                .unwrap();
        assert_eq!(block.deps.len(), 2);
        assert_eq!(block.deps[0].block_name, "x");
        assert_eq!(block.deps[1].node_id.as_deref(), Some("y"));
        assert_eq!(block.deps[1].block_name, "z");

        // Not touching `deps` at all leaves it as-is.
        let untouched = set_fence_attrs(doc, "root", "b", &FenceAttrsPatch::default()).unwrap();
        assert!(untouched.contains("deps=\"a\""));

        // An empty (but Some) list clears it outright.
        let cleared = set_fence_attrs(
            doc,
            "root",
            "b",
            &FenceAttrsPatch {
                deps: Some(Vec::new()),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(!cleared.contains("deps="));
    }

    #[test]
    fn set_fence_attrs_sets_and_clears_interpreter() {
        let doc = "# Root\n<!-- meshfox:node id=\"root\" -->\n\n```bash name=\"b\"\nprint(1)\n```\n";
        let set = set_fence_attrs(
            doc,
            "root",
            "b",
            &FenceAttrsPatch {
                interpreter: Some(Some("python3 -u".to_string())),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(set.contains("interpreter=\"python3 -u\""));

        let cleared = set_fence_attrs(
            &set,
            "root",
            "b",
            &FenceAttrsPatch {
                interpreter: Some(None),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(!cleared.contains("interpreter="));
    }

    #[test]
    fn set_fence_attrs_replaces_the_code_and_can_combine_with_attrs_in_one_call() {
        let doc = "# Root\n<!-- meshfox:node id=\"root\" -->\n\n```bash name=\"b\"\necho old\n```\n";
        let updated = set_fence_attrs(
            doc,
            "root",
            "b",
            &FenceAttrsPatch {
                always: Some(true),
                code: Some("echo new\necho more".to_string()),
                ..Default::default()
            },
        )
        .unwrap();
        let node = parse(&updated).unwrap();
        let block = scan_runnable_blocks("root", &node.node("root").unwrap().text)
            .into_iter()
            .find(|b| b.name.as_deref() == Some("b"))
            .unwrap();
        assert!(block.always);
        assert_eq!(block.code, "echo new\necho more");
        assert!(!updated.contains("echo old"));
    }

    #[test]
    fn set_fence_attrs_preserves_a_longer_delimiter() {
        // Four backticks — needed here because the code itself contains a
        // literal fenced block written as plain text (a common reason to
        // need one at all: quoting example Markdown inside a real fence).
        let doc = "# Root\n<!-- meshfox:node id=\"root\" -->\n\n````bash name=\"b\"\necho '```'\n````\n";
        let updated = set_fence_attrs(
            doc,
            "root",
            "b",
            &FenceAttrsPatch {
                cache: Some(true),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(updated.starts_with("# Root"));
        assert!(updated.contains("````bash name=\"b\" cache\necho '```'\n````"));
    }

    #[test]
    fn set_fence_attrs_is_none_for_an_unknown_node_or_block() {
        let doc = "# Root\n<!-- meshfox:node id=\"root\" -->\n\n```bash name=\"b\"\necho hi\n```\n";
        assert!(set_fence_attrs(doc, "nope", "b", &FenceAttrsPatch::default()).is_none());
        assert!(set_fence_attrs(doc, "root", "nope", &FenceAttrsPatch::default()).is_none());
    }

    #[test]
    fn set_node_id_attr_preserves_tags() {
        let doc = "# Root\n<!-- meshfox:node id=\"root\" tags=\"a,b\" -->\n";
        let updated = rename_node_id(doc, "root", "renamed").unwrap();
        assert_eq!(
            parse(&updated).unwrap().node("renamed").unwrap().tags,
            vec!["a", "b"]
        );
    }

    #[test]
    fn auto_id_from_title_when_no_comment() {
        let doc = "# Root\n\n## My Section\n<!-- meshfox:node -->\n\nbody\n";
        let c = parse(doc).unwrap();
        assert_eq!(c.node("my-section").unwrap().title, "My Section");
    }

    #[test]
    fn auto_id_from_title_dedupes_on_collision_at_parse_time() {
        // Two headings, same title, neither with an explicit id — the
        // second must not collide with the first's derived slug (mirrors
        // `insert_child_node`'s own dedup, but exercised through
        // `assign_ids`/`parse` directly rather than the node-creation path).
        let doc =
            "# Root\n\n## My Section\n<!-- meshfox:node -->\n\na\n\n## My Section\n<!-- meshfox:node -->\n\nb\n";
        let c = parse(doc).unwrap();
        assert_eq!(c.node("my-section").unwrap().text.trim(), "a");
        assert_eq!(c.node("my-section-2").unwrap().text.trim(), "b");
    }

    #[test]
    fn auto_id_from_title_falls_back_to_node_when_title_has_no_alphanumeric_characters() {
        // `slugify("---")` is empty, so `unique_slug` substitutes "node" —
        // two such headings must still dedupe against each other instead
        // of silently colliding into the same id.
        let doc = "# Root\n\n## ---\n<!-- meshfox:node -->\n\na\n\n## !!!\n<!-- meshfox:node -->\n\nb\n";
        let c = parse(doc).unwrap();
        assert_eq!(c.node("node").unwrap().text.trim(), "a");
        assert_eq!(c.node("node-2").unwrap().text.trim(), "b");
    }

    #[test]
    fn explicit_id_containing_a_comma_is_a_parse_error() {
        // A comma silently tears an id apart wherever a comma-separated id
        // list is parsed (`meshfox:edge from="a,b"`, `deps="id/block,..."`
        // — see `rewrite_deps_node_id`) — rejecting it at parse time turns
        // that into a loud, obvious error instead of a silently-wrong edge.
        let doc = "# Root\n<!-- meshfox:node id=\"root\" -->\n\n## Section\n<!-- meshfox:node id=\"a,b\" -->\n";
        assert_eq!(
            parse(doc).unwrap_err(),
            ParseError::InvalidId("a,b".to_string())
        );
    }

    #[test]
    fn explicit_id_with_a_space_or_non_latin_script_parses_fine() {
        let doc = concat!(
            "# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
            "## Section One\n<!-- meshfox:node id=\"has space\" -->\n<!-- meshfox:edge from=\"root\" -->\n\n",
            "## Section Two\n<!-- meshfox:node id=\"раздел\" -->\n<!-- meshfox:edge from=\"has space\" -->\n",
        );
        let c = parse(doc).unwrap();
        assert!(c.node("has space").is_some());
        assert_eq!(
            c.node("раздел").unwrap().extra_parents,
            vec![ExtraEdge::new("has space")]
        );
    }

    #[test]
    fn a_node_id_containing_a_space_resolves_as_a_deps_reference() {
        let doc = concat!(
            "# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
            "## Build Step\n<!-- meshfox:node id=\"build step\" -->\n\n",
            "```bash name=\"build\" cache\necho build\n```\n\n",
            "## Deploy\n<!-- meshfox:node id=\"deploy\" -->\n\n",
            "```bash name=\"deploy\" deps=\"build step/build\"\necho deploy\n```\n",
        );
        let c = parse(doc).unwrap();
        let deploy = c.node("deploy").unwrap();
        let blocks = crate::fence::scan_runnable_blocks("deploy", &deploy.text);
        assert_eq!(
            blocks[0].deps,
            vec![crate::fence::BlockRef {
                node_id: Some("build step".to_string()),
                block_name: "build".to_string(),
                sync: false,
            }]
        );
    }

    #[test]
    fn meshfox_comment_regions_are_stripped_from_a_nodes_text() {
        let doc = concat!(
            "# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
            "Visible.\n\n",
            "<!-- meshfox:comment -->This only shows in a plain Markdown renderer.",
            "<!-- /meshfox:comment -->\n\n",
            "Also visible.\n",
        );
        let c = parse(doc).unwrap();
        let text = &c.node("root").unwrap().text;
        assert!(!text.contains("plain Markdown renderer"));
        assert!(!text.contains("meshfox:comment"));
        assert!(text.contains("Visible."));
        assert!(text.contains("Also visible."));
    }

    #[test]
    fn a_meshfox_comment_region_can_make_an_otherwise_invalid_file_body_valid() {
        // A `file`/`link`/`include` node's body must be exactly one
        // Markdown link — a `meshfox:comment`-wrapped blurb alongside it
        // (context for someone reading the raw file outside meshfox)
        // shouldn't count against that, since it's not real body content
        // as far as meshfox itself is concerned.
        let doc = concat!(
            "# Root\n\n",
            "## Diagram\n<!-- meshfox:node type=\"file\" -->\n\n",
            "<!-- meshfox:comment -->See the architecture doc:<!-- /meshfox:comment -->\n",
            "[architecture](./architecture.png)\n",
        );
        let c = parse(doc).unwrap();
        let n = c.node("diagram").unwrap();
        assert_eq!(n.target.as_deref(), Some("./architecture.png"));
        assert_eq!(n.text, "[architecture](./architecture.png)");
    }

    #[test]
    fn unmarked_heading_is_not_a_node() {
        // No `meshfox:node` comment -> "My Section" is just formatting
        // inside root's body, not a separate node.
        let doc = "# Root\n\nintro\n\n## My Section\n\nmore text\n";
        let c = parse(doc).unwrap();
        assert_eq!(c.nodes.len(), 1);
        assert!(c.root().unwrap().text.contains("## My Section"));
        assert!(c.root().unwrap().text.contains("more text"));
    }

    #[test]
    fn heading_inside_fence_is_not_a_node() {
        let doc =
            "# Root\n\n```text\n# not a heading\n```\n\n## Real Section\n<!-- meshfox:node -->\n\nbody\n";
        let c = parse(doc).unwrap();
        assert_eq!(c.nodes.len(), 2);
        assert!(c.node("real-section").is_some());
    }

    #[test]
    fn heading_and_fake_node_comment_inside_arbitrary_cached_output_is_not_a_node() {
        // A cached command's captured output can be anything, including
        // text that looks exactly like real canvas structure — headings
        // immediately followed by a meshfox:node comment, plus stray
        // backtick runs of varying length. As long as the wrapping fence is
        // long enough (meshfox_core::output picks one longer than any run
        // in the body), none of it should be treated as real structure.
        let doc = "# Root\n<!-- meshfox:node id=\"root\" -->\n\n```bash name=\"evil\" cache\necho evil\n```\n<!-- meshfox:output name=\"evil\" -->\n``````text\nexit code: 0\n\n# Fake Root\n<!-- meshfox:node id=\"fake\" -->\n## Fake Section\n```\n````\n`````\n``````\n<!-- /meshfox:output -->\n";
        let c = parse(doc).unwrap();
        assert_eq!(c.nodes.len(), 1);
        assert!(c.node("fake").is_none());
    }

    #[test]
    fn heading_and_fake_node_comment_inside_unfenced_markdown_output_is_not_a_node() {
        // `output="markdown"` mode (crate::output::render_output_block_markdown)
        // splices a command's stdout in as real Markdown, with no wrapping
        // fence at all -- so this is a second, independent guard on top of
        // `heading_and_fake_node_comment_inside_arbitrary_cached_output_is_not_a_node`
        // above: the region between the `meshfox:output`/`/meshfox:output`
        // markers is opaque to heading detection regardless of whether it's
        // fenced. (`crate::output::write_output` itself additionally
        // escapes any literal `<!--` before ever writing content like this
        // -- this test exercises the range-based guard directly, as if
        // that first layer weren't there.)
        let doc = "# Root\n<!-- meshfox:node id=\"root\" -->\n\n```python name=\"evil\" cache output=\"markdown\"\nprint(payload)\n```\n<!-- meshfox:output name=\"evil\" hash=\"x\" -->\n\n# Fake Root\n<!-- meshfox:node id=\"fake\" -->\n\n<!-- /meshfox:output -->\n";
        let c = parse(doc).unwrap();
        assert_eq!(c.nodes.len(), 1);
        assert!(c.node("fake").is_none());
    }

    #[test]
    fn errors_without_root() {
        let doc = "## Section\n\nbody\n";
        assert_eq!(parse(doc).unwrap_err(), ParseError::NoRoot);
    }

    #[test]
    fn errors_on_second_root() {
        let doc = "# Root\n\n# Another Root\n<!-- meshfox:node -->\n";
        assert_eq!(parse(doc).unwrap_err(), ParseError::MultipleRoots);
    }

    #[test]
    fn errors_on_duplicate_id() {
        let doc = "# Root\n<!-- meshfox:node id=\"x\" -->\n\n## Section\n<!-- meshfox:node id=\"x\" -->\n";
        assert_eq!(
            parse(doc).unwrap_err(),
            ParseError::DuplicateId("x".to_string())
        );
    }

    #[test]
    fn errors_on_unknown_edge_target() {
        let doc =
            "# Root\n\n## Section\n<!-- meshfox:node -->\n<!-- meshfox:edge from=\"nope\" -->\n\nbody\n";
        assert_eq!(
            parse(doc).unwrap_err(),
            ParseError::UnknownParent("section".to_string(), "nope".to_string())
        );
    }

    #[test]
    fn explicit_parent_attribute_overrides_heading_outline_inference() {
        // B is written as a level-2 sibling of A (heading-outline alone
        // would make it a child of Root, same as A), but its explicit
        // `parent="a"` says it's really A's child — the escape hatch
        // `insert_child_node` reaches for once a subtree is already at
        // CommonMark's level-6 heading ceiling.
        let doc = "# Root\n\n## A\n<!-- meshfox:node id=\"a\" -->\n\n## B\n<!-- meshfox:node id=\"b\" parent=\"a\" -->\n";
        let c = parse(doc).unwrap();
        assert_eq!(c.node("b").unwrap().parent.as_deref(), Some("a"));
    }

    #[test]
    fn errors_on_unknown_explicit_parent() {
        let doc = "# Root\n\n## A\n<!-- meshfox:node id=\"a\" parent=\"nope\" -->\n";
        assert_eq!(
            parse(doc).unwrap_err(),
            ParseError::UnknownExplicitParent("a".to_string(), "nope".to_string())
        );
    }

    #[test]
    fn errors_on_cyclic_explicit_parent() {
        let doc = "# Root\n\n## A\n<!-- meshfox:node id=\"a\" parent=\"b\" -->\n\n## B\n<!-- meshfox:node id=\"b\" parent=\"a\" -->\n";
        assert_eq!(
            parse(doc).unwrap_err(),
            ParseError::CyclicParent("a".to_string())
        );
    }

    #[test]
    fn render_roundtrips_through_parse() {
        let c1 = parse(DOC).unwrap();
        let rendered = render(&c1);
        let c2 = parse(&rendered).unwrap();
        assert_eq!(c1, c2);
    }

    #[test]
    fn node_body_offset_points_at_the_start_of_the_bodys_own_span() {
        // `body_span` (same one `set_node_body` patches in place) starts
        // right after the heading/`meshfox:node` comment — its own leading
        // blank line included, same as `set_node_body`'s own replacement
        // logic trims off before use (see `Node::text`).
        let offset = node_body_offset(DOC, "smoke-test").unwrap();
        assert!(
            DOC[offset..].trim_start().starts_with("A trivial check."),
            "landed at: {:?}",
            &DOC[offset..offset + 20]
        );

        let root_offset = node_body_offset(DOC, "root").unwrap();
        assert!(DOC[root_offset..]
            .trim_start()
            .starts_with("Root body text."));

        assert_eq!(node_body_offset(DOC, "does-not-exist"), None);
    }

    #[test]
    fn set_node_body_touches_only_target_node() {
        let updated = set_node_body(DOC, "smoke-test", "New body text.").unwrap();
        let c = parse(&updated).unwrap();
        assert_eq!(c.node("smoke-test").unwrap().text, "New body text.");
        // unrelated nodes are untouched
        assert_eq!(c.node("root").unwrap().text, "Root body text.");
        assert_eq!(
            c.node("shared-smoke").unwrap().text,
            "Reused from Tests as well."
        );
        // idempotent-shaped: re-applying again still parses cleanly
        let updated2 = set_node_body(&updated, "smoke-test", "New body text.").unwrap();
        assert_eq!(parse(&updated2).unwrap(), parse(&updated).unwrap());
    }

    #[test]
    fn set_node_meta_updates_existing_comment_in_place() {
        let meta = NodeMeta {
            x: Some(999.0),
            y: Some(1.0),
            width: Some(250.0),
            height: Some(60.0),
            color: None,
            node_type: None,
            display: None,
            lang: None,
            interpreter: None,
            preview: None,
            edge_label: None,
            fold: None,
            tags: Vec::new(),
            created_at: None,
        };
        let updated = set_node_meta(DOC, "tests", &meta).unwrap();
        let c = parse(&updated).unwrap();
        assert_eq!(c.node("tests").unwrap().x, Some(999.0));
        // other nodes' positions are untouched
        assert_eq!(c.node("root").unwrap().x, Some(0.0));
        assert_eq!(c.node("smoke-test").unwrap().x, Some(0.0));
    }

    #[test]
    fn set_node_meta_inserts_comment_when_absent() {
        // Only the root can ever lack a `meshfox:node` comment and still be
        // addressable — any other node necessarily has one already (that's
        // what made it a node in the first place).
        let doc = "# Root\n\n## Section\n<!-- meshfox:node -->\n\nbody\n";
        let meta = NodeMeta {
            x: Some(10.0),
            y: Some(20.0),
            ..Default::default()
        };
        let updated = set_node_meta(doc, "root", &meta).unwrap();
        let c = parse(&updated).unwrap();
        let root = c.root().unwrap();
        assert_eq!(root.x, Some(10.0));
        assert_eq!(c.node("section").unwrap().text, "body");
    }

    #[test]
    fn detects_and_ignores_leading_marker() {
        assert!(has_marker("<!-- meshfox:canvas -->\n# Root\n"));
        assert!(!has_marker(DOC)); // DOC has no marker line
        assert!(!has_marker("# Root\n<!-- meshfox:canvas -->\n")); // not the first line

        let marked = format!("{CANVAS_MARKER}\n\n{DOC}");
        let c = parse(&marked).unwrap();
        assert_eq!(c.root().unwrap().id, "root");
    }

    #[test]
    fn render_emits_marker_for_discovery() {
        let c = parse(DOC).unwrap();
        let rendered = render(&c);
        assert!(has_marker(&rendered));
        // still parses the same either way
        assert_eq!(parse(&rendered).unwrap(), c);
    }

    #[test]
    fn file_node_parses_target_from_single_link() {
        let doc = "# Root\n\n## Diagram\n<!-- meshfox:node type=\"file\" -->\n\n[architecture](./architecture.png)\n";
        let c = parse(doc).unwrap();
        let n = c.node("diagram").unwrap();
        assert_eq!(n.node_type, NodeType::File);
        assert_eq!(n.target.as_deref(), Some("./architecture.png"));
        assert_eq!(n.text, "[architecture](./architecture.png)");
    }

    #[test]
    fn file_node_parses_display_and_lang() {
        let doc = "# Root\n\n## Diagram\n<!-- meshfox:node type=\"file\" display=\"code\" lang=\"rust\" -->\n\n[main](./main.rs)\n";
        let c = parse(doc).unwrap();
        let n = c.node("diagram").unwrap();
        assert_eq!(n.display, Some(FileDisplay::Code));
        assert_eq!(n.lang.as_deref(), Some("rust"));
    }

    #[test]
    fn file_node_defaults_to_link_display_with_no_lang() {
        let doc = "# Root\n\n## Diagram\n<!-- meshfox:node type=\"file\" -->\n\n[architecture](./architecture.png)\n";
        let c = parse(doc).unwrap();
        let n = c.node("diagram").unwrap();
        assert_eq!(n.display, None);
        assert_eq!(n.lang, None);
    }

    #[test]
    fn render_roundtrips_file_display_and_lang() {
        let doc = "# Root\n\n## Diagram\n<!-- meshfox:node type=\"file\" display=\"code\" lang=\"rust\" -->\n\n[main](./main.rs)\n";
        let c = parse(doc).unwrap();
        let rendered = render(&c);
        assert!(rendered.contains("display=\"code\""));
        assert!(rendered.contains("lang=\"rust\""));
        assert_eq!(parse(&rendered).unwrap(), c);
    }

    #[test]
    fn render_omits_default_link_display() {
        let doc = "# Root\n\n## Diagram\n<!-- meshfox:node type=\"file\" -->\n\n[architecture](./architecture.png)\n";
        let c = parse(doc).unwrap();
        let rendered = render(&c);
        assert!(!rendered.contains("display="));
    }

    #[test]
    fn set_node_meta_writes_display_and_lang() {
        let doc =
            "# Root\n\n## Diagram\n<!-- meshfox:node type=\"file\" -->\n\n[main](./main.rs)\n";
        let meta = NodeMeta {
            display: Some(FileDisplay::Code),
            lang: Some("rust".to_string()),
            ..Default::default()
        };
        let updated = set_node_meta(doc, "diagram", &meta).unwrap();
        let c = parse(&updated).unwrap();
        let n = c.node("diagram").unwrap();
        assert_eq!(n.display, Some(FileDisplay::Code));
        assert_eq!(n.lang.as_deref(), Some("rust"));
    }

    #[test]
    fn fold_attribute_round_trips_through_parse_and_set_node_meta() {
        let doc = "# Root\n\n## Section\n<!-- meshfox:node -->\n\nbody\n";
        let c = parse(doc).unwrap();
        assert_eq!(c.node("section").unwrap().fold, None);

        let meta = NodeMeta {
            fold: Some(true),
            ..Default::default()
        };
        let updated = set_node_meta(doc, "section", &meta).unwrap();
        assert_eq!(
            parse(&updated).unwrap().node("section").unwrap().fold,
            Some(true)
        );

        let meta = NodeMeta {
            fold: Some(false),
            ..Default::default()
        };
        let updated = set_node_meta(&updated, "section", &meta).unwrap();
        assert_eq!(
            parse(&updated).unwrap().node("section").unwrap().fold,
            Some(false)
        );

        // `fold: None` omits the attribute entirely, clearing it back to
        // "no override" — same "None omits" contract `color`/`display`
        // already have.
        let meta = NodeMeta {
            fold: None,
            ..Default::default()
        };
        let updated = set_node_meta(&updated, "section", &meta).unwrap();
        assert_eq!(parse(&updated).unwrap().node("section").unwrap().fold, None);
    }

    #[test]
    fn parse_fold_override_accepts_true_false_default_and_rejects_garbage() {
        assert_eq!(parse_fold_override("true"), Ok(Some(true)));
        assert_eq!(parse_fold_override("false"), Ok(Some(false)));
        assert_eq!(parse_fold_override("default"), Ok(None));
        assert!(parse_fold_override("bogus").is_err());
    }

    #[test]
    fn link_node_parses_target() {
        let doc = "# Root\n\n## Homepage\n<!-- meshfox:node type=\"link\" -->\n\n[site](https://example.com)\n";
        let c = parse(doc).unwrap();
        assert_eq!(c.node("homepage").unwrap().node_type, NodeType::Link);
        assert_eq!(
            c.node("homepage").unwrap().target.as_deref(),
            Some("https://example.com")
        );
    }

    #[test]
    fn link_node_parses_preview() {
        let doc = "# Root\n\n## Homepage\n<!-- meshfox:node type=\"link\" preview=\"true\" -->\n\n[site](https://example.com)\n";
        let c = parse(doc).unwrap();
        assert!(c.node("homepage").unwrap().preview);
    }

    #[test]
    fn link_node_defaults_to_no_preview() {
        let doc = "# Root\n\n## Homepage\n<!-- meshfox:node type=\"link\" -->\n\n[site](https://example.com)\n";
        let c = parse(doc).unwrap();
        assert!(!c.node("homepage").unwrap().preview);
    }

    #[test]
    fn preview_on_a_non_link_node_is_a_parse_error() {
        let doc = "# Root\n\n## Diagram\n<!-- meshfox:node type=\"file\" preview=\"true\" -->\n\n[main](./main.rs)\n";
        assert_eq!(
            parse(doc).unwrap_err(),
            ParseError::PreviewRequiresLinkType("diagram".to_string())
        );

        let text_doc =
            "# Root\n\n## Notes\n<!-- meshfox:node preview=\"true\" -->\n\nsome text\n";
        assert_eq!(
            parse(text_doc).unwrap_err(),
            ParseError::PreviewRequiresLinkType("notes".to_string())
        );
    }

    #[test]
    fn render_roundtrips_link_preview() {
        let doc = "# Root\n\n## Homepage\n<!-- meshfox:node type=\"link\" preview=\"true\" -->\n\n[site](https://example.com)\n";
        let c = parse(doc).unwrap();
        let rendered = render(&c);
        assert!(rendered.contains("preview=\"true\""));
        assert_eq!(parse(&rendered).unwrap(), c);
    }

    #[test]
    fn render_omits_default_no_preview() {
        let doc = "# Root\n\n## Homepage\n<!-- meshfox:node type=\"link\" -->\n\n[site](https://example.com)\n";
        let c = parse(doc).unwrap();
        let rendered = render(&c);
        assert!(!rendered.contains("preview="));
    }

    #[test]
    fn set_node_meta_writes_and_clears_preview() {
        let doc = "# Root\n\n## Homepage\n<!-- meshfox:node type=\"link\" -->\n\n[site](https://example.com)\n";
        let meta = NodeMeta {
            preview: Some(true),
            ..Default::default()
        };
        let updated = set_node_meta(doc, "homepage", &meta).unwrap();
        assert!(parse(&updated).unwrap().node("homepage").unwrap().preview);

        let meta = NodeMeta {
            preview: Some(false),
            ..Default::default()
        };
        let cleared = set_node_meta(&updated, "homepage", &meta).unwrap();
        assert!(!cleared.contains("preview="));
        assert!(!parse(&cleared).unwrap().node("homepage").unwrap().preview);
    }

    #[test]
    fn include_node_parses_target_from_single_link() {
        let doc =
            "# Root\n\n## Spec\n<!-- meshfox:node type=\"include\" -->\n\n[spec](./SPEC.md)\n";
        let c = parse(doc).unwrap();
        let n = c.node("spec").unwrap();
        assert_eq!(n.node_type, NodeType::Include);
        assert_eq!(n.target.as_deref(), Some("./SPEC.md"));
    }

    #[test]
    fn include_node_rejects_extra_text_around_link() {
        let doc = "# Root\n\n## Spec\n<!-- meshfox:node type=\"include\" -->\n\nsee [spec](./SPEC.md) please\n";
        assert_eq!(
            parse(doc).unwrap_err(),
            ParseError::InvalidLinkBody("spec".to_string(), "include")
        );
    }

    #[test]
    fn shift_headings_moves_top_level_headings_down_and_skips_fences() {
        let md = "# Title\n\nintro\n\n```text\n# not a heading\n```\n\n## Section\nbody\n";
        let shifted = shift_headings(md, 2);
        assert!(shifted.contains("### Title"));
        assert!(shifted.contains("#### Section"));
        // untouched inside the fence
        assert!(shifted.contains("```text\n# not a heading\n```"));
    }

    #[test]
    fn shift_headings_clamps_to_h6() {
        let md = "##### Deep\n\n###### Deepest\n";
        let shifted = shift_headings(md, 3);
        assert!(shifted.contains("###### Deep\n"));
        assert!(shifted.contains("###### Deepest\n"));
    }

    #[test]
    fn shift_headings_zero_is_a_no_op() {
        let md = "# Title\n\nbody\n";
        assert_eq!(shift_headings(md, 0), md);
    }

    #[test]
    fn file_node_rejects_extra_text_around_link() {
        let doc = "# Root\n\n## Diagram\n<!-- meshfox:node type=\"file\" -->\n\nsee [architecture](./architecture.png) please\n";
        assert_eq!(
            parse(doc).unwrap_err(),
            ParseError::InvalidLinkBody("diagram".to_string(), "file")
        );
    }

    #[test]
    fn file_node_rejects_empty_body() {
        let doc = "# Root\n\n## Diagram\n<!-- meshfox:node type=\"file\" -->\n\nbody\n";
        assert_eq!(
            parse(doc).unwrap_err(),
            ParseError::InvalidLinkBody("diagram".to_string(), "file")
        );
    }

    #[test]
    fn group_node_requires_empty_body() {
        let ok =
            "# Root\n\n## Frame\n<!-- meshfox:node type=\"group\" -->\n\n### Child\n<!-- meshfox:node -->\n\nbody\n";
        let c = parse(ok).unwrap();
        assert_eq!(c.node("frame").unwrap().node_type, NodeType::Group);
        assert_eq!(c.node("frame").unwrap().text, "");
        assert_eq!(c.node("child").unwrap().parent.as_deref(), Some("frame"));

        let bad = "# Root\n\n## Frame\n<!-- meshfox:node type=\"group\" -->\n\nnot empty\n";
        assert_eq!(
            parse(bad).unwrap_err(),
            ParseError::GroupHasBody("frame".to_string())
        );
    }

    #[test]
    fn a_node_body_can_carry_an_embedded_constraint_fence_alongside_anything_else() {
        // No dedicated node type or body-shape restriction any more — a
        // `` ```starlark constraint `` fence is just part of a normal
        // node's Markdown, same as a runnable `` ```bash name="..." ``
        // fence. See `crate::constraint`/`crate::fence::scan_constraint_blocks`
        // for the actual evaluation, not `parse` — the parser doesn't know
        // or care that the fence is there.
        let md = "# Root\n\n## Check\n<!-- meshfox:node -->\n\nsee below\n\n```starlark constraint\npass\n```\n";
        let c = parse(md).unwrap();
        assert_eq!(c.node("check").unwrap().node_type, NodeType::Text);
        assert!(c
            .node("check")
            .unwrap()
            .text
            .contains("```starlark constraint\npass\n```"));
    }

    #[test]
    fn unknown_type_is_a_parse_error() {
        let doc = "# Root\n\n## Section\n<!-- meshfox:node type=\"video\" -->\n\nbody\n";
        assert_eq!(
            parse(doc).unwrap_err(),
            ParseError::UnknownNodeType("section".to_string(), "video".to_string())
        );
    }

    #[test]
    fn text_type_is_default_and_omitted_on_render() {
        let c = parse(DOC).unwrap();
        assert_eq!(c.root().unwrap().node_type, NodeType::Text);
        let rendered = render(&c);
        assert!(!rendered.contains("type="));
    }

    #[test]
    fn set_node_meta_preserves_type_across_position_updates() {
        let doc = "# Root\n\n## Diagram\n<!-- meshfox:node type=\"file\" -->\n\n[img](./a.png)\n";
        let meta = NodeMeta {
            x: Some(10.0),
            ..Default::default()
        };
        let updated = set_node_meta(doc, "diagram", &meta).unwrap();
        let c = parse(&updated).unwrap();
        let n = c.node("diagram").unwrap();
        assert_eq!(n.node_type, NodeType::File);
        assert_eq!(n.x, Some(10.0));
        assert_eq!(n.target.as_deref(), Some("./a.png"));
    }

    #[test]
    fn set_node_meta_can_change_type() {
        let doc = "# Root\n\n## Section\n<!-- meshfox:node -->\n\nsome text\n";
        let meta = NodeMeta {
            node_type: Some(NodeType::Group),
            ..Default::default()
        };
        // Body isn't empty, so this specific combination is expected to
        // produce a document that no longer parses — callers (the server)
        // are responsible for validating before committing a type change.
        let updated = set_node_meta(doc, "section", &meta).unwrap();
        assert!(updated.contains("type=\"group\""));
        assert!(parse(&updated).is_err());

        let doc2 = "# Root\n\n## Section\n<!-- meshfox:node -->\n";
        let updated2 = set_node_meta(doc2, "section", &meta).unwrap();
        let c = parse(&updated2).unwrap();
        assert_eq!(c.node("section").unwrap().node_type, NodeType::Group);
    }

    #[test]
    fn set_node_meta_type_back_to_text_omits_attr() {
        let doc = "# Root\n\n## Diagram\n<!-- meshfox:node type=\"file\" -->\n\n[img](./a.png)\n";
        let meta = NodeMeta {
            node_type: Some(NodeType::Text),
            ..Default::default()
        };
        let updated = set_node_meta(doc, "diagram", &meta).unwrap();
        assert!(!updated.contains("type="));
    }

    #[test]
    fn set_node_title_renames_only_target_heading() {
        let updated = set_node_title(DOC, "smoke-test", "Renamed Test").unwrap();
        let c = parse(&updated).unwrap();
        assert_eq!(c.node("smoke-test").unwrap().title, "Renamed Test");
        // id is untouched (renaming never re-slugs an already-assigned id)
        assert!(c.node("smoke-test").is_some());
        // level and unrelated nodes are untouched
        assert_eq!(c.node("smoke-test").unwrap().level, 3);
        assert_eq!(c.node("root").unwrap().title, "Hello Project");
        assert_eq!(c.node("tests").unwrap().title, "Tests");
    }

    #[test]
    fn set_node_edges_adds_replaces_and_removes() {
        // add where none existed
        let added = set_node_edges(DOC, "tests", &[ExtraEdge::new("examples")]).unwrap();
        let c = parse(&added).unwrap();
        assert_eq!(
            c.node("tests").unwrap().extra_parents,
            vec![ExtraEdge::new("examples")]
        );

        // replace an existing set
        let replaced = set_node_edges(DOC, "shared-smoke", &[ExtraEdge::new("examples")]).unwrap();
        let c = parse(&replaced).unwrap();
        assert_eq!(
            c.node("shared-smoke").unwrap().extra_parents,
            vec![ExtraEdge::new("examples")]
        );
        // unrelated node untouched
        assert_eq!(
            c.node("root").unwrap().extra_parents,
            Vec::<ExtraEdge>::new()
        );

        // remove entirely
        let removed = set_node_edges(DOC, "shared-smoke", &[]).unwrap();
        let c = parse(&removed).unwrap();
        assert!(c.node("shared-smoke").unwrap().extra_parents.is_empty());
        assert!(!removed.contains("meshfox:edge"));
    }

    #[test]
    fn set_document_options_adds_where_none_existed() {
        let added = set_document_options(DOC, &["unfold".to_string()]).unwrap();
        assert_eq!(
            crate::options::declared_options(&parse(&added).unwrap()).unwrap(),
            vec!["unfold".to_string()]
        );
        // root's own prose is untouched
        assert!(added.contains("Root body text."));
        // unrelated nodes untouched
        assert_eq!(parse(&added).unwrap().node("tests").unwrap().x, Some(0.0));
    }

    #[test]
    fn set_document_options_replaces_an_existing_declaration() {
        let doc = "# Root\n<!-- meshfox:node id=\"root\" -->\n<!-- meshfox:option name=\"unfold\" -->\n\nSome prose about the root.\n\n## Child\n<!-- meshfox:node id=\"child\" -->\n\nbody\n";
        let replaced = set_document_options(doc, &["other".to_string()]).unwrap();
        assert_eq!(
            crate::options::declared_options(&parse(&replaced).unwrap()).unwrap(),
            vec!["other".to_string()]
        );
        assert!(replaced.contains("Some prose about the root."));
    }

    #[test]
    fn set_document_options_removes_every_declaration_when_given_an_empty_slice() {
        let doc = "# Root\n<!-- meshfox:node id=\"root\" -->\n<!-- meshfox:option name=\"unfold\" -->\n\nprose\n";
        let removed = set_document_options(doc, &[]).unwrap();
        assert!(!removed.contains("meshfox:option"));
        assert!(crate::options::declared_options(&parse(&removed).unwrap())
            .unwrap()
            .is_empty());
    }

    #[test]
    fn set_document_options_preserves_an_unrecognized_declaration_it_was_told_to_keep() {
        let doc = "# Root\n<!-- meshfox:node id=\"root\" -->\n<!-- meshfox:option name=\"some-future-option\" -->\n\nprose\n";
        let updated = set_document_options(
            doc,
            &["some-future-option".to_string(), "unfold".to_string()],
        )
        .unwrap();
        assert_eq!(
            crate::options::declared_options(&parse(&updated).unwrap()).unwrap(),
            vec!["some-future-option".to_string(), "unfold".to_string()]
        );
    }

    #[test]
    fn insert_child_node_nests_under_parent_as_last_child() {
        let (updated, new_id) = insert_child_node(DOC, "tests", "New Check").unwrap();
        assert_eq!(new_id, "new-check");
        let c = parse(&updated).unwrap();
        let n = c.node("new-check").unwrap();
        assert_eq!(n.parent.as_deref(), Some("tests"));
        assert_eq!(n.level, 3);
        assert_eq!(n.text, "");
        // existing nodes (including tests' existing child) are untouched
        assert_eq!(
            c.node("smoke-test").unwrap().parent.as_deref(),
            Some("tests")
        );
        assert_eq!(c.node("examples").unwrap().parent.as_deref(), Some("root"));
        // lands after every existing descendant of `tests` (i.e. after
        // smoke-test), not spliced in the middle of the subtree
        let smoke_pos = updated.find("### Smoke Test").unwrap();
        let new_pos = updated.find("### New Check").unwrap();
        let examples_pos = updated.find("## Examples").unwrap();
        assert!(smoke_pos < new_pos);
        assert!(new_pos < examples_pos);
    }

    #[test]
    fn insert_child_node_does_not_stamp_timestamps_by_default() {
        // `DOC` declares no `meshfox:option` — timestamps are off by
        // default (meshfox is first and foremost a documentation format).
        let (updated, new_id) = insert_child_node(DOC, "tests", "New Check").unwrap();
        let c = parse(&updated).unwrap();
        let n = c.node(&new_id).unwrap();
        assert_eq!(n.created_at, None);
        assert_eq!(n.updated_at, None);
    }

    #[test]
    fn insert_child_node_stamps_timestamps_when_auto_timestamps_declared() {
        let doc = "# Root\n<!-- meshfox:node id=\"root\" -->\n<!-- meshfox:option name=\"auto-timestamps\" -->\n\nprose\n";
        let (updated, new_id) = insert_child_node(doc, "root", "New Check").unwrap();
        let c = parse(&updated).unwrap();
        let n = c.node(&new_id).unwrap();
        assert!(n.created_at.as_deref().is_some_and(crate::timestamp::is_valid_rfc3339));
        assert_eq!(n.created_at, n.updated_at);
    }

    #[test]
    fn set_node_body_does_not_stamp_updated_at_by_default() {
        let updated = set_node_body(DOC, "smoke-test", "New body text.").unwrap();
        let c = parse(&updated).unwrap();
        assert_eq!(c.node("smoke-test").unwrap().updated_at, None);
    }

    #[test]
    fn set_node_body_stamps_updated_at_when_auto_timestamps_declared() {
        let doc = "# Root\n<!-- meshfox:node id=\"root\" -->\n<!-- meshfox:option name=\"auto-timestamps\" -->\n\n## Child\n<!-- meshfox:node id=\"child\" -->\n\nold body\n";
        let updated = set_node_body(doc, "child", "new body").unwrap();
        let c = parse(&updated).unwrap();
        let n = c.node("child").unwrap();
        assert!(n.updated_at.as_deref().is_some_and(crate::timestamp::is_valid_rfc3339));

        // re-applying the *same* body still doesn't touch updatedAt again —
        // the byte-unchanged skip applies independently of the option.
        let updated2 = set_node_body(&updated, "child", "new body").unwrap();
        assert_eq!(
            parse(&updated2).unwrap().node("child").unwrap().updated_at,
            n.updated_at
        );
    }

    #[test]
    fn insert_child_node_dedupes_id_against_existing_title_slug() {
        let (updated, new_id) = insert_child_node(DOC, "root", "Tests").unwrap();
        assert_ne!(new_id, "tests"); // already taken
        let c = parse(&updated).unwrap();
        assert_eq!(c.node(&new_id).unwrap().title, "Tests");
        assert_eq!(c.node(&new_id).unwrap().parent.as_deref(), Some("root"));
    }

    #[test]
    fn insert_child_node_random_id_ignores_the_title_slug() {
        let (updated, first_id) = insert_child_node_random_id(DOC, "root", "New Node").unwrap();
        assert_ne!(first_id, "new-node");
        assert!(first_id.chars().all(|c| c.is_ascii_digit() || c.is_ascii_lowercase()));
        let (updated, second_id) = insert_child_node_random_id(&updated, "root", "New Node").unwrap();
        assert_ne!(second_id, "new-node-2");
        assert_ne!(first_id, second_id);
        let c = parse(&updated).unwrap();
        assert_eq!(c.node(&first_id).unwrap().title, "New Node");
        assert_eq!(c.node(&second_id).unwrap().title, "New Node");
    }

    #[test]
    fn insert_child_node_under_leaf_with_no_children_appends_at_end() {
        let doc = "# Root\n\n## Leaf\n<!-- meshfox:node -->\n\nbody\n";
        let (updated, new_id) = insert_child_node(doc, "leaf", "Child").unwrap();
        let c = parse(&updated).unwrap();
        assert_eq!(c.node(&new_id).unwrap().parent.as_deref(), Some("leaf"));
        assert_eq!(c.node("leaf").unwrap().text, "body");
    }

    /// Nests six levels deep from `# Root` (root=1, ..., down to a
    /// level-6 `######` leaf) purely via repeated `insert_child_node`
    /// calls — the exact "keep clicking + on the newest node" path a user
    /// hits in the UI. Returns the resulting document and the level-6
    /// leaf's id.
    fn nest_to_heading_ceiling() -> (String, String) {
        let mut doc = "# Root\n<!-- meshfox:node id=\"root\" -->\n".to_string();
        let mut parent = "root".to_string();
        for i in 2..=6 {
            let (updated, id) = insert_child_node(&doc, &parent, &format!("N{i}")).unwrap();
            doc = updated;
            parent = id;
        }
        (doc, parent)
    }

    #[test]
    fn insert_child_node_past_heading_ceiling_uses_explicit_parent_attribute() {
        let (doc, n6) = nest_to_heading_ceiling();
        let c = parse(&doc).unwrap();
        assert_eq!(c.node(&n6).unwrap().level, 6);

        // One more level: n6 is already `######`, so n7 must also be
        // `######`, disambiguated from a plain sibling via `parent=`.
        let (updated, n7) = insert_child_node(&doc, &n6, "N7").unwrap();
        assert!(
            updated.contains(&format!("parent=\"{n6}\"")),
            "expected an explicit parent attribute once past the heading ceiling:\n{updated}"
        );
        let c = parse(&updated).unwrap();
        assert_eq!(c.node(&n7).unwrap().level, 6);
        assert_eq!(c.node(&n7).unwrap().parent.as_deref(), Some(n6.as_str()));
    }

    #[test]
    fn insert_child_node_past_heading_ceiling_supports_multiple_children() {
        // The bug this whole mechanism exists to fix: without it, a
        // *second* child added past the ceiling would misattach — either
        // to the grandparent (naive level-based inference reading two
        // consecutive `######` headings as siblings) or, worse, in between
        // the first child and whatever came after it.
        let (doc, n6) = nest_to_heading_ceiling();
        let (doc, n7) = insert_child_node(&doc, &n6, "N7").unwrap();
        let (doc, n8) = insert_child_node(&doc, &n6, "N8").unwrap();

        let c = parse(&doc).unwrap();
        // both are n6's children, not each other's
        assert_eq!(c.node(&n7).unwrap().parent.as_deref(), Some(n6.as_str()));
        assert_eq!(c.node(&n8).unwrap().parent.as_deref(), Some(n6.as_str()));
        // n8 comes after n7 in the file (appended as the newest last child)
        let n7_pos = doc.find(&format!("id=\"{n7}\"")).unwrap();
        let n8_pos = doc.find(&format!("id=\"{n8}\"")).unwrap();
        assert!(n7_pos < n8_pos);
    }

    #[test]
    fn insert_child_node_past_heading_ceiling_does_not_disturb_a_later_sibling() {
        // Give n6 an existing sibling (added *before* going past the
        // ceiling) to make sure the new subtree-boundary logic still knows
        // where n6's own subtree ends and doesn't swallow it.
        let (doc, n6) = nest_to_heading_ceiling();
        let n5 = parse(&doc)
            .unwrap()
            .node(&n6)
            .unwrap()
            .parent
            .clone()
            .unwrap();
        let (doc, sibling) = insert_child_node(&doc, &n5, "Sibling of N6").unwrap();
        let (doc, n7) = insert_child_node(&doc, &n6, "N7").unwrap();

        let c = parse(&doc).unwrap();
        assert_eq!(c.node(&n7).unwrap().parent.as_deref(), Some(n6.as_str()));
        assert_eq!(
            c.node(&sibling).unwrap().parent.as_deref(),
            Some(n5.as_str())
        );
    }

    #[test]
    fn set_node_meta_preserves_explicit_parent_across_position_updates() {
        let (doc, n6) = nest_to_heading_ceiling();
        let (doc, n7) = insert_child_node(&doc, &n6, "N7").unwrap();

        let meta = NodeMeta {
            x: Some(42.0),
            ..Default::default()
        };
        let updated = set_node_meta(&doc, &n7, &meta).unwrap();
        let c = parse(&updated).unwrap();
        assert_eq!(c.node(&n7).unwrap().parent.as_deref(), Some(n6.as_str()));
        assert_eq!(c.node(&n7).unwrap().x, Some(42.0));
    }

    #[test]
    fn render_roundtrips_a_tree_past_the_heading_ceiling() {
        let (doc, n6) = nest_to_heading_ceiling();
        let (doc, _n7) = insert_child_node(&doc, &n6, "N7").unwrap();
        let (doc, _n8) = insert_child_node(&doc, &n6, "N8").unwrap();
        let c1 = parse(&doc).unwrap();
        let rendered = render(&c1);
        let c2 = parse(&rendered).unwrap();
        assert_eq!(c1, c2);
    }

    #[test]
    fn delete_node_removes_a_subtree_past_the_heading_ceiling() {
        let (doc, n6) = nest_to_heading_ceiling();
        let (doc, n7) = insert_child_node(&doc, &n6, "N7").unwrap();
        let (doc, n7_child) = insert_child_node(&doc, &n7, "N7 child").unwrap();
        let (doc, sibling_of_n7) = insert_child_node(&doc, &n6, "Sibling of N7").unwrap();

        let updated = delete_node(&doc, &n7).unwrap();
        let c = parse(&updated).unwrap();
        assert!(c.node(&n7).is_none());
        assert!(c.node(&n7_child).is_none()); // descendant, also gone
        assert!(c.node(&n6).is_some());
        assert!(c.node(&sibling_of_n7).is_some()); // unrelated, untouched
    }

    #[test]
    fn delete_node_removes_leaf_and_leaves_the_rest_untouched() {
        let updated = delete_node(DOC, "smoke-test").unwrap();
        let c = parse(&updated).unwrap();
        assert!(c.node("smoke-test").is_none());
        assert!(c.node("tests").is_some());
        assert!(c.node("examples").is_some());
    }

    #[test]
    fn delete_node_removes_whole_subtree() {
        let updated = delete_node(DOC, "tests").unwrap();
        let c = parse(&updated).unwrap();
        assert!(c.node("tests").is_none());
        assert!(c.node("smoke-test").is_none()); // descendant, also gone
                                                 // unrelated sibling subtree untouched
        assert!(c.node("examples").is_some());
        assert!(c.node("shared-smoke").is_some());
    }

    #[test]
    fn delete_node_drops_dangling_extra_edges_pointing_at_it() {
        // shared-smoke has `<!-- meshfox:edge from="tests" -->` — deleting
        // `tests` must drop that reference too, or the result won't parse.
        let updated = delete_node(DOC, "tests").unwrap();
        let c = parse(&updated).unwrap();
        assert!(c.node("shared-smoke").unwrap().extra_parents.is_empty());
    }

    #[test]
    fn delete_node_missing_id_is_none() {
        assert_eq!(delete_node(DOC, "does-not-exist"), None);
    }

    /// Whether `a` and `b` contain the same node ids, ignoring order —
    /// `reorder_by_position` deliberately changes document order, so a
    /// plain `Canvas` equality check (order-sensitive, via `Vec<Node>`)
    /// isn't the right tool to confirm nothing was lost or duplicated.
    fn same_ids(a: &Canvas, b: &Canvas) -> bool {
        let mut a_ids: Vec<&str> = a.nodes.iter().map(|n| n.id.as_str()).collect();
        let mut b_ids: Vec<&str> = b.nodes.iter().map(|n| n.id.as_str()).collect();
        a_ids.sort();
        b_ids.sort();
        a_ids == b_ids
    }

    #[test]
    fn reorder_by_position_is_a_no_op_when_already_sorted() {
        // DOC's siblings are already laid out top-to-bottom, left-to-right,
        // so re-tiling the document in the same order must reproduce it
        // byte-for-byte, not just parse to an equal `Canvas`.
        let reordered = reorder_by_position(DOC).unwrap();
        assert_eq!(reordered, DOC);
        let tests_pos = reordered.find("id=\"tests\"").unwrap();
        let examples_pos = reordered.find("id=\"examples\"").unwrap();
        assert!(tests_pos < examples_pos);
    }

    #[test]
    fn reorder_by_position_sorts_siblings_by_y_then_x() {
        let doc = r#"# Root
<!-- meshfox:node id="root" -->

## B
<!-- meshfox:node id="b" x=0 y=100 -->

## A
<!-- meshfox:node id="a" x=0 y=0 -->

## C Right
<!-- meshfox:node id="c-right" x=300 y=100 -->

## C Left
<!-- meshfox:node id="c-left" x=0 y=100 -->
"#;
        let reordered = reorder_by_position(doc).unwrap();
        let c = parse(&reordered).unwrap();
        assert!(same_ids(&c, &parse(doc).unwrap())); // same nodes, just reshuffled
        let pos = |id: &str| reordered.find(&format!("id=\"{id}\"")).unwrap();
        // y=0 sorts before both y=100 siblings...
        assert!(pos("a") < pos("b"));
        // ...and among the y=100 ties, x=0 sorts before x=300.
        assert!(pos("c-left") < pos("c-right"));
        assert!(pos("b") < pos("c-left"));
    }

    #[test]
    fn reorder_by_position_puts_unpositioned_siblings_last_and_stable() {
        let doc = r#"# Root
<!-- meshfox:node id="root" -->

## No Position One
<!-- meshfox:node id="np-one" -->

## Placed
<!-- meshfox:node id="placed" x=0 y=0 -->

## No Position Two
<!-- meshfox:node id="np-two" -->
"#;
        let reordered = reorder_by_position(doc).unwrap();
        let pos = |id: &str| reordered.find(&format!("id=\"{id}\"")).unwrap();
        assert!(pos("placed") < pos("np-one"));
        // relative order among unpositioned siblings is preserved as-is.
        assert!(pos("np-one") < pos("np-two"));
    }

    #[test]
    fn reorder_by_position_recurses_into_grandchildren_without_relevel() {
        let doc = r#"# Root
<!-- meshfox:node id="root" -->

## Parent
<!-- meshfox:node id="parent" x=0 y=0 -->

### Child B
<!-- meshfox:node id="child-b" x=0 y=100 -->

### Child A
<!-- meshfox:node id="child-a" x=0 y=0 -->
"#;
        let reordered = reorder_by_position(doc).unwrap();
        let c = parse(&reordered).unwrap();
        assert!(same_ids(&c, &parse(doc).unwrap()));
        let pos = |id: &str| reordered.find(&format!("id=\"{id}\"")).unwrap();
        assert!(pos("child-a") < pos("child-b"));
        assert_eq!(c.node("child-a").unwrap().level, 3);
        assert_eq!(c.node("child-a").unwrap().parent.as_deref(), Some("parent"));
    }

    #[test]
    fn reorder_by_position_missing_root_is_none() {
        assert_eq!(reorder_by_position("not a heading at all"), None);
    }

    fn abc_doc() -> &'static str {
        concat!(
            "# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
            "## A\n<!-- meshfox:node id=\"a\" -->\n\nbody a\n\n",
            "## B\n<!-- meshfox:node id=\"b\" -->\n\nbody b\n\n",
            "## C\n<!-- meshfox:node id=\"c\" -->\n\nbody c\n",
        )
    }

    fn doc_order(markdown: &str) -> Vec<String> {
        parse(markdown)
            .unwrap()
            .nodes
            .iter()
            .map(|n| n.id.clone())
            .collect()
    }

    #[test]
    fn move_sibling_moves_before_a_target() {
        let updated = move_sibling(abc_doc(), "c", "a", MoveSiblingPosition::Before).unwrap();
        assert_eq!(doc_order(&updated), vec!["root", "c", "a", "b"]);
    }

    #[test]
    fn move_sibling_moves_after_a_target() {
        let updated = move_sibling(abc_doc(), "a", "c", MoveSiblingPosition::After).unwrap();
        assert_eq!(doc_order(&updated), vec!["root", "b", "c", "a"]);
    }

    #[test]
    fn move_sibling_is_unaffected_by_target_position_relative_to_the_moved_node() {
        // Moving something *earlier* in the file (target comes after it)
        // must re-find the target's shifted position correctly, not the
        // stale one from before extraction.
        let updated = move_sibling(abc_doc(), "a", "b", MoveSiblingPosition::After).unwrap();
        assert_eq!(doc_order(&updated), vec!["root", "b", "a", "c"]);
    }

    #[test]
    fn move_sibling_carries_every_descendant_along_as_one_unit() {
        let doc = concat!(
            "# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
            "## A\n<!-- meshfox:node id=\"a\" -->\n\n",
            "### A Child\n<!-- meshfox:node id=\"a-child\" -->\n\nbody\n\n",
            "## B\n<!-- meshfox:node id=\"b\" -->\n\nbody b\n",
        );
        let updated = move_sibling(doc, "a", "b", MoveSiblingPosition::After).unwrap();
        assert_eq!(doc_order(&updated), vec!["root", "b", "a", "a-child"]);
        let c = parse(&updated).unwrap();
        assert_eq!(c.node("a-child").unwrap().parent.as_deref(), Some("a"));
    }

    #[test]
    fn move_sibling_rejects_nodes_with_different_parents() {
        let doc = concat!(
            "# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
            "## A\n<!-- meshfox:node id=\"a\" -->\n\n",
            "### A Child\n<!-- meshfox:node id=\"a-child\" -->\n\nbody\n\n",
            "## B\n<!-- meshfox:node id=\"b\" -->\n\nbody b\n",
        );
        assert_eq!(
            move_sibling(doc, "a-child", "b", MoveSiblingPosition::Before),
            Err(MoveSiblingError::NotSiblings(
                "a-child".to_string(),
                "b".to_string()
            ))
        );
    }

    #[test]
    fn move_sibling_rejects_the_same_node_as_its_own_target() {
        assert_eq!(
            move_sibling(abc_doc(), "a", "a", MoveSiblingPosition::Before),
            Err(MoveSiblingError::SameNode)
        );
    }

    #[test]
    fn move_sibling_rejects_missing_nodes() {
        assert_eq!(
            move_sibling(abc_doc(), "does-not-exist", "a", MoveSiblingPosition::Before),
            Err(MoveSiblingError::NotFound("does-not-exist".to_string()))
        );
        assert_eq!(
            move_sibling(abc_doc(), "a", "does-not-exist", MoveSiblingPosition::Before),
            Err(MoveSiblingError::NotFound("does-not-exist".to_string()))
        );
    }

    #[test]
    fn delete_node_reparent_children_promotes_direct_children() {
        let updated = delete_node_reparent_children(DOC, "tests").unwrap();
        let c = parse(&updated).unwrap();
        assert!(c.node("tests").is_none());
        // smoke-test survives, now directly under root instead of tests,
        // one level shallower to match.
        let smoke = c.node("smoke-test").unwrap();
        assert_eq!(smoke.parent.as_deref(), Some("root"));
        assert_eq!(smoke.level, 2);
        assert!(smoke.text.contains("```bash name=\"smoke\" cache"));
        // unrelated sibling subtree untouched
        assert!(c.node("examples").is_some());
        assert!(c.node("shared-smoke").is_some());
    }

    #[test]
    fn delete_node_reparent_children_multiple_children_stay_siblings_in_order() {
        let (doc, second) = insert_child_node(DOC, "tests", "Second Check").unwrap();
        let updated = delete_node_reparent_children(&doc, "tests").unwrap();
        let c = parse(&updated).unwrap();
        assert!(c.node("tests").is_none());
        assert_eq!(
            c.node("smoke-test").unwrap().parent.as_deref(),
            Some("root")
        );
        assert_eq!(c.node(&second).unwrap().parent.as_deref(), Some("root"));
        // original document order (smoke-test was already there; "second"
        // was appended after it) is preserved among the promoted siblings —
        // both now one level shallower ("##" instead of "###").
        let smoke_pos = updated.find("## Smoke Test").unwrap();
        let second_pos = updated.find(&format!("id=\"{second}\"")).unwrap();
        assert!(smoke_pos < second_pos);
    }

    #[test]
    fn delete_node_reparent_children_leaf_with_no_children_is_a_plain_delete() {
        let updated = delete_node_reparent_children(DOC, "smoke-test").unwrap();
        let c = parse(&updated).unwrap();
        assert!(c.node("smoke-test").is_none());
        assert!(c.node("tests").is_some());
        assert!(c.node("examples").is_some());
    }

    #[test]
    fn delete_node_reparent_children_drops_dangling_extra_edges_pointing_at_it() {
        // shared-smoke has `<!-- meshfox:edge from="tests" -->` — deleting
        // `tests` must drop that reference too, even though `tests`' own
        // child (smoke-test) survives, promoted elsewhere.
        let updated = delete_node_reparent_children(DOC, "tests").unwrap();
        let c = parse(&updated).unwrap();
        assert!(c.node("shared-smoke").unwrap().extra_parents.is_empty());
    }

    #[test]
    fn delete_node_reparent_children_root_is_none() {
        assert_eq!(delete_node_reparent_children(DOC, "root"), None);
    }

    #[test]
    fn delete_node_reparent_children_missing_id_is_none() {
        assert_eq!(delete_node_reparent_children(DOC, "does-not-exist"), None);
    }

    #[test]
    fn delete_node_reparent_children_past_heading_ceiling_infers_via_outline() {
        // n6 is `######` (level 6); n7 is its child, also `######` (nowhere
        // deeper to go), disambiguated only by an explicit `parent="n6"`.
        // Deleting n6 and promoting n7 up to n6's own parent (n5, level 5)
        // means n7 now fits under plain heading-outline inference again
        // (level 6 directly follows level 5) — no explicit `parent=` left.
        let (doc, n6) = nest_to_heading_ceiling();
        let n5 = parse(&doc)
            .unwrap()
            .node(&n6)
            .unwrap()
            .parent
            .clone()
            .unwrap();
        let (doc, n7) = insert_child_node(&doc, &n6, "N7").unwrap();

        let updated = delete_node_reparent_children(&doc, &n6).unwrap();
        let c = parse(&updated).unwrap();
        assert!(c.node(&n6).is_none());
        assert_eq!(c.node(&n7).unwrap().parent.as_deref(), Some(n5.as_str()));
        assert_eq!(c.node(&n7).unwrap().level, 6);
        assert!(!updated.contains(&format!("parent=\"{n5}\"")));
    }

    #[test]
    fn delete_node_reparent_children_past_heading_ceiling_needs_explicit_parent() {
        // n6, n7, n8 are all `######` (n7 parent="n6", n8 parent="n7").
        // Deleting n7 promotes n8 up to n6 — still `######`, and n6 is
        // *also* `######`, so plain outline inference can't tell n8 apart
        // from a sibling of n6 anymore: it needs an explicit `parent="n6"`.
        let (doc, n6) = nest_to_heading_ceiling();
        let (doc, n7) = insert_child_node(&doc, &n6, "N7").unwrap();
        let (doc, n8) = insert_child_node(&doc, &n7, "N7 child").unwrap();

        let updated = delete_node_reparent_children(&doc, &n7).unwrap();
        let c = parse(&updated).unwrap();
        assert!(c.node(&n7).is_none());
        assert_eq!(c.node(&n8).unwrap().parent.as_deref(), Some(n6.as_str()));
        assert_eq!(c.node(&n8).unwrap().level, 6);
        assert!(c.node(&n6).is_some());
    }

    #[test]
    fn reparent_node_promotes_an_extra_parent_to_structural() {
        // shared-smoke's structural parent is "examples"; its one extra
        // parent is "tests" — promote that.
        let updated = reparent_node(DOC, "shared-smoke", "tests").unwrap();
        let c = parse(&updated).unwrap();
        let shared = c.node("shared-smoke").unwrap();
        assert_eq!(shared.parent.as_deref(), Some("tests"));
        assert!(shared.extra_parents.is_empty());
        // physically relocated: now inside "tests"' subtree, after its
        // existing child smoke-test, no longer inside "examples"' subtree
        let tests_pos = updated.find("## Tests").unwrap();
        let examples_pos = updated.find("## Examples").unwrap();
        let shared_pos = updated.find("id=\"shared-smoke\"").unwrap();
        assert!(tests_pos < shared_pos);
        assert!(shared_pos < examples_pos);
        // level matches its new depth (child of a level-2 node -> level 3,
        // same as it already was under examples, so no visible heading
        // change beyond the move itself)
        assert_eq!(shared.level, 3);
        // former sibling subtree, examples, still has its own remaining children
        assert!(c.node("examples").is_some());
    }

    #[test]
    fn reparent_node_only_drops_the_promoted_extra_parent_others_stay() {
        let doc = "# Root\n\n## A\n<!-- meshfox:node id=\"a\" -->\n\n## B\n<!-- meshfox:node id=\"b\" -->\n\n## C\n<!-- meshfox:node id=\"c\" -->\n<!-- meshfox:edge from=\"a\" -->\n<!-- meshfox:edge from=\"b\" -->\n";
        let updated = reparent_node(doc, "c", "a").unwrap();
        let c = parse(&updated).unwrap();
        let node_c = c.node("c").unwrap();
        assert_eq!(node_c.parent.as_deref(), Some("a"));
        assert_eq!(node_c.extra_parents, vec![ExtraEdge::new("b")]);
    }

    #[test]
    fn reparent_node_rejects_a_target_not_in_extra_parents() {
        // "root" is unrelated to shared-smoke's declared edges (only "tests" is).
        assert_eq!(reparent_node(DOC, "shared-smoke", "root"), None);
    }

    #[test]
    fn reparent_node_rejects_the_root() {
        assert_eq!(reparent_node(DOC, "root", "tests"), None);
    }

    #[test]
    fn reparent_node_rejects_a_cycle_onto_its_own_descendant() {
        // "a" declares an extra edge from "b", but "b" is a's own
        // structural child — promoting it would make each the other's
        // ancestor.
        let doc = "# Root\n\n## A\n<!-- meshfox:node id=\"a\" -->\n<!-- meshfox:edge from=\"b\" -->\n\n### B\n<!-- meshfox:node id=\"b\" -->\n";
        assert_eq!(reparent_node(doc, "a", "b"), None);
    }

    #[test]
    fn reparent_node_missing_ids_are_none() {
        assert_eq!(reparent_node(DOC, "does-not-exist", "tests"), None);
        assert_eq!(reparent_node(DOC, "shared-smoke", "does-not-exist"), None);
    }

    #[test]
    fn reparent_node_past_heading_ceiling_needs_explicit_parent() {
        // n6 and n7 are both `######`; n8 is n7's child (also `######`,
        // explicit parent="n7"), and separately declares an extra edge
        // from n6. Promoting that edge moves n8 under n6 — still
        // `######`, and n6 is *also* `######`, so it still needs an
        // explicit `parent="n6"` afterward (just pointing at a different
        // id than before).
        let (doc, n6) = nest_to_heading_ceiling();
        let (doc, n7) = insert_child_node(&doc, &n6, "N7").unwrap();
        let (doc, n8) = insert_child_node(&doc, &n7, "N8").unwrap();
        let doc = set_node_edges(&doc, &n8, &[ExtraEdge::new(n6.clone())]).unwrap();

        let updated = reparent_node(&doc, &n8, &n6).unwrap();
        let c = parse(&updated).unwrap();
        assert_eq!(c.node(&n8).unwrap().parent.as_deref(), Some(n6.as_str()));
        assert_eq!(c.node(&n8).unwrap().level, 6);
        assert!(c.node(&n8).unwrap().extra_parents.is_empty());
        assert!(c.node(&n7).is_some());
    }

    #[test]
    fn rename_node_id_updates_the_nodes_own_id() {
        let updated = rename_node_id(DOC, "tests", "checks").unwrap();
        let c = parse(&updated).unwrap();
        assert!(c.node("tests").is_none());
        assert_eq!(c.node("checks").unwrap().title, "Tests");
    }

    #[test]
    fn rename_node_id_is_a_no_op_when_ids_match() {
        assert_eq!(rename_node_id(DOC, "tests", "tests").unwrap(), DOC);
    }

    #[test]
    fn rename_node_id_rejects_missing_node() {
        assert_eq!(
            rename_node_id(DOC, "does-not-exist", "new-id"),
            Err(RenameIdError::NotFound("does-not-exist".to_string()))
        );
    }

    #[test]
    fn rename_node_id_rejects_a_duplicate() {
        assert_eq!(
            rename_node_id(DOC, "tests", "examples"),
            Err(RenameIdError::AlreadyExists("examples".to_string()))
        );
    }

    #[test]
    fn rename_node_id_rejects_empty_and_quoted_ids() {
        assert_eq!(rename_node_id(DOC, "tests", ""), Err(RenameIdError::Empty));
        assert_eq!(
            rename_node_id(DOC, "tests", "has\"quote"),
            Err(RenameIdError::InvalidChar)
        );
    }

    #[test]
    fn rename_node_id_rejects_a_comma() {
        assert_eq!(
            rename_node_id(DOC, "tests", "a,b"),
            Err(RenameIdError::InvalidChar)
        );
    }

    #[test]
    fn rename_node_id_accepts_a_space_or_non_latin_script() {
        let updated = rename_node_id(DOC, "tests", "has space").unwrap();
        let c = parse(&updated).unwrap();
        assert_eq!(c.node("has space").unwrap().title, "Tests");

        let updated = rename_node_id(DOC, "tests", "раздел").unwrap();
        let c = parse(&updated).unwrap();
        assert_eq!(c.node("раздел").unwrap().title, "Tests");
    }

    #[test]
    fn rename_node_id_sweeps_meshfox_edge_from_references() {
        // "shared-smoke" declares `<!-- meshfox:edge from="tests" -->` —
        // renaming "tests" must update that reference too, or the document
        // fails to parse (a dangling `from=`).
        let updated = rename_node_id(DOC, "tests", "checks").unwrap();
        let c = parse(&updated).unwrap();
        assert_eq!(
            c.node("shared-smoke").unwrap().extra_parents,
            vec![ExtraEdge::new("checks")]
        );
    }

    #[test]
    fn rename_node_id_sweeps_explicit_parent_attributes() {
        // n7 is past the heading ceiling, so it carries an explicit
        // `parent="n6"` attribute (see nest_to_heading_ceiling) rather than
        // relying on heading depth — renaming n6 must update it too.
        let (doc, n6) = nest_to_heading_ceiling();
        let (doc, n7) = insert_child_node(&doc, &n6, "N7").unwrap();

        let updated = rename_node_id(&doc, &n6, "n6-renamed").unwrap();
        let c = parse(&updated).unwrap();
        assert_eq!(c.node(&n7).unwrap().parent.as_deref(), Some("n6-renamed"));
    }

    #[test]
    fn rename_node_id_sweeps_deps_references() {
        let doc = concat!(
            "# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
            "## Build\n<!-- meshfox:node id=\"build-node\" -->\n\n",
            "```bash name=\"build\" cache\necho build\n```\n\n",
            "## Deploy\n<!-- meshfox:node id=\"deploy-node\" -->\n\n",
            "```bash name=\"deploy\" deps=\"build-node/build,other\"\necho deploy\n```\n",
        );
        let updated = rename_node_id(doc, "build-node", "build").unwrap();
        assert!(updated.contains(r#"deps="build/build,other""#));
        let c = parse(&updated).unwrap();
        assert!(c.node("build").is_some());
        let deploy = c.node("deploy-node").unwrap();
        let blocks = crate::fence::scan_runnable_blocks("deploy-node", &deploy.text);
        assert_eq!(
            blocks[0].deps,
            vec![
                crate::fence::BlockRef {
                    node_id: Some("build".to_string()),
                    block_name: "build".to_string(),
                    sync: false,
                },
                crate::fence::BlockRef {
                    node_id: None,
                    block_name: "other".to_string(),
                    sync: false,
                },
            ]
        );
    }

    #[test]
    fn clear_node_id_drops_the_attribute_when_the_id_already_matches_the_title_slug() {
        // "tests" is already exactly `slugify("Tests")` — clearing it has
        // nothing to rename, just the now-redundant attribute to drop.
        let (updated, new_id) = clear_node_id(DOC, "tests").unwrap();
        assert_eq!(new_id, "tests");
        assert!(!updated.contains(r#"id="tests""#));
        let c = parse(&updated).unwrap();
        assert_eq!(c.node("tests").unwrap().title, "Tests");
        // Everything that referenced "tests" by id must still resolve —
        // clearing shouldn't have silently broken cross-references.
        assert_eq!(
            c.node("shared-smoke").unwrap().extra_parents,
            vec![ExtraEdge::new("tests")]
        );
    }

    #[test]
    fn clear_node_id_rederives_and_sweeps_references_when_the_id_diverged_from_the_title() {
        // Give "tests" a custom id that no longer matches its title's
        // slug — clearing it now must (a) actually rename it back to
        // `slugify(title)` and (b) sweep every reference to the old id,
        // exactly like `rename_node_id` does.
        let renamed = rename_node_id(DOC, "tests", "custom-id").unwrap();
        let (updated, new_id) = clear_node_id(&renamed, "custom-id").unwrap();
        assert_eq!(new_id, "tests");
        assert!(!updated.contains(r#"id="tests""#));
        assert!(!updated.contains("custom-id"));
        let c = parse(&updated).unwrap();
        assert_eq!(c.node("tests").unwrap().title, "Tests");
        assert_eq!(
            c.node("shared-smoke").unwrap().extra_parents,
            vec![ExtraEdge::new("tests")]
        );
    }

    #[test]
    fn clear_node_id_dedupes_the_derived_slug_against_other_ids() {
        // Renaming "tests" to something whose title-slug ("examples")
        // would collide with the unrelated "examples" node already in
        // DOC (which comes *later* in the document) — clearing it must
        // fall back to the same `-2` dedup `unique_slug` uses everywhere
        // else, and the result must actually still parse (see
        // `clear_node_id`'s own doc comment on why that's not a given:
        // this exact setup — an id-less node whose title-slug matches a
        // later node's still-explicit id — is precisely the case where
        // going fully id-less would otherwise re-derive a colliding id at
        // parse time instead).
        let doc = rename_node_id(DOC, "tests", "renamed-tests").unwrap();
        let doc = set_node_title(&doc, "renamed-tests", "Examples").unwrap();
        let (updated, new_id) = clear_node_id(&doc, "renamed-tests").unwrap();
        assert_eq!(new_id, "examples-2");
        let c = parse(&updated).unwrap();
        assert!(c.node("examples-2").is_some());
        assert!(c.node("examples").is_some());
    }

    #[test]
    fn clear_node_id_keeps_an_explicit_attribute_when_going_id_less_would_reparse_wrong() {
        // Same setup as the dedup test above: going fully id-less here
        // would make `assign_ids` re-derive plain "examples" for this
        // node (it comes first in the file, so it "wins" the bare slug
        // before the real "examples" node is even reached) — a silent
        // swap of *which* node answers to "examples" at best, a
        // duplicate-id parse error at worst. `clear_node_id` must detect
        // this and keep an explicit `id="examples-2"` instead of dropping
        // the attribute.
        let doc = rename_node_id(DOC, "tests", "renamed-tests").unwrap();
        let doc = set_node_title(&doc, "renamed-tests", "Examples").unwrap();
        let (updated, new_id) = clear_node_id(&doc, "renamed-tests").unwrap();
        assert_eq!(new_id, "examples-2");
        assert!(updated.contains(r#"id="examples-2""#));
    }

    #[test]
    fn clear_node_id_rejects_missing_node() {
        assert_eq!(
            clear_node_id(DOC, "does-not-exist"),
            Err(ClearIdError::NotFound("does-not-exist".to_string()))
        );
    }

    // TODO.canvas.md: "Ошибка при неизвестных параметрах в validate" —
    // `unknown_node_edge_attr` is a `validate`-only check, separate from
    // `parse` itself (which must keep silently accepting an attribute it
    // doesn't recognize, for forward/backward format compatibility).
    #[test]
    fn unknown_node_edge_attr_is_none_for_a_document_using_only_known_attrs() {
        assert_eq!(unknown_node_edge_attr(DOC), None);
    }

    #[test]
    fn unknown_node_edge_attr_catches_a_typo_d_node_attribute() {
        let doc = "# Root\n<!-- meshfox:node id=\"root\" colr=\"1\" -->\n\nbody\n";
        let err = unknown_node_edge_attr(doc).expect("colr is not a known attribute");
        assert_eq!(err.attr, "colr");
        assert!(err.context.contains("Root"));
    }

    #[test]
    fn unknown_node_edge_attr_catches_a_typo_d_edge_attribute() {
        let doc = "# Root\n<!-- meshfox:node id=\"root\" -->\n\n## Child\n<!-- meshfox:node id=\"child\" -->\n<!-- meshfox:edge form=\"root\" -->\n\nbody\n";
        let err = unknown_node_edge_attr(doc).expect("form (typo for from) is not known");
        assert_eq!(err.attr, "form");
        assert!(err.context.contains("Child"));
    }

    #[test]
    fn unknown_node_edge_attr_ignores_a_meshfox_node_comment_written_inside_a_fence() {
        let doc = "# Root\n<!-- meshfox:node id=\"root\" -->\n\n```\n<!-- meshfox:node id=\"x\" colr=\"1\" -->\n```\n";
        assert_eq!(unknown_node_edge_attr(doc), None);
    }
}

