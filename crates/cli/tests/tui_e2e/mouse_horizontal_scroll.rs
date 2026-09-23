//! TODO.canvas.md: "Мышь в панелях TUI" — `MouseEventKind::ScrollLeft`/
//! `ScrollRight` aren't matched anywhere in `App::on_mouse` (they fall
//! into its catch-all `_ => {}`). The document pane's own code lines
//! already wrap instead of clipping (confirmed directly — not a case this
//! gap actually affects), but the Output pane's run-output lines don't:
//! `ui.rs::render_output` slices the tail N *lines* and renders each one
//! with no `Paragraph::wrap`, so a single output line wider than the pane
//! is genuinely clipped with no way to see its tail today.
//!
//! A self-triggered run's own live stdout now also splices inline under
//! its own block in the Document pane (`App::on_run_event`'s own
//! `step_output` live splice, same posture the web UI already has) — and
//! that pane wraps instead of clipping, so the wide line's own tail would
//! show up there regardless of any Output-pane scrolling, defeating this
//! test's whole premise. Fullscreening the Output pane right after
//! opening it keeps the Document pane (and its own copy of the line) off
//! screen for the rest of the test, so `screen_text()` only ever reflects
//! the Output pane's own clip/scroll behavior.

use std::time::Duration;

use crate::fixtures;
use crate::harness::TuiSession;

#[test]
#[ignore = "not implemented — see TODO.canvas.md's horizontal-scroll bullet"]
fn scrolling_right_over_the_output_pane_reveals_a_clipped_lines_tail() {
    let (canvas_path, dir) = fixtures::write_fixture(fixtures::WIDE_OUTPUT_LINE);
    let mut session = TuiSession::spawn(&canvas_path, dir, 30, 100);
    session
        .wait_for("Root", Duration::from_secs(5))
        .expect("initial render");

    // A single-block run (this fixture's own chain is just one step) never
    // auto-expands the Output console (`App::begin_http_run`'s own "not
    // worth losing screen space over" gate needs >= 2 steps) — click the
    // collapsed strip open first, same as a real user would have to, so
    // the run's real stdout actually lands somewhere visible. Then `f`
    // fullscreens it (see the module doc comment above for why: keeps
    // the Document pane's own inline copy of the line off screen).
    let (out_row, out_col) = session.find("Output").expect("collapsed Output strip");
    session.send_mouse_click(out_row, out_col);
    session.send_keys("f");

    session.send_keys("r");
    session
        .wait_for("line-start-marker", Duration::from_secs(10))
        .expect("the block should run and print its own long line");
    assert!(
        !session.screen_text().contains("line-end-marker"),
        "the line's own tail shouldn't fit in a 100-column Output pane"
    );

    let (row, col) = session.find("line-start-marker").expect("the wide output line");
    for _ in 0..20 {
        session.send_mouse_scroll_horizontal(row, col, true); // true = right
    }
    std::thread::sleep(Duration::from_millis(100));

    assert!(
        session.screen_text().contains("line-end-marker"),
        "scrolling right over the Output pane should eventually bring the line's tail into view"
    );
}
