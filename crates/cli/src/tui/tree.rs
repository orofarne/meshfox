//! Flattening the canvas's node tree into visible rows for the TUI's left
//! pane, respecting which nodes are currently collapsed.

use std::collections::HashSet;

use meshfox_core::{scan_runnable_blocks, Canvas, NodeType};

pub struct TreeRow {
    pub node_id: String,
    pub title: String,
    pub depth: usize,
    pub node_type: NodeType,
    pub has_children: bool,
    pub expanded: bool,
    /// How many runnable blocks this node's own body declares — 0 means
    /// nothing here for `r` to run.
    pub runnable_count: usize,
    pub has_cache: bool,
    pub has_tty: bool,
    /// Aggregate pass/fail across every embedded constraint fence in this
    /// node's own body (`node.constraint_results`, populated by
    /// `App`'s `resolve_includes` before `flatten` ever runs) — `Some(true)`
    /// only when every one of them passed, `None` when the node has no
    /// constraint fences at all.
    pub constraint_ok: Option<bool>,
    /// The node's own `color` attribute, verbatim (a JSON-Canvas preset
    /// `"1"`-`"6"` or a literal `#rrggbb` hex string) — resolved to an
    /// actual `ratatui::style::Color` at render time, not here (see
    /// `ui::render_tree`), same "keep the raw value on the model, resolve
    /// it where it's drawn" split `web/src/MeshNode.tsx`'s `resolveNodeColor`
    /// already uses. `None` means no color was set.
    pub color: Option<String>,
    /// The node's own `tags` attribute, verbatim — empty means none.
    pub tags: Vec<String>,
}

pub fn flatten(canvas: &Canvas, expanded: &HashSet<String>) -> Vec<TreeRow> {
    let mut rows = Vec::new();
    if let Ok(root) = canvas.root() {
        visit(canvas, root, 0, expanded, &mut rows);
    }
    rows
}

fn visit(
    canvas: &Canvas,
    node: &meshfox_core::Node,
    depth: usize,
    expanded: &HashSet<String>,
    rows: &mut Vec<TreeRow>,
) {
    let children = canvas.children(&node.id);
    let has_children = !children.is_empty();
    let is_expanded = depth == 0 || expanded.contains(&node.id);
    let blocks = scan_runnable_blocks(&node.id, &node.text);
    let constraint_ok = if node.constraint_results.is_empty() {
        None
    } else {
        Some(node.constraint_results.iter().all(|r| r.ok))
    };

    rows.push(TreeRow {
        node_id: node.id.clone(),
        title: node.title.clone(),
        depth,
        node_type: node.node_type,
        has_children,
        expanded: is_expanded,
        runnable_count: blocks.len(),
        has_cache: blocks.iter().any(|b| b.cache),
        has_tty: blocks.iter().any(|b| b.tty),
        constraint_ok,
        color: node.color.clone(),
        tags: node.tags.clone(),
    });

    if has_children && is_expanded {
        for child in children {
            visit(canvas, child, depth + 1, expanded, rows);
        }
    }
}
