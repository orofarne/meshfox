//! The MCP `form_submit` and `form_reset` tools, driven over stdio against a
//! real `meshfox mcp`: a submit commits values into the session (what a
//! later `run` sees) and starts the `autorun` blocks it reaches; a reset
//! puts the fields back to their defaults and starts nothing.

use std::path::{Path, PathBuf};
use std::time::Duration;

use rmcp::model::CallToolRequestParams;
use rmcp::transport::{ConfigureCommandExt, TokioChildProcess};
use rmcp::{RoleClient, ServiceError, ServiceExt};
use tokio::process::Command;

const CANVAS: &str = concat!(
    "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
    "<!-- meshfox:var name=\"N\" default=\"dflt\" -->\n\n",
    "```form name=\"f\"\nfield var=\"N\"\n```\n\n",
    "```bash name=\"log\" env=\"N\" autorun\necho \"$N\" >> autorun.txt\n```\n\n",
    "```bash name=\"maybe-fail\" env=\"N\" autorun\n[ \"$N\" != boom ] || { echo bad >&2; exit 3; }\n```\n\n",
    "```bash name=\"show\" env=\"N\"\necho \"N=$N\"\n```\n",
);

fn unique_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("mf-mcp-form-{}", uuid::Uuid::new_v4().simple()));
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

fn autorun_lines(dir: &Path) -> Vec<String> {
    std::fs::read_to_string(dir.join("autorun.txt"))
        .map(|s| s.lines().map(str::to_string).collect())
        .unwrap_or_default()
}

async fn show(mcp: &Client) -> String {
    let (is_error, result) = call(
        mcp,
        "run",
        serde_json::json!({ "canvas_id": "doc.canvas.md", "node_id": "root", "block": "show" }),
    )
    .await
    .unwrap();
    assert!(!is_error, "{result}");
    result["steps"][0]["output"]
        .as_str()
        .unwrap()
        .trim()
        .to_string()
}

#[tokio::test]
async fn form_submit_commits_values_and_starts_autorun_and_form_reset_undoes_it() {
    let dir = unique_dir();
    let mcp = start(&dir).await;
    call(
        &mcp,
        "canvas_open",
        serde_json::json!({ "path": "doc.canvas.md" }),
    )
    .await
    .unwrap();
    let id = "doc.canvas.md";

    assert_eq!(show(&mcp).await, "N=dflt");

    let (is_error, sent) = call(
        &mcp,
        "form_submit",
        serde_json::json!({
            "canvas_id": id, "node_id": "root", "block": "f", "values": { "N": "hello" }
        }),
    )
    .await
    .unwrap();
    assert!(!is_error, "{sent}");
    // The call waits for the autorun block and reports how it went.
    assert_eq!(sent["success"], true, "{sent}");
    let blocks: Vec<&str> = sent["autorun"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["block"].as_str().unwrap())
        .collect();
    assert!(
        blocks.contains(&"log") && blocks.contains(&"maybe-fail"),
        "{sent}"
    );
    assert!(
        sent["autorun"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["exit_code"] == 0),
        "{sent}"
    );
    assert_eq!(autorun_lines(&dir), vec!["hello"]);
    assert_eq!(
        show(&mcp).await,
        "N=hello",
        "the submitted value is the session's"
    );

    let (is_error, reset) = call(
        &mcp,
        "form_reset",
        serde_json::json!({ "canvas_id": id, "node_id": "root", "block": "f" }),
    )
    .await
    .unwrap();
    assert!(!is_error, "{reset}");
    assert_eq!(reset["cleared"], 1, "{reset}");
    assert_eq!(show(&mcp).await, "N=dflt");
    let before = autorun_lines(&dir).len();
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(autorun_lines(&dir).len(), before, "a reset starts nothing");

    let wrong = call(
        &mcp,
        "form_submit",
        serde_json::json!({ "canvas_id": id, "node_id": "root", "block": "show" }),
    )
    .await;
    assert!(
        wrong.is_err() || wrong.as_ref().is_ok_and(|(e, _)| *e),
        "a bash block is not a form: {wrong:?}"
    );

    // A failing autorun block makes the call report `success: false` with
    // its exit code and output; `wait: false` returns at once.
    let (_, boom) = call(
        &mcp,
        "form_submit",
        serde_json::json!({
            "canvas_id": id, "node_id": "root", "block": "f", "values": { "N": "boom" }
        }),
    )
    .await
    .unwrap();
    assert_eq!(boom["success"], false, "{boom}");
    let bad = boom["autorun"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["block"] == "maybe-fail")
        .unwrap();
    assert_eq!(bad["exit_code"], 3, "{boom}");
    assert!(bad["output"].as_str().unwrap().contains("bad"), "{boom}");
    let (_, nowait) = call(
        &mcp,
        "form_submit",
        serde_json::json!({
            "canvas_id": id, "node_id": "root", "block": "f",
            "values": { "N": "later" }, "wait": false
        }),
    )
    .await
    .unwrap();
    assert!(nowait["autorun"][0].get("exit_code").is_none(), "{nowait}");

    // Running a form directly parks on it; sending it releases the run.
    let (is_error, ran) = call(
        &mcp,
        "run",
        serde_json::json!({ "canvas_id": id, "node_id": "root", "block": "f" }),
    )
    .await
    .unwrap();
    assert!(!is_error, "{ran}");
    assert_eq!(ran["status"], "awaiting_form", "{ran}");
    assert_eq!(ran["awaiting_form"]["block"], "f", "{ran}");
    let (is_error, released) = call(
        &mcp,
        "form_submit",
        serde_json::json!({
            "canvas_id": id, "node_id": "root", "block": "f",
            "values": { "N": "dflt" }, "wait": false
        }),
    )
    .await
    .unwrap();
    assert!(!is_error, "{released}");

    let _ = std::fs::remove_dir_all(&dir);
}

const FLOW: &str = concat!(
    "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
    "<!-- meshfox:var name=\"ANSWER\" -->\n\n",
    "```bash name=\"prep\"\necho prepared > flow.txt\n```\n\n",
    "```form name=\"review\" deps=\"prep\"\ntext \"Check flow.txt\"\nfield var=\"ANSWER\"\n```\n\n",
    "```bash name=\"finish\" deps=\"review\" env=\"ANSWER\"\necho \"answer=$ANSWER\" >> flow.txt\n```\n",
);

#[tokio::test]
async fn a_run_parks_on_a_form_step_and_form_submit_resumes_it() {
    let dir = unique_dir();
    std::fs::write(dir.join("flow.canvas.md"), FLOW).unwrap();
    let mcp = start(&dir).await;
    let id = "flow.canvas.md";
    call(&mcp, "canvas_open", serde_json::json!({ "path": id }))
        .await
        .unwrap();

    let (is_error, parked) = call(
        &mcp,
        "run",
        serde_json::json!({ "canvas_id": id, "node_id": "root", "block": "finish" }),
    )
    .await
    .unwrap();
    assert!(!is_error, "{parked}");
    assert_eq!(parked["status"], "awaiting_form", "{parked}");
    assert_eq!(parked["awaiting_form"]["node_id"], "root", "{parked}");
    assert_eq!(parked["awaiting_form"]["block"], "review", "{parked}");
    assert_eq!(
        std::fs::read_to_string(dir.join("flow.txt")).unwrap(),
        "prepared\n",
        "finish must not run before the form is answered"
    );

    let (is_error, sent) = call(
        &mcp,
        "form_submit",
        serde_json::json!({
            "canvas_id": id, "node_id": "root", "block": "review",
            "values": { "ANSWER": "yes" }
        }),
    )
    .await
    .unwrap();
    assert!(!is_error, "{sent}");
    let mut text = String::new();
    for _ in 0..100 {
        text = std::fs::read_to_string(dir.join("flow.txt")).unwrap_or_default();
        if text.contains("answer=yes") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(text, "prepared\nanswer=yes\n");

    // The transcript records what the form waited for and how it was answered.
    let mut logs = String::new();
    for _ in 0..100 {
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_meshfox"))
            .args(["logs", "--canvas", "flow.canvas.md", "--chain", "finish"])
            .current_dir(&dir)
            .env("HOME", dir.join("home"))
            .env("MESHFOX_SERVER_SOCKET", "")
            .output()
            .expect("meshfox logs runs");
        logs = String::from_utf8_lossy(&out.stdout).into_owned();
        if logs.contains("ANSWER=yes") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(logs.contains("ANSWER=yes"), "form answers in logs: {logs}");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn cli_form_submit_waits_for_the_autorun_blocks_prints_their_output_and_fails_with_them() {
    let dir = unique_dir();
    let submit = |value: &str, extra: &[&str]| {
        std::process::Command::new(env!("CARGO_BIN_EXE_meshfox"))
            .args(["form", "submit", "--canvas", "doc.canvas.md", "root", "f"])
            .args(["--set", &format!("N={value}")])
            .args(extra)
            .current_dir(&dir)
            .env("HOME", dir.join("home"))
            .env("MESHFOX_SERVER_SOCKET", "")
            .output()
            .expect("meshfox runs")
    };

    let ok = submit("hi", &[]);
    let stdout = String::from_utf8_lossy(&ok.stdout);
    assert!(
        ok.status.success(),
        "{stdout}{}",
        String::from_utf8_lossy(&ok.stderr)
    );
    assert!(stdout.contains("== root/log: ok"), "{stdout}");
    assert_eq!(
        autorun_lines(&dir),
        vec!["hi"],
        "the block finished before the CLI returned"
    );

    let bad = submit("boom", &[]);
    let stdout = String::from_utf8_lossy(&bad.stdout);
    assert_eq!(bad.status.code(), Some(1), "{stdout}");
    assert!(stdout.contains("== root/maybe-fail: exit 3"), "{stdout}");
    assert!(
        stdout.contains("bad"),
        "the failing block's output is shown: {stdout}"
    );

    let detached = submit("later", &["--no-wait"]);
    let stdout = String::from_utf8_lossy(&detached.stdout);
    assert!(detached.status.success(), "{stdout}");
    assert!(stdout.contains("started root/log"), "{stdout}");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn cli_run_without_a_terminal_cancels_a_chain_parked_on_a_form() {
    let dir = unique_dir();
    std::fs::write(dir.join("flow.canvas.md"), FLOW).unwrap();
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_meshfox"))
        .args(["run", "--canvas", "flow.canvas.md", "finish"])
        .stdin(std::process::Stdio::null())
        .current_dir(&dir)
        .env("HOME", dir.join("home"))
        .env("MESHFOX_SERVER_SOCKET", "")
        .output()
        .expect("meshfox runs");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "{stderr}");
    assert!(stderr.contains("no terminal to ask it"), "{stderr}");
    assert!(stderr.contains("was cancelled"), "{stderr}");
    assert_eq!(
        std::fs::read_to_string(dir.join("flow.txt")).unwrap(),
        "prepared\n",
        "finish must not run"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
