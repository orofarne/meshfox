//! TODO.canvas.md: "Мышь в панелях TUI" — clicking a block's own run
//! affordance in the document pane. The only thing that currently even
//! *looks* clickable there is a `button` fence's `▶ caption (r to run)`
//! marker (`markdown.rs`'s `BUTTON_LANG` branch) — every ordinary runnable
//! block has no inline run control at all, only the tree's `r`/`R`. This
//! test covers clicking that marker running its own `deps=` chain, same as
//! pressing `r` on it would (`markdown::ClickTarget::RunBlock`,
//! `App::activate_click_target`).

use std::time::Duration;

use crate::fixtures;
use crate::harness::TuiSession;

#[test]
#[ignore = "pty-based e2e — run via `cargo test --test tui_e2e -- --ignored`"]
fn clicking_a_button_fences_marker_runs_its_deps_chain() {
    let (canvas_path, dir) = fixtures::write_fixture(fixtures::BUTTON_FENCE);
    let mut session = TuiSession::spawn(&canvas_path, dir, 30, 100);
    session
        .wait_for("Go", Duration::from_secs(5))
        .expect("the button fence's own caption should render");
    assert!(
        !session.screen_text().contains("tui-e2e-button-marker"),
        "nothing should have run yet"
    );

    let (row, col) = session.find("Go").expect("▶ Go marker");
    session.send_mouse_click(row, col);

    session
        .wait_for("tui-e2e-button-marker", Duration::from_secs(10))
        .expect("clicking the button marker should run its deps= chain, same as pressing r on it");
}

#[test]
#[ignore = "pty-based e2e"]
fn confirm_button_waits_for_y_and_does_not_remember_approval() {
    let (canvas_path, dir) = fixtures::write_fixture(concat!(
        "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n",
        "```bash name=\"cleanup\" confirm\necho approved >> effects\n```\n",
        "```button name=\"go\" deps=\"cleanup\"\nGo\n```\n",
    ));
    let effect = canvas_path.parent().unwrap().join("effects");
    let mut session = TuiSession::spawn(&canvas_path, dir, 30, 100);
    session.wait_for("Go", Duration::from_secs(10)).unwrap();
    let (row, col) = session.find("Go").unwrap();
    session.send_mouse_click(row, col);
    session
        .wait_for("confirm run", Duration::from_secs(10))
        .unwrap();
    assert!(!effect.exists());
    session.send_keys("\r");
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while session.screen_text().contains("confirm run") && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(30));
    }
    assert!(!session.screen_text().contains("confirm run"));
    assert!(!effect.exists());
    let (row, col) = session.find("Go").unwrap();
    session.send_mouse_click(row, col);
    session
        .wait_for("confirm run", Duration::from_secs(10))
        .unwrap();
    session.send_keys("y");
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while !effect.exists() && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(std::fs::read_to_string(&effect).unwrap(), "approved\n");
    // The file is written before StepDone/Done reaches the TUI. Wait for
    // completion before requesting another run, otherwise its busy guard
    // correctly rejects the click and no confirmation can appear.
    session
        .wait_for("Output (done)", Duration::from_secs(5))
        .unwrap();
    let (row, col) = session.find("Go").unwrap();
    session.send_mouse_click(row, col);
    session
        .wait_for("confirm run", Duration::from_secs(10))
        .unwrap();
    session.send_keys("\x1b");
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while session.screen_text().contains("confirm run") && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(30));
    }
    assert!(!session.screen_text().contains("confirm run"));
    assert_eq!(std::fs::read_to_string(effect).unwrap(), "approved\n");
}
