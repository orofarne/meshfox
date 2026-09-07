//! TODO.canvas.md: "Мышь в панелях TUI" — clicking a block name inside its
//! own deps line (`├─ after: …`/`via …`, see `markdown.rs`'s `dep_line`)
//! should jump to it — the TUI counterpart to the web UI's `jumpTo`
//! (`web/src/MeshNode.tsx`). "Jump" here means: move the tree's own
//! selection to the block's owning node (so the document pane switches to
//! showing it) — there's no canvas to pan/scroll to in the TUI the way
//! there is on the web. Checked via the tree's own selected-row highlight
//! (`Modifier::REVERSED`, same convention every `List` selection already
//! uses), not the document pane's content — a block's own *source code*
//! (unlike its run output) is visible in the document pane regardless of
//! whether its owning node is actually selected.

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
    // (`via RESOURCE: producer/make`) is actually on screen.
    session.send_keys("jj");
    session
        .wait_for("producer/make", Duration::from_secs(5))
        .expect("Consumer's implicit deps line should show its source block");
    // "Consumer"/"Producer" alone would also match the document pane's own
    // border title (the currently-selected node's title, rendered above
    // the tree pane's row-major position on screen) — the `[run,cache]`
    // badge suffix only the tree's own row carries disambiguates it.
    let (consumer_row, consumer_col) = session.find("Consumer [run").expect("Consumer's own tree row");
    assert!(
        session.inverse_at(consumer_row, consumer_col),
        "Consumer's tree row should be the selected (reverse-video) one right now"
    );

    let (row, col) = session.find("producer/make").expect("deps-line block-name text");
    session.send_mouse_click(row, col);
    std::thread::sleep(Duration::from_millis(100));

    let (producer_row, producer_col) = session.find("Producer [run").expect("Producer's own tree row");
    assert!(
        session.inverse_at(producer_row, producer_col),
        "clicking the deps-line's block name should move the tree's own selection to Producer"
    );
}
