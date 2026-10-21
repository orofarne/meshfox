//! The `Ctrl-p` command palette: structural node edits (new, rename,
//! delete) from the tree, applied through the worker and picked up again
//! by the TUI's own reload.

use std::path::Path;
use std::time::Duration;

use crate::fixtures;
use crate::harness::TuiSession;

const CTRL_P: &str = "\x10";
const BACKSPACE: &str = "\x7f";

fn spawn() -> (TuiSession, std::path::PathBuf) {
    let (canvas_path, dir) = fixtures::write_fixture(fixtures::SIMPLE_RUNNABLE);
    let session = TuiSession::spawn(&canvas_path, dir, 30, 100);
    session
        .wait_for("Leaf", Duration::from_secs(5))
        .expect("the tree should list the leaf");
    (session, canvas_path)
}

fn open_palette(session: &mut TuiSession, command: &str) {
    session.send_keys(CTRL_P);
    session
        .wait_for("Commands", Duration::from_secs(5))
        .expect("Ctrl-p should open the palette");
    session.send_keys(command);
    session.send_keys("\r");
}

fn wait_for_file(path: &Path, what: &str, pred: impl Fn(&str) -> bool) {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        if pred(&std::fs::read_to_string(path).unwrap_or_default()) {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!(
        "timed out waiting for {what}; file:\n{}",
        std::fs::read_to_string(path).unwrap_or_default()
    );
}

#[test]
#[ignore = "pty-based e2e — run via `cargo test --test tui_e2e -- --ignored`"]
fn rename_changes_the_selected_nodes_title() {
    let (mut session, canvas) = spawn();
    session.send_keys("j");
    open_palette(&mut session, "rename");
    session
        .wait_for("New title: Leaf", Duration::from_secs(5))
        .expect("the title field should be prefilled");
    for _ in 0..4 {
        session.send_keys(BACKSPACE);
    }
    session.send_keys("Twig");
    session.send_keys("\r");
    wait_for_file(&canvas, "the renamed heading", |s| s.contains("## Twig"));
    session
        .wait_for("Twig", Duration::from_secs(5))
        .expect("the tree should show the new title");
}

#[test]
#[ignore = "pty-based e2e — run via `cargo test --test tui_e2e -- --ignored`"]
fn new_child_adds_a_node_under_the_selection() {
    let (mut session, canvas) = spawn();
    open_palette(&mut session, "new child");
    session.send_keys("Fresh");
    session.send_keys("\r");
    wait_for_file(&canvas, "the new node", |s| s.contains("Fresh"));
    session
        .wait_for("Fresh", Duration::from_secs(5))
        .expect("the tree should show the new node");
}

#[test]
#[ignore = "pty-based e2e — run via `cargo test --test tui_e2e -- --ignored`"]
fn delete_asks_for_confirmation_first() {
    let (mut session, canvas) = spawn();
    session.send_keys("j");
    open_palette(&mut session, "delete node and");
    session
        .wait_for("y confirm", Duration::from_secs(5))
        .expect("delete should ask first");
    session.send_keys("n");
    assert!(std::fs::read_to_string(&canvas)
        .unwrap()
        .contains("## Leaf"));
    session.send_keys("\r");
    session.send_keys("y");
    wait_for_file(&canvas, "the node to be gone", |s| !s.contains("## Leaf"));
}

const TWO_LEAVES: &str = concat!(
    "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
    "## Alpha\n<!-- meshfox:node id=\"alpha\" parent=\"root\" -->\n\n",
    "## Beta\n<!-- meshfox:node id=\"beta\" parent=\"root\" -->\n\n",
);

fn spawn_two() -> (TuiSession, std::path::PathBuf) {
    let (canvas_path, dir) = fixtures::write_fixture(TWO_LEAVES);
    let session = TuiSession::spawn(&canvas_path, dir, 30, 100);
    session
        .wait_for("Beta", Duration::from_secs(5))
        .expect("the tree should list both leaves");
    (session, canvas_path)
}

#[test]
#[ignore = "pty-based e2e — run via `cargo test --test tui_e2e -- --ignored`"]
fn move_down_swaps_the_node_with_its_next_sibling() {
    let (mut session, canvas) = spawn_two();
    session.send_keys("j");
    open_palette(&mut session, "move node down");
    wait_for_file(
        &canvas,
        "Alpha after Beta",
        |s| matches!((s.find("## Alpha"), s.find("## Beta")), (Some(a), Some(b)) if b < a),
    );
}

#[test]
#[ignore = "pty-based e2e — run via `cargo test --test tui_e2e -- --ignored`"]
fn change_id_rewrites_the_nodes_id() {
    let (mut session, canvas) = spawn_two();
    session.send_keys("j");
    open_palette(&mut session, "change node id");
    session
        .wait_for("New id: alpha", Duration::from_secs(5))
        .expect("the id field should be prefilled");
    session.send_keys("2");
    session.send_keys("\r");
    wait_for_file(&canvas, "the new id", |s| s.contains("id=\"alpha2\""));
}

#[test]
#[ignore = "pty-based e2e — run via `cargo test --test tui_e2e -- --ignored`"]
fn move_to_reparents_under_the_chosen_node() {
    let (mut session, canvas) = spawn_two();
    session.send_keys("j");
    open_palette(&mut session, "move node to");
    session
        .wait_for("Move under", Duration::from_secs(5))
        .expect("the parent picker should open");
    session.send_keys("beta");
    session.send_keys("\r");
    wait_for_file(&canvas, "Alpha under Beta", |s| s.contains("### Alpha"));
}

#[test]
#[ignore = "pty-based e2e — run via `cargo test --test tui_e2e -- --ignored`"]
fn node_commands_from_the_editor_keep_it_open_on_the_new_text() {
    let (mut session, canvas) = spawn();
    session.send_keys("j");
    session.send_keys("e");
    std::thread::sleep(Duration::from_millis(300));
    open_palette(&mut session, "rename");
    session
        .wait_for("New title: Leaf", Duration::from_secs(5))
        .expect("the title field should be prefilled");
    for _ in 0..4 {
        session.send_keys(BACKSPACE);
    }
    session.send_keys("Twig");
    session.send_keys("\r");
    wait_for_file(&canvas, "the renamed heading", |s| s.contains("## Twig"));
    // Still in the editor (the palette is gone, the buffer shows the file's
    // new text), and a second command works against the fresh state.
    session
        .wait_for("## Twig", Duration::from_secs(5))
        .expect("the editor buffer should show the renamed heading");
    open_palette(&mut session, "new child");
    session.send_keys("Fresh");
    session.send_keys("\r");
    wait_for_file(&canvas, "the new node", |s| s.contains("Fresh"));
    session
        .wait_for("Fresh", Duration::from_secs(5))
        .expect("the editor buffer should show the new node");
}

#[test]
#[ignore = "pty-based e2e — run via `cargo test --test tui_e2e -- --ignored`"]
fn statistics_view_opens_from_the_palette_and_closes() {
    let (mut session, _canvas) = spawn();
    open_palette(&mut session, "canvas statistics");
    session
        .wait_for("Retained history", Duration::from_secs(5))
        .expect("the report should be shown");
    session.send_keys("q");
    std::thread::sleep(Duration::from_millis(300));
    assert!(!session.screen_text().contains("Retained history"));
}

#[test]
#[ignore = "pty-based e2e — run via `cargo test --test tui_e2e -- --ignored`"]
fn palette_actions_run_the_same_code_as_their_keys() {
    let (mut session, _canvas) = spawn();
    open_palette(&mut session, "help and keys");
    session
        .wait_for("keybindings", Duration::from_secs(5))
        .expect("the help overlay should open");
}

#[test]
#[ignore = "pty-based e2e — run via `cargo test --test tui_e2e -- --ignored`"]
fn block_statistics_follow_run_history_block_resolution() {
    let (mut session, _canvas) = spawn();
    // Tree-focused on a node with one block: that block, no picker.
    session.send_keys("j");
    open_palette(&mut session, "block statistics");
    session
        .wait_for("leaf::leaf", Duration::from_secs(5))
        .expect("the report should be for the node's only block");
    session.send_keys("q");
    // Document focus with a selected block: that block.
    session.send_keys("\t");
    session.send_keys("]");
    open_palette(&mut session, "block statistics");
    session
        .wait_for("leaf::leaf", Duration::from_secs(5))
        .expect("the report should be for the selected block");
}
