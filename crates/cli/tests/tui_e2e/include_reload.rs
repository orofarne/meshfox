//! An `include` node's target file edited by an external editor while the
//! TUI is open must show up in the Document pane without touching the
//! including canvas itself (the web UI already behaves this way).

use std::time::Duration;

use crate::fixtures;
use crate::harness::TuiSession;

const CANVAS: &str = concat!(
    "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
    "## Spec\n<!-- meshfox:node id=\"spec\" parent=\"root\" type=\"include\" -->\n\n",
    "[spec](./spec.md)\n",
);

#[test]
#[ignore = "pty-based e2e — run via `cargo test --test tui_e2e -- --ignored`"]
fn an_external_edit_of_an_included_markdown_file_shows_up_without_restart() {
    let (canvas_path, dir) = fixtures::write_fixture(CANVAS);
    let spec = dir.join("spec.md");
    std::fs::write(&spec, "Alpha-before paragraph.\n").unwrap();

    let mut session = TuiSession::spawn(&canvas_path, dir, 40, 100);
    session
        .wait_for("Root", Duration::from_secs(5))
        .expect("initial render");
    session.send_keys("j");
    session
        .wait_for("Alpha-before", Duration::from_secs(5))
        .expect("the included file's content should be shown");

    // Make sure the new mtime differs even on coarse-grained filesystems.
    std::thread::sleep(Duration::from_millis(1100));
    std::fs::write(&spec, "Beta-after paragraph.\n").unwrap();

    session
        .wait_for("Beta-after", Duration::from_secs(10))
        .unwrap_or_else(|e| {
            panic!(
                "external edit of the included file never appeared: {e}\n{}",
                session.screen_text()
            )
        });
}
