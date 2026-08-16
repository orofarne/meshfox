//! Shared brain for meshfox's server and CLI: the canvas data model,
//! `.canvas.md` parsing/rendering, Markdown runnable-fence scanning,
//! cached-output rewriting, and code executors.
//!
//! See the repository README for the on-disk format and conventions (the
//! README is itself a valid meshfox document).

pub mod attrs;
pub mod canvas;
pub mod constraint;
pub mod deps;
pub mod exec;
pub mod fence;
pub mod file_read;
pub mod include;
pub mod mdcanvas;
pub mod options;
pub mod output;
pub mod staticgen;
pub mod tree;
pub mod varcache;
pub mod vars;

pub use canvas::{ArrowEnd, Canvas, EdgeLineStyle, ExtraEdge, FileDisplay, Node, NodeType};
pub use constraint::{evaluate as evaluate_constraints, ConstraintResult, ConstraintStatus};
pub use deps::{BlockAddr, DepsError};
pub use exec::{executor_for, is_supported_lang, Executor};
pub use fence::{scan_code_blocks, scan_runnable_blocks, BlockRef, CodeBlock, EnvRef};
pub use file_read::{confine, preview, ConfineError, FilePreview, PreviewError, FILE_PREVIEW_MAX_BYTES};
pub use include::IncludeError;
pub use mdcanvas::{parse_fold_override, NodeMeta, ParseError};
pub use options::{declared_options, OptionsError};
pub use output::{write_output, ExecOutput};
pub use staticgen::{Asset, EdgeView, NodeView, Position, SiteData};
pub use tree::{RunnableBlock, TreeError};
pub use varcache::VarCache;
pub use vars::{
    declared_vars, map_block_env, resolve as resolve_vars, resolve_block_env, validate_env_refs, validate_value,
    BlockEnvResolution, ResolvedVars, VarDecl, VarType, VarsError,
};

use thiserror::Error;

#[derive(Debug, Error)]
pub enum RunError {
    #[error(transparent)]
    Tree(#[from] TreeError),
    #[error("no runnable code block named {0:?} in node {1:?}")]
    BlockNotFound(String, String),
    #[error("no executor registered for language {0:?}")]
    NoExecutor(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Deps(#[from] DepsError),
}

/// Result of running one code block.
pub struct RunOutcome {
    pub result: ExecOutput,
    /// If the block opted into `cache`, the node's new full body text with
    /// the output region inserted/updated — patch this into the source
    /// file with `mdcanvas::set_node_body` (don't re-serialize the whole
    /// document, which would reformat unrelated content).
    pub updated_node_text: Option<(String, String)>,
}

/// Resolve `path` (node ids from the root's children down) to a node, and
/// run the code block named `block_name` inside it. Does not mutate
/// `canvas` or touch disk — the caller applies `updated_node_text` to the
/// actual source file, see `RunOutcome`. Only runs `block_name` itself —
/// use `resolve_run_chain` first if its `deps=` should run too.
///
/// `block_name` can be omitted from the *last* path segment instead: if
/// `path` names a node whose only runnable block shares that node's own
/// id (implicitly, or via an explicit `name=` that just happens to match),
/// passing that same trailing segment as `block_name` here resolves it —
/// see `resolve_target` and SPEC.md's "Runnable code fences".
pub fn run_block(canvas: &Canvas, path: &[&str], block_name: &str) -> Result<RunOutcome, RunError> {
    let target = resolve_target(canvas, path, block_name)?;
    run_block_by_id(canvas, &target.node_id, &target.block_name)
}

/// Same as `run_block`, but addresses the node directly by `id` instead of
/// walking a root-relative path — what `resolve_run_chain`'s output is
/// meant to be fed into, since a dependency chain is a list of `BlockAddr`
/// (node id + block name), not root-relative paths.
pub fn run_block_by_id(canvas: &Canvas, node_id: &str, block_name: &str) -> Result<RunOutcome, RunError> {
    let node = canvas
        .node(node_id)
        .ok_or_else(|| TreeError::NodeNotFound(node_id.to_string()))?;
    run_block_in(node, block_name)
}

fn run_block_in(node: &Node, block_name: &str) -> Result<RunOutcome, RunError> {
    let node_id = node.id.clone();
    let text = node.text.clone();

    let blocks = scan_runnable_blocks(&node_id, &text);
    let block = blocks
        .iter()
        .find(|b| b.name.as_deref() == Some(block_name))
        .ok_or_else(|| RunError::BlockNotFound(block_name.to_string(), node_id.clone()))?;

    let executor =
        executor_for(&block.lang).ok_or_else(|| RunError::NoExecutor(block.lang.clone()))?;
    let result = executor.run(&block.code)?;

    let updated_node_text = if block.cache {
        write_output(&text, block_name, &result).map(|updated| (node_id, updated))
    } else {
        None
    };

    Ok(RunOutcome {
        result,
        updated_node_text,
    })
}

/// Resolve `path` + `block_name` to a `BlockAddr`, then, if `with_deps` is
/// set, expand it into the full run order — every block it transitively
/// `deps=`-depends on, in dependency order, ending with the target itself
/// (see `deps::resolve_chain`). With `with_deps` false, the chain is just
/// the target alone — for a caller that wants to run *this* block only,
/// skipping whatever it `deps=` on (e.g. the web UI's plain "run" button
/// next to "run chain", or the CLI's `--no-deps`). Feed the result to
/// `run_block_by_id`, one at a time, re-parsing the canvas between steps if
/// any step's cached output is applied back to the source first (mirrors
/// how the CLI already runs a list of independently-requested blocks).
pub fn resolve_run_chain(
    canvas: &Canvas,
    path: &[&str],
    block_name: &str,
    with_deps: bool,
) -> Result<Vec<BlockAddr>, RunError> {
    let target = resolve_target(canvas, path, block_name)?;
    if with_deps {
        Ok(deps::resolve_chain(canvas, target)?)
    } else {
        Ok(vec![target])
    }
}

/// The set of declared-variable names any block in `chain` actually
/// references via its own `env=` — the union across the whole chain, not
/// necessarily every variable the document declares. Used to scope
/// resolution/prompting to only what a specific run needs (see
/// `vars::resolve_block_env`/`vars::map_block_env`) — a chain whose blocks
/// declare no `env=` at all yields an empty set, meaning nothing about
/// `meshfox:var` is even looked at for that run.
pub fn env_var_names_for_chain(canvas: &Canvas, chain: &[BlockAddr]) -> std::collections::HashSet<String> {
    let mut needed = std::collections::HashSet::new();
    for addr in chain {
        let Some(node) = canvas.node(&addr.node_id) else { continue };
        let blocks = scan_runnable_blocks(&addr.node_id, &node.text);
        if let Some(block) = blocks.iter().find(|b| b.name.as_deref() == Some(addr.block_name.as_str())) {
            needed.extend(block.env.iter().map(|er| er.var_name.clone()));
        }
    }
    needed
}

/// Resolves `path` + `block_name` to the block to actually address: tries
/// `block_name` literally, inside the node `path` resolves to (today's
/// only behavior); if that's not a runnable block there, falls back to
/// treating `block_name` as *one more path segment* — letting a node
/// addressed that way stand in for its own `default` block (see
/// `fence::default_block`: the sole implicitly- or explicitly-self-named
/// block, or one explicitly flagged `default`), so a trailing block-name
/// argument that would just repeat the node's own id can be omitted
/// entirely. Only ever takes the fallback when the literal address doesn't
/// already work, so this is fully backward compatible with every address
/// that resolved before it existed.
fn resolve_target(canvas: &Canvas, path: &[&str], block_name: &str) -> Result<BlockAddr, RunError> {
    let node = canvas.resolve_path(path)?;
    if has_runnable_block(canvas, &node.id, block_name) {
        return Ok(BlockAddr::new(node.id.clone(), block_name.to_string()));
    }

    let mut extended: Vec<&str> = path.to_vec();
    extended.push(block_name);
    if let Ok(child) = canvas.resolve_path(&extended) {
        if let Some(name) = default_block_name(canvas, &child.id) {
            return Ok(BlockAddr::new(child.id.clone(), name));
        }
    }

    Err(RunError::BlockNotFound(block_name.to_string(), node.id.clone()))
}

fn has_runnable_block(canvas: &Canvas, node_id: &str, block_name: &str) -> bool {
    canvas
        .node(node_id)
        .is_some_and(|n| scan_runnable_blocks(node_id, &n.text).iter().any(|b| b.name.as_deref() == Some(block_name)))
}

/// The name of `node_id`'s default block, if it has exactly one (see
/// `fence::default_block`). `None` both when no block qualifies and when
/// more than one does — an ambiguous node just isn't eligible for the
/// shortcut, same as any other ambiguous case here; `meshfox validate`
/// (`deps::validate`) is what actually reports the conflict.
fn default_block_name(canvas: &Canvas, node_id: &str) -> Option<String> {
    let node = canvas.node(node_id)?;
    let blocks = scan_runnable_blocks(node_id, &node.text);
    fence::default_block(node_id, &blocks).ok().flatten()?.name.clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOC: &str =
        "# Project\n\n## Tests\n<!-- meshfox:node -->\n\n```bash name=\"smoke\" cache\necho ok\n```\n";

    #[test]
    fn run_block_executes_and_reports_cached_update() {
        let canvas = Canvas::from_markdown(DOC).unwrap();
        let outcome = run_block(&canvas, &["tests"], "smoke").unwrap();
        assert_eq!(outcome.result.exit_code, 0);
        assert_eq!(outcome.result.output.trim(), "ok");

        let (node_id, updated_text) = outcome.updated_node_text.expect("cache was requested");
        assert_eq!(node_id, "tests");
        assert!(updated_text.contains("meshfox:output name=\"smoke\""));
    }

    #[test]
    fn run_block_without_cache_reports_no_update() {
        let doc =
            "# Project\n\n## Tests\n<!-- meshfox:node -->\n\n```bash name=\"smoke\"\necho ok\n```\n";
        let canvas = Canvas::from_markdown(doc).unwrap();
        let outcome = run_block(&canvas, &["tests"], "smoke").unwrap();
        assert!(outcome.updated_node_text.is_none());
    }

    #[test]
    fn run_block_by_id_matches_run_block_by_path() {
        let canvas = Canvas::from_markdown(DOC).unwrap();
        let by_path = run_block(&canvas, &["tests"], "smoke").unwrap();
        let by_id = run_block_by_id(&canvas, "tests", "smoke").unwrap();
        assert_eq!(by_path.result, by_id.result);
    }

    #[test]
    fn run_block_omits_the_trailing_name_when_it_would_just_repeat_the_node() {
        let doc = concat!(
            "# Project\n\n",
            "## Tests\n<!-- meshfox:node id=\"tests\" -->\n\n",
            "### Smoke\n<!-- meshfox:node id=\"smoke\" -->\n\n",
            "```bash\necho hi\n```\n",
        );
        let canvas = Canvas::from_markdown(doc).unwrap();
        // "smoke"'s sole fence has no name= — implicitly named "smoke".
        // `meshfox run tests smoke` (no separate trailing block name)
        // should still resolve it via the path alone.
        let outcome = run_block(&canvas, &["tests"], "smoke").unwrap();
        assert_eq!(outcome.result.output.trim(), "hi");
    }

    #[test]
    fn run_block_omits_the_trailing_name_for_an_explicit_default_flag() {
        let doc = concat!(
            "# Project\n\n",
            "## Tests\n<!-- meshfox:node id=\"tests\" -->\n\n",
            "### E2e\n<!-- meshfox:node id=\"e2e\" -->\n\n",
            "```bash name=\"prep\"\necho prep\n```\n\n",
            "```bash name=\"run\" default\necho ran\n```\n",
        );
        let canvas = Canvas::from_markdown(doc).unwrap();
        // "e2e"'s runnable block is named "run", not "e2e" — only the
        // explicit `default` flag makes `meshfox run tests e2e` resolve to
        // it without a trailing block-name argument.
        let outcome = run_block(&canvas, &["tests"], "e2e").unwrap();
        assert_eq!(outcome.result.output.trim(), "ran");
    }

    #[test]
    fn run_block_fallback_does_not_kick_in_when_the_node_has_no_default() {
        let doc = concat!(
            "# Project\n\n",
            "## Tests\n<!-- meshfox:node id=\"tests\" -->\n\n",
            "### E2e\n<!-- meshfox:node id=\"e2e\" -->\n\n",
            "```bash name=\"prep\"\necho prep\n```\n\n",
            "```bash name=\"run\"\necho ran\n```\n",
        );
        let canvas = Canvas::from_markdown(doc).unwrap();
        // Neither block is default (no flag, no name matching "e2e") — the
        // trailing block name is still required.
        assert!(run_block(&canvas, &["tests"], "e2e").is_err());
        assert_eq!(
            run_block(&canvas, &["tests", "e2e"], "run").unwrap().result.output.trim(),
            "ran"
        );
    }

    #[test]
    fn run_block_fallback_does_not_kick_in_for_an_unrelated_name() {
        let doc = concat!(
            "# Project\n\n",
            "## Tests\n<!-- meshfox:node id=\"tests\" -->\n\n",
            "### Smoke\n<!-- meshfox:node id=\"smoke\" -->\n\n",
            "```bash name=\"check\"\necho hi\n```\n",
        );
        let canvas = Canvas::from_markdown(doc).unwrap();
        // The sole block is explicitly named "check", not "smoke" — this
        // isn't the "name matches its own node" case, so no shortcut.
        assert!(run_block(&canvas, &["tests"], "smoke").is_err());
        // The real address still works, same as always.
        assert_eq!(
            run_block(&canvas, &["tests", "smoke"], "check").unwrap().result.output.trim(),
            "hi"
        );
    }

    #[test]
    fn resolve_run_chain_orders_deps_before_target() {
        let doc = concat!(
            "# Project\n\n## Tests\n<!-- meshfox:node -->\n\n",
            "```bash name=\"build\" cache\necho build\n```\n\n",
            "```bash name=\"test\" deps=\"build\"\necho test\n```\n",
        );
        let canvas = Canvas::from_markdown(doc).unwrap();
        let chain = resolve_run_chain(&canvas, &["tests"], "test", true).unwrap();
        assert_eq!(
            chain,
            vec![BlockAddr::new("tests", "build"), BlockAddr::new("tests", "test")]
        );
    }

    #[test]
    fn resolve_run_chain_with_deps_false_skips_the_chain() {
        let doc = concat!(
            "# Project\n\n## Tests\n<!-- meshfox:node -->\n\n",
            "```bash name=\"build\" cache\necho build\n```\n\n",
            "```bash name=\"test\" deps=\"build\"\necho test\n```\n",
        );
        let canvas = Canvas::from_markdown(doc).unwrap();
        let chain = resolve_run_chain(&canvas, &["tests"], "test", false).unwrap();
        assert_eq!(chain, vec![BlockAddr::new("tests", "test")]);
    }

    #[test]
    fn env_var_names_for_chain_is_empty_when_nothing_declares_env() {
        let doc = "# Project\n\n## Tests\n<!-- meshfox:node -->\n\n```bash name=\"build\"\necho build\n```\n";
        let canvas = Canvas::from_markdown(doc).unwrap();
        let chain = vec![BlockAddr::new("tests", "build")];
        assert!(env_var_names_for_chain(&canvas, &chain).is_empty());
    }

    #[test]
    fn env_var_names_for_chain_collects_the_union_across_blocks() {
        let doc = concat!(
            "# Project\n\n## Tests\n<!-- meshfox:node -->\n\n",
            "```bash name=\"a\" env=\"$X\"\necho a\n```\n\n",
            "```bash name=\"b\" env=\"LOCAL=$Y\"\necho b\n```\n",
        );
        let canvas = Canvas::from_markdown(doc).unwrap();
        let chain = vec![BlockAddr::new("tests", "a"), BlockAddr::new("tests", "b")];
        let needed = env_var_names_for_chain(&canvas, &chain);
        assert_eq!(needed, ["X".to_string(), "Y".to_string()].into_iter().collect());
    }
}
