//! Variable cast rules (SPEC.md, "Casting between variables").
//!
//! The **consumer** — the position that reads a value — decides the shape it
//! needs; the variable only declares what it holds. Wherever a reference can
//! cross between a list and a plain string (`default_var=`, `choices_var=`,
//! `suggest_var=`, `env=`), this module is the one place that converts.
//!
//! * a `list` source read where a list is wanted gives its elements — no
//!   pipe is needed (or accepted);
//! * a `list` source read where text is wanted is *rendered*, JSON unless a
//!   pipe picks another [`ListFormat`] (`default_var="TAGS|comma"`);
//! * a string source read where a list is wanted is *parsed*, comma-separated
//!   unless a pipe says otherwise (`choices_var="BRANCHES|lines"`).
//!
//! The pipe names are the [`ListFormat`] names, in both directions.

use crate::listvars::{self, ListFormat};
use crate::vars::VarType;

/// The pipe refinements of one declaration's references (`NAME|format`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RefCasts {
    pub default: Option<ListFormat>,
    pub choices: Option<ListFormat>,
    pub suggest: Option<ListFormat>,
}

/// Splits a reference attribute value, `NAME` or `NAME|format`.
pub fn split_ref(raw: &str) -> Result<(String, Option<ListFormat>), String> {
    match raw.split_once('|') {
        None => Ok((raw.trim().to_string(), None)),
        Some((name, format)) => {
            let format = format.trim();
            ListFormat::parse(format)
                .map(|f| (name.trim().to_string(), Some(f)))
                .ok_or_else(|| format.to_string())
        }
    }
}

/// A list read from `value`, a variable of type `source`.
pub fn to_list(value: &str, source: VarType, format: Option<ListFormat>) -> Vec<String> {
    if source == VarType::List {
        return listvars::parse_list(value).unwrap_or_default();
    }
    format
        .unwrap_or(ListFormat::Comma)
        .parse_items(value)
        .unwrap_or_default()
}

/// Text read from `value`, a variable of type `source`.
pub fn to_text(value: &str, source: VarType, format: Option<ListFormat>) -> String {
    if source != VarType::List {
        return value.to_string();
    }
    match listvars::parse_list(value) {
        Ok(items) => format.unwrap_or(ListFormat::Json).render(&items),
        Err(_) => value.to_string(),
    }
}

/// The value a variable of type `target` takes from `value`, a variable of
/// type `source` (`default_var=`).
pub fn to_target(
    value: &str,
    source: VarType,
    target: VarType,
    format: Option<ListFormat>,
) -> String {
    match (source == VarType::List, target == VarType::List) {
        (true, false) => to_text(value, source, format),
        (false, true) => {
            let items = format
                .unwrap_or(ListFormat::Comma)
                .parse_items(value)
                .unwrap_or_default();
            listvars::to_json(&items)
        }
        _ => value.to_string(),
    }
}

/// Why a pipe on a reference from a `source` variable cannot do anything,
/// for `meshfox validate`; `None` when the combination is meaningful.
pub fn meaningless_pipe(source: VarType, target: VarType) -> Option<&'static str> {
    match (source == VarType::List, target == VarType::List) {
        (true, true) => Some("a list read as a list is just its elements"),
        (false, false) => Some("a plain value read as plain text is not converted"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_reference_may_carry_a_pipe() {
        assert_eq!(split_ref("X").unwrap(), ("X".into(), None));
        assert_eq!(
            split_ref("X|lines").unwrap(),
            ("X".into(), Some(ListFormat::Lines))
        );
        assert_eq!(split_ref("X|bogus").unwrap_err(), "bogus");
    }

    #[test]
    fn a_list_source_gives_its_elements_and_a_string_is_parsed() {
        assert_eq!(
            to_list(r#"["a","b c"]"#, VarType::List, None),
            v(&["a", "b c"])
        );
        assert_eq!(
            to_list("a, b ,c", VarType::String, None),
            v(&["a", "b", "c"])
        );
        assert_eq!(
            to_list("a\nb c\n", VarType::String, Some(ListFormat::Lines)),
            v(&["a", "b c"])
        );
        assert_eq!(
            to_list("{a,\"b c\"}", VarType::String, Some(ListFormat::Pg)),
            v(&["a", "b c"])
        );
    }

    #[test]
    fn a_list_in_a_text_position_is_json_unless_a_pipe_says_otherwise() {
        assert_eq!(to_text(r#"["a","b"]"#, VarType::List, None), r#"["a","b"]"#);
        assert_eq!(
            to_text(r#"["a","b"]"#, VarType::List, Some(ListFormat::Comma)),
            "a,b"
        );
        assert_eq!(to_text("plain", VarType::String, None), "plain");
    }

    #[test]
    fn a_default_is_cast_to_the_declaring_type() {
        let list = r#"["a","b"]"#;
        assert_eq!(
            to_target(
                list,
                VarType::List,
                VarType::String,
                Some(ListFormat::Space)
            ),
            "a b"
        );
        assert_eq!(
            to_target("x,y", VarType::String, VarType::List, None),
            r#"["x","y"]"#
        );
        assert_eq!(to_target("5", VarType::Int, VarType::String, None), "5");
        assert_eq!(to_target(list, VarType::List, VarType::List, None), list);
    }

    #[test]
    fn a_pipe_is_only_meaningful_across_the_list_boundary() {
        assert!(meaningless_pipe(VarType::List, VarType::List).is_some());
        assert!(meaningless_pipe(VarType::String, VarType::String).is_some());
        assert!(meaningless_pipe(VarType::List, VarType::String).is_none());
        assert!(meaningless_pipe(VarType::String, VarType::List).is_none());
    }
}
