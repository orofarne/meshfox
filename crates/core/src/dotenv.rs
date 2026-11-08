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

/// Like [`parse`], for a block's `MESHFOX_VARS_OUT` file: a `NAME[]=item` line
/// *appends* an element to the list variable `NAME` (one element per line, any
/// characters but a newline), and the result holds `NAME` as the JSON array
/// that is the stored form of a list. A plain `NAME=<value>` line sets the
/// value outright (for a list variable it must be a JSON array); lines are
/// applied in order, so a later one wins over an earlier one. Elements are
/// trimmed like every dotenv value.
pub(crate) fn parse_with_lists(contents: &str) -> HashMap<String, String> {
    let mut values: HashMap<String, String> = HashMap::new();
    let mut lists: HashMap<String, Vec<String>> = HashMap::new();
    for line in contents.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let (key, value) = (key.trim(), value.trim());
        if let Some(name) = key.strip_suffix("[]") {
            let name = name.trim_end();
            let list = lists.entry(name.to_string()).or_insert_with(|| {
                // `NAME=["a"]` earlier, then `NAME[]=b`: keep appending.
                values
                    .remove(name)
                    .and_then(|v| crate::listvars::parse_list(&v).ok())
                    .unwrap_or_default()
            });
            list.push(value.to_string());
        } else {
            lists.remove(key);
            values.insert(key.to_string(), value.to_string());
        }
    }
    for (name, items) in lists {
        values.insert(name, crate::listvars::to_json(&items));
    }
    values
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bracket_lines_append_list_elements_in_order() {
        let out = parse_with_lists("IDS[]=1\nOTHER=x\nIDS[]=Acme, Inc.\nIDS[]= 3 \n");
        assert_eq!(out["IDS"], r#"["1","Acme, Inc.","3"]"#);
        assert_eq!(out["OTHER"], "x");
    }

    #[test]
    fn a_json_line_sets_the_list_and_appends_continue_it() {
        let out = parse_with_lists("IDS=[\"a\",\"b\"]\nIDS[]=c\n");
        assert_eq!(out["IDS"], r#"["a","b","c"]"#);
    }

    #[test]
    fn a_later_plain_line_replaces_earlier_appends() {
        let out = parse_with_lists("IDS[]=1\nIDS[]=2\nIDS=[\"z\"]\n");
        assert_eq!(out["IDS"], r#"["z"]"#);
    }

    #[test]
    fn plain_parse_is_unchanged_by_brackets() {
        let out = parse("IDS[]=1\n");
        assert_eq!(out.get("IDS[]").map(String::as_str), Some("1"));
    }

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
