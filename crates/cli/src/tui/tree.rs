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
    });

    if has_children && is_expanded {
        for child in children {
            visit(canvas, child, depth + 1, expanded, rows);
        }
    }
}
