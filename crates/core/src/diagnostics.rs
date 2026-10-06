//! Non-failing validation diagnostics with stable machine-readable codes.
use crate::{vars, Canvas, VarsError};

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Warning {
    pub code: &'static str,
    pub node_id: String,
    pub message: String,
}

pub fn warnings(canvas: &Canvas) -> Result<Vec<Warning>, VarsError> {
    let mut warnings = Vec::new();
    for node in &canvas.nodes {
        if node.parent.is_some() {
            for decl in vars::scan_var_decls(&node.text)? {
                warnings.push(Warning {
                    code: "deprecated-node-scoped-var",
                    node_id: node.id.clone(),
                    message: format!("node-scoped meshfox:var {:?} is deprecated; use meshfox:arg before its runnable block (existing behavior is preserved)", decl.name),
                });
            }
        }
    }
    Ok(warnings)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn warns_only_for_real_local_variables_without_changing_them() {
        let canvas = Canvas::from_markdown("<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n<!-- meshfox:var name=\"GLOBAL\" default=\"a\" -->\n## Child\n<!-- meshfox:node id=\"child\" -->\n<!-- meshfox:var name=\"LOCAL\" default=\"b\" -->\n```text\n<!-- meshfox:var name=\"EXAMPLE\" -->\n```\n").unwrap();
        let before = vars::declared_vars(&canvas).unwrap();
        let warnings = warnings(&canvas).unwrap();
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].code, "deprecated-node-scoped-var");
        assert_eq!(warnings[0].node_id, "child");
        assert_eq!(before, vars::declared_vars(&canvas).unwrap());
    }
}
