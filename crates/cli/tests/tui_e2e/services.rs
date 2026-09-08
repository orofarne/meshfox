//! Real-pty coverage for `service` blocks in the TUI (`crates/cli/src/tui/`)
//! — **experimental**, see SPEC.md's "Service blocks (experimental)". Same
//! harness every other suite here uses (`harness::TuiSession`); see
//! `main.rs`'s own module doc comment for why every test here is
//! `#[ignore]`d.

use std::time::Duration;

use crate::fixtures;
use crate::harness::TuiSession;

fn is_alive(pid: u32) -> bool {
    // SAFETY: signal 0 sends nothing, just probes existence/permission.
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

/// Pulls the pid out of "==> srv (service started, pid 1234)", however it's
/// currently wrapped/positioned on screen — `TuiSession::screen_text` joins
/// rendered rows with `\n`, so a run's own transcript line can appear
/// anywhere in that blob depending on pane heights.
fn find_service_pid(screen: &str) -> Option<u32> {
    let marker = "service started, pid ";
    let start = screen.find(marker)? + marker.len();
    let digits: String = screen[start..].chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
}

#[test]
#[ignore = "pty-based e2e — run via `cargo test --test tui_e2e -- --ignored`"]
fn running_a_service_block_streams_a_started_line_and_shows_live_glyphs_then_quit_stops_it() {
    let (canvas_path, dir) = fixtures::write_fixture(fixtures::SERVICE_RUNNABLE);
    let mut session = TuiSession::spawn(&canvas_path, dir, 30, 100);
    session
        .wait_for("Root", Duration::from_secs(5))
        .expect("initial render");

    // Root's own sole block is the service — already selected, no need to
    // move down first.
    session.send_keys("r");

    session
        .wait_for("service started, pid", Duration::from_secs(10))
        .expect("the service-started line should show up in the Output pane");
    // Parsed now, before anything else touches the screen — the services
    // view (below) covers/scrolls other panes, so this line isn't
    // guaranteed to still be readable verbatim afterward.
    let pid = find_service_pid(&session.screen_text()).expect("parse the service's own pid");
    assert!(is_alive(pid), "service should actually be running at this point");

    // `v` — the services view is how you actually see a daemon's own log
    // in the TUI (`ui::render_services_view`'s log pane, below the list).
    // Checking the marker text shows up *there* (not just anywhere on
    // screen) also proves the chain didn't wait for the process to exit
    // (`sleep 30`) before this — the service was already streaming real
    // captured output by the time this opens, not just "spawned, nothing
    // heard from yet".
    session.send_keys("v");
    session
        .wait_for("srv log", Duration::from_secs(5))
        .expect("the services view's log pane should be titled after the selected service");
    session
        .wait_for("tui-e2e-service-marker", Duration::from_secs(5))
        .expect("the service's own real stdout should show up in its log pane");
    session.send_keys("q");

    // Row glyph: a live, running service is `●service` (green) — not the
    // declared-but-idle `○service` a node with the attribute but nothing
    // tracked would show.
    session
        .wait_for("●service", Duration::from_secs(5))
        .expect("the tree row should show the running-service glyph");
    // Footer aggregate.
    session
        .wait_for("service(s) running", Duration::from_secs(5))
        .expect("the footer should show the service aggregate line");

    session.send_keys("q");
    assert!(
        session.wait_for_exit(Duration::from_secs(5)),
        "meshfox tui should still exit on q with a service running"
    );

    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let mut still_alive = is_alive(pid);
    while still_alive && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
        still_alive = is_alive(pid);
    }
    assert!(
        !still_alive,
        "service pid {pid} survived quitting the TUI — it was orphaned, not stopped"
    );
}

/// Pulls the pid out of "running · pid 1234" or "stopped · pid 1234" —
/// whichever `render_services_view` currently shows for the one service
/// this fixture has.
fn find_list_pid(screen: &str, status_word: &str) -> Option<u32> {
    let marker = format!("{status_word} · pid ");
    let start = screen.find(&marker)? + marker.len();
    let digits: String = screen[start..].chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
}

#[test]
#[ignore = "pty-based e2e — run via `cargo test --test tui_e2e -- --ignored`"]
fn v_opens_the_services_view_and_s_r_stop_and_restart() {
    let (canvas_path, dir) = fixtures::write_fixture(fixtures::SERVICE_RUNNABLE);
    let mut session = TuiSession::spawn(&canvas_path, dir, 30, 100);
    session
        .wait_for("Root", Duration::from_secs(5))
        .expect("initial render");

    session.send_keys("r");
    session
        .wait_for("service started, pid", Duration::from_secs(10))
        .expect("service should start");
    let pid = find_service_pid(&session.screen_text()).expect("parse the service's own pid");

    session.send_keys("v");
    session
        .wait_for("services", Duration::from_secs(5))
        .expect("the services view should open");
    session
        .wait_for("running · pid", Duration::from_secs(5))
        .expect("the list should show the running service");
    assert_eq!(
        find_list_pid(&session.screen_text(), "running"),
        Some(pid),
        "list should show the same pid the run just reported"
    );

    session.send_keys("s");
    session
        .wait_for("stopped · pid", Duration::from_secs(5))
        .expect("the list should flip to stopped after s");

    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let mut still_alive = is_alive(pid);
    while still_alive && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
        still_alive = is_alive(pid);
    }
    assert!(!still_alive, "pid {pid} should be dead after stopping via s");

    session.send_keys("r");
    session
        .wait_for("running · pid", Duration::from_secs(5))
        .expect("the list should flip back to running after r");
    let restarted_pid = find_list_pid(&session.screen_text(), "running").expect("parse the restarted pid");
    assert_ne!(restarted_pid, pid, "restart should give it a fresh pid");
    assert!(is_alive(restarted_pid), "restarted service should actually be running");

    // Closing the view uncovers the tree again, glyph and all.
    session.send_keys("q");
    session
        .wait_for("●service", Duration::from_secs(5))
        .expect("closing the view should reveal the running-service glyph on the row again");
}
