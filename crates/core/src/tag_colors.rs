//! Document-scoped tag→color defaults (`<!-- meshfox:tag-color tag="..."
//! color="..." -->`) — lets a whole group of nodes sharing a tag (e.g.
//! "fixed") pick up the same color without setting `color=` on every one
//! of them by hand. Same root-only placement restriction as
//! `crate::vars`/`crate::options`, and the same reasoning: a misplaced or
//! duplicate declaration should fail loudly (`meshfox validate`), not
//! silently do nothing.
//!
//! One declaration per tag (not one comment listing every tag) — a tag
//! itself may contain spaces or other characters a bare `key="value"`
//! token can't safely stand in for, so `tag=`/`color=` each get their own
//! quoted attribute value instead of the tag name doubling as an
//! attribute key.

use crate::attrs::parse_attrs;
use crate::canvas::{Canvas, Node};
use std::collections::{HashMap, HashSet};
use thiserror::Error;

#[derive(Debug, Error, PartialEq)]
pub enum TagColorError {
    #[error("a meshfox:tag-color comment is missing its required tag= attribute")]
    MissingTag,
    #[error("meshfox:tag-color {0:?} is missing its required color= attribute")]
    MissingColor(String),
    #[error("duplicate meshfox:tag-color declaration for tag {0:?}")]
    DuplicateTag(String),
    #[error("meshfox:tag-color {0:?} is declared in node {1:?} — tag colors may only be declared in the root node")]
    NotInRoot(String, String),
}

fn parse_tag_color_comment(line: &str) -> Option<HashMap<String, String>> {
    let trimmed = line.trim();
    let inner = trimmed.strip_prefix("<!--")?.strip_suffix("-->")?.trim();
    let rest = inner.strip_prefix("meshfox:tag-color")?;
    Some(parse_attrs(rest.trim()))
}

/// Scans `markdown` (a node's own body text) for `meshfox:tag-color`
/// comments, fence-aware — same convention `vars::scan_var_decls`/
/// `options::scan_option_decls` already use — and output-region-aware
/// (`fence::in_output_region`), same reasoning as `options::scan_option_decls`'s
/// own doc comment.
pub fn scan_tag_color_decls(markdown: &str) -> Result<Vec<(String, String)>, TagColorError> {
    let fence_ranges = crate::fence::fenced_byte_ranges(markdown);
    let output_ranges = crate::output::output_byte_ranges(markdown);
    let mut fi = 0;
    let mut decls = Vec::new();
    let mut offset = 0;
    for line in markdown.split('\n') {
        let start = offset;
        offset += line.len() + 1;
        while fi < fence_ranges.len() && fence_ranges[fi].end <= start {
            fi += 1;
        }
        if fi < fence_ranges.len() && fence_ranges[fi].start <= start {
            continue;
        }
        if crate::fence::in_output_region(&output_ranges, start) {
            continue;
        }
        if crate::attrs::is_indented_as_code(line) {
            continue;
        }
        if let Some(attrs) = parse_tag_color_comment(line) {
            let tag = attrs.get("tag").cloned().ok_or(TagColorError::MissingTag)?;
            let color = attrs
                .get("color")
                .cloned()
                .ok_or_else(|| TagColorError::MissingColor(tag.clone()))?;
            decls.push((tag, color));
        }
    }
    Ok(decls)
}

/// Every tag→color default `canvas` declares, in document order — always
/// from the root node only (see `TagColorError::NotInRoot`), with a
/// repeated `tag` across the document rejected the same way
/// `vars::declared_vars` rejects a duplicate variable name.
pub fn declared_tag_colors(canvas: &Canvas) -> Result<HashMap<String, String>, TagColorError> {
    let mut root_decls = Vec::new();
    for node in &canvas.nodes {
        let decls = scan_tag_color_decls(&node.text)?;
        if node.parent.is_none() {
            root_decls = decls;
        } else if let Some((tag, _)) = decls.into_iter().next() {
            return Err(TagColorError::NotInRoot(tag, node.id.clone()));
        }
    }

    let mut seen = HashSet::new();
    let mut map = HashMap::new();
    for (tag, color) in root_decls {
        if !seen.insert(tag.clone()) {
            return Err(TagColorError::DuplicateTag(tag));
        }
        map.insert(tag, color);
    }
    Ok(map)
}

/// A node's own `color`, or, when unset, the color of the first of its own
/// `tags` (in the order they're written on that node) that `tag_colors`
/// has a default for. `None` when neither applies — same "no explicit
/// color, no inherited style" fallback every consumer already gives an
/// uncolored node.
pub fn effective_color<'a>(node: &'a Node, tag_colors: &'a HashMap<String, String>) -> Option<&'a str> {
    node.color
        .as_deref()
        .or_else(|| node.tags.iter().find_map(|t| tag_colors.get(t)).map(String::as_str))
}

/// `meshfox validate`-only: the first `meshfox:tag-color` comment
/// attribute anywhere in `markdown` that isn't `tag`/`color` — same
/// fence-aware line scan as `scan_tag_color_decls`, kept separate for the
/// same reason `vars::unknown_var_attr` is (see `attrs::UnknownAttrError`'s
/// own doc comment).
pub fn unknown_tag_color_attr(markdown: &str) -> Option<crate::attrs::UnknownAttrError> {
    const TAG_COLOR_ATTRS: &[&str] = &["tag", "color"];
    let fence_ranges = crate::fence::fenced_byte_ranges(markdown);
    let output_ranges = crate::output::output_byte_ranges(markdown);
    let mut fi = 0;
    let mut offset = 0;
    for line in markdown.split('\n') {
        let start = offset;
        offset += line.len() + 1;
        while fi < fence_ranges.len() && fence_ranges[fi].end <= start {
            fi += 1;
        }
        if fi < fence_ranges.len() && fence_ranges[fi].start <= start {
            continue;
        }
        if crate::fence::in_output_region(&output_ranges, start) {
            continue;
        }
        if crate::attrs::is_indented_as_code(line) {
            continue;
        }
        if let Some(attrs) = parse_tag_color_comment(line) {
            if let Some(attr) = crate::attrs::first_unknown(&attrs, TAG_COLOR_ATTRS) {
                let tag = attrs.get("tag").cloned().unwrap_or_else(|| "<untagged>".to_string());
                return Some(crate::attrs::UnknownAttrError {
                    context: format!("the meshfox:tag-color comment for {tag:?}"),
                    attr: attr.to_string(),
                });
            }
        }
    }
    None
}

/// Populates every node's `effective_color` from its own `color` (if any)
/// or its own tags against this document's declared tag-color defaults —
/// never done by `mdcanvas::parse` itself, only by whichever consumer
/// wants it (the server before serving `GET /api/canvas`, the TUI's `App`
/// before `tree::flatten`, PDF export before rendering), same convention
/// `constraint::annotate_status` already uses. Best-effort: a malformed
/// declaration just means no tag falls back to a color (same as if none
/// were declared) rather than breaking whatever's rendering this canvas —
/// `meshfox validate` is what surfaces that loudly instead.
pub fn annotate_effective_colors(canvas: &mut Canvas) {
    let tag_colors = declared_tag_colors(canvas).unwrap_or_default();
    for node in &mut canvas.nodes {
        node.effective_color = effective_color(node, &tag_colors).map(str::to_string);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mdcanvas::parse;

    fn canvas(md: &str) -> Canvas {
        parse(md).unwrap()
    }

    #[test]
    fn scans_a_simple_declaration() {
        let decls = scan_tag_color_decls("<!-- meshfox:tag-color tag=\"bug\" color=\"1\" -->\n").unwrap();
        assert_eq!(decls, vec![("bug".to_string(), "1".to_string())]);
    }

    #[test]
    fn missing_tag_is_an_error() {
        let err = scan_tag_color_decls("<!-- meshfox:tag-color color=\"1\" -->\n").unwrap_err();
        assert_eq!(err, TagColorError::MissingTag);
    }

    #[test]
    fn missing_color_is_an_error() {
        let err = scan_tag_color_decls("<!-- meshfox:tag-color tag=\"bug\" -->\n").unwrap_err();
        assert_eq!(err, TagColorError::MissingColor("bug".to_string()));
    }

    #[test]
    fn ignores_a_declaration_inside_a_fence() {
        let decls =
            scan_tag_color_decls("```\n<!-- meshfox:tag-color tag=\"bug\" color=\"1\" -->\n```\n")
                .unwrap();
        assert!(decls.is_empty());
    }

    // Same escape hatch fence.rs's own fence-open check uses — SPEC.md
    // documents `meshfox:tag-color` with a worked example written as an
    // indented (4+ space) code block. See vars.rs's own regression test for
    // why this matters once a scanner's target can be spliced in from
    // elsewhere via `include`.
    #[test]
    fn ignores_a_declaration_written_as_an_indented_code_block() {
        let decls =
            scan_tag_color_decls("    <!-- meshfox:tag-color tag=\"bug\" color=\"1\" -->\n").unwrap();
        assert!(decls.is_empty());
    }

    // TODO.canvas.md: "Другие fence-aware сканеры не знают про
    // meshfox:output-регионы" — see options.rs's own regression test for
    // why an unfenced `output="markdown"` region needs its own check
    // beyond the plain fence one.
    #[test]
    fn ignores_a_declaration_forged_inside_an_unfenced_markdown_output_region() {
        let md = concat!(
            "```bash name=\"smoke\" cache output=\"markdown\"\necho hi\n```\n",
            "<!-- meshfox:output name=\"smoke\" hash=\"x\" -->\n\n",
            "<!-- meshfox:tag-color tag=\"bug\" color=\"1\" -->\n\n",
            "<!-- /meshfox:output -->\n",
        );
        let decls = scan_tag_color_decls(md).unwrap();
        assert!(decls.is_empty());
    }

    #[test]
    fn declared_tag_colors_reads_only_the_root_node() {
        let doc = "# Root\n<!-- meshfox:node id=\"root\" -->\n<!-- meshfox:tag-color tag=\"bug\" color=\"1\" -->\n\nbody\n\n## Child\n<!-- meshfox:node id=\"child\" -->\n\nbody\n";
        let c = canvas(doc);
        let map = declared_tag_colors(&c).unwrap();
        assert_eq!(map.get("bug").map(String::as_str), Some("1"));
    }

    #[test]
    fn declared_tag_colors_rejects_a_declaration_outside_the_root() {
        let doc = "# Root\n<!-- meshfox:node id=\"root\" -->\n\nbody\n\n## Child\n<!-- meshfox:node id=\"child\" -->\n<!-- meshfox:tag-color tag=\"bug\" color=\"1\" -->\n\nbody\n";
        let c = canvas(doc);
        let err = declared_tag_colors(&c).unwrap_err();
        assert_eq!(
            err,
            TagColorError::NotInRoot("bug".to_string(), "child".to_string())
        );
    }

    #[test]
    fn declared_tag_colors_rejects_duplicate_tags() {
        let doc = "# Root\n<!-- meshfox:node id=\"root\" -->\n<!-- meshfox:tag-color tag=\"bug\" color=\"1\" -->\n<!-- meshfox:tag-color tag=\"bug\" color=\"2\" -->\n\nbody\n";
        let c = canvas(doc);
        let err = declared_tag_colors(&c).unwrap_err();
        assert_eq!(err, TagColorError::DuplicateTag("bug".to_string()));
    }

    #[test]
    fn effective_color_prefers_an_explicit_color_over_any_tag_default() {
        let doc = "# Root\n<!-- meshfox:node id=\"root\" color=\"3\" tags=\"bug\" -->\n\nbody\n";
        let c = canvas(doc);
        let mut tag_colors = HashMap::new();
        tag_colors.insert("bug".to_string(), "1".to_string());
        let node = c.node("root").unwrap();
        assert_eq!(effective_color(node, &tag_colors), Some("3"));
    }

    #[test]
    fn effective_color_falls_back_to_the_first_matching_tag_in_the_nodes_own_order() {
        let doc = "# Root\n<!-- meshfox:node id=\"root\" tags=\"untagged,bug,feature\" -->\n\nbody\n";
        let c = canvas(doc);
        let mut tag_colors = HashMap::new();
        tag_colors.insert("bug".to_string(), "1".to_string());
        tag_colors.insert("feature".to_string(), "4".to_string());
        let node = c.node("root").unwrap();
        assert_eq!(effective_color(node, &tag_colors), Some("1"));
    }

    #[test]
    fn effective_color_is_none_without_an_explicit_color_or_a_matching_tag() {
        let doc = "# Root\n<!-- meshfox:node id=\"root\" tags=\"untagged\" -->\n\nbody\n";
        let c = canvas(doc);
        let node = c.node("root").unwrap();
        assert_eq!(effective_color(node, &HashMap::new()), None);
    }

    #[test]
    fn annotate_effective_colors_populates_every_node() {
        let doc = "# Root\n<!-- meshfox:node id=\"root\" -->\n<!-- meshfox:tag-color tag=\"bug\" color=\"1\" -->\n\nbody\n\n## Child\n<!-- meshfox:node id=\"child\" tags=\"bug\" -->\n\nbody\n";
        let mut c = canvas(doc);
        annotate_effective_colors(&mut c);
        assert_eq!(c.node("child").unwrap().effective_color.as_deref(), Some("1"));
        assert_eq!(c.node("root").unwrap().effective_color, None);
    }

    #[test]
    fn annotate_effective_colors_is_a_no_op_fallback_on_a_malformed_declaration() {
        // Missing color= makes `declared_tag_colors` error — best-effort
        // display shouldn't break over it, only `meshfox validate` should.
        let doc = "# Root\n<!-- meshfox:node id=\"root\" -->\n<!-- meshfox:tag-color tag=\"bug\" -->\n\nbody\n\n## Child\n<!-- meshfox:node id=\"child\" tags=\"bug\" -->\n\nbody\n";
        let mut c = canvas(doc);
        annotate_effective_colors(&mut c);
        assert_eq!(c.node("child").unwrap().effective_color, None);
    }

    #[test]
    fn unknown_tag_color_attr_is_none_for_known_attributes_only() {
        assert_eq!(
            unknown_tag_color_attr("<!-- meshfox:tag-color tag=\"bug\" color=\"1\" -->\n"),
            None
        );
    }

    #[test]
    fn unknown_tag_color_attr_catches_a_typo_d_attribute() {
        let err = unknown_tag_color_attr("<!-- meshfox:tag-color tag=\"bug\" colr=\"1\" -->\n")
            .expect("colr is not a known attribute");
        assert_eq!(err.attr, "colr");
    }
}
