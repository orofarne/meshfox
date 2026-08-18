//! `meshfox pdf`: `Canvas -> PDF bytes` via a real (headless) browser,
//! rather than a hand-rolled layout/rendering engine — an earlier attempt
//! embedded the Typst engine and hand-rolled Markdown->Typst rendering
//! plus hand-drawn vector graphics for a spatial overview page, and got
//! layout fidelity wrong in exactly the way `meshfox static`'s own design
//! notes already warn against (see `crates/core/src/staticgen.rs`'s module
//! doc comment): a browser already lays out flex/flow content, tables, code
//! wrapping, and page breaks correctly, so there's no reason to re-derive
//! any of that by hand. `meshfox pdf` instead builds on the same,
//! already-tested `meshfox_core::staticgen` data `meshfox static` uses, and
//! gets a real Chrome/Chromium (`browser`) to print it.
//!
//! Two HTML pages are rendered (`render`) and printed independently
//! (`browser::print_file`, one `Page.printToPDF` call each), then stitched
//! into one output PDF (`merge`) — rather than gambling on Chrome's
//! headless print engine supporting differently-sized `@page` sections
//! within a single print job, which isn't reliably verifiable:
//!   - **document page(s)**: the full node tree in flow/document order
//!     (headings by depth, tags, body, target, recursing into children) —
//!     ignores real canvas position entirely, standard A4 pagination.
//!   - **canvas page**: a single custom-sized page, printed at true 1:1
//!     CSS-px scale (the bounding box of every node's own box, no scaling
//!     to fit a fixed paper size) — every node's own box, full body always
//!     shown (never folded, regardless of the document's own fold
//!     settings — a printed page has no click to unfold later), plus
//!     connectors for both structural (parent -> child) and `meshfox:edge`
//!     cross-reference relationships. A node with a real, authored position
//!     keeps its `x`/`y`/`width` exactly; height is always auto-sized to
//!     the node's own real content instead, on every node, authored or
//!     not — a fixed `height=` a canvas author set back when this box only
//!     ever showed a title isn't allowed to clip the real body content it
//!     shows now. That auto-sizing (like the auto-placement for anything
//!     with no real position at all) is computed client-side, in
//!     `diagram.html.tera`'s own script, against the real browser-rendered
//!     content height — not guessed at in Rust (see `render`'s own module
//!     doc comment for why a fixed/guessed height stopped being an option
//!     once a box started showing real body content). So this page always
//!     has something worth printing, the same way `meshfox view` always
//!     shows *something*, not just for a canvas someone has hand-positioned
//!     every node of.

mod browser;
mod merge;
pub(crate) mod render;

use meshfox_core::staticgen;
use meshfox_core::Canvas;
use std::path::Path;

/// Restricts `generate` to just one of the two page kinds
/// (`meshfox pdf --mode <canvas|document>`) — `None` (the default): both.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Mode {
    /// Just the spatial canvas overview page — every node at its own box
    /// (real position where authored, auto-laid-out otherwise), no
    /// document-flow pages at all.
    Canvas,
    /// Just the flow document page(s) — no canvas page.
    Document,
}

/// Renders `canvas` to PDF bytes. `canvas_dir` is the canvas file's own
/// directory (confinement boundary for local image references, same as
/// `static`). `mode` restricts the output to a single page kind — see
/// `Mode`.
pub fn generate(canvas: &Canvas, canvas_dir: &Path, mode: Option<Mode>) -> Result<Vec<u8>, String> {
    let (site, assets) = staticgen::build(canvas, canvas_dir, None);

    let work_dir = render::temp_work_dir();
    std::fs::create_dir_all(&work_dir)
        .map_err(|e| format!("failed to create a temp directory: {e}"))?;
    let result = generate_in(&site, &assets, mode, &work_dir);
    std::fs::remove_dir_all(&work_dir).ok();
    result
}

fn generate_in(
    site: &staticgen::SiteData,
    assets: &[staticgen::Asset],
    mode: Option<Mode>,
    work_dir: &Path,
) -> Result<Vec<u8>, String> {
    render::copy_assets(assets, work_dir)?;
    render::copy_fonts(work_dir)?;

    let browser = browser::launch()?;

    let want_canvas = mode != Some(Mode::Document);
    let want_document = mode != Some(Mode::Canvas);

    let canvas_pdf = if want_canvas {
        let canvas_html = render::write_diagram_page(site, work_dir)?;
        Some(browser::print_diagram(&browser, &canvas_html)?)
    } else {
        None
    };

    let document_pdf = if want_document {
        let document_html = render::write_document_page(site, work_dir)?;
        Some(browser::print_file(
            &browser,
            &document_html,
            browser::document_options(),
        )?)
    } else {
        None
    };

    match (canvas_pdf, document_pdf) {
        (Some(canvas), Some(document)) => merge::concat(&[canvas, document]),
        (Some(canvas), None) => Ok(canvas),
        (None, Some(document)) => Ok(document),
        (None, None) => unreachable!("want_canvas || want_document is always true"),
    }
}
