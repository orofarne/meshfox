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

impl BlockAttrsUpdate {
    /// A request that changes nothing.
    pub fn blank() -> Self {
        BlockAttrsUpdate {
            name: None,
            lang: None,
            cache: None,
            always: None,
            default: None,
            tty: None,
            autoclose: None,
            service: None,
            deps: None,
            env: None,
            interpreter: None,
            clear_interpreter: false,
            code: None,
        }
    }
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
    let edge_ops = resolve_edge_ops(req, resolve_from)?;

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

    check_update_shape(req, final_type)?;

    if changes_metadata(req) {
        // This also has the side effect of pinning the node's `id=` attribute
        // explicitly the moment any of its metadata changes, same as any other
        // first write-back.
        let meta = patched_meta(req, initial_node, final_type)?;
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

    if let Some(ops) = &edge_ops {
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

/// A request's edge operations, with every source named by the id it is
/// written under in this document.
struct ResolvedEdgeOps {
    add: Vec<String>,
    patch: Vec<EdgePatch>,
    remove: Vec<String>,
}

fn resolve_edge_ops(
    req: &NodeUpdate,
    resolve_from: &dyn Fn(&str) -> Result<String, EditError>,
) -> Result<Option<ResolvedEdgeOps>, EditError> {
    let Some(ops) = req.edges.as_ref() else {
        return Ok(None);
    };
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
    Ok(Some(ResolvedEdgeOps {
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
    }))
}

/// What a request may not combine, whatever the document holds.
fn check_update_shape(req: &NodeUpdate, final_type: NodeType) -> Result<(), EditError> {
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
    Ok(())
}

/// Whether the request changes anything stored in the node's metadata comment.
fn changes_metadata(req: &NodeUpdate) -> bool {
    req.clear_position
        || [
            req.title.is_some(),
            req.node_type.is_some(),
            req.color.is_some(),
            req.display.is_some(),
            req.lang.is_some(),
            req.interpreter.is_some(),
            req.preview.is_some(),
            req.tags.is_some(),
            req.fold.is_some(),
            req.edge_label.is_some(),
            req.edge_label_at.is_some(),
            req.edge_source_side.is_some(),
            req.edge_target_side.is_some(),
            req.edge_via.is_some(),
            req.x.is_some(),
            req.y.is_some(),
            req.width.is_some(),
            req.height.is_some(),
            req.created_at.is_some(),
        ]
        .contains(&true)
}

/// A value the request sets, or else the one the node has; with
/// `clearPosition` the request's value is taken even when it sets none.
fn carry<T>(requested: Option<T>, existing: Option<T>, clear: bool) -> Option<T> {
    if clear {
        requested
    } else {
        requested.or(existing)
    }
}

/// `auto` clears an edge side; anything else must name one.
fn edge_side(
    requested: Option<&str>,
    current: Option<EdgeSide>,
    field: &str,
) -> Result<Option<EdgeSide>, EditError> {
    match requested {
        None => Ok(current),
        Some("auto") => Ok(None),
        Some(side) => EdgeSide::parse(side)
            .map(Some)
            .ok_or_else(|| EditError::Invalid(format!("invalid {field} {side:?}"))),
    }
}

/// The middle of the arrow is the default, so it is stored as nothing.
fn edge_label_position(
    requested: Option<u16>,
    current: Option<u16>,
) -> Result<Option<u16>, EditError> {
    match requested {
        None => Ok(current),
        Some(500) => Ok(None),
        Some(at @ 0..=1000) => Ok(Some(at)),
        Some(_) => invalid("edgeLabelAt must be between 0 and 1000"),
    }
}

/// The node's metadata as the request leaves it.
fn patched_meta(
    req: &NodeUpdate,
    node: &crate::canvas::Node,
    final_type: NodeType,
) -> Result<NodeMeta, EditError> {
    let (display, lang, interpreter) = if final_type == NodeType::File {
        (
            req.display.or(node.display),
            req.lang.clone().or_else(|| node.lang.clone()),
            req.interpreter.clone().or_else(|| node.interpreter.clone()),
        )
    } else {
        (None, None, None)
    };
    let preview = (final_type == NodeType::Link).then(|| req.preview.unwrap_or(node.preview));
    // Unlike `color` (which would happily store and write back a literal
    // `color=""` if sent empty), an empty `edgeLabel` clears the attribute
    // entirely rather than leaving that cruft behind — the client always
    // sends this key explicitly (never omitted) whenever the label
    // actually changed, including changing it *to* empty, so there's no
    // "not sent at all" case to conflate this with.
    let edge_label = match &req.edge_label {
        None => node.edge_label.clone(),
        Some(s) if s.trim().is_empty() => None,
        Some(s) => Some(s.clone()),
    };
    // Groups never store a width/height regardless of what's in `x`/`y`
    // here (already rejected above if the request tried to set one) —
    // carrying `existing` width/height forward for a group would just
    // re-write whatever stray value was already there instead of actually
    // clearing it, so force both to `None` for one.
    let (width, height) = if final_type == NodeType::Group {
        (None, None)
    } else {
        (
            carry(req.width, node.width, req.clear_position),
            carry(req.height, node.height, req.clear_position),
        )
    };
    Ok(NodeMeta {
        block_attrs: None,
        x: carry(req.x, node.x, req.clear_position),
        y: carry(req.y, node.y, req.clear_position),
        width,
        height,
        color: req.color.clone().or_else(|| node.color.clone()),
        node_type: req.node_type,
        display,
        lang,
        interpreter,
        preview,
        edge_label,
        edge_label_at: edge_label_position(req.edge_label_at, node.edge_label_at)?,
        edge_source_side: edge_side(
            req.edge_source_side.as_deref(),
            node.edge_source_side,
            "edgeSourceSide",
        )?,
        edge_target_side: edge_side(
            req.edge_target_side.as_deref(),
            node.edge_target_side,
            "edgeTargetSide",
        )?,
        edge_via: req
            .edge_via
            .clone()
            .unwrap_or_else(|| node.edge_via.clone()),
        fold: resolve_fold_override(req.fold.as_deref(), node.fold)?,
        tags: match &req.tags {
            Some(ops) => apply_tag_ops(node.tags.clone(), ops),
            None => node.tags.clone(),
        },
        created_at: req.created_at.clone().or_else(|| node.created_at.clone()),
    })
}

// ---- the rest of the document-editing endpoints: each one validates, calls a
// ---- patcher from `mdcanvas`, and checks the result still parses.

use std::collections::HashMap;

fn parse_checked(text: &str) -> Result<Canvas, EditError> {
    Canvas::from_markdown(text).map_err(|e| EditError::Invalid(e.to_string()))
}

fn no_node(id: &str) -> EditError {
    EditError::NotFound(format!("no node {id:?}"))
}

/// Adds a new, empty-bodied child heading node under `parent_id`, as the last
/// item of its subtree, with `body` as its text when given (a body replacement
/// of an *existing* node needs a base revision; a node that doesn't exist yet
/// has nothing to be stale against). With `title_slug_id` the id is a readable
/// slug of the title, otherwise a random one. Deliberately doesn't set a
/// position — the client's auto-layout places it. Returns the new document and
/// the new node's id.
pub fn create_node(
    raw: &str,
    parent_id: &str,
    title: &str,
    title_slug_id: bool,
    body: Option<&str>,
) -> Result<(String, String), EditError> {
    let insert = if title_slug_id {
        mdcanvas::insert_child_node
    } else {
        mdcanvas::insert_child_node_random_id
    };
    let (mut updated, new_id) = insert(raw, parent_id, title).ok_or_else(|| no_node(parent_id))?;
    if let Some(body) = body {
        updated = mdcanvas::set_node_body(&updated, &new_id, body).ok_or_else(|| {
            EditError::Invalid(format!("failed to set the body of the new node {new_id:?}"))
        })?;
    }
    parse_checked(&updated)?;
    Ok((updated, new_id))
}

/// Deletes node `id` — its whole subtree, or with `reparent_children` just
/// itself, promoting its direct children to its own parent. The root is
/// refused rather than producing a rootless document.
pub fn remove_node(raw: &str, id: &str, reparent_children: bool) -> Result<String, EditError> {
    let canvas = parse_checked(raw)?;
    let node = canvas.node(id).ok_or_else(|| no_node(id))?;
    if node.parent.is_none() {
        return invalid("can't delete the root node");
    }
    let updated = if reparent_children {
        mdcanvas::delete_node_reparent_children(raw, id)
    } else {
        mdcanvas::delete_node(raw, id)
    }
    .ok_or_else(|| no_node(id))?;
    parse_checked(&updated)?;
    Ok(updated)
}

/// The `NodeMeta` that rewrites `node`'s `meshfox:node` line with its position
/// and size cleared and everything else carried over.
fn layout_cleared_meta(node: &crate::canvas::Node) -> NodeMeta {
    NodeMeta {
        block_attrs: None,
        x: None,
        y: None,
        width: None,
        height: None,
        color: node.color.clone(),
        node_type: None,
        display: node.display,
        lang: node.lang.clone(),
        interpreter: node.interpreter.clone(),
        preview: Some(node.preview),
        edge_label: node.edge_label.clone(),
        edge_label_at: node.edge_label_at,
        edge_source_side: node.edge_source_side,
        edge_target_side: node.edge_target_side,
        edge_via: node.edge_via.clone(),
        fold: node.fold,
        tags: node.tags.clone(),
        created_at: node.created_at.clone(),
    }
}

/// Makes `id` a direct child of `new_parent_id` in place of its structural
/// parent: promotes its existing extra edge from there (the document must
/// already state that relationship — this never invents one). The node moves
/// with its fragment, so a move into, out of or between groups would silently
/// change what its `x`/`y` mean; they are re-expressed so the node stays where
/// it was drawn.
pub fn reparent_node(raw: &str, id: &str, new_parent_id: &str) -> Result<String, EditError> {
    let canvas = parse_checked(raw)?;
    let node = canvas.node(id).ok_or_else(|| no_node(id))?;
    if node.parent.is_none() {
        return invalid("can't reparent the root node");
    }
    canvas
        .node(new_parent_id)
        .ok_or_else(|| no_node(new_parent_id))?;
    if !node.extra_parents.iter().any(|e| e.from == new_parent_id) {
        return invalid(format!(
            "{new_parent_id:?} is not one of {id:?}'s extra parents"
        ));
    }
    // The pre-move absolute position, in the *old* parent chain.
    let abs_before = canvas.resolve_absolute_position(id);
    let mut updated = mdcanvas::reparent_node(raw, id, new_parent_id).ok_or_else(|| {
        EditError::Invalid(format!(
            "can't reparent {id:?} onto {new_parent_id:?} (would create a cycle)"
        ))
    })?;
    let new_canvas = parse_checked(&updated)?;
    // Then the same place expressed in the new parent chain. Only when it
    // changed, and only through `x`/`y`; a node with no position of its own
    // stays unpositioned rather than getting a synthetic anchor.
    if let Some((abs_x, abs_y)) = abs_before {
        if let Some((local_x, local_y)) = new_canvas.absolute_to_local(id, abs_x, abs_y) {
            if let Some(new_node) = new_canvas.node(id) {
                if new_node.x != Some(local_x) || new_node.y != Some(local_y) {
                    let meta = NodeMeta {
                        x: Some(local_x),
                        y: Some(local_y),
                        width: new_node.width,
                        height: new_node.height,
                        ..layout_cleared_meta(new_node)
                    };
                    if let Some(patched) = mdcanvas::set_node_meta(&updated, id, &meta) {
                        updated = patched;
                    }
                }
            }
        }
    }
    Ok(updated)
}

/// Moves node `id`'s whole subtree to sit immediately before (`before`) or
/// after `target_id`, which must be a sibling under the same structural parent
/// (moving among a *different* parent's children is [`reparent_node`]'s job).
pub fn move_sibling(
    raw: &str,
    id: &str,
    target_id: &str,
    before: bool,
) -> Result<String, EditError> {
    let position = if before {
        mdcanvas::MoveSiblingPosition::Before
    } else {
        mdcanvas::MoveSiblingPosition::After
    };
    let updated = mdcanvas::move_sibling(raw, id, target_id, position).map_err(|e| match e {
        mdcanvas::MoveSiblingError::NotFound(_) => EditError::NotFound(e.to_string()),
        mdcanvas::MoveSiblingError::NotSiblings(_, _) | mdcanvas::MoveSiblingError::SameNode => {
            EditError::Invalid(e.to_string())
        }
    })?;
    parse_checked(&updated)?;
    Ok(updated)
}

/// Changes node `id`'s id to `new_id`, rewriting every reference the parser
/// tracks (other nodes' `parent=` / `meshfox:edge from=`) exactly and
/// best-effort the `deps="id/block"` text of fences. Not found for an unknown
/// id; invalid for an empty id, one with a `"`, a reserved one, or one already
/// taken.
pub fn rename_node_id(raw: &str, id: &str, new_id: &str) -> Result<String, EditError> {
    let updated = mdcanvas::rename_node_id(raw, id, new_id).map_err(|e| match e {
        mdcanvas::RenameIdError::NotFound(_) => EditError::NotFound(e.to_string()),
        _ => EditError::Invalid(e.to_string()),
    })?;
    parse_checked(&updated)?;
    Ok(updated)
}

/// Drops node `id`'s explicit `id="..."` attribute, handing it back to the
/// parser's title-slug fallback. Returns the new document and the id the node
/// has now (usually unchanged; a fresh slug if the title has diverged).
pub fn clear_node_id(raw: &str, id: &str) -> Result<(String, String), EditError> {
    let (updated, new_id) =
        mdcanvas::clear_node_id(raw, id).map_err(|e| EditError::NotFound(e.to_string()))?;
    parse_checked(&updated)?;
    Ok((updated, new_id))
}

/// Replaces the document's whole set of declared `meshfox:option` names with
/// exactly `options`, in the given order; an empty list removes every
/// declaration.
pub fn set_options(raw: &str, options: &[String]) -> Result<String, EditError> {
    let updated = mdcanvas::set_document_options(raw, options)
        .ok_or_else(|| EditError::Invalid("document has no root node".to_string()))?;
    parse_checked(&updated)?;
    Ok(updated)
}

/// Appends `text` to node `id`'s body.
pub fn append_node_body(raw: &str, id: &str, text: &str) -> Result<String, EditError> {
    let updated = mdcanvas::append_node_body(raw, id, text).ok_or_else(|| no_node(id))?;
    parse_checked(&updated)?;
    Ok(updated)
}

/// One node's new box in a layout save — what the client itself moved or
/// resized, nothing else. `width`/`height` omitted mean "leave as they are".
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LayoutBox {
    pub id: String,
    pub x: f64,
    pub y: f64,
    #[serde(default)]
    pub width: Option<f64>,
    #[serde(default)]
    pub height: Option<f64>,
}

/// A node's currently drawn position: only a sort hint for the same request
/// (see [`save_layout`]), never authored data.
#[derive(Debug, Clone, Copy, Deserialize)]
pub struct LayoutHint {
    pub x: f64,
    pub y: f64,
}

/// Saves the position/size of every box back into the document, patching each
/// node's `meshfox:node` line in place rather than regenerating anything, then
/// reorders each parent's children to match the new layout (top-to-bottom,
/// then left-to-right), so the heading order tracks the canvas. Only positions
/// and sizes are taken from the client: each box is applied onto the node's
/// *current* fields, so a stale copy of a node cannot write itself back along
/// with its drag. `locate` turns a client's node id into the id it is written
/// under here (`None`: no longer there — skipped, not an error).
pub fn save_layout(
    raw: &str,
    boxes: &[LayoutBox],
    hints: &HashMap<String, LayoutHint>,
    locate: &dyn Fn(&str) -> Option<String>,
) -> Result<String, EditError> {
    use crate::canvas::NodeType;
    let mut out = raw.to_string();
    // Parsed once, before any patch: `set_node_meta` only rewrites the one
    // node's own line, so every other node's fields are the same here as in
    // `out` however many boxes were applied.
    let current = parse_checked(raw)?;
    for node_box in boxes {
        let Some(local_id) = locate(&node_box.id) else {
            continue;
        };
        let Some(node) = current.node(&local_id) else {
            continue;
        };
        // Include nodes never reach the client as such (they are resolved
        // away first); skipped, same as a stray unknown id.
        if node.node_type == NodeType::Include {
            continue;
        }
        // A group's *size* is always derived from its members, never
        // authored — whatever width/height the client reports is ignored
        // rather than written as if it were data. Its own *position* is a
        // real anchor its members are relative to, so that is kept.
        let mut meta = NodeMeta::from_node(node);
        meta.x = Some(node_box.x);
        meta.y = Some(node_box.y);
        if node.node_type == NodeType::Group {
            meta.width = None;
            meta.height = None;
        } else {
            meta.width = node_box.width.or(meta.width);
            meta.height = node_box.height.or(meta.height);
        }
        if let Some(patched) = mdcanvas::set_node_meta(&out, &local_id, &meta) {
            out = patched;
        }
    }
    let mut local_hints: HashMap<String, (f64, f64)> = HashMap::new();
    for (id, hint) in hints {
        if let Some(local_id) = locate(id) {
            local_hints.insert(local_id, (hint.x, hint.y));
        }
    }
    if let Some(reordered) = mdcanvas::reorder_by_position(&out, &local_hints) {
        out = reordered;
    }
    parse_checked(&out)?;
    Ok(out)
}

/// Clears every node's stored `x`/`y`/`width`/`height`, reverting the document
/// to auto-placed. Every other field is carried over; a group's own position
/// (a real anchor) is cleared like any other.
pub fn clear_layout(raw: &str) -> Result<String, EditError> {
    let canvas = parse_checked(raw)?;
    let mut out = raw.to_string();
    for node in &canvas.nodes {
        if let Some(patched) = mdcanvas::set_node_meta(&out, &node.id, &layout_cleared_meta(node)) {
            out = patched;
        }
    }
    Ok(out)
}

/// [`clear_layout`] for one node.
pub fn clear_node_layout(raw: &str, id: &str) -> Result<String, EditError> {
    let canvas = parse_checked(raw)?;
    let node = canvas.node(id).ok_or_else(|| no_node(id))?;
    mdcanvas::set_node_meta(raw, id, &layout_cleared_meta(node)).ok_or_else(|| no_node(id))
}

/// Re-sorts every parent's structural children by their position, with no
/// hints (an unpositioned sibling sorts last, stably).
pub fn reorder_siblings(raw: &str) -> Result<String, EditError> {
    mdcanvas::reorder_by_position(raw, &HashMap::new())
        .ok_or_else(|| EditError::Invalid("failed to parse".to_string()))
}

/// Changes to one runnable code block's fence attributes (and, optionally, its
/// code) — `PATCH /api/nodes/:id/blocks/:name`. A field that is absent is left
/// alone.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BlockAttrsUpdate {
    pub name: Option<String>,
    pub lang: Option<String>,
    pub cache: Option<bool>,
    pub always: Option<bool>,
    pub default: Option<bool>,
    pub tty: Option<bool>,
    pub autoclose: Option<bool>,
    pub service: Option<bool>,
    /// Comma-separated `deps=` list, the syntax `--deps` already accepts —
    /// `None` leaves it untouched, `Some("")` clears it, anything else replaces
    /// it outright.
    pub deps: Option<String>,
    /// Same convention as `deps`, for `env=`.
    pub env: Option<String>,
    /// `None` (the field not sent) leaves the interpreter untouched.
    /// `clearInterpreter: true` clears it (mutually exclusive with `interpreter`
    /// being set): a plain `Option<String>` cannot reach that third "explicitly
    /// cleared" state on its own.
    pub interpreter: Option<String>,
    #[serde(default)]
    pub clear_interpreter: bool,
    /// New code, replacing everything between the fence's delimiter lines.
    /// `None` leaves it untouched.
    pub code: Option<String>,
}

/// Applies `req` to the runnable block `block_name` of node `id`. When `deps`
/// is touched, the whole document is checked with `validate_deps` — the one
/// check that needs the run planner (every reference resolves, no cycles, one
/// default per node), so it is the caller's: a server passes the real one, a
/// host that never runs anything may pass `|_| Ok(())`.
pub fn update_block_attrs(
    raw: &str,
    id: &str,
    block_name: &str,
    req: &BlockAttrsUpdate,
    validate_deps: &dyn Fn(&Canvas) -> Result<(), String>,
) -> Result<String, EditError> {
    if req.interpreter.is_some() && req.clear_interpreter {
        return invalid("interpreter is mutually exclusive with clearInterpreter");
    }
    let interpreter = if req.clear_interpreter {
        Some(None)
    } else {
        req.interpreter.clone().map(Some)
    };
    let deps = req.deps.as_deref().map(crate::fence::parse_deps_list);
    let env = req.env.as_deref().map(crate::fence::parse_env_list);
    let not_found = || {
        EditError::NotFound(format!(
            "no runnable code block named {block_name:?} in node {id:?}"
        ))
    };

    let canvas = parse_checked(raw)?;
    let node = canvas.node(id).ok_or_else(not_found)?;
    if !node
        .runnable_blocks()
        .iter()
        .any(|b| b.name.as_deref() == Some(block_name))
    {
        return Err(not_found());
    }
    let patch = mdcanvas::FenceAttrsPatch {
        name: req.name.clone(),
        lang: req.lang.clone(),
        cache: req.cache,
        always: req.always,
        default: req.default,
        tty: req.tty,
        autoclose: req.autoclose,
        service: req.service,
        deps,
        env,
        interpreter,
        code: req.code.clone(),
    };
    if node.is_runnable_file() {
        if let Some(why) = patch.file_node_conflict() {
            return invalid(why.to_string());
        }
    }
    let updated = mdcanvas::set_fence_attrs(raw, id, block_name, &patch).ok_or_else(not_found)?;
    let updated_canvas = parse_checked(&updated)?;
    if req.deps.is_some() {
        validate_deps(&updated_canvas).map_err(EditError::Invalid)?;
    }
    Ok(updated)
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

    // ---- the other document edits ----

    fn ids(raw: &str) -> Vec<String> {
        Canvas::from_markdown(raw)
            .unwrap()
            .nodes
            .iter()
            .map(|n| n.id.clone())
            .collect()
    }

    #[test]
    fn a_node_is_created_under_a_parent_with_a_readable_or_random_id() {
        let (out, id) = create_node(DOC, "root", "Fresh idea", true, Some("its body")).unwrap();
        assert_eq!(id, "fresh-idea");
        assert_eq!(node(&out, "fresh-idea").parent.as_deref(), Some("root"));
        assert_eq!(node(&out, "fresh-idea").text.trim(), "its body");
        assert_eq!(
            node(&out, "child").text.trim(),
            "body",
            "nothing else moved"
        );

        let (out, id) = create_node(DOC, "child", "Under child", false, None).unwrap();
        assert_ne!(id, "under-child", "a random id, not a slug");
        assert_eq!(node(&out, &id).parent.as_deref(), Some("child"));
        assert!(matches!(
            create_node(DOC, "ghost", "x", true, None),
            Err(EditError::NotFound(_))
        ));
    }

    #[test]
    fn a_node_is_removed_with_its_subtree_or_alone_and_the_root_never() {
        let nested = create_node(DOC, "child", "Grandchild", true, None)
            .unwrap()
            .0;
        let all = remove_node(&nested, "child", false).unwrap();
        assert!(!ids(&all).contains(&"child".to_string()));
        assert!(!ids(&all).contains(&"grandchild".to_string()));
        let alone = remove_node(&nested, "child", true).unwrap();
        assert!(!ids(&alone).contains(&"child".to_string()));
        assert_eq!(node(&alone, "grandchild").parent.as_deref(), Some("root"));
        assert!(err(remove_node(DOC, "root", false))
            .to_string()
            .contains("root"));
        assert!(matches!(
            remove_node(DOC, "ghost", false),
            Err(EditError::NotFound(_))
        ));
    }

    #[test]
    fn siblings_are_moved_next_to_each_other_and_only_siblings() {
        let out = move_sibling(DOC, "other", "child", true).unwrap();
        let order: Vec<_> = ids(&out);
        assert!(order.iter().position(|i| i == "other") < order.iter().position(|i| i == "child"));
        assert!(matches!(
            move_sibling(DOC, "child", "child", true),
            Err(EditError::Invalid(_))
        ));
        assert!(matches!(
            move_sibling(DOC, "child", "ghost", true),
            Err(EditError::NotFound(_))
        ));
        let nested = create_node(DOC, "child", "Grandchild", true, None)
            .unwrap()
            .0;
        assert!(matches!(
            move_sibling(&nested, "grandchild", "other", false),
            Err(EditError::Invalid(_))
        ));
    }

    #[test]
    fn a_node_is_reparented_only_onto_one_of_its_extra_parents() {
        let with_edge = apply(
            DOC,
            "other",
            NodeUpdate {
                edges: Some(EdgeOps {
                    add: vec!["child".into()],
                    ..EdgeOps::default()
                }),
                ..NodeUpdate::blank()
            },
        )
        .unwrap();
        let out = reparent_node(&with_edge, "other", "child").unwrap();
        assert_eq!(node(&out, "other").parent.as_deref(), Some("child"));
        // Not an extra parent: refused; the root: refused.
        assert!(matches!(
            reparent_node(DOC, "other", "child"),
            Err(EditError::Invalid(_))
        ));
        assert!(matches!(
            reparent_node(&with_edge, "root", "child"),
            Err(EditError::Invalid(_))
        ));
        assert!(matches!(
            reparent_node(&with_edge, "other", "ghost"),
            Err(EditError::NotFound(_))
        ));
    }

    #[test]
    fn an_id_is_renamed_everywhere_it_is_referenced_and_cleared_back_to_the_slug() {
        let with_edge = apply(
            DOC,
            "other",
            NodeUpdate {
                edges: Some(EdgeOps {
                    add: vec!["child".into()],
                    ..EdgeOps::default()
                }),
                ..NodeUpdate::blank()
            },
        )
        .unwrap();
        let out = rename_node_id(&with_edge, "child", "kid").unwrap();
        assert!(ids(&out).contains(&"kid".to_string()));
        assert_eq!(
            node(&out, "other").extra_parents[0].from,
            "kid",
            "references follow"
        );
        assert!(matches!(
            rename_node_id(DOC, "child", "other"),
            Err(EditError::Invalid(_))
        ));
        assert!(matches!(
            rename_node_id(DOC, "child", ""),
            Err(EditError::Invalid(_))
        ));
        assert!(matches!(
            rename_node_id(DOC, "ghost", "x"),
            Err(EditError::NotFound(_))
        ));
        let (cleared, now) = clear_node_id(&out, "kid").unwrap();
        assert_eq!(now, "child", "the id is the title's slug again");
        assert!(ids(&cleared).contains(&"child".to_string()));
    }

    #[test]
    fn options_are_replaced_and_a_body_is_appended() {
        let out = set_options(DOC, &["unfold".to_string()]).unwrap();
        assert_eq!(
            crate::options::declared_options(&Canvas::from_markdown(&out).unwrap()).unwrap(),
            ["unfold"]
        );
        let none = set_options(&out, &[]).unwrap();
        assert!(
            crate::options::declared_options(&Canvas::from_markdown(&none).unwrap())
                .unwrap()
                .is_empty()
        );
        let appended = append_node_body(DOC, "child", "\nmore").unwrap();
        assert!(node(&appended, "child").text.contains("body"));
        assert!(node(&appended, "child").text.contains("more"));
        assert!(matches!(
            append_node_body(DOC, "ghost", "x"),
            Err(EditError::NotFound(_))
        ));
    }

    #[test]
    fn a_blocks_attributes_and_code_change_and_deps_are_checked_by_the_caller() {
        let doc =
            "# Root\n<!-- meshfox:node id=\"root\" -->\n\n```bash name=\"build\"\necho hi\n```\n";
        let ok = |_: &Canvas| Ok(());
        let out = update_block_attrs(
            doc,
            "root",
            "build",
            &BlockAttrsUpdate {
                cache: Some(true),
                code: Some("echo bye".into()),
                ..BlockAttrsUpdate::blank()
            },
            &ok,
        )
        .unwrap();
        assert!(out.contains("cache") && out.contains("echo bye") && !out.contains("echo hi"));
        // `deps` is checked only when it is touched, and the caller's check wins.
        let refuse = |_: &Canvas| Err("a dependency cycle".to_string());
        let touched = BlockAttrsUpdate {
            deps: Some("build".into()),
            ..BlockAttrsUpdate::blank()
        };
        assert!(
            err(update_block_attrs(doc, "root", "build", &touched, &refuse))
                .to_string()
                .contains("cycle")
        );
        let untouched = BlockAttrsUpdate {
            cache: Some(false),
            ..BlockAttrsUpdate::blank()
        };
        assert!(update_block_attrs(doc, "root", "build", &untouched, &refuse).is_ok());
        // Unknown block / node, and the mutually exclusive pair.
        assert!(matches!(
            update_block_attrs(doc, "root", "nope", &untouched, &ok),
            Err(EditError::NotFound(_))
        ));
        assert!(matches!(
            update_block_attrs(doc, "ghost", "build", &untouched, &ok),
            Err(EditError::NotFound(_))
        ));
        let both = BlockAttrsUpdate {
            interpreter: Some("python".into()),
            clear_interpreter: true,
            ..BlockAttrsUpdate::blank()
        };
        assert!(matches!(
            update_block_attrs(doc, "root", "build", &both, &ok),
            Err(EditError::Invalid(_))
        ));
    }

    #[test]
    fn a_layout_save_writes_boxes_onto_current_fields_and_reorders_by_position() {
        let locate = |id: &str| Some(id.to_string());
        let boxes = vec![
            LayoutBox {
                id: "other".into(),
                x: 0.0,
                y: 0.0,
                width: Some(200.0),
                height: None,
            },
            LayoutBox {
                id: "child".into(),
                x: 500.0,
                y: 0.0,
                width: None,
                height: Some(90.0),
            },
            LayoutBox {
                id: "ghost".into(),
                x: 1.0,
                y: 1.0,
                width: None,
                height: None,
            },
        ];
        let out = save_layout(DOC, &boxes, &HashMap::new(), &locate).unwrap();
        let other = node(&out, "other");
        assert_eq!(
            (other.x, other.y, other.width),
            (Some(0.0), Some(0.0), Some(200.0))
        );
        let child = node(&out, "child");
        assert_eq!((child.x, child.height), (Some(500.0), Some(90.0)));
        assert_eq!(child.tags, ["a"], "fields the box does not carry are kept");
        // `other` is to the left of `child` now, so it comes first in the text.
        let order = ids(&out);
        assert!(order.iter().position(|i| i == "other") < order.iter().position(|i| i == "child"));
        // A node the client no longer knows is skipped, not an error.
        assert!(
            save_layout(DOC, &boxes, &HashMap::new(), &|id: &str| (id != "ghost")
                .then(|| id.to_string()))
            .is_ok()
        );
    }

    #[test]
    fn a_group_never_stores_the_size_a_client_reports() {
        let group_doc = "# Root\n<!-- meshfox:node id=\"root\" -->\n\n## G\n<!-- meshfox:node id=\"g\" type=\"group\" -->\n";
        let boxes = vec![LayoutBox {
            id: "g".into(),
            x: 5.0,
            y: 6.0,
            width: Some(100.0),
            height: Some(100.0),
        }];
        let out = save_layout(group_doc, &boxes, &HashMap::new(), &|id: &str| {
            Some(id.to_string())
        })
        .unwrap();
        let g = node(&out, "g");
        assert_eq!(
            (g.x, g.y, g.width, g.height),
            (Some(5.0), Some(6.0), None, None)
        );
    }

    #[test]
    fn layout_is_cleared_for_one_node_or_all_and_everything_else_stays() {
        let laid = apply(
            DOC,
            "child",
            NodeUpdate {
                x: Some(1.0),
                y: Some(2.0),
                width: Some(3.0),
                height: Some(4.0),
                color: Some("2".into()),
                ..NodeUpdate::blank()
            },
        )
        .unwrap();
        let one = clear_node_layout(&laid, "child").unwrap();
        let child = node(&one, "child");
        assert_eq!(
            (child.x, child.y, child.width, child.height),
            (None, None, None, None)
        );
        assert_eq!(
            (child.color.as_deref(), child.tags.as_slice()),
            (Some("2"), ["a".to_string()].as_slice())
        );
        let all = clear_layout(&laid).unwrap();
        assert_eq!(node(&all, "child").x, None);
        assert!(matches!(
            clear_node_layout(&laid, "ghost"),
            Err(EditError::NotFound(_))
        ));
        assert!(reorder_siblings(DOC).is_ok());
    }
}
