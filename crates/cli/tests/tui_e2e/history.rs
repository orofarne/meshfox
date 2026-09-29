//! `H` history view (`App::open_history_view`) — lists the worker's undo
//! log and `enter` jumps the canvas to the selected step via
//! `POST /api/history/goto`. Driven end to end: a real edit through the
//! source editor lands in the log, then `H` + `enter` on the step before it
//! must put the file back.

use std::time::Duration;

use crate::fixtures;
use crate::harness::TuiSession;

#[test]
#[ignore = "pty-based e2e — run via `cargo test --test tui_e2e -- --ignored`"]
fn history_view_jumps_the_canvas_back_to_an_earlier_step() {
    let (canvas_path, dir) = fixtures::write_fixture(fixtures::SIMPLE_RUNNABLE);
    let mut session = TuiSession::spawn(&canvas_path, dir, 30, 100);
    session.wait_for("Root", Duration::from_secs(5)).expect("the tree should render");

    // Edit: open the editor, append a marker line at the end of the file,
    // save, leave. (edtui's vim keys: `G` last line, `o` open line below.)
    session.send_keys("e");
    std::thread::sleep(Duration::from_millis(300));
    session.send_keys("Go");
    session.send_keys("HISTORYMARKER");
    session.send_keys("\x1b");
    session.send_keys("\x13"); // Ctrl-s
    std::thread::sleep(Duration::from_millis(500));
    session.send_keys("\x1b"); // leave the editor
    assert!(
        std::fs::read_to_string(&canvas_path).unwrap().contains("HISTORYMARKER"),
        "the edit should have been saved to disk"
    );

    session.send_keys("H");
    session
        .wait_for("jump here", Duration::from_secs(5))
        .unwrap_or_else(|_| panic!("H should open the history view; screen:\n{}", session.screen_text()));

    // Newest step first and preselected (the current state) — one `j` moves
    // to the step before it, i.e. the state without the marker.
    session.send_keys("j");
    session.send_keys("\r");

    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        if !std::fs::read_to_string(&canvas_path).unwrap().contains("HISTORYMARKER") {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the jump should have reverted the marker; screen:\n{}",
            session.screen_text()
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}
