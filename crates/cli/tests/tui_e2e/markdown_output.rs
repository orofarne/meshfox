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
    session
        .wait_for("Root", Duration::from_secs(5))
        .expect("initial render");

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

/// Mirrors scripts/screencast/README.canvas.md: an optional output_dir argument,
/// accepted unchanged in the run form, followed by streamed plain output.
#[test]
#[ignore = "pty-based e2e — run via `cargo test --test tui_e2e -- --ignored`"]
fn parameterized_record_shows_running_status_and_output_beneath_its_source() {
    let source = concat!(
        "<!-- meshfox:canvas -->\n# Record screencast\n",
        "<!-- meshfox:node id=\"record-screencast\" -->\n",
        "<!-- meshfox:arg name=\"output_dir\" default=\"../../target/screencast\" -->\n",
        "```bash name=\"record\"\n",
        "printf 'record%s: %s\\n' ing \"$output_dir\"\nsleep 3\n",
        "```\n",
    );
    let (canvas_path, dir) = fixtures::write_fixture(source);
    let mut session = TuiSession::spawn(&canvas_path, dir, 40, 120);
    session
        .wait_for("Record screencast", Duration::from_secs(5))
        .expect("initial render");
    session.send_keys("r");
    session
        .wait_for("arguments for this application", Duration::from_secs(5))
        .expect("argument form");
    session.send_keys("\r");
    // This header exists only in the Document pane, never in the console.
    session
        .wait_for("output: record ·", Duration::from_secs(10))
        .expect("inline output frame");
    session
        .wait_for("live · running", Duration::from_secs(2))
        .expect("block running status");
    session
        .wait_for("recording: ../../target/screencast", Duration::from_secs(2))
        .expect("streamed output");
    let screen = session.screen_text();
    let frame = screen.find("output: record ·").unwrap();
    assert!(
        screen[frame..].contains("recording: ../../target/screencast"),
        "stdout beneath source:\n{screen}"
    );
    session
        .wait_for("live · done", Duration::from_secs(10))
        .expect("block completion");
    assert!(session
        .screen_text()
        .contains("recording: ../../target/screencast"));
}

#[test]
#[ignore = "pty-based e2e — run via `cargo test --test tui_e2e -- --ignored`"]
fn color_spans_paint_terminal_cells_and_restore_nested_colors() {
    let source = concat!(
        "<!-- meshfox:canvas -->\n# Colors\n<!-- meshfox:node id=\"root\" -->\n\n",
        "[ColorOuter **ColorBold** [ColorInner]{color=\"#abc\"} ColorTail]{color=\"#e06c75\" background-color=\"#fff3b0\"} PlainAfter\n\n",
        "[BackgroundOnly]{background-color=#abc}\n\n",
        "| Table |\n| --- |\n| [ColorCell]{color=#e06c75 background-color=#fff3b0} |\n\n",
        "`[LiteralCode]{color=#fff}`\n\n",
        "```starlark render name=\"colors\" output=\"markdown\"\n",
        "print(md.background_color(md.color('StarlarkColor', '#e06c75'), '#fff3b0'))\n```\n",
    );
    let (canvas_path, dir) = fixtures::write_fixture(source);
    let session = TuiSession::spawn(&canvas_path, dir, 50, 140);
    session
        .wait_for("StarlarkColor", Duration::from_secs(10))
        .expect("rendered colors");
    for word in [
        "ColorOuter",
        "ColorBold",
        "ColorTail",
        "ColorCell",
        "StarlarkColor",
    ] {
        let (r, c) = session
            .find(word)
            .unwrap_or_else(|| panic!("missing {word}: {}", session.screen_text()));
        assert_eq!(
            session.fgcolor_at(r, c),
            Some(vt100::Color::Rgb(224, 108, 117)),
            "foreground {word} at {r},{c}: {}",
            session.screen_text()
        );
        assert_eq!(
            session.bgcolor_at(r, c),
            Some(vt100::Color::Rgb(255, 243, 176)),
            "background {word}"
        );
    }
    let (r, c) = session.find("ColorInner").unwrap();
    assert_eq!(
        session.fgcolor_at(r, c),
        Some(vt100::Color::Rgb(170, 187, 204))
    );
    assert_eq!(
        session.bgcolor_at(r, c),
        Some(vt100::Color::Rgb(255, 243, 176))
    );
    let (r, c) = session.find("BackgroundOnly").unwrap();
    assert_eq!(
        session.bgcolor_at(r, c),
        Some(vt100::Color::Rgb(170, 187, 204))
    );
    let (r, c) = session.find("PlainAfter").unwrap();
    assert_ne!(
        session.fgcolor_at(r, c),
        Some(vt100::Color::Rgb(224, 108, 117))
    );
    assert_ne!(
        session.bgcolor_at(r, c),
        Some(vt100::Color::Rgb(255, 243, 176))
    );
    assert!(session.screen_text().contains("[LiteralCode]{color=#fff}"));
}
