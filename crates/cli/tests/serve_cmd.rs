//! `meshfox serve` end to end, against real processes: the coordinator
//! answers `get_port` by spawning a real worker, `meshfox cores ls|kill`
//! see and stop it, and the same coordinator works when systemd (here: a
//! `sh` that sets `LISTEN_PID`/`LISTEN_FDS` and `exec`s it) hands it the
//! listening socket.

mod common;

use common::*;
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixListener;
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};

#[test]
fn serve_spawns_lists_and_kills_cores() {
    let dir = unique_dir();
    let socket = dir.join("s.sock");
    let mut serve = Reaper(
        meshfox(&dir)
            .args(["serve", "--socket"])
            .arg(&socket)
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    wait_for("the socket", || socket.exists());

    run_scenario(&dir, &socket);

    // A second coordinator must not steal a live socket.
    let second = meshfox(&dir)
        .args(["serve", "--socket"])
        .arg(&socket)
        .output()
        .unwrap();
    assert!(!second.status.success());
    assert!(String::from_utf8_lossy(&second.stderr).contains("already listening"));

    terminate(&mut serve);
    assert!(!socket.exists(), "a bound socket is unlinked on exit");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn serve_uses_the_socket_systemd_hands_over() {
    let dir = unique_dir();
    let socket = dir.join("a.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let fd = listener.as_raw_fd();

    // `sh` stands in for systemd: LISTEN_PID must be the pid of the process
    // that ends up running `serve`, which `exec` keeps.
    let mut cmd = Command::new("sh");
    cmd.arg("-c")
        .arg(format!(
            "LISTEN_PID=$$ LISTEN_FDS=1 exec {} serve",
            env!("CARGO_BIN_EXE_meshfox")
        ))
        .env("HOME", &dir)
        .current_dir(&dir)
        .stderr(Stdio::null());
    // SAFETY: only fcntl/dup2 between the fork and the exec.
    unsafe {
        cmd.pre_exec(move || {
            if fd == 3 {
                // `dup2(3, 3)` is a no-op that keeps FD_CLOEXEC, which would
                // close the socket on `exec`; clear the flag instead.
                let flags = libc::fcntl(3, libc::F_GETFD);
                if flags < 0 || libc::fcntl(3, libc::F_SETFD, flags & !libc::FD_CLOEXEC) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
            } else if libc::dup2(fd, 3) < 0 {
                // dup2 onto a different descriptor clears FD_CLOEXEC.
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut serve = Reaper(cmd.spawn().unwrap());

    // The socket already accepts connections (we hold the listener), so
    // retry until the first request is actually answered.
    wait_for("serve to answer", || {
        cores(&dir, &socket, &["ls"]).status.success()
    });

    run_scenario(&dir, &socket);

    terminate(&mut serve);
    assert!(
        socket.exists(),
        "a socket handed over by systemd is not unlinked"
    );
    drop(listener);
    let _ = std::fs::remove_dir_all(&dir);
}
