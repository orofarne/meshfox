//! Small shared attribute-string tokenizer, used both by runnable code
//! fences (`` ```bash name="x" cache ``) and by the `meshfox:node` /
//! `meshfox:edge` HTML comments in the Markdown canvas format.

use std::collections::HashMap;

/// True if `line` is indented 4+ *spaces* (a tab doesn't count) —
/// CommonMark's own threshold for "this is an indented code block, not
/// real document structure", the same escape hatch `crate::fence::
/// fence_open` already uses for a fence's own opening line so SPEC.md's
/// worked examples of the fence syntax read as inert documentation rather
/// than being picked up as real runnable/constraint blocks. Every
/// `meshfox:*` HTML-comment scanner that isn't already anchored to
/// something *else* CommonMark would protect the same way (a fenced code
/// block, or — for `meshfox:node`/`meshfox:edge` — the heading their own
/// marker line always immediately follows, and an indented `#` heading
/// isn't a heading at all) needs this same check on the comment line
/// itself: `crate::vars::scan_var_decls`/`unknown_var_attr`,
/// `crate::options::scan_option_decls`/`unknown_option_attr`,
/// `crate::tag_colors::scan_tag_color_decls`/`unknown_tag_color_attr` — a
/// bare `line.trim()` before matching `<!--` would otherwise erase the
/// indentation signal entirely and treat SPEC.md's own indented example
/// (`    <!-- meshfox:var name="INSTALL_PATH" ... -->`) as a real
/// declaration once it's spliced into another document via `include` and
/// actually scanned (see the regression this was written to fix: a
/// node-scoped `meshfox:var` made non-root nodes newly reachable to
/// `declared_vars`, which is what first exposed this — SPEC.md's own
/// indented `INSTALL_PATH` example collided with README.md's real one).
pub(crate) fn is_indented_as_code(line: &str) -> bool {
    line.len() - line.trim_start_matches(' ').len() >= 4
}

pub fn tokenize(s: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut cur = String::new();
    let mut in_quotes = false;
    for c in s.chars() {
        if c == '"' {
            in_quotes = !in_quotes;
            cur.push(c);
        } else if c.is_whitespace() && !in_quotes {
            if !cur.is_empty() {
                tokens.push(std::mem::take(&mut cur));
            }
        } else {
            cur.push(c);
        }
    }
    if !cur.is_empty() {
        tokens.push(cur);
    }
    tokens
}

pub fn unquote(v: &str) -> String {
    if v.len() >= 2 && v.starts_with('"') && v.ends_with('"') {
        v[1..v.len() - 1].to_string()
    } else {
        v.to_string()
    }
}

/// `key="value"` / `key=value` / bare `key` (flag, value `"true"`) tokens
/// into a map. Order is not preserved — attribute sets are small and
/// unordered by convention.
pub fn attrs_from_tokens(tokens: impl IntoIterator<Item = String>) -> HashMap<String, String> {
    let mut attrs = HashMap::new();
    for tok in tokens {
        if let Some(eq) = tok.find('=') {
            attrs.insert(tok[..eq].to_string(), unquote(&tok[eq + 1..]));
        } else {
            attrs.insert(tok, "true".to_string());
        }
    }
    attrs
}

pub fn parse_attrs(s: &str) -> HashMap<String, String> {
    attrs_from_tokens(tokenize(s))
}

/// `meshfox validate`-only: an attribute name found on a `meshfox:node`/
/// `meshfox:edge`/`meshfox:var`/`meshfox:option` comment or a runnable
/// fence's own info string that isn't part of that construct's known
/// vocabulary. Every other reader (`run`/`view`/`tui`/the server, and
/// `mdcanvas::parse`/`vars`/`options`/`fence` parsing in general) keeps
/// silently accepting an attribute it doesn't recognize — that's the
/// forward/backward-compatibility behavior a format needs between
/// versions. Only `validate` is meant to catch a typo'd attribute name
/// loudly, so this lives as a separate, `validate`-only pass over each
/// construct's own already-parsed attribute map, not folded into the real
/// parsers' own error types.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown attribute {attr:?} on {context}")]
pub struct UnknownAttrError {
    pub context: String,
    pub attr: String,
}

/// The first key in `attrs` that isn't in `known` (alphabetical, so the
/// rare case of more than one unknown attribute on the same line is at
/// least deterministic) — the shared building block every
/// `unknown_*_attr` check (`mdcanvas`, `vars`, `options`, `fence`) filters
/// its own already-parsed attribute map through.
pub fn first_unknown<'a>(attrs: &'a HashMap<String, String>, known: &[&str]) -> Option<&'a str> {
    let mut unknown: Vec<&str> = attrs
        .keys()
        .map(String::as_str)
        .filter(|k| !known.contains(k))
        .collect();
    unknown.sort_unstable();
    unknown.into_iter().next()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_unknown_ignores_known_keys() {
        let attrs = parse_attrs(r#"id="root" color="1""#);
        assert_eq!(first_unknown(&attrs, &["id", "color"]), None);
    }

    #[test]
    fn first_unknown_finds_a_typo_d_attribute() {
        let attrs = parse_attrs(r#"id="root" colr="1""#);
        assert_eq!(first_unknown(&attrs, &["id", "color"]), Some("colr"));
    }

    #[test]
    fn first_unknown_is_deterministic_with_more_than_one_unknown() {
        let attrs = parse_attrs(r#"zzz="1" aaa="2""#);
        assert_eq!(first_unknown(&attrs, &[]), Some("aaa"));
    }

    #[test]
    fn parses_quoted_and_bare_and_flag() {
        let attrs = parse_attrs(r#"name="build" x=10 cache"#);
        assert_eq!(attrs.get("name").map(String::as_str), Some("build"));
        assert_eq!(attrs.get("x").map(String::as_str), Some("10"));
        assert_eq!(attrs.get("cache").map(String::as_str), Some("true"));
    }
}
