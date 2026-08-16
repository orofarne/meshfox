//! The in-memory canvas model: a tree (plus optional extra cross-edges) of
//! Markdown-bearing nodes.
//!
//! This struct is deliberately format-agnostic — `crate::mdcanvas` is what
//! knows how to read/write it as the on-disk `.canvas.md` Markdown outline.
//! It also doubles as the JSON shape the server hands to the web UI over
//! HTTP, which is why it derives `Serialize`/`Deserialize`.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct Canvas {
    pub nodes: Vec<Node>,
    /// Every `<!-- meshfox:option name="..." -->` this document declares
    /// (see `crate::options`) — e.g. `unfold`, which flips the web UI's
    /// own default fold state for the whole document. Never set by
    /// `mdcanvas::parse` itself (a malformed declaration shouldn't break
    /// basic parsing — `meshfox validate` is what surfaces that loudly, see
    /// `options::declared_options`); populated by whichever consumer wants
    /// it, same convention `Node::constraint_results`/`asset_base` already
    /// use.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<String>,
}

/// JSON Canvas's four node types, plus meshfox's own `include` (see
/// `crate::include`). `Text` is the default and the only one with freeform
/// Markdown content — see `mdcanvas` for the `type=` attribute and
/// per-type body constraints.
///
/// There's no `Constraint` variant: a Starlark contract (see
/// `crate::constraint`) is a ` ```starlark constraint ` fence embedded in
/// any node's body, same as a runnable ` ```bash name="..." ` fence — not a
/// node type of its own.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum NodeType {
    #[default]
    Text,
    File,
    Link,
    Group,
    /// Body is a single Markdown link, same shape as `File`/`Link` — but
    /// the target is another Markdown (or `.canvas.md`) file whose content
    /// gets spliced in dynamically wherever a consumer resolves includes
    /// (`crate::include::resolve`, called by the server before serving
    /// `GET /api/canvas`). Never resolved on disk: `run`/`validate` parse
    /// the raw file and see the bare link, same as `file`/`link`.
    Include,
}

impl NodeType {
    pub fn is_text(&self) -> bool {
        matches!(self, NodeType::Text)
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            NodeType::Text => "text",
            NodeType::File => "file",
            NodeType::Link => "link",
            NodeType::Group => "group",
            NodeType::Include => "include",
        }
    }
}

/// An extra incoming edge (`meshfox:edge from="..."`), plus the optional
/// per-edge styling attributes on that same comment line — label text,
/// stroke color, line style, and arrowhead choice at each end. `None` on
/// any of these means "not written", not "explicitly cleared": `mdcanvas`
/// omits the attribute entirely rather than writing e.g. `style="solid"`,
/// and a client rendering an edge with `None` here is free to pick its own
/// default (see the web UI, which keeps the pre-existing dashed/arrow-end
/// look for an edge that never had these attributes at all).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub struct ExtraEdge {
    pub from: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub style: Option<EdgeLineStyle>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arrow_start: Option<ArrowEnd>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arrow_end: Option<ArrowEnd>,
    /// Free-form labels (`tags="a,b,c"` on `meshfox:edge`) — purely
    /// descriptive, no structural meaning, same as a node's own `tags`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
}

impl ExtraEdge {
    /// A bare edge with no styling — same shape a plain `meshfox:edge
    /// from="..."` line (or the old `Vec<String>` form) parses to.
    pub fn new(from: impl Into<String>) -> Self {
        ExtraEdge { from: from.into(), ..Default::default() }
    }
}

/// An extra edge's line style — `style=` on `meshfox:edge`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum EdgeLineStyle {
    Solid,
    Dashed,
    Dotted,
}

impl EdgeLineStyle {
    pub fn as_str(&self) -> &'static str {
        match self {
            EdgeLineStyle::Solid => "solid",
            EdgeLineStyle::Dashed => "dashed",
            EdgeLineStyle::Dotted => "dotted",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "solid" => Some(EdgeLineStyle::Solid),
            "dashed" => Some(EdgeLineStyle::Dashed),
            "dotted" => Some(EdgeLineStyle::Dotted),
            _ => None,
        }
    }
}

/// Whether an extra edge shows an arrowhead at a given end — `arrowStart=`/
/// `arrowEnd=` on `meshfox:edge`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ArrowEnd {
    None,
    Arrow,
}

impl ArrowEnd {
    pub fn as_str(&self) -> &'static str {
        match self {
            ArrowEnd::None => "none",
            ArrowEnd::Arrow => "arrow",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "none" => Some(ArrowEnd::None),
            "arrow" => Some(ArrowEnd::Arrow),
            _ => None,
        }
    }
}

/// How a `file` node's target is shown on the canvas. `display=` on
/// `meshfox:node` — only meaningful (and only ever parsed/rendered) for
/// `type="file"`; see `mdcanvas`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum FileDisplay {
    #[default]
    Link,
    Code,
}

impl FileDisplay {
    pub fn is_link(&self) -> bool {
        matches!(self, FileDisplay::Link)
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            FileDisplay::Link => "link",
            FileDisplay::Code => "code",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Node {
    /// Stable identity used for CLI/API addressing. Defaults to a slug of
    /// `title` the first time the document is parsed, but once a
    /// `meshfox:node` comment has been written for a node, that id is
    /// pinned regardless of later title edits.
    pub id: String,
    /// The literal heading text (what's rendered), independent of `id`.
    pub title: String,
    /// Markdown heading level: 1 for the root, 2+ for everything else.
    pub level: u8,
    #[serde(default, rename = "type", skip_serializing_if = "NodeType::is_text")]
    pub node_type: NodeType,
    /// The node whose heading lexically encloses this one. `None` only for
    /// the root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    /// Extra incoming edges beyond `parent`, declared via `meshfox:edge`
    /// comments — for graphs that aren't a clean nesting tree.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extra_parents: Vec<ExtraEdge>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub x: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub y: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    /// `fold="true"`/`fold="false"` on `meshfox:node` — an explicit
    /// per-node override of the document's own default fold state (see
    /// `Canvas::options`' `unfold` option and SPEC.md's "Options"
    /// section), for a node whose subtree should always start folded (or
    /// always expanded) regardless of what the rest of the document
    /// defaults to. `None` means no override — the node just follows
    /// whatever the document-wide default resolves to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fold: Option<bool>,
    /// Free-form labels (`tags="a,b,c"` on `meshfox:node`) — purely
    /// descriptive, no structural meaning (unlike `parent`/`extra_parents`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// For `file`/`link` nodes: the path or URL, parsed out of the body's
    /// one Markdown link. `None` for `text`/`group`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    /// `file`-node only: how the target is shown on the canvas — a plain
    /// clickable link (default), or a read-only syntax-highlighted preview
    /// of the target file's own content. `None` for every other node type.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display: Option<FileDisplay>,
    /// `file`-node only: language hint for the `display="code"` preview's
    /// syntax highlighting (e.g. `"rust"`). `None` means auto-detect from
    /// the target's file extension.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lang: Option<String>,
    /// `file`-node only: an executable (e.g. `"python"`) to run against
    /// `target`, making the node runnable — see `Node::is_runnable_file`.
    /// `None` means the node isn't runnable this way.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interpreter: Option<String>,
    /// Label text for the *structural* edge from `parent` into this node
    /// (`edgeLabel=` on `meshfox:node`) — the implicit nesting edge has no
    /// dedicated line of its own the way an `ExtraEdge` does (see
    /// `ExtraEdge::label`), so this lives on the child end instead: "the
    /// label of the edge that points at me". `None` for the root (no
    /// incoming edge to label) and for every node that's never had one set.
    /// Purely descriptive text — unlike `ExtraEdge`, a structural edge has
    /// no color/style/arrowhead attributes to go with it (see SPEC.md).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edge_label: Option<String>,
    /// Markdown body between this heading (and its meshfox comments) and
    /// the next heading. For `group`, always empty. For `file`/`link`,
    /// always exactly the one Markdown link `target` was parsed from.
    pub text: String,
    /// Most recently evaluated result of every ` ```starlark constraint `
    /// fence embedded in this node's body (see
    /// `crate::constraint::annotate_status`), one entry per fence, in
    /// document order. Never set by `mdcanvas::parse` itself — only by
    /// whatever consumer wants it (the server, before serving `GET
    /// /api/canvas`). Empty just means this consumer didn't evaluate this
    /// node's constraints (or it has none), not that they passed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub constraint_results: Vec<crate::constraint::ConstraintStatus>,
    /// Absolute directory a relative asset reference (an `![](...)` image,
    /// or a plain link) inside this node's `text` should resolve against,
    /// when that differs from the including document's own directory —
    /// i.e. this node's body came from an `include` target that lives
    /// elsewhere on disk (see `crate::include::resolve`). `None` for every
    /// node that wasn't spliced in from an include, which keeps resolving
    /// relative to the canvas file's own directory as before. Never set by
    /// `mdcanvas::parse` itself, same as `constraint_results`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asset_base: Option<String>,
    /// Where this node's own content actually lives on disk, when that
    /// differs from the document being viewed — i.e. this node was
    /// spliced in from a canvas `include` target (`crate::include::resolve`).
    /// `origin_path` is that target file's own canonical path; `origin_id`
    /// is this node's id *within that file*, before `include::resolve`
    /// namespaced it to `{include_id}/{original_id}` (see `Node::id`).
    /// `None` for every node that lives directly in the document being
    /// viewed, including the `include` node itself (only its *spliced-in
    /// descendants* get an origin). Never set by `mdcanvas::parse`, and
    /// never sent over the wire (`#[serde(skip)]`, unlike `asset_base`) —
    /// this is a same-process breadcrumb for a consumer that wants to
    /// *write back* to the right file (the server's mutating endpoints
    /// re-derive it themselves by resolving again, rather than trusting a
    /// client-echoed path).
    #[serde(skip)]
    pub origin_path: Option<String>,
    #[serde(skip)]
    pub origin_id: Option<String>,
}

impl Node {
    /// True for a `file` node with both a `target` and a non-empty
    /// `interpreter` set — eligible to run as `interpreter target` (see
    /// SPEC.md's "Node types"). `link`/every other type never qualifies,
    /// same restriction `display`/`lang` already have.
    pub fn is_runnable_file(&self) -> bool {
        self.node_type == NodeType::File
            && self.target.is_some()
            && self.interpreter.as_deref().is_some_and(|i| !i.trim().is_empty())
    }
}

impl Canvas {
    pub fn node(&self, id: &str) -> Option<&Node> {
        self.nodes.iter().find(|n| n.id == id)
    }

    pub fn node_mut(&mut self, id: &str) -> Option<&mut Node> {
        self.nodes.iter_mut().find(|n| n.id == id)
    }

    /// Direct children by document nesting (i.e. `parent == id`) — this is
    /// the tree used for CLI/UI path addressing. Does not include nodes
    /// that only reference `id` via an extra `meshfox:edge`.
    pub fn children(&self, id: &str) -> Vec<&Node> {
        self.nodes
            .iter()
            .filter(|n| n.parent.as_deref() == Some(id))
            .collect()
    }

    /// Absolute position for `id`, walking up through `group`-typed
    /// ancestors accumulating each one's own real anchor — a group's own
    /// `x`/`y` mean "top-left of this group's frame in its own parent's
    /// frame" (see SPEC.md), so a member's stored `x`/`y` is relative to
    /// its nearest group ancestor, not the whole document. Stops
    /// (harmlessly leaving `x`/`y` untouched) at the first non-`group`
    /// ancestor or the root, so this is exactly `(node.x, node.y)` for any
    /// node that isn't nested under a group at all.
    ///
    /// `None` if `id` itself has no stored `x`/`y`, or any `group` ancestor
    /// in the chain has no anchor of its own (never dragged) — an
    /// intentional "no heuristic" answer: `layout`/the web client's own
    /// auto-layout supply their own synthetic fallback for this case,
    /// `staticgen` treats `None` as "not really positioned" (flows via
    /// CSS instead of a fixed pixel position).
    pub fn resolve_absolute_position(&self, id: &str) -> Option<(f64, f64)> {
        let node = self.node(id)?;
        let (mut x, mut y) = (node.x?, node.y?);
        let mut current = node.parent.as_deref();
        while let Some(parent_id) = current {
            let parent = self.node(parent_id)?;
            if parent.node_type != NodeType::Group {
                break;
            }
            x += parent.x?;
            y += parent.y?;
            current = parent.parent.as_deref();
        }
        Some((x, y))
    }

    /// Inverse of `resolve_absolute_position`: converts an absolute
    /// `(abs_x, abs_y)` into whatever frame `id` should store it in given
    /// its *current* parent chain, subtracting each `group`-typed
    /// ancestor's own anchor along the way. `None` under the same
    /// "an ancestor group has no anchor yet" condition
    /// `resolve_absolute_position` uses — the caller (e.g. a reparent) is
    /// expected to leave the node's stored position untouched in that case
    /// rather than inventing a synthetic one.
    pub fn absolute_to_local(&self, id: &str, abs_x: f64, abs_y: f64) -> Option<(f64, f64)> {
        let node = self.node(id)?;
        let (mut x, mut y) = (abs_x, abs_y);
        let mut current = node.parent.as_deref();
        while let Some(parent_id) = current {
            let parent = self.node(parent_id)?;
            if parent.node_type != NodeType::Group {
                break;
            }
            x -= parent.x?;
            y -= parent.y?;
            current = parent.parent.as_deref();
        }
        Some((x, y))
    }
}

#[cfg(test)]
mod tests {
    use crate::mdcanvas::parse;

    #[test]
    fn resolve_absolute_position_adds_the_groups_own_anchor() {
        let doc = "# Root\n\n## Frame\n<!-- meshfox:node type=\"group\" x=100 y=100 -->\n\n### Member\n<!-- meshfox:node x=20 y=20 -->\n\nbody\n";
        let canvas = parse(doc).unwrap();
        assert_eq!(canvas.resolve_absolute_position("member"), Some((120.0, 120.0)));
    }

    #[test]
    fn resolve_absolute_position_is_none_when_a_group_ancestor_has_no_anchor() {
        let doc = "# Root\n\n## Frame\n<!-- meshfox:node type=\"group\" -->\n\n### Member\n<!-- meshfox:node x=20 y=20 -->\n\nbody\n";
        let canvas = parse(doc).unwrap();
        assert_eq!(canvas.resolve_absolute_position("member"), None);
    }

    #[test]
    fn resolve_absolute_position_is_none_when_the_node_itself_is_unpositioned() {
        let doc = "# Root\n\n## Frame\n<!-- meshfox:node type=\"group\" x=100 y=100 -->\n\n### Member\n<!-- meshfox:node -->\n\nbody\n";
        let canvas = parse(doc).unwrap();
        assert_eq!(canvas.resolve_absolute_position("member"), None);
    }

    #[test]
    fn resolve_absolute_position_is_plain_xy_for_a_node_not_nested_under_any_group() {
        let doc = "# Root\n\n## Section\n<!-- meshfox:node x=50 y=60 -->\n\nbody\n";
        let canvas = parse(doc).unwrap();
        assert_eq!(canvas.resolve_absolute_position("section"), Some((50.0, 60.0)));
    }

    #[test]
    fn resolve_absolute_position_compounds_through_nested_groups() {
        let doc = "# Root\n\n###### Outer\n<!-- meshfox:node id=\"outer\" type=\"group\" x=10 y=10 -->\n\n###### Inner\n<!-- meshfox:node id=\"inner\" type=\"group\" parent=\"outer\" x=5 y=5 -->\n\n###### Leaf\n<!-- meshfox:node id=\"leaf\" parent=\"inner\" x=2 y=2 -->\n\nbody\n";
        let canvas = parse(doc).unwrap();
        assert_eq!(canvas.resolve_absolute_position("leaf"), Some((17.0, 17.0)));
    }

    #[test]
    fn absolute_to_local_round_trips_through_resolve_absolute_position() {
        let doc = "# Root\n\n## Frame\n<!-- meshfox:node type=\"group\" x=100 y=100 -->\n\n### Member\n<!-- meshfox:node x=20 y=20 -->\n\nbody\n";
        let canvas = parse(doc).unwrap();
        let (abs_x, abs_y) = canvas.resolve_absolute_position("member").unwrap();
        assert_eq!(canvas.absolute_to_local("member", abs_x, abs_y), Some((20.0, 20.0)));
    }

    #[test]
    fn absolute_to_local_is_none_when_a_group_ancestor_has_no_anchor() {
        let doc = "# Root\n\n## Frame\n<!-- meshfox:node type=\"group\" -->\n\n### Member\n<!-- meshfox:node -->\n\nbody\n";
        let canvas = parse(doc).unwrap();
        assert_eq!(canvas.absolute_to_local("member", 200.0, 200.0), None);
    }

    #[test]
    fn absolute_to_local_is_identity_for_a_node_not_nested_under_any_group() {
        let doc = "# Root\n\n## Section\n<!-- meshfox:node -->\n\nbody\n";
        let canvas = parse(doc).unwrap();
        assert_eq!(canvas.absolute_to_local("section", 42.0, 43.0), Some((42.0, 43.0)));
    }
}

pub(crate) fn slugify(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_dash = false;
    for c in s.chars() {
        if c.is_alphanumeric() {
            out.extend(c.to_lowercase());
            prev_dash = false;
        } else if !prev_dash && !out.is_empty() {
            out.push('-');
            prev_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    out
}
