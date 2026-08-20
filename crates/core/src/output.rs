//! Writing a code block's cached output back into its node's Markdown.
//!
//! The output lives directly under the fence, wrapped in HTML comment
//! markers keyed by block name so a re-run can find and replace just that
//! region. See README.md's "Cached output" section for the on-disk shape.

use crate::fence::{fingerprint, scan_runnable_blocks};

#[derive(Debug, Clone, PartialEq)]
pub struct ExecOutput {
    pub exit_code: i32,
    pub output: String,
    /// Wall-clock time the block's own process actually ran, in
    /// milliseconds — timed by whichever caller spawned it (every
    /// `Executor`/`stream_exec`/`pty_exec` spawn site), not derived here.
    /// Rendered alongside `exit_code` in the cached-output header (see
    /// `render_output_block`) so a re-opened canvas still shows how long
    /// the last run took, the same information the web UI's live view
    /// already ticks up in real time while a block is running.
    pub duration_ms: u64,
}

fn start_marker(name: &str, hash: &str) -> String {
    format!("<!-- meshfox:output name=\"{name}\" hash=\"{hash}\" -->")
}

/// Just the `name=`-keyed prefix of `start_marker`, with no `hash=` (which
/// varies run to run) — what an *existing* marker for `block_name` is
/// actually matched against, both to find it for replacement
/// (`write_output`) and to read its stored hash back out
/// (`cached_output_hash`). A plain `starts_with` on this, rather than a
/// full literal match against a freshly-rendered `start_marker`, is what
/// lets a still-current marker (any hash) be found and replaced/read at
/// all — matching on the *new* hash would never find the *old* line.
fn start_marker_prefix(name: &str) -> String {
    format!("<!-- meshfox:output name=\"{name}\"")
}

const END_MARKER: &str = "<!-- /meshfox:output -->";

/// `duration_ms` as a short, human-readable duration — `"842ms"` under a
/// second, `"2.3s"` under a minute, `"1m 05s"` beyond that. Rounds rather
/// than truncates fractional seconds so a genuinely-just-under-a-second run
/// doesn't display as a suspicious `"0.0s"`.
pub fn format_duration_ms(duration_ms: u64) -> String {
    if duration_ms < 1000 {
        return format!("{duration_ms}ms");
    }
    let total_seconds = (duration_ms as f64 / 1000.0).round() as u64;
    if total_seconds < 60 {
        format!("{:.1}s", duration_ms as f64 / 1000.0)
    } else {
        format!("{}m {:02}s", total_seconds / 60, total_seconds % 60)
    }
}

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

fn render_output_block(name: &str, output: &ExecOutput, hash: &str) -> String {
    let body = format!(
        "exit code: {} · {}\n\n{}",
        output.exit_code,
        format_duration_ms(output.duration_ms),
        output.output.trim_end()
    );
    let fence = "`".repeat(safe_fence_len(&body));
    format!(
        "{start}\n{fence}text\n{body}\n{fence}\n{end}\n",
        start = start_marker(name, hash),
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
///
/// The marker written also carries `crate::fence::fingerprint(block)` —
/// `block` exactly as found here, i.e. the fence *as it stands in
/// `markdown` right now* — so a later reader (`cached_output_hash`) can
/// tell whether the fence has changed since this output was captured
/// without needing any separate session state; see SPEC.md's "Cached
/// output".
pub fn write_output(markdown: &str, block_name: &str, output: &ExecOutput) -> Option<String> {
    let blocks = scan_runnable_blocks(block_name, markdown);
    let block = blocks
        .iter()
        .find(|b| b.name.as_deref() == Some(block_name))?;
    let insert_point = block.span.end;
    let hash = fingerprint(block);
    let rendered = render_output_block(block_name, output, &hash);
    let marker = start_marker_prefix(block_name);

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

/// Reads back the `hash=` `write_output` embedded in `block_name`'s own
/// cached-output marker, if that block has cached output at all — the
/// counterpart read half of `write_output`'s own write, using the exact
/// same "look for the marker right after the fence's own span" locality
/// (not a whole-document text search, which a command's own captured
/// output could fool the same way `output_containing_backtick_runs_and_
/// headings_round_trips_safely` already guards `write_output` itself
/// against). `None` for a block with no cached output at all, or one
/// cached before this field existed.
pub fn cached_output_hash(markdown: &str, block_name: &str) -> Option<String> {
    let blocks = scan_runnable_blocks(block_name, markdown);
    let block = blocks
        .iter()
        .find(|b| b.name.as_deref() == Some(block_name))?;
    let after = &markdown[block.span.end..];
    let trimmed_after = after.trim_start_matches(['\n', ' ', '\t']);
    let prefix = start_marker_prefix(block_name);
    if !trimmed_after.starts_with(&prefix) {
        return None;
    }
    let line_end = trimmed_after.find("-->")? + "-->".len();
    let marker_line = &trimmed_after[..line_end];
    let inner = marker_line
        .strip_prefix("<!--")?
        .strip_suffix("-->")?
        .trim()
        .strip_prefix("meshfox:output")?;
    crate::attrs::parse_attrs(inner.trim()).remove("hash")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fence::scan_code_blocks;

    fn out(code: i32, s: &str) -> ExecOutput {
        ExecOutput {
            exit_code: code,
            output: s.to_string(),
            duration_ms: 0,
        }
    }

    #[test]
    fn format_duration_ms_under_a_second_is_milliseconds() {
        assert_eq!(format_duration_ms(0), "0ms");
        assert_eq!(format_duration_ms(842), "842ms");
    }

    #[test]
    fn format_duration_ms_under_a_minute_is_one_decimal_seconds() {
        assert_eq!(format_duration_ms(1000), "1.0s");
        assert_eq!(format_duration_ms(2340), "2.3s");
        assert_eq!(format_duration_ms(59_499), "59.5s");
    }

    #[test]
    fn format_duration_ms_right_at_the_minute_boundary_rounds_into_minutes() {
        // 59_999ms rounds to 60 whole seconds, which is no longer < 60 --
        // the boundary check uses the same rounded `total_seconds` the
        // "m/s" branch itself displays, so this is consistent rather than
        // a `"60.0s"` that never actually appears.
        assert_eq!(format_duration_ms(59_999), "1m 00s");
    }

    #[test]
    fn format_duration_ms_a_minute_or_more_is_minutes_and_seconds() {
        assert_eq!(format_duration_ms(60_000), "1m 00s");
        assert_eq!(format_duration_ms(65_000), "1m 05s");
        assert_eq!(format_duration_ms(3_725_000), "62m 05s");
    }

    #[test]
    fn write_output_renders_the_duration_alongside_the_exit_code() {
        let md = "```bash name=\"build\" cache\ncargo build\n```\n";
        let result = ExecOutput {
            exit_code: 0,
            output: "ok".to_string(),
            duration_ms: 2300,
        };
        let updated = write_output(md, "build", &result).unwrap();
        assert!(updated.contains("exit code: 0 · 2.3s"), "{updated}");
    }

    #[test]
    fn inserts_output_when_absent() {
        let md = "```bash name=\"build\" cache\ncargo build\n```\n";
        let updated = write_output(md, "build", &out(0, "ok")).unwrap();
        assert!(updated.contains("<!-- meshfox:output name=\"build\" hash=\""));
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
    fn cached_output_hash_is_none_without_cached_output() {
        let md = "```bash name=\"build\" cache\ncargo build\n```\n";
        assert_eq!(cached_output_hash(md, "build"), None);
    }

    #[test]
    fn cached_output_hash_reads_back_what_write_output_wrote() {
        let md = "```bash name=\"build\" cache\ncargo build\n```\n";
        let updated = write_output(md, "build", &out(0, "ok")).unwrap();
        let block = &scan_code_blocks(&updated)[0];
        assert_eq!(
            cached_output_hash(&updated, "build"),
            Some(crate::fence::fingerprint(block))
        );
    }

    #[test]
    fn cached_output_hash_changes_once_the_fences_own_code_changes() {
        let md = "```bash name=\"build\" cache\ncargo build\n```\n";
        let updated = write_output(md, "build", &out(0, "ok")).unwrap();
        let stored_hash = cached_output_hash(&updated, "build").unwrap();

        // Simulate an edit to the fence's own code, leaving the cached
        // output region (and its old hash) untouched — same shape a real
        // editor save produces before anything re-runs the block.
        let edited = updated.replacen("cargo build", "cargo build --release", 1);
        let live_block = &scan_code_blocks(&edited)[0];
        assert_ne!(stored_hash, crate::fence::fingerprint(live_block));
        // The stored marker itself is untouched by the edit -- only what
        // it's compared against (the fence's own live fingerprint) moved.
        assert_eq!(cached_output_hash(&edited, "build"), Some(stored_hash));
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
        let canvas =
            crate::mdcanvas::parse(&format!("# Root\n<!-- meshfox:node -->\n\n{updated}")).unwrap();
        assert_eq!(canvas.nodes.len(), 1);
    }
}
