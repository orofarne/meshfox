//! Read-mode selection through the actual event loop and terminal handoff.
use crate::{fixtures, harness::TuiSession};
use base64::Engine;
use std::time::Duration;
const WAIT: Duration = Duration::from_secs(8);
const CANVAS: &str = concat!(
    "<!-- meshfox:canvas -->\n# Copy demo\n<!-- meshfox:node id=\"root\" -->\n\n",
    "alpha beta gamma delta\n\n",
    "```bash name=\"hello\"\nprintf COPY_OUTPUT\\n\n```\n",
);
fn spawn(config: &str) -> TuiSession {
    let (path, dir) = fixtures::write_fixture(CANVAS);
    std::fs::create_dir_all(dir.join(".meshfox")).unwrap();
    std::fs::write(
        dir.join(".meshfox/config.toml"),
        format!("[tui]\n{config}\n"),
    )
    .unwrap();
    let session = TuiSession::spawn(&path, dir, 30, 110);
    session.wait_for("alpha beta", WAIT).unwrap();
    session
}
fn expect_clipboard(session: &TuiSession, text: &str) {
    let b64 = base64::engine::general_purpose::STANDARD.encode(text);
    let expected = format!("\x1b]52;c;{b64}\x07");
    let deadline = std::time::Instant::now() + WAIT;
    loop {
        let raw = session.raw_output();
        if raw
            .windows(expected.len())
            .any(|w| w == expected.as_bytes())
        {
            return;
        }
        if std::time::Instant::now() >= deadline {
            let raw = String::from_utf8_lossy(&raw);
            let copies = raw
                .split("\x1b]52;c;")
                .skip(1)
                .filter_map(|s| s.split_once('\x07'))
                .map(|(s, _)| {
                    String::from_utf8_lossy(
                        &base64::engine::general_purpose::STANDARD
                            .decode(s)
                            .unwrap_or_default(),
                    )
                    .into_owned()
                })
                .collect::<Vec<_>>();
            panic!(
                "expected {text:?}, terminal received {copies:?}\n{}",
                session.screen_text()
            );
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}
#[test]
#[ignore = "pty-based e2e"]
fn mouse_and_keyboard_selection_use_terminal_by_default() {
    let mut session = spawn("");
    let (row, col) = session.find("alpha beta").unwrap();
    session.send_mouse_drag(row, col, row, col + 4);
    session.wait_for("SELECT", WAIT).unwrap();
    session.send_keys("y");
    expect_clipboard(&session, "alpha");
    session.send_keys("\x1b");
    session.wait_for("v select text", WAIT).unwrap();
    session.send_mouse_click(row, col);
    session.send_keys("v \x1b[F"); // End selects the first logical line.
    session.wait_for("SELECT", WAIT).unwrap();
    session.send_keys("y");
    expect_clipboard(&session, "alpha beta gamma delta");
    session.send_keys("\x1b");
    session.wait_for("v select text", WAIT).unwrap();
    session.send_keys("q");
    assert!(session.wait_for_exit(WAIT));
}
#[test]
#[ignore = "pty-based e2e"]
fn palette_plain_text_keeps_selection_and_returns_with_mouse_enabled() {
    let mut session = spawn("");
    let (row, col) = session.find("alpha beta").unwrap();
    session.send_mouse_drag(row, col, row, col + 4);
    session.wait_for("SELECT", WAIT).unwrap();
    session.send_keys("\x10");
    session.wait_for("Commands", WAIT).unwrap();
    session.send_keys("copy plain text\r");
    session
        .wait_for("Enter / Esc: return to TUI", WAIT)
        .unwrap();
    assert!(!session.in_alternate_screen());
    session.wait_for_raw("\x1b[?1006l", WAIT);
    assert!(session.screen_text().contains("alpha"));
    session.clear_raw();
    session.send_keys("\r");
    session.wait_for("SELECT", WAIT).unwrap();
    assert!(session.in_alternate_screen());
    session.wait_for_raw("\x1b[?1006h", WAIT);
    session.send_keys("y");
    expect_clipboard(&session, "alpha");
    session.send_keys("\x1b");
    session.wait_for("v select text", WAIT).unwrap();
    session.send_keys("q");
    assert!(session.wait_for_exit(WAIT));
}
#[test]
#[ignore = "pty-based e2e"]
fn text_config_and_explicit_terminal_override_are_independent() {
    let mut session = spawn("clipboard = \"text\"");
    let (row, col) = session.find("alpha beta").unwrap();
    session.send_mouse_drag(row, col, row, col + 4);
    session.wait_for("SELECT", WAIT).unwrap();
    session.send_keys("\x10");
    session.wait_for("Commands", WAIT).unwrap();
    session.send_keys("copy terminal\r");
    expect_clipboard(&session, "alpha");
    session.send_keys("y");
    session
        .wait_for("Enter / Esc: return to TUI", WAIT)
        .unwrap();
    assert!(!session.in_alternate_screen());
    session.send_keys("\r");
    session.wait_for("SELECT", WAIT).unwrap();
    session.send_keys("\x1b");
    session.wait_for("v select text", WAIT).unwrap();
    session.send_keys("q");
    assert!(session.wait_for_exit(WAIT));
}
#[test]
#[ignore = "pty-based e2e"]
fn code_drag_excludes_the_gutter_and_does_not_run_the_block() {
    let mut session = spawn("");
    let (row, col) = session.find("printf COPY_OUTPUT").unwrap();
    session.send_mouse_drag(row, col, row, col + 5);
    session.wait_for("SELECT", WAIT).unwrap();
    session.send_keys("y");
    expect_clipboard(&session, "printf");
    assert!(!session.screen_text().contains("Output (running"));
    session.send_keys("\x1b");
    session.wait_for("block: hello", WAIT).unwrap();
    session.send_keys("q");
    assert!(session.wait_for_exit(WAIT));
}

#[test]
#[ignore = "pty-based e2e"]
fn single_code_click_keeps_normal_keys_and_double_click_selects_a_word() {
    let mut session = spawn("");
    let (row, col) = session.find("printf COPY_OUTPUT").unwrap();
    session.send_mouse_click(row, col);
    session.wait_for("block: hello", WAIT).unwrap();
    session.send_keys("c");
    session.wait_for("code ▸", WAIT).unwrap();
    session.send_keys("c");
    let (row, col) = session.find("alpha beta").unwrap();
    session.send_mouse_double_click(row, col + 1);
    session.wait_for("SELECT", WAIT).unwrap();
    session.send_keys("y");
    expect_clipboard(&session, "alpha");
    session.send_keys("\x1b");
    session.wait_for("v sel", WAIT).unwrap();
    session.send_keys("q");
    assert!(session.wait_for_exit(WAIT));
}

#[test]
#[ignore = "pty-based e2e"]
fn copying_flashes_the_selection_then_restores_it_without_input() {
    let mut session = spawn("");
    let (row, col) = session.find("alpha beta").unwrap();
    session.send_mouse_drag(row, col, row, col + 4);
    session.wait_for("SELECT", WAIT).unwrap();
    session.clear_raw();
    session.send_keys("y");
    expect_clipboard(&session, "alpha");
    let theme = meshfox_core::tui_theme::Theme::darkfox();
    let escape = |color: meshfox_core::tui_theme::ThemeColor| {
        let (r, g, b) = color.approx_rgb().unwrap();
        format!("48;2;{r};{g};{b}m")
    };
    let flash = escape(theme.ui.accent);
    let normal = escape(theme.editor.selection);
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    loop {
        let raw = session.raw_output();
        if let Some(start) = raw.windows(flash.len()).position(|w| w == flash.as_bytes()) {
            if raw[start + flash.len()..]
                .windows(normal.len())
                .any(|w| w == normal.as_bytes())
            {
                break;
            }
        }
        assert!(
            std::time::Instant::now() < deadline,
            "copy flash must expire without input; expected {flash:?} then {normal:?}; raw: {:?}",
            String::from_utf8_lossy(&raw)
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    session.send_keys("\x1b");
    session.wait_for("v select text", WAIT).unwrap();
    session.send_keys("q");
    assert!(session.wait_for_exit(WAIT));
}

#[test]
#[ignore = "pty-based e2e"]
fn keyboard_cursor_can_choose_and_replace_a_middle_selection_without_exiting() {
    let mut session = spawn("");
    let (row, col) = session.find("alpha beta").unwrap();
    session.send_keys("\tv");
    session.wait_for("CURSOR", WAIT).unwrap();
    session.wait_for_raw("\x1b[?25h", WAIT);
    let (current_row, _) = session.cursor_position();
    session.send_keys("\x1b[H");
    let direction = if row >= current_row {
        "\x1b[B"
    } else {
        "\x1b[A"
    };
    for _ in 0..row.abs_diff(current_row) {
        session.send_keys(direction);
    }
    session.send_keys("llllll");
    let deadline = std::time::Instant::now() + WAIT;
    while session.cursor_position() != (row, col + 6) {
        assert!(
            std::time::Instant::now() < deadline,
            "cursor should reach middle of text: {:?}",
            session.cursor_position()
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(!session.screen_text().contains("SELECT ·"));
    session.send_keys(" lll");
    session.wait_for("SELECT", WAIT).unwrap();
    session.send_keys("y");
    expect_clipboard(&session, "beta");
    session.send_keys(" ll ");
    session.send_keys("\x1b[F");
    session.send_keys("y");
    expect_clipboard(&session, "gamma delta");
    session.send_keys("\x1b");
    session.wait_for("v select text", WAIT).unwrap();
    session.send_keys("q");
    assert!(session.wait_for_exit(WAIT));
}
