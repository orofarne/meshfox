//! The plain `KEY=value`-per-line format shared by `crate::varcache` (the
//! on-disk resolved-variable cache) and `crate::varout` (a `from=` source
//! block's own output file) — same shape, same parsing rules, one place to
//! keep them in sync.

use std::collections::HashMap;

/// Parses `contents` as `KEY=value` lines — blank lines and `#`-prefixed
/// comment lines are skipped, everything else must contain an `=`. A line
/// that doesn't is silently ignored rather than treated as an error: both
/// callers read a file they don't fully control the contents of (a
/// hand-edited cache, a block's own output), and a malformed line there
/// shouldn't take down parsing of every other line in it.
pub(crate) fn parse(contents: &str) -> HashMap<String, String> {
    contents
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                return None;
            }
            let (k, v) = line.split_once('=')?;
            Some((k.trim().to_string(), v.trim().to_string()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_simple_key_value_lines() {
        let out = parse("A=1\nB=hello world\n");
        assert_eq!(out.get("A").map(String::as_str), Some("1"));
        assert_eq!(out.get("B").map(String::as_str), Some("hello world"));
    }

    #[test]
    fn skips_blank_lines_and_comments() {
        let out = parse("\n# a comment\nA=1\n\n#B=2\n");
        assert_eq!(out.len(), 1);
        assert_eq!(out.get("A").map(String::as_str), Some("1"));
    }

    #[test]
    fn ignores_a_line_with_no_equals_sign() {
        let out = parse("not-a-kv-line\nA=1\n");
        assert_eq!(out.len(), 1);
        assert_eq!(out.get("A").map(String::as_str), Some("1"));
    }

    #[test]
    fn trims_whitespace_around_key_and_value() {
        let out = parse("  A  =  1  \n");
        assert_eq!(out.get("A").map(String::as_str), Some("1"));
    }

    #[test]
    fn value_may_contain_further_equals_signs() {
        let out = parse("URL=https://example.com?a=1&b=2\n");
        assert_eq!(
            out.get("URL").map(String::as_str),
            Some("https://example.com?a=1&b=2")
        );
    }
}
