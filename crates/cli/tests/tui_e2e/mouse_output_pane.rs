//! TODO.canvas.md: "Мышь в панелях TUI (tree/document/output)" — the
//! Output-pane bullet (click-to-focus, `Tab`-focus, scroll, fullscreen
//! toggle). Implemented; these all pass now.

use std::time::Duration;

use crate::fixtures;
use crate::harness::TuiSession;

/// Mirrors `crates/cli/src/tui/theme.rs::ACCENT` — kept as a plain literal
/// here rather than importing it, since `crates/cli` has no `[lib]`
/// target for an integration test to link against (only `[[bin]]`); if
/// `theme::ACCENT` is ever retuned, update this too.
const ACCENT: vt100::Color = vt100::Color::Rgb(0xff, 0x6e, 0x15);

#[test]
#[ignore = "pty-based e2e — run via `cargo test --test tui_e2e -- --ignored`"]
fn clicking_inside_the_output_pane_focuses_it() {
    let (canvas_path, dir) = fixtures::write_fixture(fixtures::SIMPLE_RUNNABLE);
    let mut session = TuiSession::spawn(&canvas_path, dir, 30, 100);
    session
        .wait_for("Output", Duration::from_secs(5))
        .expect("initial render");

    let (row, col) = session.find("Output").expect("Output pane title");
    session.send_mouse_click(row, col);

    // Once Output can be focused, its own border (title included, same as
    // `pane_border`'s ACCENT/BORDER split for Tree/Document today) should
    // switch to the focused color.
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(
        session.fgcolor_at(row, col),
        Some(ACCENT),
        "Output pane's title should render in the focused (ACCENT) color after a click on it"
    );
}

#[test]
#[ignore = "pty-based e2e — run via `cargo test --test tui_e2e -- --ignored`"]
fn tab_cycles_focus_through_the_output_pane_too() {
    let (canvas_path, dir) = fixtures::write_fixture(fixtures::SIMPLE_RUNNABLE);
    let mut session = TuiSession::spawn(&canvas_path, dir, 30, 100);
    session
        .wait_for("Output", Duration::from_secs(5))
        .expect("initial render");
    let (row, col) = session.find("Output").expect("Output pane title");

    // Today Tab only cycles Tree <-> Document (2 stops); once Output joins
    // the cycle, some bounded number of Tabs must land focus on it.
    for _ in 0..4 {
        session.send_keys("\t");
        std::thread::sleep(Duration::from_millis(50));
        if session.fgcolor_at(row, col) == Some(ACCENT) {
            return;
        }
    }
    panic!("Tab never focused the Output pane");
}

#[test]
#[ignore = "pty-based e2e — run via `cargo test --test tui_e2e -- --ignored`"]
fn scrolling_over_the_output_pane_scrolls_its_own_content() {
    // A run whose output has more lines than the (small) Output pane can
    // show at once — scrolling over it should reveal an earlier line that
    // was scrolled out, without touching the tree/document panes.
    let body = concat!(
        "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
        "```bash name=\"root\" cache\nfor i in $(seq 1 40); do echo tui-e2e-line-$i; done\n```\n",
    );
    let (canvas_path, dir) = fixtures::write_fixture(body);
    let mut session = TuiSession::spawn(&canvas_path, dir, 30, 100);
    session
        .wait_for("Root", Duration::from_secs(5))
        .expect("initial render");
    session.send_keys("r");
    session
        .wait_for("tui-e2e-line-40", Duration::from_secs(10))
        .expect("run should finish and show its last line");
    assert!(
        !session.screen_text().contains("tui-e2e-line-1\n"),
        "the very first line should already be scrolled out of the small Output pane"
    );

    let (row, col) = session.find("Output").expect("Output pane title");
    session.send_mouse_scroll(row + 2, col, false); // false = up, toward earlier lines
    std::thread::sleep(Duration::from_millis(100));
    assert!(
        session.screen_text().contains("tui-e2e-line-1"),
        "scrolling up over the Output pane should bring an earlier line back into view"
    );
}

#[test]
#[ignore = "pty-based e2e — run via `cargo test --test tui_e2e -- --ignored`"]
fn double_clicking_the_output_title_expands_it_to_fullscreen_and_back() {
    let (canvas_path, dir) = fixtures::write_fixture(fixtures::SIMPLE_RUNNABLE);
    let mut session = TuiSession::spawn(&canvas_path, dir, 30, 100);
    session
        .wait_for("Root", Duration::from_secs(5))
        .expect("initial render");
    assert!(
        session.screen_text().contains("Root"),
        "the tree is visible before expanding Output"
    );

    let (row, col) = session.find("Output").expect("Output pane title");
    session.send_mouse_double_click(row, col);
    std::thread::sleep(Duration::from_millis(100));
    assert!(
        !session.screen_text().contains("Root"),
        "the tree pane should be hidden while Output is fullscreen"
    );

    // The title moved (fullscreen Output starts at row 0) — re-locate it
    // rather than reusing the pre-fullscreen coordinate, same as a real
    // user would have to click wherever it actually is now.
    let (row, col) = session.find("Output").expect("Output pane title, now fullscreen");
    session.send_mouse_double_click(row, col);
    std::thread::sleep(Duration::from_millis(100));
    assert!(
        session.screen_text().contains("Root"),
        "double-clicking again should restore the normal three-pane layout"
    );
}
