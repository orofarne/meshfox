//! The MCP `run` and `session_reset` tools, driven over stdio against a real
//! `meshfox mcp`: a chain runs and reports every step; a dependency that
//! already ran is skipped until `fresh` (one call) or `session_reset` (all);
//! failures, missing variables, unknown nodes and timeouts come back as
//! errors the agent can act on.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use rmcp::model::CallToolRequestParams;
use rmcp::transport::{ConfigureCommandExt, TokioChildProcess};
use rmcp::{RoleClient, ServiceError, ServiceExt};
use tokio::process::Command;

const CANVAS: &str = concat!(
    "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
    "<!-- meshfox:var name=\"WHO\" prompt=\"who\" -->\n\n",
    "```bash name=\"dep\"\necho ran >> count.txt\n```\n\n",
    "```bash name=\"target\" deps=\"dep\"\necho hello-from-target\n```\n\n",
    "```bash name=\"bad\"\necho nope >&2\nexit 3\n```\n\n",
    "```bash name=\"needs-who\" env=\"WHO\"\necho \"hi $WHO\"\n```\n\n",
    "```bash name=\"slow\"\nsleep 30\n```\n\n",
    "## App\n<!-- meshfox:node id=\"app\" -->\n\n",
    "```bash name=\"app\"\necho the-default-block\n```\n",
);

fn unique_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("mf-mcp-run-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(dir.join("home")).unwrap();
    std::fs::write(dir.join("doc.canvas.md"), CANVAS).unwrap();
    dir.canonicalize().unwrap()
}

type Client = rmcp::service::RunningService<RoleClient, ()>;

async fn start(dir: &Path) -> Client {
    let command = Command::new(env!("CARGO_BIN_EXE_meshfox")).configure(|cmd| {
        cmd.arg("mcp")
            .current_dir(dir)
            .env("HOME", dir.join("home"))
            .env("MESHFOX_SERVER_SOCKET", "");
    });
    let transport = TokioChildProcess::new(command).unwrap();
    ().serve(transport).await.expect("mcp root starts")
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

fn runs(dir: &Path) -> usize {
    std::fs::read_to_string(dir.join("count.txt"))
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
async fn run_reports_the_chain_skips_fresh_dependencies_and_fresh_and_reset_force_them() {
    let dir = unique_dir();
    let mcp = start(&dir).await;
    call(&mcp, "canvas_open", serde_json::json!({ "path": "doc.canvas.md" }))
        .await
        .unwrap();
    let id = "doc.canvas.md";
    let run = |extra: serde_json::Value| {
        let mut args = serde_json::json!({ "canvas_id": id, "node_id": "root", "block": "target" });
        args.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        let mcp = &mcp;
        async move { call(mcp, "run", args).await }
    };

    let (is_error, first) = run(serde_json::json!({})).await.unwrap();
    assert!(!is_error, "{first}");
    assert_eq!(first["success"], true, "{first}");
    assert_eq!(step(&first, "dep")["status"], "ran");
    assert_eq!(step(&first, "target")["status"], "ran");
    assert!(step(&first, "target")["output"]
        .as_str()
        .unwrap()
        .contains("hello-from-target"));
    assert_eq!(runs(&dir), 1);

    let (_, second) = run(serde_json::json!({})).await.unwrap();
    assert_eq!(step(&second, "dep")["status"], "skipped", "{second}");
    assert_eq!(runs(&dir), 1, "an already-fresh dependency is not run again");

    let (_, fresh) = run(serde_json::json!({ "fresh": true })).await.unwrap();
    assert_eq!(step(&fresh, "dep")["status"], "ran", "{fresh}");
    assert_eq!(runs(&dir), 2);

    let (_, after) = run(serde_json::json!({})).await.unwrap();
    assert_eq!(step(&after, "dep")["status"], "skipped", "fresh forgets nothing: {after}");

    let (is_error, reset) = call(&mcp, "session_reset", serde_json::json!({ "canvas_id": id }))
        .await
        .unwrap();
    assert!(!is_error && reset["reset"] == true, "{reset}");
    let (_, again) = run(serde_json::json!({})).await.unwrap();
    assert_eq!(step(&again, "dep")["status"], "ran", "{again}");
    assert_eq!(runs(&dir), 3);

    let both = run(serde_json::json!({ "fresh": true, "no_deps": true })).await;
    assert!(both.unwrap_err().contains("can't be combined"));

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn run_reports_failures_missing_variables_unknown_nodes_defaults_and_timeouts() {
    let dir = unique_dir();
    let mcp = start(&dir).await;
    call(&mcp, "canvas_open", serde_json::json!({ "path": "doc.canvas.md" }))
        .await
        .unwrap();
    let id = "doc.canvas.md";

    // A failing block: a normal result, flagged as an error, with the exit
    // code and the step's own output.
    let (is_error, bad) = call(
        &mcp,
        "run",
        serde_json::json!({ "canvas_id": id, "node_id": "root", "block": "bad" }),
    )
    .await
    .unwrap();
    assert!(is_error, "{bad}");
    assert_eq!(bad["success"], false);
    assert_eq!(step(&bad, "bad")["status"], "failed");
    assert_eq!(step(&bad, "bad")["exit_code"], 3);
    assert!(step(&bad, "bad")["output"].as_str().unwrap().contains("nope"));

    // An unresolved variable is named, not prompted for; supplying it works.
    let err = call(
        &mcp,
        "run",
        serde_json::json!({ "canvas_id": id, "node_id": "root", "block": "needs-who" }),
    )
    .await
    .unwrap_err();
    assert!(err.contains("WHO"), "{err}");
    let (is_error, ok) = call(
        &mcp,
        "run",
        serde_json::json!({
            "canvas_id": id, "node_id": "root", "block": "needs-who",
            "vars": { "WHO": "agent" }
        }),
    )
    .await
    .unwrap();
    assert!(!is_error, "{ok}");
    assert!(step(&ok, "needs-who")["output"].as_str().unwrap().contains("hi agent"));

    // Unknown node.
    let err = call(
        &mcp,
        "run",
        serde_json::json!({ "canvas_id": id, "node_id": "nope", "block": "x" }),
    )
    .await
    .unwrap_err();
    assert!(err.contains("no node"), "{err}");

    // No block given: the node's default block (named like the node).
    let (is_error, default) = call(
        &mcp,
        "run",
        serde_json::json!({ "canvas_id": id, "node_id": "app" }),
    )
    .await
    .unwrap();
    assert!(!is_error, "{default}");
    assert!(step(&default, "app")["output"]
        .as_str()
        .unwrap()
        .contains("the-default-block"));

    // A step still running at the deadline is killed and reported.
    let started = Instant::now();
    let (is_error, slow) = call(
        &mcp,
        "run",
        serde_json::json!({
            "canvas_id": id, "node_id": "root", "block": "slow", "timeout_ms": 700
        }),
    )
    .await
    .unwrap();
    assert!(is_error, "{slow}");
    assert_eq!(slow["timed_out"], true, "{slow}");
    assert_eq!(slow["success"], false);
    assert!(
        started.elapsed() < Duration::from_secs(20),
        "the timeout must cut a 30 s step short"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
