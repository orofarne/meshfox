//! TODO.canvas.md: "Мышь в панелях TUI" — `MouseEventKind::ScrollLeft`/
//! `ScrollRight` aren't matched anywhere in `App::on_mouse` (they fall
//! into its catch-all `_ => {}`). The document pane's own code lines
//! already wrap instead of clipping (confirmed directly — not a case this
//! gap actually affects), but the Output pane's run-output lines don't:
//! `ui.rs::render_output` slices the tail N *lines* and renders each one
//! with no `Paragraph::wrap`, so a single output line wider than the pane
//! is genuinely clipped with no way to see its tail today.

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
