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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canvas::Canvas;

    fn dir(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!(
            "meshfox-constraint-vars-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// A `dir` variable chooses where a `file` node and an `include` node read
    /// from; a Starlark fence then sees that file's parsed content and the
    /// included body. The only way a script reaches variables today.
    #[test]
    fn a_dir_variable_steers_the_file_and_include_nodes_a_starlark_fence_reads() {
        let canvas_dir = dir("canvas");
        let data_dir = dir("data");
        std::fs::write(data_dir.join("data.json"), r#"{"answer": 42}"#).unwrap();
        std::fs::write(
            data_dir.join("child.canvas.md"),
            "<!-- meshfox:canvas -->\n# Child\n<!-- meshfox:node id=\"root\" -->\n\nincluded marker text\n",
        )
        .unwrap();
        let var = "MESHFOX_TEST_STARLARK_DATA_DIR";
        std::env::set_var(var, &data_dir);

        let raw = format!(
            concat!(
                "# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
                "<!-- meshfox:var name=\"{var}\" type=\"dir\" -->\n\n",
                "## Data\n<!-- meshfox:node id=\"data\" type=\"file\" -->\n\n[data](${var}/data.json)\n\n",
                "## Child\n<!-- meshfox:node id=\"child\" type=\"include\" -->\n\n[child](${var}/child.canvas.md)\n\n",
                "## Check\n<!-- meshfox:node id=\"check\" -->\n\n",
                "```starlark constraint\n",
                "if doc.node(\"data\").json()[\"answer\"] != 42:\n    fail(\"file node not read through the variable\")\n",
                "if \"included marker text\" not in doc.node(\"child\").text:\n    fail(\"include not read through the variable: \" + doc.node(\"child\").text)\n",
                "```\n",
            ),
            var = var
        );
        let path = canvas_dir.join("c.canvas.md");
        std::fs::write(&path, &raw).unwrap();

        let canvas = Canvas::from_markdown(&raw).unwrap();
        let resolved = crate::include::resolve(&canvas, &path).unwrap();
        let access = crate::FileAccess::for_canvas_path(&resolved, &path);
        let results = evaluate_with_access(&resolved, Some(&canvas_dir), &access);
        std::env::remove_var(var);

        assert_eq!(results.len(), 1);
        assert!(results[0].ok, "{:?}", results[0].messages);
    }
}
