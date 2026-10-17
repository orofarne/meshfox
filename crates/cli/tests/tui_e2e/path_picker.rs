//! SPEC.md's "Path pickers" in the TUI: `Ctrl-O` on a `type="file"`/
//! `type="dir"` field of a `form` fence opens a modal over the worker's
//! `GET /api/fs/list`; `Enter`/a click descends or chooses, and the chosen
//! absolute path lands in the field's live buffer.

use std::time::Duration;

use crate::fixtures;
use crate::harness::TuiSession;

const CTRL_O: &str = "\x0f";

/// Polls the screen until `pred` holds — `wait_for` can only wait for text
/// to *appear*, and the form behind the modal stays visible throughout.
fn wait_until(session: &TuiSession, what: &str, pred: impl Fn(&str) -> bool) {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        if pred(&session.screen_text()) {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!(
        "timed out waiting for {what}; screen:\n{}",
        session.screen_text()
    );
}

fn spawn_with_footage() -> TuiSession {
    let (canvas_path, dir) = fixtures::write_fixture(fixtures::FORM_PATHS);
    let footage = dir.join("footage");
    std::fs::create_dir_all(&footage).unwrap();
    std::fs::write(footage.join("clip.mp4"), "x").unwrap();
    std::fs::write(footage.join("interview.mkv"), "x").unwrap();
    std::fs::write(footage.join("notes.txt"), "x").unwrap();
    let mut session = TuiSession::spawn(&canvas_path, dir, 36, 110);
    session
        .wait_for("Input video", Duration::from_secs(5))
        .expect("the form should render inline");
    let (row, col) = session.find("Input video").expect("the field's own label");
    session.send_mouse_click(row, col);
    session
}

#[test]
#[ignore = "pty-based e2e — run via `cargo test --test tui_e2e -- --ignored`"]
fn ctrl_o_browses_to_a_file_with_the_keyboard_and_hints_by_extension() {
    let mut session = spawn_with_footage();
    session.send_keys(CTRL_O);
    session
        .wait_for("choose file", Duration::from_secs(5))
        .expect("Ctrl-O on a file field should open the picker");
    assert!(
        session.screen_text().contains("footage/"),
        "the canvas dir's subdirectory is listed"
    );

    // `..` is row 0; `j` moves to `footage/`, Enter descends.
    session.send_keys("j");
    session.send_keys("\r");
    session
        .wait_for("clip.mp4", Duration::from_secs(5))
        .expect("the descended directory should list its files");
    assert!(session.screen_text().contains("interview.mkv"));
    assert!(
        !session.screen_text().contains("notes.txt"),
        "ext=\"mp4,mkv\" narrows the files shown"
    );

    // `e` switches the hint off.
    session.send_keys("e");
    session
        .wait_for("notes.txt", Duration::from_secs(5))
        .expect("the ext hint is switchable");
    session.send_keys("e");

    // `..`, then clip.mp4.
    session.send_keys("j");
    session.send_keys("\r");
    session
        .wait_for("footage/clip.mp4", Duration::from_secs(5))
        .expect("the chosen absolute path should land in the field");
    assert!(
        !session.screen_text().contains("choose file"),
        "the picker closes after choosing"
    );
}

#[test]
#[ignore = "pty-based e2e — run via `cargo test --test tui_e2e -- --ignored`"]
fn a_second_click_on_a_picker_row_activates_it_and_escape_cancels() {
    let mut session = spawn_with_footage();
    session.send_keys(CTRL_O);
    session
        .wait_for("footage/", Duration::from_secs(5))
        .expect("the picker should list the canvas directory");

    let (row, col) = session.find("footage/").expect("the footage row");
    session.send_mouse_click(row, col);
    session.send_mouse_click(row, col);
    session
        .wait_for("clip.mp4", Duration::from_secs(5))
        .expect("clicking the selected row again should descend into it");

    session.send_keys("\x1b");
    wait_until(&session, "Esc to close the picker", |s| {
        !s.contains("choose file")
    });
}

#[test]
#[ignore = "pty-based e2e — run via `cargo test --test tui_e2e -- --ignored`"]
fn a_dir_field_chooses_the_directory_it_is_browsing() {
    let mut session = spawn_with_footage();
    // Second field: its label, then Ctrl-O.
    let (row, col) = session.find("Work dir").expect("the dir field's label");
    session.send_mouse_click(row, col);
    session.send_keys(CTRL_O);
    session
        .wait_for("choose directory", Duration::from_secs(5))
        .expect("Ctrl-O on a dir field should open a directory picker");
    // Descend into footage/, then `s` chooses it.
    session.send_keys("j");
    session.send_keys("\r");
    session.send_keys("s");
    wait_until(&session, "the chosen directory in the field", |s| {
        !s.contains("choose directory") && s.contains("/footage")
    });
}

#[test]
#[ignore = "pty-based e2e — run via `cargo test --test tui_e2e -- --ignored`"]
fn clicking_the_browse_marker_opens_the_picker_without_a_key() {
    let (canvas_path, dir) = fixtures::write_fixture(fixtures::FORM_PATHS);
    let mut session = TuiSession::spawn(&canvas_path, dir, 36, 110);
    session
        .wait_for("[Browse… ^o]", Duration::from_secs(5))
        .expect("a dir/file field should carry a visible Browse marker");
    let (row, col) = session.find("[Browse… ^o]").expect("the marker");
    session.send_mouse_click(row, col + 1);
    session
        .wait_for("choose file", Duration::from_secs(5))
        .expect("clicking the marker should open the picker for that field");
}

#[test]
#[ignore = "pty-based e2e — run via `cargo test --test tui_e2e -- --ignored`"]
fn a_form_is_framed_and_brackets_move_the_selection_onto_it() {
    let (canvas_path, dir) = fixtures::write_fixture(fixtures::FORM_PATHS);
    let mut session = TuiSession::spawn(&canvas_path, dir, 36, 110);
    session
        .wait_for("form · pick", Duration::from_secs(5))
        .expect("a form should be drawn in a frame with its name in the header");
    assert!(session.screen_text().contains("┌─ form · pick"));

    // Tab to the Document pane, `]` selects the only block: the form.
    session.send_keys("\t");
    session.send_keys("]");
    session
        .wait_for("›─ form · pick", Duration::from_secs(5))
        .expect("the selected form's corner becomes ›");

    // Enter edits it, and says so.
    session.send_keys("\r");
    session
        .wait_for("· editing", Duration::from_secs(5))
        .expect("an edited form's header says so");

    // Esc stops editing but keeps the selection.
    session.send_keys("\x1b");
    wait_until(&session, "editing to end", |s| !s.contains("· editing"));
    assert!(session.screen_text().contains("›─ form · pick"));
}
