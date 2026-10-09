//! A runnable `file` node (`interpreter=` + target) is executed by the
//! worker, not by the TUI process itself: its output still reaches the
//! Output pane.

use std::time::Duration;

use crate::fixtures;
use crate::harness::TuiSession;

const CANVAS: &str = concat!(
    "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
    "## Script\n<!-- meshfox:node id=\"script\" parent=\"root\" type=\"file\" interpreter=\"sh\" -->\n\n",
    "[script](./hello.sh)\n",
);

#[test]
#[ignore = "pty-based e2e — run via `cargo test --test tui_e2e -- --ignored`"]
fn running_a_file_node_streams_its_output_from_the_worker() {
    let (canvas_path, dir) = fixtures::write_fixture(CANVAS);
    std::fs::write(dir.join("hello.sh"), "printf 'file-node-%s\\n' marker\n").unwrap();

    let mut session = TuiSession::spawn(&canvas_path, dir, 40, 100);
    session
        .wait_for("Root", Duration::from_secs(5))
        .expect("initial render");
    session.send_keys("j");
    session.send_keys("r");
    session
        .wait_for("Output (done)", Duration::from_secs(10))
        .expect("the run should finish");
    // The console stays collapsed for a single run; open it to read it.
    let (row, col) = session.find("Output").expect("collapsed Output strip");
    session.send_mouse_click(row, col);
    session
        .wait_for("file-node-marker", Duration::from_secs(10))
        .unwrap_or_else(|e| {
            panic!(
                "file node output never appeared: {e}\n{}",
                session.screen_text()
            )
        });
}
