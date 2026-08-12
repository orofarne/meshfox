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
}

/// JSON Canvas's four node types, plus meshfox's own `include` (see
/// `crate::include`). `Text` is the default and the only one with freeform
/// Markdown content — see `mdcanvas` for the `type=` attribute and
/// per-type body constraints.
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
    /// `GET /api/canvas`). Never resolved on disk: `run`/`fmt`/`validate`
    /// parse the raw file and see the bare link, same as `file`/`link`.
    Include,
    /// Body is exactly one ` ```starlark ` fence — a sandboxed contract over
    /// the document tree (see `crate::constraint`), evaluated by `meshfox
    /// check`. No Markdown prose alongside it, same "body is exactly one
    /// thing" shape as `File`/`Link`/`Include`.
    Constraint,
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
            NodeType::Constraint => "constraint",
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
    /// Markdown body between this heading (and its meshfox comments) and
    /// the next heading. For `group`, always empty. For `file`/`link`,
    /// always exactly the one Markdown link `target` was parsed from.
    pub text: String,
    /// `constraint`-node only: its most recently evaluated result (see
    /// `crate::constraint::annotate_status`). Never set by `mdcanvas::parse`
    /// itself — only by whatever consumer wants it (the server, before
    /// serving `GET /api/canvas`). `None` for a `constraint` node just means
    /// this consumer didn't evaluate it, not that it passed; `None` for
    /// every other node type always.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub constraint_status: Option<crate::constraint::ConstraintStatus>,
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
