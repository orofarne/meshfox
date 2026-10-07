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
    #[error("application graph exceeds its limit ({0})")]
    ExpansionLimit(&'static str),
    #[error("block arguments: {0}")]
    Arguments(String),
    #[error("artifact dependency: {0}")]
    Artifacts(String),
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
    #[error("node {0:?} block {1:?}: `service` and `tty` are mutually exclusive — a service is a non-interactive background process, not something handed a real terminal")]
    ServiceTtyConflict(String, String),
    #[error("node {0:?} block {1:?}: `service` and `cache` are mutually exclusive — a service never exits under normal operation, so there's no completed output for `cache` to freeze")]
    ServiceCacheConflict(String, String),
    #[error("node {0:?} block {1:?}: a `button` fence can't also carry `{2}` — it has no real code of its own to run under it")]
    ButtonAttrConflict(String, String, &'static str),
    #[error("node {0:?} block {1:?}: a `form` fence can't also carry `{2}` — it has no real code of its own to run under it, and no \"done\" state for a dependency/dependent edge to mean anything")]
    FormAttrConflict(String, String, &'static str),
    #[error("node {0:?} block {1:?}: `autorun` and `tty` are mutually exclusive — there's no human to hand a terminal to when a variable changes unattended")]
    AutorunTtyConflict(String, String),
    #[error("node {0:?} block {1:?}: `send=` only means anything on a `form` fence")]
    SendWithoutForm(String, String),
    #[error("node {0:?} block {1:?}: `output-attrs=` only means anything with `output=\"image\"`")]
    OutputAttrsWithoutImage(String, String),
    #[error("node {0:?} block {1:?}: invalid output-attrs={2:?} — expected the image-attribute grammar, e.g. `width=50% bg=#fff` (`width=`/`height=` an integer or integer%, `bg=` `#rgb`/`#rrggbb`/`transparent`)")]
    InvalidOutputAttrs(String, String, String),
    #[error("node {0:?} block {1:?}: unknown render={2:?} — known kinds are {3:?}")]
    UnknownRenderKind(String, String, String, &'static [&'static str]),
    #[error("node {0:?} block {1:?}: `deps=` names {2:?}, a `form` fence — a form has no \"done\" state, so nothing can depend on it")]
    FormCannotBeDepsTarget(String, String, BlockAddr),
    #[error(transparent)]
    Vars(#[from] crate::vars::VarsError),
    #[error(transparent)]
    Form(#[from] crate::form::FormError),
}

pub fn resolve_ref(owner_node_id: &str, r: &BlockRef) -> BlockAddr {
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
    visit(
        canvas,
        target,
        &decls,
        true,
        &mut order,
        &mut visited,
        &mut stack,
    )?;
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
    visit(
        canvas,
        target,
        &decls,
        false,
        &mut order,
        &mut visited,
        &mut stack,
    )?;
    Ok(order)
}

/// Fetches the `CodeBlock` `addr` addresses, owned — used by `visit` for
/// the node currently being expanded.
pub fn find_block(canvas: &Canvas, addr: &BlockAddr) -> Result<crate::fence::CodeBlock, DepsError> {
    find_block_with_values(canvas, addr, &canvas.artifact_values)
}

fn find_block_with_values(
    canvas: &Canvas,
    addr: &BlockAddr,
    values: &HashMap<String, String>,
) -> Result<crate::fence::CodeBlock, DepsError> {
    let node = canvas
        .node(&addr.node_id)
        .ok_or_else(|| DepsError::BlockNotFound(addr.node_id.clone(), addr.block_name.clone()))?;
    let app = crate::args::Application::parse(&addr.block_name).map_err(DepsError::Arguments)?;
    if !scan_runnable_blocks(&addr.node_id, &node.text)
        .iter()
        .any(|b| b.name.as_deref() == Some(app.definition.as_str()))
    {
        return Err(DepsError::BlockNotFound(
            addr.node_id.clone(),
            addr.block_name.clone(),
        ));
    }
    let block = crate::args::bind_block(&addr.node_id, &node.text, &addr.block_name, values)
        .map_err(DepsError::Arguments)?;
    crate::vars::validate_selected_env(canvas, &addr.node_id, &block)?;
    Ok(block)
}

const MAX_APPLICATION_DEPTH: usize = 128;
const MAX_APPLICATIONS: usize = 4096;

fn check_expansion(depth: usize, applications: usize) -> Result<(), DepsError> {
    if depth >= MAX_APPLICATION_DEPTH {
        return Err(DepsError::ExpansionLimit("128 nested applications"));
    }
    if applications >= MAX_APPLICATIONS {
        return Err(DepsError::ExpansionLimit("4096 applications"));
    }
    Ok(())
}

/// Resolve one edge in the caller's lexical scope. A computed global whose
/// source has not been observed leaves the edge pending; the runner replans
/// after that source completes. All other missing references remain errors.
fn application_ref(
    canvas: &Canvas,
    node_id: &str,
    owner: &crate::fence::CodeBlock,
    dep: &BlockRef,
    values: &HashMap<String, String>,
    decls: &[VarDecl],
) -> Result<Option<BlockAddr>, DepsError> {
    let addr = resolve_ref(node_id, dep);
    let app = crate::args::Application::parse(&addr.block_name).map_err(DepsError::Arguments)?;
    let mut scope = canvas.artifact_values.clone();
    scope.extend(values.clone());
    scope.extend(owner.arguments.iter().map(|(k, v)| (k.clone(), v.clone())));
    if app.bindings.values().any(|binding| match binding {
        crate::args::Binding::Reference(name) => {
            !scope.contains_key(name)
                && decls
                    .iter()
                    .any(|decl| decl.name == *name && decl.from.is_some())
        }
        _ => false,
    }) {
        // Schema validation still catches unknown/missing arguments and bad literals.
        validate_application_ref(canvas, node_id, owner, dep, decls)?;
        return Ok(None);
    }
    let bound = find_block_with_values(canvas, &addr, &scope)?;
    Ok(Some(BlockAddr::new(addr.node_id, bound.name.unwrap())))
}

/// Validate a function's edge without inventing values for its mandatory args.
fn validate_application_ref(
    canvas: &Canvas,
    node_id: &str,
    owner: &crate::fence::CodeBlock,
    dep: &BlockRef,
    decls: &[VarDecl],
) -> Result<crate::fence::CodeBlock, DepsError> {
    let addr = resolve_ref(node_id, dep);
    let app = crate::args::Application::parse(&addr.block_name).map_err(DepsError::Arguments)?;
    let node = canvas
        .node(&addr.node_id)
        .ok_or_else(|| DepsError::BlockNotFound(addr.node_id.clone(), app.definition.clone()))?;
    let block = scan_runnable_blocks(&node.id, &node.text)
        .into_iter()
        .find(|block| block.name.as_deref() == Some(&app.definition))
        .ok_or_else(|| DepsError::BlockNotFound(addr.node_id.clone(), app.definition.clone()))?;
    let signatures = crate::args::scan_signatures(&node.id, &node.text)
        .map_err(|e| DepsError::Arguments(e.to_string()))?;
    let args = signatures
        .iter()
        .find(|sig| sig.block.span == block.span)
        .map(|sig| sig.args.as_slice())
        .unwrap_or_default();
    let owner_node = canvas.node(node_id).unwrap();
    let local: HashSet<_> = crate::args::scan_signatures(node_id, &owner_node.text)
        .map_err(|e| DepsError::Arguments(e.to_string()))?
        .into_iter()
        .filter(|sig| sig.block.span == owner.span)
        .flat_map(|sig| sig.args.into_iter().map(|arg| arg.name))
        .collect();
    for (name, binding) in &app.bindings {
        let arg = args.iter().find(|arg| arg.name == *name).ok_or_else(|| {
            DepsError::Arguments(format!(
                "unknown argument {name:?} for {:?}",
                app.definition
            ))
        })?;
        match binding {
            crate::args::Binding::Literal(value) => {
                crate::args::canonical_value(arg, value).map_err(DepsError::Arguments)?;
            }
            crate::args::Binding::Reference(name) => {
                if !local.contains(name) && !decls.iter().any(|decl| decl.name == *name) {
                    return Err(DepsError::Arguments(format!(
                        "unknown argument reference ${name} in {node_id}/{}",
                        owner.name.as_deref().unwrap_or("")
                    )));
                }
            }
        }
    }
    for arg in args {
        if arg.is_required() && !app.bindings.contains_key(&arg.name) {
            return Err(DepsError::Arguments(format!(
                "missing required argument {:?} for {:?}",
                arg.name, app.definition
            )));
        }
    }
    Ok(block)
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
/// artifact path variable references.
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
    let artifact_names = crate::artifacts::var_refs(block);
    let dependency_names = crate::args::dependency_refs(block);
    let env_names = block
        .env
        .iter()
        .map(|e| e.var_name.as_str())
        .chain(interpreter_names.iter().map(String::as_str))
        .chain(artifact_names.iter().map(String::as_str))
        .chain(dependency_names.iter().map(String::as_str));
    let selected = crate::args::selected_env_var_names(block);
    crate::vars::close_over_var_refs(
        decls,
        env_names.filter(|name| !block.arguments.contains_key(*name))
            .chain(selected.iter().map(String::as_str)),
    )
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
    let block = find_block(canvas, &addr)?;
    let addr = BlockAddr::new(&addr.node_id, block.name.as_deref().unwrap());
    let key = addr.key();
    if visited.contains(&key) {
        return Ok(());
    }
    if let Some(pos) = stack.iter().position(|a| a.key() == key) {
        let mut cycle = stack[pos..].to_vec();
        cycle.push(addr);
        return Err(DepsError::Cycle(cycle));
    }

    check_expansion(stack.len(), visited.len() + stack.len())?;
    stack.push(addr.clone());

    // Observe computed values before action dependencies, especially `!`
    // preparations whose necessity depends on those values.
    let mut dep_addrs = implicit_from_deps(&addr.node_id, &block, decls);
    dep_addrs.extend(crate::artifacts::producer_deps(canvas, &addr, &block)?);
    if follow_deps {
        for dep in &block.deps {
            if let Some(dep) = application_ref(
                canvas,
                &addr.node_id,
                &block,
                dep,
                &canvas.artifact_values,
                decls,
            )? {
                dep_addrs.push(dep);
            }
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

/// Plan required runs using known values. Execution cascades only along explicit
/// `deps=` edges; implicit `from=` edges provide values, not dirty propagation.
/// Cached produced values are provisional until a source has actually run.
/// Runners must re-plan after sources complete, with their actual values and
/// the set of steps already executed, before deciding whether to skip a step.
pub fn compute_forced_reruns(
    canvas: &Canvas,
    chain: &[BlockAddr],
    fingerprint_vars: impl FnMut(
        &crate::fence::CodeBlock,
        &HashMap<String, String>,
    ) -> HashMap<String, String>,
    cached_run: impl Fn(&BlockAddr) -> Option<(String, HashMap<String, String>)>,
) -> Result<HashSet<BlockAddr>, DepsError> {
    compute_forced_reruns_after(canvas, chain, fingerprint_vars, cached_run, &HashSet::new())
}

/// Re-plan with actual execution facts. Already executed explicit dependencies
/// still force consumers even after their freshness records have been updated.
pub fn compute_forced_reruns_after(
    canvas: &Canvas,
    chain: &[BlockAddr],
    mut fingerprint_vars: impl FnMut(
        &crate::fence::CodeBlock,
        &HashMap<String, String>,
    ) -> HashMap<String, String>,
    cached_run: impl Fn(&BlockAddr) -> Option<(String, HashMap<String, String>)>,
    executed: &HashSet<BlockAddr>,
) -> Result<HashSet<BlockAddr>, DepsError> {
    let decls = crate::vars::declared_vars(canvas)?;
    let target = chain.last();
    let mut forced: HashSet<BlockAddr> = HashSet::new();
    let mut sim_computed: HashMap<String, String> = HashMap::new();
    let mut resolved_edges: HashMap<BlockAddr, Vec<(BlockAddr, bool)>> = HashMap::new();

    for addr in chain {
        let block = find_block(canvas, addr)?;
        let mut values = canvas.artifact_values.clone();
        values.extend(fingerprint_vars(&block, &sim_computed));
        let mut cascaded = false;
        let mut edges = Vec::new();
        for dep in &block.deps {
            if let Some(resolved) =
                application_ref(canvas, &addr.node_id, &block, dep, &values, &decls)?
            {
                cascaded |= forced.contains(&resolved);
                edges.push((resolved, dep.sync));
            }
        }
        resolved_edges.insert(addr.clone(), edges);
        let live_fingerprint = closure_fingerprint_with(canvas, &decls, addr, &values)?;
        let cached = cached_run(addr);
        let run_for_real = executed.contains(addr)
            || Some(addr) == target
            || block.always
            || cascaded
            || !cached
                .as_ref()
                .is_some_and(|(fp, _)| *fp == live_fingerprint);
        if run_for_real {
            forced.insert(addr.clone());
        }
        // Keep prior values as predictions, including for always sources.
        // Actual values supplied by the runner must take precedence.
        if let Some((_, produced)) = cached {
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
        for (dep, sync) in &resolved_edges[addr] {
            if *sync {
                forced.insert(dep.clone());
            }
        }
    }

    Ok(forced)
}

/// Fingerprint of `target` *and everything it depends on* — the one
/// fingerprint that answers "is a result recorded for this block still
/// valid?", both for skipping an already-run dependency
/// (`compute_forced_reruns`, the server's per-step check) and for deciding
/// whether a stored run still describes the document (`meshfox-server`'s
/// run history). Built from [`crate::fence::session_fingerprint`] of the
/// block itself (its own code, interpreter, `env=`/`deps=` references, and
/// the values in `values` of the variables it references) folded with the
/// closure fingerprints of its explicit `deps=` dependencies, recursively —
/// so editing a dependency's code, or a
/// variable value a dependency references, changes the fingerprint of every
/// block above it, which a block's own fingerprint doesn't (it only names
/// its `deps=`, not their content).
///
/// `values` is the map of variable values to fold in (only the names a
/// block references are read). The caller decides which belong: the per-step
/// skip check passes everything resolved for this run, secrets included; a
/// stored run's fingerprint is better computed only from values that can be
/// reproduced later, so it doesn't look stale the moment it ends.
///
/// `fingerprint`/`session_fingerprint` underneath stay as they are —
/// `fingerprint` is mirrored byte-for-byte in the web UI and embedded in
/// on-disk output markers. What *doesn't* fold in here is a rerun forced by
/// something a fingerprint can't see (`always`, a `!` edge): that stays
/// `compute_forced_reruns`'s cascade.
pub fn closure_fingerprint(
    canvas: &Canvas,
    target: &BlockAddr,
    values: &HashMap<String, String>,
) -> Result<String, DepsError> {
    let decls = crate::vars::declared_vars(canvas)?;
    closure_fingerprint_with(canvas, &decls, target, values)
}

/// [`closure_fingerprint`] for a caller that already has the canvas's
/// variable declarations (`crate::vars::declared_vars`).
pub fn closure_fingerprint_with(
    canvas: &Canvas,
    decls: &[VarDecl],
    target: &BlockAddr,
    values: &HashMap<String, String>,
) -> Result<String, DepsError> {
    fn go(
        canvas: &Canvas,
        decls: &[VarDecl],
        addr: &BlockAddr,
        values: &HashMap<String, String>,
        memo: &mut HashMap<String, String>,
        stack: &mut Vec<BlockAddr>,
    ) -> Result<String, DepsError> {
        let mut scope = canvas.artifact_values.clone();
        scope.extend(values.clone());
        let block = find_block_with_values(canvas, addr, &scope)?;
        let canonical = BlockAddr::new(&addr.node_id, block.name.as_deref().unwrap());
        let addr = &canonical;
        let key = addr.key();
        if let Some(done) = memo.get(&key) {
            return Ok(done.clone());
        }
        if let Some(pos) = stack.iter().position(|a| a.key() == key) {
            let mut cycle = stack[pos..].to_vec();
            cycle.push(addr.clone());
            return Err(DepsError::Cycle(cycle));
        }
        check_expansion(stack.len(), memo.len() + stack.len())?;
        let mut deps = Vec::new();
        for dep in &block.deps {
            if let Some(dep) = application_ref(canvas, &addr.node_id, &block, dep, values, decls)? {
                deps.push(dep);
            }
        }
        deps.sort_by_key(BlockAddr::key);
        deps.dedup();
        stack.push(addr.clone());
        let mut parts = vec![
            key.clone(),
            crate::fence::session_fingerprint(&block, values),
        ];
        let mut has_artifacts = block.attrs.contains_key("inputs") || block.attrs.contains_key("outputs");
        for attr in ["inputs", "outputs"] {
            if block.attrs.contains_key(attr) {
                parts.push(crate::artifacts::fingerprint(canvas, addr, &block, values, attr, false)?);
            }
        }
        for dep in &deps {
            let fingerprint = go(canvas, decls, dep, values, memo, stack)?;
            has_artifacts |= fingerprint.len() == 64;
            parts.push(fingerprint);
        }
        stack.pop();
        let fp = if has_artifacts {
            // Preserve the full content-digest strength through explicit closures.
            let mut hash = blake3::Hasher::new();
            for part in parts {
                hash.update(&(part.len() as u64).to_le_bytes());
                hash.update(part.as_bytes());
            }
            hash.finalize().to_hex().to_string()
        } else {
            crate::fence::combine_fingerprints(parts)
        };
        memo.insert(key, fp.clone());
        Ok(fp)
    }
    go(
        canvas,
        decls,
        target,
        values,
        &mut HashMap::new(),
        &mut Vec::new(),
    )
}

/// Validates every `deps=` reference in the whole canvas resolves to a real
/// block, that the graph has no cycles, and that no node has more than one
/// `default` block (see `crate::fence::default_block`) — used by `meshfox
/// check`.
pub fn validate(canvas: &Canvas) -> Result<(), DepsError> {
    crate::artifacts::validate(canvas)?;
    let decls = crate::vars::declared_vars(canvas)?;
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
                return Err(DepsError::AutocloseWithoutTty(
                    node.id.clone(),
                    name.clone(),
                ));
            }
            if block.service && block.tty {
                return Err(DepsError::ServiceTtyConflict(node.id.clone(), name.clone()));
            }
            if block.service && block.cache {
                return Err(DepsError::ServiceCacheConflict(
                    node.id.clone(),
                    name.clone(),
                ));
            }
            if block.autorun && block.tty {
                return Err(DepsError::AutorunTtyConflict(node.id.clone(), name.clone()));
            }
            if block.attrs.contains_key("send") && !crate::exec::is_form(&block.lang) {
                return Err(DepsError::SendWithoutForm(node.id.clone(), name.clone()));
            }
            if let Some(attrs) = block.attrs.get("output-attrs") {
                if block.attrs.get("output").map(String::as_str) != Some("image") {
                    return Err(DepsError::OutputAttrsWithoutImage(
                        node.id.clone(),
                        name.clone(),
                    ));
                }
                if crate::image_attrs::parse_inner(attrs).is_none() {
                    return Err(DepsError::InvalidOutputAttrs(
                        node.id.clone(),
                        name.clone(),
                        attrs.clone(),
                    ));
                }
            }
            if let Some(kind) = &block.render {
                if !crate::fence::RENDER_KINDS.contains(&kind.as_str()) {
                    return Err(DepsError::UnknownRenderKind(
                        node.id.clone(),
                        name.clone(),
                        kind.clone(),
                        crate::fence::RENDER_KINDS,
                    ));
                }
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
                } else if block.service {
                    Some("service")
                } else if block.autorun {
                    Some("autorun")
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
            if crate::exec::is_form(&block.lang) {
                let conflict = if block.interpreter.is_some() {
                    Some("interpreter")
                } else if block.cache {
                    Some("cache")
                } else if !block.env.is_empty() {
                    Some("env")
                } else if block.tty {
                    Some("tty")
                } else if block.service {
                    Some("service")
                } else if block.autorun {
                    Some("autorun")
                } else if !block.deps.is_empty() {
                    Some("deps")
                } else {
                    None
                };
                if let Some(attr) = conflict {
                    return Err(DepsError::FormAttrConflict(
                        node.id.clone(),
                        name.clone(),
                        attr,
                    ));
                }
                crate::form::form_block(block)?;
            }
            for dep in &block.deps {
                let dep_addr = resolve_ref(&node.id, dep);
                let dep_block = validate_application_ref(canvas, &node.id, block, dep, &decls)?;
                if crate::exec::is_form(&dep_block.lang) {
                    return Err(DepsError::FormCannotBeDepsTarget(
                        node.id.clone(),
                        name.clone(),
                        dep_addr,
                    ));
                }
            }
            // Parameterized definitions are not applications; mandatory arguments
            // are checked when a concrete application is requested.
            let has_unbound_refs = block.deps.iter().any(|dep| {
                crate::args::Application::parse(&dep.block_name).is_ok_and(|app| {
                    app.bindings.values().any(|binding| matches!(binding, crate::args::Binding::Reference(name) if !canvas.artifact_values.contains_key(name)))
                })
            });
            if !has_unbound_refs
                && !crate::args::scan_signatures(&node.id, &node.text)
                    .map_err(|e| DepsError::Arguments(e.to_string()))?
                    .iter()
                    .any(|sig| {
                        sig.block.span == block.span && sig.args.iter().any(|arg| arg.is_required())
                    })
            {
                resolve_chain(canvas, BlockAddr::new(node.id.clone(), name.clone()))?;
            }
        }
    }
    Ok(())
}

/// Authored confirmation gates for the complete resolved application chain.
/// Check before executing any step, including steps that precede the gated one.
pub fn confirmation_blocks(canvas: &Canvas, chain: &[BlockAddr]) -> Vec<String> {
    chain
        .iter()
        .filter_map(|addr| {
            find_block(canvas, addr)
                .ok()
                .filter(|block| block.requires_confirmation())
                .map(|_| format!("{}/{}", addr.node_id, addr.block_name))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn canvas(md: &str) -> Canvas {
        Canvas::from_markdown(md).unwrap()
    }

    #[test]
    fn closure_fingerprint_changes_when_a_dependency_or_a_referenced_value_does() {
        let doc = |dep_code: &str| {
            format!(
                "# P\n\n## T\n<!-- meshfox:node id=\"t\" -->\n\n```bash name=\"build\"\n{dep_code}\n```\n\n```bash name=\"test\" deps=\"build\" env=\"$X\"\necho test\n```\n"
            )
        };
        let addr = BlockAddr::new("t", "test");
        let fp = |d: &str, x: &str| {
            let canvas = Canvas::from_markdown(&doc(d)).unwrap();
            let values = HashMap::from([("X".to_string(), x.to_string())]);
            closure_fingerprint(&canvas, &addr, &values).unwrap()
        };
        let base = fp("echo a", "1");
        assert_eq!(base, fp("echo a", "1"));
        assert_ne!(base, fp("echo b", "1"), "a dependency's code must count");
        assert_ne!(
            base,
            fp("echo a", "2"),
            "a referenced variable's value must count"
        );
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
    fn validate_catches_service_and_tty_on_the_same_block() {
        let c = canvas(concat!(
            "# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
            "```bash name=\"srv\" service tty\nnpm run start\n```\n",
        ));
        assert_eq!(
            validate(&c).unwrap_err(),
            DepsError::ServiceTtyConflict("root".to_string(), "srv".to_string())
        );
    }

    #[test]
    fn validate_catches_service_and_cache_on_the_same_block() {
        let c = canvas(concat!(
            "# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
            "```bash name=\"srv\" service cache\nnpm run start\n```\n",
        ));
        assert_eq!(
            validate(&c).unwrap_err(),
            DepsError::ServiceCacheConflict("root".to_string(), "srv".to_string())
        );
    }

    #[test]
    fn validate_catches_service_and_autoclose_on_the_same_block_via_the_autoclose_check() {
        // No dedicated service+autoclose error: `autoclose` on a non-`tty`
        // block is already rejected unconditionally, and `service` can
        // never validly carry `tty` (ServiceTtyConflict) — so this
        // combination is already unreachable without tripping one of those
        // two existing checks first.
        let c = canvas(concat!(
            "# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
            "```bash name=\"srv\" service autoclose\nnpm run start\n```\n",
        ));
        assert_eq!(
            validate(&c).unwrap_err(),
            DepsError::AutocloseWithoutTty("root".to_string(), "srv".to_string())
        );
    }

    #[test]
    fn validate_catches_button_with_service() {
        let c = canvas(concat!(
            "# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
            "```button name=\"go\" service\n```\n",
        ));
        assert_eq!(
            validate(&c).unwrap_err(),
            DepsError::ButtonAttrConflict("root".to_string(), "go".to_string(), "service")
        );
    }

    #[test]
    fn validate_ok_for_a_plain_service_block() {
        let c = canvas(concat!(
            "# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
            "```bash name=\"srv\" service\nnpm run start\n```\n",
        ));
        assert!(validate(&c).is_ok());
    }

    #[test]
    fn service_block_may_be_a_deps_target_and_have_its_own_deps() {
        let c = canvas(concat!(
            "# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
            "```bash name=\"install\" cache\nnpm install\n```\n\n",
            "```bash name=\"srv\" service deps=\"install\"\nnpm run start\n```\n\n",
            "```bash name=\"smoke\" deps=\"srv\"\ncurl localhost\n```\n",
        ));
        assert!(validate(&c).is_ok());
        let chain = resolve_chain(&c, BlockAddr::new("root", "smoke")).unwrap();
        assert_eq!(
            chain,
            vec![
                BlockAddr::new("root", "install"),
                BlockAddr::new("root", "srv"),
                BlockAddr::new("root", "smoke"),
            ]
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
            vec![
                BlockAddr::new("root", "shell"),
                BlockAddr::new("root", "build")
            ]
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
            vec![
                BlockAddr::new("root", "setup"),
                BlockAddr::new("root", "run"),
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
            let fp = closure_fingerprint(&c, addr, &vars).unwrap();
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
        let migrate_fp = closure_fingerprint(&c, &migrate_addr, &vars).unwrap();
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
            let fp = closure_fingerprint(&c, addr, &vars).unwrap();
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
            let fp = closure_fingerprint(&c, addr, &vars).unwrap();
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

    /// Rechecking a computed value does not force a fresh consumer.
    #[test]
    fn compute_forced_reruns_does_not_cascade_through_an_implicit_from_edge() {
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
            let fp = closure_fingerprint(&c, addr, &vars).unwrap();
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
                BlockAddr::new("root", "validate"),
            ])
        );
    }

    #[test]
    fn validate_catches_autorun_and_tty_on_the_same_block() {
        let c = canvas(concat!(
            "# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
            "```bash name=\"shell\" tty autorun\nbash\n```\n",
        ));
        assert_eq!(
            validate(&c).unwrap_err(),
            DepsError::AutorunTtyConflict("root".to_string(), "shell".to_string())
        );
    }

    #[test]
    fn validate_ok_for_an_autorun_block_with_no_tty() {
        let c = canvas(concat!(
            "# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
            "<!-- meshfox:var name=\"X\" default=\"1\" -->\n\n",
            "```bash name=\"build\" env=\"$X\" autorun\necho build\n```\n",
        ));
        assert!(validate(&c).is_ok());
    }

    #[test]
    fn validate_catches_send_on_a_non_form_fence() {
        let c = canvas(concat!(
            "# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
            "```bash name=\"build\" send=\"Go\"\necho build\n```\n",
        ));
        assert_eq!(
            validate(&c).unwrap_err(),
            DepsError::SendWithoutForm("root".to_string(), "build".to_string())
        );
    }

    #[test]
    fn validate_ok_for_a_known_render_kind() {
        let c = canvas(concat!(
            "# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
            "```bash name=\"build\" render=\"form\"\necho build\n```\n",
        ));
        assert!(validate(&c).is_ok());
    }

    #[test]
    fn validate_catches_an_unknown_render_kind() {
        let c = canvas(concat!(
            "# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
            "```bash name=\"build\" render=\"chart\"\necho build\n```\n",
        ));
        assert_eq!(
            validate(&c).unwrap_err(),
            DepsError::UnknownRenderKind(
                "root".to_string(),
                "build".to_string(),
                "chart".to_string(),
                crate::fence::RENDER_KINDS,
            )
        );
    }

    #[test]
    fn validate_catches_output_attrs_without_output_image() {
        let c = canvas(concat!(
            "# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
            "```bash name=\"d\" cache output-attrs=\"bg=#fff\"\necho hi\n```\n",
        ));
        assert_eq!(
            validate(&c).unwrap_err(),
            DepsError::OutputAttrsWithoutImage("root".to_string(), "d".to_string())
        );
        let c = canvas(concat!(
            "# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
            "```bash name=\"d\" cache output=\"markdown\" output-attrs=\"bg=#fff\"\necho hi\n```\n",
        ));
        assert_eq!(
            validate(&c).unwrap_err(),
            DepsError::OutputAttrsWithoutImage("root".to_string(), "d".to_string())
        );
    }

    #[test]
    fn validate_catches_invalid_output_attrs_and_accepts_valid_ones() {
        let bad = canvas(concat!(
            "# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
            "```bash name=\"d\" cache output=\"image\" output-attrs=\"bg=white\"\necho hi\n```\n",
        ));
        assert_eq!(
            validate(&bad).unwrap_err(),
            DepsError::InvalidOutputAttrs(
                "root".to_string(),
                "d".to_string(),
                "bg=white".to_string()
            )
        );
        let good = canvas(concat!(
            "# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
            "```bash name=\"d\" cache output=\"image\" output-attrs=\"width=50% bg=transparent\"\necho hi\n```\n",
        ));
        assert!(validate(&good).is_ok());
    }

    #[test]
    fn validate_ok_for_a_plain_form_fence() {
        let c = canvas(concat!(
            "# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
            "<!-- meshfox:var name=\"X\" default=\"1\" -->\n\n",
            "```form name=\"pick\" send=\"Go\"\nfield var=\"X\"\n```\n",
        ));
        assert!(validate(&c).is_ok());
    }

    #[test]
    fn validate_catches_form_with_env() {
        let c = canvas(concat!(
            "<!-- meshfox:var name=\"X\" default=\"1\" -->\n",
            "# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
            "```form name=\"pick\" env=\"X\"\n```\n",
        ));
        assert_eq!(
            validate(&c).unwrap_err(),
            DepsError::FormAttrConflict("root".to_string(), "pick".to_string(), "env")
        );
    }

    #[test]
    fn validate_catches_form_with_cache() {
        let c = canvas(concat!(
            "# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
            "```form name=\"pick\" cache\n```\n",
        ));
        assert_eq!(
            validate(&c).unwrap_err(),
            DepsError::FormAttrConflict("root".to_string(), "pick".to_string(), "cache")
        );
    }

    #[test]
    fn validate_catches_form_with_tty() {
        let c = canvas(concat!(
            "# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
            "```form name=\"pick\" tty\n```\n",
        ));
        assert_eq!(
            validate(&c).unwrap_err(),
            DepsError::FormAttrConflict("root".to_string(), "pick".to_string(), "tty")
        );
    }

    #[test]
    fn validate_catches_form_with_service() {
        let c = canvas(concat!(
            "# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
            "```form name=\"pick\" service\n```\n",
        ));
        assert_eq!(
            validate(&c).unwrap_err(),
            DepsError::FormAttrConflict("root".to_string(), "pick".to_string(), "service")
        );
    }

    #[test]
    fn validate_catches_form_with_autorun() {
        let c = canvas(concat!(
            "# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
            "```form name=\"pick\" autorun\n```\n",
        ));
        assert_eq!(
            validate(&c).unwrap_err(),
            DepsError::FormAttrConflict("root".to_string(), "pick".to_string(), "autorun")
        );
    }

    #[test]
    fn validate_catches_form_with_its_own_deps() {
        let c = canvas(concat!(
            "# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
            "```bash name=\"build\" cache\necho build\n```\n\n",
            "```form name=\"pick\" deps=\"build\"\n```\n",
        ));
        assert_eq!(
            validate(&c).unwrap_err(),
            DepsError::FormAttrConflict("root".to_string(), "pick".to_string(), "deps")
        );
    }

    #[test]
    fn validate_catches_form_with_interpreter() {
        let c = canvas(concat!(
            "# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
            "```form name=\"pick\" interpreter=\"python3\"\n```\n",
        ));
        assert_eq!(
            validate(&c).unwrap_err(),
            DepsError::FormAttrConflict("root".to_string(), "pick".to_string(), "interpreter")
        );
    }

    #[test]
    fn validate_catches_a_form_fence_as_someone_elses_deps_target() {
        let c = canvas(concat!(
            "# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
            "```form name=\"pick\"\n```\n\n",
            "```bash name=\"build\" deps=\"pick\"\necho build\n```\n",
        ));
        assert_eq!(
            validate(&c).unwrap_err(),
            DepsError::FormCannotBeDepsTarget(
                "root".to_string(),
                "build".to_string(),
                BlockAddr::new("root", "pick"),
            )
        );
    }

    #[test]
    fn validate_catches_a_form_field_with_a_bad_body_line() {
        let c = canvas(concat!(
            "# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
            "```form name=\"pick\"\nnot a field line\n```\n",
        ));
        assert!(matches!(validate(&c).unwrap_err(), DepsError::Form(_)));
    }
}

#[cfg(test)]
mod application_graph_tests {
    use super::*;

    fn doc(extra: &str) -> Canvas {
        Canvas::from_markdown(&format!(
            r#"# Root
<!-- meshfox:node id="root" -->
<!-- meshfox:arg name="lang" type="select" choices="en,hy" -->
<!-- meshfox:arg name="n" type="int" default="1" -->
```bash name="download"
echo download
```
<!-- meshfox:arg name="lang" type="select" choices="en,hy" -->
```bash name="extract" deps="download[n=01,lang=$lang]"
echo extract
```
{extra}
"#
        ))
        .unwrap()
    }

    fn names(chain: &[BlockAddr]) -> Vec<&str> {
        chain.iter().map(|addr| addr.block_name.as_str()).collect()
    }

    #[test]
    fn named_edges_forward_locals_and_share_canonical_applications() {
        let canvas = doc(
            r#"```bash name="merge" deps="extract[lang=en],extract[lang=hy],download[lang=en,n=1]"
echo merge
```"#,
        );
        validate(&canvas).unwrap();
        let chain = resolve_chain(&canvas, BlockAddr::new("root", "merge")).unwrap();
        assert_eq!(
            names(&chain),
            [
                "download[lang=en,n=1]",
                "extract[lang=en]",
                "download[lang=hy,n=1]",
                "extract[lang=hy]",
                "merge"
            ]
        );
        let no_deps =
            resolve_from_chain(&canvas, BlockAddr::new("root", "extract[lang=en]")).unwrap();
        assert_eq!(names(&no_deps), ["extract[lang=en]"]);
    }

    #[test]
    fn closure_fingerprint_tracks_forwarded_arguments_and_child_code() {
        let forwarded = doc("");
        let mut literal = forwarded.clone();
        literal
            .nodes
            .iter_mut()
            .find(|n| n.id == "root")
            .unwrap()
            .text = literal
            .node("root")
            .unwrap()
            .text
            .replace("lang=$lang", "lang=hy");
        let target = BlockAddr::new("root", "extract[lang=hy]");
        // Own metadata differs, but the forwarded child's code participates.
        let before = closure_fingerprint(&forwarded, &target, &HashMap::new()).unwrap();
        assert_ne!(
            before,
            closure_fingerprint(&literal, &target, &HashMap::new()).unwrap()
        );
        let node = literal.nodes.iter_mut().find(|n| n.id == "root").unwrap();
        node.text = node.text.replace("echo download", "echo changed");
        assert_ne!(
            before,
            closure_fingerprint(&literal, &target, &HashMap::new()).unwrap()
        );
        assert_ne!(
            before,
            closure_fingerprint(
                &forwarded,
                &BlockAddr::new("root", "extract[lang=en]"),
                &HashMap::new()
            )
            .unwrap()
        );
    }

    #[test]
    fn validation_checks_uninstantiated_functions_without_placeholder_values() {
        for dep in [
            "download[lang=$TYPO]",
            "download[lang=fr]",
            "download",
            "download[lang=$lang,n=no]",
            "download[lang=$lang,wrong=1]",
        ] {
            let mut canvas = doc("");
            let node = canvas.nodes.iter_mut().find(|n| n.id == "root").unwrap();
            node.text = node.text.replace("download[n=01,lang=$lang]", dep);
            assert!(validate(&canvas).is_err(), "accepted {dep}");
        }
    }

    #[test]
    fn cycle_detection_uses_canonical_applications() {
        let canvas = Canvas::from_markdown(
            r#"# Root
<!-- meshfox:node id="root" -->
<!-- meshfox:arg name="n" type="int" -->
```bash name="x" deps="y[n=$n]"
echo x
```
<!-- meshfox:arg name="n" type="int" -->
```bash name="y" deps="x[n=01]"
echo y
```
"#,
        )
        .unwrap();
        let error = resolve_chain(&canvas, BlockAddr::new("root", "x[n=1]")).unwrap_err();
        assert_eq!(
            error,
            DepsError::Cycle(vec![
                BlockAddr::new("root", "x[n=1]"),
                BlockAddr::new("root", "y[n=1]"),
                BlockAddr::new("root", "x[n=1]")
            ])
        );
        assert!(matches!(
            closure_fingerprint(&canvas, &BlockAddr::new("root", "x[n=1]"), &HashMap::new()),
            Err(DepsError::Cycle(_))
        ));
    }

    #[test]
    fn sync_and_execution_cascade_follow_the_resolved_application() {
        let canvas = doc(r#"```bash name="target" deps="extract[lang=hy]!"
echo target
```"#);
        let chain = resolve_chain(&canvas, BlockAddr::new("root", "target")).unwrap();
        let cached: HashMap<_, _> = chain
            .iter()
            .map(|addr| {
                (
                    addr.clone(),
                    closure_fingerprint(&canvas, addr, &HashMap::new()).unwrap(),
                )
            })
            .collect();
        let cache = |addr: &BlockAddr| cached.get(addr).map(|fp| (fp.clone(), HashMap::new()));
        let forced = compute_forced_reruns(&canvas, &chain, |_, _| HashMap::new(), cache).unwrap();
        assert!(forced.contains(&BlockAddr::new("root", "extract[lang=hy]")));
        assert!(!forced.contains(&BlockAddr::new("root", "download[lang=hy,n=1]")));
        let executed = HashSet::from([BlockAddr::new("root", "download[lang=hy,n=1]")]);
        let forced =
            compute_forced_reruns_after(&canvas, &chain, |_, _| HashMap::new(), cache, &executed)
                .unwrap();
        assert!(forced.contains(&BlockAddr::new("root", "extract[lang=hy]")));
    }

    #[test]
    fn computed_bindings_observe_the_source_before_expanding_the_edge() {
        let mut canvas = doc(r#"<!-- meshfox:var name="LANG" from="observe" -->
```bash name="observe"
echo LANG=hy > "$MESHFOX_VARS_OUT"
```
```bash name="target" deps="extract[lang=$LANG]"
echo target
```"#);
        validate(&canvas).unwrap();
        let target = BlockAddr::new("root", "target");
        assert_eq!(
            names(&resolve_chain(&canvas, target.clone()).unwrap()),
            ["observe", "target"]
        );
        let needed = crate::run_chain_var_names(&canvas, &target).unwrap();
        assert!(needed.contains("LANG"));
        canvas.artifact_values.insert("LANG".into(), "hy".into());
        assert_eq!(
            names(&resolve_chain(&canvas, target.clone()).unwrap()),
            [
                "observe",
                "download[lang=hy,n=1]",
                "extract[lang=hy]",
                "target"
            ]
        );
        // Fingerprinting accepts the actual observed values even with an unseeded canvas.
        canvas.artifact_values.clear();
        let hy = closure_fingerprint(
            &canvas,
            &target,
            &HashMap::from([("LANG".into(), "hy".into())]),
        )
        .unwrap();
        let en = closure_fingerprint(
            &canvas,
            &target,
            &HashMap::from([("LANG".into(), "en".into())]),
        )
        .unwrap();
        assert_ne!(hy, en);
    }

    #[test]
    fn expansion_limits_return_errors_instead_of_overflowing_the_stack() {
        let mut md = String::from("# Root\n<!-- meshfox:node id=\"root\" -->\n");
        for n in 0..=MAX_APPLICATION_DEPTH {
            let dep = if n < MAX_APPLICATION_DEPTH {
                format!(" deps=\"n{}/x\"", n + 1)
            } else {
                String::new()
            };
            md.push_str(&format!("## Node {n}\n<!-- meshfox:node id=\"n{n}\" -->\n```bash name=\"x\"{dep}\necho x\n```\n"));
        }
        let canvas = Canvas::from_markdown(&md).unwrap();
        let target = BlockAddr::new("n0", "x");
        assert!(matches!(
            resolve_chain(&canvas, target.clone()),
            Err(DepsError::ExpansionLimit(_))
        ));
        assert!(matches!(
            closure_fingerprint(&canvas, &target, &HashMap::new()),
            Err(DepsError::ExpansionLimit(_))
        ));
        assert!(matches!(
            check_expansion(0, MAX_APPLICATIONS),
            Err(DepsError::ExpansionLimit(_))
        ));
    }
}
