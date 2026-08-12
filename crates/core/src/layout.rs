//! A default, "good enough" tree layout: given the canvas tree (and
//! `extra_parents`, which don't affect placement), compute an x/y/width/
//! height for every node from scratch.
//!
//! Shaped for documents, not org charts: the root and its direct children
//! ("sections") read top-to-bottom in one column, same as a normal
//! Markdown document's title followed by its headings. Anything nested
//! deeper than that branches off to the right of its own parent instead of
//! continuing the vertical list — a section's blocks hang off it sideways,
//! and further nesting keeps growing rightward from there.
//!
//! Size is estimated from each node's content (line count, whether it has
//! a code fence, whether it has runnable blocks needing a button row) —
//! not a flat constant — so a node with a paragraph and cached output ends
//! up taller than a one-line stub.
//!
//! This is deliberately a simple heuristic, not a publication-quality
//! tree-drawing algorithm — it exists to give freshly-authored nodes a
//! reasonable default, not to be the final word on layout. Nothing here is
//! persisted automatically; `meshfox fmt` is this module's only caller,
//! writing the result into the file for nodes that don't already have a
//! position. The web UI no longer uses this at all — it computes its own,
//! independent layout for unpositioned nodes client-side (see
//! `web/src/autolayout.ts`), reactive to the browser's actual viewport size
//! and each node's real rendered content height, neither of which this
//! module (or the server) has access to.
//!
//! Group boxes are always computed from their descendants regardless of
//! any stored size, same rule as everywhere else groups are handled.

use crate::canvas::{Canvas, Node, NodeType};
use std::collections::HashMap;

const H_GAP: f64 = 80.0;
const V_GAP: f64 = 60.0;
const GROUP_PADDING: f64 = 40.0;
const GROUP_TITLE_SPACE: f64 = 40.0;
/// The root's direct children ("sections") get only a small nudge right of
/// it, reading top-to-bottom like a document's title followed by its
/// headings — the full indent step (`H_GAP` + parent width) only kicks in
/// one level deeper, where a section's own content branches off it.
const ROOT_CHILD_INDENT: f64 = 32.0;

const NARROW_WIDTH: f64 = 280.0;
const WIDE_WIDTH: f64 = 440.0;
const MIN_HEIGHT: f64 = 100.0;
const MAX_HEIGHT: f64 = 640.0;
const TITLE_HEIGHT: f64 = 40.0;
const BODY_PADDING: f64 = 24.0;
const LINE_HEIGHT: f64 = 22.0;
const BUTTON_ROW_HEIGHT: f64 = 40.0;
const CHARS_PER_LINE_NARROW: f64 = 42.0;
const CHARS_PER_LINE_WIDE: f64 = 70.0;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LayoutBox {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// A fresh layout for every node in `canvas`, keyed by node id.
///
/// The root and its direct children ("sections") read top-to-bottom in
/// close to one column, same as a document's title followed by its
/// headings. From there down it's a classic indented tree view: each
/// section's own content (and any deeper nesting under that) steps fully
/// to the right of its parent, with siblings at a given depth stacked
/// vertically — same shape as a file tree or a collapsed outline.
pub fn compute(canvas: &Canvas) -> HashMap<String, LayoutBox> {
    let mut boxes = HashMap::new();
    let Some(root) = canvas.nodes.iter().find(|n| n.parent.is_none()) else {
        return boxes;
    };

    let (rw, rh) = intrinsic_size(root);
    boxes.insert(root.id.clone(), LayoutBox { x: 0.0, y: 0.0, width: rw, height: rh });

    let mut y_cursor = rh;
    for section in canvas.children(&root.id) {
        y_cursor += V_GAP;
        let consumed = place_rightward(canvas, section, ROOT_CHILD_INDENT, y_cursor, &mut boxes);
        y_cursor += consumed;
    }

    layout_groups(canvas, &mut boxes);
    boxes
}

/// Lays out `node` and its full subtree at indent column `x`: `node` itself
/// goes at `x`, and its children recurse one column further right,
/// stacking vertically among themselves — the classic indented-tree-view
/// shape, applied recursively at every depth. Returns the total vertical
/// space this node's subtree consumed, so a caller placing multiple
/// siblings in the same column can advance its cursor without overlapping
/// this one.
///
/// Known imprecision: a `group` descendant's box gets padded outward by
/// `layout_groups` *after* this pass reserves space for it, so a group
/// sitting at the edge of its reserved band can slightly overlap a
/// neighboring sibling. `V_GAP`/`H_GAP` leave enough slack for this not to
/// matter in ordinary trees. A node with a real (authored or dragged)
/// position also breaks the "no overlap" guarantee across it: it anchors
/// its own subtree there instead of at the caller-supplied ideal spot (see
/// below), so a sibling section positioned purely synthetically has no way
/// to know to route around it. Same tradeoff as the group case — acceptable
/// for a "good enough default", not attempted for real layout quality.
fn place_rightward(
    canvas: &Canvas,
    node: &Node,
    x: f64,
    y: f64,
    boxes: &mut HashMap<String, LayoutBox>,
) -> f64 {
    let (w, h) = intrinsic_size(node);
    // Anchor at the node's own real position when it has one, rather than
    // the ideal spot the caller computed — so a freshly added child of a
    // node the user has actually dragged branches off from where it really
    // sits on screen, not from the synthetic position that node would
    // occupy in a from-scratch auto-layout. Without this, a child's
    // suggested position (and thus, transitively, its own group's derived
    // box — see `layout_groups`) could land anywhere in the document,
    // nowhere near the parent it's actually nested under.
    let (x, y) = match (node.x, node.y) {
        (Some(rx), Some(ry)) => (rx, ry),
        _ => (x, y),
    };
    let children = canvas.children(&node.id);

    if children.is_empty() {
        boxes.insert(node.id.clone(), LayoutBox { x, y, width: w, height: h });
        return h;
    }

    let child_x = x + w + H_GAP;
    let mut cursor = y;
    // `span` tracks how much vertical space the children collectively need
    // as a pure sum of gaps + each child's own returned size — deliberately
    // *not* `cursor - y`. A child anchored at its own real position (just
    // above) can make `cursor` jump somewhere unrelated to `y`, and reusing
    // that jump as this node's "consumed" size would balloon it into a box
    // wildly bigger than its actual content — which then bubbles up into
    // the group padding pass below and, worse, into `layout_groups`' bounds
    // for any enclosing group. Summing each child's own height keeps this
    // node's reported size a true reflection of its subtree regardless of
    // where any single descendant's real position happens to sit.
    let mut span = 0.0;
    for (i, child) in children.iter().enumerate() {
        if i > 0 {
            cursor += V_GAP;
            span += V_GAP;
        }
        let child_h = place_rightward(canvas, child, child_x, cursor, boxes);
        cursor += child_h;
        span += child_h;
    }
    let consumed = span.max(h);
    // Only recenter a synthetic y against its children's stacked span — a
    // real y is the user's own, and shifting it to "look centered" would
    // silently move a node they positioned on purpose.
    let node_y = if node.y.is_some() { y } else { y + (consumed - h) / 2.0 };
    boxes.insert(node.id.clone(), LayoutBox { x, y: node_y, width: w, height: h });
    consumed
}

/// Overrides every group's box with the bounding box of its full subtree
/// (all descendants, not just direct children), deepest groups first so a
/// group-of-groups sees its nested group's already-resolved box.
fn layout_groups(canvas: &Canvas, boxes: &mut HashMap<String, LayoutBox>) {
    let mut groups: Vec<&Node> = canvas
        .nodes
        .iter()
        .filter(|n| n.node_type == NodeType::Group)
        .collect();
    // Actual tree depth, not `.level` — once a subtree passes CommonMark's
    // level-6 heading ceiling, `.level` stops climbing (every node past
    // that point writes `######` and relies on an explicit `parent=`
    // attribute instead, see `mdcanvas::insert_child_node`), so two groups
    // at genuinely different nesting depths could otherwise tie or even
    // sort backwards.
    groups.sort_by(|a, b| tree_depth(canvas, &b.id).cmp(&tree_depth(canvas, &a.id)));

    for group in groups {
        let members = subtree_ids(canvas, &group.id);
        let member_boxes: Vec<LayoutBox> =
            members.iter().filter_map(|id| member_box(canvas, id, boxes)).collect();
        if member_boxes.is_empty() {
            continue;
        }
        let min_x = member_boxes.iter().map(|b| b.x).fold(f64::INFINITY, f64::min);
        let min_y = member_boxes.iter().map(|b| b.y).fold(f64::INFINITY, f64::min);
        let max_x = member_boxes
            .iter()
            .map(|b| b.x + b.width)
            .fold(f64::NEG_INFINITY, f64::max);
        let max_y = member_boxes
            .iter()
            .map(|b| b.y + b.height)
            .fold(f64::NEG_INFINITY, f64::max);
        boxes.insert(
            group.id.clone(),
            LayoutBox {
                x: min_x - GROUP_PADDING,
                y: min_y - GROUP_PADDING - GROUP_TITLE_SPACE,
                width: max_x - min_x + GROUP_PADDING * 2.0,
                height: max_y - min_y + GROUP_PADDING * 2.0 + GROUP_TITLE_SPACE,
            },
        );
    }
}

/// A group member's box for bounding-box purposes: its *real*, authored (or
/// dragged) position/size when it has one, since that's what actually
/// renders on screen — falling back to the synthetic tree-layout box
/// (`boxes`, from `place_rightward`) only for a member that's still
/// unpositioned. Without this, a group's box would track where its members
/// *would* sit in a fresh auto-layout instead of where they actually are,
/// so moving a real node (or the whole group, which moves its members —
/// see the web client's group-drag handling) would silently leave the
/// group's dashed frame behind.
fn member_box(canvas: &Canvas, id: &str, boxes: &HashMap<String, LayoutBox>) -> Option<LayoutBox> {
    let node = canvas.node(id)?;
    if let (Some(x), Some(y)) = (node.x, node.y) {
        let (width, height) = intrinsic_size(node);
        return Some(LayoutBox { x, y, width, height });
    }
    boxes.get(id).copied()
}

/// Number of `parent` hops from `id` up to the root — unlike `.level`, this
/// keeps climbing past CommonMark's level-6 heading ceiling (see
/// `layout_groups`), since it walks the resolved tree rather than counting
/// `#` characters.
fn tree_depth(canvas: &Canvas, id: &str) -> usize {
    let mut depth = 0;
    let mut cur = canvas.node(id).and_then(|n| n.parent.clone());
    while let Some(p) = cur {
        depth += 1;
        cur = canvas.node(&p).and_then(|n| n.parent.clone());
    }
    depth
}

fn subtree_ids(canvas: &Canvas, id: &str) -> Vec<String> {
    let mut out = Vec::new();
    for child in canvas.children(id) {
        out.push(child.id.clone());
        out.extend(subtree_ids(canvas, &child.id));
    }
    out
}

fn intrinsic_size(node: &Node) -> (f64, f64) {
    let (ew, eh) = estimate_size(node);
    (node.width.unwrap_or(ew), node.height.unwrap_or(eh))
}

/// A rough, content-aware default size — not real text measurement (no
/// font metrics available here), just enough to tell a one-line stub from
/// a node with a paragraph, a code fence, and cached output.
fn estimate_size(node: &Node) -> (f64, f64) {
    match node.node_type {
        NodeType::Group => (NARROW_WIDTH, MIN_HEIGHT),
        NodeType::File | NodeType::Link | NodeType::Include => {
            (NARROW_WIDTH, TITLE_HEIGHT + BODY_PADDING + LINE_HEIGHT * 2.0)
        }
        NodeType::Text => estimate_text_size(&node.text),
    }
}

fn estimate_text_size(text: &str) -> (f64, f64) {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return (NARROW_WIDTH, MIN_HEIGHT);
    }

    let wide = has_code_fence(trimmed);
    let width = if wide { WIDE_WIDTH } else { NARROW_WIDTH };
    let chars_per_line = if wide { CHARS_PER_LINE_WIDE } else { CHARS_PER_LINE_NARROW };

    let wrapped_lines: f64 = trimmed
        .lines()
        .map(|line| {
            if line.is_empty() {
                1.0
            } else {
                (line.chars().count() as f64 / chars_per_line).ceil().max(1.0)
            }
        })
        .sum();

    let has_runnable = !crate::fence::scan_code_blocks(trimmed).is_empty();
    let button_row = if has_runnable { BUTTON_ROW_HEIGHT } else { 0.0 };

    let height = TITLE_HEIGHT + BODY_PADDING + wrapped_lines * LINE_HEIGHT + button_row;
    (width, height.clamp(MIN_HEIGHT, MAX_HEIGHT))
}

fn has_code_fence(text: &str) -> bool {
    text.lines().any(|l| l.trim_start().starts_with("```"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mdcanvas::parse;

    fn overlaps(a: &LayoutBox, b: &LayoutBox) -> bool {
        a.x < b.x + b.width && b.x < a.x + a.width && a.y < b.y + b.height && b.y < a.y + a.height
    }

    #[test]
    fn sections_get_a_small_nudge_and_stack_vertically() {
        let doc = "# Root\n\n## A\n<!-- meshfox:node -->\n\nbody\n\n## B\n<!-- meshfox:node -->\n\nbody\n";
        let canvas = parse(doc).unwrap();
        let boxes = compute(&canvas);
        assert_eq!(boxes.len(), 3);
        let root = boxes.get("root").unwrap();
        let a = boxes.get("a").unwrap();
        let b = boxes.get("b").unwrap();
        assert!(!overlaps(a, b));
        // a small nudge right of root, well short of a full indent step
        assert!(a.x > root.x);
        assert!(a.x < root.x + root.width);
        assert_eq!(a.x, b.x);
        // stacked top to bottom
        assert!(b.y > a.y);
    }

    #[test]
    fn deeper_nesting_branches_to_the_right_of_its_parent() {
        let doc = "# Root\n\n## A\n<!-- meshfox:node -->\n\n### A1\n<!-- meshfox:node -->\n\nbody\n\n#### A1a\n<!-- meshfox:node -->\n\nbody\n";
        let canvas = parse(doc).unwrap();
        let boxes = compute(&canvas);
        let a = boxes.get("a").unwrap();
        let a1 = boxes.get("a1").unwrap();
        let a1a = boxes.get("a1a").unwrap();
        assert!(a1.x >= a.x + a.width);
        // nesting keeps growing rightward, not just one level deep
        assert!(a1a.x >= a1.x + a1.width);
    }

    #[test]
    fn tall_subtree_pushes_next_section_down_without_overlap() {
        let doc = "# Root\n\n## A\n<!-- meshfox:node -->\n\n### A1\n<!-- meshfox:node -->\n\nbody\n\n### A2\n<!-- meshfox:node -->\n\nbody\n\n## B\n<!-- meshfox:node -->\n\nbody\n";
        let canvas = parse(doc).unwrap();
        let boxes = compute(&canvas);
        let a1 = boxes.get("a1").unwrap();
        let a2 = boxes.get("a2").unwrap();
        let b = boxes.get("b").unwrap();
        assert!(!overlaps(a1, a2));
        assert!(!overlaps(a1, b));
        assert!(!overlaps(a2, b));
    }

    #[test]
    fn longer_content_yields_a_taller_box() {
        let short = "# Root\n\n## Short\n<!-- meshfox:node -->\n\none line\n";
        let long = "# Root\n\n## Long\n<!-- meshfox:node -->\n\nfirst paragraph with a fair bit of text in it\n\nsecond paragraph, also with a fair bit of text in it\n\nthird paragraph too\n";
        let short_box = compute(&parse(short).unwrap());
        let long_box = compute(&parse(long).unwrap());
        assert!(long_box.get("long").unwrap().height > short_box.get("short").unwrap().height);
    }

    #[test]
    fn code_fence_yields_a_wider_box() {
        let plain = "# Root\n\n## Plain\n<!-- meshfox:node -->\n\njust prose\n";
        let coded =
            "# Root\n\n## Coded\n<!-- meshfox:node -->\n\n```bash name=\"x\"\necho hi\n```\n";
        let plain_box = compute(&parse(plain).unwrap());
        let coded_box = compute(&parse(coded).unwrap());
        assert!(coded_box.get("coded").unwrap().width > plain_box.get("plain").unwrap().width);
    }

    #[test]
    fn group_box_contains_all_descendants() {
        let doc = "# Root\n\n## Frame\n<!-- meshfox:node type=\"group\" -->\n\n### Child1\n<!-- meshfox:node -->\n\nbody\n\n### Child2\n<!-- meshfox:node -->\n\nbody\n";
        let canvas = parse(doc).unwrap();
        let boxes = compute(&canvas);
        let frame = boxes.get("frame").unwrap();
        let c1 = boxes.get("child1").unwrap();
        let c2 = boxes.get("child2").unwrap();
        for child in [c1, c2] {
            assert!(frame.x <= child.x);
            assert!(frame.y <= child.y);
            assert!(frame.x + frame.width >= child.x + child.width);
            assert!(frame.y + frame.height >= child.y + child.height);
        }
    }

    #[test]
    fn a_bare_childs_suggestion_anchors_off_its_real_parent_not_the_ideal_layout_spot() {
        // Parent has an explicit position far from where the tree
        // algorithm would put it on its own — a stand-in for the user
        // having dragged it. The bare (position-less) child's own
        // suggestion should branch off from *that* real spot, not from
        // wherever a from-scratch layout would have placed the parent.
        let doc = "# Root\n\n## Parent\n<!-- meshfox:node x=5000 y=5000 w=250 h=60 -->\n\n### Child\n<!-- meshfox:node -->\n\nbody\n";
        let canvas = parse(doc).unwrap();
        let boxes = compute(&canvas);
        let child = boxes.get("child").unwrap();
        assert!(child.x >= 5000.0 + 250.0);
        assert!(child.x < 5000.0 + 250.0 + H_GAP + 1.0);
        assert!((child.y - 5000.0).abs() < 1.0);
    }

    #[test]
    fn a_childs_real_position_far_from_its_synthetic_spot_does_not_balloon_the_parent() {
        // Child1 has a real position wildly far from where the tree
        // algorithm would place it — the parent's own reported size must
        // stay a true reflection of its subtree's actual content height,
        // not the raw (and here huge) gap between the child's synthetic and
        // real spots.
        let doc = "# Root\n\n## Parent\n<!-- meshfox:node -->\n\n### Child1\n<!-- meshfox:node x=9000 y=9000 w=100 h=80 -->\n\nbody\n\n### Child2\n<!-- meshfox:node -->\n\nbody\n";
        let canvas = parse(doc).unwrap();
        let boxes = compute(&canvas);
        let parent = boxes.get("parent").unwrap();
        assert!(parent.height < 1000.0);
    }

    #[test]
    fn group_box_follows_a_members_real_dragged_position() {
        // Child2 has an explicit, authored position/size far from where the
        // synthetic tree layout would ever put it (mimicking a user drag) —
        // the group's box must be derived from *that* real box, not the
        // ideal auto-layout position, or the frame would visibly stop
        // enclosing a member the moment it's dragged.
        let doc = "# Root\n\n## Frame\n<!-- meshfox:node type=\"group\" -->\n\n### Child1\n<!-- meshfox:node -->\n\nbody\n\n### Child2\n<!-- meshfox:node x=2000 y=3000 w=100 h=80 -->\n\nbody\n";
        let canvas = parse(doc).unwrap();
        let boxes = compute(&canvas);
        let frame = boxes.get("frame").unwrap();
        assert!(frame.x <= 2000.0);
        assert!(frame.y <= 3000.0);
        assert!(frame.x + frame.width >= 2100.0);
        assert!(frame.y + frame.height >= 3080.0);
    }

    #[test]
    fn outer_group_box_reflects_a_nested_groups_resolved_box_past_the_heading_ceiling() {
        // "Outer" and "Inner" are both written as `######` — CommonMark's
        // level-6 ceiling — with Inner's real nesting under Outer (and
        // Leaf's under Inner) only expressed via an explicit `parent=`
        // attribute (see `mdcanvas::insert_child_node`). Sorting groups by
        // raw `.level` alone would tie the two at 6 and risk resolving
        // Outer *before* Inner (stable sort keeps document order), so
        // Outer's box would be computed from Inner's still-synthetic
        // placeholder box instead of Inner's real, leaf-derived one —
        // this only passes if `layout_groups` instead resolves by actual
        // tree depth, deepest first.
        let doc = "# Root\n<!-- meshfox:node id=\"root\" -->\n\n###### Outer\n<!-- meshfox:node id=\"outer\" type=\"group\" -->\n\n###### Inner\n<!-- meshfox:node id=\"inner\" type=\"group\" parent=\"outer\" -->\n\n###### Leaf\n<!-- meshfox:node id=\"leaf\" parent=\"inner\" x=100 y=100 w=50 h=40 -->\n\nbody\n";
        let canvas = parse(doc).unwrap();
        assert_eq!(canvas.node("inner").unwrap().level, 6);
        assert_eq!(canvas.node("outer").unwrap().level, 6);

        let boxes = compute(&canvas);
        let outer = boxes.get("outer").unwrap();
        // `inner`'s own box is always correctly resolved by the time
        // `compute` returns (it only depends on `leaf`), positioned right
        // around leaf's real (100, 100). If `outer` were instead built
        // *before* inner's resolution (stable-sorting groups by tied raw
        // `.level` alone, rather than by actual tree depth, can pick this
        // wrong order), it would fold in inner's stale pre-resolution
        // placeholder too — parked far away, whatever the ordinary
        // synthetic tree layout happened to put there — ballooning
        // `outer.width` well past what leaf and inner's real, resolved
        // area alone would ever need.
        assert!(outer.width < 300.0, "outer.width = {} (ballooned by a stale placeholder?)", outer.width);
    }

    #[test]
    fn empty_group_keeps_a_placeholder_box() {
        let doc = "# Root\n\n## Empty\n<!-- meshfox:node type=\"group\" -->\n";
        let canvas = parse(doc).unwrap();
        let boxes = compute(&canvas);
        assert!(boxes.contains_key("empty"));
    }

    #[test]
    fn deterministic_across_runs() {
        let doc = "# Root\n\n## A\n<!-- meshfox:node -->\n\nbody\n\n## B\n<!-- meshfox:node -->\n\nbody\n";
        let canvas = parse(doc).unwrap();
        let b1 = compute(&canvas);
        let b2 = compute(&canvas);
        assert_eq!(b1.get("a"), b2.get("a"));
        assert_eq!(b1.get("b"), b2.get("b"));
    }

}
