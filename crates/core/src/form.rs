//! Parsing a `` ```form `` fence's own body — see SPEC.md's "Form fences".
//!
//! A `form` fence (`crate::exec::FORM_LANG`) is a non-executing runnable
//! fence, same family as a `button` fence: its body is never run as code.
//! Instead it's a line-based list of `field var="NAME" [label="..."]`
//! entries, one per line, referencing *existing* `meshfox:var` declarations
//! (see `crate::vars`) rather than a parallel schema — same
//! "one line, one item" convention `meshfox:edge` already uses, just inside
//! a fence body instead of an HTML comment. The fence's own `name=`/`send=`
//! attributes live on its info string, parsed by `crate::fence` like any
//! other runnable fence's attributes — this module only parses the body.

use thiserror::Error;

/// One `field var="NAME" [label="..."]` line in a `form` fence's body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormField {
    /// The `meshfox:var` this field sets — validated in scope (and against
    /// `from=`-computed vars) by `crate::vars::validate_var_scope`, not
    /// here; this module only parses syntax.
    pub var: String,
    /// `label="..."` — display text for this field; falls back to the
    /// referenced variable's own `prompt`/`name` when omitted (same
    /// fallback shape `crate::vars::VarDecl::prompt` already has relative
    /// to `name`).
    pub label: Option<String>,
}

/// A fully-parsed `form` fence: its own `name`/`send` (read straight off
/// the fence's `attrs`/`code`, same as `button`'s own caption convention)
/// plus its body's `field` list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormBlock {
    pub name: String,
    /// `send="..."` — the Send button's caption. Falls back to a plain
    /// `"Send"` when omitted.
    pub send: Option<String>,
    pub fields: Vec<FormField>,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum FormError {
    #[error("a form fence is missing its required name= attribute")]
    MissingName,
    #[error("a `field` line has no var= attribute: {0:?}")]
    EmptyFieldVar(String),
    #[error("form {0:?} has two `field` lines for the same variable {1:?}")]
    DuplicateFieldVar(String, String),
    #[error("form {0:?} body has a line that isn't a `field ...` entry (and isn't blank): {1:?}")]
    UnexpectedLine(String, String),
}

/// Parses a `form` fence's own body (`CodeBlock::code`) into its `field`
/// list, in document order. A blank line is ignored (for readability,
/// mirroring how blank lines between `meshfox:edge` comments are already
/// harmless); any other non-`field` line is a parse error — there's nothing
/// else a form fence's body is for.
pub fn parse_form_body(form_name: &str, code: &str) -> Result<Vec<FormField>, FormError> {
    let mut fields = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for line in code.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let rest = match trimmed.strip_prefix("field") {
            Some(r) if r.is_empty() || r.starts_with(char::is_whitespace) => r,
            _ => {
                return Err(FormError::UnexpectedLine(
                    form_name.to_string(),
                    trimmed.to_string(),
                ))
            }
        };
        let attrs = crate::attrs::parse_attrs(rest);
        let var = attrs
            .get("var")
            .cloned()
            .ok_or_else(|| FormError::EmptyFieldVar(trimmed.to_string()))?;
        if !seen.insert(var.clone()) {
            return Err(FormError::DuplicateFieldVar(form_name.to_string(), var));
        }
        fields.push(FormField {
            var,
            label: attrs.get("label").cloned(),
        });
    }
    Ok(fields)
}

/// Builds a `FormBlock` from an already-scanned `form`-lang `CodeBlock`
/// (`crate::exec::is_form(&block.lang)` — caller's own responsibility to
/// check, same as every other lang-specific reader in this crate, e.g.
/// `crate::exec::is_button`).
pub fn form_block(block: &crate::fence::CodeBlock) -> Result<FormBlock, FormError> {
    let name = block.name.clone().ok_or(FormError::MissingName)?;
    let fields = parse_form_body(&name, &block.code)?;
    Ok(FormBlock {
        name,
        send: block.attrs.get("send").cloned(),
        fields,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fence::scan_code_blocks;

    fn scan_one(md: &str) -> crate::fence::CodeBlock {
        scan_code_blocks(md).remove(0)
    }

    #[test]
    fn parses_fields_in_order_with_optional_labels() {
        let md = concat!(
            "```form name=\"pick-region\" send=\"Apply\"\n",
            "field var=\"INSTALL_PATH\"\n",
            "field var=\"REGION\" label=\"AWS Region\"\n",
            "```\n",
        );
        let block = scan_one(md);
        let form = form_block(&block).unwrap();
        assert_eq!(form.name, "pick-region");
        assert_eq!(form.send, Some("Apply".to_string()));
        assert_eq!(
            form.fields,
            vec![
                FormField { var: "INSTALL_PATH".to_string(), label: None },
                FormField {
                    var: "REGION".to_string(),
                    label: Some("AWS Region".to_string())
                },
            ]
        );
    }

    #[test]
    fn send_defaults_to_none_when_omitted() {
        let md = "```form name=\"f\"\nfield var=\"X\"\n```\n";
        let form = form_block(&scan_one(md)).unwrap();
        assert_eq!(form.send, None);
    }

    #[test]
    fn blank_lines_between_fields_are_ignored() {
        let md = concat!(
            "```form name=\"f\"\n",
            "field var=\"A\"\n",
            "\n",
            "field var=\"B\"\n",
            "```\n",
        );
        let form = form_block(&scan_one(md)).unwrap();
        assert_eq!(form.fields.len(), 2);
    }

    #[test]
    fn empty_fields_list_is_allowed() {
        let md = "```form name=\"confirm-only\" send=\"Go\"\n```\n";
        let form = form_block(&scan_one(md)).unwrap();
        assert_eq!(form.fields, vec![]);
    }

    #[test]
    fn rejects_a_field_line_with_no_var() {
        let md = "```form name=\"f\"\nfield label=\"oops\"\n```\n";
        let err = form_block(&scan_one(md)).unwrap_err();
        assert!(matches!(err, FormError::EmptyFieldVar(_)));
    }

    #[test]
    fn rejects_duplicate_var_across_two_fields() {
        let md = "```form name=\"f\"\nfield var=\"X\"\nfield var=\"X\"\n```\n";
        let err = form_block(&scan_one(md)).unwrap_err();
        assert_eq!(
            err,
            FormError::DuplicateFieldVar("f".to_string(), "X".to_string())
        );
    }

    #[test]
    fn rejects_a_non_field_line() {
        let md = "```form name=\"f\"\nnot a field line\n```\n";
        let err = form_block(&scan_one(md)).unwrap_err();
        assert!(matches!(err, FormError::UnexpectedLine(_, _)));
    }

    #[test]
    fn rejects_a_form_fence_with_no_name() {
        // `scan_code_blocks` itself already requires `name=` to produce a
        // `CodeBlock` at all (same as every other runnable fence), so
        // `form_block`'s own `MissingName` only matters for a hand-built
        // `CodeBlock` that skipped that filter -- exercised directly here.
        let block = crate::fence::CodeBlock {
            lang: crate::exec::FORM_LANG.to_string(),
            name: None,
            cache: false,
            default: false,
            tty: false,
            autoclose: false,
            service: false,
            always: false,
            autorun: false,
            render: None,
            fold: false,
            deps: Vec::new(),
            env: Vec::new(),
            interpreter: None,
            attrs: std::collections::HashMap::new(),
            code: String::new(),
            span: 0..0,
        };
        assert_eq!(form_block(&block).unwrap_err(), FormError::MissingName);
    }
}
