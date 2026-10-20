use std::{
    path::PathBuf,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

struct Process(Child);
impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn setup() -> (PathBuf, PathBuf) {
    let dir = std::env::temp_dir().join(format!("meshfox-console-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let canvas = dir.join("test.canvas.md");
    std::fs::write(&canvas, "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n\n```sh name=build\necho before-attach\nsleep 3\necho after-attach\nsleep 2\necho survived > survived.txt\n```\n").unwrap();
    (dir, canvas)
}
fn command(dir: &std::path::Path) -> Command {
    #[cfg(target_os = "macos")]
    {
        static SIGN: std::sync::Once = std::sync::Once::new();
        SIGN.call_once(|| {
            assert!(Command::new(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../scripts/run-signed.sh"
            ))
            .args(["--sign-only", env!("CARGO_BIN_EXE_meshfox")])
            .status()
            .unwrap()
            .success());
        });
    }
    let mut command = Command::new(env!("CARGO_BIN_EXE_meshfox"));
    command.env("HOME", dir).current_dir(dir);
    command
}
fn wait_for(mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(15);
    while !condition() {
        assert!(Instant::now() < deadline, "timed out");
        std::thread::sleep(Duration::from_millis(50));
    }
}
#[test]
fn absent_core_does_not_create_discovery_files() {
    let (dir, canvas) = setup();
    let output = command(&dir).arg("console").arg(&canvas).output().unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("no core running"));
    assert!(!dir.join(".meshfox").exists());
    std::fs::remove_dir_all(dir).unwrap();
}
#[test]
fn attach_replays_tails_and_detaches_without_stopping_run() {
    let (dir, canvas) = setup();
    let _worker = Process(
        command(&dir)
            .arg("view")
            .arg(&canvas)
            .args(["--port", "0", "--watcher-socket"])
            .arg(dir.join("fake.sock"))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let lock = dir.join(".meshfox/test.canvas.md.worker.lock");
    wait_for(|| std::fs::read_to_string(&lock).is_ok_and(|s| s.contains("port=")));
    let run_log = dir.join("run.log");
    let mut run = Process(
        command(&dir)
            .arg(&canvas)
            .args(["run", "build"])
            .stdout(std::fs::File::create(&run_log).unwrap())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    wait_for(|| std::fs::read_to_string(&run_log).is_ok_and(|s| s.contains("before-attach")));
    let console_log = dir.join("console.log");
    let mut console = Process(
        command(&dir)
            .arg(&canvas)
            .arg("console")
            .stdout(std::fs::File::create(&console_log).unwrap())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    wait_for(|| std::fs::read_to_string(&console_log).is_ok_and(|s| s.contains("after-attach")));
    let log = std::fs::read_to_string(&console_log).unwrap();
    assert_eq!(log.matches("before-attach").count(), 1);
    assert_eq!(log.matches("after-attach").count(), 1);
    // SAFETY: signal only our owned child process.
    unsafe {
        libc::kill(console.0.id() as i32, libc::SIGINT);
    }
    wait_for(|| console.0.try_wait().unwrap().is_some());
    assert!(run.0.wait().unwrap().success());
    assert!(dir.join("survived.txt").exists());
}

#[test]
fn configured_coordinator_is_only_asked_to_list_existing_cores() {
    use std::io::{BufRead, BufReader, Write};
    let (dir, canvas) = setup();
    let socket = PathBuf::from("/tmp").join(format!("mfx-console-{}.sock", uuid::Uuid::new_v4()));
    let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    listener.set_nonblocking(true).unwrap();
    let mut console = Process(
        command(&dir)
            .arg("console")
            .arg(&canvas)
            .env("MESHFOX_SERVER_SOCKET", &socket)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let mut connection = None;
    wait_for(|| {
        connection = listener.accept().ok();
        connection.is_some()
    });
    let (mut stream, _) = connection.unwrap();
    // A socket accepted from a non-blocking listener is non-blocking too on
    // macOS/BSD (Linux resets it), which would turn the read below into an
    // immediate `WouldBlock` instead of waiting for the console's request.
    stream.set_nonblocking(false).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut request = String::new();
    BufReader::new(stream.try_clone().unwrap())
        .read_line(&mut request)
        .unwrap();
    let message: meshfox_server::watcher_protocol::Message =
        serde_json::from_str(&request).unwrap();
    assert!(matches!(
        message,
        meshfox_server::watcher_protocol::Message::ListCores
    ));
    stream.write_all(b"{\"cores\":[]}\n").unwrap();
    wait_for(|| console.0.try_wait().unwrap().is_some());
    assert!(!console.0.wait().unwrap().success());
    assert!(!dir.join(".meshfox").exists());
    std::fs::remove_file(socket).unwrap();
}
