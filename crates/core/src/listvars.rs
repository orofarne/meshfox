//! `type="list"` variables (SPEC.md, "List variables").
//!
//! A list variable holds an ordered list of strings. Internally it travels as
//! a JSON array in a string (the variable stores — session, cache, `--set` —
//! are string maps); that is an implementation detail. Nothing about a
//! separator belongs to the variable: how a list *looks* is decided by whoever
//! reads it, through a [`ListFormat`] chosen in `env="NAME|format"`.

/// How a consumer wants a list rendered into its environment variable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListFormat {
    /// A compact JSON array, `["a","b c"]`. The default: lossless.
    Json,
    /// One element per line.
    Lines,
    /// Shell-quoted words separated by spaces (safe for `eval`/`set --`).
    Space,
    /// Elements joined with `,` — no quoting, for simple values.
    Comma,
    /// Elements joined with a tab character.
    Tab,
    /// A PostgreSQL array literal, `{a,"b c"}`.
    Pg,
}

impl ListFormat {
    pub const NAMES: &'static [&'static str] = &["json", "lines", "space", "comma", "tab", "pg"];

    pub fn parse(name: &str) -> Option<ListFormat> {
        Some(match name {
            "json" => ListFormat::Json,
            "lines" => ListFormat::Lines,
            "space" => ListFormat::Space,
            "comma" => ListFormat::Comma,
            "tab" => ListFormat::Tab,
            "pg" => ListFormat::Pg,
            _ => return None,
        })
    }

    pub fn render(self, items: &[String]) -> String {
        match self {
            ListFormat::Json => to_json(items),
            ListFormat::Lines => items.join("\n"),
            ListFormat::Space => items
                .iter()
                .map(|i| shell_quote(i))
                .collect::<Vec<_>>()
                .join(" "),
            ListFormat::Comma => items.join(","),
            ListFormat::Tab => items.join("\t"),
            ListFormat::Pg => {
                let inner: Vec<String> = items.iter().map(|i| pg_element(i)).collect();
                format!("{{{}}}", inner.join(","))
            }
        }
    }
}

/// The canonical stored form: a compact JSON array of strings.
pub fn to_json(items: &[String]) -> String {
    serde_json::to_string(items).expect("a list of strings always serializes")
}

/// Parses the stored form. Accepts a JSON array whose elements are strings,
/// numbers or booleans (a `jq` pipeline naturally produces `[1,2]`); numbers
/// and booleans become their text. Anything else is an error.
pub fn parse_list(value: &str) -> Result<Vec<String>, String> {
    let parsed: serde_json::Value = serde_json::from_str(value.trim())
        .map_err(|_| "expects a JSON array, e.g. [\"a\",\"b\"]".to_string())?;
    let serde_json::Value::Array(items) = parsed else {
        return Err("expects a JSON array, e.g. [\"a\",\"b\"]".to_string());
    };
    items
        .into_iter()
        .map(|item| match item {
            serde_json::Value::String(s) => Ok(s),
            serde_json::Value::Number(n) => Ok(n.to_string()),
            serde_json::Value::Bool(b) => Ok(b.to_string()),
            other => Err(format!("has a list element that is not a string: {other}")),
        })
        .collect()
}

fn shell_quote(item: &str) -> String {
    let safe = !item.is_empty()
        && item
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "_@%+=:,./-".contains(c));
    if safe {
        item.to_string()
    } else {
        format!("'{}'", item.replace('\'', "'\\''"))
    }
}

fn pg_element(item: &str) -> String {
    let needs_quotes = item.is_empty()
        || item.eq_ignore_ascii_case("null")
        || item
            .chars()
            .any(|c| c.is_whitespace() || matches!(c, ',' | '{' | '}' | '"' | '\\'));
    if !needs_quotes {
        return item.to_string();
    }
    let mut out = String::from("\"");
    for c in item.chars() {
        if c == '"' || c == '\\' {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn items(values: &[&str]) -> Vec<String> {
        values.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn json_is_the_lossless_default_and_round_trips() {
        let list = items(&["a", "Acme, Inc.", "say \"hi\"", ""]);
        let json = ListFormat::Json.render(&list);
        assert_eq!(json, r#"["a","Acme, Inc.","say \"hi\"",""]"#);
        assert_eq!(parse_list(&json).unwrap(), list);
    }

    #[test]
    fn simple_formats_join_elements() {
        let list = items(&["1", "2", "3"]);
        assert_eq!(ListFormat::Lines.render(&list), "1\n2\n3");
        assert_eq!(ListFormat::Comma.render(&list), "1,2,3");
        assert_eq!(ListFormat::Tab.render(&list), "1\t2\t3");
        assert_eq!(ListFormat::Space.render(&list), "1 2 3");
        assert_eq!(ListFormat::Pg.render(&list), "{1,2,3}");
    }

    #[test]
    fn an_empty_list_renders_empty_in_every_format() {
        let none: Vec<String> = Vec::new();
        assert_eq!(ListFormat::Json.render(&none), "[]");
        assert_eq!(ListFormat::Lines.render(&none), "");
        assert_eq!(ListFormat::Space.render(&none), "");
        assert_eq!(ListFormat::Pg.render(&none), "{}");
    }

    #[test]
    fn space_quotes_for_the_shell() {
        let list = items(&["Acme, Inc.", "it's", "plain-1.txt", ""]);
        assert_eq!(
            ListFormat::Space.render(&list),
            r#"'Acme, Inc.' 'it'\''s' plain-1.txt ''"#
        );
    }

    #[test]
    fn pg_quotes_what_postgres_requires() {
        let list = items(&["1", "Acme, Inc.", "a\"b", "back\\slash", "NULL", ""]);
        assert_eq!(
            ListFormat::Pg.render(&list),
            r#"{1,"Acme, Inc.","a\"b","back\\slash","NULL",""}"#
        );
    }

    #[test]
    fn parse_list_accepts_numbers_and_bools_and_rejects_the_rest() {
        assert_eq!(
            parse_list("[1, 2, true, \"x\"]").unwrap(),
            items(&["1", "2", "true", "x"])
        );
        assert!(parse_list("a,b").is_err());
        assert!(parse_list("{\"a\":1}").is_err());
        assert!(parse_list("[[1]]").is_err());
        assert!(parse_list("[null]").is_err());
    }

    #[test]
    fn format_names_round_trip() {
        for name in ListFormat::NAMES {
            assert!(ListFormat::parse(name).is_some(), "{name}");
        }
        assert_eq!(ListFormat::parse("tabs"), None);
    }
}
