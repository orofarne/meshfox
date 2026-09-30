//! Flattening the canvas's node tree into visible rows for the TUI's left
//! pane, respecting which nodes are currently collapsed.

use std::collections::{HashMap, HashSet};

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
    /// Whether any of this node's own blocks is a `service` (see
    /// `meshfox_core::CodeBlock::service`, SPEC.md's "Service blocks
    /// (experimental)") — same "declared in the source" flag `has_tty`
    /// already is, not a live status (that's `App.services`, cross-
    /// referenced by node id at render time in `ui::tree_row_words` since
    /// it changes independently of the document — see that function's own
    /// comment for why it isn't a field here).
    pub has_service: bool,
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
    /// A one-hop extra-edge link shown below its source. `node_id` points
    /// to the real target, but this row never recursively visits it.
    pub reference: Option<ReferenceRow>,
}

#[derive(Clone, PartialEq, Eq)]
pub struct ReferenceRow {
    pub source_id: String,
    pub edge_index: usize,
    pub label: Option<String>,
}

type OutgoingLinks = HashMap<String, Vec<(String, usize, Option<String>)>>;

pub fn flatten(canvas: &Canvas, expanded: &HashSet<String>) -> Vec<TreeRow> {
    let mut rows = Vec::new();
    let mut outgoing: OutgoingLinks = HashMap::new();
    for target in &canvas.nodes {
        for (edge_index, edge) in target.extra_parents.iter().enumerate() {
            outgoing.entry(edge.from.clone()).or_default().push((
                target.id.clone(),
                edge_index,
                edge.label.clone(),
            ));
        }
    }
    if let Ok(root) = canvas.root() {
        visit(canvas, root, 0, expanded, &outgoing, &mut rows);
    }
    rows
}

fn visit(
    canvas: &Canvas,
    node: &meshfox_core::Node,
    depth: usize,
    expanded: &HashSet<String>,
    outgoing: &OutgoingLinks,
    rows: &mut Vec<TreeRow>,
) {
    let children = canvas.children(&node.id);
    let links = outgoing.get(&node.id);
    let has_children = !children.is_empty() || links.is_some_and(|links| !links.is_empty());
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
        has_service: blocks.iter().any(|b| b.service),
        constraint_ok,
        color: node.effective_color.clone(),
        tags: node.tags.clone(),
        reference: None,
    });

    if has_children && is_expanded {
        for child in children {
            visit(canvas, child, depth + 1, expanded, outgoing, rows);
        }
        if let Some(links) = links {
            for (target_id, edge_index, label) in links {
                let Some(target) = canvas.node(target_id) else {
                    continue;
                };
                rows.push(TreeRow {
                    node_id: target.id.clone(),
                    title: target.title.clone(),
                    depth: depth + 1,
                    node_type: target.node_type,
                    has_children: false,
                    expanded: false,
                    runnable_count: 0,
                    has_cache: false,
                    has_tty: false,
                    has_service: false,
                    constraint_ok: None,
                    color: None,
                    tags: Vec::new(),
                    reference: Some(ReferenceRow {
                        source_id: node.id.clone(),
                        edge_index: *edge_index,
                        label: label.clone(),
                    }),
                });
            }
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

    #[test]
    fn extra_edges_follow_real_children_and_never_duplicate_subtrees() {
        let canvas = Canvas::from_markdown(concat!(
            "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n",
            "## Source\n<!-- meshfox:node id=\"source\" -->\n",
            "<!-- meshfox:edge from=\"target\" label=\"returns\" -->\n",
            "### Child\n<!-- meshfox:node id=\"child\" -->\n",
            "## Target\n<!-- meshfox:node id=\"target\" -->\n",
            "<!-- meshfox:edge from=\"source\" label=\"uses worker\" -->\n",
        ))
        .unwrap();
        let collapsed = flatten(&canvas, &HashSet::new());
        assert_eq!(
            collapsed
                .iter()
                .map(|r| r.node_id.as_str())
                .collect::<Vec<_>>(),
            ["root", "source", "target"]
        );
        assert!(collapsed[1].has_children);
        assert!(collapsed[2].has_children); // its outgoing reference is foldable

        let expanded = HashSet::from(["source".to_string(), "target".to_string()]);
        let rows = flatten(&canvas, &expanded);
        assert_eq!(
            rows.iter().map(|r| r.node_id.as_str()).collect::<Vec<_>>(),
            ["root", "source", "child", "target", "target", "source"]
        );
        assert!(rows[3].reference.is_some());
        assert_eq!(
            rows[3].reference.as_ref().unwrap().label.as_deref(),
            Some("uses worker")
        );
        assert!(rows[4].reference.is_none());
        assert_eq!(
            rows[5].reference.as_ref().unwrap().label.as_deref(),
            Some("returns")
        );
        assert_eq!(rows.len(), 6); // the two-way cycle does not recurse
    }
}
