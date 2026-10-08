//! `display="table"` and `display="code"` in the TUI, end to end: the real
//! binary in a real pty, its real worker, and the real `duckdb` CLI (tests
//! that need it skip themselves when there is none). `display="code"` is
//! here because it now reads through the worker's API like the table does,
//! instead of straight off disk.

use std::time::Duration;

use crate::fixtures;
use crate::harness::TuiSession;

const CANVAS: &str = concat!(
    "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
    "## Notes\n<!-- meshfox:node id=\"notes\" parent=\"root\" type=\"file\" display=\"code\" -->\n\n[hello.txt](hello.txt)\n\n",
    "## Sales\n<!-- meshfox:node id=\"sales\" parent=\"root\" type=\"file\" display=\"table\" -->\n\n[sales.csv](sales.csv)\n",
);

fn duckdb_available() -> bool {
    if let Ok(p) = std::env::var("MESHFOX_DUCKDB") {
        return std::path::Path::new(&p).exists();
    }
    let on_path = std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).any(|d| d.join("duckdb").exists()))
        .unwrap_or(false);
    on_path
        || [
            "/opt/homebrew/bin/duckdb",
            "/usr/local/bin/duckdb",
            "/usr/bin/duckdb",
        ]
        .iter()
        .any(|p| std::path::Path::new(p).exists())
}

fn fixture() -> (std::path::PathBuf, std::path::PathBuf) {
    let (canvas_path, dir) = fixtures::write_fixture(CANVAS);
    std::fs::write(dir.join("hello.txt"), "hello from the file on disk\n").unwrap();
    let mut csv = String::from("id,name,amount\n");
    for i in 1..=300 {
        csv.push_str(&format!("{i},name{i:03},{}\n", i * 2));
    }
    std::fs::write(dir.join("sales.csv"), csv).unwrap();
    (canvas_path, dir)
}

#[test]
#[ignore = "pty-based e2e — run via `cargo test --test tui_e2e -- --ignored`"]
fn a_code_preview_is_read_through_the_worker_and_follows_the_file() {
    let (canvas_path, dir) = fixture();
    let file = dir.join("hello.txt");
    let mut session = TuiSession::spawn(&canvas_path, dir, 30, 110);
    session
        .wait_for("Root", Duration::from_secs(5))
        .expect("initial render");

    session.send_keys("j"); // Notes
    session
        .wait_for("hello from the file on disk", Duration::from_secs(10))
        .expect("the file's content, fetched from the worker");

    // The pane re-reads once the cached copy is a couple of seconds old: edit
    // the file, move away and back, and the new content appears.
    std::fs::write(&file, "the file was edited\n").unwrap();
    std::thread::sleep(Duration::from_millis(2300));
    session.send_keys("j");
    session.send_keys("k");
    session
        .wait_for("the file was edited", Duration::from_secs(10))
        .expect("the refreshed content");
}

#[test]
#[ignore = "pty-based e2e — run via `cargo test --test tui_e2e -- --ignored`"]
fn a_table_shows_inline_and_opens_full_screen_with_sort_filter_and_search() {
    if !duckdb_available() {
        eprintln!("skipping: no duckdb CLI");
        return;
    }
    let (canvas_path, dir) = fixture();
    let mut session = TuiSession::spawn(&canvas_path, dir, 36, 110);
    session
        .wait_for("Root", Duration::from_secs(5))
        .expect("initial render");

    session.send_keys("jj"); // Sales
    session
        .wait_for("300 rows × 3 cols", Duration::from_secs(20))
        .expect("the inline status line once the import is done");
    session
        .wait_for("name001", Duration::from_secs(5))
        .expect("the first rows inline");
    assert!(
        !session.screen_text().contains("name011"),
        "only the head of the table is inline:\n{}",
        session.screen_text()
    );

    // Full screen.
    session.send_keys("\r");
    session
        .wait_for("q close", Duration::from_secs(5))
        .expect("the full-screen table's key hint");
    session.send_keys("G");
    session
        .wait_for("name300", Duration::from_secs(10))
        .expect("the last row after G");
    session.send_keys("g");
    session
        .wait_for("name001", Duration::from_secs(10))
        .expect("back at the top");

    // Sort the id column descending (s twice): the top row becomes 300.
    session.send_keys("ss");
    session
        .wait_for("▼", Duration::from_secs(10))
        .expect("sort marker");
    session
        .wait_for("name300", Duration::from_secs(10))
        .expect("descending id puts name300 first");
    assert!(
        !session.screen_text().contains("name001"),
        "name001 is the last row now:\n{}",
        session.screen_text()
    );

    // Reset, then search.
    session.send_keys("x");
    session
        .wait_for("300 rows × 3 cols", Duration::from_secs(10))
        .expect("reset");
    session.send_keys("/name042\r");
    session
        .wait_for("1 of 300 rows", Duration::from_secs(10))
        .expect("search narrows the view");
    session.send_keys("x");

    // Filter the amount column: amount is 2 * id, so >=590 leaves 6 rows.
    session.send_keys("ll");
    session.send_keys("f>=590\r");
    session
        .wait_for("6 of 300 rows", Duration::from_secs(10))
        .expect("the column filter");
    session.send_keys("x");

    // A value the column can't hold is reported, not fatal.
    session.send_keys("f=abc\r");
    session
        .wait_for("abc", Duration::from_secs(10))
        .expect("the worker's complaint");
    session.send_keys("x");

    session.send_keys("q");
    session
        .wait_for("Document", Duration::from_secs(5))
        .expect("back in the three-pane view");
    assert!(
        !session.screen_text().contains("q close"),
        "the full-screen table is gone:\n{}",
        session.screen_text()
    );
}

#[test]
#[ignore = "pty-based e2e — run via `cargo test --test tui_e2e -- --ignored`"]
fn the_wheel_scrolls_and_a_header_click_sorts() {
    if !duckdb_available() {
        eprintln!("skipping: no duckdb CLI");
        return;
    }
    let (canvas_path, dir) = fixture();
    let mut session = TuiSession::spawn(&canvas_path, dir, 36, 110);
    session
        .wait_for("Root", Duration::from_secs(5))
        .expect("initial render");
    session.send_keys("jj");
    session
        .wait_for("300 rows × 3 cols", Duration::from_secs(20))
        .expect("ready");
    session.send_keys("\r");
    session
        .wait_for("q close", Duration::from_secs(5))
        .expect("full screen");

    let (row, col) = session.find("name001").expect("first row");
    for _ in 0..4 {
        session.send_mouse_scroll(row, col, true);
    }
    session
        .wait_for("name013", Duration::from_secs(10))
        .expect("12 rows down after four wheel notches");

    // Clicking the `amount` header sorts by it (ascending, then descending).
    let (hrow, hcol) = session.find("amount").expect("amount header");
    session.send_mouse_click(hrow, hcol);
    session
        .wait_for("▲", Duration::from_secs(10))
        .expect("ascending");
    session.send_mouse_click(hrow, hcol);
    session
        .wait_for("▼", Duration::from_secs(10))
        .expect("descending");
    session
        .wait_for("name300", Duration::from_secs(10))
        .expect("largest amount first");
}
