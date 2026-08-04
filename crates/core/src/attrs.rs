//! Small shared attribute-string tokenizer, used both by runnable code
//! fences (`` ```bash name="x" cache ``) and by the `meshfox:node` /
//! `meshfox:edge` HTML comments in the Markdown canvas format.

use std::collections::HashMap;

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_quoted_and_bare_and_flag() {
        let attrs = parse_attrs(r#"name="build" x=10 cache"#);
        assert_eq!(attrs.get("name").map(String::as_str), Some("build"));
        assert_eq!(attrs.get("x").map(String::as_str), Some("10"));
        assert_eq!(attrs.get("cache").map(String::as_str), Some("true"));
    }
}
