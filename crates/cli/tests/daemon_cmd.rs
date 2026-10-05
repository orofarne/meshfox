//! The macOS menu-bar daemon (`macos/MeshfoxDaemon`) against the same
//! scenario `meshfox serve` has to pass (`common::run_scenario`): the two
//! are separate implementations of one wire protocol, and this is what keeps
//! them from drifting apart.
//!
//! Ignored by default: it needs the Swift package built first and it puts
//! the daemon's 🦊 in the menu bar for a few seconds. Run it with
//!
//! ```text
//! (cd macos/MeshfoxDaemon && swift build)
//! cargo test -p meshfox-cli --test daemon_cmd -- --ignored
//! ```
#![cfg(target_os = "macos")]

mod common;

use common::*;
use std::path::Path;
use std::process::{Command, Stdio};

#[test]
#[ignore = "needs `swift build` in macos/MeshfoxDaemon; shows a menu-bar icon while it runs"]
fn macos_daemon_spawns_lists_and_kills_cores() {
    let bin = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../macos/MeshfoxDaemon/.build/debug/MeshfoxDaemon");
    assert!(
        bin.exists(),
        "{} is missing; run `swift build` in macos/MeshfoxDaemon first",
        bin.display()
    );

    let dir = unique_dir();
    let socket = dir.join("d.sock");
    // With a `server_socket` already configured the daemon doesn't open its
    // first-launch "connect the CLI?" dialog, which would block the run.
    std::fs::create_dir_all(dir.join(".meshfox")).unwrap();
    std::fs::write(
        dir.join(".meshfox/config.toml"),
        format!("server_socket = {:?}\n", socket.to_str().unwrap()),
    )
    .unwrap();

    let mut daemon = Reaper(
        Command::new(bin)
            .env("HOME", &dir)
            .env("MESHFOX_BIN", env!("CARGO_BIN_EXE_meshfox"))
            .env("MESHFOX_DAEMON_SOCKET", &socket)
            // `.cargo/config.toml` sets this to "" for every cargo run, and
            // an env value beats the `server_socket` in the config above.
            .env("MESHFOX_SERVER_SOCKET", &socket)
            // Not inherited: a daemon still holding the harness's stdout pipe
            // would keep `cargo test` waiting even after a failed run.
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );

    wait_for("the daemon to answer", || {
        socket.exists() && cores(&dir, &socket, &["ls"]).status.success()
    });

    run_scenario(&dir, &socket);

    terminate(&mut daemon);
    assert!(!socket.exists(), "a bound socket is unlinked on exit");
    let _ = std::fs::remove_dir_all(&dir);
}
