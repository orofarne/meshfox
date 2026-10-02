//! `meshfox mcp` is one process; each open canvas is a registry entry whose
//! calls go to that canvas's worker. These tests drive a real `meshfox mcp`
//! over stdio and check what happens when the worker behind a canvas dies or
//! hangs, and when a canvas was closed after sitting idle or never opened:
//! the next call must just work, and one canvas's trouble must not reach
//! another.

use std::path::{Path, PathBuf};
use std::process::{Command as StdCommand, Stdio};
use std::time::{Duration, Instant};

use rmcp::model::CallToolRequestParams;
use rmcp::transport::{ConfigureCommandExt, TokioChildProcess};
use rmcp::{RoleClient, ServiceError, ServiceExt};
use tokio::process::Command;

const CANVAS: &str = concat!(
    "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n\nbody\n\n",
    "```bash name=\"sh\"\ntrue\n```\n",
);

fn unique_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("mf-mcp-live-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::create_dir_all(dir.join("home")).unwrap();
    std::fs::write(dir.join("doc.canvas.md"), CANVAS).unwrap();
    dir.canonicalize().unwrap()
}

type Client = rmcp::service::RunningService<RoleClient, ()>;

/// A `meshfox mcp` root rooted at `dir`, isolated from the developer's own
/// `~/.meshfox` (a `server_socket` there would point every canvas at their
/// real daemon).
async fn start(dir: &Path, idle_secs: Option<u64>) -> Client {
    match idle_secs {
        Some(secs) => {
            let secs = secs.to_string();
            start_with(
                dir,
                &[
                    ("MESHFOX_MCP_IDLE_SECS", secs.as_str()),
                    ("MESHFOX_MCP_SWEEP_SECS", "1"),
                ],
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
    ().serve(transport).await.expect("mcp root starts")
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
    let list = call(client, "canvas_list", serde_json::json!({}))
        .await
        .unwrap();
    list["canvases"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["canvas_id"].as_str().unwrap().to_string())
        .collect()
}

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

/// A standalone worker for `file`, which calls from the root's canvas will find
/// through the lock file. `--watcher-socket` makes this process the worker
/// itself; without it `view` is only the watcher and starts the worker as its
/// child, and stopping the watcher would leave the worker answering.
fn standalone_worker(dir: &Path, file: &str) -> std::process::Child {
    let worker = StdCommand::new(env!("CARGO_BIN_EXE_meshfox"))
        .args(["view", file, "--port", "0", "--no-open", "--no-auto-exit"])
        .arg("--watcher-socket")
        .arg(dir.join("fake.sock"))
        .current_dir(dir)
        .env("HOME", dir.join("home"))
        .env("MESHFOX_SERVER_SOCKET", "")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let lock = dir.join(".meshfox").join(format!("{file}.worker.lock"));
    wait_until("the worker to report its port", || {
        std::fs::read_to_string(&lock).is_ok_and(|s| s.contains("port="))
    });
    worker
}

async fn finish(client: Client, dir: &Path) {
    let _ = client.cancel().await;
    let _ = std::fs::remove_dir_all(dir);
}

/// The worker behind a canvas is killed (a crash, an OOM kill, a stray
/// `kill`): the canvas stays open, and the next call finds or starts another
/// worker by itself — no "no open canvas", no stale answer. (Without a
/// daemon, the new worker is hosted by the MCP process itself.)
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_canvas_whose_worker_died_is_served_by_a_new_one_on_the_next_call() {
    let dir = unique_dir();
    let mut worker = standalone_worker(&dir, "doc.canvas.md");
    let client = start(&dir, None).await;

    let opened = call(
        &client,
        "canvas_open",
        serde_json::json!({ "path": "doc.canvas.md" }),
    )
    .await
    .unwrap();
    let canvas_id = opened["canvas_id"].as_str().unwrap().to_string();
    assert_eq!(canvas_id, "doc.canvas.md");
    node_show(&client, &canvas_id)
        .await
        .expect("a live canvas answers");

    let first = worker.id();
    signal(first, "KILL");
    let _ = worker.wait();
    assert!(!process_exists(first));

    assert_eq!(listed(&client).await, vec!["doc.canvas.md".to_string()]);
    node_show(&client, &canvas_id)
        .await
        .expect("a new worker serves the next call");

    // `canvas_open` on it is no different.
    call(
        &client,
        "canvas_open",
        serde_json::json!({ "path": "doc.canvas.md" }),
    )
    .await
    .unwrap();

    finish(client, &dir).await;
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
    let escape = node_show(&client, "../elsewhere.canvas.md")
        .await
        .unwrap_err();
    assert!(
        escape.contains("outside") || escape.contains("no canvas file by that path"),
        "{escape}"
    );

    finish(client, &dir).await;
}

/// A canvas left alone past the idle timeout is dropped from the registry —
/// and that must cost the caller nothing: the next call reopens it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_canvas_closed_for_sitting_idle_is_reopened_by_the_next_call() {
    let dir = unique_dir();
    let client = start(&dir, Some(1)).await;
    call(
        &client,
        "canvas_open",
        serde_json::json!({ "path": "doc.canvas.md" }),
    )
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

    finish(client, &dir).await;
}

/// What a canvas holds that its worker does not is the debug shells this
/// server started; they must not outlive the canvas being closed, going idle,
/// or the host going away.
async fn start_debug_shell(client: &Client) -> (String, u32) {
    let started = call(
        client,
        "debug_start",
        serde_json::json!({ "canvas_id": "doc.canvas.md", "node_id": "root", "block_name": "sh" }),
    )
    .await
    .unwrap();
    let session_id = started["session_id"].as_str().unwrap().to_string();
    let sent = call(
        client,
        "debug_send",
        serde_json::json!({ "canvas_id": "doc.canvas.md", "session_id": session_id, "code": "echo $$" }),
    )
    .await
    .unwrap();
    let pid = sent["stdout"].as_str().unwrap().trim().parse().unwrap();
    (session_id, pid)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn closing_a_canvas_ends_its_debug_shells() {
    let dir = unique_dir();
    // The shells belong to a worker that outlives this server.
    let mut worker = standalone_worker(&dir, "doc.canvas.md");
    let client = start(&dir, None).await;
    let (_, shell) = start_debug_shell(&client).await;
    assert!(process_exists(shell));

    call(
        &client,
        "canvas_close",
        serde_json::json!({ "canvas_id": "doc.canvas.md" }),
    )
    .await
    .unwrap();
    wait_until("the debug shell to end", || !process_exists(shell));

    finish(client, &dir).await;
    let _ = worker.kill();
    let _ = worker.wait();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_idle_canvas_s_debug_shells_end_with_it() {
    let dir = unique_dir();
    // The shells belong to a worker that outlives this server.
    let mut worker = standalone_worker(&dir, "doc.canvas.md");
    let client = start(&dir, Some(1)).await;
    let (_, shell) = start_debug_shell(&client).await;

    wait_until("the idle sweep to end the debug shell", || {
        !process_exists(shell)
    });
    assert!(listed(&client).await.is_empty());

    finish(client, &dir).await;
    let _ = worker.kill();
    let _ = worker.wait();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_host_going_away_ends_every_debug_shell() {
    let dir = unique_dir();
    // The shells belong to a worker that outlives this server.
    let mut worker = standalone_worker(&dir, "doc.canvas.md");
    let client = start(&dir, None).await;
    let (_, shell) = start_debug_shell(&client).await;

    let _ = client.cancel().await;
    wait_until("the debug shell to end", || !process_exists(shell));

    let _ = std::fs::remove_dir_all(&dir);
    let _ = worker.kill();
    let _ = worker.wait();
}

// ---- A worker that is alive but not answering ----
//
// `SIGSTOP` is the model of a hang: the process exists, its socket is open, and
// it will never answer. The worker is shared with the web UI, the TUI and the
// CLI, so it is reported, never killed.

/// A call to a hung worker comes back with an honest error, not never — the
/// worker is left alone, and works again once it does.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_call_to_a_hung_worker_ends_with_an_error_and_leaves_the_worker_alone() {
    let dir = unique_dir();
    let mut worker = standalone_worker(&dir, "doc.canvas.md");
    let client = start_with(&dir, &[("MESHFOX_MCP_CALL_DEADLINE_SECS", "5")]).await;
    node_show(&client, "doc.canvas.md").await.unwrap();

    signal(worker.id(), "STOP");
    let started = Instant::now();
    let err = node_show(&client, "doc.canvas.md").await.unwrap_err();
    assert!(
        err.contains("did not answer") || err.contains("did not finish"),
        "{err}"
    );
    assert!(
        started.elapsed() < Duration::from_secs(20),
        "took {:?}",
        started.elapsed()
    );
    assert!(process_exists(worker.id()), "the shared worker was stopped");

    signal(worker.id(), "CONT");
    node_show(&client, "doc.canvas.md")
        .await
        .expect("the canvas works again once its worker does");

    let _ = worker.kill();
    let _ = worker.wait();
    finish(client, &dir).await;
}

/// One canvas with a call stuck in it must not stop `canvas_list`, or calls on
/// any other canvas, from answering: nothing there waits on a canvas's lock.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stuck_call_on_one_canvas_blocks_neither_the_list_nor_the_other_canvases() {
    let dir = unique_dir();
    std::fs::write(dir.join("other.canvas.md"), CANVAS).unwrap();
    let mut worker = standalone_worker(&dir, "doc.canvas.md");
    let client =
        std::sync::Arc::new(start_with(&dir, &[("MESHFOX_MCP_CALL_DEADLINE_SECS", "10")]).await);
    node_show(&client, "doc.canvas.md").await.unwrap();
    node_show(&client, "other.canvas.md").await.unwrap();

    signal(worker.id(), "STOP");
    let in_flight = {
        let client = std::sync::Arc::clone(&client);
        tokio::spawn(async move { node_show(&client, "doc.canvas.md").await })
    };
    tokio::time::sleep(Duration::from_millis(700)).await;

    let started = Instant::now();
    let list = call(&client, "canvas_list", serde_json::json!({}))
        .await
        .unwrap();
    assert!(
        started.elapsed() < Duration::from_secs(8),
        "canvas_list waited: {:?}",
        started.elapsed()
    );
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
    assert!(entry("doc.canvas.md")["health"]
        .as_str()
        .unwrap()
        .contains("worker is not answering"));
    assert_eq!(entry("other.canvas.md")["busy"], false);
    assert_eq!(entry("other.canvas.md")["health"], "ok");

    let started = Instant::now();
    node_show(&client, "other.canvas.md")
        .await
        .expect("the other canvas still answers");
    assert!(started.elapsed() < Duration::from_secs(4));

    // The stuck call ends, with an honest error.
    let outcome = in_flight.await.unwrap();
    assert!(outcome.is_err());

    signal(worker.id(), "CONT");
    let _ = worker.kill();
    let _ = worker.wait();
    // The spawned call has finished and let go of its share of the client.
    let client = std::sync::Arc::try_unwrap(client)
        .ok()
        .expect("no other owner left");
    finish(client, &dir).await;
}

/// Calls on one canvas run one at a time: while one is stuck, the next waits
/// for it instead of overtaking it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn calls_on_one_canvas_run_one_at_a_time() {
    let dir = unique_dir();
    let mut worker = standalone_worker(&dir, "doc.canvas.md");
    let client =
        std::sync::Arc::new(start_with(&dir, &[("MESHFOX_MCP_CALL_DEADLINE_SECS", "4")]).await);
    node_show(&client, "doc.canvas.md").await.unwrap();

    signal(worker.id(), "STOP");
    let first = {
        let client = std::sync::Arc::clone(&client);
        tokio::spawn(async move { node_show(&client, "doc.canvas.md").await })
    };
    tokio::time::sleep(Duration::from_millis(500)).await;
    let started = Instant::now();
    let second = {
        let client = std::sync::Arc::clone(&client);
        tokio::spawn(async move { node_show(&client, "doc.canvas.md").await })
    };
    first.await.unwrap().unwrap_err();
    // The second one only began when the first ended (4 s in), so it finished
    // well after one deadline from when it was sent.
    second.await.unwrap().unwrap_err();
    assert!(
        started.elapsed() > Duration::from_millis(6500),
        "the second call overtook the first: {:?}",
        started.elapsed()
    );

    signal(worker.id(), "CONT");
    let _ = worker.kill();
    let _ = worker.wait();
    let client = std::sync::Arc::try_unwrap(client)
        .ok()
        .expect("no other owner left");
    finish(client, &dir).await;
}

/// The worker is hung but the canvas stays open: `canvas_list` says so, and
/// the report clears when the worker comes back.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_hung_worker_is_reported_by_canvas_list_and_the_report_clears() {
    let dir = unique_dir();
    let mut worker = standalone_worker(&dir, "doc.canvas.md");
    let client = start(&dir, None).await;
    node_show(&client, "doc.canvas.md").await.unwrap();

    signal(worker.id(), "STOP");
    let health =
        |list: &serde_json::Value| list["canvases"][0]["health"].as_str().unwrap().to_string();
    let list = call(&client, "canvas_list", serde_json::json!({}))
        .await
        .unwrap();
    assert!(health(&list).contains("worker is not answering"), "{list}");
    assert!(
        process_exists(worker.id()),
        "the worker was stopped for its hang"
    );
    assert_eq!(listed(&client).await, vec!["doc.canvas.md".to_string()]);

    signal(worker.id(), "CONT");
    let list = call(&client, "canvas_list", serde_json::json!({}))
        .await
        .unwrap();
    assert_eq!(health(&list), "ok", "{list}");

    let _ = worker.kill();
    let _ = worker.wait();
    finish(client, &dir).await;
}
