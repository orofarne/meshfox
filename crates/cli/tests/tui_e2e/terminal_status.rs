use crate::harness::TuiSession;
use std::time::Duration;

fn fixture(body: &str) -> (TuiSession, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "mfx-status-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let canvas = dir.join("doc.md");
    std::fs::write(&canvas, format!("<!-- meshfox:canvas -->\n# Status fixture\n<!-- meshfox:node id=\"root\" -->\n\n{body}")).unwrap();
    let session = TuiSession::spawn_with_status(&canvas, dir, 30, 110, "iterm2");
    session
        .wait_for("Status fixture", Duration::from_secs(10))
        .unwrap();
    (session, canvas)
}

fn session(body: &str) -> TuiSession {
    fixture(body).0
}

#[test]
#[ignore = "pty-based e2e"]
fn terminal_status_tracks_run_and_clears_on_quit() {
    let mut session = session("```bash name=build\nsleep 0.3; echo done\n```\n");
    session.clear_raw();
    session.send_keys("r");
    session.wait_for_raw("status=running: build;", Duration::from_secs(5));
    session.wait_for_raw("status=done: build;", Duration::from_secs(5));
    session.clear_raw();
    session.send_keys("q");
    assert!(session.wait_for_exit(Duration::from_secs(5)));
    session.wait_for_raw("status=;indicator=;status-color=", Duration::from_secs(2));
}

#[test]
#[ignore = "pty-based e2e"]
fn terminal_status_tracks_confirmation_then_failure() {
    let mut session = session("```bash name=deploy confirm\nexit 7\n```\n");
    session.send_keys("r");
    session.wait_for_raw("status=waiting: confirmation;", Duration::from_secs(5));
    session.send_keys("y");
    session.wait_for_raw("status=failed: deploy;", Duration::from_secs(5));
    session.send_keys("q");
    assert!(session.wait_for_exit(Duration::from_secs(5)));
}

#[test]
#[ignore = "pty-based e2e"]
fn terminal_status_services_become_ready_then_report_crash() {
    let mut session = session("```bash name=server service\necho ready; sleep 4; exit 7\n```\n");
    session.send_keys("r");
    session.wait_for_raw("status=ready: 1 services;", Duration::from_secs(8));
    session.wait_for_raw("status=failed: server;", Duration::from_secs(10));
    session.send_keys("q");
    assert!(session.wait_for_exit(Duration::from_secs(5)));
}

#[test]
#[ignore = "pty-based e2e"]
fn terminal_status_observes_runs_started_by_another_cli() {
    let (mut session, canvas) = fixture("```bash name=job\necho begin; sleep 0.5; echo end\n```\n");
    session.clear_raw();
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_meshfox"))
        .arg(&canvas)
        .args(["run", "job", "--terminal-status", "off"])
        .env("HOME", canvas.parent().unwrap())
        .env_remove("MESHFOX_SERVER_SOCKET")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    session.wait_for_raw("status=running: job;", Duration::from_secs(5));
    session.wait_for_raw("status=done: job;", Duration::from_secs(5));
    session.send_keys("q");
    assert!(session.wait_for_exit(Duration::from_secs(5)));
}

#[test]
#[ignore = "pty-based e2e"]
fn terminal_status_clears_on_external_shutdown_signal() {
    let mut session = session("```bash name=build\nsleep 30\n```\n");
    session.send_keys("r");
    session.wait_for_raw("status=running: build;", Duration::from_secs(5));
    session.clear_raw();
    session.send_signal(libc::SIGTERM);
    assert!(session.wait_for_exit(Duration::from_secs(5)));
    session.wait_for_raw("status=;indicator=;status-color=", Duration::from_secs(2));
}
