//! TODO.canvas.md: "Мышь в панелях TUI" — `App::on_mouse` is currently a
//! no-op whenever a modal (`var_form`/`block_picker`) is open
//! (`app.rs:705-707`), so today only `j`/`k`/arrows can move a modal
//! list's own selection. This covers the block picker (opened by `r` on a
//! node with more than one runnable block) — clicking a row should select
//! it, same as `j`/`k` would.

use std::time::Duration;

use crate::fixtures;
use crate::harness::TuiSession;

#[test]
#[ignore = "not implemented — see TODO.canvas.md's modals bullet"]
fn clicking_a_row_in_the_block_picker_selects_it() {
    let (canvas_path, dir) = fixtures::write_fixture(fixtures::TWO_BLOCKS);
    let mut session = TuiSession::spawn(&canvas_path, dir, 30, 100);
    session
        .wait_for("Root", Duration::from_secs(5))
        .expect("initial render");

    session.send_keys("r");
    session
        .wait_for("which block?", Duration::from_secs(5))
        .expect("2 runnable blocks should open the picker");
    // "alpha"/"beta" alone would also match the document pane's own
    // `┌─ bash · alpha ──` fence headers, which sit above the picker in
    // row-major scan order — the `  [cache]` badge suffix disambiguates
    // the picker's own row text from that.
    let (alpha_row, alpha_col) = session.find("alpha  [cache]").expect("alpha row in the picker");
    assert!(
        session.inverse_at(alpha_row, alpha_col),
        "alpha (the first/default row) should start out selected"
    );

    let (beta_row, beta_col) = session.find("beta  [cache]").expect("beta row in the picker");
    session.send_mouse_click(beta_row, beta_col);
    std::thread::sleep(Duration::from_millis(100));
    assert!(
        session.inverse_at(beta_row, beta_col),
        "clicking beta's own row should select it"
    );
    assert!(
        !session.inverse_at(alpha_row, alpha_col),
        "alpha should no longer be the selected row"
    );

    session.send_keys("\r");
    session
        .wait_for("tui-e2e-beta-output", Duration::from_secs(10))
        .expect("confirming the click-selected row should run beta, not alpha");
}
