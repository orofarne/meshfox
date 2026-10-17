//! The source editor's save when the file changed on disk while it was open
//! (`App::save_source_editor`, `source_diff.rs`): edits that don't overlap
//! are merged by the worker and saved; overlapping ones open a side-by-side
//! comparison with `t` take theirs / `m` keep mine / `esc` back. Driven end
//! to end — the "other client" is a plain write to the canvas file, which
//! the worker adopts as an external edit.

use std::path::Path;
use std::time::Duration;

use crate::fixtures;
use crate::harness::TuiSession;

/// Enough lines that the top and the bottom are far apart.
const DOC: &str = concat!(
    "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
    "alpha line\nbravo line\ncharlie line\ndelta line\necho line\nfoxtrot line\n",
    "golf line\nhotel line\n",
);

fn open_editor_and_append(session: &mut TuiSession, marker: &str) {
    session
        .wait_for("Root", Duration::from_secs(5))
        .expect("the tree should render");
    session.send_keys("e");
    std::thread::sleep(Duration::from_millis(300));
    // edtui's vim keys: `G` last line, `o` open a line below.
    session.send_keys("Go");
    session.send_keys(marker);
    session.send_keys("\x1b");
}

fn on_disk(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap()
}

#[test]
#[ignore = "pty-based e2e — run via `cargo test --test tui_e2e -- --ignored`"]
fn edits_that_dont_overlap_are_merged_and_saved() {
    let (canvas_path, dir) = fixtures::write_fixture(DOC);
    let mut session = TuiSession::spawn(&canvas_path, dir, 30, 100);
    open_editor_and_append(&mut session, "MINE-LINE");

    // Elsewhere: the heading at the top, far from where the buffer typed.
    std::fs::write(&canvas_path, DOC.replace("# Root", "# Root Renamed")).unwrap();

    session.send_keys("\x13"); // Ctrl-s
    session
        .wait_for(
            "merged with changes made elsewhere",
            Duration::from_secs(10),
        )
        .unwrap_or_else(|e| panic!("{e}"));

    let disk = on_disk(&canvas_path);
    assert!(disk.contains("# Root Renamed"), "disk:\n{disk}");
    assert!(disk.contains("MINE-LINE"), "disk:\n{disk}");
}

#[test]
#[ignore = "pty-based e2e — run via `cargo test --test tui_e2e -- --ignored`"]
fn overlapping_edits_open_a_comparison_and_take_theirs_discards_the_buffer() {
    let (canvas_path, dir) = fixtures::write_fixture(DOC);
    let mut session = TuiSession::spawn(&canvas_path, dir, 30, 110);
    open_editor_and_append(&mut session, "MINE-LINE");

    // Elsewhere: a different line added at the very same place.
    std::fs::write(&canvas_path, format!("{DOC}THEIRS-LINE\n")).unwrap();

    session.send_keys("\x13"); // Ctrl-s
    session
        .wait_for("overlap changes made elsewhere", Duration::from_secs(10))
        .unwrap_or_else(|e| panic!("{e}"));
    let screen = session.screen_text();
    assert!(screen.contains("THEIRS-LINE"), "screen:\n{screen}");
    assert!(screen.contains("MINE-LINE"), "screen:\n{screen}");
    assert!(screen.contains("theirs"), "screen:\n{screen}");
    assert!(screen.contains("take theirs"), "screen:\n{screen}");
    // Nothing was written.
    let disk = on_disk(&canvas_path);
    assert!(disk.contains("THEIRS-LINE") && !disk.contains("MINE-LINE"));

    session.send_keys("t");
    session
        .wait_for("Ctrl-s save", Duration::from_secs(5))
        .unwrap_or_else(|e| panic!("{e}"));
    let screen = session.screen_text();
    assert!(screen.contains("THEIRS-LINE"), "screen:\n{screen}");
    assert!(!screen.contains("MINE-LINE"), "screen:\n{screen}");
    assert!(!screen.contains("[modified]"), "screen:\n{screen}");
    assert!(!on_disk(&canvas_path).contains("MINE-LINE"));
}

#[test]
#[ignore = "pty-based e2e — run via `cargo test --test tui_e2e -- --ignored`"]
fn keep_mine_writes_the_buffer_over_the_file() {
    let (canvas_path, dir) = fixtures::write_fixture(DOC);
    let mut session = TuiSession::spawn(&canvas_path, dir, 30, 110);
    open_editor_and_append(&mut session, "MINE-LINE");
    std::fs::write(&canvas_path, format!("{DOC}THEIRS-LINE\n")).unwrap();

    session.send_keys("\x13");
    session
        .wait_for("overlap changes made elsewhere", Duration::from_secs(10))
        .unwrap_or_else(|e| panic!("{e}"));
    session.send_keys("m");

    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        let disk = on_disk(&canvas_path);
        if disk.contains("MINE-LINE") {
            assert!(!disk.contains("THEIRS-LINE"), "disk:\n{disk}");
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "keep mine should have written the buffer; screen:\n{}",
            session.screen_text()
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[test]
#[ignore = "pty-based e2e — run via `cargo test --test tui_e2e -- --ignored`"]
fn esc_goes_back_to_the_buffer_without_deciding() {
    let (canvas_path, dir) = fixtures::write_fixture(DOC);
    let mut session = TuiSession::spawn(&canvas_path, dir, 30, 110);
    open_editor_and_append(&mut session, "MINE-LINE");
    std::fs::write(&canvas_path, format!("{DOC}THEIRS-LINE\n")).unwrap();

    session.send_keys("\x13");
    session
        .wait_for("overlap changes made elsewhere", Duration::from_secs(10))
        .unwrap_or_else(|e| panic!("{e}"));
    session.send_keys("\x1b");
    session
        .wait_for("not saved", Duration::from_secs(5))
        .unwrap_or_else(|e| panic!("{e}"));
    let screen = session.screen_text();
    assert!(screen.contains("MINE-LINE"), "screen:\n{screen}");
    let disk = on_disk(&canvas_path);
    assert!(disk.contains("THEIRS-LINE") && !disk.contains("MINE-LINE"));
}
