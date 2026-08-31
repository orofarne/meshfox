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
use std::collections::{HashMap, HashSet};
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
    #[error("node {0:?} block {1:?}: `autoclose` only means anything on a `tty` block")]
    AutocloseWithoutTty(String, String),
    #[error("node {0:?} block {1:?}: a `button` fence can't also carry `{2}` — it has no real code of its own to run under it")]
    ButtonAttrConflict(String, String, &'static str),
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

/// Implicit `from=` dependency addresses `block` (living in node `node_id`)
/// picks up through its own `env=`/`interpreter=` — not just the names
/// literally in `block.env`: a var referenced only indirectly, through
/// another (directly-referenced) var's own `default_var`/`choices_var`,
/// still needs its `from=` source to have run first, or that other var's
/// dynamic default/choices could never be materialized (see
/// `vars::close_over_var_refs`). A block's own `interpreter=` can reference
/// a declared variable too (`$NAME` — see `crate::exec::interpreter_var_refs`)
/// and needs exactly the same treatment: running the block at all requires
/// knowing what to spawn, so a `from=`-computed interpreter path is just as
/// much an implicit dependency as one referenced via `env=`. Shared by
/// `visit` (which also walks `deps=`, gated by its own `follow_deps`) and
/// `direct_deps` (which always wants both).
fn implicit_from_deps(
    node_id: &str,
    block: &crate::fence::CodeBlock,
    decls: &[VarDecl],
) -> Vec<BlockAddr> {
    let interpreter_names = block
        .interpreter
        .as_deref()
        .map(crate::exec::interpreter_var_refs)
        .unwrap_or_default();
    let env_names = block
        .env
        .iter()
        .map(|e| e.var_name.as_str())
        .chain(interpreter_names.iter().map(String::as_str));
    crate::vars::close_over_var_refs(decls, env_names)
        .into_iter()
        .filter_map(|name| {
            decls
                .iter()
                .find(|d| d.name == name)
                .and_then(|d| d.from.as_ref())
                .map(|from| resolve_ref(node_id, from))
        })
        .collect()
}

/// Every direct dependency address `block` (living in node `node_id`) has —
/// its own `deps=` entries plus `implicit_from_deps` — regardless of
/// whether a `deps=` entry carries the `!` (`BlockRef::sync`) marker. Used
/// by `compute_forced_reruns`'s forward (dependency-forces-consumer)
/// cascade, which doesn't care how the edge got there, only that one
/// exists — unlike `visit`'s own `follow_deps`, which exists to let a
/// caller opt out of `deps=` entirely (`resolve_from_chain`), not to
/// distinguish a plain `deps=` entry from a `!` one.
fn direct_deps(node_id: &str, block: &crate::fence::CodeBlock, decls: &[VarDecl]) -> Vec<BlockAddr> {
    let mut dep_addrs: Vec<BlockAddr> = block.deps.iter().map(|d| resolve_ref(node_id, d)).collect();
    dep_addrs.extend(implicit_from_deps(node_id, block, decls));
    dep_addrs
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
    dep_addrs.extend(implicit_from_deps(&addr.node_id, &block, decls));

    for dep_addr in dep_addrs {
        visit(canvas, dep_addr, decls, follow_deps, order, visited, stack)?;
    }
    stack.pop();

    visited.insert(key);
    order.push(addr);
    Ok(())
}

/// Which of `chain`'s entries a session-freshness-aware runner (the TUI,
/// the web UI) must actually run for real, bypassing the usual "already ran
/// successfully this session and hasn't changed" skip — the last entry
/// (the block actually requested) and every `always` block are always in
/// this set, same as today. Two propagation passes ride on top of that
/// baseline:
///
/// - *Forward* (dependency forces consumer, every `deps=`/implicit `from=`
///   edge, unconditionally): if a block ends up in this set for *any*
///   reason — it's the target, `always`, its own fingerprint no longer
///   matches, or it was itself forced by this same rule — everything that
///   depends on it (directly) is forced too, transitively, the same way a
///   `make`/Bazel-style build propagates a rebuild to everything downstream
///   of a changed input. This is what keeps a plain, non-`always` step from
///   silently reusing a cached result that was only valid against whatever
///   its own dependency looked like *before* that dependency's forced
///   rerun — in particular, a consumer of an `always` block is no longer
///   the declaring block's problem to force by hand.
/// - *Backward* (consumer forces dependency, `deps=` entries marked `!` —
///   `BlockRef::sync` — only): whenever the block that declared a `!` edge
///   ends up in this set, its `!` dependency is added too, even if nothing
///   about the dependency itself (fingerprint, forward cascade) would have
///   forced it. This is the narrower, opt-in complement to the forward
///   rule: forward propagation alone can only make a dependency's forced
///   rerun spread to *more* things running more often, never make an
///   otherwise-independent, plain-fingerprint dependency (no `always`, own
///   fingerprint unchanged) rerun *less often than always* just because one
///   particular consumer needs it fresh — see `BlockRef::sync`'s own doc
///   comment for the motivating case (a migration that should reset state
///   exactly when, and only when, the load step that repopulates it is
///   about to rerun, not on every session-`always` tick).
///
/// This is a *dry run*: no block is actually executed. `fingerprint_vars`
/// computes the same var-name-keyed map `crate::fence::session_fingerprint`
/// needs for one block, given whatever `from=`-produced values this dry run
/// has simulated so far (see below) — a caller that resolves its whole
/// chain's variables up front into one flat map (the web server) can ignore
/// that second argument and return `overrides/cache/default ∪ computed`;
/// one that resolves each block's own `env=` independently (the TUI, via
/// `crate::vars::resolve_block_env`) plugs it straight in as that call's own
/// `computed` argument, mirroring the real run exactly. `cached_run` looks
/// up a block's session-freshness record (its `fingerprint` and
/// `produced_vars`, same fields `SessionRun` in `crates/server`/`crates/cli`
/// carries) — `None` if it hasn't completed successfully yet this session.
/// For any entry this dry run decides would be *skipped*, `cached_run`'s
/// `produced_vars` are folded into the simulated `computed` set feeding
/// later blocks' `fingerprint_vars` calls, mirroring exactly what the real
/// run does in that case; for an entry decided to run for real, its
/// *actual* output values aren't known yet (nothing has run) — only
/// relevant if some other block's `env=` needs a `from=` value out of a
/// block that only ends up forced to run via a `!` edge (rather than its own
/// fingerprint already having flagged it stale), a narrow enough case to
/// leave as a known gap rather than complicate this further.
pub fn compute_forced_reruns(
    canvas: &Canvas,
    chain: &[BlockAddr],
    mut fingerprint_vars: impl FnMut(&crate::fence::CodeBlock, &HashMap<String, String>) -> HashMap<String, String>,
    cached_run: impl Fn(&BlockAddr) -> Option<(String, HashMap<String, String>)>,
) -> Result<HashSet<BlockAddr>, DepsError> {
    let decls = crate::vars::declared_vars(canvas)?;
    let target = chain.last();
    let mut forced: HashSet<BlockAddr> = HashSet::new();
    let mut sim_computed: HashMap<String, String> = HashMap::new();

    for addr in chain {
        let block = find_block(canvas, addr)?;
        // Forward cascade: `chain` is topologically sorted (dependencies
        // before dependents), so every direct dependency has already been
        // decided by the time we reach `addr` — if any of them is forced,
        // `addr` is too, regardless of its own fingerprint.
        let cascaded = direct_deps(&addr.node_id, &block, &decls)
            .iter()
            .any(|dep| forced.contains(dep));
        let live_fingerprint =
            crate::fence::session_fingerprint(&block, &fingerprint_vars(&block, &sim_computed));
        let cached = cached_run(addr);
        let run_for_real = Some(addr) == target
            || block.always
            || cascaded
            || !cached
                .as_ref()
                .is_some_and(|(fp, _)| *fp == live_fingerprint);
        if run_for_real {
            forced.insert(addr.clone());
        } else if let Some((_, produced)) = cached {
            sim_computed.extend(produced);
        }
    }

    // Reverse (dependent-before-dependency) pass so a `!` edge sees its
    // declaring block's *final* decision, including anything that block
    // itself only picked up via a `!` edge from an even later consumer —
    // chained sync edges propagate transitively in one pass this way.
    for addr in chain.iter().rev() {
        if !forced.contains(addr) {
            continue;
        }
        let block = find_block(canvas, addr)?;
        for dep in &block.deps {
            if dep.sync {
                forced.insert(resolve_ref(&addr.node_id, dep));
            }
        }
    }

    Ok(forced)
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
            if block.autoclose && !block.tty {
                return Err(DepsError::AutocloseWithoutTty(node.id.clone(), name.clone()));
            }
            if crate::exec::is_button(&block.lang) {
                let conflict = if block.interpreter.is_some() {
                    Some("interpreter")
                } else if block.cache {
                    Some("cache")
                } else if !block.env.is_empty() {
                    Some("env")
                } else if block.tty {
                    Some("tty")
                } else {
                    None
                };
                if let Some(attr) = conflict {
                    return Err(DepsError::ButtonAttrConflict(
                        node.id.clone(),
                        name.clone(),
                        attr,
                    ));
                }
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
    fn validate_ok_for_a_plain_button_fence() {
        let c = canvas(concat!(
            "# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
            "```bash name=\"build\" cache\necho build\n```\n\n",
            "```button name=\"full-import\" default deps=\"build\"\nRun everything\n```\n",
        ));
        assert!(validate(&c).is_ok());
    }

    #[test]
    fn validate_catches_button_with_interpreter() {
        let c = canvas(concat!(
            "# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
            "```button name=\"go\" interpreter=\"python3\"\n```\n",
        ));
        assert_eq!(
            validate(&c).unwrap_err(),
            DepsError::ButtonAttrConflict("root".to_string(), "go".to_string(), "interpreter")
        );
    }

    #[test]
    fn validate_catches_button_with_cache() {
        let c = canvas(concat!(
            "# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
            "```button name=\"go\" cache\n```\n",
        ));
        assert_eq!(
            validate(&c).unwrap_err(),
            DepsError::ButtonAttrConflict("root".to_string(), "go".to_string(), "cache")
        );
    }

    #[test]
    fn validate_catches_button_with_env() {
        let c = canvas(concat!(
            "<!-- meshfox:var name=\"X\" default=\"1\" -->\n",
            "# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
            "```button name=\"go\" env=\"X\"\n```\n",
        ));
        assert_eq!(
            validate(&c).unwrap_err(),
            DepsError::ButtonAttrConflict("root".to_string(), "go".to_string(), "env")
        );
    }

    #[test]
    fn validate_catches_button_with_tty() {
        let c = canvas(concat!(
            "# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
            "```button name=\"go\" tty\n```\n",
        ));
        assert_eq!(
            validate(&c).unwrap_err(),
            DepsError::ButtonAttrConflict("root".to_string(), "go".to_string(), "tty")
        );
    }

    #[test]
    fn validate_catches_autoclose_on_a_non_tty_block() {
        let c = canvas(concat!(
            "# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
            "```bash name=\"build\" autoclose\necho hi\n```\n",
        ));
        assert_eq!(
            validate(&c).unwrap_err(),
            DepsError::AutocloseWithoutTty("root".to_string(), "build".to_string())
        );
    }

    #[test]
    fn validate_allows_autoclose_on_a_tty_block() {
        let c = canvas(concat!(
            "# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
            "```bash name=\"shell\" tty autoclose\nbash\n```\n",
        ));
        assert!(validate(&c).is_ok());
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
    fn chain_includes_a_from_source_reached_only_through_interpreter() {
        // `run`'s own `env=` never mentions PYTHON at all -- it's only
        // referenced via `interpreter="$PYTHON -u"` -- but `setup`'s
        // computed value is still needed before `run` can even be spawned.
        let c = canvas(concat!(
            "# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
            "<!-- meshfox:var name=\"PYTHON\" from=\"setup\" -->\n\n",
            "```bash name=\"setup\" cache\necho PYTHON=/usr/bin/python3\n```\n\n",
            "```python name=\"run\" interpreter=\"$PYTHON -u\"\nprint('hi')\n```\n",
        ));
        let chain = resolve_chain(&c, BlockAddr::new("root", "run")).unwrap();
        assert_eq!(
            chain,
            vec![BlockAddr::new("root", "setup"), BlockAddr::new("root", "run"),]
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

    /// `migrate` <-`!`- `load` <- `validate`, all fully "fresh" (every
    /// cached fingerprint matches what a dry run would compute) — nothing
    /// but the requested target (`validate`) should be forced, in
    /// particular *not* `migrate`: its declaring block (`load`) isn't
    /// itself running for real, so the `!` edge has nothing to propagate.
    #[test]
    fn compute_forced_reruns_does_not_force_a_sync_dep_when_its_declaring_block_is_skipped() {
        let c = canvas(concat!(
            "# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
            "```bash name=\"migrate\"\necho migrate\n```\n\n",
            "```bash name=\"load\" deps=\"migrate!\"\necho load\n```\n\n",
            "```bash name=\"validate\" deps=\"load\"\necho validate\n```\n",
        ));
        let chain = resolve_chain(&c, BlockAddr::new("root", "validate")).unwrap();
        let vars = HashMap::new();
        let mut cached: HashMap<BlockAddr, (String, HashMap<String, String>)> = HashMap::new();
        for addr in &chain {
            let block = find_block(&c, addr).unwrap();
            let fp = crate::fence::session_fingerprint(&block, &vars);
            cached.insert(addr.clone(), (fp, HashMap::new()));
        }
        let forced = compute_forced_reruns(
            &c,
            &chain,
            |_block, _computed| vars.clone(),
            |addr| cached.get(addr).cloned(),
        )
        .unwrap();
        assert_eq!(forced, HashSet::from([BlockAddr::new("root", "validate")]));
    }

    /// Same chain, but `load` itself has no session-freshness record yet
    /// (so it must run for real) — its `!` edge now forces `migrate` too,
    /// even though `migrate`'s own cached fingerprint still matches.
    #[test]
    fn compute_forced_reruns_forces_a_sync_dep_when_its_declaring_block_runs_for_real() {
        let c = canvas(concat!(
            "# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
            "```bash name=\"migrate\"\necho migrate\n```\n\n",
            "```bash name=\"load\" deps=\"migrate!\"\necho load\n```\n\n",
            "```bash name=\"validate\" deps=\"load\"\necho validate\n```\n",
        ));
        let chain = resolve_chain(&c, BlockAddr::new("root", "validate")).unwrap();
        let vars = HashMap::new();
        let migrate_addr = BlockAddr::new("root", "migrate");
        let migrate_block = find_block(&c, &migrate_addr).unwrap();
        let migrate_fp = crate::fence::session_fingerprint(&migrate_block, &vars);
        let mut cached: HashMap<BlockAddr, (String, HashMap<String, String>)> = HashMap::new();
        cached.insert(migrate_addr.clone(), (migrate_fp, HashMap::new()));
        // No entry for `load` at all — never run this session yet.

        let forced = compute_forced_reruns(
            &c,
            &chain,
            |_block, _computed| vars.clone(),
            |addr| cached.get(addr).cloned(),
        )
        .unwrap();
        assert_eq!(
            forced,
            HashSet::from([
                migrate_addr,
                BlockAddr::new("root", "load"),
                BlockAddr::new("root", "validate"),
            ])
        );
    }

    /// No `!` anywhere — `migrate` is plain `always`, `load` has a plain
    /// (unmarked) `deps="migrate"`. Every block's own fingerprint matches
    /// its cache, so without the forward cascade `load` would be wrongly
    /// skipped even though `migrate` just reran for real underneath it —
    /// this is the general "cascading dirtiness" case (`TODO.canvas.md`),
    /// as opposed to the two tests above, which are the narrower `!` case.
    #[test]
    fn compute_forced_reruns_cascades_forward_from_an_always_dependency_to_a_plain_consumer() {
        let c = canvas(concat!(
            "# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
            "```bash name=\"migrate\" always\necho migrate\n```\n\n",
            "```bash name=\"load\" deps=\"migrate\"\necho load\n```\n\n",
            "```bash name=\"validate\" deps=\"load\"\necho validate\n```\n",
        ));
        let chain = resolve_chain(&c, BlockAddr::new("root", "validate")).unwrap();
        let vars = HashMap::new();
        let mut cached: HashMap<BlockAddr, (String, HashMap<String, String>)> = HashMap::new();
        for addr in &chain {
            let block = find_block(&c, addr).unwrap();
            let fp = crate::fence::session_fingerprint(&block, &vars);
            cached.insert(addr.clone(), (fp, HashMap::new()));
        }
        let forced = compute_forced_reruns(
            &c,
            &chain,
            |_block, _computed| vars.clone(),
            |addr| cached.get(addr).cloned(),
        )
        .unwrap();
        assert_eq!(
            forced,
            HashSet::from([
                BlockAddr::new("root", "migrate"),
                BlockAddr::new("root", "load"),
                BlockAddr::new("root", "validate"),
            ])
        );
    }

    /// The forward cascade only follows real edges — a sibling dependency
    /// of the same consumer that doesn't itself depend on the `always`
    /// block must stay skippable.
    #[test]
    fn compute_forced_reruns_does_not_cascade_to_an_unrelated_sibling_dependency() {
        let c = canvas(concat!(
            "# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
            "```bash name=\"migrate\" always\necho migrate\n```\n\n",
            "```bash name=\"other\"\necho other\n```\n\n",
            "```bash name=\"validate\" deps=\"migrate,other\"\necho validate\n```\n",
        ));
        let chain = resolve_chain(&c, BlockAddr::new("root", "validate")).unwrap();
        let vars = HashMap::new();
        let mut cached: HashMap<BlockAddr, (String, HashMap<String, String>)> = HashMap::new();
        for addr in &chain {
            let block = find_block(&c, addr).unwrap();
            let fp = crate::fence::session_fingerprint(&block, &vars);
            cached.insert(addr.clone(), (fp, HashMap::new()));
        }
        let forced = compute_forced_reruns(
            &c,
            &chain,
            |_block, _computed| vars.clone(),
            |addr| cached.get(addr).cloned(),
        )
        .unwrap();
        assert_eq!(
            forced,
            HashSet::from([
                BlockAddr::new("root", "migrate"),
                BlockAddr::new("root", "validate"),
            ])
        );
    }

    /// The forward cascade also follows an *implicit* `from=` edge, not
    /// just an explicit `deps=` one — `deploy` never names `provision` in
    /// its own `deps=`, only reaches it through `env="$X"` plus `X`'s own
    /// `from="provision"` declaration.
    #[test]
    fn compute_forced_reruns_cascades_forward_through_an_implicit_from_edge() {
        let c = canvas(concat!(
            "# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
            "<!-- meshfox:var name=\"X\" from=\"provision\" -->\n\n",
            "```bash name=\"provision\" always\necho id=abc\n```\n\n",
            "```bash name=\"deploy\" env=\"$X\"\necho deploy\n```\n\n",
            "```bash name=\"validate\" deps=\"deploy\"\necho validate\n```\n",
        ));
        let chain = resolve_chain(&c, BlockAddr::new("root", "validate")).unwrap();
        let mut vars = HashMap::new();
        vars.insert("X".to_string(), "abc".to_string());
        let mut cached: HashMap<BlockAddr, (String, HashMap<String, String>)> = HashMap::new();
        for addr in &chain {
            let block = find_block(&c, addr).unwrap();
            let fp = crate::fence::session_fingerprint(&block, &vars);
            cached.insert(addr.clone(), (fp, HashMap::new()));
        }
        let forced = compute_forced_reruns(
            &c,
            &chain,
            |_block, _computed| vars.clone(),
            |addr| cached.get(addr).cloned(),
        )
        .unwrap();
        assert_eq!(
            forced,
            HashSet::from([
                BlockAddr::new("root", "provision"),
                BlockAddr::new("root", "deploy"),
                BlockAddr::new("root", "validate"),
            ])
        );
    }
}
