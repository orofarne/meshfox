//! The MCP root keeps one child process per open canvas. These tests drive a
//! real `meshfox mcp` over stdio and check that the registry follows what is
//! actually alive: a canvas whose process died, or that was closed after
//! sitting idle, or that was never opened at all, is opened again by the next
//! call instead of failing with "no open canvas".

use std::path::{Path, PathBuf};
use std::process::{Command as StdCommand, Stdio};
use std::time::{Duration, Instant};

use rmcp::model::CallToolRequestParams;
use rmcp::transport::{ConfigureCommandExt, TokioChildProcess};
use rmcp::{RoleClient, ServiceError, ServiceExt};
use tokio::process::Command;

const CANVAS: &str =
    "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n\nbody\n";

fn unique_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("mf-mcp-live-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::create_dir_all(dir.join("home")).unwrap();
    std::fs::write(dir.join("doc.canvas.md"), CANVAS).unwrap();
    dir.canonicalize().unwrap()
}

/// The root MCP process under test, with its pid — tests run side by side in
/// this one process, so "my canvas process" can only be found as a child of
/// *my* root.
struct Mcp {
    client: rmcp::service::RunningService<RoleClient, ()>,
    root_pid: u32,
}

impl std::ops::Deref for Mcp {
    type Target = rmcp::service::RunningService<RoleClient, ()>;
    fn deref(&self) -> &Self::Target {
        &self.client
    }
}

type Client = Mcp;

/// A `meshfox mcp` root rooted at `dir`, isolated from the developer's own
/// `~/.meshfox` (a `server_socket` there would point every canvas at their
/// real daemon).
async fn start(dir: &Path, idle_secs: Option<u64>) -> Client {
    match idle_secs {
        Some(secs) => {
            let secs = secs.to_string();
            start_with(
                dir,
                &[("MESHFOX_MCP_IDLE_SECS", secs.as_str()), ("MESHFOX_MCP_SWEEP_SECS", "1")],
            )
            .await
        }
        None => start_with(dir, &[]).await,
    }
}

/// `start`, with extra environment for the root (the timing test hooks).
async fn start_with(dir: &Path, envs: &[(&str, &str)]) -> Client {
    let command = Command::new(env!("CARGO_BIN_EXE_meshfox")).configure(|cmd| {
        cmd.arg("mcp")
            .current_dir(dir)
            .env("HOME", dir.join("home"))
            .env("MESHFOX_SERVER_SOCKET", "");
        for (name, value) in envs {
            cmd.env(name, value);
        }
    });
    let transport = TokioChildProcess::new(command).unwrap();
    let root_pid = transport.id().expect("the root process has a pid");
    let client = ().serve(transport).await.expect("mcp root starts");
    Mcp { client, root_pid }
}

async fn call(
    client: &Client,
    tool: &'static str,
    args: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let serde_json::Value::Object(map) = args else {
        panic!("tool arguments must be a JSON object");
    };
    let request = CallToolRequestParams::new(tool).with_arguments(map);
    match client.call_tool(request).await {
        Ok(result) => Ok(result.structured_content.unwrap_or(serde_json::Value::Null)),
        Err(ServiceError::McpError(e)) => Err(e.message.to_string()),
        Err(e) => Err(e.to_string()),
    }
}

fn children_of(pid: u32) -> Vec<u32> {
    let out = StdCommand::new("pgrep")
        .args(["-P", &pid.to_string()])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.trim().parse().ok())
        .collect()
}

/// The pid of the canvas process this root spawned.
fn canvas_process(mcp: &Mcp) -> u32 {
    let leaves = children_of(mcp.root_pid);
    assert_eq!(leaves.len(), 1, "expected exactly one canvas process, got {leaves:?}");
    leaves[0]
}

fn wait_until(what: &str, mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        if condition() {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("timed out waiting for {what}");
}

async fn node_show(client: &Client, canvas_id: &str) -> Result<serde_json::Value, String> {
    call(
        client,
        "node_show",
        serde_json::json!({ "canvas_id": canvas_id, "node_id": "root" }),
    )
    .await
}

async fn listed(client: &Client) -> Vec<String> {
    let list = call(client, "canvas_list", serde_json::json!({})).await.unwrap();
    list["canvases"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["canvas_id"].as_str().unwrap().to_string())
        .collect()
}

/// The process behind an open canvas is killed (a crash, an OOM kill, a
/// stray `kill`): the registry must notice, drop it from the list, and the
/// next call must reopen the canvas by itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_canvas_whose_process_died_is_reopened_by_the_next_call() {
    let dir = unique_dir();
    let client = start(&dir, None).await;

    let opened = call(&client, "canvas_open", serde_json::json!({ "path": "doc.canvas.md" }))
        .await
        .unwrap();
    let canvas_id = opened["canvas_id"].as_str().unwrap().to_string();
    assert_eq!(canvas_id, "doc.canvas.md");
    node_show(&client, &canvas_id).await.expect("a live canvas answers");

    let first = canvas_process(&client);
    StdCommand::new("kill").args(["-9", &first.to_string()]).status().unwrap();
    wait_until("the killed canvas process to be gone", || {
        !StdCommand::new("kill")
            .args(["-0", &first.to_string()])
            .stderr(Stdio::null())
            .status()
            .unwrap()
            .success()
    });

    // Not listed as open any more: it isn't.
    assert!(listed(&client).await.is_empty());

    // And the next call just works, on a fresh process.
    node_show(&client, &canvas_id)
        .await
        .expect("the canvas is reopened on demand");
    assert_eq!(listed(&client).await, vec!["doc.canvas.md".to_string()]);
    assert_ne!(canvas_process(&client), first, "a new process took over");

    let _ = client.client.cancel().await;
    let _ = std::fs::remove_dir_all(&dir);
}

/// `canvas_open` on a canvas whose process died must not answer "already
/// open" from a stale registry entry.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn canvas_open_does_not_trust_a_dead_registry_entry() {
    let dir = unique_dir();
    let client = start(&dir, None).await;
    call(&client, "canvas_open", serde_json::json!({ "path": "doc.canvas.md" }))
        .await
        .unwrap();
    let first = canvas_process(&client);
    StdCommand::new("kill").args(["-9", &first.to_string()]).status().unwrap();
    wait_until("the killed canvas process to be gone", || {
        !StdCommand::new("kill")
            .args(["-0", &first.to_string()])
            .stderr(Stdio::null())
            .status()
            .unwrap()
            .success()
    });

    call(&client, "canvas_open", serde_json::json!({ "path": "doc.canvas.md" }))
        .await
        .unwrap();
    assert_ne!(canvas_process(&client), first, "canvas_open started a new process");
    node_show(&client, "doc.canvas.md").await.expect("and it answers");

    let _ = client.client.cancel().await;
    let _ = std::fs::remove_dir_all(&dir);
}

/// A canvas id is the file's path under the root, so a call names what to
/// open as well as what is open: no `canvas_open` needed first, and a path
/// that isn't a canvas under the root is refused with a message saying so.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_call_on_a_canvas_that_was_never_opened_opens_it() {
    let dir = unique_dir();
    let client = start(&dir, None).await;

    assert!(listed(&client).await.is_empty());
    node_show(&client, "doc.canvas.md")
        .await
        .expect("opened on demand");
    assert_eq!(listed(&client).await, vec!["doc.canvas.md".to_string()]);

    let missing = node_show(&client, "nope.canvas.md").await.unwrap_err();
    assert!(missing.contains("no canvas file by that path"), "{missing}");
    let escape = node_show(&client, "../elsewhere.canvas.md").await.unwrap_err();
    assert!(
        escape.contains("outside") || escape.contains("no canvas file by that path"),
        "{escape}"
    );

    let _ = client.client.cancel().await;
    let _ = std::fs::remove_dir_all(&dir);
}

/// A canvas left alone past the idle timeout is closed to free its process —
/// and that must cost the caller nothing: the next call reopens it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_canvas_closed_for_sitting_idle_is_reopened_by_the_next_call() {
    let dir = unique_dir();
    let client = start(&dir, Some(1)).await;
    call(&client, "canvas_open", serde_json::json!({ "path": "doc.canvas.md" }))
        .await
        .unwrap();
    assert_eq!(listed(&client).await.len(), 1);

    // Idle for longer than the timeout plus a sweep.
    tokio::time::sleep(Duration::from_secs(4)).await;
    assert!(
        listed(&client).await.is_empty(),
        "the idle sweep should have closed it"
    );

    node_show(&client, "doc.canvas.md")
        .await
        .expect("reopened on demand after the idle close");

    let _ = client.client.cancel().await;
    let _ = std::fs::remove_dir_all(&dir);
}

// ---- A process that is alive but not answering ----
//
// `SIGSTOP` is the model of a hang: the process exists, its stdio is open, and
// it will never answer — nothing the registry can see closes.

fn signal(pid: u32, name: &str) {
    StdCommand::new("kill")
        .args([&format!("-{name}"), &pid.to_string()])
        .stderr(Stdio::null())
        .status()
        .unwrap();
}

fn process_exists(pid: u32) -> bool {
    StdCommand::new("kill")
        .args(["-0", &pid.to_string()])
        .stderr(Stdio::null())
        .status()
        .unwrap()
        .success()
}

/// Pings are the only thing that can find a hung process nobody is calling:
/// after three unanswered ones it is stopped and dropped, and the next call
/// reopens the canvas.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_hung_canvas_process_is_stopped_by_pings_and_the_canvas_reopened() {
    let dir = unique_dir();
    let client = start_with(
        &dir,
        &[("MESHFOX_MCP_PING_SECS", "1"), ("MESHFOX_MCP_PING_TIMEOUT_SECS", "1")],
    )
    .await;
    call(&client, "canvas_open", serde_json::json!({ "path": "doc.canvas.md" }))
        .await
        .unwrap();
    node_show(&client, "doc.canvas.md").await.unwrap();
    let hung = canvas_process(&client);

    signal(hung, "STOP");
    assert!(process_exists(hung), "a stopped process is still there");
    wait_until("the hung process to be stopped by pings", || !process_exists(hung));

    node_show(&client, "doc.canvas.md")
        .await
        .expect("the canvas is reopened by the next call");
    assert_ne!(canvas_process(&client), hung);

    let _ = client.client.cancel().await;
    let _ = std::fs::remove_dir_all(&dir);
}

/// A call to a hung process comes back at the deadline with an honest error,
/// not never — and the process is stopped, so later calls don't queue
/// behind it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_call_the_process_does_not_answer_ends_at_the_deadline_and_stops_it() {
    let dir = unique_dir();
    let client = start_with(
        &dir,
        &[("MESHFOX_MCP_CALL_DEADLINE_SECS", "5"), ("MESHFOX_MCP_PING_SECS", "3600")],
    )
    .await;
    call(&client, "canvas_open", serde_json::json!({ "path": "doc.canvas.md" }))
        .await
        .unwrap();
    node_show(&client, "doc.canvas.md").await.unwrap();
    let hung = canvas_process(&client);
    signal(hung, "STOP");

    let started = Instant::now();
    let err = node_show(&client, "doc.canvas.md").await.unwrap_err();
    assert!(err.contains("did not answer"), "{err}");
    assert!(err.contains("may or may not have been applied"), "{err}");
    assert!(started.elapsed() < Duration::from_secs(20), "took {:?}", started.elapsed());
    wait_until("the hung process to be gone", || !process_exists(hung));

    node_show(&client, "doc.canvas.md")
        .await
        .expect("the next call reopens the canvas");

    let _ = client.client.cancel().await;
    let _ = std::fs::remove_dir_all(&dir);
}

/// One canvas with a call stuck in it must not stop `canvas_list`, or calls on
/// any other canvas, from answering: nothing there waits on a canvas's session.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stuck_call_on_one_canvas_blocks_neither_the_list_nor_the_other_canvases() {
    let dir = unique_dir();
    std::fs::write(dir.join("other.canvas.md"), CANVAS).unwrap();
    let client = std::sync::Arc::new(
        start_with(
            &dir,
            &[("MESHFOX_MCP_CALL_DEADLINE_SECS", "10"), ("MESHFOX_MCP_PING_SECS", "3600")],
        )
        .await,
    );
    call(&client, "canvas_open", serde_json::json!({ "path": "doc.canvas.md" }))
        .await
        .unwrap();
    node_show(&client, "doc.canvas.md").await.unwrap();
    let stuck = canvas_process(&client);
    call(&client, "canvas_open", serde_json::json!({ "path": "other.canvas.md" }))
        .await
        .unwrap();
    node_show(&client, "other.canvas.md").await.unwrap();

    signal(stuck, "STOP");
    let in_flight = {
        let client = std::sync::Arc::clone(&client);
        tokio::spawn(async move { node_show(&client, "doc.canvas.md").await })
    };
    tokio::time::sleep(Duration::from_millis(700)).await;

    let started = Instant::now();
    let list = call(&client, "canvas_list", serde_json::json!({})).await.unwrap();
    assert!(started.elapsed() < Duration::from_secs(4), "canvas_list waited: {:?}", started.elapsed());
    let entry = |id: &str| {
        list["canvases"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["canvas_id"] == id)
            .unwrap_or_else(|| panic!("{id} not listed: {list}"))
            .clone()
    };
    assert_eq!(entry("doc.canvas.md")["busy"], true);
    assert_eq!(entry("other.canvas.md")["busy"], false);

    let started = Instant::now();
    node_show(&client, "other.canvas.md").await.expect("the other canvas still answers");
    assert!(started.elapsed() < Duration::from_secs(4));

    // The stuck call ends at its deadline, with the honest error.
    let outcome = in_flight.await.unwrap();
    assert!(outcome.unwrap_err().contains("did not answer"));

    // The spawned call has finished and let go of its share of the client.
    let client = std::sync::Arc::try_unwrap(client).ok().expect("no other owner left");
    let _ = client.client.cancel().await;
    let _ = std::fs::remove_dir_all(&dir);
}

/// A canvas process that is fine while the worker it talks to is hung is not
/// the one to stop: stopping it would not unhang a worker that lives
/// elsewhere. The pings say so, `canvas_list` shows it, and it clears when
/// the worker comes back.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_hung_worker_is_reported_without_stopping_the_canvas_process() {
    let dir = unique_dir();
    // A standalone worker the canvas process will find through the lock file.
    // `--watcher-socket` makes this process the worker itself; without it
    // `view` is only the watcher and starts the worker as its child, and
    // stopping the watcher would leave the worker answering.
    let fake_watcher = dir.join("fake.sock");
    let mut worker = StdCommand::new(env!("CARGO_BIN_EXE_meshfox"))
        .args(["view", "doc.canvas.md", "--port", "0", "--no-open", "--no-auto-exit"])
        .arg("--watcher-socket")
        .arg(&fake_watcher)
        .current_dir(&dir)
        .env("HOME", dir.join("home"))
        .env("MESHFOX_SERVER_SOCKET", "")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let lock = dir.join(".meshfox").join("doc.canvas.md.worker.lock");
    wait_until("the worker to report its port", || {
        std::fs::read_to_string(&lock).is_ok_and(|s| s.contains("port="))
    });

    let client = start_with(&dir, &[("MESHFOX_MCP_PING_SECS", "1")]).await;
    call(&client, "canvas_open", serde_json::json!({ "path": "doc.canvas.md" }))
        .await
        .unwrap();
    node_show(&client, "doc.canvas.md").await.unwrap();
    let leaf = canvas_process(&client);

    signal(worker.id(), "STOP");
    let health = |list: &serde_json::Value| list["canvases"][0]["health"].as_str().unwrap().to_string();
    let mut reported = false;
    let mut last = serde_json::Value::Null;
    for _ in 0..100 {
        last = call(&client, "canvas_list", serde_json::json!({})).await.unwrap();
        if health(&last).contains("worker is not answering") {
            reported = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    assert!(reported, "the hung worker was never reported; last list: {last}");
    assert!(process_exists(leaf), "the canvas process was stopped for its worker's hang");
    assert_eq!(listed(&client).await, vec!["doc.canvas.md".to_string()]);

    signal(worker.id(), "CONT");
    let mut recovered = false;
    for _ in 0..100 {
        let list = call(&client, "canvas_list", serde_json::json!({})).await.unwrap();
        if health(&list) == "ok" {
            recovered = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    assert!(recovered, "the report never cleared once the worker came back");

    let _ = worker.kill();
    let _ = worker.wait();
    let _ = client.client.cancel().await;
    let _ = std::fs::remove_dir_all(&dir);
}
