//! TODO.canvas.md: "Мышь в панелях TUI" — clicking a block's own run
//! affordance in the document pane. The only thing that currently even
//! *looks* clickable there is a `button` fence's `▶ caption (r to run)`
//! marker (`markdown.rs`'s `BUTTON_LANG` branch) — every ordinary runnable
//! block has no inline run control at all, only the tree's `r`/`R`. This
//! test is written against clicking that marker running its own `deps=`
//! chain, same as pressing `r` on it would.

use std::time::Duration;

use crate::fixtures;
use crate::harness::TuiSession;

#[test]
#[ignore = "not implemented — see TODO.canvas.md's run/chain-button bullet"]
fn clicking_a_button_fences_marker_runs_its_deps_chain() {
    let (canvas_path, dir) = fixtures::write_fixture(fixtures::BUTTON_FENCE);
    let mut session = TuiSession::spawn(&canvas_path, dir, 30, 100);
    session
        .wait_for("Go", Duration::from_secs(5))
        .expect("the button fence's own caption should render");
    assert!(
        !session.screen_text().contains("tui-e2e-button-marker"),
        "nothing should have run yet"
    );

    let (row, col) = session.find("Go").expect("▶ Go marker");
    session.send_mouse_click(row, col);

    session
        .wait_for("tui-e2e-button-marker", Duration::from_secs(10))
        .expect("clicking the button marker should run its deps= chain, same as pressing r on it");
}
