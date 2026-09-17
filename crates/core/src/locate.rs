//! Finding a node in the primary document — shared by every consumer that
//! needs to both *read* a specific node's own text and, later, *write* an
//! edit or a cached block-output back to it.

use crate::canvas::Canvas;
use crate::mdcanvas::ParseError;
use thiserror::Error;

/// Where a node's content lives, and its text as of the moment this was
/// called.
#[derive(Debug)]
pub struct LocatedNode {
    pub raw: String,
    pub local_id: String,
}

#[derive(Debug, Error)]
pub enum LocateError {
    #[error(transparent)]
    Parse(#[from] ParseError),
    #[error("no node {0:?}")]
    NotFound(String),
}

/// Finds `id` in `primary_raw` (the document's own already-loaded text).
pub fn locate_node(primary_raw: &str, id: &str) -> Result<LocatedNode, LocateError> {
    let primary = Canvas::from_markdown(primary_raw)?;
    if primary.node(id).is_some() {
        return Ok(LocatedNode {
            raw: primary_raw.to_string(),
            local_id: id.to_string(),
        });
    }
    Err(LocateError::NotFound(id.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_a_node_in_the_primary_document_directly() {
        let raw = "<!-- meshfox:canvas -->\n# Base\n<!-- meshfox:node id=\"base\" -->\n\nbody\n";
        let located = locate_node(raw, "base").unwrap();
        assert_eq!(located.local_id, "base");
        assert_eq!(located.raw, raw);
    }

    #[test]
    fn errors_on_an_unknown_id() {
        let raw = "<!-- meshfox:canvas -->\n# Base\n<!-- meshfox:node id=\"base\" -->\n\nbody\n";
        let err = locate_node(raw, "nope").unwrap_err();
        assert!(matches!(err, LocateError::NotFound(id) if id == "nope"));
    }
}
