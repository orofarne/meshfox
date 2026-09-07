//! TODO.canvas.md: "Мышь в панелях TUI" — `App::on_mouse` only handles
//! `MouseEventKind::Down(Left)` + scroll today; `Drag`, a second `Down` in
//! the same place (double-click — a real terminal never sends this as its
//! own event, an app has to detect two `Down`s close together itself),
//! and `Up` are all unhandled. The checklist item deliberately left the
//! exact scope open ("нужно решить область"); this file covers the one
//! sub-case that's well-scoped and clearly valuable on its own:
//! double-clicking a tree row runs its node's own default block, mirroring
//! a file manager's double-click-to-open convention. `Drag`
//! (text-selection in the document pane, or drag-to-resize — see the
//! separate, lower-priority "Ресайз панелей в TUI мышью" TODO node) and a
//! bare `Up` (press-then-drag-away-to-cancel, matching how an ordinary UI
//! button behaves) are intentionally left for whenever that scope gets
//! decided, not sketched here.

use std::time::Duration;

use crate::fixtures;
use crate::harness::TuiSession;

#[test]
#[ignore = "not implemented — see TODO.canvas.md's drag/double-click/mouse-up bullet"]
fn double_clicking_a_tree_row_runs_its_default_block() {
    let (canvas_path, dir) = fixtures::write_fixture(fixtures::SIMPLE_RUNNABLE);
    let mut session = TuiSession::spawn(&canvas_path, dir, 30, 100);
    session
        .wait_for("Leaf", Duration::from_secs(5))
        .expect("initial render");
    assert!(
        !session.screen_text().contains("tui-e2e-marker-output"),
        "nothing should have run yet"
    );

    let (row, col) = session.find("Leaf").expect("Leaf's own tree row");
    session.send_mouse_double_click(row, col);

    session
        .wait_for("tui-e2e-marker-output", Duration::from_secs(10))
        .expect("double-clicking a tree row should run its node's own default block");
}
