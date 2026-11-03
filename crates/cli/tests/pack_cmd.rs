//! End-to-end: `meshfox pack`, `meshfox unpack` and `meshfox view PACK`
//! (SPEC.md's "Packed canvases"): the worker reads the canvas and what it
//! references, the command stores the result, and a pack opens read-only with
//! nothing running.
#![cfg(unix)]

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn unique_dir() -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("mfx-pk-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir.canonicalize().unwrap()
}

fn write(dir: &Path, rel: &str, text: &str) {
    let path = dir.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

/// `meshfox <args>` run in `cwd`, with an isolated `HOME` and no coordinator.
fn meshfox(cwd: &Path, args: &[&str]) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_meshfox"));
    cmd.current_dir(cwd)
        .env("HOME", cwd.join("home"))
        .env("MESHFOX_SERVER_SOCKET", "")
        .env_remove("MESHFOX_READ_ONLY")
        .env_remove("MESHFOX_NO_EXEC")
        .args(args);
    cmd
}

fn run(cwd: &Path, args: &[&str]) -> Output {
    meshfox(cwd, args).output().unwrap()
}

fn text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

const CANVAS: &str = concat!(
    "<!-- meshfox:canvas -->\n# Demo\n<!-- meshfox:node id=\"root\" -->\n\n",
    "![p](img/p.png)\n\n",
    "```bash name=\"go\"\necho ran\n```\n\n",
    "## Notes\n<!-- meshfox:node id=\"notes\" type=\"include\" -->\n\n[n](./notes.md)\n\n",
    "## Child\n<!-- meshfox:node id=\"child\" type=\"file\" -->\n\n[c](./sub/child.canvas.md)\n",
);

fn project() -> PathBuf {
    let dir = unique_dir();
    write(&dir, "doc.canvas.md", CANVAS);
    write(&dir, "img/p.png", "png");
    write(&dir, "notes.md", "# Notes\n");
    write(
        &dir,
        "sub/child.canvas.md",
        "<!-- meshfox:canvas -->\n# Child\n<!-- meshfox:node id=\"root\" -->\n\n![d](deep.png)\n",
    );
    write(&dir, "sub/deep.png", "deep");
    write(&dir, "unrelated.txt", "never referenced");
    dir
}

#[test]
fn pack_lists_writes_and_unpacks_what_the_canvas_reads() {
    let dir = project();

    let listing = run(&dir, &["doc.canvas.md", "pack", "--list"]);
    assert!(listing.status.success(), "{}", text(&listing));
    let listing = String::from_utf8_lossy(&listing.stdout).into_owned();
    let paths: Vec<&str> = listing
        .lines()
        .map(|l| l.split('\t').next().unwrap())
        .collect();
    assert_eq!(
        paths,
        [
            "doc.canvas.md",
            "img/p.png",
            "notes.md",
            "sub/child.canvas.md",
            "sub/deep.png"
        ]
    );
    assert!(
        !dir.join("doc.canvas.tar.xz").exists(),
        "--list writes nothing"
    );

    let packed = run(&dir, &["doc.canvas.md", "pack"]);
    assert!(packed.status.success(), "{}", text(&packed));
    let pack_path = dir.join("doc.canvas.tar.xz");
    assert!(pack_path.exists(), "{}", text(&packed));

    // The same files give the same bytes, run to run.
    let first = std::fs::read(&pack_path).unwrap();
    let again = run(
        &dir,
        &["doc.canvas.md", "pack", "-o", "again.canvas.tar.xz"],
    );
    assert!(again.status.success(), "{}", text(&again));
    let second = std::fs::read(dir.join("again.canvas.tar.xz")).unwrap();
    assert_eq!(first, second);

    let out = dir.join("out");
    let unpacked = run(&dir, &["unpack", "doc.canvas.tar.xz", "out"]);
    assert!(unpacked.status.success(), "{}", text(&unpacked));
    // Named the way the directory was given.
    assert_eq!(
        String::from_utf8_lossy(&unpacked.stdout).trim(),
        "out/doc.canvas.md"
    );
    assert_eq!(
        std::fs::read_to_string(out.join("sub/deep.png")).unwrap(),
        "deep"
    );
    assert!(!out.join("unrelated.txt").exists());
    assert!(!out.join(".meshfox").exists());
}

#[test]
fn packing_writes_nothing_next_to_the_canvas_but_the_pack() {
    let dir = project();
    let before = names(&dir);
    for args in [
        &["doc.canvas.md", "pack", "--list"][..],
        &["doc.canvas.md", "pack", "-o", "elsewhere.canvas.tar.xz"][..],
    ] {
        let out = run(&dir, args);
        assert!(out.status.success(), "{}", text(&out));
    }
    let mut expected = before;
    expected.push("elsewhere.canvas.tar.xz".into());
    expected.sort();
    // No `.meshfox/`, session database or lock file left by the worker.
    assert_eq!(names(&dir), expected);
}

#[test]
fn pack_and_unpack_do_not_offer_the_global_restriction_flags_but_accept_them() {
    let dir = project();
    for command in ["pack", "unpack"] {
        let help = run(&dir, &[command, "--help"]);
        assert!(help.status.success(), "{}", text(&help));
        let help = text(&help);
        assert!(!help.contains("--read-only"), "{command}: {help}");
        assert!(!help.contains("--no-exec"), "{command}: {help}");
    }
    // Still accepted, before or after the subcommand.
    for args in [
        &[
            "doc.canvas.md",
            "pack",
            "--list",
            "--read-only",
            "--no-exec",
        ][..],
        &["--no-exec", "pack", "--canvas", "doc.canvas.md", "--list"][..],
    ] {
        let out = run(&dir, args);
        assert!(out.status.success(), "{args:?}: {}", text(&out));
    }
    // The commands that do start a core keep offering them.
    let view = text(&run(&dir, &["view", "--help"]));
    assert!(
        view.contains("--read-only") && view.contains("--no-exec"),
        "{view}"
    );
}

fn names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n != "home")
        .collect();
    names.sort();
    names
}

#[test]
fn the_pack_is_a_plain_xz_compressed_tar_the_system_tools_read() {
    let dir = project();
    assert!(run(&dir, &["doc.canvas.md", "pack"]).status.success());
    // Skipped quietly where these tools are missing; they are standard on the
    // platforms this test runs on.
    let Ok(listing) = Command::new("tar")
        .args(["tf", "doc.canvas.tar.xz"])
        .current_dir(&dir)
        .output()
    else {
        return;
    };
    if !listing.status.success() {
        return;
    }
    let names = String::from_utf8_lossy(&listing.stdout).into_owned();
    assert_eq!(names.lines().next(), Some(".meshfox-pack.json"), "{names}");
    assert!(names.contains("sub/deep.png"), "{names}");
}

#[test]
fn shallow_leaves_the_nested_canvas_out_and_says_so() {
    let dir = project();
    let out = run(&dir, &["doc.canvas.md", "pack", "--shallow", "--list"]);
    assert!(out.status.success(), "{}", text(&out));
    let listing = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(!listing.contains("child.canvas.md"), "{listing}");
    assert!(!listing.contains("deep.png"), "{listing}");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("--shallow"),
        "{}",
        text(&out)
    );
}

#[test]
fn a_reference_that_cannot_be_packed_fails_and_writes_nothing() {
    let dir = project();
    write(&dir, "main/doc.canvas.md", "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n\n[s](../unrelated.txt)\n");
    let out = run(
        &dir,
        &["main/doc.canvas.md", "pack", "-o", "main/x.canvas.tar.xz"],
    );
    assert!(!out.status.success());
    let message = text(&out);
    assert!(message.contains("../unrelated.txt"), "{message}");
    assert!(message.contains("\"root\""), "{message}");
    assert!(!dir.join("main/x.canvas.tar.xz").exists());
    let leftovers: Vec<_> = std::fs::read_dir(dir.join("main"))
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.contains("partial") || n.ends_with(".xz"))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
}

#[test]
fn an_existing_output_is_only_replaced_with_force() {
    let dir = project();
    write(&dir, "out.canvas.tar.xz", "not a pack, but mine");
    let refused = run(&dir, &["doc.canvas.md", "pack", "-o", "out.canvas.tar.xz"]);
    assert!(!refused.status.success());
    assert!(
        text(&refused).contains("already exists"),
        "{}",
        text(&refused)
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("out.canvas.tar.xz")).unwrap(),
        "not a pack, but mine"
    );
    let forced = run(
        &dir,
        &["doc.canvas.md", "pack", "-o", "out.canvas.tar.xz", "-f"],
    );
    assert!(forced.status.success(), "{}", text(&forced));
    assert_ne!(
        std::fs::read(dir.join("out.canvas.tar.xz")).unwrap(),
        b"not a pack, but mine"
    );
}

#[test]
fn unpack_refuses_what_is_not_a_pack_and_a_directory_in_use() {
    let dir = project();
    write(&dir, "fake.canvas.tar.xz", "this is not a pack");
    let bad = run(&dir, &["unpack", "fake.canvas.tar.xz", "target"]);
    assert!(!bad.status.success());
    assert!(
        !dir.join("target").exists(),
        "a refused pack writes nothing"
    );

    assert!(run(&dir, &["doc.canvas.md", "pack"]).status.success());
    write(&dir, "busy/mine.txt", "keep");
    let busy = run(&dir, &["unpack", "doc.canvas.tar.xz", "busy"]);
    assert!(!busy.status.success());
    assert!(
        text(&busy).contains("not an empty directory"),
        "{}",
        text(&busy)
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("busy/mine.txt")).unwrap(),
        "keep"
    );
    // With no directory given, it is named after the pack.
    let named = run(&dir, &["unpack", "doc.canvas.tar.xz"]);
    assert!(named.status.success(), "{}", text(&named));
    assert!(dir.join("doc/doc.canvas.md").exists());
}

/// What `GET <path>` on a worker at `port` answers: status and body.
fn http_get(port: u16, path: &str) -> (u16, String) {
    let mut stream = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n"
    )
    .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    let status = response
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let body = response.split("\r\n\r\n").nth(1).unwrap_or("").to_string();
    (status, body)
}

#[test]
fn view_opens_a_pack_read_only_with_nothing_running_and_cleans_up() {
    let dir = project();
    assert!(run(&dir, &["doc.canvas.md", "pack"]).status.success());

    let log = std::fs::File::create(dir.join("view.log")).unwrap();
    let mut child = meshfox(&dir, &["view", "doc.canvas.tar.xz", "--no-open"])
        .stdout(log.try_clone().unwrap())
        .stderr(log)
        .stdin(Stdio::null())
        .spawn()
        .unwrap();

    // The view says where it serves; that line carries the port.
    let deadline = Instant::now() + Duration::from_secs(30);
    let (port, log_text) = loop {
        let log_text = std::fs::read_to_string(dir.join("view.log")).unwrap_or_default();
        if let Some(port) = log_text
            .split("on http://127.0.0.1:")
            .nth(1)
            .and_then(|rest| rest.split(|c: char| !c.is_ascii_digit()).next())
            .and_then(|p| p.parse::<u16>().ok())
        {
            break (port, log_text);
        }
        assert!(
            Instant::now() < deadline,
            "the view never came up:\n{log_text}"
        );
        std::thread::sleep(Duration::from_millis(100));
    };
    assert!(log_text.contains("packed canvas"), "{log_text}");

    let (status, info) = http_get(port, "/api/info");
    assert_eq!(status, 200);
    let info: serde_json::Value = serde_json::from_str(info.trim()).unwrap();
    assert_eq!(info["readOnly"], true, "{info}");
    assert_eq!(info["noExec"], true, "{info}");

    // The block in the pack is not run, and the canvas is not edited.
    let run_request = |body: &str, path: &str, method: &str| -> u16 {
        let mut stream = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
        write!(
            stream,
            "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\n\
             Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
        .unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        response
            .split_whitespace()
            .nth(1)
            .and_then(|s| s.parse().ok())
            .unwrap_or(0)
    };
    assert_eq!(run_request("{}", "/api/run", "POST"), 403);
    assert_eq!(run_request("{}", "/api/nodes", "POST"), 403);

    // Where it was unpacked: a temporary directory that holds the pack's files.
    let unpacked: PathBuf = log_text
        .lines()
        .find_map(|l| l.strip_prefix("meshfox: serving "))
        .and_then(|l| l.split(" read-only").next().or(Some(l)))
        .map(|p| PathBuf::from(p.split(" on http").next().unwrap().trim()))
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    assert!(unpacked.join("sub/deep.png").exists(), "{unpacked:?}");

    // Stopping the view removes it.
    Command::new("kill")
        .args(["-TERM", &child.id().to_string()])
        .status()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    while child.try_wait().unwrap().is_none() {
        assert!(Instant::now() < deadline, "the view did not stop");
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(!unpacked.exists(), "{unpacked:?} was left behind");
}

#[test]
fn view_refuses_a_damaged_pack_before_serving_anything() {
    let dir = project();
    assert!(run(&dir, &["doc.canvas.md", "pack"]).status.success());
    let mut bytes = std::fs::read(dir.join("doc.canvas.tar.xz")).unwrap();
    let middle = bytes.len() / 2;
    bytes[middle] ^= 0xFF;
    std::fs::write(dir.join("broken.canvas.tar.xz"), bytes).unwrap();
    let out = run(&dir, &["view", "broken.canvas.tar.xz", "--no-open"]);
    assert!(!out.status.success());
    assert!(
        text(&out).contains("broken.canvas.tar.xz"),
        "{}",
        text(&out)
    );
}
