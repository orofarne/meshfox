//! End-to-end test for `meshfox pdf`: invokes the built binary against small,
//! self-contained canvas fixtures and checks the PDF bytes it produces — the
//! parts a unit test inside `crates/cli/src/pdf/render.rs` can't reach (CLI
//! arg parsing, the `--out`/`--force` clobber guard, and driving a real
//! browser end to end).
//!
//! Best-effort, same convention as `site_template_layout.rs`'s own
//! `playwright_core_entry()`: skips (doesn't fail the suite) when no
//! Chrome/Chromium/Edge can be found on this machine, rather than
//! triggering `meshfox pdf`'s own automatic Chromium download from an
//! ordinary `cargo test` run — real browser automation is useful here, but
//! shouldn't become a hard requirement (or a surprise network fetch) just to
//! run the test suite somewhere with no browser toolchain set up.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn unique_dir(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("meshfox-pdf-cmd-test-{tag}-{nanos}-{n}"))
}

fn write_file(path: &Path, contents: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, contents).unwrap();
}

fn meshfox() -> Command {
    Command::new(env!("CARGO_BIN_EXE_meshfox"))
}

/// `None` (meaning "skip this test") when no system Chrome/Chromium/Edge is
/// discoverable — the same check `pdf::browser::launch` makes first,
/// checked here too so a machine with none never triggers that function's
/// own automatic Chromium download just by running the test suite.
fn system_browser_available() -> bool {
    headless_chrome::browser::default_executable().is_ok()
}

fn page_count(pdf_bytes: &[u8]) -> usize {
    lopdf::Document::load_mem(pdf_bytes)
        .expect("output must be a well-formed PDF")
        .get_pages()
        .len()
}

const OUTLINE_CANVAS: &str = concat!(
    "<!-- meshfox:canvas -->\n",
    "# Root\n",
    "<!-- meshfox:node id=\"root\" -->\n",
    "\n",
    "intro text\n",
    "\n",
    "## Child\n",
    "<!-- meshfox:node id=\"child\" tags=\"demo\" -->\n",
    "\n",
    "some body content\n",
);

/// Same content as `OUTLINE_CANVAS`, but every node also carries a real
/// position and there's a `meshfox:edge` cross-reference between them — the
/// flow-order body text is unchanged from `OUTLINE_CANVAS`, only a canvas
/// page should ever be added ahead of it.
const POSITIONED_CANVAS: &str = concat!(
    "<!-- meshfox:canvas -->\n",
    "# Root\n<!-- meshfox:node id=\"root\" x=0 y=0 w=200 h=100 -->\n",
    "\n",
    "intro text\n",
    "\n",
    "## Child\n<!-- meshfox:node id=\"child\" tags=\"demo\" x=300 y=0 w=200 h=100 -->\n",
    "\n",
    "some body content\n",
    "<!-- meshfox:edge from=\"root\" to=\"child\" -->\n",
);

#[test]
fn renders_a_pdf_for_a_plain_outline_canvas() {
    if !system_browser_available() {
        eprintln!("skipping: no system Chrome/Chromium/Edge found");
        return;
    }

    let canvas_path = unique_dir("canvas").join("doc.canvas.md");
    write_file(&canvas_path, OUTLINE_CANVAS);
    let out_path = unique_dir("out").join("doc.pdf");

    let status = meshfox()
        .arg("pdf")
        .arg(&canvas_path)
        .arg("--out")
        .arg(&out_path)
        .status()
        .expect("failed to run meshfox");
    assert!(status.success());

    let bytes = std::fs::read(&out_path).unwrap();
    assert!(bytes.starts_with(b"%PDF-"), "output must be a PDF");
    assert!(page_count(&bytes) >= 1);

    let _ = std::fs::remove_dir_all(canvas_path.parent().unwrap());
    let _ = std::fs::remove_dir_all(out_path.parent().unwrap());
}

/// `meshfox pdf <canvas> [--mode <m>] --out <out>`, returning the produced
/// PDF's page count.
fn run_pdf(canvas_path: &Path, out_path: &Path, mode: Option<&str>) -> usize {
    let mut cmd = meshfox();
    cmd.arg("pdf").arg(canvas_path).arg("--out").arg(out_path);
    if let Some(mode) = mode {
        cmd.arg("--mode").arg(mode);
    }
    let status = cmd.status().expect("failed to run meshfox");
    assert!(status.success());
    page_count(&std::fs::read(out_path).unwrap())
}

#[test]
fn a_canvas_with_no_authored_position_still_gets_a_default_mode_canvas_page() {
    // The canvas page is never conditional on a real, authored position —
    // a node with none is auto-laid-out (same algorithm the live web UI's
    // own `autolayout.ts` uses), so the default output is always exactly
    // one canvas page plus the document page(s), even for a canvas nobody
    // has ever dragged a node in.
    if !system_browser_available() {
        eprintln!("skipping: no system Chrome/Chromium/Edge found");
        return;
    }

    let dir = unique_dir("canvas-default-composition");
    let canvas_path = dir.join("doc.canvas.md");
    write_file(&canvas_path, OUTLINE_CANVAS);

    let canvas_only_pages = run_pdf(&canvas_path, &dir.join("canvas-only.pdf"), Some("canvas"));
    let document_only_pages = run_pdf(
        &canvas_path,
        &dir.join("document-only.pdf"),
        Some("document"),
    );
    let default_pages = run_pdf(&canvas_path, &dir.join("default.pdf"), None);

    assert_eq!(
        canvas_only_pages, 1,
        "--mode canvas is always exactly one page"
    );
    assert_eq!(
        default_pages,
        canvas_only_pages + document_only_pages,
        "default mode is canvas + document, always"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn authored_positions_do_not_change_the_default_mode_page_count() {
    // A real position only changes *where* a node's box lands on the
    // canvas page, never *whether* there is one — so the same content with
    // vs. without authored positions must produce the same page count
    // either way.
    if !system_browser_available() {
        eprintln!("skipping: no system Chrome/Chromium/Edge found");
        return;
    }

    let plain_canvas_path = unique_dir("canvas-plain").join("doc.canvas.md");
    write_file(&plain_canvas_path, OUTLINE_CANVAS);
    let plain_pages = run_pdf(
        &plain_canvas_path,
        &unique_dir("out-plain").join("doc.pdf"),
        None,
    );

    let positioned_canvas_path = unique_dir("canvas-positioned").join("doc.canvas.md");
    write_file(&positioned_canvas_path, POSITIONED_CANVAS);
    let positioned_pages = run_pdf(
        &positioned_canvas_path,
        &unique_dir("out-positioned").join("doc.pdf"),
        None,
    );

    assert_eq!(plain_pages, positioned_pages);

    let _ = std::fs::remove_dir_all(plain_canvas_path.parent().unwrap());
    let _ = std::fs::remove_dir_all(positioned_canvas_path.parent().unwrap());
}

#[test]
fn mode_document_page_count_is_unaffected_by_authored_positions() {
    if !system_browser_available() {
        eprintln!("skipping: no system Chrome/Chromium/Edge found");
        return;
    }

    let plain_canvas_path = unique_dir("canvas-mode-doc-plain").join("doc.canvas.md");
    write_file(&plain_canvas_path, OUTLINE_CANVAS);
    let plain_pages = run_pdf(
        &plain_canvas_path,
        &unique_dir("out-mode-doc-plain").join("doc.pdf"),
        Some("document"),
    );

    let positioned_canvas_path = unique_dir("canvas-mode-doc-positioned").join("doc.canvas.md");
    write_file(&positioned_canvas_path, POSITIONED_CANVAS);
    let positioned_pages = run_pdf(
        &positioned_canvas_path,
        &unique_dir("out-mode-doc-positioned").join("doc.pdf"),
        Some("document"),
    );

    assert_eq!(plain_pages, positioned_pages);

    let _ = std::fs::remove_dir_all(plain_canvas_path.parent().unwrap());
    let _ = std::fs::remove_dir_all(positioned_canvas_path.parent().unwrap());
}

#[test]
fn refuses_to_clobber_an_existing_out_file_without_force() {
    // No browser needed: the clobber check runs before `meshfox pdf` ever
    // touches one.
    let canvas_path = unique_dir("canvas-clobber").join("doc.canvas.md");
    write_file(&canvas_path, OUTLINE_CANVAS);
    let out_path = unique_dir("out-clobber").join("doc.pdf");
    write_file(&out_path, "leftover, not a real PDF");

    let without_force = meshfox()
        .arg("pdf")
        .arg(&canvas_path)
        .arg("--out")
        .arg(&out_path)
        .status()
        .expect("failed to run meshfox");
    assert!(!without_force.success());
    assert_eq!(
        std::fs::read_to_string(&out_path).unwrap(),
        "leftover, not a real PDF",
        "must survive the refused run"
    );

    let _ = std::fs::remove_dir_all(canvas_path.parent().unwrap());
    let _ = std::fs::remove_dir_all(out_path.parent().unwrap());
}

#[test]
fn defaults_the_out_path_to_the_canvas_filename_with_a_pdf_extension() {
    if !system_browser_available() {
        eprintln!("skipping: no system Chrome/Chromium/Edge found");
        return;
    }

    let dir = unique_dir("canvas-default-out");
    let canvas_path = dir.join("doc.canvas.md");
    write_file(&canvas_path, OUTLINE_CANVAS);

    let status = meshfox()
        .arg("pdf")
        .arg(&canvas_path)
        .status()
        .expect("failed to run meshfox");
    assert!(status.success());

    let default_out = dir.join("doc.canvas.pdf");
    assert!(
        default_out.exists(),
        "expected {} to exist",
        default_out.display()
    );
    assert!(std::fs::read(&default_out).unwrap().starts_with(b"%PDF-"));

    let _ = std::fs::remove_dir_all(&dir);
}
