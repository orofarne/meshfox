use std::time::Duration;

use crate::{fixtures, harness::TuiSession};

#[test]
#[ignore = "pty-based e2e — run via `cargo test --test tui_e2e search:: -- --ignored`"]
fn canvas_search_navigates_hidden_nodes_and_keeps_the_last_result_on_close() {
    let source = "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n\n## Alpha\n<!-- meshfox:node id=\"alpha\" -->\n\n### First\n<!-- meshfox:node id=\"first\" -->\n\nzzzneedle zzzneedle\n\n## Beta\n<!-- meshfox:node id=\"beta\" -->\n\n### Second\n<!-- meshfox:node id=\"second\" -->\n\nzzzneedle final-result\n";
    let (path, dir) = fixtures::write_fixture(source);
    let mut session = TuiSession::spawn(&path, dir, 25, 120);
    session.wait_for("Root", Duration::from_secs(5)).unwrap();
    session.send_keys("/zzzneedle");
    session.wait_for("1/3", Duration::from_secs(3)).unwrap();
    session
        .wait_for("zzzneedle zzzneedle", Duration::from_secs(3))
        .unwrap();
    session.send_keys("\r");
    session.wait_for("2/3", Duration::from_secs(3)).unwrap();
    session
        .wait_for("zzzneedle zzzneedle", Duration::from_secs(3))
        .unwrap();
    session.send_keys("\r");
    session.wait_for("3/3", Duration::from_secs(3)).unwrap();
    session
        .wait_for("final-result", Duration::from_secs(3))
        .unwrap();
    session.send_keys("\x1b");
    session
        .wait_for("tab focus", Duration::from_secs(3))
        .unwrap();
    assert!(session.screen_text().contains("final-result"));
    // q must get back to the ordinary keymap after Escape.
    session.send_keys("q");
    assert!(session.wait_for_exit(Duration::from_secs(5)));
}
