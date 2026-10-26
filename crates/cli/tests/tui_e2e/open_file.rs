//! `o` on a `file` node with `[tui.open]` configured: the TUI itself runs the
//! configured command — on its terminal, or detached with a trailing `&`. The
//! "programs" here are shell scripts that record how they were started.

use std::path::Path;
use std::time::Duration;

use crate::fixtures;
use crate::harness::TuiSession;

const CANVAS: &str = concat!(
    "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
    "## Sheet\n<!-- meshfox:node id=\"sheet\" parent=\"root\" type=\"file\" -->\n\n[data.ods](data.ods)\n",
);

fn fixture(open_value: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let (canvas_path, dir) = fixtures::write_fixture(CANVAS);
    std::fs::write(dir.join("data.ods"), "x").unwrap();
    let script = dir.join("fake-app.sh");
    // `-t 0`: was it given a terminal. Written last, so the log is complete
    // once it exists.
    std::fs::write(
        &script,
        format!(
            "if [ -t 0 ]; then t=tty; else t=notty; fi\necho \"$1 $t\" > '{0}.tmp'\nmv '{0}.tmp' '{0}'\n",
            dir.join("app.log").display()
        ),
    )
    .unwrap();
    std::fs::create_dir_all(dir.join(".meshfox")).unwrap();
    std::fs::write(
        dir.join(".meshfox/config.toml"),
        format!(
            "[tui.open]\n\"xls,*.ODS\" = \"sh '{}'{open_value}\"\n",
            script.display()
        ),
    )
    .unwrap();
    (canvas_path, dir)
}

fn open_selected_file(canvas_path: &Path, dir: std::path::PathBuf) -> TuiSession {
    let mut session = TuiSession::spawn(canvas_path, dir, 30, 100);
    session
        .wait_for("Sheet", Duration::from_secs(5))
        .expect("the tree should render");
    session.send_keys("j");
    session.send_keys("o");
    session
}

#[test]
#[ignore = "pty-based e2e — run via `cargo test --test tui_e2e -- --ignored`"]
fn o_runs_the_configured_command_on_the_terminal_and_comes_back() {
    let (canvas_path, dir) = fixture("");
    let session = open_selected_file(&canvas_path, dir.clone());
    session
        .wait_for("closed sh", Duration::from_secs(10))
        .unwrap_or_else(|e| panic!("{e}"));

    let log = std::fs::read_to_string(dir.join("app.log")).unwrap();
    let (file, tty) = log.trim().split_once(' ').unwrap();
    assert!(file.ends_with("data.ods"), "file: {file}");
    assert_eq!(tty, "tty", "the command should own the terminal");
    assert!(session.in_alternate_screen());
    assert!(session.screen_text().contains("Sheet"));
}

#[test]
#[ignore = "pty-based e2e — run via `cargo test --test tui_e2e -- --ignored`"]
fn a_trailing_ampersand_runs_it_in_the_background_without_a_terminal() {
    let (canvas_path, dir) = fixture(" &");
    let session = open_selected_file(&canvas_path, dir.clone());
    session
        .wait_for("opened with sh", Duration::from_secs(10))
        .unwrap_or_else(|e| panic!("{e}"));
    for _ in 0..100 {
        if dir.join("app.log").exists() {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let log = std::fs::read_to_string(dir.join("app.log")).unwrap();
    assert!(log.trim().ends_with("data.ods notty"), "log: {log}");
    assert!(session.in_alternate_screen());
}
