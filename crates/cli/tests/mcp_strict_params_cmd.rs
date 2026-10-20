//! MCP tool arguments are checked strictly, driven over stdio against a real
//! `meshfox mcp`: a parameter the tool's schema doesn't declare (the typo
//! `add_tag` for `add_tags` that used to be dropped silently, answering
//! "updated" for an update that changed nothing) is refused with a hint and
//! nothing runs; a `node_meta` call that changes nothing is an error too;
//! and the correct spelling still works.

use std::path::{Path, PathBuf};

use rmcp::model::CallToolRequestParams;
use rmcp::transport::{ConfigureCommandExt, TokioChildProcess};
use rmcp::{RoleClient, ServiceError, ServiceExt};
use tokio::process::Command;

const CANVAS: &str = concat!(
    "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
    "## Task\n<!-- meshfox:node id=\"task\" tags=\"old\" -->\n\n",
    "первая строка тела, достаточно длинная для обрезки\n",
);

fn unique_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("mf-mcp-strict-{}", uuid::Uuid::new_v4().simple()));
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

/// Structured content of a successful call, or `Err(message)` when the call
/// was rejected.
async fn call(
    client: &Client,
    tool: &'static str,
    args: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let serde_json::Value::Object(map) = args else {
        panic!("tool arguments must be a JSON object");
    };
    match client
        .call_tool(CallToolRequestParams::new(tool).with_arguments(map))
        .await
    {
        Ok(result) if result.is_error.unwrap_or(false) => {
            Err(format!("{:?}", result.structured_content))
        }
        Ok(result) => Ok(result.structured_content.unwrap_or(serde_json::Value::Null)),
        Err(ServiceError::McpError(e)) => Err(e.message.to_string()),
        Err(e) => Err(e.to_string()),
    }
}

fn tags_of(shown: &serde_json::Value) -> Vec<String> {
    shown["tags"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t.as_str().unwrap().to_string())
        .collect()
}

#[tokio::test]
async fn a_misspelled_parameter_is_refused_with_a_hint_and_nothing_runs() {
    let dir = unique_dir();
    let client = start(&dir).await;

    let error = call(
        &client,
        "node_meta",
        serde_json::json!({
            "canvas_id": "doc.canvas.md", "node_id": "task", "add_tag": "new"
        }),
    )
    .await
    .expect_err("an unknown parameter must be an error");
    assert!(error.contains("add_tag"), "{error}");
    assert!(error.contains("did you mean \"add_tags\""), "{error}");
    assert!(error.contains("nothing was done"), "{error}");

    let shown = call(
        &client,
        "node_show",
        serde_json::json!({ "canvas_id": "doc.canvas.md", "node_id": "task" }),
    )
    .await
    .unwrap();
    assert_eq!(
        tags_of(&shown),
        ["old"],
        "the refused call changed the node"
    );

    // Tools that take no `canvas_id` are checked too.
    let error = call(
        &client,
        "canvas_open",
        serde_json::json!({ "path": "doc.canvas.md", "creat": true }),
    )
    .await
    .expect_err("canvas_open must refuse an unknown parameter");
    assert!(error.contains("did you mean \"create\""), "{error}");

    let _ = client.cancel().await;
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn node_meta_that_changes_nothing_is_an_error_and_the_right_spelling_works() {
    let dir = unique_dir();
    let client = start(&dir).await;
    let target = |extra: serde_json::Value| {
        let mut args = serde_json::json!({ "canvas_id": "doc.canvas.md", "node_id": "task" });
        args.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        args
    };

    for nothing in [serde_json::json!({}), serde_json::json!({ "add_tags": [] })] {
        let error = call(&client, "node_meta", target(nothing))
            .await
            .expect_err("an update with no fields must be an error");
        assert!(error.contains("nothing to change"), "{error}");
    }

    // Fields flattened in from `McpNodeFields` and the tag lists are all
    // still accepted.
    call(
        &client,
        "node_meta",
        target(serde_json::json!({ "add_tags": ["new"], "x": 10.0 })),
    )
    .await
    .expect("the right spelling is accepted");
    let shown = call(
        &client,
        "node_show",
        serde_json::json!({ "canvas_id": "doc.canvas.md", "node_id": "task" }),
    )
    .await
    .unwrap();
    assert_eq!(tags_of(&shown), ["old", "new"]);
    assert_eq!(shown["x"], 10.0);

    let _ = client.cancel().await;
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn body_limit_cuts_bodies_and_reports_how_much_was_left_out() {
    let dir = unique_dir();
    let client = start(&dir).await;

    let shown = call(
        &client,
        "node_show",
        serde_json::json!({
            "canvas_id": "doc.canvas.md", "node_id": "task",
            "include_body": true, "body_limit": 6
        }),
    )
    .await
    .unwrap();
    assert_eq!(shown["body"], "первая");
    assert!(shown["body_truncated"].as_u64().unwrap() > 0);

    let found = call(
        &client,
        "node_find",
        serde_json::json!({
            "canvas_id": "doc.canvas.md", "selector": "#task",
            "show": true, "include_body": true, "body_limit": 6
        }),
    )
    .await
    .unwrap();
    assert_eq!(found["nodes"][0]["body"], "первая");

    let whole = call(
        &client,
        "node_show",
        serde_json::json!({
            "canvas_id": "doc.canvas.md", "node_id": "task", "include_body": true
        }),
    )
    .await
    .unwrap();
    assert!(whole.get("body_truncated").is_none());
    assert!(whole["body"].as_str().unwrap().len() > 6);

    let _ = client.cancel().await;
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn the_check_tool_reports_render_blocks_and_a_failing_one_fails_it() {
    let dir = unique_dir();
    std::fs::write(
        dir.join("views.canvas.md"),
        concat!(
            "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
            "```starlark render name=\"good\"\nprint(sum([1, 2]))\n```\n\n",
            "```starlark render name=\"bad\"\nprint(no_such_name)\n```\n",
        ),
    )
    .unwrap();
    let client = start(&dir).await;

    let checked = call(
        &client,
        "check",
        serde_json::json!({ "canvas_id": "views.canvas.md" }),
    )
    .await
    .unwrap();
    assert_eq!(checked["ok"], false);
    let renders = checked["renders"].as_array().unwrap();
    assert_eq!(renders.len(), 2, "{checked}");
    assert_eq!(renders[0]["label"], "root/good");
    assert_eq!(renders[0]["ok"], true);
    assert_eq!(renders[1]["label"], "root/bad");
    assert_eq!(renders[1]["ok"], false);
    assert!(renders[1]["error"]
        .as_str()
        .unwrap()
        .contains("no_such_name"));

    let _ = client.cancel().await;
    let _ = std::fs::remove_dir_all(&dir);
}
