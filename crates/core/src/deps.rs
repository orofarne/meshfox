//! Dependency graph over runnable code blocks (`deps=` on a fence).
//!
//! Independent of the node tree — a block can depend on any other block in
//! the document, not just ones in the same or a related node (see
//! `crate::fence::BlockRef`). Running a block automatically runs its full
//! transitive dependency chain first, in dependency order; see
//! `resolve_chain`.
//!
//! A block's own `env=` can also pull in an extra, *implicit* dependency:
//! any declared variable it references (directly, or indirectly through
//! another var's `default_var`/`choices_var` — see
//! `crate::vars::close_over_var_refs`) that's `from=`-computed (see
//! `crate::vars::VarDecl::from`) needs its source block to have already
//! run — `visit` folds that in as just another edge, right alongside
//! `deps=`, so cycle detection covers it for free. A `tty` block may be a
//! dependency (explicit or implicit) of any other block, `tty` or not —
//! each runner already hands the terminal/pty over at exactly that point
//! in the chain and continues once it exits.

use crate::canvas::Canvas;
use crate::fence::{scan_runnable_blocks, BlockRef};
use crate::vars::VarDecl;
use thiserror::Error;

/// Fully resolved address of a runnable block: which node it lives in, and
/// its `name` within that node.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BlockAddr {
    pub node_id: String,
    pub block_name: String,
}

impl BlockAddr {
    pub fn new(node_id: impl Into<String>, block_name: impl Into<String>) -> Self {
        BlockAddr {
            node_id: node_id.into(),
            block_name: block_name.into(),
        }
    }

    fn key(&self) -> String {
        format!("{}::{}", self.node_id, self.block_name)
    }
}

#[derive(Debug, Error, PartialEq)]
pub enum DepsError {
    #[error("no runnable block named {1:?} in node {0:?}")]
    BlockNotFound(String, String),
    #[error("dependency cycle: {}", .0.iter().map(|a| a.key()).collect::<Vec<_>>().join(" -> "))]
    Cycle(Vec<BlockAddr>),
    #[error("node {0:?} has more than one default block ({1:?}) — only one block per node may be `default` (or named after the node's own id)")]
    MultipleDefaults(String, Vec<String>),
    #[error("node {0:?} block {1:?}: `tty` and `cache` are mutually exclusive — an interactive session isn't the kind of deterministic exit-code-plus-text `cache` can save/replay")]
    CacheTtyConflict(String, String),
    #[error(transparent)]
    Vars(#[from] crate::vars::VarsError),
}

fn resolve_ref(owner_node_id: &str, r: &BlockRef) -> BlockAddr {
    BlockAddr {
        node_id: r
            .node_id
            .clone()
            .unwrap_or_else(|| owner_node_id.to_string()),
        block_name: r.block_name.clone(),
    }
}

/// Topologically-sorted list of blocks to run so that every (transitive)
/// dependency of `target` runs before it, with no duplicates — the last
/// entry is always `target` itself. Includes both `deps=` edges and
/// implicit `from=` edges (see the module doc comment). Errors on a
/// reference to a block that doesn't exist, or a dependency cycle.
pub fn resolve_chain(canvas: &Canvas, target: BlockAddr) -> Result<Vec<BlockAddr>, DepsError> {
    let decls = crate::vars::declared_vars(canvas)?;
    let mut order = Vec::new();
    let mut visited = std::collections::HashSet::new();
    let mut stack: Vec<BlockAddr> = Vec::new();
    visit(canvas, target, &decls, true, &mut order, &mut visited, &mut stack)?;
    Ok(order)
}

/// Same as `resolve_chain`, but ignoring `deps=` entirely — only `target`'s
/// transitive `from=` sources. For the `--no-deps`/`with_deps=false` case
/// (see `resolve_run_chain` in `crate::lib`): unlike a `deps=` dependency,
/// which might already have fresh cached output (a legitimate reason to
/// skip rerunning it), a `from=`-declared variable has no value at all
/// until its source block has run — skipping that edge is never a valid
/// choice, so it's included even when the caller opted out of `deps=`.
pub fn resolve_from_chain(canvas: &Canvas, target: BlockAddr) -> Result<Vec<BlockAddr>, DepsError> {
    let decls = crate::vars::declared_vars(canvas)?;
    let mut order = Vec::new();
    let mut visited = std::collections::HashSet::new();
    let mut stack: Vec<BlockAddr> = Vec::new();
    visit(canvas, target, &decls, false, &mut order, &mut visited, &mut stack)?;
    Ok(order)
}

/// Fetches the `CodeBlock` `addr` addresses, owned — used by `visit` for
/// the node currently being expanded.
fn find_block(canvas: &Canvas, addr: &BlockAddr) -> Result<crate::fence::CodeBlock, DepsError> {
    let node = canvas
        .node(&addr.node_id)
        .ok_or_else(|| DepsError::BlockNotFound(addr.node_id.clone(), addr.block_name.clone()))?;
    scan_runnable_blocks(&addr.node_id, &node.text)
        .into_iter()
        .find(|b| b.name.as_deref() == Some(addr.block_name.as_str()))
        .ok_or_else(|| DepsError::BlockNotFound(addr.node_id.clone(), addr.block_name.clone()))
}

/// `follow_deps` gates whether `block.deps` (`deps=`) edges are walked;
/// implicit `from=` edges (any declared variable `block.env` references
/// whose `VarDecl::from` is set) are always walked regardless — see
/// `resolve_chain`/`resolve_from_chain`.
fn visit(
    canvas: &Canvas,
    addr: BlockAddr,
    decls: &[VarDecl],
    follow_deps: bool,
    order: &mut Vec<BlockAddr>,
    visited: &mut std::collections::HashSet<String>,
    stack: &mut Vec<BlockAddr>,
) -> Result<(), DepsError> {
    let key = addr.key();
    if visited.contains(&key) {
        return Ok(());
    }
    if let Some(pos) = stack.iter().position(|a| a.key() == key) {
        let mut cycle = stack[pos..].to_vec();
        cycle.push(addr);
        return Err(DepsError::Cycle(cycle));
    }

    let block = find_block(canvas, &addr)?;

    stack.push(addr.clone());

    let mut dep_addrs: Vec<BlockAddr> = if follow_deps {
        block
            .deps
            .iter()
            .map(|d| resolve_ref(&addr.node_id, d))
            .collect()
    } else {
        Vec::new()
    };
    // Not just the names literally in `block.env` — a var referenced only
    // indirectly, through another (directly-referenced) var's own
    // `default_var`/`choices_var`, still needs its `from=` source to have
    // run first, or that other var's dynamic default/choices could never
    // be materialized. See `vars::close_over_var_refs`.
    let env_names = block.env.iter().map(|e| e.var_name.as_str());
    for name in crate::vars::close_over_var_refs(decls, env_names) {
        if let Some(from) = decls
            .iter()
            .find(|d| d.name == name)
            .and_then(|d| d.from.as_ref())
        {
            dep_addrs.push(resolve_ref(&addr.node_id, from));
        }
    }

    for dep_addr in dep_addrs {
        visit(canvas, dep_addr, decls, follow_deps, order, visited, stack)?;
    }
    stack.pop();

    visited.insert(key);
    order.push(addr);
    Ok(())
}

/// Validates every `deps=` reference in the whole canvas resolves to a real
/// block, that the graph has no cycles, and that no node has more than one
/// `default` block (see `crate::fence::default_block`) — used by `meshfox
/// check`.
pub fn validate(canvas: &Canvas) -> Result<(), DepsError> {
    for node in &canvas.nodes {
        let blocks = scan_runnable_blocks(&node.id, &node.text);
        if let Err(names) = crate::fence::default_block(&node.id, &blocks) {
            return Err(DepsError::MultipleDefaults(node.id.clone(), names));
        }
        for block in &blocks {
            let Some(name) = &block.name else { continue };
            if block.tty && block.cache {
                return Err(DepsError::CacheTtyConflict(node.id.clone(), name.clone()));
            }
            resolve_chain(canvas, BlockAddr::new(node.id.clone(), name.clone()))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn canvas(md: &str) -> Canvas {
        Canvas::from_markdown(md).unwrap()
    }

    #[test]
    fn chain_orders_dependencies_before_dependent() {
        let c = canvas(concat!(
            "# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
            "```bash name=\"build\" cache\necho build\n```\n\n",
            "```bash name=\"test\" deps=\"build\"\necho test\n```\n",
        ));
        let chain = resolve_chain(&c, BlockAddr::new("root", "test")).unwrap();
        assert_eq!(
            chain,
            vec![
                BlockAddr::new("root", "build"),
                BlockAddr::new("root", "test")
            ]
        );
    }

    #[test]
    fn chain_resolves_deps_on_an_implicitly_named_block() {
        // build-node's sole fence has no name= — implicitly named after
        // its own node id. Referencing it cross-node still needs the full
        // `node-id/block-name` form (bare/same-node deps syntax is
        // unaffected by this) — here that's "build-node/build-node".
        let c = canvas(concat!(
            "# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
            "## Build\n<!-- meshfox:node id=\"build-node\" -->\n\n",
            "```bash cache\necho build\n```\n\n",
            "## Deploy\n<!-- meshfox:node id=\"deploy-node\" -->\n\n",
            "```bash name=\"deploy\" deps=\"build-node/build-node\"\necho deploy\n```\n",
        ));
        let chain = resolve_chain(&c, BlockAddr::new("deploy-node", "deploy")).unwrap();
        assert_eq!(
            chain,
            vec![
                BlockAddr::new("build-node", "build-node"),
                BlockAddr::new("deploy-node", "deploy"),
            ]
        );
    }

    #[test]
    fn chain_resolves_cross_node_deps() {
        let c = canvas(concat!(
            "# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
            "## Build\n<!-- meshfox:node id=\"build-node\" -->\n\n",
            "```bash name=\"build\" cache\necho build\n```\n\n",
            "## Deploy\n<!-- meshfox:node id=\"deploy-node\" -->\n\n",
            "```bash name=\"deploy\" deps=\"build-node/build\"\necho deploy\n```\n",
        ));
        let chain = resolve_chain(&c, BlockAddr::new("deploy-node", "deploy")).unwrap();
        assert_eq!(
            chain,
            vec![
                BlockAddr::new("build-node", "build"),
                BlockAddr::new("deploy-node", "deploy"),
            ]
        );
    }

    #[test]
    fn chain_dedupes_diamond_dependencies() {
        // c depends on a and b, both of which depend on base — base must
        // appear exactly once, before everything that needs it.
        let c = canvas(concat!(
            "# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
            "```bash name=\"base\" cache\necho base\n```\n\n",
            "```bash name=\"a\" deps=\"base\"\necho a\n```\n\n",
            "```bash name=\"b\" deps=\"base\"\necho b\n```\n\n",
            "```bash name=\"c\" deps=\"a,b\"\necho c\n```\n",
        ));
        let chain = resolve_chain(&c, BlockAddr::new("root", "c")).unwrap();
        assert_eq!(chain.len(), 4);
        assert_eq!(chain.last(), Some(&BlockAddr::new("root", "c")));
        let pos = |name: &str| chain.iter().position(|a| a.block_name == name).unwrap();
        assert!(pos("base") < pos("a"));
        assert!(pos("base") < pos("b"));
        assert!(pos("a") < pos("c"));
        assert!(pos("b") < pos("c"));
    }

    #[test]
    fn detects_direct_cycle() {
        let c = canvas(concat!(
            "# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
            "```bash name=\"a\" deps=\"b\"\necho a\n```\n\n",
            "```bash name=\"b\" deps=\"a\"\necho b\n```\n",
        ));
        let err = resolve_chain(&c, BlockAddr::new("root", "a")).unwrap_err();
        assert!(matches!(err, DepsError::Cycle(_)));
    }

    #[test]
    fn detects_missing_dependency() {
        let c = canvas(concat!(
            "# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
            "```bash name=\"a\" deps=\"nope\"\necho a\n```\n",
        ));
        assert_eq!(
            resolve_chain(&c, BlockAddr::new("root", "a")).unwrap_err(),
            DepsError::BlockNotFound("root".to_string(), "nope".to_string())
        );
    }

    #[test]
    fn validate_ok_for_clean_graph() {
        let c = canvas(concat!(
            "# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
            "```bash name=\"build\" cache\necho build\n```\n\n",
            "```bash name=\"test\" deps=\"build\"\necho test\n```\n",
        ));
        assert!(validate(&c).is_ok());
    }

    #[test]
    fn validate_catches_more_than_one_default_block_in_a_node() {
        let c = canvas(concat!(
            "# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
            "```bash name=\"root\"\necho a\n```\n\n",
            "```bash name=\"other\" default\necho b\n```\n",
        ));
        assert!(
            matches!(validate(&c), Err(DepsError::MultipleDefaults(node, _)) if node == "root")
        );
    }

    #[test]
    fn validate_catches_cache_and_tty_on_the_same_block() {
        let c = canvas(concat!(
            "# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
            "```bash name=\"shell\" tty cache\nbash\n```\n",
        ));
        assert_eq!(
            validate(&c).unwrap_err(),
            DepsError::CacheTtyConflict("root".to_string(), "shell".to_string())
        );
    }

    #[test]
    fn validate_allows_interpreter_on_a_tty_block() {
        let c = canvas(concat!(
            "# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
            "```python name=\"shell\" tty interpreter=\"python3\"\npass\n```\n",
        ));
        assert!(validate(&c).is_ok());
    }

    #[test]
    fn a_non_tty_block_may_depend_on_a_tty_block() {
        // Each runner already hands the terminal/pty over at exactly this
        // point in the chain and continues once it exits -- there's
        // nothing left this restriction was actually protecting against.
        let c = canvas(concat!(
            "# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
            "```bash name=\"shell\" tty\nbash\n```\n\n",
            "```bash name=\"build\" deps=\"shell\"\necho build\n```\n",
        ));
        assert!(validate(&c).is_ok());
        let chain = resolve_chain(&c, BlockAddr::new("root", "build")).unwrap();
        assert_eq!(
            chain,
            vec![BlockAddr::new("root", "shell"), BlockAddr::new("root", "build")]
        );
    }

    #[test]
    fn resolve_chain_allows_a_tty_block_to_depend_on_another_tty_block() {
        let c = canvas(concat!(
            "# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
            "```bash name=\"a\" tty\nbash\n```\n\n",
            "```bash name=\"b\" tty deps=\"a\"\nbash\n```\n",
        ));
        assert!(validate(&c).is_ok());
        let chain = resolve_chain(&c, BlockAddr::new("root", "b")).unwrap();
        assert_eq!(
            chain,
            vec![BlockAddr::new("root", "a"), BlockAddr::new("root", "b")]
        );
    }

    #[test]
    fn validate_catches_cycle_even_if_never_directly_targeted() {
        let c = canvas(concat!(
            "# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
            "```bash name=\"a\" deps=\"b\"\necho a\n```\n\n",
            "```bash name=\"b\" deps=\"a\"\necho b\n```\n",
        ));
        assert!(validate(&c).is_err());
    }

    #[test]
    fn chain_includes_a_from_source_as_an_implicit_dependency() {
        let c = canvas(concat!(
            "# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
            "<!-- meshfox:var name=\"RESOURCE_ID\" from=\"provision\" -->\n\n",
            "```bash name=\"provision\" cache\necho id=abc\n```\n\n",
            "```bash name=\"deploy\" env=\"$RESOURCE_ID\"\necho deploy\n```\n",
        ));
        let chain = resolve_chain(&c, BlockAddr::new("root", "deploy")).unwrap();
        assert_eq!(
            chain,
            vec![
                BlockAddr::new("root", "provision"),
                BlockAddr::new("root", "deploy"),
            ]
        );
    }

    #[test]
    fn chain_includes_a_from_source_reached_only_through_choices_var() {
        // `deploy`'s own env= only names REGION -- REGIONS_LIST is only
        // reachable via REGION's own choices_var, but its `from=` source
        // still has to run before `deploy` does, or REGION could never
        // get its choices materialized.
        let c = canvas(concat!(
            "# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
            "<!-- meshfox:var name=\"REGIONS_LIST\" from=\"list-regions\" -->\n",
            "<!-- meshfox:var name=\"REGION\" type=\"select\" choices_var=\"REGIONS_LIST\" -->\n\n",
            "```bash name=\"list-regions\" cache\necho us,eu\n```\n\n",
            "```bash name=\"deploy\" env=\"$REGION\"\necho deploy\n```\n",
        ));
        let chain = resolve_chain(&c, BlockAddr::new("root", "deploy")).unwrap();
        assert_eq!(
            chain,
            vec![
                BlockAddr::new("root", "list-regions"),
                BlockAddr::new("root", "deploy"),
            ]
        );
    }

    #[test]
    fn chain_only_pulls_in_a_from_source_when_env_actually_references_it() {
        // `provision` is a `from=` target for RESOURCE_ID, but this block
        // never references RESOURCE_ID via its own env= — so it must not
        // gain an implicit dependency on it.
        let c = canvas(concat!(
            "# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
            "<!-- meshfox:var name=\"RESOURCE_ID\" from=\"provision\" -->\n\n",
            "```bash name=\"provision\" cache\necho id=abc\n```\n\n",
            "```bash name=\"unrelated\"\necho hi\n```\n",
        ));
        let chain = resolve_chain(&c, BlockAddr::new("root", "unrelated")).unwrap();
        assert_eq!(chain, vec![BlockAddr::new("root", "unrelated")]);
    }

    #[test]
    fn chain_dedupes_a_from_source_shared_with_an_explicit_dep() {
        let c = canvas(concat!(
            "# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
            "<!-- meshfox:var name=\"RESOURCE_ID\" from=\"provision\" -->\n\n",
            "```bash name=\"provision\" cache\necho id=abc\n```\n\n",
            "```bash name=\"deploy\" deps=\"provision\" env=\"$RESOURCE_ID\"\necho deploy\n```\n",
        ));
        let chain = resolve_chain(&c, BlockAddr::new("root", "deploy")).unwrap();
        assert_eq!(
            chain,
            vec![
                BlockAddr::new("root", "provision"),
                BlockAddr::new("root", "deploy"),
            ]
        );
    }

    #[test]
    fn detects_a_cycle_through_a_from_edge() {
        let c = canvas(concat!(
            "# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
            "<!-- meshfox:var name=\"X\" from=\"a\" -->\n\n",
            "```bash name=\"a\" env=\"$X\"\necho a\n```\n",
        ));
        let err = resolve_chain(&c, BlockAddr::new("root", "a")).unwrap_err();
        assert!(matches!(err, DepsError::Cycle(_)));
    }

    #[test]
    fn validate_catches_a_from_target_that_does_not_exist() {
        let c = canvas(concat!(
            "# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
            "<!-- meshfox:var name=\"X\" from=\"nope\" -->\n\n",
            "```bash name=\"a\" env=\"$X\"\necho a\n```\n",
        ));
        assert_eq!(
            validate(&c).unwrap_err(),
            DepsError::BlockNotFound("root".to_string(), "nope".to_string())
        );
    }

    #[test]
    fn resolve_from_chain_ignores_deps_but_still_includes_from_sources() {
        let c = canvas(concat!(
            "# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
            "<!-- meshfox:var name=\"RESOURCE_ID\" from=\"provision\" -->\n\n",
            "```bash name=\"build\" cache\necho build\n```\n\n",
            "```bash name=\"provision\" cache\necho id=abc\n```\n\n",
            "```bash name=\"deploy\" deps=\"build\" env=\"$RESOURCE_ID\"\necho deploy\n```\n",
        ));
        let chain = resolve_from_chain(&c, BlockAddr::new("root", "deploy")).unwrap();
        // `build` (a plain deps= dependency) is skipped, but `provision`
        // (a from= source) is not — it's the only way RESOURCE_ID could
        // ever get a value.
        assert_eq!(
            chain,
            vec![
                BlockAddr::new("root", "provision"),
                BlockAddr::new("root", "deploy"),
            ]
        );
    }

    #[test]
    fn resolve_from_chain_is_just_the_target_when_it_has_no_from_refs() {
        let c = canvas(concat!(
            "# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
            "```bash name=\"build\" cache\necho build\n```\n\n",
            "```bash name=\"deploy\" deps=\"build\"\necho deploy\n```\n",
        ));
        let chain = resolve_from_chain(&c, BlockAddr::new("root", "deploy")).unwrap();
        assert_eq!(chain, vec![BlockAddr::new("root", "deploy")]);
    }
}
