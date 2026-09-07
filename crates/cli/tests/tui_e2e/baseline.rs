//! Regression coverage for the one path in this repo with zero real-
//! terminal coverage before this suite existed: a key actually typed into
//! a real terminal, going through the real `crossterm::event::read()`
//! loop in `crates/cli/src/tui/mod.rs::run`, running a real process, and
//! the real output coming back through the real terminal. Every existing
//! TUI test (`crates/cli/src/tui/{app,ui}.rs`) calls `App::on_key`/renders
//! via `TestBackend` directly — none of them touch that real path.

use std::time::Duration;

use crate::fixtures;
use crate::harness::TuiSession;

#[test]
#[ignore = "pty-based e2e — run via `cargo test --test tui_e2e -- --ignored`"]
fn starts_enters_raw_mode_and_renders_the_tree() {
    let (canvas_path, dir) = fixtures::write_fixture(fixtures::SIMPLE_RUNNABLE);
    let session = TuiSession::spawn(&canvas_path, dir, 30, 100);
    session
        .wait_for("Root", Duration::from_secs(5))
        .expect("root node's title should render");
}

#[test]
#[ignore = "pty-based e2e — run via `cargo test --test tui_e2e -- --ignored`"]
fn pressing_r_on_a_selected_node_runs_its_block_and_streams_real_output() {
    let (canvas_path, dir) = fixtures::write_fixture(fixtures::SIMPLE_RUNNABLE);
    let mut session = TuiSession::spawn(&canvas_path, dir, 30, 100);
    session
        .wait_for("Root", Duration::from_secs(5))
        .expect("initial render");

    // Root is selected first; "Leaf" (the node with the runnable block) is
    // the next row down.
    session.send_keys("j");
    session
        .wait_for("Leaf", Duration::from_secs(2))
        .expect("tree should still render after moving selection");
    session.send_keys("r");

    session
        .wait_for("tui-e2e-marker-output", Duration::from_secs(10))
        .expect("the block's real stdout should show up in the Output pane");
}

#[test]
#[ignore = "pty-based e2e — run via `cargo test --test tui_e2e -- --ignored`"]
fn pressing_q_actually_exits_the_process() {
    let (canvas_path, dir) = fixtures::write_fixture(fixtures::SIMPLE_RUNNABLE);
    let mut session = TuiSession::spawn(&canvas_path, dir, 30, 100);
    session
        .wait_for("Root", Duration::from_secs(5))
        .expect("initial render");

    session.send_keys("q");
    assert!(
        session.wait_for_exit(Duration::from_secs(5)),
        "meshfox tui should have exited (and restored the real terminal) after q"
    );
}
