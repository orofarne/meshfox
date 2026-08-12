//! Evaluating embedded ` ```starlark constraint ` fences: sandboxed
//! Starlark contracts over the document tree, run by `meshfox check`.
//!
//! A constraint is a fence living in some node's Markdown body (see
//! `crate::fence::scan_constraint_blocks`), not a node type of its own — a
//! node can carry zero, one, or several. Each constraint's script gets
//! `doc` — the document's root node — and `self` — the node whose body the
//! fence lives in — built fresh from the `Canvas` before every run: plain
//! Starlark values (structs, closures over a shared node list) with no
//! reference back into Rust, so there's nothing for the sandbox to leak
//! into or mutate. There's no separate "document" type — every node,
//! `doc` included, exposes `.children()`/`.descendants()`/`.node(id)`/
//! `.nodes_with_tag(tag)`, so a script scopes a check to its own subtree
//! with `self.descendants()` the same way it would reach the whole
//! document via `doc.descendants()`. `fail(msg)` records a violation
//! without stopping the script, so one constraint can report every
//! offending node in a single run instead of just the first. Evaluation is
//! bounded (tick count, callstack depth, heap size) so a buggy or
//! adversarial script can't hang or blow up whatever's calling `evaluate`
//! (`meshfox check`, or the server evaluating every constraint on every
//! canvas load).

use crate::canvas::{Canvas, Node};
use starlark::any::ProvidesStaticType;
use starlark::environment::{Globals, GlobalsBuilder, LibraryExtension, Module};
use starlark::eval::Evaluator;
use starlark::syntax::{AstModule, Dialect};

/// `Dialect::Standard` alone only allows `for`/`if` inside a `def` (as in a
/// Bazel BUILD file) — too restrictive for a short predicate script that's
/// really just "loop over some nodes and call `fail`". Everything else
/// about `Standard` (no type annotations, no f-strings, ...) stays as-is.
fn dialect() -> Dialect {
    Dialect {
        enable_top_level_stmt: true,
        ..Dialect::Standard
    }
}
use serde::{Deserialize, Serialize};
use starlark::values::none::NoneType;
use std::cell::RefCell;
use std::fmt::Write as _;

/// Result of running one embedded constraint fence's script.
#[derive(Debug, Clone, PartialEq)]
pub struct ConstraintResult {
    /// Id of the node whose body the fence lives in.
    pub node_id: String,
    /// Title of the node whose body the fence lives in.
    pub title: String,
    /// Human-readable identifier for this fence specifically, since a node
    /// can carry more than one: the explicit `name="..."` attribute (as
    /// `"<node-id>/<name>"`) if given, else just `node_id` when it's the
    /// node's only constraint, else `"<node-id>#<n>"` (1-based, in document
    /// order) to keep multiple unnamed ones apart.
    pub label: String,
    pub ok: bool,
    /// Every `fail(msg)` call the script made, in order. On a script/parse
    /// error or a resource-limit trip, this holds that one error message
    /// instead — either way, `ok` is false whenever this is non-empty.
    pub messages: Vec<String>,
}

/// One constraint fence's result, in the shape sent over `GET /api/canvas`
/// (see `crate::canvas::Node::constraint_results`) — same information as
/// `ConstraintResult` minus the node's own id/title, which the `Node` this
/// rides along on already carries.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ConstraintStatus {
    pub label: String,
    pub ok: bool,
    pub messages: Vec<String>,
}

impl From<ConstraintResult> for ConstraintStatus {
    fn from(r: ConstraintResult) -> Self {
        ConstraintStatus { label: r.label, ok: r.ok, messages: r.messages }
    }
}

/// The default identifier for a constraint fence with no explicit
/// `name="..."` — see `ConstraintResult::label`.
fn label_for(node_id: &str, name: Option<&str>, index: usize, total_in_node: usize) -> String {
    match name {
        Some(name) => format!("{node_id}/{name}"),
        None if total_in_node <= 1 => node_id.to_string(),
        None => format!("{node_id}#{index}"),
    }
}

/// Instruction-count budget per constraint run (checked periodically, not
/// per-instruction — see `starlark::eval::Evaluator::set_max_tick_count`).
/// Generous for a script that's just filtering/looping over a node list,
/// tight enough to fail fast on a real infinite loop.
const MAX_TICKS: u64 = 1_000_000;
const MAX_CALLSTACK: usize = 64;
const MAX_HEAP_BYTES: usize = 64 * 1024 * 1024;

/// Every `fail(msg)` call a running script has made so far, shared with the
/// `fail` builtin through `Evaluator::extra` (see the crate-level "Collect
/// Starlark values" pattern this mirrors).
#[derive(Debug, Default, ProvidesStaticType)]
struct Violations(RefCell<Vec<String>>);

impl Violations {
    fn push(&self, msg: String) {
        self.0.borrow_mut().push(msg);
    }
}

#[starlark::starlark_module]
fn constraint_globals(builder: &mut GlobalsBuilder) {
    /// Record a violation without stopping the script — call it once per
    /// offending node so one constraint can report all of them in one run.
    fn fail<'v>(msg: &str, eval: &mut Evaluator<'v, '_, '_>) -> anyhow::Result<NoneType> {
        eval.extra
            .expect("Violations always set before eval_module")
            .downcast_ref::<Violations>()
            .expect("extra is always a Violations")
            .push(msg.to_string());
        Ok(NoneType)
    }
}

fn globals() -> Globals {
    GlobalsBuilder::extended_by(&[LibraryExtension::StructType])
        .with(constraint_globals)
        .build()
}

/// Runs every embedded constraint fence's script against `canvas`, in
/// document order (node order, then fence order within a node). Each fence
/// gets its own fresh Starlark heap/module — nothing persists between
/// them, so one constraint's globals or state can't leak into another's.
pub fn evaluate(canvas: &Canvas) -> Vec<ConstraintResult> {
    let prelude = build_prelude(canvas);
    let globals = globals();
    let mut results = Vec::new();
    for node in &canvas.nodes {
        let blocks = crate::fence::scan_constraint_blocks(&node.text);
        let total = blocks.len();
        for (i, block) in blocks.into_iter().enumerate() {
            results.push(evaluate_one(&prelude, &globals, node, block, i + 1, total));
        }
    }
    results
}

/// Runs every embedded constraint fence's script and writes each node's
/// results into its own `constraint_results` — never set by
/// `mdcanvas::parse` itself, only by whatever consumer wants it (the
/// server, before serving `GET /api/canvas`), populated by a consumer
/// rather than by parsing.
pub fn annotate_status(canvas: &mut Canvas) {
    let mut by_node: std::collections::HashMap<String, Vec<ConstraintStatus>> = std::collections::HashMap::new();
    for result in evaluate(canvas) {
        by_node.entry(result.node_id.clone()).or_default().push(result.into());
    }
    for node in &mut canvas.nodes {
        if let Some(results) = by_node.remove(&node.id) {
            node.constraint_results = results;
        }
    }
}

fn evaluate_one(
    prelude: &str,
    globals: &Globals,
    node: &Node,
    block: crate::fence::ConstraintBlock,
    index: usize,
    total_in_node: usize,
) -> ConstraintResult {
    let node_id = node.id.clone();
    let title = node.title.clone();
    let label = label_for(&node_id, block.name.as_deref(), index, total_in_node);

    let source = format!("{prelude}self = doc.node({})\n{}\n", star_str(&node_id), block.code);
    let violations = Violations::default();

    let outcome: Result<(), String> = Module::with_temp_heap(|module| {
        let ast = AstModule::parse(&format!("{label}.star"), source, &dialect())
            .map_err(|e| e.to_string())?;
        let mut eval = Evaluator::new(&module);
        eval.set_max_callstack_size(MAX_CALLSTACK).map_err(|e| e.to_string())?;
        eval.set_max_heap_size(MAX_HEAP_BYTES).map_err(|e| e.to_string())?;
        eval.set_max_tick_count(MAX_TICKS).map_err(|e| e.to_string())?;
        eval.extra = Some(&violations);
        eval.eval_module(ast, globals).map_err(|e| e.to_string())?;
        Ok(())
    });

    let mut messages = violations.0.into_inner();
    let ok = messages.is_empty() && outcome.is_ok();
    if let Err(e) = outcome {
        messages.push(e);
    }

    ConstraintResult { node_id, title, label, ok, messages }
}

/// Starlark source defining `doc` — the document's root node — and every
/// other node in `canvas` as a read-only struct reachable from it via
/// `.children()`/`.descendants()`. There's no separate "document" type:
/// `doc` is exactly the same shape as any node a script reaches through
/// `.children()`, just the one with no `parent`. Built fresh from the
/// `Canvas` for every `evaluate` call (not held onto or reused across
/// calls), so a script can never see a stale document.
fn build_prelude(canvas: &Canvas) -> String {
    let mut out = String::new();
    out.push_str(
        "def _children_of(id):\n\
         \x20   return [n for n in _nodes if n.parent == id]\n\
         \n\
         def _descendants_of(id):\n\
         \x20   result = []\n\
         \x20   for c in _children_of(id):\n\
         \x20       result.append(c)\n\
         \x20       result += _descendants_of(c.id)\n\
         \x20   return result\n\
         \n\
         def _node_by_id(id):\n\
         \x20   for n in _nodes:\n\
         \x20       if n.id == id:\n\
         \x20           return n\n\
         \x20   return None\n\
         \n\
         def _nodes_with_tag(tag):\n\
         \x20   return [n for n in _nodes if tag in n.tags]\n\
         \n\
         def _make_node(id, title, type, parent, tags):\n\
         \x20   return struct(\n\
         \x20       id = id,\n\
         \x20       title = title,\n\
         \x20       type = type,\n\
         \x20       parent = parent,\n\
         \x20       tags = tags,\n\
         \x20       children = lambda: _children_of(id),\n\
         \x20       descendants = lambda: _descendants_of(id),\n\
         \x20       node = _node_by_id,\n\
         \x20       nodes_with_tag = _nodes_with_tag,\n\
         \x20   )\n\
         \n",
    );

    out.push_str("_nodes = [\n");
    for n in &canvas.nodes {
        let tags = n.tags.iter().map(|t| star_str(t)).collect::<Vec<_>>().join(", ");
        let parent = match &n.parent {
            Some(p) => star_str(p),
            None => "None".to_string(),
        };
        let _ = writeln!(
            out,
            "    _make_node(id={}, title={}, type={}, parent={}, tags=[{}]),",
            star_str(&n.id),
            star_str(&n.title),
            star_str(n.node_type.as_str()),
            parent,
            tags,
        );
    }
    out.push_str("]\n\n");
    out.push_str("doc = [n for n in _nodes if n.parent == None][0]\n\n");
    out
}

/// A Starlark double-quoted string literal for `s` — same escaping rules
/// as Python's, which is all a node id/title/tag should ever need since
/// they're arbitrary user text (a heading can contain a `"` or a newline).
fn star_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\x{:02x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Canvas;

    fn canvas(md: &str) -> Canvas {
        Canvas::from_markdown(md).unwrap()
    }

    #[test]
    fn a_passing_constraint_has_no_messages() {
        let c = canvas(
            "# Root\n<!-- meshfox:node id=\"root\" -->\n\n\
             ## Check\n<!-- meshfox:node id=\"check\" -->\n\n\
             ```starlark constraint\npass\n```\n",
        );
        let results = evaluate(&c);
        assert_eq!(results.len(), 1);
        assert!(results[0].ok, "{:?}", results[0].messages);
        assert!(results[0].messages.is_empty());
        assert_eq!(results[0].label, "check");
    }

    #[test]
    fn fail_records_a_message_without_stopping_the_script() {
        let c = canvas(
            "# Root\n<!-- meshfox:node id=\"root\" -->\n\n\
             ## Check\n<!-- meshfox:node id=\"check\" -->\n\n\
             ```starlark constraint\nfail(\"first\")\nfail(\"second\")\n```\n",
        );
        let results = evaluate(&c);
        assert!(!results[0].ok);
        assert_eq!(results[0].messages, vec!["first".to_string(), "second".to_string()]);
    }

    #[test]
    fn a_plain_starlark_fence_without_the_flag_is_not_a_constraint() {
        // A documentation example showing Starlark syntax, not an actual
        // check — same as an unnamed bash fence isn't runnable.
        let c = canvas(
            "# Root\n<!-- meshfox:node id=\"root\" -->\n\n\
             ## Example\n<!-- meshfox:node id=\"example\" -->\n\n\
             ```starlark\nfail(\"not actually run\")\n```\n",
        );
        assert!(evaluate(&c).is_empty());
    }

    #[test]
    fn a_constraint_fence_coexists_with_prose_and_other_content() {
        // The whole point of embedding: a node keeps its normal prose (and
        // other fences) alongside its check, rather than needing a
        // dedicated node whose body is exactly one fence.
        let c = canvas(
            "# Root\n<!-- meshfox:node id=\"root\" -->\n\n\
             ## Check\n<!-- meshfox:node id=\"check\" -->\n\n\
             Some prose explaining the rule.\n\n\
             ```bash name=\"build\"\ncargo build\n```\n\n\
             ```starlark constraint\npass\n```\n\n\
             More prose after.\n",
        );
        let results = evaluate(&c);
        assert_eq!(results.len(), 1);
        assert!(results[0].ok, "{:?}", results[0].messages);
    }

    #[test]
    fn multiple_unnamed_constraints_in_one_node_get_indexed_labels() {
        let c = canvas(
            "# Root\n<!-- meshfox:node id=\"root\" -->\n\n\
             ## Check\n<!-- meshfox:node id=\"check\" -->\n\n\
             ```starlark constraint\npass\n```\n\n\
             ```starlark constraint\nfail(\"second one\")\n```\n",
        );
        let results = evaluate(&c);
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].label, "check#1");
        assert!(results[0].ok);
        assert_eq!(results[1].label, "check#2");
        assert!(!results[1].ok);
    }

    #[test]
    fn a_named_constraint_is_labeled_node_id_slash_name() {
        let c = canvas(
            "# Root\n<!-- meshfox:node id=\"root\" -->\n\n\
             ## Check\n<!-- meshfox:node id=\"check\" -->\n\n\
             ```starlark constraint name=\"table-shape\"\npass\n```\n",
        );
        let results = evaluate(&c);
        assert_eq!(results[0].label, "check/table-shape");
    }

    #[test]
    fn nodes_with_tag_and_children_see_the_real_tree() {
        let c = canvas(
            "# Root\n<!-- meshfox:node id=\"root\" -->\n\n\
             ## Table\n<!-- meshfox:node id=\"table\" tags=\"table\" -->\n\n\
             ### Schema\n<!-- meshfox:node id=\"schema\" type=\"file\" -->\n\n[schema](schema.sql)\n\n\
             ## Check\n<!-- meshfox:node id=\"check\" -->\n\n\
             ```starlark constraint\n\
             for n in doc.nodes_with_tag(\"table\"):\n\
             \x20   files = [c for c in n.children() if c.type == \"file\"]\n\
             \x20   if len(files) != 1:\n\
             \x20       fail(n.id + \": expected exactly one file child, got \" + str(len(files)))\n\
             ```\n",
        );
        let results = evaluate(&c);
        assert!(results[0].ok, "{:?}", results[0].messages);
    }

    #[test]
    fn nodes_with_tag_catches_a_real_violation() {
        let c = canvas(
            "# Root\n<!-- meshfox:node id=\"root\" -->\n\n\
             ## Table\n<!-- meshfox:node id=\"table\" tags=\"table\" -->\n\n\
             ## Check\n<!-- meshfox:node id=\"check\" -->\n\n\
             ```starlark constraint\n\
             for n in doc.nodes_with_tag(\"table\"):\n\
             \x20   files = [c for c in n.children() if c.type == \"file\"]\n\
             \x20   if len(files) != 1:\n\
             \x20       fail(n.id + \": expected exactly one file child, got \" + str(len(files)))\n\
             ```\n",
        );
        let results = evaluate(&c);
        assert!(!results[0].ok);
        assert_eq!(results[0].messages, vec!["table: expected exactly one file child, got 0".to_string()]);
    }

    #[test]
    fn nodes_with_tag_is_available_from_any_node_not_just_doc() {
        // `nodes_with_tag`/`node` are document-wide lookups, but they're
        // the same closure on every node (not doc-specific) — calling them
        // via `self` instead of `doc` works identically.
        let c = canvas(
            "# Root\n<!-- meshfox:node id=\"root\" -->\n\n\
             ## Table\n<!-- meshfox:node id=\"table\" tags=\"table\" -->\n\n\
             ## Check\n<!-- meshfox:node id=\"check\" -->\n\n\
             ```starlark constraint\nif len(self.nodes_with_tag(\"table\")) != 1:\n\x20   fail(\"expected one\")\n```\n",
        );
        let results = evaluate(&c);
        assert!(results[0].ok, "{:?}", results[0].messages);
    }

    #[test]
    fn self_is_the_node_the_fence_lives_in() {
        let c = canvas(
            "# Root\n<!-- meshfox:node id=\"root\" -->\n\n\
             ## Check\n<!-- meshfox:node id=\"check\" -->\n\n\
             ```starlark constraint\n\
             if self.id != \"check\" or self.type != \"text\":\n\
             \x20   fail(\"self is wrong: \" + self.id)\n\
             ```\n",
        );
        let results = evaluate(&c);
        assert!(results[0].ok, "{:?}", results[0].messages);
    }

    #[test]
    fn self_descendants_scopes_a_check_to_its_own_node_s_subtree() {
        // Mirrors the real shape that motivated `descendants`: a
        // constraint typically governs the subtree of the node its fence
        // lives in (like `table-shape` living directly in `entities`,
        // above `references`/`user-data`/`app-data`), and the
        // `table`-tagged node is nested two levels under that node — a
        // plain `self.children()` wouldn't reach it, only `descendants`
        // does. A sibling subtree with its own `table` node sits outside
        // `self`, and must NOT be seen — unlike the whole-document
        // `nodes_with_tag`, `descendants` only walks `self`'s own subtree.
        let c = canvas(
            "# Root\n<!-- meshfox:node id=\"root\" -->\n\n\
             ## Check\n<!-- meshfox:node id=\"check\" -->\n\n\
             ```starlark constraint\n\
             for n in self.descendants():\n\
             \x20   if \"table\" in n.tags:\n\
             \x20       files = [c for c in n.children() if c.type == \"file\"]\n\
             \x20       if len(files) != 1:\n\
             \x20           fail(n.id + \": expected exactly one file child, got \" + str(len(files)))\n\
             ```\n\n\
             ### User Data\n<!-- meshfox:node id=\"user-data\" parent=\"check\" -->\n\n\
             #### Schedule\n<!-- meshfox:node id=\"schedule\" tags=\"table\" -->\n\n\
             ##### schema.sql\n<!-- meshfox:node id=\"schedule-schema\" type=\"file\" -->\n\n[s](s.sql)\n\n\
             ## Outside\n<!-- meshfox:node id=\"outside\" -->\n\n\
             ### Unrelated table\n<!-- meshfox:node id=\"unrelated-table\" tags=\"table\" -->\n\n\
             body\n",
        );
        let results = evaluate(&c);
        assert!(results[0].ok, "{:?}", results[0].messages);
    }

    #[test]
    fn self_descendants_still_catches_a_violation_nested_two_levels_down() {
        let c = canvas(
            "# Root\n<!-- meshfox:node id=\"root\" -->\n\n\
             ## Check\n<!-- meshfox:node id=\"check\" -->\n\n\
             ```starlark constraint\n\
             for n in self.descendants():\n\
             \x20   if \"table\" in n.tags:\n\
             \x20       files = [c for c in n.children() if c.type == \"file\"]\n\
             \x20       if len(files) != 1:\n\
             \x20           fail(n.id + \": expected exactly one file child, got \" + str(len(files)))\n\
             ```\n\n\
             ### User Data\n<!-- meshfox:node id=\"user-data\" parent=\"check\" -->\n\n\
             #### Schedule\n<!-- meshfox:node id=\"schedule\" tags=\"table\" -->\n\nbody\n",
        );
        let results = evaluate(&c);
        assert!(!results[0].ok);
        assert_eq!(results[0].messages, vec!["schedule: expected exactly one file child, got 0".to_string()]);
    }

    #[test]
    fn a_syntax_error_is_reported_as_a_failure_not_a_panic() {
        let c = canvas(
            "# Root\n<!-- meshfox:node id=\"root\" -->\n\n\
             ## Check\n<!-- meshfox:node id=\"check\" -->\n\n\
             ```starlark constraint\nthis is not valid starlark(((\n```\n",
        );
        let results = evaluate(&c);
        assert!(!results[0].ok);
        assert_eq!(results[0].messages.len(), 1);
    }

    #[test]
    fn a_runaway_loop_is_stopped_by_the_tick_limit() {
        // Starlark has no `while` (disallowed by the grammar entirely, for
        // exactly this reason) — a huge-but-finite `for` over `range()` is
        // the way to build an expensive-enough loop to trip the budget.
        let c = canvas(
            "# Root\n<!-- meshfox:node id=\"root\" -->\n\n\
             ## Check\n<!-- meshfox:node id=\"check\" -->\n\n\
             ```starlark constraint\nfor i in range(100000000):\n\x20   pass\n```\n",
        );
        let results = evaluate(&c);
        assert!(!results[0].ok);
        assert_eq!(results[0].messages.len(), 1);
    }

    #[test]
    fn a_title_with_special_characters_round_trips_through_the_prelude() {
        let c = canvas(
            "# Root\n<!-- meshfox:node id=\"root\" -->\n\n\
             ## He said \"hi\"\n<!-- meshfox:node id=\"quoted\" -->\n\n\
             ## Check\n<!-- meshfox:node id=\"check\" -->\n\n\
             ```starlark constraint\n\
             n = doc.node(\"quoted\")\n\
             if n.title != 'He said \"hi\"':\n\
             \x20   fail(\"title mismatch: \" + n.title)\n\
             ```\n",
        );
        let results = evaluate(&c);
        assert!(results[0].ok, "{:?}", results[0].messages);
    }

    #[test]
    fn nodes_without_constraint_fences_are_not_evaluated() {
        let c = canvas("# Root\n<!-- meshfox:node id=\"root\" -->\n\nsome text\n");
        assert!(evaluate(&c).is_empty());
    }

    #[test]
    fn annotate_status_writes_results_onto_the_owning_node() {
        let mut c = canvas(
            "# Root\n<!-- meshfox:node id=\"root\" -->\n\n\
             ## Check\n<!-- meshfox:node id=\"check\" -->\n\n\
             ```starlark constraint\nfail(\"nope\")\n```\n",
        );
        assert!(c.node("root").unwrap().constraint_results.is_empty());
        assert!(c.node("check").unwrap().constraint_results.is_empty());

        annotate_status(&mut c);

        assert!(c.node("root").unwrap().constraint_results.is_empty());
        let results = &c.node("check").unwrap().constraint_results;
        assert_eq!(results.len(), 1);
        assert!(!results[0].ok);
        assert_eq!(results[0].messages, vec!["nope".to_string()]);
    }

    #[test]
    fn annotate_status_marks_a_passing_constraint_ok() {
        let mut c = canvas(
            "# Root\n<!-- meshfox:node id=\"root\" -->\n\n\
             ## Check\n<!-- meshfox:node id=\"check\" -->\n\n\
             ```starlark constraint\npass\n```\n",
        );
        annotate_status(&mut c);
        let results = &c.node("check").unwrap().constraint_results;
        assert!(results[0].ok);
        assert!(results[0].messages.is_empty());
    }

    #[test]
    fn annotate_status_writes_multiple_results_for_multiple_fences() {
        let mut c = canvas(
            "# Root\n<!-- meshfox:node id=\"root\" -->\n\n\
             ## Check\n<!-- meshfox:node id=\"check\" -->\n\n\
             ```starlark constraint name=\"a\"\npass\n```\n\n\
             ```starlark constraint name=\"b\"\nfail(\"nope\")\n```\n",
        );
        annotate_status(&mut c);
        let results = &c.node("check").unwrap().constraint_results;
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].label, "check/a");
        assert!(results[0].ok);
        assert_eq!(results[1].label, "check/b");
        assert!(!results[1].ok);
    }
}
