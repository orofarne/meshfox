//! Document-scoped boolean settings (`<!-- meshfox:option name="..." -->`)
//! that change a *consumer's* default behavior for the whole document —
//! e.g. the web UI's `unfold` option, which flips its own default from
//! "every subtree folded except root" to "everything expanded". See
//! SPEC.md's "Options" section for the full writeup.
//!
//! Deliberately minimal next to `crate::vars`: an option is a bare
//! presence flag (declared = on, absent = off), never resolved, prompted
//! for, or cached — there's nothing here but parsing. Same root-only
//! placement restriction as `meshfox:var`, and the same reasoning: a
//! misplaced declaration should fail loudly (`meshfox validate`), not
//! silently do nothing.

use crate::attrs::parse_attrs;
use crate::canvas::Canvas;
use std::collections::HashSet;
use thiserror::Error;

#[derive(Debug, Error, PartialEq)]
pub enum OptionsError {
    #[error("a meshfox:option comment is missing its required name= attribute")]
    MissingName,
    #[error("duplicate meshfox:option name {0:?}")]
    DuplicateName(String),
    #[error("meshfox:option {0:?} is declared in node {1:?} — options may only be declared in the root node")]
    NotInRoot(String, String),
}

/// `meshfox:option`'s only attribute — `unknown_option_attr` (below,
/// `meshfox validate`-only) diffs a declaration's own attribute keys
/// against this.
const OPTION_ATTRS: &[&str] = &["name"];

pub(crate) fn parse_option_comment(
    line: &str,
) -> Option<std::collections::HashMap<String, String>> {
    let trimmed = line.trim();
    let inner = trimmed.strip_prefix("<!--")?.strip_suffix("-->")?.trim();
    let rest = inner.strip_prefix("meshfox:option")?;
    Some(parse_attrs(rest.trim()))
}

/// Scans `markdown` (a node's own body text) for `meshfox:option` comments,
/// fence-aware — a line inside a code fence (e.g. a worked example showing
/// off the syntax itself) is never mistaken for a real declaration, same
/// convention `vars::scan_var_decls`/`mdcanvas::scan` already use.
pub fn scan_option_decls(markdown: &str) -> Result<Vec<String>, OptionsError> {
    let fence_ranges = crate::fence::fenced_byte_ranges(markdown);
    let mut fi = 0;
    let mut names = Vec::new();
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
        if let Some(attrs) = parse_option_comment(line) {
            names.push(
                attrs
                    .get("name")
                    .cloned()
                    .ok_or(OptionsError::MissingName)?,
            );
        }
    }
    Ok(names)
}

/// `meshfox validate`-only: the first `meshfox:option` comment attribute
/// anywhere in `markdown` that isn't `name` — same fence-aware line scan
/// as `scan_option_decls`, kept separate for the same reason
/// `vars::unknown_var_attr` is (see `attrs::UnknownAttrError`'s own doc
/// comment).
pub fn unknown_option_attr(markdown: &str) -> Option<crate::attrs::UnknownAttrError> {
    let fence_ranges = crate::fence::fenced_byte_ranges(markdown);
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
        if let Some(attrs) = parse_option_comment(line) {
            if let Some(attr) = crate::attrs::first_unknown(&attrs, OPTION_ATTRS) {
                return Some(crate::attrs::UnknownAttrError {
                    context: "a meshfox:option comment".to_string(),
                    attr: attr.to_string(),
                });
            }
        }
    }
    None
}

/// Every option `canvas` declares, in document order — always from the
/// root node only (see `OptionsError::NotInRoot`), with a repeated `name`
/// across the document rejected the same way `vars::declared_vars` rejects
/// a duplicate variable name.
pub fn declared_options(canvas: &Canvas) -> Result<Vec<String>, OptionsError> {
    let mut root_names = Vec::new();
    for node in &canvas.nodes {
        let names = scan_option_decls(&node.text)?;
        if node.parent.is_none() {
            root_names = names;
        } else if let Some(first) = names.into_iter().next() {
            return Err(OptionsError::NotInRoot(first, node.id.clone()));
        }
    }

    let mut seen = HashSet::new();
    for name in &root_names {
        if !seen.insert(name.clone()) {
            return Err(OptionsError::DuplicateName(name.clone()));
        }
    }
    Ok(root_names)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mdcanvas::parse;

    #[test]
    fn scans_a_simple_option() {
        let names = scan_option_decls("<!-- meshfox:option name=\"unfold\" -->\n\nbody\n").unwrap();
        assert_eq!(names, vec!["unfold".to_string()]);
    }

    #[test]
    fn missing_name_is_an_error() {
        let err = scan_option_decls("<!-- meshfox:option -->\n").unwrap_err();
        assert_eq!(err, OptionsError::MissingName);
    }

    #[test]
    fn ignores_an_option_comment_inside_a_fence() {
        let names =
            scan_option_decls("```\n<!-- meshfox:option name=\"unfold\" -->\n```\n").unwrap();
        assert!(names.is_empty());
    }

    #[test]
    fn declared_options_reads_only_the_root_node() {
        let doc = "# Root\n<!-- meshfox:node id=\"root\" -->\n<!-- meshfox:option name=\"unfold\" -->\n\nbody\n\n## Child\n<!-- meshfox:node id=\"child\" -->\n\nbody\n";
        let canvas = parse(doc).unwrap();
        assert_eq!(
            declared_options(&canvas).unwrap(),
            vec!["unfold".to_string()]
        );
    }

    #[test]
    fn declared_options_rejects_a_declaration_outside_the_root() {
        let doc = "# Root\n<!-- meshfox:node id=\"root\" -->\n\nbody\n\n## Child\n<!-- meshfox:node id=\"child\" -->\n<!-- meshfox:option name=\"unfold\" -->\n\nbody\n";
        let canvas = parse(doc).unwrap();
        let err = declared_options(&canvas).unwrap_err();
        assert_eq!(
            err,
            OptionsError::NotInRoot("unfold".to_string(), "child".to_string())
        );
    }

    #[test]
    fn declared_options_rejects_duplicate_names() {
        let doc = "# Root\n<!-- meshfox:node id=\"root\" -->\n<!-- meshfox:option name=\"unfold\" -->\n<!-- meshfox:option name=\"unfold\" -->\n\nbody\n";
        let canvas = parse(doc).unwrap();
        let err = declared_options(&canvas).unwrap_err();
        assert_eq!(err, OptionsError::DuplicateName("unfold".to_string()));
    }

    // TODO.canvas.md: "Ошибка при неизвестных параметрах в validate" —
    // `unknown_option_attr` is `validate`-only, same split as
    // `mdcanvas::unknown_node_edge_attr`.
    #[test]
    fn unknown_option_attr_is_none_for_name_only() {
        assert_eq!(
            unknown_option_attr("<!-- meshfox:option name=\"unfold\" -->\n"),
            None
        );
    }

    #[test]
    fn unknown_option_attr_catches_a_typo_d_attribute() {
        let err = unknown_option_attr("<!-- meshfox:option nme=\"unfold\" -->\n")
            .expect("nme is not a known attribute");
        assert_eq!(err.attr, "nme");
    }
}
