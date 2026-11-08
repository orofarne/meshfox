//! A `form` fence as a step of a run chain: the run parks on it, the TUI
//! opens the form for answering, and Send continues the run.

use std::time::Duration;

use crate::fixtures;
use crate::harness::TuiSession;

const FLOW: &str = concat!(
    "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
    "<!-- meshfox:var name=\"TUI_ANSWER\" -->\n\n",
    "```button name=\"go\" deps=\"finish\"\nStartFlow\n```\n\n",
    "```bash name=\"prep\"\necho prep-done-marker\n```\n\n",
    "```form name=\"review\" deps=\"prep\" send=\"Continue\"\n",
    "field var=\"TUI_ANSWER\" label=\"Answer\"\n```\n\n",
    "```bash name=\"finish\" deps=\"review\" env=\"$TUI_ANSWER\"\n",
    "echo \"result-is-$TUI_ANSWER\"\n```\n",
);

#[test]
#[ignore = "pty-based e2e — run via `cargo test --test tui_e2e -- --ignored`"]
fn a_run_parks_on_a_form_step_and_send_continues_it() {
    let (canvas_path, dir) = fixtures::write_fixture(FLOW);
    let mut session = TuiSession::spawn(&canvas_path, dir, 40, 110);
    let timeout = Duration::from_secs(10);
    session
        .wait_for("StartFlow", Duration::from_secs(5))
        .unwrap();

    let (row, col) = session.find("StartFlow").expect("the button");
    session.send_mouse_click(row, col);

    // The chain runs `prep`, then parks on the form and says so.
    session.wait_for("prep-done-marker", timeout).unwrap();
    session.wait_for("waiting for form", timeout).unwrap();
    assert!(
        !session.screen_text().contains("result-is-yes"),
        "finish must not run before the form is answered"
    );

    // The form is open for editing: type the answer, move to Send, send.
    session.send_keys("yes");
    session.wait_for("Answer: yes", timeout).unwrap();
    session.send_keys("\r\r");
    session
        .wait_for("result-is-yes", timeout)
        .expect("Send on the awaited form should continue the run");
}

const LIST_FLOW: &str = concat!(
    "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
    "<!-- meshfox:var name=\"TUI_REGIONS\" type=\"list\" choices=\"us,eu,ap\" -->\n\n",
    "```button name=\"go\" deps=\"finish\"\nStartList\n```\n\n",
    "```form name=\"pick\" send=\"Continue\"\n",
    "field var=\"TUI_REGIONS\" label=\"Regions\"\n```\n\n",
    "```bash name=\"finish\" deps=\"pick\" env=\"TUI_REGIONS|comma\"\n",
    "echo \"regions-are-$TUI_REGIONS\"\n```\n",
);

#[test]
#[ignore = "pty-based e2e — run via `cargo test --test tui_e2e -- --ignored`"]
fn a_list_field_with_choices_is_ticked_in_a_checklist_popup() {
    let (canvas_path, dir) = fixtures::write_fixture(LIST_FLOW);
    let mut session = TuiSession::spawn(&canvas_path, dir, 40, 110);
    let timeout = Duration::from_secs(10);
    session
        .wait_for("StartList", Duration::from_secs(5))
        .unwrap();
    let (row, col) = session.find("StartList").expect("the button");
    session.send_mouse_click(row, col);
    session.wait_for("waiting for form", timeout).unwrap();

    // Enter opens the checklist; tick `us` and `ap`, confirm with Enter.
    session.send_keys("\r");
    session.wait_for("Space tick", timeout).unwrap();
    session.wait_for("[ ] eu", timeout).unwrap();
    session.send_keys(" ");
    session.wait_for("[x] us", timeout).unwrap();
    session.send_keys("\x1b[B\x1b[B ");
    session.wait_for("[x] ap", timeout).unwrap();
    session.send_keys("\r");
    session.wait_for("Regions: us, ap", timeout).unwrap();

    // Down to Send, Enter sends; the consumer got the list as `us,ap`.
    session.send_keys("\x1b[B\r");
    session
        .wait_for("regions-are-us,ap", timeout)
        .expect("the ticked items should reach the consumer in its format");
}
