//! The MCP tools that mirror the CLI's run-history and maintenance commands,
//! driven over stdio against a real `meshfox mcp`: `run` with `detach` and
//! `skip_fresh`, `logs`, `stats`, `list`, `render`, `prune`, `cores_list` and
//! `cores_kill`.

mod common;

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use common::{terminate, wait_for, Reaper};
use rmcp::model::CallToolRequestParams;
use rmcp::transport::{ConfigureCommandExt, TokioChildProcess};
use rmcp::{RoleClient, ServiceError, ServiceExt};
use tokio::process::Command;

const CANVAS: &str = concat!(
    "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
    "```bash name=\"dep\"\necho ran >> dep.txt\n```\n\n",
    "```bash name=\"target\" deps=\"dep\"\necho ran >> target.txt\necho hello-from-target\n```\n\n",
    "```bash name=\"slow\"\nsleep 2\necho detached-done > done.txt\necho detached-done\n```\n\n",
    "```starlark render name=\"total\"\nprint(\"total:\", sum([1, 2, 3]))\n```\n\n",
    "## App\n<!-- meshfox:node id=\"app\" -->\n\n",
    "```bash name=\"app\"\necho the-default-block\n```\n\n",
    "## Broken\n<!-- meshfox:node id=\"broken\" -->\n\n",
    "```starlark render name=\"bad\"\nprint(no_such_name)\n```\n",
);

fn unique_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("mf-mcp-ops-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(dir.join("home")).unwrap();
    std::fs::create_dir_all(dir.join("tmp")).unwrap();
    std::fs::write(dir.join("doc.canvas.md"), CANVAS).unwrap();
    dir.canonicalize().unwrap()
}

type Client = rmcp::service::RunningService<RoleClient, ()>;

/// `meshfox mcp` rooted at `dir`, with its own HOME and TMPDIR (so `prune`
/// never sees the developer's temp files) and `socket` as the coordinator
/// (`None`: no coordinator at all).
async fn start(dir: &Path, socket: Option<&Path>) -> Client {
    let command = Command::new(env!("CARGO_BIN_EXE_meshfox")).configure(|cmd| {
        cmd.arg("mcp")
            .current_dir(dir)
            .env("HOME", dir.join("home"))
            .env("TMPDIR", dir.join("tmp"))
            .env(
                "MESHFOX_SERVER_SOCKET",
                socket.map(Path::as_os_str).unwrap_or_default(),
            );
    });
    let transport = TokioChildProcess::new(command).unwrap();
    ().serve(transport).await.expect("mcp root starts")
}

async fn open(mcp: &Client) -> &'static str {
    call(
        mcp,
        "canvas_open",
        serde_json::json!({ "path": "doc.canvas.md" }),
    )
    .await
    .unwrap();
    "doc.canvas.md"
}

/// `(is_error, structured content)` of a call, or `Err(message)` when the
/// call itself was rejected (a protocol-level error).
async fn call(
    client: &Client,
    tool: &'static str,
    args: serde_json::Value,
) -> Result<(bool, serde_json::Value), String> {
    let serde_json::Value::Object(map) = args else {
        panic!("tool arguments must be a JSON object");
    };
    match client
        .call_tool(CallToolRequestParams::new(tool).with_arguments(map))
        .await
    {
        Ok(result) => Ok((
            result.is_error.unwrap_or(false),
            result.structured_content.unwrap_or(serde_json::Value::Null),
        )),
        Err(ServiceError::McpError(e)) => Err(e.message.to_string()),
        Err(e) => Err(e.to_string()),
    }
}

fn lines(dir: &Path, file: &str) -> usize {
    std::fs::read_to_string(dir.join(file))
        .map(|s| s.lines().count())
        .unwrap_or(0)
}

fn step<'a>(result: &'a serde_json::Value, block: &str) -> &'a serde_json::Value {
    result["steps"]
        .as_array()
        .and_then(|steps| steps.iter().find(|s| s["block"] == block))
        .unwrap_or_else(|| panic!("no step {block:?} in {result}"))
}

#[tokio::test]
async fn the_new_tools_are_listed_and_the_run_schema_has_detach_and_skip_fresh() {
    let dir = unique_dir();
    let mcp = start(&dir, None).await;
    let tools = mcp.list_all_tools().await.unwrap();
    for name in [
        "logs",
        "stats",
        "list",
        "render",
        "prune",
        "cores_list",
        "cores_kill",
    ] {
        assert!(tools.iter().any(|t| t.name == name), "no tool {name}");
    }
    let run = tools.iter().find(|t| t.name == "run").unwrap();
    let schema = serde_json::Value::Object((*run.input_schema).clone()).to_string();
    assert!(schema.contains("\"detach\"") && schema.contains("\"skip_fresh\""));
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn a_detached_run_returns_its_chain_id_at_once_and_logs_reads_the_finished_run() {
    let dir = unique_dir();
    let mcp = start(&dir, None).await;
    let id = open(&mcp).await;

    let started = Instant::now();
    let (is_error, accepted) = call(
        &mcp,
        "run",
        serde_json::json!({ "canvas_id": id, "node_id": "root", "block": "slow", "detach": true }),
    )
    .await
    .unwrap();
    assert!(!is_error, "{accepted}");
    assert_eq!(accepted["detached"], true, "{accepted}");
    let chain_id = accepted["chain_id"].as_i64().expect("a chain id");
    assert!(
        !dir.join("done.txt").exists(),
        "the call returned before the 2 s block finished (took {:?})",
        started.elapsed()
    );

    // Poll the run to its end.
    let deadline = Instant::now() + Duration::from_secs(30);
    let page = loop {
        let (_, page) = call(
            &mcp,
            "logs",
            serde_json::json!({ "canvas_id": id, "id": chain_id }),
        )
        .await
        .unwrap();
        if page["selected"]["status"] != "running" {
            break page;
        }
        assert!(Instant::now() < deadline, "run never finished: {page}");
        tokio::time::sleep(Duration::from_millis(200)).await;
    };
    assert_eq!(page["selected"]["status"], "succeeded", "{page}");
    assert_eq!(page["selected"]["id"], chain_id);
    assert!(
        page["events"].to_string().contains("detached-done"),
        "the transcript carries the step's output: {page}"
    );
    assert!(dir.join("done.txt").exists());

    // Reading on from `next_seq` finds nothing new.
    let next = page["nextSeq"].as_i64().unwrap();
    let (_, tail) = call(
        &mcp,
        "logs",
        serde_json::json!({ "canvas_id": id, "id": chain_id, "since_seq": next }),
    )
    .await
    .unwrap();
    assert_eq!(tail["events"].as_array().unwrap().len(), 0, "{tail}");
    assert_eq!(tail["more"], false);

    // Without an id: the latest chain; with `list`: the invocations.
    let (_, latest) = call(&mcp, "logs", serde_json::json!({ "canvas_id": id }))
        .await
        .unwrap();
    assert_eq!(latest["selected"]["id"], chain_id, "{latest}");
    let (_, listed) = call(
        &mcp,
        "logs",
        serde_json::json!({ "canvas_id": id, "list": true }),
    )
    .await
    .unwrap();
    assert!(
        listed["runs"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["id"] == chain_id && r["block"] == "slow"),
        "{listed}"
    );

    // The list is compact: no per-run step list, a count of what matched,
    // and `limit` keeps only the newest.
    assert!(
        listed["runs"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r.get("options").is_none()),
        "{listed}"
    );
    assert!(listed["runs_total"].as_u64().unwrap() >= 1, "{listed}");
    call(
        &mcp,
        "run",
        serde_json::json!({ "canvas_id": id, "node_id": "app" }),
    )
    .await
    .unwrap();
    let (_, newest) = call(
        &mcp,
        "logs",
        serde_json::json!({ "canvas_id": id, "list": true, "limit": 1 }),
    )
    .await
    .unwrap();
    assert_eq!(newest["runs"].as_array().unwrap().len(), 1, "{newest}");
    assert_eq!(newest["runs"][0]["block"], "app", "the newest run first");
    assert!(newest["runs_total"].as_u64().unwrap() >= 2, "{newest}");

    // An id that was never issued is an error, not an empty transcript.
    assert!(call(
        &mcp,
        "logs",
        serde_json::json!({ "canvas_id": id, "id": 987_654 })
    )
    .await
    .is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn skip_fresh_skips_an_up_to_date_block_and_conflicts_with_fresh() {
    let dir = unique_dir();
    let mcp = start(&dir, None).await;
    let id = open(&mcp).await;
    let run = |extra: serde_json::Value| {
        let mut args = serde_json::json!({ "canvas_id": id, "node_id": "root", "block": "target" });
        args.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        let mcp = &mcp;
        async move { call(mcp, "run", args).await }
    };

    let (_, first) = run(serde_json::json!({ "skip_fresh": true }))
        .await
        .unwrap();
    assert_eq!(step(&first, "target")["status"], "ran", "no record yet");
    assert_eq!(lines(&dir, "target.txt"), 1);

    let (is_error, second) = run(serde_json::json!({ "skip_fresh": true }))
        .await
        .unwrap();
    assert!(!is_error, "{second}");
    assert_eq!(step(&second, "target")["status"], "skipped", "{second}");
    assert_eq!(lines(&dir, "target.txt"), 1, "up to date: not run again");
    assert_eq!(lines(&dir, "dep.txt"), 1);

    let (_, plain) = run(serde_json::json!({})).await.unwrap();
    assert_eq!(step(&plain, "target")["status"], "ran", "{plain}");
    assert_eq!(
        lines(&dir, "target.txt"),
        2,
        "without the flag a requested block always runs"
    );

    let both = run(serde_json::json!({ "skip_fresh": true, "fresh": true })).await;
    assert!(both.unwrap_err().contains("can't be combined"));
    assert_eq!(lines(&dir, "target.txt"), 2, "nothing ran");
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn stats_summarises_the_runs_and_rejects_a_bad_since() {
    let dir = unique_dir();
    let mcp = start(&dir, None).await;
    let id = open(&mcp).await;
    call(
        &mcp,
        "run",
        serde_json::json!({ "canvas_id": id, "node_id": "root", "block": "target" }),
    )
    .await
    .unwrap();

    let (is_error, report) = call(&mcp, "stats", serde_json::json!({ "canvas_id": id }))
        .await
        .unwrap();
    assert!(!is_error, "{report}");
    assert!(report.to_string().contains("target"), "{report}");
    let blocks = |report: &serde_json::Value| report["blocks"].as_array().unwrap().clone();
    assert!(
        blocks(&report).iter().all(|b| b.get("recent").is_none()),
        "no per-block recent lists by default: {report}"
    );
    let (is_error, recent) = call(
        &mcp,
        "stats",
        serde_json::json!({
            "canvas_id": id, "since": "1h", "chain": true, "include_recent": true
        }),
    )
    .await
    .unwrap();
    assert!(
        !is_error && recent.to_string().contains("target"),
        "{recent}"
    );
    assert!(
        blocks(&recent)
            .iter()
            .any(|b| !b["recent"].as_array().unwrap().is_empty()),
        "include_recent keeps them: {recent}"
    );

    let err = call(
        &mcp,
        "stats",
        serde_json::json!({ "canvas_id": id, "since": "yesterday-ish" }),
    )
    .await
    .unwrap_err();
    assert!(err.contains("RFC3339"), "{err}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn list_names_every_runnable_block_with_its_run_arguments() {
    let dir = unique_dir();
    let mcp = start(&dir, None).await;
    let id = open(&mcp).await;
    let (is_error, listed) = call(&mcp, "list", serde_json::json!({ "canvas_id": id }))
        .await
        .unwrap();
    assert!(!is_error, "{listed}");
    let blocks = listed["blocks"].as_array().unwrap();
    let find = |name: &str| {
        blocks
            .iter()
            .find(|b| b["block"] == name)
            .unwrap_or_else(|| panic!("no block {name:?} in {listed}"))
    };
    let target = find("target");
    assert_eq!(target["node_id"], "root");
    let command = target["command"].as_str().unwrap();
    assert!(
        command.starts_with("meshfox run") && command.ends_with("target"),
        "{command}"
    );
    assert_eq!(find("app")["node_id"], "app");
    assert_eq!(find("app")["default"], true, "named like its node");
    assert_eq!(target["tty"], false);

    // What `list` names is what `run` takes.
    let (is_error, ran) = call(
        &mcp,
        "run",
        serde_json::json!({
            "canvas_id": id, "node_id": find("app")["node_id"], "block": find("app")["block"]
        }),
    )
    .await
    .unwrap();
    assert!(!is_error, "{ran}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn render_returns_each_blocks_markdown_and_reports_failures_as_data() {
    let dir = unique_dir();
    let mcp = start(&dir, None).await;
    let id = open(&mcp).await;

    let (is_error, one) = call(
        &mcp,
        "render",
        serde_json::json!({ "canvas_id": id, "node": "root", "name": "total" }),
    )
    .await
    .unwrap();
    assert!(!is_error, "{one}");
    assert_eq!(one["ok"], true);
    let blocks = one["blocks"].as_array().unwrap();
    assert_eq!(blocks.len(), 1, "{one}");
    assert!(
        blocks[0]["markdown"].as_str().unwrap().contains("total: 6"),
        "{one}"
    );
    assert_eq!(blocks[0]["error"], serde_json::Value::Null);

    // Everything: the failing block is data, and the overall flag says so.
    let (is_error, all) = call(&mcp, "render", serde_json::json!({ "canvas_id": id }))
        .await
        .unwrap();
    assert!(!is_error, "{all}");
    assert_eq!(all["ok"], false, "{all}");
    let bad = all["blocks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["node_id"] == "broken")
        .unwrap();
    assert_eq!(bad["ok"], false);
    assert!(bad["error"].as_str().unwrap().contains("no_such_name"));

    let err = call(
        &mcp,
        "render",
        serde_json::json!({ "canvas_id": id, "node": "app" }),
    )
    .await
    .unwrap_err();
    assert!(err.contains("no starlark render block"), "{err}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn prune_reports_by_default_removes_only_with_yes_and_stays_under_the_root() {
    let dir = unique_dir();
    let orphan = dir.join(".meshfox/gone.canvas.md.venv");
    std::fs::create_dir_all(&orphan).unwrap();
    std::fs::write(orphan.join("pyvenv.cfg"), "12345").unwrap();
    let mcp = start(&dir, None).await;
    let found = |report: &serde_json::Value| {
        report["candidates"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["path"].as_str().unwrap().ends_with("gone.canvas.md.venv"))
    };

    let (is_error, dry) = call(&mcp, "prune", serde_json::json!({})).await.unwrap();
    assert!(!is_error, "{dry}");
    assert_eq!(dry["applied"], false);
    assert!(found(&dry), "{dry}");
    assert!(orphan.exists(), "a dry run removes nothing");
    assert_eq!(dry["removed"].as_array().unwrap().len(), 0);

    let (is_error, done) = call(&mcp, "prune", serde_json::json!({ "yes": true }))
        .await
        .unwrap();
    assert!(!is_error, "{done}");
    assert_eq!(done["applied"], true);
    assert_eq!(done["removed"].as_array().unwrap().len(), 1, "{done}");
    assert!(!orphan.exists());
    assert!(
        dir.join("doc.canvas.md").exists(),
        "canvases are never touched"
    );

    // Nothing left; and a directory outside the root is refused.
    let (_, again) = call(&mcp, "prune", serde_json::json!({})).await.unwrap();
    assert!(!found(&again), "{again}");
    let err = call(&mcp, "prune", serde_json::json!({ "path": ".." }))
        .await
        .unwrap_err();
    assert!(err.contains("outside"), "{err}");
    let err = call(
        &mcp,
        "prune",
        serde_json::json!({ "path": "doc.canvas.md" }),
    )
    .await
    .unwrap_err();
    assert!(err.contains("isn't a directory"), "{err}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn cores_tools_fail_clearly_without_a_coordinator() {
    let dir = unique_dir();
    let mcp = start(&dir, None).await;
    let err = call(&mcp, "cores_list", serde_json::json!({}))
        .await
        .unwrap_err();
    assert!(err.contains("server_socket"), "{err}");
    let err = call(
        &mcp,
        "cores_kill",
        serde_json::json!({ "path": "doc.canvas.md" }),
    )
    .await
    .unwrap_err();
    assert!(err.contains("server_socket"), "{err}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn cores_list_and_kill_see_and_stop_the_coordinators_worker() {
    // Short: a Unix socket path has a tight length budget.
    let dir = common::unique_dir();
    std::fs::create_dir_all(dir.join("home")).unwrap();
    std::fs::create_dir_all(dir.join("tmp")).unwrap();
    std::fs::write(dir.join("doc.canvas.md"), CANVAS).unwrap();
    let dir = dir.canonicalize().unwrap();
    let socket = dir.join("s.sock");
    let mut serve = Reaper(
        common::meshfox(&dir)
            .args(["serve", "--socket"])
            .arg(&socket)
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    wait_for("the socket", || socket.exists());

    let mcp = start(&dir, Some(&socket)).await;
    let id = open(&mcp).await;
    // Any canvas call makes the coordinator spawn the canvas's core.
    let (is_error, listed) = call(&mcp, "list", serde_json::json!({ "canvas_id": id }))
        .await
        .unwrap();
    assert!(!is_error, "{listed}");

    let (is_error, cores) = call(&mcp, "cores_list", serde_json::json!({}))
        .await
        .unwrap();
    assert!(!is_error, "{cores}");
    let mine = cores["cores"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| {
            c["canvas_path"]
                .as_str()
                .unwrap()
                .ends_with("doc.canvas.md")
        })
        .unwrap_or_else(|| panic!("no core for the canvas: {cores}"));
    assert!(mine["pid"].as_u64().unwrap() > 0);

    let (is_error, killed) = call(
        &mcp,
        "cores_kill",
        serde_json::json!({ "path": "doc.canvas.md" }),
    )
    .await
    .unwrap();
    assert!(!is_error, "{killed}");
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let (_, cores) = call(&mcp, "cores_list", serde_json::json!({}))
            .await
            .unwrap();
        if !cores.to_string().contains("doc.canvas.md") {
            break;
        }
        assert!(Instant::now() < deadline, "the core never went: {cores}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // Nothing is running for it any more; and the root bounds the path.
    let err = call(
        &mcp,
        "cores_kill",
        serde_json::json!({ "path": "doc.canvas.md" }),
    )
    .await
    .unwrap_err();
    assert!(err.contains("no such core"), "{err}");
    let err = call(
        &mcp,
        "cores_kill",
        serde_json::json!({ "path": "../elsewhere.canvas.md" }),
    )
    .await
    .unwrap_err();
    assert!(err.contains("outside"), "{err}");

    drop(mcp);
    terminate(&mut serve);
    let _ = std::fs::remove_dir_all(&dir);
}
