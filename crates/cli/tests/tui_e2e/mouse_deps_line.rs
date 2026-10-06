//! TODO.canvas.md: "Мышь в панелях TUI" — clicking a block name inside its
//! own deps line (`├─ deps: …`/`via var: …`, see `markdown.rs`'s `dep_lines`)
//! should jump to it — the TUI counterpart to the web UI's `jumpTo`
//! (`web/src/MeshNode.tsx`). "Jump" here means: move the tree's own
//! selection to the block's owning node (so the document pane switches to
//! showing it) — there's no canvas to pan/scroll to in the TUI the way
//! there is on the web. Check the document pane's selected-node title and
//! producer contract, rather than span-dependent reverse-video styling.

use std::time::Duration;

use crate::fixtures;
use crate::harness::TuiSession;

#[test]
#[ignore = "pty-based e2e — run via `cargo test --test tui_e2e -- --ignored`"]
fn clicking_the_block_name_in_a_deps_line_selects_its_owning_node() {
    let (canvas_path, dir) = fixtures::write_fixture(fixtures::DEPS_LINE_JUMP);
    let mut session = TuiSession::spawn(&canvas_path, dir, 30, 100);
    session
        .wait_for("Root", Duration::from_secs(5))
        .expect("initial render");

    // Select "Consumer" (second child of Root) so its own deps line
    // (`via var: RESOURCE → producer/make`) is actually on screen.
    session.send_keys("jj");
    session
        .wait_for("producer/make", Duration::from_secs(5))
        .expect("Consumer's implicit deps line should show its source block");
    session
        .wait_for("│Consumer", Duration::from_secs(5))
        .expect("Consumer's document should be selected before the click");

    let (row, col) = session
        .find("producer/make")
        .expect("deps-line block-name text");
    session.send_mouse_click(row, col);
    session
        .wait_for("│Producer", Duration::from_secs(5))
        .expect("clicking the dependency should select Producer's document");
    session
        .wait_for("exports: RESOURCE", Duration::from_secs(5))
        .expect("the selected producer's contract should be rendered");
}
