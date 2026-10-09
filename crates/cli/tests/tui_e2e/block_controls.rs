//! Block selection, independent folds and direct run/history controls.
use crate::{fixtures, harness::TuiSession};
use std::time::{Duration, Instant};

fn wait_absent(session: &TuiSession, text: &str) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while session.screen_text().contains(text) {
        assert!(
            Instant::now() < deadline,
            "{text} remains on screen:\n{}",
            session.screen_text()
        );
        std::thread::sleep(Duration::from_millis(30));
    }
}

fn click_control(session: &mut TuiSession, text: &str) {
    // The tree also has a [run] badge; document controls occur below it.
    let screen = session.screen_text();
    let (row, col) = screen
        .lines()
        .collect::<Vec<_>>()
        .into_iter()
        .enumerate()
        .rev()
        .find_map(|(row, line)| {
            line.rfind(text)
                .map(|byte| (row as u16, line[..byte].chars().count() as u16))
        })
        .unwrap_or_else(|| panic!("missing {text}:\n{screen}"));
    session.send_mouse_click(row, col + 1);
}

#[test]
#[ignore = "pty-based e2e"]
fn keyboard_block_selection_targets_run_and_history_and_esc_restores_picker() {
    let (path, dir) = fixtures::write_fixture(concat!(
        "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n",
        "```sh name=first fold\nprintf FIRST_RESULT\n```\n",
        "```sh name=second fold\nprintf SECOND_RESULT\n```\n",
    ));
    let mut session = TuiSession::spawn(&path, dir, 30, 110);
    session
        .wait_for("[code ▸]", Duration::from_secs(5))
        .unwrap();
    session.send_keys("\t]]");
    session
        .wait_for("block: second", Duration::from_secs(5))
        .unwrap();
    session.send_keys("R");
    session
        .wait_for("SECOND_RESULT", Duration::from_secs(10))
        .unwrap();
    assert!(!session.screen_text().contains("FIRST_RESULT"));
    session.wait_for("done", Duration::from_secs(5)).unwrap();
    session.send_keys("L");
    session
        .wait_for("run history — root/second", Duration::from_secs(5))
        .unwrap();
    session.wait_for("exit 0", Duration::from_secs(5)).unwrap();
    session.send_keys("\x1b");
    wait_absent(&session, "run history —");
    session.send_keys("c");
    session
        .wait_for("printf SECOND_RESULT", Duration::from_secs(5))
        .unwrap();
    session.send_keys("c");
    wait_absent(&session, "printf SECOND_RESULT");
    assert!(session.screen_text().contains("SECOND_RESULT"));
    session.send_keys("\r");
    wait_absent(&session, "SECOND_RESULT");
    session.send_keys("\r");
    session
        .wait_for("SECOND_RESULT", Duration::from_secs(5))
        .unwrap();
    session.send_keys("\x1b");
    wait_absent(&session, "block: second");
    session.send_keys("r");
    session
        .wait_for("which block?", Duration::from_secs(5))
        .unwrap();
}

#[test]
#[ignore = "pty-based e2e"]
fn mouse_controls_work_on_wrapped_header_and_keep_run_access_when_folded() {
    let (path, dir) = fixtures::write_fixture(concat!(
        "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n",
        "```sh name=build fold\nprintf MOUSE_RESULT\n```\n",
    ));
    // Document is narrow enough that run/history controls wrap.
    let mut session = TuiSession::spawn(&path, dir, 30, 60);
    session.wait_for("[run]", Duration::from_secs(5)).unwrap();
    click_control(&mut session, "[run]");
    session
        .wait_for("MOUSE_RESULT", Duration::from_secs(10))
        .unwrap();
    session.wait_for("done", Duration::from_secs(5)).unwrap();
    click_control(&mut session, "[history]");
    session
        .wait_for("run history — root/build", Duration::from_secs(5))
        .unwrap();
    session.send_keys("\x1b");
    wait_absent(&session, "run history —");
    click_control(&mut session, "[code ▸]");
    session
        .wait_for("printf MOUSE_RESULT", Duration::from_secs(5))
        .unwrap();
    click_control(&mut session, "[▾]");
    wait_absent(&session, "MOUSE_RESULT");
    assert!(session.screen_text().contains("[run]"));
    click_control(&mut session, "[▸]");
    session
        .wait_for("printf MOUSE_RESULT", Duration::from_secs(5))
        .unwrap();
    session
        .wait_for("output: build", Duration::from_secs(5))
        .unwrap();
}
