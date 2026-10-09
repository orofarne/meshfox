//! Helpers shared by the coordinator end-to-end tests (`serve_cmd`,
//! `daemon_cmd`): one scenario every coordinator must pass, whichever
//! implementation (`meshfox serve`, the macOS daemon) is listening.

#![allow(dead_code)]

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// Short: a Unix socket path has a tight length budget (`SUN_LEN`).
pub fn unique_dir() -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("mfx-serve-it-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

pub fn meshfox(dir: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_meshfox"));
    // Never read the developer's own ~/.meshfox config.
    cmd.env("HOME", dir).current_dir(dir);
    cmd
}

pub fn write_canvas(dir: &Path) -> PathBuf {
    let path = dir.join("doc.canvas.md");
    std::fs::write(&path, "<!-- meshfox:canvas -->\n# Doc\n").unwrap();
    path.canonicalize().unwrap()
}

pub fn wait_for(what: &str, mut ok: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        if ok() {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("timed out waiting for {what}");
}

pub fn cores(dir: &Path, socket: &Path, args: &[&str]) -> Output {
    meshfox(dir)
        .env("MESHFOX_SERVER_SOCKET", socket)
        .arg("cores")
        .args(args)
        .output()
        .unwrap()
}

pub fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// One request line, one reply line — the watcher protocol by hand.
pub fn get_port(socket: &Path, canvas: &Path) -> u16 {
    let mut stream = UnixStream::connect(socket).unwrap();
    writeln!(
        stream,
        "{{\"op\":\"get_port\",\"canvas_path\":{}}}",
        serde_json::to_string(canvas).unwrap()
    )
    .unwrap();
    let mut reply = String::new();
    BufReader::new(stream).read_line(&mut reply).unwrap();
    let v: serde_json::Value = serde_json::from_str(reply.trim()).unwrap();
    v["port"]
        .as_u64()
        .unwrap_or_else(|| panic!("get_port failed: {reply}")) as u16
}

/// Kills the wrapped process when dropped, so a test that fails halfway
/// doesn't leave a coordinator running.
pub struct Reaper(pub Child);

impl std::ops::Deref for Reaper {
    type Target = Child;
    fn deref(&self) -> &Child {
        &self.0
    }
}

impl std::ops::DerefMut for Reaper {
    fn deref_mut(&mut self) -> &mut Child {
        &mut self.0
    }
}

impl Drop for Reaper {
    fn drop(&mut self) {
        // SIGTERM first: a coordinator shuts its workers down on it, a
        // SIGKILL would orphan them.
        let _ = Command::new("kill")
            .args(["-TERM", &self.0.id().to_string()])
            .status();
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline && matches!(self.0.try_wait(), Ok(None)) {
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

pub fn terminate(child: &mut Child) {
    Command::new("kill")
        .args(["-TERM", &child.id().to_string()])
        .status()
        .unwrap();
    wait_for("serve to exit", || child.try_wait().unwrap().is_some());
}

/// The scenario every coordinator must pass; `socket` is where `serve`
/// listens, however it got hold of it.
pub fn run_scenario(dir: &Path, socket: &Path) {
    let canvas = write_canvas(dir);

    let out = cores(dir, socket, &["ls"]);
    assert!(out.status.success(), "{out:?}");
    assert_eq!(stdout(&out), "no cores running\n");

    let port = get_port(socket, &canvas);
    assert_ne!(port, 0);

    let out = cores(dir, socket, &["ls"]);
    let listing = stdout(&out);
    let fields: Vec<&str> = listing.trim_end().split('\t').collect();
    assert_eq!(fields.len(), 3, "{listing:?}");
    assert_eq!(Path::new(fields[0]), canvas);
    assert_eq!(fields[1], port.to_string());
    assert!(fields[2].parse::<u32>().unwrap() > 0);

    // A second get_port reuses the same core.
    assert_eq!(get_port(socket, &canvas), port);

    let canvas_arg = canvas.to_str().unwrap();
    let out = cores(dir, socket, &["kill", canvas_arg]);
    assert!(out.status.success(), "{out:?}");
    wait_for("the core to disappear from `cores list`", || {
        stdout(&cores(dir, socket, &["ls"])) == "no cores running\n"
    });

    let out = cores(dir, socket, &["kill", canvas_arg]);
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("no such core"),
        "{out:?}"
    );
}
