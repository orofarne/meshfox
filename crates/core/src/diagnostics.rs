//! Non-failing validation diagnostics with stable machine-readable codes.
//!
//! No check emits a warning at the moment (node-scoped `meshfox:var`s used
//! to, see SPEC.md "Block arguments and applications"); the type and the
//! `warnings` entry point stay so the CLI's stderr output and the MCP
//! `warnings` array keep their shape for the next diagnostic.
use crate::{Canvas, VarsError};

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Warning {
    pub code: &'static str,
    pub node_id: String,
    pub message: String,
}

pub fn warnings(_canvas: &Canvas) -> Result<Vec<Warning>, VarsError> {
    Ok(Vec::new())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vars;

    #[test]
    fn node_scoped_variables_do_not_warn() {
        let canvas = Canvas::from_markdown("<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n<!-- meshfox:var name=\"GLOBAL\" default=\"a\" -->\n## Child\n<!-- meshfox:node id=\"child\" -->\n<!-- meshfox:var name=\"LOCAL\" default=\"b\" -->\n").unwrap();
        assert!(warnings(&canvas).unwrap().is_empty());
        assert_eq!(vars::declared_vars(&canvas).unwrap().len(), 2);
    }
}
