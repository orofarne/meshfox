//! Writing a code block's cached output back into its node's Markdown.
//!
//! The output lives directly under the fence, wrapped in HTML comment
//! markers keyed by block name so a re-run can find and replace just that
//! region. See README.md's "Cached output" section for the on-disk shape.

use crate::fence::scan_runnable_blocks;

#[derive(Debug, Clone, PartialEq)]
pub struct ExecOutput {
    pub exit_code: i32,
    pub output: String,
}

fn start_marker(name: &str) -> String {
    format!("<!-- meshfox:output name=\"{name}\" -->")
}

const END_MARKER: &str = "<!-- /meshfox:output -->";

/// A fence length guaranteed to not be closed early by anything in `body`:
/// one longer than the longest run of backticks `body` contains, and never
/// shorter than 3. `body` is a command's captured output, so it can contain
/// literally anything — including a run of backticks as long as (or longer
/// than) a fixed ` ```` ` would be.
fn safe_fence_len(body: &str) -> usize {
    let mut longest_run = 0;
    let mut run = 0;
    for c in body.chars() {
        if c == '`' {
            run += 1;
            longest_run = longest_run.max(run);
        } else {
            run = 0;
        }
    }
    (longest_run + 1).max(3)
}

fn render_output_block(name: &str, output: &ExecOutput) -> String {
    let body = format!("exit code: {}\n\n{}", output.exit_code, output.output.trim_end());
    let fence = "`".repeat(safe_fence_len(&body));
    format!(
        "{start}\n{fence}text\n{body}\n{fence}\n{end}\n",
        start = start_marker(name),
        end = END_MARKER,
    )
}

/// Insert or update the cached-output region for the code block named
/// `block_name` in `markdown`. Returns `None` if no runnable block with
/// that name exists. `block_name` doubles as the node-id fallback
/// `scan_runnable_blocks` needs to resolve an implicitly-named lone
/// block — correct because callers always pass the block's already-
/// resolved effective name, which *is* the owning node's id in exactly
/// that case.
pub fn write_output(markdown: &str, block_name: &str, output: &ExecOutput) -> Option<String> {
    let blocks = scan_runnable_blocks(block_name, markdown);
    let block = blocks.iter().find(|b| b.name.as_deref() == Some(block_name))?;
    let insert_point = block.span.end;
    let rendered = render_output_block(block_name, output);
    let marker = start_marker(block_name);

    let after = &markdown[insert_point..];
    let trimmed_after = after.trim_start_matches(['\n', ' ', '\t']);
    let gap = after.len() - trimmed_after.len();

    let mut result = String::with_capacity(markdown.len() + rendered.len());
    result.push_str(&markdown[..insert_point]);
    result.push('\n');
    result.push_str(&rendered);

    if trimmed_after.starts_with(&marker) {
        if let Some(end_idx) = trimmed_after.find(END_MARKER) {
            let region_end = gap + end_idx + END_MARKER.len();
            result.push_str(&markdown[insert_point + region_end..]);
            return Some(result);
        }
    }
    result.push_str(after);
    Some(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fence::scan_code_blocks;

    fn out(code: i32, s: &str) -> ExecOutput {
        ExecOutput {
            exit_code: code,
            output: s.to_string(),
        }
    }

    #[test]
    fn inserts_output_when_absent() {
        let md = "```bash name=\"build\" cache\ncargo build\n```\n";
        let updated = write_output(md, "build", &out(0, "ok")).unwrap();
        assert!(updated.contains("<!-- meshfox:output name=\"build\" -->"));
        assert!(updated.contains("exit code: 0"));
        assert!(updated.contains("ok"));
        assert!(updated.contains("<!-- /meshfox:output -->"));
        // the block is still there and still scannable
        let blocks = scan_code_blocks(&updated);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].name.as_deref(), Some("build"));
    }

    #[test]
    fn replaces_existing_output_in_place() {
        let md = "```bash name=\"build\" cache\ncargo build\n```\n";
        let first = write_output(md, "build", &out(1, "first run failed")).unwrap();
        let second = write_output(&first, "build", &out(0, "second run ok")).unwrap();

        assert!(!second.contains("first run failed"));
        assert!(second.contains("second run ok"));
        assert!(second.contains("exit code: 0"));
        // exactly one marker pair, not stacked
        assert_eq!(second.matches("meshfox:output name=\"build\"").count(), 1);
    }

    #[test]
    fn leaves_unrelated_content_untouched() {
        let md = "intro\n\n```bash name=\"build\" cache\ncargo build\n```\n\noutro text\n";
        let updated = write_output(md, "build", &out(0, "ok")).unwrap();
        assert!(updated.starts_with("intro\n\n"));
        assert!(updated.trim_end().ends_with("outro text"));
    }

    #[test]
    fn returns_none_for_unknown_block() {
        let md = "```bash name=\"build\"\ncargo build\n```\n";
        assert!(write_output(md, "nope", &out(0, "ok")).is_none());
    }

    #[test]
    fn output_containing_backtick_runs_and_headings_round_trips_safely() {
        // Output from an arbitrary command can contain anything — including
        // text that looks exactly like meshfox's own syntax. The written
        // block must use a fence long enough that none of it can be
        // (mis)read as closing the block early.
        let evil = "# Fake Heading\n```\n````\n`````\n<!-- meshfox:node id=\"fake\" -->";
        let md = "```bash name=\"evil\" cache\necho evil\n```\n";
        let updated = write_output(md, "evil", &out(0, evil)).unwrap();

        // The output block must still be found intact by a re-scan (i.e.
        // the fence wasn't closed early by content inside it), and the
        // evil payload must be fully preserved, backtick runs and all.
        assert!(updated.contains(evil));
        let blocks = scan_code_blocks(&updated);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].name.as_deref(), Some("evil"));

        // A full canvas parse must not treat the embedded fake heading /
        // meshfox:node comment as real structure.
        let canvas = crate::mdcanvas::parse(&format!("# Root\n<!-- meshfox:node -->\n\n{updated}")).unwrap();
        assert_eq!(canvas.nodes.len(), 1);
    }
}
