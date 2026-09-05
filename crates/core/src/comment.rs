//! `meshfox:comment` — a marker pair whose *own* two lines behave exactly
//! like the plain HTML comments they are (invisible to any Markdown
//! renderer, meshfox included), but the text *between* them is ordinary
//! Markdown as far as anything other than meshfox is concerned:
//!
//! ```markdown
//! <!-- meshfox:comment -->Any text<!-- /meshfox:comment -->
//! ```
//!
//! A plain Markdown renderer (GitHub, ...) shows "Any text" completely
//! normally — it's just paragraph content sitting between two invisible
//! comments. meshfox's own tooling (the web UI, the TUI, `static`/`pdf`
//! export) instead recognizes the marker pair and drops the *whole*
//! region, text included, before a node's body ever reaches any of them —
//! see `strip`, called once from `mdcanvas::parse` so every consumer of
//! `Node::text` gets this for free. See SPEC.md's "Comments" for the
//! intended use (context meant only for someone reading the raw file
//! outside meshfox — `meshfox create`'s own template uses it for exactly
//! that, see `crates/cli/src/main.rs::write_canvas_template`).

use crate::fence::fenced_byte_ranges;
use std::ops::Range;

const START_MARKER: &str = "<!-- meshfox:comment -->";
const END_MARKER: &str = "<!-- /meshfox:comment -->";

/// Removes every top-level (fence-aware — a marker written literally
/// inside a code fence, e.g. as a usage example, is left alone, same
/// spirit as `mdcanvas::scan`'s own fence-awareness for headings/node
/// comments — and output-region-aware, same reasoning as
/// `fence::in_output_region`'s own doc comment: a marker only present
/// because a command's own `output="markdown"` stdout got spliced in must
/// never be mistaken for one the document's author actually wrote)
/// `meshfox:comment` region from `markdown`, markers and enclosed text
/// alike. Pairs each start marker with the next end marker after it, left
/// to right; a start marker with no end marker anywhere after it is left
/// untouched (malformed input — same "don't guess, leave it alone" every
/// other marker-pair parser in this crate uses).
pub(crate) fn strip(markdown: &str) -> String {
    let fenced = fenced_byte_ranges(markdown);
    let output = crate::output::output_byte_ranges(markdown);
    let in_fence = |pos: usize| fenced.iter().any(|r| r.start <= pos && pos < r.end);
    let hidden = |pos: usize| in_fence(pos) || crate::fence::in_output_region(&output, pos);

    let starts: Vec<usize> = markdown
        .match_indices(START_MARKER)
        .map(|(i, _)| i)
        .filter(|&i| !hidden(i))
        .collect();
    let ends: Vec<usize> = markdown
        .match_indices(END_MARKER)
        .map(|(i, _)| i)
        .filter(|&i| !hidden(i))
        .collect();

    let mut regions: Vec<Range<usize>> = Vec::new();
    let mut cursor = 0;
    for &start in &starts {
        if start < cursor {
            // Already inside a region just claimed by an earlier pairing
            // — an unterminated start marker can't happen here since
            // every claimed region really did end at a real end marker.
            continue;
        }
        if let Some(&end) = ends.iter().find(|&&e| e >= start + START_MARKER.len()) {
            let region_end = end + END_MARKER.len();
            regions.push(start..region_end);
            cursor = region_end;
        }
    }

    let mut out = String::with_capacity(markdown.len());
    let mut last = 0;
    for r in &regions {
        out.push_str(&markdown[last..r.start]);
        last = r.end;
    }
    out.push_str(&markdown[last..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_a_region_including_its_own_markers() {
        assert_eq!(strip("before <!-- meshfox:comment -->hidden<!-- /meshfox:comment --> after"), "before  after");
    }

    #[test]
    fn strips_multiple_separate_regions() {
        let md = "a <!-- meshfox:comment -->x<!-- /meshfox:comment --> b <!-- meshfox:comment -->y<!-- /meshfox:comment --> c";
        assert_eq!(strip(md), "a  b  c");
    }

    #[test]
    fn leaves_plain_text_with_no_markers_untouched() {
        assert_eq!(strip("just some text\n\nmore text\n"), "just some text\n\nmore text\n");
    }

    #[test]
    fn leaves_an_unterminated_start_marker_untouched() {
        let md = "before <!-- meshfox:comment -->never closed";
        assert_eq!(strip(md), md);
    }

    #[test]
    fn a_marker_written_literally_inside_a_code_fence_is_left_alone() {
        // Showing someone the syntax inside a fenced example (like this
        // module's own doc comment does) must not itself be treated as a
        // real comment region.
        let md = "```markdown\n<!-- meshfox:comment -->Any text<!-- /meshfox:comment -->\n```\n";
        assert_eq!(strip(md), md);
    }

    #[test]
    fn a_region_can_span_a_fence_that_sits_entirely_inside_it() {
        let md = "<!-- meshfox:comment -->before\n```\ncode\n```\nafter<!-- /meshfox:comment -->kept";
        assert_eq!(strip(md), "kept");
    }

    // TODO.canvas.md: "Другие fence-aware сканеры не знают про
    // meshfox:output-регионы" — an `output="markdown"` region splices a
    // command's raw stdout in unfenced (see
    // `output::render_output_block_markdown`), so a marker pair landing
    // there isn't caught by the plain fence check above at all; must be
    // recognized as opaque via `output::output_byte_ranges` instead.
    #[test]
    fn a_marker_pair_forged_inside_an_unfenced_markdown_output_region_is_left_alone() {
        let md = concat!(
            "keep before\n\n",
            "```bash name=\"smoke\" cache output=\"markdown\"\necho hi\n```\n",
            "<!-- meshfox:output name=\"smoke\" hash=\"x\" -->\n\n",
            "<!-- meshfox:comment -->forged<!-- /meshfox:comment -->\n\n",
            "<!-- /meshfox:output -->\n",
            "keep after\n",
        );
        assert_eq!(strip(md), md);
    }
}
