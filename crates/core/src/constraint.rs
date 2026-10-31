//! `meshfox_model::constraint` plus its disk-backed conveniences (the model
//! itself reads files only through a [`FileSource`](crate::file_source::FileSource)).

pub use meshfox_model::constraint::*;

use crate::canvas::Canvas;
use std::path::Path;

/// Runs every embedded constraint fence's script against `canvas`, in
/// document order (node order, then fence order within a node). Each fence
/// gets its own fresh Starlark heap/module — nothing persists between
/// them, so one constraint's globals or state can't leak into another's.
///
/// `base_dir` is the directory a `file`-type node's target resolves
/// against (confined to it — see `crate::file_read`), for `.content()`/
/// `.json()`/`.yaml()`/`.toml()`/`.csv()`; `None` when `canvas` wasn't read
/// from a real file (an in-memory/test canvas, or a caller that doesn't
/// want constraints touching disk at all), which makes every one of those
/// calls return `None` rather than erroring.
pub fn evaluate(canvas: &Canvas, base_dir: Option<&Path>) -> Vec<ConstraintResult> {
    evaluate_with_access(canvas, base_dir, &crate::dirvars::FileAccess::none())
}

/// Runs every embedded constraint fence's script and writes each node's
/// results into its own `constraint_results` — never set by
/// `mdcanvas::parse` itself, only by whatever consumer wants it (the
/// server, before serving `GET /api/canvas`), populated by a consumer
/// rather than by parsing. `base_dir` is forwarded to `evaluate` as-is.
pub fn annotate_status(canvas: &mut Canvas, base_dir: Option<&Path>) {
    annotate_status_with_access(canvas, base_dir, &crate::dirvars::FileAccess::none())
}
