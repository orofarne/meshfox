//! The values of declared variables a Starlark fence can read as `vars`.
//!
//! The model knows nothing about how a variable gets its value (the
//! environment, the answer cache, a run's output, ...): whoever evaluates a
//! document resolves the variables and hands the result in as a
//! [`ScriptVars`] -- an empty one when there is nothing to offer (a browser
//! embed, a static export). A variable that has no value is simply absent.
//! Secrets are never put here.

use crate::canvas::Canvas;
use std::collections::BTreeMap;

/// One variable's value, typed the way its declaration says: `int` is an
/// integer, `bool` a boolean, `list` a list of its items, everything else
/// (`string`, `select`, `dir`, `file`, `date`, `datetime`) a string.
#[derive(Debug, Clone, PartialEq)]
pub enum ScriptValue {
    Str(String),
    Int(i64),
    Bool(bool),
    List(Vec<ScriptValue>),
}

impl ScriptValue {
    fn write_starlark(&self, out: &mut String) {
        match self {
            ScriptValue::Str(s) => out.push_str(&crate::constraint::star_str(s)),
            ScriptValue::Int(i) => out.push_str(&i.to_string()),
            ScriptValue::Bool(true) => out.push_str("True"),
            ScriptValue::Bool(false) => out.push_str("False"),
            ScriptValue::List(items) => {
                out.push('[');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    item.write_starlark(out);
                }
                out.push(']');
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
struct Entry {
    /// The node the variable is declared in: only that node's subtree sees it
    /// (a root declaration is visible everywhere).
    owner: String,
    value: ScriptValue,
}

/// Every variable value offered to the fences of one document.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ScriptVars {
    entries: BTreeMap<String, Entry>,
}

impl ScriptVars {
    pub fn new() -> ScriptVars {
        ScriptVars::default()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Offers `value` as `vars[name]` to the fences in `owner`'s subtree.
    pub fn insert(&mut self, name: &str, owner: &str, value: ScriptValue) {
        self.entries.insert(
            name.to_string(),
            Entry {
                owner: owner.to_string(),
                value,
            },
        );
    }

    pub fn get(&self, name: &str) -> Option<&ScriptValue> {
        self.entries.get(name).map(|e| &e.value)
    }

    /// Starlark source binding `vars` for a fence living in `node_id`: a dict
    /// of the variables visible from there (see `owner`).
    pub(crate) fn prelude_for(&self, canvas: &Canvas, node_id: &str) -> String {
        let mut out = String::from("vars = {");
        let mut first = true;
        for (name, entry) in &self.entries {
            if !within_subtree(canvas, node_id, &entry.owner) {
                continue;
            }
            if !first {
                out.push_str(", ");
            }
            first = false;
            out.push_str(&crate::constraint::star_str(name));
            out.push_str(": ");
            entry.value.write_starlark(&mut out);
        }
        out.push_str("}\n");
        out
    }
}

/// `node_id` is `ancestor` itself or nested below it.
fn within_subtree(canvas: &Canvas, node_id: &str, ancestor: &str) -> bool {
    let mut current = Some(node_id);
    while let Some(id) = current {
        if id == ancestor {
            return true;
        }
        current = canvas.node(id).and_then(|n| n.parent.as_deref());
    }
    false
}
