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
    /// `node.effective_color` — the node's own explicit `color=`, or (see
    /// `meshfox_core::tag_colors::effective_color`) a fallback derived from
    /// its tags against the document's `meshfox:tag-color` defaults.
    /// Still a JSON-Canvas preset `"1"`-`"6"` or a literal `#rrggbb` hex
    /// string either way — resolved to an actual `ratatui::style::Color`
    /// at render time, not here (see `ui::render_tree`), same "keep the raw
    /// value on the model, resolve it where it's drawn" split
    /// `web/src/MeshNode.tsx`'s `resolveNodeColor` already uses. `None`
    /// means neither applies.
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
        color: node.effective_color.clone(),
        tags: node.tags.clone(),
    });

    if has_children && is_expanded {
        for child in children {
            visit(canvas, child, depth + 1, expanded, rows);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // TODO.canvas.md: "Node colour by tag" — `TreeRow.color` reads
    // `node.effective_color`, populated by the caller (`App`'s
    // `resolve_includes`) before `flatten` ever runs, same as
    // `constraint_results`.
    #[test]
    fn flatten_uses_a_nodes_effective_color_not_its_raw_color() {
        let mut canvas = meshfox_core::Canvas::from_markdown(concat!(
            "# Root\n<!-- meshfox:node id=\"root\" -->\n",
            "<!-- meshfox:tag-color tag=\"bug\" color=\"1\" -->\n\n",
            "## Child\n<!-- meshfox:node id=\"child\" tags=\"bug\" -->\n\nbody\n",
        ))
        .unwrap();
        meshfox_core::annotate_effective_colors(&mut canvas);

        let rows = flatten(&canvas, &HashSet::new());
        let child = rows.iter().find(|r| r.node_id == "child").unwrap();
        assert_eq!(child.color.as_deref(), Some("1"));
    }
}
