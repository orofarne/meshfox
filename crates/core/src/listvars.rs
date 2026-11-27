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

    pub fn name(self) -> &'static str {
        match self {
            ListFormat::Json => "json",
            ListFormat::Lines => "lines",
            ListFormat::Space => "space",
            ListFormat::Comma => "comma",
            ListFormat::Tab => "tab",
            ListFormat::Pg => "pg",
        }
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

impl ListFormat {
    /// The inverse of [`ListFormat::render`]: reads a list a consumer wants
    /// out of text written in this format. Empty elements are dropped for
    /// the separator formats (`lines`, `comma`, `tab`, `space`), which have
    /// no way to say "an empty string" unambiguously; `json` and `pg` keep
    /// them because they quote.
    pub fn parse_items(self, text: &str) -> Result<Vec<String>, String> {
        match self {
            ListFormat::Json => parse_list(text),
            ListFormat::Lines => Ok(text
                .lines()
                .map(|l| l.trim_end_matches('\r'))
                .filter(|l| !l.trim().is_empty())
                .map(str::to_string)
                .collect()),
            ListFormat::Comma => Ok(text
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect()),
            ListFormat::Tab => Ok(text
                .split('\t')
                .map(|s| s.trim_matches(|c| c == '\n' || c == '\r'))
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect()),
            ListFormat::Space => split_shell_words(text),
            ListFormat::Pg => parse_pg(text),
        }
    }
}

/// POSIX-ish word splitting: whitespace separates, `'..'` is literal,
/// `".."` honours `\"` and `\\`, a bare backslash escapes the next char.
fn split_shell_words(text: &str) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_word = false;
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        match c {
            c if c.is_whitespace() => {
                if in_word {
                    out.push(std::mem::take(&mut cur));
                    in_word = false;
                }
            }
            '\'' => {
                in_word = true;
                loop {
                    match chars.next() {
                        Some('\'') => break,
                        Some(c) => cur.push(c),
                        None => return Err("has an unterminated ' quote".into()),
                    }
                }
            }
            '"' => {
                in_word = true;
                loop {
                    match chars.next() {
                        Some('"') => break,
                        Some('\\') => match chars.next() {
                            Some(c) if c == '"' || c == '\\' => cur.push(c),
                            Some(c) => {
                                cur.push('\\');
                                cur.push(c);
                            }
                            None => return Err("has an unterminated \" quote".into()),
                        },
                        Some(c) => cur.push(c),
                        None => return Err("has an unterminated \" quote".into()),
                    }
                }
            }
            '\\' => {
                in_word = true;
                match chars.next() {
                    Some(c) => cur.push(c),
                    None => cur.push('\\'),
                }
            }
            c => {
                in_word = true;
                cur.push(c);
            }
        }
    }
    if in_word {
        out.push(cur);
    }
    Ok(out)
}

/// A one-dimensional PostgreSQL array literal, `{a,"b c"}`.
fn parse_pg(text: &str) -> Result<Vec<String>, String> {
    let text = text.trim();
    let inner = text
        .strip_prefix('{')
        .and_then(|t| t.strip_suffix('}'))
        .ok_or_else(|| "expects a PostgreSQL array literal, e.g. {a,\"b c\"}".to_string())?;
    let mut out = Vec::new();
    let mut chars = inner.chars().peekable();
    loop {
        while chars.peek().is_some_and(|c| c.is_whitespace()) {
            chars.next();
        }
        if chars.peek().is_none() {
            break;
        }
        let mut item = String::new();
        if chars.peek() == Some(&'"') {
            chars.next();
            loop {
                match chars.next() {
                    Some('"') => break,
                    Some('\\') => match chars.next() {
                        Some(c) => item.push(c),
                        None => return Err("has a dangling backslash in a PostgreSQL array".into()),
                    },
                    Some(c) => item.push(c),
                    None => return Err("has an unterminated quote in a PostgreSQL array".into()),
                }
            }
            while chars.peek().is_some_and(|c| c.is_whitespace()) {
                chars.next();
            }
        } else {
            while let Some(&c) = chars.peek() {
                if c == ',' {
                    break;
                }
                item.push(c);
                chars.next();
            }
            item = item.trim().to_string();
        }
        out.push(item);
        match chars.next() {
            Some(',') => continue,
            None => break,
            Some(c) => return Err(format!("has an unexpected {c:?} in a PostgreSQL array")),
        }
    }
    Ok(out)
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

/// What a person types at a terminal prompt or a plain text field for a list:
/// a JSON array, or simply items separated by commas (`us, eu`). Returns the
/// stored JSON form. A comma-separated entry cannot hold an element with a
/// comma in it — type that one as a JSON array.
pub fn from_user_input(input: &str) -> Result<String, String> {
    let input = input.trim();
    if input.starts_with('[') {
        return parse_list(input).map(|items| to_json(&items));
    }
    let items: Vec<String> = input
        .split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(str::to_string)
        .collect();
    Ok(to_json(&items))
}

/// The stored JSON as a person would like to see and edit it in a text field:
/// items joined by `, ` when none of them needs quoting, the JSON array
/// otherwise (so [`from_user_input`] reads it back unchanged).
pub fn to_user_text(stored: &str) -> String {
    match parse_list(stored) {
        Ok(items)
            if items
                .iter()
                .all(|i| !i.contains(',') && i == i.trim() && !i.starts_with('[')) =>
        {
            items.join(", ")
        }
        _ => stored.to_string(),
    }
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
    fn every_format_parses_back_what_it_renders() {
        let list = items(&["a", "b c", "it's", "x,y", "", "say \"hi\""]);
        // json and pg are the lossless ones; the separator formats cannot
        // carry an empty element, and comma/lines/tab cannot carry their
        // own separator.
        for f in [ListFormat::Json, ListFormat::Pg, ListFormat::Space] {
            let back = f.parse_items(&f.render(&list)).unwrap();
            let expected: Vec<String> = if f == ListFormat::Space {
                list.iter().filter(|s| !s.is_empty()).cloned().collect()
            } else {
                list.clone()
            };
            // `Space` quotes the empty element ('') so it round-trips too.
            if f == ListFormat::Space {
                assert_eq!(back.len(), list.len(), "{f:?}");
            } else {
                assert_eq!(back, expected, "{f:?}");
            }
        }
        let plain = items(&["a", "b c", "d"]);
        for f in [ListFormat::Lines, ListFormat::Comma, ListFormat::Tab] {
            assert_eq!(f.parse_items(&f.render(&plain)).unwrap(), plain, "{f:?}");
        }
    }

    #[test]
    fn parsing_is_forgiving_about_whitespace_and_empties() {
        assert_eq!(
            ListFormat::Comma.parse_items(" a , ,b,").unwrap(),
            items(&["a", "b"])
        );
        assert_eq!(
            ListFormat::Lines.parse_items("a\r\n\nb\n").unwrap(),
            items(&["a", "b"])
        );
        assert_eq!(
            ListFormat::Space.parse_items("  a   'b c' d\\ e ").unwrap(),
            items(&["a", "b c", "d e"])
        );
        assert_eq!(
            ListFormat::Pg.parse_items("{}").unwrap(),
            Vec::<String>::new()
        );
        assert_eq!(
            ListFormat::Pg.parse_items("{a, \"b,c\" ,d}").unwrap(),
            items(&["a", "b,c", "d"])
        );
        assert!(ListFormat::Pg.parse_items("a,b").is_err());
        assert!(ListFormat::Space.parse_items("'open").is_err());
        assert!(ListFormat::Json.parse_items("not json").is_err());
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
    fn user_input_accepts_json_or_commas_and_reads_its_own_text_back() {
        assert_eq!(from_user_input("us, eu ,").unwrap(), r#"["us","eu"]"#);
        assert_eq!(from_user_input("").unwrap(), "[]");
        assert_eq!(
            from_user_input(r#"["a, b","c"]"#).unwrap(),
            r#"["a, b","c"]"#
        );
        assert!(from_user_input("[oops").is_err());
        assert_eq!(to_user_text(r#"["us","eu"]"#), "us, eu");
        // An element with a comma keeps the JSON form, which round-trips.
        let stored = r#"["a, b","c"]"#;
        assert_eq!(to_user_text(stored), stored);
        assert_eq!(from_user_input(&to_user_text(stored)).unwrap(), stored);
        assert_eq!(to_user_text("not json"), "not json");
    }

    #[test]
    fn format_names_round_trip() {
        for name in ListFormat::NAMES {
            assert!(ListFormat::parse(name).is_some(), "{name}");
        }
        assert_eq!(ListFormat::parse("tabs"), None);
    }
}
