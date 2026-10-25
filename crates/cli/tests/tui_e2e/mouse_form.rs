//! SPEC.md's "Form fences"/`autorun` — a `render`-worthy fence with no
//! real code of its own (`markdown.rs`'s `FORM_LANG` branch), rendered
//! inline as labeled field rows plus a `[Send]` marker instead of raw
//! code. Clicking a field enters edit mode (`App::activate_click_target`'s
//! `ClickTarget::FormField`), typing edits its live buffer, and clicking
//! `[Send]` (`ClickTarget::FormSend`) commits every field into
//! `App::session_vars` and queues whichever `autorun` blocks the
//! just-changed value reaches (`meshfox_core::autorun_blocks_for_changed_vars`)
//! — no `r` press anywhere in this flow.

use std::time::Duration;

use crate::fixtures;
use crate::harness::TuiSession;

#[test]
#[ignore = "pty-based e2e — run via `cargo test --test tui_e2e -- --ignored`"]
fn filling_in_a_form_field_and_clicking_send_autoruns_the_dependent_block() {
    let (canvas_path, dir) = fixtures::write_fixture(fixtures::FORM_AUTORUN);
    let mut session = TuiSession::spawn(&canvas_path, dir, 30, 100);
    session
        .wait_for("Greeting", Duration::from_secs(5))
        .expect("the form's own field label should render inline, not as raw code");
    assert!(
        !session.screen_text().contains("field var"),
        "the form fence's raw source shouldn't show while not editing its own node's body"
    );
    // Not a bare `"greeting is"` — the autorun block's own raw source
    // (`echo "greeting is $GREETING"`, shown as-is in the Document pane
    // right below the form) already contains that substring literally,
    // with nothing having run at all. Only the *interpolated* value only
    // real output could ever produce actually proves that.
    assert!(
        !session.screen_text().contains("greeting is Hello"),
        "nothing should have run yet"
    );

    let (row, col) = session.find("Greeting").expect("the field's own label");
    session.send_mouse_click(row, col);
    session.send_keys("Hello");
    session
        .wait_for("Greeting: Hello", Duration::from_secs(5))
        .expect("typing into the field should show up live, before Send is even clicked");

    let (row, col) = session.find("[Send]").expect("the form's own Send marker");
    session.send_mouse_click(row, col);

    session
        .wait_for("greeting is Hello", Duration::from_secs(10))
        .expect(
            "clicking Send should commit the field and automatically start the autorun block \
         that references it, with no r press",
        );
}

/// Exercise the real event loop and renderer: selecting a value changes
/// only the draft; Send is still required to commit it and trigger autorun.
#[test]
#[ignore = "pty-based e2e — run via `cargo test --test tui_e2e -- --ignored`"]
fn select_dropdown_supports_keyboard_cancel_mouse_and_explicit_send() {
    let (canvas_path, dir) = fixtures::write_fixture(concat!(
        "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
        "<!-- meshfox:var name=\"TUI_SELECT_CHOICE\" type=\"select\" ",
        "choices=\"AlphaChoice,BetaChoice,GammaChoice\" default=\"AlphaChoice\" session -->\n\n",
        "```form name=\"pick\" send=\"Send\"\n",
        "field var=\"TUI_SELECT_CHOICE\" label=\"Choice\"\n```\n\n",
        "```bash name=\"show\" env=\"$TUI_SELECT_CHOICE\" autorun\n",
        "printf '%s\\n' \"$TUI_SELECT_CHOICE\" >> select-submissions\n",
        "echo \"selected choice is $TUI_SELECT_CHOICE\"\n```\n",
    ));
    let submissions = dir.join("select-submissions");
    let mut session = TuiSession::spawn(&canvas_path, dir, 30, 110);
    let timeout = Duration::from_secs(5);
    session.wait_for("Choice: [", timeout).unwrap();

    // A field click opens a list of actual options, rather than a text input.
    let (row, col) = session.find("Choice:").unwrap();
    session.send_mouse_click(row, col);
    session.wait_for("Choose · Enter select", timeout).unwrap();
    session.wait_for("› AlphaChoice", timeout).unwrap();
    assert!(session.screen_text().contains("BetaChoice"));
    assert!(session.screen_text().contains("GammaChoice"));
    session.send_keys("\x1b[B"); // Down
    session.wait_for("› BetaChoice", timeout).unwrap();
    session.send_keys("\r");
    session
        .wait_for("Choice: [ BetaChoice ▾ ]", timeout)
        .unwrap();
    assert!(!session
        .screen_text()
        .contains("selected choice is BetaChoice"));

    // Enter reopens the list at the current value; Escape discards navigation.
    session.send_keys("\r");
    session.wait_for("› BetaChoice", timeout).unwrap();
    session.send_keys("\x1b[B");
    session.wait_for("› GammaChoice", timeout).unwrap();
    session.send_keys("\x1b");
    let deadline = std::time::Instant::now() + timeout;
    while session.screen_text().contains("Choose · Enter select") {
        assert!(
            std::time::Instant::now() < deadline,
            "Escape did not close the list"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(session.screen_text().contains("Choice: [ BetaChoice ▾ ]"));

    // Space opens the same list and a mouse click confirms an option.
    session.send_keys(" ");
    session.wait_for("Choose · Enter select", timeout).unwrap();
    let (row, col) = session.find("GammaChoice").unwrap();
    session.send_mouse_click(row, col);
    session
        .wait_for("Choice: [ GammaChoice ▾ ]", timeout)
        .unwrap();
    assert!(!session
        .screen_text()
        .contains("selected choice is GammaChoice"));

    // Only a separate Send commits the choice and starts the dependent block.
    let (row, col) = session.find("[Send]").unwrap();
    session.send_mouse_click(row, col);
    session
        .wait_for("selected choice is GammaChoice", Duration::from_secs(10))
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(submissions).unwrap(),
        "GammaChoice\n"
    );
}
