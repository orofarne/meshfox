//! TODO.canvas.md: "Ресайз панелей в TUI мышью" — the tree/document split
//! and the Output pane's own height used to be entirely fixed
//! (`ui::compute_layout`'s hardcoded `Percentage(30)/Percentage(70)` and
//! `OUTPUT_HEIGHT` constant), with no "drag the border" state at all.
//! `App::resize_handle_at`/`on_resize_drag` now hit-test a
//! `MouseEventKind::Down(Left)` against the seam between two panes and,
//! for the rest of the gesture (`Drag(Left)` until the matching `Up(Left)`),
//! translate the cursor's own position into a new `tree_width_pct`/
//! `output_height` on `App`.

use std::time::Duration;

use crate::fixtures;
use crate::harness::TuiSession;

#[test]
#[ignore = "pty-based e2e — run via `cargo test --test tui_e2e -- --ignored`"]
fn dragging_the_tree_document_seam_resizes_the_tree_pane() {
    let (canvas_path, dir) = fixtures::write_fixture(fixtures::SIMPLE_RUNNABLE);
    let mut session = TuiSession::spawn(&canvas_path, dir, 30, 100);
    session
        .wait_for("Root", Duration::from_secs(5))
        .expect("initial render");

    // The seam between the tree and document panes is where their own
    // adjacent right/left borders draw two vertical bars side by side —
    // the only place "││" appears on a plain two-node tree with no long
    // wrapped lines of its own.
    let (row, col) = session.find("││").expect("tree/document seam");

    // Drag it well to the right — same row (a horizontal drag), a large
    // enough jump that rounding to a whole percentage still lands far past
    // where it started.
    session.send_mouse_drag(row, col, row, 65);
    std::thread::sleep(Duration::from_millis(100));

    let (_, new_col) = session.find("││").expect("tree/document seam, now moved");
    assert!(
        new_col > col + 20,
        "dragging the seam right should widen the tree pane — was at column {col}, now at {new_col}"
    );
}

#[test]
#[ignore = "pty-based e2e — run via `cargo test --test tui_e2e -- --ignored`"]
fn dragging_the_output_seam_resizes_the_output_pane() {
    let (canvas_path, dir) = fixtures::write_fixture(fixtures::SIMPLE_RUNNABLE);
    let mut session = TuiSession::spawn(&canvas_path, dir, 30, 100);
    session
        .wait_for("Root", Duration::from_secs(5))
        .expect("initial render");

    // The Output pane's own title sits right on its top border row — one
    // row *above* it is the tree/document row's own bottom border, which
    // is the actual resize handle (Output's own top border/title row is
    // deliberately excluded — see `App::resize_handle_at`'s own doc
    // comment — so a click there keeps behaving as a plain focus click,
    // not a resize start).
    let (title_row, col) = session.find("Output").expect("Output pane title");
    let row = title_row - 1;

    // Drag the seam up — same column (a vertical drag) — growing the
    // Output pane at the tree/document row's expense.
    session.send_mouse_drag(row, col, row.saturating_sub(5), col);
    std::thread::sleep(Duration::from_millis(100));

    let (new_title_row, _) = session.find("Output").expect("Output pane title, now moved");
    assert!(
        new_title_row + 3 <= title_row,
        "dragging the seam up should grow the Output pane, moving its title row up — was at row {title_row}, now at {new_title_row}"
    );
}
