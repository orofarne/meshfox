//! `e` with `[tui] editor` configured: the TUI hands the terminal to the
//! user's editor on a scratch copy of the source, then saves what comes
//! back through the ordinary save. The "editor" here is a shell script that
//! appends a line and records the line number it was asked to open at.

use std::path::Path;
use std::time::Duration;

use crate::fixtures;
use crate::harness::TuiSession;

fn configure(dir: &Path, script_body: &str) {
    std::fs::create_dir_all(dir.join(".meshfox")).unwrap();
    let script = dir.join("fake-editor.sh");
    std::fs::write(&script, script_body).unwrap();
    std::fs::write(
        dir.join(".meshfox/config.toml"),
        format!(
            "[tui]\neditor = \"sh '{}' {{file}} {{line}}\"\n",
            script.display()
        ),
    )
    .unwrap();
}

#[test]
#[ignore = "pty-based e2e — run via `cargo test --test tui_e2e -- --ignored`"]
fn e_edits_in_the_configured_editor_and_saves_straight_away() {
    let (canvas_path, dir) = fixtures::write_fixture(fixtures::SIMPLE_RUNNABLE);
    configure(
        &dir,
        &format!(
            "printf 'EXT-LINE\\n' >> \"$1\"\necho \"$2 $1\" > '{}'\n",
            dir.join("args.log").display()
        ),
    );
    let mut session = TuiSession::spawn(&canvas_path, dir.clone(), 30, 100);
    session
        .wait_for("Root", Duration::from_secs(5))
        .expect("the tree should render");
    session.send_keys("e");
    session
        .wait_for("saved", Duration::from_secs(10))
        .unwrap_or_else(|e| panic!("{e}"));

    let disk = std::fs::read_to_string(&canvas_path).unwrap();
    assert!(disk.ends_with("EXT-LINE\n"), "disk:\n{disk}");
    let args = std::fs::read_to_string(dir.join("args.log")).unwrap();
    let (line, file) = args.trim().split_once(' ').unwrap();
    assert!(line.parse::<usize>().unwrap() > 1, "line: {line}");
    assert!(file.ends_with(".canvas.md"), "file: {file}");
    assert!(!Path::new(file).exists(), "scratch file should be removed");
    // Back in the TUI, still alive.
    assert!(session.screen_text().contains("Root"));
}

#[test]
#[ignore = "pty-based e2e — run via `cargo test --test tui_e2e -- --ignored`"]
fn a_failing_editor_discards_the_edit_and_returns_to_the_tree() {
    let (canvas_path, dir) = fixtures::write_fixture(fixtures::SIMPLE_RUNNABLE);
    configure(&dir, "printf 'EXT-LINE\\n' >> \"$1\"\nexit 1\n");
    let mut session = TuiSession::spawn(&canvas_path, dir, 30, 100);
    session
        .wait_for("Root", Duration::from_secs(5))
        .expect("the tree should render");
    session.send_keys("e");
    session
        .wait_for("nothing saved", Duration::from_secs(10))
        .unwrap_or_else(|e| panic!("{e}"));

    let disk = std::fs::read_to_string(&canvas_path).unwrap();
    assert!(!disk.contains("EXT-LINE"), "disk:\n{disk}");
    assert!(session.in_alternate_screen());
}
