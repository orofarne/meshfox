//! `output="markdown"` — a block's own live output, shown inline right
//! under it in the Document pane (`markdown::push_live_output`), is plain
//! text while the run is going and real rendered Markdown once it's done.

use std::time::Duration;

use crate::fixtures;
use crate::harness::TuiSession;

// The marker and the table's separator row are assembled by `printf` at
// run time, so neither ever appears verbatim in the fence's own source
// (also shown in the Document pane) — only in real output.
const MARKDOWN_TABLE: &str = concat!(
    "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
    "```bash name=\"md\" output=\"markdown\"\n",
    "printf '| score | name |\\n|%s|%s|\\n| 1.0 | ZMARK%sZ |\\n' ---- ---- ER\n",
    "```\n",
);

#[test]
#[ignore = "pty-based e2e — run via `cargo test --test tui_e2e -- --ignored`"]
fn a_finished_markdown_block_renders_its_output_inline_under_the_block() {
    let (canvas_path, dir) = fixtures::write_fixture(MARKDOWN_TABLE);
    let mut session = TuiSession::spawn(&canvas_path, dir, 40, 100);
    session.wait_for("Root", Duration::from_secs(5)).expect("initial render");

    session.send_keys("r");
    session
        .wait_for("ZMARKERZ", Duration::from_secs(10))
        .expect("the block's stdout should show up inline under it");
    // The frame's header flips to "done" once the run finishes, which is
    // also when the raw text is swapped for the rendered table.
    session
        .wait_for("markdown · done", Duration::from_secs(10))
        .expect("the live frame should reach its done state");

    let screen = session.screen_text();
    assert!(
        !screen.contains("|----|----|"),
        "the raw pipe-table syntax should have been rendered, not left as literal text:\n{screen}"
    );
}
