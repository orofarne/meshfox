//! Shared brain for meshfox's server and CLI: the canvas data model,
//! `.canvas.md` parsing/rendering, Markdown runnable-fence scanning,
//! cached-output rewriting, and code executors.
//!
//! See the repository README for the on-disk format and conventions (the
//! README is itself a valid meshfox document).

pub mod attrs;
pub mod canvas;
pub mod comment;
pub mod constraint;
pub mod deps;
mod dotenv;
pub mod exec;
pub mod fence;
pub mod file_read;
pub mod image_attrs;
pub mod include;
pub mod locate;
pub mod mdcanvas;
pub mod options;
pub mod output;
pub mod staticgen;
pub mod subsup;
pub mod syntax_dirs;
pub mod tag_colors;
pub mod timestamp;
pub mod tree;
pub mod varcache;
pub mod varout;
pub mod vars;

pub use canvas::{ArrowEnd, Canvas, EdgeLineStyle, ExtraEdge, FileDisplay, Node, NodeType};
pub use constraint::{evaluate as evaluate_constraints, ConstraintResult, ConstraintStatus};
pub use deps::{compute_forced_reruns, BlockAddr, DepsError};
pub use exec::{
    interpreter_var_refs, is_supported_lang, resolve_command, resolve_interpreter,
    split_interpreter, ResolvedCommand,
};
pub use fence::{
    fingerprint, parse_deps_list, parse_env_list, scan_code_blocks, scan_runnable_blocks,
    session_fingerprint, BlockRef, CodeBlock, EnvRef,
};
pub use file_read::{
    confine, preview, ConfineError, FilePreview, PreviewError, FILE_PREVIEW_MAX_BYTES,
};
pub use include::IncludeError;
pub use locate::{locate_node, LocateError, LocatedNode};
pub use mdcanvas::{parse_fold_override, parse_tags, FenceAttrsPatch, NodeMeta, ParseError};
pub use options::{declared_options, OptionsError};
pub use output::{cached_output_hash, format_duration_ms, write_output, ExecOutput};
pub use staticgen::{Asset, EdgeView, NodeView, Position, SiteData};
pub use tag_colors::{
    annotate_effective_colors, declared_tag_colors, effective_color, TagColorError,
};
pub use tree::{RunnableBlock, TreeError};
pub use varcache::VarCache;
pub use varout::{
    allocate_path as allocate_vars_out_path, from_targets,
    read_and_cleanup as read_and_cleanup_vars_out, VARS_OUT_ENV,
};
pub use vars::{
    close_over_var_refs, declared_vars, map_block_env, resolve as resolve_vars,
    resolve_block_env, validate_env_refs, validate_value, validate_var_refs, validate_var_scope,
    BlockEnvResolution, ResolvedVars, VarDecl, VarType, VarsError,
};

pub use attrs::UnknownAttrError;

/// `meshfox validate`-only: fails on the first `meshfox:node`/
/// `meshfox:edge`/`meshfox:var`/`meshfox:option`/runnable-fence attribute
/// anywhere in `markdown` that its own construct doesn't recognize —
/// every other reader (`run`/`view`/`tui`/the server) keeps silently
/// accepting one it doesn't know, so a canvas written against a newer
/// format version still opens (see `attrs::UnknownAttrError`'s own doc
/// comment for why this is a separate pass rather than living in any of
/// those constructs' own parse-time error types).
pub fn validate_known_attrs(markdown: &str) -> Result<(), UnknownAttrError> {
    if let Some(e) = mdcanvas::unknown_node_edge_attr(markdown) {
        return Err(e);
    }
    if let Some(e) = vars::unknown_var_attr(markdown) {
        return Err(e);
    }
    if let Some(e) = options::unknown_option_attr(markdown) {
        return Err(e);
    }
    if let Some(e) = fence::unknown_fence_attr(markdown) {
        return Err(e);
    }
    if let Some(e) = tag_colors::unknown_tag_color_attr(markdown) {
        return Err(e);
    }
    Ok(())
}

use thiserror::Error;

#[derive(Debug, Error)]
pub enum RunError {
    #[error(transparent)]
    Tree(#[from] TreeError),
    #[error("no runnable code block named {0:?} in node {1:?}")]
    BlockNotFound(String, String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Deps(#[from] DepsError),
}

/// Resolve `path` + `block_name` to a `BlockAddr`, then, if `with_deps` is
/// set, expand it into the full run order — every block it transitively
/// `deps=`-depends on, in dependency order, ending with the target itself
/// (see `deps::resolve_chain`). With `with_deps` false, `deps=` is skipped
/// — for a caller that wants to run *this* block only (e.g. the web UI's
/// plain "run" button next to "run chain", or the CLI's `--no-deps`) — but
/// the target's transitive `from=` sources (see `vars::VarDecl::from`,
/// `deps::resolve_from_chain`) are included either way: unlike a `deps=`
/// dependency, which might already have fresh cached output, a
/// `from=`-declared variable has no value at all until its source block
/// runs, so skipping that edge is never something `--no-deps` can mean.
/// Feed the result to a real spawner (`meshfox_server::stream_exec`), one
/// step at a time, re-parsing the canvas between steps if any step's cached
/// output is applied back to the source first (mirrors how the CLI already
/// runs a list of independently-requested blocks).
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
        Ok(deps::resolve_from_chain(canvas, target)?)
    }
}

/// The set of declared-variable names any block in `chain` actually
/// references via its own `env=` — the union across the whole chain, not
/// necessarily every variable the document declares. Used to scope
/// resolution/prompting to only what a specific run needs (see
/// `vars::resolve_block_env`/`vars::map_block_env`) — a chain whose blocks
/// declare no `env=` at all yields an empty set, meaning nothing about
/// `meshfox:var` is even looked at for that run.
pub fn env_var_names_for_chain(
    canvas: &Canvas,
    chain: &[BlockAddr],
) -> std::collections::HashSet<String> {
    let mut needed = std::collections::HashSet::new();
    for addr in chain {
        let Some(node) = canvas.node(&addr.node_id) else {
            continue;
        };
        let blocks = scan_runnable_blocks(&addr.node_id, &node.text);
        if let Some(block) = blocks
            .iter()
            .find(|b| b.name.as_deref() == Some(addr.block_name.as_str()))
        {
            needed.extend(block.env.iter().map(|er| er.var_name.clone()));
            // A block's own `interpreter=` can reference a declared
            // variable too (`$NAME` — see `exec::interpreter_var_refs`),
            // and needs it resolved just as much as an `env=` reference
            // does — it's what decides what actually gets spawned.
            if let Some(spec) = &block.interpreter {
                needed.extend(exec::interpreter_var_refs(spec));
            }
        }
    }
    // Not just the names literally in each block's own env= -- a var
    // referenced only indirectly, through another (directly-referenced)
    // var's own `default_var`/`choices_var`, still needs to be resolved
    // (and, if `from=`-computed, its own source block still needs to
    // run) for that other var's dynamic default/choices to ever be
    // materialized. A malformed declaration here degrades to "no
    // closure, just the literal names" rather than erroring -- the same
    // graceful-degradation posture `vars::resolve` itself takes; a real
    // parse error is `meshfox validate`'s job to report, not this one's.
    let decls = vars::declared_vars(canvas).unwrap_or_default();
    vars::close_over_var_refs(&decls, needed.iter().map(String::as_str))
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

    Err(RunError::BlockNotFound(
        block_name.to_string(),
        node.id.clone(),
    ))
}

fn has_runnable_block(canvas: &Canvas, node_id: &str, block_name: &str) -> bool {
    canvas.node(node_id).is_some_and(|n| {
        scan_runnable_blocks(node_id, &n.text)
            .iter()
            .any(|b| b.name.as_deref() == Some(block_name))
    })
}

/// The name of `node_id`'s default block, if it has exactly one (see
/// `fence::default_block`). `None` both when no block qualifies and when
/// more than one does — an ambiguous node just isn't eligible for the
/// shortcut, same as any other ambiguous case here; `meshfox validate`
/// (`deps::validate`) is what actually reports the conflict.
fn default_block_name(canvas: &Canvas, node_id: &str) -> Option<String> {
    let node = canvas.node(node_id)?;
    let blocks = scan_runnable_blocks(node_id, &node.text);
    fence::default_block(node_id, &blocks)
        .ok()
        .flatten()?
        .name
        .clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOC: &str =
        "# Project\n\n## Tests\n<!-- meshfox:node -->\n\n```bash name=\"smoke\" cache\necho ok\n```\n";

    #[test]
    fn resolve_target_omits_the_trailing_name_when_it_would_just_repeat_the_node() {
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
        let addr = resolve_target(&canvas, &["tests"], "smoke").unwrap();
        assert_eq!(addr.node_id, "smoke");
        assert_eq!(addr.block_name, "smoke");
    }

    #[test]
    fn resolve_target_omits_the_trailing_name_for_an_explicit_default_flag() {
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
        let addr = resolve_target(&canvas, &["tests"], "e2e").unwrap();
        assert_eq!(addr.node_id, "e2e");
        assert_eq!(addr.block_name, "run");
    }

    #[test]
    fn resolve_target_fallback_does_not_kick_in_when_the_node_has_no_default() {
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
        assert!(resolve_target(&canvas, &["tests"], "e2e").is_err());
        let addr = resolve_target(&canvas, &["tests", "e2e"], "run").unwrap();
        assert_eq!(addr.node_id, "e2e");
        assert_eq!(addr.block_name, "run");
    }

    #[test]
    fn resolve_target_fallback_does_not_kick_in_for_an_unrelated_name() {
        let doc = concat!(
            "# Project\n\n",
            "## Tests\n<!-- meshfox:node id=\"tests\" -->\n\n",
            "### Smoke\n<!-- meshfox:node id=\"smoke\" -->\n\n",
            "```bash name=\"check\"\necho hi\n```\n",
        );
        let canvas = Canvas::from_markdown(doc).unwrap();
        // The sole block is explicitly named "check", not "smoke" — this
        // isn't the "name matches its own node" case, so no shortcut.
        assert!(resolve_target(&canvas, &["tests"], "smoke").is_err());
        // The real address still works, same as always.
        let addr = resolve_target(&canvas, &["tests", "smoke"], "check").unwrap();
        assert_eq!(addr.node_id, "smoke");
        assert_eq!(addr.block_name, "check");
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
            vec![
                BlockAddr::new("tests", "build"),
                BlockAddr::new("tests", "test")
            ]
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
        assert_eq!(
            needed,
            ["X".to_string(), "Y".to_string()].into_iter().collect()
        );
    }

    #[test]
    fn env_var_names_for_chain_includes_a_default_var_reference_not_in_any_env() {
        let doc = concat!(
            "# Project\n\n",
            "<!-- meshfox:var name=\"BASE\" default=\"1\" -->\n",
            "<!-- meshfox:var name=\"X\" default_var=\"BASE\" -->\n\n",
            "## Tests\n<!-- meshfox:node -->\n\n",
            "```bash name=\"build\" env=\"$X\"\necho build\n```\n",
        );
        let canvas = Canvas::from_markdown(doc).unwrap();
        let chain = vec![BlockAddr::new("tests", "build")];
        let needed = env_var_names_for_chain(&canvas, &chain);
        assert_eq!(
            needed,
            ["X".to_string(), "BASE".to_string()].into_iter().collect()
        );
    }

    // TODO.canvas.md: "Ошибка при неизвестных параметрах в validate" —
    // `validate_known_attrs` just chains each construct's own
    // `unknown_*_attr` check; these confirm it actually reaches every one
    // of them, not just node/edge.
    #[test]
    fn validate_known_attrs_is_ok_for_a_document_using_only_known_attrs() {
        assert_eq!(validate_known_attrs(DOC), Ok(()));
    }

    #[test]
    fn validate_known_attrs_catches_an_unknown_var_attribute_even_with_clean_node_attrs() {
        let doc = "# Project\n<!-- meshfox:var name=\"X\" defualt=\"1\" -->\n\nbody\n";
        let err = validate_known_attrs(doc).unwrap_err();
        assert_eq!(err.attr, "defualt");
    }

    #[test]
    fn validate_known_attrs_catches_an_unknown_option_attribute() {
        let doc = "# Project\n<!-- meshfox:option nme=\"unfold\" -->\n\nbody\n";
        let err = validate_known_attrs(doc).unwrap_err();
        assert_eq!(err.attr, "nme");
    }

    #[test]
    fn validate_known_attrs_catches_an_unknown_fence_attribute() {
        let doc =
            "# Project\n\nbody\n\n```bash name=\"build\" cach\ncargo build\n```\n";
        let err = validate_known_attrs(doc).unwrap_err();
        assert_eq!(err.attr, "cach");
    }
}
