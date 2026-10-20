//! A runnable `file` node (`interpreter=` + target) is a block like a
//! fence's: it is drawn with the same header controls, run by the worker
//! through the ordinary run path, and its output appears right under it.

use std::time::Duration;

use crate::fixtures;
use crate::harness::TuiSession;

const CANVAS: &str = concat!(
    "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
    "## Prepare\n<!-- meshfox:node id=\"prepare\" parent=\"root\" -->\n\n",
    "```sh name=\"prep\"\nprintf 'prepared-%s\\n' dep\n```\n\n",
    "## Script\n<!-- meshfox:node id=\"script\" parent=\"root\" type=\"file\" interpreter=\"sh\" deps=\"prepare/prep\" -->\n\n",
    "[script](./hello.sh)\n\nRuns the hello script.\n",
);

#[test]
#[ignore = "pty-based e2e — run via `cargo test --test tui_e2e -- --ignored`"]
fn a_file_node_is_drawn_and_run_like_a_fence() {
    let (canvas_path, dir) = fixtures::write_fixture(CANVAS);
    std::fs::write(dir.join("hello.sh"), "printf 'file-node-%s\\n' marker\n").unwrap();

    let mut session = TuiSession::spawn(&canvas_path, dir, 40, 110);
    session
        .wait_for("Root", Duration::from_secs(5))
        .expect("initial render");
    // Rows: root, prepare, script.
    session.send_keys("jj");
    // The same block header a fence gets: fold, history and run controls,
    // the node's dependency, and the file's caption above them.
    session
        .wait_for("[history]", Duration::from_secs(5))
        .unwrap_or_else(|e| panic!("no block header: {e}\n{}", session.screen_text()));
    let screen = session.screen_text();
    assert!(screen.contains("[run]"), "{screen}");
    assert!(screen.contains("[chain]"), "{screen}");
    assert!(screen.contains("prepare/prep"), "{screen}");
    assert!(screen.contains("Runs the hello script."), "{screen}");

    // `R` runs only the file; its output lands under the block.
    session.send_keys("R");
    session
        .wait_for("file-node-marker", Duration::from_secs(10))
        .unwrap_or_else(|e| {
            panic!(
                "file node output never appeared: {e}\n{}",
                session.screen_text()
            )
        });
    assert!(!session.screen_text().contains("prepared-dep"));
    // The output shows before the run is over; a key pressed while it still
    // is would only be told a run is already in progress.
    session
        .wait_for("run finished", Duration::from_secs(10))
        .unwrap_or_else(|e| panic!("the run never finished: {e}\n{}", session.screen_text()));

    // `r` runs the chain, dependency first.
    session.send_keys("r");
    session
        .wait_for("prepared-dep", Duration::from_secs(10))
        .unwrap_or_else(|e| panic!("dependency never ran: {e}\n{}", session.screen_text()));
}
