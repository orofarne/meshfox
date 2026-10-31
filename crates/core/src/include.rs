//! `meshfox_model::include` plus its disk-backed conveniences (the model
//! itself reads files only through a [`FileSource`](crate::file_source::FileSource)).

pub use meshfox_model::include::*;

use crate::canvas::{Canvas, NodeType};
use std::path::Path;

/// Returns a new `Canvas` with every `include` node's target dumped into
/// its own body text (see the module doc comment). `base_path` is the file
/// `canvas` was parsed from, used to resolve a relative include target
/// against its directory.
pub fn resolve(canvas: &Canvas, base_path: &Path) -> Result<Canvas, IncludeError> {
    resolve_with_access(canvas, base_path, &access_for(canvas, base_path))
}

/// The file access `canvas`'s `include` nodes are confined by. Building it
/// re-reads every configuration file and the answer cache, so a document
/// without an `include` node (the common case, and the one a polling caller
/// hits twice a second) gets an empty one instead: nothing consults it.
fn access_for(canvas: &Canvas, base_path: &Path) -> crate::FileAccess {
    if canvas
        .nodes
        .iter()
        .any(|n| n.node_type == NodeType::Include)
    {
        crate::FileAccess::for_canvas_path(canvas, base_path)
    } else {
        crate::FileAccess::none()
    }
}

/// Lists every `include` node declared directly in `canvas` (see
/// `IncludeInfo`), without ever reading its target's content — `base_path`
/// is the file `canvas` was parsed from, same as `resolve`.
pub fn list_includes(canvas: &Canvas, base_path: &Path) -> Vec<IncludeInfo> {
    list_includes_with_access(canvas, base_path, &access_for(canvas, base_path))
}
