//! `meshfox serve --http` end to end, against real processes: the gateway in
//! front of real workers, its token handling, what it prints for a canvas
//! "opened" through the coordinator, and that it keeps working when a core
//! goes away underneath it.
//!
//! Plain HTTP/1.0 over a `TcpStream` (the reply is not chunked, so the body
//! is simply what follows the headers).

mod common;

use common::*;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::Stdio;

struct Gateway {
    _serve: Reaper,
    port: u16,
    socket: PathBuf,
    stderr: PathBuf,
}

/// Starts `serve --socket … --http 127.0.0.1:0 --root <dir>` plus `extra`
/// args and env, and waits for the line that names the port it got.
fn start(dir: &Path, addr: &str, extra: &[&str], env: &[(&str, &str)]) -> Gateway {
    let socket = dir.join("s.sock");
    let stderr = dir.join("serve.err");
    let mut cmd = meshfox(dir);
    cmd.args(["serve", "--socket"])
        .arg(&socket)
        .args(["--http", addr, "--root"])
        .arg(dir)
        .args(extra)
        .env_remove("MESHFOX_TOKEN")
        .stderr(Stdio::from(std::fs::File::create(&stderr).unwrap()));
    for (k, v) in env {
        cmd.env(k, v);
    }
    let serve = Reaper(cmd.spawn().unwrap());
    let mut port = 0;
    wait_for("the gateway to report its port", || {
        let log = std::fs::read_to_string(&stderr).unwrap_or_default();
        port = log
            .lines()
            .find_map(|l| l.split("http on http://").nth(1))
            .and_then(|rest| rest.split('/').next())
            .and_then(|hostport| hostport.rsplit(':').next())
            .and_then(|p| p.parse().ok())
            .unwrap_or(0);
        port != 0
    });
    Gateway {
        _serve: serve,
        port,
        socket,
        stderr,
    }
}

impl Gateway {
    fn get(&self, path: &str, bearer: Option<&str>) -> (u16, String) {
        let mut request = format!("GET {path} HTTP/1.0\r\nHost: 127.0.0.1:{}\r\n", self.port);
        if let Some(token) = bearer {
            request.push_str(&format!("Authorization: Bearer {token}\r\n"));
        }
        request.push_str("\r\n");
        let mut stream = TcpStream::connect(("127.0.0.1", self.port)).unwrap();
        stream.write_all(request.as_bytes()).unwrap();
        let mut raw = String::new();
        stream.read_to_string(&mut raw).unwrap();
        let (head, body) = raw.split_once("\r\n\r\n").unwrap_or((&raw, ""));
        let status = head
            .lines()
            .next()
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        (status, body.to_string())
    }

    fn slug_of(&self, path: &str, bearer: Option<&str>) -> String {
        let (status, body) = self.get("/_/index.json", bearer);
        assert_eq!(status, 200, "{body}");
        let index: serde_json::Value = serde_json::from_str(&body).unwrap();
        index["canvases"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["path"] == path)
            .unwrap_or_else(|| panic!("{path} not in {body}"))["slug"]
            .as_str()
            .unwrap()
            .to_string()
    }

    fn log(&self) -> String {
        std::fs::read_to_string(&self.stderr).unwrap_or_default()
    }

    fn cores(&self, dir: &Path, args: &[&str]) -> std::process::Output {
        cores(dir, &self.socket, args)
    }
}

/// The pid column of the single line `cores ls` prints.
fn core_pid(listing: &str) -> u32 {
    listing
        .trim_end()
        .split('\t')
        .nth(2)
        .unwrap_or_else(|| panic!("no core in {listing:?}"))
        .parse()
        .unwrap()
}

#[test]
fn the_gateway_serves_a_real_core_and_starts_a_new_one_after_it_is_killed() {
    let dir = unique_dir();
    let canvas = write_canvas(&dir);
    let gw = start(&dir, "127.0.0.1:0", &[], &[]);
    let slug = gw.slug_of("doc.canvas.md", None);

    // Nothing runs until somebody asks for the canvas.
    assert_eq!(stdout(&gw.cores(&dir, &["ls"])), "no cores running\n");

    let (status, body) = gw.get(&format!("/c/{slug}/api/info"), None);
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("\"remote\":true"), "{body}");
    let first = core_pid(&stdout(&gw.cores(&dir, &["ls"])));

    // The page itself, with relative assets.
    let (status, page) = gw.get(&format!("/c/{slug}/"), None);
    assert_eq!(status, 200);
    assert!(page.contains("./assets/"), "{page}");

    // The index marks it running.
    let (_, index) = gw.get("/_/index.json", None);
    assert!(index.contains("\"running\":true"), "{index}");

    // Kill the core underneath the gateway: the next request starts a new one.
    let out = gw.cores(&dir, &["kill", canvas.to_str().unwrap()]);
    assert!(out.status.success(), "{out:?}");
    wait_for("the core to go", || {
        stdout(&gw.cores(&dir, &["ls"])) == "no cores running\n"
    });
    let (status, body) = gw.get(&format!("/c/{slug}/api/info"), None);
    assert_eq!(status, 200, "{body}");
    let second = core_pid(&stdout(&gw.cores(&dir, &["ls"])));
    assert_ne!(first, second, "a new core, not the old one");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_plain_md_that_opens_with_the_marker_is_listed_and_served() {
    let dir = unique_dir();
    write_canvas(&dir);
    // What meshfox itself treats as a canvas: a README.md that starts with the
    // marker. A plain notes file is not. Nothing is skipped by name: what is
    // left out is what `.meshfoxignore` says (here `node_modules/`, anywhere).
    std::fs::write(dir.join("README.md"), "<!-- meshfox:canvas -->\n# Readme\n").unwrap();
    std::fs::write(dir.join("notes.md"), "# just notes\n").unwrap();
    for pkg in ["node_modules/pkg", "target/doc"] {
        std::fs::create_dir_all(dir.join(pkg)).unwrap();
        std::fs::write(dir.join(pkg).join("README.md"), "<!-- meshfox:canvas -->\n").unwrap();
    }
    std::fs::write(dir.join(".meshfoxignore"), "# not served\nnode_modules/\n").unwrap();
    let gw = start(&dir, "127.0.0.1:0", &[], &[]);

    let (_, index) = gw.get("/_/index.json", None);
    assert!(index.contains("\"path\":\"README.md\""), "{index}");
    assert!(!index.contains("notes.md"), "{index}");
    assert!(!index.contains("node_modules"), "{index}");
    // `target` is a perfectly good directory name: not guessed at.
    assert!(index.contains("target/doc/README.md"), "{index}");

    let slug = gw.slug_of("README.md", None);
    assert!(slug.starts_with("readme-"), "{slug}");
    let (status, body) = gw.get(&format!("/c/{slug}/api/info"), None);
    assert_eq!(status, 200, "{body}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_token_from_the_environment_or_a_file_is_required() {
    let dir = unique_dir();
    write_canvas(&dir);

    let env = start(&dir, "127.0.0.1:0", &[], &[("MESHFOX_TOKEN", "from-env")]);
    assert_eq!(env.get("/_/index.json", None).0, 401);
    assert_eq!(env.get("/_/index.json", Some("wrong")).0, 401);
    assert_eq!(env.get("/_/index.json", Some("from-env")).0, 200);
    assert_eq!(env.get("/_/ping", None).0, 200, "ping needs no token");
    drop(env);

    let file = dir.join("token");
    std::fs::write(&file, "from-file\nignored second line\n").unwrap();
    let from_file = start(
        &dir,
        "127.0.0.1:0",
        &["--token-file", file.to_str().unwrap()],
        &[],
    );
    assert_eq!(from_file.get("/_/index.json", Some("from-file")).0, 200);
    assert_eq!(from_file.get("/_/index.json", Some("from-env")).0, 401);
    drop(from_file);

    // The environment wins over the file.
    let both = start(
        &dir,
        "127.0.0.1:0",
        &["--token-file", file.to_str().unwrap()],
        &[("MESHFOX_TOKEN", "from-env")],
    );
    assert_eq!(both.get("/_/index.json", Some("from-env")).0, 200);
    assert_eq!(both.get("/_/index.json", Some("from-file")).0, 401);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_reachable_address_without_a_token_generates_one_and_says_so() {
    let dir = unique_dir();
    write_canvas(&dir);
    let gw = start(&dir, "0.0.0.0:0", &[], &[]);
    assert_eq!(gw.get("/_/index.json", None).0, 401);
    let log = gw.log();
    let token = log
        .lines()
        .skip_while(|l| !l.contains("generated access token"))
        .nth(1)
        .unwrap_or_else(|| panic!("no token in {log:?}"))
        .trim()
        .to_string();
    assert!(token.len() >= 64, "{token:?}");
    assert_eq!(gw.get("/_/index.json", Some(&token)).0, 200);
    // A loopback address stays open: nothing was generated for the other tests' sake.
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn loopback_without_a_token_is_open_and_prints_no_token() {
    let dir = unique_dir();
    write_canvas(&dir);
    let gw = start(&dir, "127.0.0.1:0", &[], &[]);
    assert_eq!(gw.get("/_/index.json", None).0, 200);
    assert!(!gw.log().contains("generated access token"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn bad_http_options_fail_the_whole_start_with_a_reason() {
    let dir = unique_dir();
    write_canvas(&dir);
    let socket = dir.join("bad.sock");
    let run = |args: &[&str]| {
        meshfox(&dir)
            .env_remove("MESHFOX_TOKEN")
            .args(["serve", "--socket"])
            .arg(&socket)
            .args(args)
            .output()
            .unwrap()
    };

    let out = run(&[
        "--http",
        "127.0.0.1:0",
        "--root",
        "/nonexistent/served/root",
    ]);
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("--root"),
        "{out:?}"
    );

    let empty = dir.join("empty-token");
    std::fs::write(&empty, "\n").unwrap();
    let out = run(&[
        "--http",
        "127.0.0.1:0",
        "--token-file",
        empty.to_str().unwrap(),
    ]);
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("no token"),
        "{out:?}"
    );

    let out = run(&[
        "--http",
        "127.0.0.1:0",
        "--token-file",
        "/nonexistent/token",
    ]);
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("cannot read"),
        "{out:?}"
    );

    // A port that is taken.
    let taken = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = taken.local_addr().unwrap().to_string();
    let out = run(&["--http", &addr]);
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("--http"),
        "{out:?}"
    );

    // The flags that only make sense with --http are refused without it.
    let out = run(&["--root", "/tmp"]);
    assert!(!out.status.success());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn opening_a_canvas_through_the_coordinator_prints_its_gateway_address() {
    let dir = unique_dir();
    let canvas = write_canvas(&dir);
    // A wildcard bind: there is no browser here to open, so the address is
    // printed, as the published port would be reached.
    let gw = start(
        &dir,
        "0.0.0.0:0",
        &["--public-url", "https://notes.example/"],
        &[("MESHFOX_TOKEN", "t")],
    );
    let slug = gw.slug_of("doc.canvas.md", Some("t"));
    let out = gw.cores(&dir, &["open", canvas.to_str().unwrap()]);
    assert!(out.status.success(), "{out:?}");
    wait_for("the address to be printed", || {
        gw.log()
            .contains(&format!("open https://notes.example/c/{slug}/"))
    });
    let log = gw.log();
    assert!(!log.contains("open http://127.0.0.1"), "{log}");
    assert!(!log.contains("couldn't open a browser"), "{log}");
    let _ = std::fs::remove_dir_all(&dir);
}
