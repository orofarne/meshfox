//! Root/path resolution over the Canvas tree (`Node::parent` links).

use crate::canvas::{Canvas, Node};
use crate::fence::{scan_runnable_blocks, CodeBlock};
use thiserror::Error;

/// One runnable code block reachable from the root, paired with the
/// node-id path to reach its owning node — the same `path` shape
/// `resolve_path` takes, so `meshfox run <path...> <name>` can be built
/// straight from this. Used by `meshfox list`.
#[derive(Debug, Clone, PartialEq)]
pub struct RunnableBlock {
    pub path: Vec<String>,
    pub node_id: String,
    pub block: CodeBlock,
}

#[derive(Debug, Error, PartialEq)]
pub enum TreeError {
    #[error("canvas has no nodes")]
    Empty,
    #[error("canvas has no root node (every node has a parent — is there a cycle?)")]
    NoRoot,
    #[error("canvas has {0} candidate root nodes (nodes with no parent): {1:?} — meshfox expects exactly one")]
    MultipleRoots(usize, Vec<String>),
    #[error("node {0:?} not found")]
    NodeNotFound(String),
}

impl Canvas {
    /// The single root node: the node with no `parent`.
    pub fn root(&self) -> Result<&Node, TreeError> {
        if self.nodes.is_empty() {
            return Err(TreeError::Empty);
        }
        let mut roots: Vec<&Node> = self.nodes.iter().filter(|n| n.parent.is_none()).collect();
        match roots.len() {
            0 => Err(TreeError::NoRoot),
            1 => Ok(roots.pop().unwrap()),
            n => Err(TreeError::MultipleRoots(
                n,
                roots.iter().map(|node| node.id.clone()).collect(),
            )),
        }
    }

    /// Resolve a path of node ids, starting from the root's children, down
    /// to the addressed node. `["tests", "smoke-test"]` walks root ->
    /// child id "tests" -> child id "smoke-test".
    pub fn resolve_path(&self, path: &[&str]) -> Result<&Node, TreeError> {
        let mut current = self.root()?;
        for segment in path {
            let next = self
                .children(&current.id)
                .into_iter()
                .find(|n| n.id == *segment)
                .ok_or_else(|| TreeError::NodeNotFound(segment.to_string()))?;
            current = next;
        }
        Ok(current)
    }

    /// Every runnable code block in the canvas, in tree order (depth-first,
    /// same traversal shape `mdcanvas::render_node` uses), each paired with
    /// the node-id path to reach its owning node from the root.
    pub fn list_runnable(&self) -> Result<Vec<RunnableBlock>, TreeError> {
        let root = self.root()?;
        let mut out = Vec::new();
        self.collect_runnable(root, &mut Vec::new(), &mut out);
        Ok(out)
    }

    fn collect_runnable(&self, node: &Node, path: &mut Vec<String>, out: &mut Vec<RunnableBlock>) {
        for block in scan_runnable_blocks(&node.id, &node.text) {
            out.push(RunnableBlock {
                path: path.clone(),
                node_id: node.id.clone(),
                block,
            });
        }
        for child in self.children(&node.id) {
            path.push(child.id.clone());
            self.collect_runnable(child, path, out);
            path.pop();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(id: &str, parent: Option<&str>, text: &str) -> Node {
        Node {
            id: id.to_string(),
            title: id.to_string(),
            level: if parent.is_none() { 1 } else { 2 },
            node_type: crate::canvas::NodeType::Text,
            parent: parent.map(str::to_string),
            extra_parents: Vec::new(),
            x: None,
            y: None,
            width: None,
            height: None,
            color: None,
            fold: None,
            tags: Vec::new(),
            target: None,
            display: None,
            lang: None,
            interpreter: None,
            preview: false,
            edge_label: None,
            text: text.to_string(),
            constraint_results: Vec::new(),
            asset_base: None,
            origin_path: None,
            origin_id: None,
            plain_markdown_include: false,
        }
    }

    fn sample() -> Canvas {
        Canvas {
            nodes: vec![
                node("root", None, ""),
                node("tests", Some("root"), ""),
                node("examples", Some("tests"), ""),
                node(
                    "test1",
                    Some("examples"),
                    "```bash name=\"test1\"\necho hi\n```",
                ),
            ],
            options: Vec::new(),
        }
    }

    #[test]
    fn finds_root() {
        let c = sample();
        assert_eq!(c.root().unwrap().id, "root");
    }

    #[test]
    fn resolves_nested_path() {
        let c = sample();
        let node = c.resolve_path(&["tests", "examples", "test1"]).unwrap();
        assert_eq!(node.id, "test1");
    }

    #[test]
    fn errors_on_no_root() {
        let mut c = sample();
        c.nodes[0].parent = Some("test1".to_string());
        assert_eq!(c.root().unwrap_err(), TreeError::NoRoot);
    }

    #[test]
    fn errors_on_missing_segment() {
        let c = sample();
        assert!(c.resolve_path(&["tests", "nope"]).is_err());
    }

    #[test]
    fn list_runnable_finds_block_with_its_path() {
        let c = sample();
        let blocks = c.list_runnable().unwrap();
        assert_eq!(blocks.len(), 1);
        // `path` is the same "root's children down to (and including) the
        // owning node" convention `resolve_path` takes — ready to feed
        // straight into `meshfox run <path...> <name>`.
        assert_eq!(
            blocks[0].path,
            vec![
                "tests".to_string(),
                "examples".to_string(),
                "test1".to_string()
            ]
        );
        assert_eq!(blocks[0].node_id, "test1");
        assert_eq!(blocks[0].block.name.as_deref(), Some("test1"));
    }

    #[test]
    fn list_runnable_includes_root_level_blocks_with_empty_path() {
        let doc = "# Root\n\n```bash name=\"root-block\"\necho hi\n```\n";
        let c = Canvas::from_markdown(doc).unwrap();
        let blocks = c.list_runnable().unwrap();
        assert_eq!(blocks.len(), 1);
        assert!(blocks[0].path.is_empty());
        assert_eq!(blocks[0].node_id, "root");
    }

    #[test]
    fn list_runnable_visits_multiple_blocks_in_document_order() {
        let doc = concat!(
            "# Root\n\n",
            "## A\n<!-- meshfox:node id=\"a\" -->\n\n",
            "```bash name=\"a1\"\necho a1\n```\n\n",
            "## B\n<!-- meshfox:node id=\"b\" -->\n\n",
            "```bash name=\"b1\"\necho b1\n```\n",
        );
        let c = Canvas::from_markdown(doc).unwrap();
        let blocks = c.list_runnable().unwrap();
        let names: Vec<_> = blocks
            .iter()
            .map(|b| b.block.name.clone().unwrap())
            .collect();
        assert_eq!(names, vec!["a1", "b1"]);
        assert_eq!(blocks[0].path, vec!["a".to_string()]);
        assert_eq!(blocks[1].path, vec!["b".to_string()]);
    }
}
