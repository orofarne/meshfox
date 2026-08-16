//! Drives a real headless Chrome/Chromium to print a local HTML file to PDF
//! bytes — the whole reason `meshfox pdf` exists (see the parent module's
//! own doc comment): a browser already gets Markdown/CSS layout right, so
//! there's no reason to re-derive it by hand.
//!
//! Browser sourcing, in order: a system Chrome/Chromium/Edge install first
//! (`headless_chrome::browser::default_executable()` — checks the `CHROME`
//! env var, common binary names on `PATH`, then well-known install
//! locations); if none is found, `headless_chrome`'s own `fetch` feature
//! (enabled on this crate's dependency) downloads a pinned Chromium build
//! into its own managed cache directory the first time this ever runs on a
//! given machine (`Browser::new` with `LaunchOptions { path: None, .. }`
//! triggers this automatically — see that crate's `Process::new`). Either
//! way, the caller never has to distinguish the two paths: `launch()`
//! returns a driveable `Browser` regardless of which one supplied the
//! executable.

use headless_chrome::types::PrintToPdfOptions;
use headless_chrome::{Browser, LaunchOptions, Tab};
use std::path::Path;
use std::time::{Duration, Instant};

/// How long to wait for a local file's `document.readyState` to reach
/// `"complete"` before giving up — see `wait_for_load` for why this is
/// polled instead of using `Tab::wait_until_navigated`.
const LOAD_TIMEOUT: Duration = Duration::from_secs(20);
/// How much longer, on top of `LOAD_TIMEOUT`, to wait for
/// `diagram.html.tera`'s own layout script to finish (see
/// `wait_for_diagram_size`) — real body content (images especially) can
/// take a moment longer than a bare page load.
const DIAGRAM_LAYOUT_TIMEOUT: Duration = Duration::from_secs(20);

/// 96 CSS px = 1in — the standard CSS/print ratio, so the diagram page
/// prints at true 1:1 scale (see the parent module's doc comment) rather
/// than an arbitrary scale factor.
const PX_PER_IN: f64 = 96.0;
/// Hard cap per printed page dimension, in inches (200cm) — guards against
/// one stray huge coordinate producing a pathological page; crops rather
/// than scales to fit (a real follow-up if it ever comes up in practice,
/// not needed for realistic hand-placed canvases).
const MAX_DIMENSION_IN: f64 = 200.0 / 2.54;

pub fn launch() -> Result<Browser, String> {
    let path = headless_chrome::browser::default_executable().ok();
    let launch_options = LaunchOptions::default_builder()
        .path(path)
        .build()
        .map_err(|e| format!("chrome launch options: {e}"))?;
    Browser::new(launch_options)
        .map_err(|e| format!("failed to launch a Chrome/Chromium browser: {e}"))
}

/// Navigates a fresh tab to `html_path` (a local file) and prints it to PDF
/// bytes with `options`.
pub fn print_file(
    browser: &Browser,
    html_path: &Path,
    options: PrintToPdfOptions,
) -> Result<Vec<u8>, String> {
    let tab = browser
        .new_tab()
        .map_err(|e| format!("failed to open a browser tab: {e}"))?;
    navigate(&tab, html_path)?;
    tab.print_to_pdf(Some(options))
        .map_err(|e| format!("failed to print {} to PDF: {e}", html_path.display()))
}

/// Navigates to `diagram.html.tera`'s own rendered output and prints it —
/// unlike `print_file`, the paper size isn't known ahead of time: it comes
/// from that page's own client-side layout script (real node boxes, sized
/// from their actual rendered content — see `render`'s own module doc
/// comment for why this can't be precomputed in Rust any more), read back
/// via `window.__meshfoxDiagramWidth`/`Height` once that script signals
/// it's done.
pub fn print_diagram(browser: &Browser, html_path: &Path) -> Result<Vec<u8>, String> {
    let tab = browser
        .new_tab()
        .map_err(|e| format!("failed to open a browser tab: {e}"))?;
    navigate(&tab, html_path)?;
    let (width_px, height_px) = wait_for_diagram_size(&tab)?;
    let width_in = (width_px / PX_PER_IN).min(MAX_DIMENSION_IN);
    let height_in = (height_px / PX_PER_IN).min(MAX_DIMENSION_IN);
    let options = PrintToPdfOptions {
        paper_width: Some(width_in),
        paper_height: Some(height_in),
        margin_top: Some(0.0),
        margin_bottom: Some(0.0),
        margin_left: Some(0.0),
        margin_right: Some(0.0),
        print_background: Some(true),
        ..Default::default()
    };
    tab.print_to_pdf(Some(options))
        .map_err(|e| format!("failed to print {} to PDF: {e}", html_path.display()))
}

fn navigate(tab: &Tab, html_path: &Path) -> Result<(), String> {
    let url = format!("file://{}", html_path.display());
    tab.navigate_to(&url)
        .map_err(|e| format!("failed to navigate to {}: {e}", html_path.display()))?;
    wait_for_load(tab).map_err(|e| format!("failed to load {}: {e}", html_path.display()))
}

/// Polls `document.readyState`/`location.href` directly instead of using
/// `Tab::wait_until_navigated` (which waits for a `Page.lifecycleEvent`
/// named `"networkAlmostIdle"` — a real page's own network traffic settling
/// down, which a zero-request local `file://` document apparently never
/// fires reliably: verified against Chrome 151, where a plain static local
/// HTML file's own `wait_until_navigated` call never returns, timing out
/// every time). `location.href` guards a narrow race right after
/// `new_tab()`: a fresh tab's own initial `about:blank` document is already
/// `"complete"` before our own `navigate_to` call has actually taken effect,
/// so checking `readyState` alone could report done before the real
/// navigation even started.
fn wait_for_load(tab: &Tab) -> Result<(), String> {
    let deadline = Instant::now() + LOAD_TIMEOUT;
    loop {
        let ready = tab
            .evaluate(
                "document.readyState === 'complete' && location.href !== 'about:blank'",
                false,
            )
            .ok()
            .and_then(|obj| obj.value)
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if ready {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err("timed out waiting for the page to finish loading".to_string());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Polls for `diagram.html.tera`'s own layout script to publish
/// `window.__meshfoxDiagramWidth`/`Height` (set only once its whole
/// measure-then-layout pass — including waiting for every real `<img>` in a
/// node's body to finish loading — has run to completion), returning that
/// real page size in px.
fn wait_for_diagram_size(tab: &Tab) -> Result<(f64, f64), String> {
    let deadline = Instant::now() + LOAD_TIMEOUT + DIAGRAM_LAYOUT_TIMEOUT;
    loop {
        let width = read_number(tab, "window.__meshfoxDiagramWidth");
        let height = read_number(tab, "window.__meshfoxDiagramHeight");
        if let (Some(width), Some(height)) = (width, height) {
            return Ok((width, height));
        }
        if Instant::now() >= deadline {
            return Err(
                "timed out waiting for the canvas page's own layout script to finish".to_string(),
            );
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn read_number(tab: &Tab, expression: &str) -> Option<f64> {
    tab.evaluate(expression, false)
        .ok()
        .and_then(|obj| obj.value)
        .and_then(|v| v.as_f64())
}

/// `PrintToPdfOptions` for the document (flow) page: CSS drives the page
/// size (`document.html.tera`'s own `@page { size: a4; margin: 2cm; }`)
/// rather than any fixed dimension here.
pub fn document_options() -> PrintToPdfOptions {
    PrintToPdfOptions {
        prefer_css_page_size: Some(true),
        print_background: Some(true),
        ..Default::default()
    }
}
