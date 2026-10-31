//! The meshfox document model: the `.canvas.md` format itself — parsing and
//! surgical patching, runnable-fence scanning, cached-output regions, the
//! Starlark constraint/render passes — with no filesystem, process or
//! operating-system dependency of its own. Anything it needs from outside
//! comes through [`file_source::FileSource`] (files) and [`host`] (the clock
//! and randomness), so it builds for `wasm32-unknown-unknown` as it is.
//!
//! `meshfox-core` re-exports every module here under its old path and adds
//! the disk-backed conveniences on top.

pub mod attrs;
pub mod block_refs;
pub mod body_rev;
pub mod canvas;
pub mod color_spans;
pub mod comment;
pub mod constraint;
pub mod fence;
pub mod file_source;
pub mod host;
pub mod image_attrs;
pub mod include;
pub mod locate;
mod markdown_fragment;
pub mod mdcanvas;
pub mod merge;
pub mod options;
pub mod output;
pub mod render;
pub mod slug;
pub mod starlark_render;
pub mod subsup;
pub mod suggest;
pub mod svg;
pub mod tag_colors;
pub mod timestamp;
pub mod tree;

#[cfg(test)]
mod test_disk;
