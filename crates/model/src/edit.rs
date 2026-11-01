//! Editing a document as text, with no server and no disk: the pure half of
//! what `PATCH /api/nodes/:id` does. `meshfox view` and the in-browser editor
//! both call these, so a node edit means the same thing in both.
//!
//! What stays with the caller is everything about *who else may be editing*:
//! comparing a client's `baseRev` with the node's current text, three-way
//! merging, where the document is written, the undo log, events. By the time
//! [`apply_node_update`] runs the request is settled; it returns the new text
//! of the document, validated to parse, or says why not — and then nothing has
//! been applied (it never returns half an edit).

use serde::Deserialize;

use crate::canvas::{ArrowEnd, Canvas, EdgeLineStyle, EdgeSide, ExtraEdge, FileDisplay, NodeType};
use crate::mdcanvas::{self, NodeMeta};

/// Why an edit was not applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditError {
    /// No such node (or the document it is looked up in has none).
    NotFound(String),
    /// The edit cannot be honoured as asked: a value is not one of the
    /// field's spellings, fields conflict, or the result would not parse.
    Invalid(String),
}

impl std::fmt::Display for EditError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EditError::NotFound(m) | EditError::Invalid(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for EditError {}

fn invalid<T>(message: impl Into<String>) -> Result<T, EditError> {
    Err(EditError::Invalid(message.into()))
}

/// Tag changes for [`NodeUpdate::tags`]: `remove` first, then `add`
/// (appended in the order given, skipping tags the node already has — an
/// existing tag keeps its place). A tag in both lists ends up present.
#[derive(Debug, Default, Deserialize)]
pub struct TagOps {
    #[serde(default)]
    pub add: Vec<String>,
    #[serde(default)]
    pub remove: Vec<String>,
}

pub fn apply_tag_ops(mut tags: Vec<String>, ops: &TagOps) -> Vec<String> {
    tags.retain(|t| !ops.remove.contains(t));
    for tag in &ops.add {
        let tag = tag.trim();
        if !tag.is_empty() && !tags.iter().any(|t| t == tag) {
            tags.push(tag.to_string());
        }
    }
    tags
}

/// Edge changes for [`NodeUpdate::edges`], each entry naming the edge's
/// source node (`from`): `remove` first, then `add` (a plain edge, a no-op
/// when one from that node already exists — it never wipes the existing
/// edge's label or route), then `patch` (changes to individual fields of the
/// edge from that node, see [`EdgePatch`]; a node that has no such edge — it
/// was removed meanwhile — is skipped, not an error). An edge created with
/// properties is an `add` and a `patch` in the same request.
#[derive(Debug, Default, Deserialize)]
pub struct EdgeOps {
    #[serde(default)]
    pub add: Vec<String>,
    #[serde(default)]
    pub patch: Vec<EdgePatch>,
    #[serde(default)]
    pub remove: Vec<String>,
}

/// Changes to individual fields of one extra edge — the edge counterpart of
/// [`NodeUpdate`]'s per-field handling, with the same conventions: a field
/// that is **absent** is left alone, so two clients changing different fields
/// of the same edge both land; a field that is present is set, and cleared
/// back to unset by the same spellings a node uses — `""` for `label` and
/// `color`, `"auto"` for the sides, `"default"` for `style` and the arrow ends
/// (`"none"` there is a real value, an end with no arrowhead), `500` for
/// `labelAt` (the midpoint), `[]` for `via`.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EdgePatch {
    pub from: String,
    pub label: Option<String>,
    pub label_at: Option<u16>,
    pub color: Option<String>,
    pub style: Option<String>,
    pub arrow_start: Option<String>,
    pub arrow_end: Option<String>,
    pub source_side: Option<String>,
    pub target_side: Option<String>,
    pub via: Option<Vec<crate::canvas::RoutePoint>>,
    pub tags: Option<TagOps>,
}

fn invalid_edge_field(field: &str, value: &str, expected: &str) -> EditError {
    EditError::Invalid(format!(
        "invalid edge {field} {value:?} (expected {expected})"
    ))
}

/// Applies `patch`'s present fields onto `edge`; invalid for a value that
/// isn't one of the field's spellings, with nothing applied past that field.
pub fn apply_edge_patch(edge: &mut ExtraEdge, patch: &EdgePatch) -> Result<(), EditError> {
    if let Some(label) = &patch.label {
        edge.label = (!label.trim().is_empty()).then(|| label.clone());
    }
    if let Some(at) = patch.label_at {
        edge.label_at = match at {
            500 => None,
            0..=1000 => Some(at),
            _ => return invalid("labelAt must be between 0 and 1000"),
        };
    }
    if let Some(color) = &patch.color {
        edge.color = (!color.trim().is_empty()).then(|| color.clone());
    }
    if let Some(style) = patch.style.as_deref() {
        edge.style = match style {
            "default" => None,
            other => Some(EdgeLineStyle::parse(other).ok_or_else(|| {
                invalid_edge_field("style", other, "solid, dashed, dotted or default")
            })?),
        };
    }
    for (field, value, slot) in [
        (
            "arrowStart",
            patch.arrow_start.as_deref(),
            &mut edge.arrow_start,
        ),
        ("arrowEnd", patch.arrow_end.as_deref(), &mut edge.arrow_end),
    ] {
        if let Some(value) = value {
            *slot =
                match value {
                    "default" => None,
                    other => Some(ArrowEnd::parse(other).ok_or_else(|| {
                        invalid_edge_field(field, other, "none, arrow or default")
                    })?),
                };
        }
    }
    for (field, value, slot) in [
        (
            "sourceSide",
            patch.source_side.as_deref(),
            &mut edge.source_side,
        ),
        (
            "targetSide",
            patch.target_side.as_deref(),
            &mut edge.target_side,
        ),
    ] {
        if let Some(value) = value {
            *slot = match value {
                "auto" => None,
                other => Some(EdgeSide::parse(other).ok_or_else(|| {
                    invalid_edge_field(field, other, "left, right, top, bottom or auto")
                })?),
            };
        }
    }
    if let Some(via) = &patch.via {
        edge.via = via.clone();
    }
    if let Some(ops) = &patch.tags {
        edge.tags = apply_tag_ops(std::mem::take(&mut edge.tags), ops);
    }
    Ok(())
}

/// `raw`'s string sentinel (see [`NodeUpdate::fold`]) resolved against
/// `existing` (the node's current value, kept when nothing was sent) into the
/// `Option<bool>` `Node::fold` itself wants. Invalid for anything other than
/// `"true"`/`"false"`/`"default"`.
pub fn resolve_fold_override(
    raw: Option<&str>,
    existing: Option<bool>,
) -> Result<Option<bool>, EditError> {
    match raw {
        None => Ok(existing),
        Some(s) => mdcanvas::parse_fold_override(s).map_err(EditError::Invalid),
    }
}

/// Everything `PATCH /api/nodes/:id` can change about one node. A field that is
/// absent is left alone; the request is applied all or nothing.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeUpdate {
    pub title: Option<String>,
    pub node_type: Option<NodeType>,
    pub color: Option<String>,
    /// New link target for a `file`/`link` node — written as the node's
    /// whole body (`[title](target)`), replacing whatever body was there.
    pub target: Option<String>,
    /// New raw Markdown body for a `text` node — what the web UI's editor
    /// sends. Mutually exclusive with `target` in practice (the client only
    /// ever sends one, depending on node type), but nothing here enforces that
    /// beyond whichever one is applied last winning.
    pub text: Option<String>,
    /// The `bodyRev` of the node as the client last saw it — **required
    /// whenever `text` is sent**, and ignored otherwise. Checked by the
    /// caller, not by [`apply_node_update`].
    pub base_rev: Option<String>,
    /// Optional original title for atomic editor saves (the caller's).
    pub base_title: Option<String>,
    /// With `text`: when `baseRev` (or `baseTitle`) is stale, ask the caller to
    /// three-way merge rather than reject (the caller's).
    pub merge: Option<bool>,
    /// Changes to the node's extra incoming edges (`meshfox:edge
    /// from="..."`), applied to the edges it has right now — never a
    /// replacement list, for the same reason as `tags`. `None` leaves them
    /// untouched.
    pub edges: Option<EdgeOps>,
    /// `file`-node display mode — `None` leaves the existing value untouched.
    pub display: Option<FileDisplay>,
    /// `file`-node syntax-highlighting language hint.
    pub lang: Option<String>,
    /// `file`-node interpreter — makes the node runnable.
    pub interpreter: Option<String>,
    /// `link`-node social preview toggle.
    pub preview: Option<bool>,
    /// Changes to the node's tags, applied to the tags it has right now —
    /// never a replacement list, so two clients adding different tags at once
    /// both land. `None` leaves them untouched.
    pub tags: Option<TagOps>,
    /// Structural-edge label — the text shown on the implicit edge from this
    /// node's parent into it. `None` (key not sent) leaves it untouched;
    /// `Some("")` (sent, empty) clears it back to unset rather than writing a
    /// literal `edgeLabel=""`.
    pub edge_label: Option<String>,
    /// 0..=1000 along the rendered path; 500 clears back to the midpoint.
    pub edge_label_at: Option<u16>,
    /// "auto" clears an explicitly selected structural-edge side.
    pub edge_source_side: Option<String>,
    pub edge_target_side: Option<String>,
    pub edge_via: Option<Vec<crate::canvas::RoutePoint>>,
    /// Per-node fold-state override. A string sentinel, since `null` is
    /// indistinguishable from an absent field to `serde`'s usual `Option<T>`:
    /// `"true"`/`"false"` set an explicit override, `"default"` clears it
    /// (see [`resolve_fold_override`]).
    pub fold: Option<String>,
    /// Absolute position/size — `None` leaves each untouched. `width`/`height`
    /// are rejected for a group node (real or becoming one via `nodeType` in
    /// the same request): its box is always derived from its children.
    pub x: Option<f64>,
    pub y: Option<f64>,
    pub width: Option<f64>,
    pub height: Option<f64>,
    #[serde(default)]
    pub clear_position: bool,
    /// `None` leaves it untouched; an explicit RFC3339 string replaces it.
    pub created_at: Option<String>,
}

impl NodeUpdate {
    /// A request that changes nothing.
    pub fn blank() -> Self {
        NodeUpdate {
            title: None,
            node_type: None,
            color: None,
            target: None,
            text: None,
            base_rev: None,
            base_title: None,
            merge: None,
            edges: None,
            display: None,
            lang: None,
            interpreter: None,
            preview: None,
            tags: None,
            edge_label: None,
            edge_label_at: None,
            edge_source_side: None,
            edge_target_side: None,
            edge_via: None,
            fold: None,
            x: None,
            y: None,
            width: None,
            height: None,
            clear_position: false,
            created_at: None,
        }
    }
}

/// A body edit made against a version of the node that is no longer the
/// current one: what the node is now, for the caller to show, merge against, or
/// refuse with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StaleBase {
    /// The revision the client says it read.
    pub base_rev: String,
    pub current_text: String,
    pub current_rev: String,
    pub current_title: String,
    /// The client's `baseTitle` differs from the node's current title.
    pub title_stale: bool,
}

/// Whether `req`, which replaces a node's body, was made against the node as it
/// is now. `Ok(None)`: it was, or `req` does not replace a body. `baseRev` is
/// **required** whenever `text` is sent — the check cannot be skipped — and
/// its absence is an [`EditError::Invalid`]. What to do about a stale base (a
/// conflict answer, or a merge first) is the caller's.
pub fn check_base(
    raw: &str,
    local_id: &str,
    req: &NodeUpdate,
) -> Result<Option<StaleBase>, EditError> {
    if req.text.is_none() {
        return Ok(None);
    }
    let Some(base_rev) = req.base_rev.as_deref() else {
        return invalid(
            "baseRev is required when replacing a node's body — read the node first and \
             send back its bodyRev",
        );
    };
    let canvas = Canvas::from_markdown(raw).map_err(|e| EditError::Invalid(e.to_string()))?;
    let node = canvas
        .node(local_id)
        .ok_or_else(|| EditError::NotFound(format!("no node {local_id:?}")))?;
    let current_rev = crate::body_rev::body_rev(&node.text);
    let title_stale = req
        .base_title
        .as_ref()
        .is_some_and(|title| title != &node.title);
    if base_rev == current_rev && !title_stale {
        return Ok(None);
    }
    Ok(Some(StaleBase {
        base_rev: base_rev.to_string(),
        current_text: node.text.clone(),
        current_rev,
        current_title: node.title.clone(),
        title_stale,
    }))
}

/// Applies `req` to node `local_id` of the document `raw` and returns the new
/// document text. The patched document is validated to parse before it is
/// returned — an invalid combination (e.g. `nodeType: group` with a non-empty
/// body, or a `link` with no link in its body) is an [`EditError::Invalid`] and
/// nothing is applied, rather than a partial edit.
///
/// `resolve_from` turns the id a request names an edge's source by into the id
/// it is written under in this document (the identity, when every id is
/// local); adding an edge from a node that doesn't exist is the error it
/// returns, while removing or patching one whose source is already gone just
/// matches the id as given.
pub fn apply_node_update(
    raw: &str,
    local_id: &str,
    req: &NodeUpdate,
    resolve_from: &dyn Fn(&str) -> Result<String, EditError>,
) -> Result<String, EditError> {
    struct EdgeOpsLocal {
        add: Vec<String>,
        patch: Vec<EdgePatch>,
        remove: Vec<String>,
    }
    let edge_ops_local = req
        .edges
        .as_ref()
        .map(|ops| -> Result<EdgeOpsLocal, EditError> {
            // Every patch is checked now, against a scratch edge, so a bad
            // value is an error whether or not the edge it names still exists.
            for p in &ops.patch {
                apply_edge_patch(&mut ExtraEdge::default(), p)?;
            }
            let patch = ops
                .patch
                .iter()
                .map(|p| EdgePatch {
                    // A source that no longer exists has no edge to patch; its
                    // id then simply matches nothing below.
                    from: resolve_from(&p.from).unwrap_or_else(|_| p.from.clone()),
                    label: p.label.clone(),
                    label_at: p.label_at,
                    color: p.color.clone(),
                    style: p.style.clone(),
                    arrow_start: p.arrow_start.clone(),
                    arrow_end: p.arrow_end.clone(),
                    source_side: p.source_side.clone(),
                    target_side: p.target_side.clone(),
                    via: p.via.clone(),
                    tags: p.tags.as_ref().map(|t| TagOps {
                        add: t.add.clone(),
                        remove: t.remove.clone(),
                    }),
                })
                .collect();
            Ok(EdgeOpsLocal {
                add: ops
                    .add
                    .iter()
                    .map(|f| resolve_from(f))
                    .collect::<Result<_, _>>()?,
                patch,
                remove: ops
                    .remove
                    .iter()
                    .map(|f| resolve_from(f).unwrap_or_else(|_| f.clone()))
                    .collect(),
            })
        })
        .transpose()?;

    let not_found = || EditError::NotFound(format!("no node {local_id:?}"));
    let parse =
        |text: &str| Canvas::from_markdown(text).map_err(|e| EditError::Invalid(e.to_string()));
    let mut raw = raw.to_string();

    // Read the node's current fields once, up front — every mutation below is
    // a surgical splice that doesn't require (and, for a type change that
    // hasn't had its body fixed up yet, must NOT require) the document to
    // still parse cleanly in between. Only the fully-patched result is
    // validated, at the very end: re-parsing after e.g. the type change below
    // would spuriously reject a still-in-progress edit — switching a node to
    // `file`/`link` and supplying its target in the same request — since the
    // type would already say `file`/`link` while the body briefly still isn't
    // a single link.
    let initial = parse(&raw)?;
    let initial_node = initial.node(local_id).ok_or_else(not_found)?;
    // A plain-Markdown `include` node keeps its own id post-resolve, so it is
    // found right here in the primary document — but its *body*, as the client
    // saw it, is the include target's own (shifted-headings) content, not what
    // is stored here (a bare `[label](target)` link). Writing that back as
    // `text` would silently try to overwrite the link with the target's whole
    // content — reject it with a clear reason up front.
    if initial_node.node_type == NodeType::Include && req.text.is_some() {
        return invalid(format!(
            "node {local_id:?} is a plain-Markdown include — its shown body comes from the include \
             target file, not from here; open that file directly to edit it (use `target` \
             here to change which file it links to instead)"
        ));
    }
    let (
        x,
        y,
        width,
        height,
        existing_color,
        existing_display,
        existing_lang,
        existing_interpreter,
        existing_preview,
        existing_tags,
        existing_fold,
        existing_edge_label,
        existing_created_at,
    ) = (
        initial_node.x,
        initial_node.y,
        initial_node.width,
        initial_node.height,
        initial_node.color.clone(),
        initial_node.display,
        initial_node.lang.clone(),
        initial_node.interpreter.clone(),
        initial_node.preview,
        initial_node.tags.clone(),
        initial_node.fold,
        initial_node.edge_label.clone(),
        initial_node.created_at.clone(),
    );
    // `display`/`lang`/`interpreter` only mean anything on a `file` node —
    // clear them (rather than leave a stale attribute behind) whenever this
    // request moves the node to some other type. `preview` is the same idea
    // but for `link` nodes.
    let final_type = req.node_type.unwrap_or(initial_node.node_type);
    let mut title = initial_node.title.clone();

    if let Some(new_title) = &req.title {
        raw = mdcanvas::try_set_node_title(&raw, local_id, new_title).map_err(|e| match e {
            mdcanvas::SetTitleError::NotFound(_) => not_found(),
            // The node exists; the request itself can't be honoured safely.
            mdcanvas::SetTitleError::DerivedId(_) => EditError::Invalid(e.to_string()),
        })?;
        title = new_title.clone();
    }

    // A group's box is always derived from its children, never stored — reject
    // an explicit width/height for one outright (whether it already is a group,
    // or is becoming one via `nodeType` in this same request) rather than
    // silently writing (and immediately ignoring) a size nothing will ever
    // read back.
    if final_type == NodeType::Group && (req.width.is_some() || req.height.is_some()) {
        return invalid(
            "group nodes never store a size — their box is always derived from their children",
        );
    }
    if req.clear_position
        && (req.x.is_some() || req.y.is_some() || req.width.is_some() || req.height.is_some())
    {
        return invalid("clearPosition cannot be combined with x/y/width/height");
    }
    if let Some(created_at) = &req.created_at {
        if !crate::timestamp::is_valid_rfc3339(created_at) {
            return invalid(format!(
                "invalid createdAt {created_at:?} — expected RFC3339"
            ));
        }
    }

    if req.title.is_some()
        || req.node_type.is_some()
        || req.color.is_some()
        || req.display.is_some()
        || req.lang.is_some()
        || req.interpreter.is_some()
        || req.preview.is_some()
        || req.tags.is_some()
        || req.fold.is_some()
        || req.edge_label.is_some()
        || req.edge_label_at.is_some()
        || req.edge_source_side.is_some()
        || req.edge_target_side.is_some()
        || req.edge_via.is_some()
        || req.x.is_some()
        || req.y.is_some()
        || req.width.is_some()
        || req.height.is_some()
        || req.clear_position
        || req.created_at.is_some()
    {
        // This also has the side effect of pinning the node's `id=` attribute
        // explicitly the moment any of its metadata changes, same as any other
        // first write-back.
        let (display, lang, interpreter) = if final_type == NodeType::File {
            (
                req.display.or(existing_display),
                req.lang.clone().or(existing_lang),
                req.interpreter.clone().or(existing_interpreter),
            )
        } else {
            (None, None, None)
        };
        let preview = if final_type == NodeType::Link {
            Some(req.preview.unwrap_or(existing_preview))
        } else {
            None
        };
        // Unlike `color` (which would happily store and write back a literal
        // `color=""` if sent empty), an empty `edgeLabel` clears the attribute
        // entirely rather than leaving that cruft behind — the client always
        // sends this key explicitly (never omitted) whenever the label
        // actually changed, including changing it *to* empty, so there's no
        // "not sent at all" case to conflate this with.
        let edge_label = match &req.edge_label {
            None => existing_edge_label,
            Some(s) if s.trim().is_empty() => None,
            Some(s) => Some(s.clone()),
        };
        // Groups never store a width/height regardless of what's in `x`/`y`
        // here (already rejected above if the request tried to set one) —
        // carrying `existing` width/height forward for a group would just
        // re-write whatever stray value was already there instead of actually
        // clearing it, so force both to `None` for one.
        let (final_width, final_height) = if final_type == NodeType::Group {
            (None, None)
        } else {
            (
                if req.clear_position {
                    req.width
                } else {
                    req.width.or(width)
                },
                if req.clear_position {
                    req.height
                } else {
                    req.height.or(height)
                },
            )
        };
        let meta = NodeMeta {
            block_attrs: None,
            x: if req.clear_position {
                req.x
            } else {
                req.x.or(x)
            },
            y: if req.clear_position {
                req.y
            } else {
                req.y.or(y)
            },
            width: final_width,
            height: final_height,
            color: req.color.clone().or(existing_color),
            node_type: req.node_type,
            display,
            lang,
            interpreter,
            preview,
            edge_label,
            edge_label_at: match req.edge_label_at {
                None => initial_node.edge_label_at,
                Some(500) => None,
                Some(at @ 0..=1000) => Some(at),
                Some(_) => return invalid("edgeLabelAt must be between 0 and 1000"),
            },
            edge_source_side: match req.edge_source_side.as_deref() {
                None => initial_node.edge_source_side,
                Some("auto") => None,
                Some(side) => Some(EdgeSide::parse(side).ok_or_else(|| {
                    EditError::Invalid(format!("invalid edgeSourceSide {side:?}"))
                })?),
            },
            edge_target_side: match req.edge_target_side.as_deref() {
                None => initial_node.edge_target_side,
                Some("auto") => None,
                Some(side) => Some(EdgeSide::parse(side).ok_or_else(|| {
                    EditError::Invalid(format!("invalid edgeTargetSide {side:?}"))
                })?),
            },
            edge_via: req
                .edge_via
                .clone()
                .unwrap_or_else(|| initial_node.edge_via.clone()),
            fold: resolve_fold_override(req.fold.as_deref(), existing_fold)?,
            tags: match &req.tags {
                Some(ops) => apply_tag_ops(existing_tags, ops),
                None => existing_tags,
            },
            created_at: req.created_at.clone().or(existing_created_at),
        };
        raw = mdcanvas::set_node_meta(&raw, local_id, &meta).ok_or_else(not_found)?;
    }

    if let Some(target) = &req.target {
        // Preserves an existing caption rather than silently dropping it —
        // this patch only ever means "change the link", never "clear whatever
        // explanatory text was under it".
        let body = match &initial_node.caption {
            Some(caption) => format!("[{title}]({target})\n\n{caption}"),
            None => format!("[{title}]({target})"),
        };
        raw = mdcanvas::set_node_body(&raw, local_id, &body).ok_or_else(not_found)?;
    }

    if let Some(text) = &req.text {
        raw = mdcanvas::set_node_body(&raw, local_id, text).ok_or_else(not_found)?;
    }

    if let Some(ops) = &edge_ops_local {
        let mut edges = initial_node.extra_parents.clone();
        edges.retain(|e| !ops.remove.contains(&e.from));
        for from in &ops.add {
            if !edges.iter().any(|e| &e.from == from) {
                edges.push(ExtraEdge::new(from.as_str()));
            }
        }
        for patch in &ops.patch {
            if let Some(edge) = edges.iter_mut().find(|e| e.from == patch.from) {
                apply_edge_patch(edge, patch)?;
            }
        }
        raw = mdcanvas::set_node_edges(&raw, local_id, &edges).ok_or_else(not_found)?;
    }

    // Validate the whole patched document before handing it back.
    parse(&raw)?;
    Ok(raw)
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOC: &str = concat!(
        "# Root\n<!-- meshfox:node id=\"root\" -->\n\nThe root.\n\n",
        "## Child\n<!-- meshfox:node id=\"child\" tags=\"a\" -->\n\nbody\n\n",
        "## Other\n<!-- meshfox:node id=\"other\" -->\n\nelse\n\n",
        "## Link\n<!-- meshfox:node id=\"link\" type=\"link\" -->\n\n[Link](https://a.example)\n\nwhy\n",
    );

    fn identity(from: &str) -> Result<String, EditError> {
        Ok(from.to_string())
    }

    fn apply(raw: &str, id: &str, req: NodeUpdate) -> Result<String, EditError> {
        apply_node_update(raw, id, &req, &identity)
    }

    fn node(raw: &str, id: &str) -> crate::canvas::Node {
        Canvas::from_markdown(raw)
            .unwrap()
            .node(id)
            .unwrap()
            .clone()
    }

    fn err(r: Result<String, EditError>) -> EditError {
        r.expect_err("expected the edit to be refused")
    }

    #[test]
    fn a_body_edit_needs_a_base_and_a_stale_one_is_reported_with_the_current_node() {
        let current_rev = crate::body_rev::body_rev(&node(DOC, "child").text);
        let req = |base: Option<&str>, base_title: Option<&str>| NodeUpdate {
            text: Some("new".into()),
            base_rev: base.map(str::to_string),
            base_title: base_title.map(str::to_string),
            ..NodeUpdate::blank()
        };
        // No `text`: nothing to check.
        assert_eq!(check_base(DOC, "child", &NodeUpdate::blank()), Ok(None));
        // `text` without a base is refused outright.
        assert!(matches!(
            check_base(DOC, "child", &req(None, None)),
            Err(EditError::Invalid(_))
        ));
        // The current base passes.
        assert_eq!(
            check_base(DOC, "child", &req(Some(&current_rev), Some("Child"))),
            Ok(None)
        );
        // A stale body, or a stale title, is reported.
        let stale = check_base(DOC, "child", &req(Some("0000"), None))
            .unwrap()
            .unwrap();
        assert_eq!(
            (stale.current_title.as_str(), stale.title_stale),
            ("Child", false)
        );
        assert_eq!(stale.current_rev, current_rev);
        assert_eq!(stale.current_text.trim(), "body");
        let stale = check_base(DOC, "child", &req(Some(&current_rev), Some("Old title")))
            .unwrap()
            .unwrap();
        assert!(stale.title_stale);
        assert!(matches!(
            check_base(DOC, "nope", &req(Some("x"), None)),
            Err(EditError::NotFound(_))
        ));
    }

    #[test]
    fn a_blank_request_changes_nothing_but_still_validates() {
        assert_eq!(apply(DOC, "child", NodeUpdate::blank()).unwrap(), DOC);
        assert!(matches!(
            err(apply(DOC, "nope", NodeUpdate::blank())),
            EditError::NotFound(_)
        ));
        assert!(matches!(
            err(apply("no heading\n", "x", NodeUpdate::blank())),
            EditError::Invalid(_)
        ));
    }

    #[test]
    fn the_body_title_and_color_change_and_the_rest_is_left_alone() {
        let out = apply(
            DOC,
            "child",
            NodeUpdate {
                text: Some("new body".into()),
                title: Some("Renamed".into()),
                color: Some("3".into()),
                ..NodeUpdate::blank()
            },
        )
        .unwrap();
        let child = node(&out, "child");
        assert_eq!(child.text.trim(), "new body");
        assert_eq!(child.title, "Renamed");
        assert_eq!(child.color.as_deref(), Some("3"));
        assert_eq!(child.tags, ["a"], "tags the request did not mention stay");
        assert_eq!(node(&out, "other").text.trim(), "else");
    }

    #[test]
    fn tags_are_added_and_removed_never_replaced() {
        let out = apply(
            DOC,
            "child",
            NodeUpdate {
                tags: Some(TagOps {
                    add: vec!["b".into(), "a".into(), "  ".into()],
                    remove: vec![],
                }),
                ..NodeUpdate::blank()
            },
        )
        .unwrap();
        assert_eq!(node(&out, "child").tags, ["a", "b"]);
        let out = apply(
            &out,
            "child",
            NodeUpdate {
                tags: Some(TagOps {
                    add: vec![],
                    remove: vec!["a".into()],
                }),
                ..NodeUpdate::blank()
            },
        )
        .unwrap();
        assert_eq!(node(&out, "child").tags, ["b"]);
    }

    #[test]
    fn position_and_size_are_set_and_cleared() {
        let out = apply(
            DOC,
            "child",
            NodeUpdate {
                x: Some(10.0),
                y: Some(20.0),
                width: Some(300.0),
                height: Some(120.0),
                ..NodeUpdate::blank()
            },
        )
        .unwrap();
        let child = node(&out, "child");
        assert_eq!(
            (child.x, child.y, child.width, child.height),
            (Some(10.0), Some(20.0), Some(300.0), Some(120.0))
        );
        let out = apply(
            &out,
            "child",
            NodeUpdate {
                clear_position: true,
                ..NodeUpdate::blank()
            },
        )
        .unwrap();
        let child = node(&out, "child");
        assert_eq!(
            (child.x, child.y, child.width, child.height),
            (None, None, None, None)
        );
    }

    #[test]
    fn conflicting_or_malformed_requests_are_refused_and_apply_nothing() {
        let blank = NodeUpdate::blank;
        for (what, req) in [
            (
                "clearPosition with x",
                NodeUpdate {
                    clear_position: true,
                    x: Some(1.0),
                    ..blank()
                },
            ),
            (
                "a bad createdAt",
                NodeUpdate {
                    created_at: Some("yesterday".into()),
                    ..blank()
                },
            ),
            (
                "a bad edgeLabelAt",
                NodeUpdate {
                    edge_label_at: Some(2000),
                    ..blank()
                },
            ),
            (
                "a bad edge side",
                NodeUpdate {
                    edge_source_side: Some("middle".into()),
                    ..blank()
                },
            ),
            (
                "a bad fold word",
                NodeUpdate {
                    fold: Some("bogus".into()),
                    ..blank()
                },
            ),
            (
                "a size for a group",
                NodeUpdate {
                    node_type: Some(NodeType::Group),
                    width: Some(10.0),
                    ..blank()
                },
            ),
            (
                "a group that still has a body",
                NodeUpdate {
                    node_type: Some(NodeType::Group),
                    ..blank()
                },
            ),
        ] {
            assert!(
                matches!(apply(DOC, "child", req), Err(EditError::Invalid(_))),
                "{what}"
            );
        }
    }

    #[test]
    fn a_link_keeps_its_caption_when_only_the_target_changes() {
        let out = apply(
            DOC,
            "link",
            NodeUpdate {
                target: Some("https://b.example".into()),
                ..NodeUpdate::blank()
            },
        )
        .unwrap();
        let link = node(&out, "link");
        assert_eq!(link.target.as_deref(), Some("https://b.example"));
        assert_eq!(link.caption.as_deref(), Some("why"));
    }

    #[test]
    fn switching_a_node_to_a_link_and_giving_its_target_in_one_request_works() {
        // The node's body is not a link yet when its type changes; only the
        // finished document has to parse.
        let out = apply(
            DOC,
            "other",
            NodeUpdate {
                node_type: Some(NodeType::Link),
                target: Some("https://c.example".into()),
                ..NodeUpdate::blank()
            },
        )
        .unwrap();
        let other = node(&out, "other");
        assert_eq!(other.node_type, NodeType::Link);
        assert_eq!(other.target.as_deref(), Some("https://c.example"));
    }

    #[test]
    fn an_include_node_refuses_a_text_edit_but_takes_a_new_target() {
        let doc = "# Root\n<!-- meshfox:node id=\"root\" -->\n\n## Inc\n<!-- meshfox:node id=\"inc\" type=\"include\" -->\n\n[Inc](./a.md)\n";
        let refused = err(apply(
            doc,
            "inc",
            NodeUpdate {
                text: Some("# whatever".into()),
                ..NodeUpdate::blank()
            },
        ));
        assert!(refused.to_string().contains("plain-Markdown include"));
        let out = apply(
            doc,
            "inc",
            NodeUpdate {
                target: Some("./b.md".into()),
                ..NodeUpdate::blank()
            },
        )
        .unwrap();
        assert_eq!(node(&out, "inc").target.as_deref(), Some("./b.md"));
    }

    #[test]
    fn edges_are_added_patched_and_removed_by_their_source() {
        let out = apply(
            DOC,
            "other",
            NodeUpdate {
                edges: Some(EdgeOps {
                    add: vec!["child".into()],
                    patch: vec![EdgePatch {
                        from: "child".into(),
                        label: Some("depends".into()),
                        style: Some("dashed".into()),
                        ..EdgePatch::default()
                    }],
                    remove: vec![],
                }),
                ..NodeUpdate::blank()
            },
        )
        .unwrap();
        let edge = node(&out, "other").extra_parents[0].clone();
        assert_eq!(edge.from, "child");
        assert_eq!(edge.label.as_deref(), Some("depends"));
        assert_eq!(edge.style, Some(EdgeLineStyle::Dashed));

        let out = apply(
            &out,
            "other",
            NodeUpdate {
                edges: Some(EdgeOps {
                    remove: vec!["child".into()],
                    ..EdgeOps::default()
                }),
                ..NodeUpdate::blank()
            },
        )
        .unwrap();
        assert!(node(&out, "other").extra_parents.is_empty());
    }

    #[test]
    fn a_bad_edge_value_is_refused_even_when_that_edge_does_not_exist() {
        let req = NodeUpdate {
            edges: Some(EdgeOps {
                patch: vec![EdgePatch {
                    from: "ghost".into(),
                    style: Some("wavy".into()),
                    ..EdgePatch::default()
                }],
                ..EdgeOps::default()
            }),
            ..NodeUpdate::blank()
        };
        assert!(err(apply(DOC, "other", req)).to_string().contains("style"));
    }

    #[test]
    fn adding_an_edge_from_a_node_the_resolver_cannot_find_is_not_found() {
        let req = NodeUpdate {
            edges: Some(EdgeOps {
                add: vec!["ghost".into()],
                ..EdgeOps::default()
            }),
            ..NodeUpdate::blank()
        };
        let resolve = |from: &str| -> Result<String, EditError> {
            if from == "ghost" {
                Err(EditError::NotFound("no node \"ghost\"".into()))
            } else {
                Ok(from.to_string())
            }
        };
        assert!(matches!(
            apply_node_update(DOC, "other", &req, &resolve),
            Err(EditError::NotFound(_))
        ));
    }

    #[test]
    fn the_fold_sentinel_is_set_kept_and_cleared() {
        let fold = |raw: Option<&str>, existing| resolve_fold_override(raw, existing).unwrap();
        assert_eq!(fold(None, Some(true)), Some(true));
        assert_eq!(fold(None, None), None);
        assert_eq!(fold(Some("true"), None), Some(true));
        assert_eq!(fold(Some("false"), Some(true)), Some(false));
        assert_eq!(fold(Some("default"), Some(true)), None);
        assert!(resolve_fold_override(Some("bogus"), None).is_err());
    }
}
