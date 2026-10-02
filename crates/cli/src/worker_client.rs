//! Thin HTTP/WS client for CLI, MCP and TUI operations. Canvas mutations
//! always reach the worker selected or started by `coordinator::get_or_spawn`;
//! this module never writes the primary canvas directly.

use meshfox_core::{Canvas, FileDisplay, NodeType};
use meshfox_server::stream_exec::OutputStream;
use std::collections::{HashMap, HashSet};
use std::time::Duration;

// ---- Time limits for HTTP calls to a worker -------------------------------
//
// A worker that is alive but not answering used to hang its caller for ever:
// every call below went through a client with no time limit at all. Every
// request now has one, chosen by what it does:
//
// | class | limit | calls |
// |---|---|---|
// | quick | 30 s | node edits and reads, canvas/vars/history/run/service reads, forms, `debug_start`/`debug_stop`, file reads |
// | whole document | 60 s | `put_canvas_raw` (a canvas-valued include goes through another worker, which the daemon gives 15 s to start), `history_goto` (replays up to 200 steps) |
// | control | 20 s | `stop_service`, `restart_service`, `kill_run` (a `SIGKILL` of the process group and a wait of up to 5 s) |
// | debug command | the command's own `timeoutMs` + 15 s | `debug_send` — its own limit must fire before this one does |
//
// Connecting gets 5 s on top of that: a worker listens on localhost, so a
// dead port refuses at once and anything slower is a worker that accepted
// the connection and then stopped serving. WebSocket streams (runs, `tty`,
// watching) are not HTTP requests and are not covered by any of this.

const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const QUICK: Duration = Duration::from_secs(30);
const WHOLE_DOCUMENT: Duration = Duration::from_secs(60);
const CONTROL: Duration = Duration::from_secs(20);
const DEBUG_SEND_MARGIN: Duration = Duration::from_secs(15);
const PING: Duration = Duration::from_secs(3);

/// The limit for a `debug_send`: the command's own timeout, plus a margin so
/// the worker's answer to *that* timeout arrives before this client gives up.
fn debug_send_limit(timeout_ms: u64) -> Duration {
    Duration::from_millis(timeout_ms) + DEBUG_SEND_MARGIN
}

#[cfg(test)]
tokio::task_local! {
    /// Lets a test shrink every limit to something it can wait for, for the
    /// duration of one future — without touching the process-wide state other
    /// tests running alongside it share.
    pub(crate) static LIMIT_OVERRIDE: Duration;
}

/// `default`, unless a test has overridden it (see `LIMIT_OVERRIDE`).
fn time_limit(default: Duration) -> Duration {
    #[cfg(test)]
    if let Ok(overridden) = LIMIT_OVERRIDE.try_with(|d| *d) {
        return overridden;
    }
    default
}

/// The HTTP client for one call: the connect limit; each request adds its own
/// overall limit (`time_limit(...)`). Deliberately a new client every time, as
/// before limits existed, not one shared (pooled) client: sharing connections
/// between calls made `meshfox run`'s Ctrl-C path leave a service orphaned in
/// `service_run_cmd` — found by swapping the shared client for a fresh one,
/// which made that failure go away; the mechanism was not pinned down.
fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .build()
        .expect("an HTTP client with a connect timeout")
}

/// What probing a worker with `ping` found.
pub enum WorkerHealth {
    /// It answered.
    Answering,
    /// Nothing is listening any more: the worker exited, so nothing is hung —
    /// the next call finds or starts another.
    Gone,
    /// It accepted the connection and did not answer in time.
    Unresponsive,
}

/// `GET /api/ping` — asks whether a worker is merely alive (its process
/// exists, its port is open) or actually serving. A worker that predates the
/// endpoint answers like any unknown path (its UI shell), which still counts
/// as answering. 3 seconds.
pub async fn ping(port: u16) -> WorkerHealth {
    match client()
        .get(format!("{}/api/ping", base_url(port)))
        .timeout(time_limit(PING))
        .send()
        .await
    {
        Ok(res) if res.status().is_success() => WorkerHealth::Answering,
        Ok(_) => WorkerHealth::Unresponsive,
        Err(e) if e.is_timeout() => WorkerHealth::Unresponsive,
        Err(_) => WorkerHealth::Gone,
    }
}

/// A transport error as text for a person: a worker that did not answer in
/// time or could not be reached says so, rather than a bare reqwest message.
fn describe(error: &reqwest::Error) -> String {
    if error.is_timeout() {
        "the worker did not answer in time — it may be hung".to_string()
    } else if error.is_connect() {
        "could not connect to the worker — it may have exited".to_string()
    } else {
        error.to_string()
    }
}

/// `http://127.0.0.1:<port>/api/nodes/<id>` — built via `path_segments_mut`
/// (not a hand-rolled `format!`) so a node id containing characters that
/// aren't plain path-safe (an include-spliced node's namespaced id, e.g.
/// `docs/setup`) is percent-encoded the same way `web/src/api.ts`'s own
/// `encodeURIComponent(id)` already handles it, rather than accidentally
/// introducing an extra path segment.
fn node_url(port: u16, node_id: &str) -> Result<reqwest::Url, String> {
    let mut url = reqwest::Url::parse(&format!("http://127.0.0.1:{port}/api/nodes"))
        .map_err(|e| e.to_string())?;
    url.path_segments_mut()
        .map_err(|_| "couldn't build the worker's node URL".to_string())?
        .push(node_id);
    Ok(url)
}

/// Field-for-field mirror of `crates/server/src/lib.rs`'s own
/// `UpdateNodeRequest` — every field left at `None`/`Default::default()`
/// leaves that piece of the node untouched, same "not sent" convention the
/// server side already documents on each field there. One shared type for
/// every CLI/MCP `node <op>` that boils down to a `PATCH /api/nodes/:id`
/// once a worker exists (`meta`/`rename`/`edges`, and `node add`'s own
/// follow-up fields) rather than a narrower struct per caller.
/// Tag changes for `NodeUpdate::tags`: `remove` first, then `add` — applied
/// by the server to the tags the node has *at that moment*, never a
/// replacement list, so two clients adding different tags both land.
#[derive(Default, serde::Serialize)]
pub struct TagOps {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub add: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub remove: Vec<String>,
}

/// Edge changes for `NodeUpdate::edges`, keyed by the edge's source node:
/// `remove`, then `add` (a plain edge; a no-op when one from that node
/// already exists, so an existing label/route is never wiped). The server
/// also accepts per-field `patch`es (the web UI's edge editor sends them);
/// the CLI and MCP only add and remove edges.
#[derive(Default, serde::Serialize)]
pub struct EdgeOps {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub add: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub remove: Vec<String>,
}

#[derive(Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeUpdate {
    pub title: Option<String>,
    pub node_type: Option<NodeType>,
    pub color: Option<String>,
    pub target: Option<String>,
    pub text: Option<String>,
    /// The `bodyRev` of the node as the caller last read it — the server
    /// requires it whenever `text` is set and answers `409` if the body has
    /// changed since.
    pub base_rev: Option<String>,
    pub edges: Option<EdgeOps>,
    pub display: Option<FileDisplay>,
    pub lang: Option<String>,
    pub interpreter: Option<String>,
    pub preview: Option<bool>,
    pub tags: Option<TagOps>,
    pub edge_label: Option<String>,
    /// `"true"`/`"false"`/`"default"` — same string-sentinel convention
    /// `UpdateNodeRequest::fold`'s own doc comment explains (a plain
    /// `Option<bool>` can't reach a "clear the override back to unset"
    /// third state on its own).
    pub fold: Option<String>,
    pub x: Option<f64>,
    pub y: Option<f64>,
    pub width: Option<f64>,
    pub height: Option<f64>,
    pub clear_position: bool,
    pub created_at: Option<String>,
}

/// PATCH `/api/nodes/:id` with whatever `update` actually sets — the
/// worker-routed counterpart to `crates/server/src/lib.rs::update_node`,
/// which this has identical semantics to (including its group-node
/// width/height rejection and `createdAt` RFC3339 validation, both
/// surfaced here as an ordinary `Err` same as any other `422`).
pub async fn update_node(port: u16, node_id: &str, update: &NodeUpdate) -> Result<(), String> {
    let url = node_url(port, node_id)?;
    let res = client()
        .patch(url)
        .json(update)
        .timeout(time_limit(QUICK))
        .send()
        .await
        .map_err(|e| describe(&e))?;
    into_result(res).await
}

/// `update_node` with just `text` set — the same `mdcanvas::set_node_body`
/// primitive `update_node`'s own `req.text` handling already applies, so
/// this has identical semantics to `apply_node_body`'s direct-file version.
/// `base_rev` is the `bodyRev` the caller read the body at; if the body has
/// changed since, this fails with the node's current text and revision in
/// the error, ready to retry against.
pub async fn update_node_body(
    port: u16,
    node_id: &str,
    body: &str,
    base_rev: &str,
) -> Result<(), String> {
    update_node(
        port,
        node_id,
        &NodeUpdate {
            text: Some(body.to_string()),
            base_rev: Some(base_rev.to_string()),
            ..Default::default()
        },
    )
    .await
}

pub async fn append_node_body(port: u16, node_id: &str, addition: &str) -> Result<(), String> {
    let mut url = node_url(port, node_id)?;
    url.path_segments_mut()
        .map_err(|_| "couldn't build the worker's append URL".to_string())?
        .push("append");
    let res = client()
        .post(url)
        .body(addition.to_string())
        .timeout(time_limit(QUICK))
        .send()
        .await
        .map_err(|e| describe(&e))?;
    into_result(res).await
}

/// DELETE `/api/nodes/:id[?children=reparent]` — the same
/// `mdcanvas::delete_node`/`delete_node_reparent_children` primitives and
/// root-node guard `apply_node_rm` already has (`crates/server/src/lib.rs::remove_node`),
/// so this has identical semantics to `apply_node_rm`'s direct-file version.
pub async fn remove_node(port: u16, node_id: &str, keep_children: bool) -> Result<(), String> {
    let mut url = node_url(port, node_id)?;
    if keep_children {
        url.query_pairs_mut().append_pair("children", "reparent");
    }
    let res = client()
        .delete(url)
        .timeout(time_limit(QUICK))
        .send()
        .await
        .map_err(|e| describe(&e))?;
    into_result(res).await
}

/// POST `/api/nodes` — the worker-routed counterpart to CLI/MCP `node
/// add`. `title_slug_id: true` requests the plain title-slug id scheme
/// `node add` has always produced (`mdcanvas::insert_child_node`) instead
/// of the web UI's own random one — see `crates/server/src/lib.rs`'s
/// `CreateNodeRequest::title_slug_id` doc comment for why a caller needs
/// to opt into this explicitly rather than the id scheme depending on
/// whether a worker happens to be running. Returns the new node's own id
/// (`CreateNodeResponse::newId`) — nothing about either insertion function
/// lets a caller predict it in advance.
pub async fn create_node(
    port: u16,
    parent_id: &str,
    title: &str,
    title_slug_id: bool,
    body: Option<&str>,
) -> Result<String, String> {
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Response {
        new_id: String,
    }
    let res = client()
        .post(format!("{}/api/nodes", base_url(port)))
        .json(&serde_json::json!({
            "parentId": parent_id,
            "title": title,
            "titleSlugId": title_slug_id,
            "body": body,
        }))
        .timeout(time_limit(QUICK))
        .send()
        .await
        .map_err(|e| describe(&e))?;
    if !res.status().is_success() {
        let status = res.status();
        let text = res.text().await.unwrap_or_default();
        return Err(if text.is_empty() {
            status.to_string()
        } else {
            text
        });
    }
    res.json::<Response>()
        .await
        .map(|r| r.new_id)
        .map_err(|e| e.to_string())
}

/// POST `/api/nodes/:id/reparent` — the same `mdcanvas::reparent_node`
/// primitive `apply_node_mv`'s own direct-file path (`crates/cli/src/main.rs`)
/// calls, including the position-frame conversion it does server-side
/// (see that endpoint's own doc comment) — this client never needs to
/// replicate that math itself. `newParentId` must already be one of the
/// node's declared extra parents; `crate::main::node_mv`'s worker-routed
/// path adds that edge first via `update_node`'s own `extra_parents`
/// before calling this, mirroring `apply_node_mv`'s own two-step shape.
pub async fn reparent_node(port: u16, node_id: &str, new_parent_id: &str) -> Result<(), String> {
    let mut url = node_url(port, node_id)?;
    url.path_segments_mut()
        .map_err(|_| "couldn't build the worker's reparent URL".to_string())?
        .push("reparent");
    let res = client()
        .post(url)
        .json(&serde_json::json!({ "newParentId": new_parent_id }))
        .timeout(time_limit(QUICK))
        .send()
        .await
        .map_err(|e| describe(&e))?;
    into_result(res).await
}

/// POST `/api/nodes/:id/move` — the same `mdcanvas::move_sibling`
/// primitive `apply_node_move`'s own direct-file path calls; `before`
/// selects which of `MoveSiblingRequest`'s own mutually-exclusive
/// `before`/`after` fields carries `target_id`.
pub async fn move_sibling(
    port: u16,
    node_id: &str,
    target_id: &str,
    before: bool,
) -> Result<(), String> {
    let mut url = node_url(port, node_id)?;
    url.path_segments_mut()
        .map_err(|_| "couldn't build the worker's move URL".to_string())?
        .push("move");
    let body = if before {
        serde_json::json!({ "before": target_id })
    } else {
        serde_json::json!({ "after": target_id })
    };
    let res = client()
        .post(url)
        .json(&body)
        .timeout(time_limit(QUICK))
        .send()
        .await
        .map_err(|e| describe(&e))?;
    into_result(res).await
}

/// POST `/api/nodes/:id/rename-id` — the same `mdcanvas::rename_node_id`
/// primitive `apply_node_set_id`'s own direct-file path (CLI `node
/// set_id`/`mdcanvas::rename_node_id`) calls.
pub async fn rename_node_id(port: u16, node_id: &str, new_id: &str) -> Result<(), String> {
    let mut url = node_url(port, node_id)?;
    url.path_segments_mut()
        .map_err(|_| "couldn't build the worker's rename-id URL".to_string())?
        .push("rename-id");
    let res = client()
        .post(url)
        .json(&serde_json::json!({ "newId": new_id }))
        .timeout(time_limit(QUICK))
        .send()
        .await
        .map_err(|e| describe(&e))?;
    into_result(res).await
}

/// Field-for-field mirror of `crates/server/src/lib.rs`'s own
/// `UpdateBlockAttrsRequest` — see that struct's own doc comment for why
/// `deps`/`env` carry the same comma-separated text syntax the CLI already
/// parses instead of a typed list, and why `interpreter`/`clearInterpreter`
/// are a two-field pair rather than a nested-Option sentinel.
#[derive(Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BlockAttrsUpdate {
    pub name: Option<String>,
    pub lang: Option<String>,
    pub cache: Option<bool>,
    pub always: Option<bool>,
    pub default: Option<bool>,
    pub tty: Option<bool>,
    pub autoclose: Option<bool>,
    pub service: Option<bool>,
    pub deps: Option<String>,
    pub env: Option<String>,
    pub interpreter: Option<String>,
    pub clear_interpreter: bool,
    pub code: Option<String>,
}

/// PATCH `/api/nodes/:id/block/:blockName` — the worker-routed counterpart
/// to CLI/MCP `node block`'s own direct-file `apply_node_block`.
pub async fn set_block_attrs(
    port: u16,
    node_id: &str,
    block_name: &str,
    update: &BlockAttrsUpdate,
) -> Result<(), String> {
    let mut url = node_url(port, node_id)?;
    url.path_segments_mut()
        .map_err(|_| "couldn't build the worker's block-attrs URL".to_string())?
        .push("block")
        .push(block_name);
    let res = client()
        .patch(url)
        .json(update)
        .timeout(time_limit(QUICK))
        .send()
        .await
        .map_err(|e| describe(&e))?;
    into_result(res).await
}

/// POST `/api/canvas/reorder-siblings` — the worker-routed counterpart to
/// CLI/MCP `node reorder`'s own direct-file `apply_node_reorder`. No node
/// id at all: this re-sorts every parent's children in the whole document
/// at once, same as the direct-file path does.
pub async fn reorder_document(port: u16) -> Result<(), String> {
    let res = client()
        .post(format!("{}/api/canvas/reorder-siblings", base_url(port)))
        .timeout(time_limit(QUICK))
        .send()
        .await
        .map_err(|e| describe(&e))?;
    into_result(res).await
}

/// One `/api/undo`/`/api/redo`/`/api/history/goto` response — deliberately
/// narrower than the server's own `UndoRedoResponse` (no full `Canvas`):
/// every caller here just reports the resulting undo/redo state, then
/// re-fetches the canvas itself (`get_canvas_raw`) if it actually needs to
/// show anything from it — an unrecognized `canvas`-shaped blob in the
/// JSON is simply ignored by `#[derive(Deserialize)]`'s own default
/// "unknown fields are dropped" behavior, same as `create_node`'s own
/// narrower `Response` above.
#[derive(serde::Deserialize, serde::Serialize, Debug, Clone, Copy)]
#[serde(rename_all = "camelCase")]
pub struct UndoRedoResult {
    pub changed: bool,
    pub can_undo: bool,
    pub can_redo: bool,
}

async fn post_undo_redo(url: String) -> Result<UndoRedoResult, String> {
    let res = client()
        .post(url)
        .timeout(time_limit(QUICK))
        .send()
        .await
        .map_err(|e| describe(&e))?;
    if !res.status().is_success() {
        let status = res.status();
        let text = res.text().await.unwrap_or_default();
        return Err(if text.is_empty() {
            status.to_string()
        } else {
            text
        });
    }
    res.json().await.map_err(|e| e.to_string())
}

/// `POST /api/undo` — the worker-routed counterpart to `crates/server/
/// src/lib.rs`'s `api_undo`. A no-op (`changed: false`), not an error,
/// when there's nothing left to undo.
pub async fn undo(port: u16) -> Result<UndoRedoResult, String> {
    post_undo_redo(format!("{}/api/undo", base_url(port))).await
}

/// `POST /api/redo` — the mirror image of [`undo`].
pub async fn redo(port: u16) -> Result<UndoRedoResult, String> {
    post_undo_redo(format!("{}/api/redo", base_url(port))).await
}

/// `POST /api/history/goto` — jumps directly to `seq` (as listed by
/// [`history`] below), in whichever direction that is from the current
/// cursor — undoing or redoing as many steps as it takes in one call. An
/// unreachable/stale `seq` lands as far as it can rather than erroring
/// (`changed` says whether anything actually moved); see `crates/server/
/// src/lib.rs`'s own `jump_to` for why.
pub async fn history_goto(port: u16, seq: i64) -> Result<UndoRedoResult, String> {
    let res = client()
        .post(format!("{}/api/history/goto", base_url(port)))
        .json(&serde_json::json!({ "seq": seq }))
        .timeout(time_limit(WHOLE_DOCUMENT))
        .send()
        .await
        .map_err(|e| describe(&e))?;
    if !res.status().is_success() {
        let status = res.status();
        let text = res.text().await.unwrap_or_default();
        return Err(if text.is_empty() {
            status.to_string()
        } else {
            text
        });
    }
    res.json().await.map_err(|e| e.to_string())
}

#[derive(serde::Deserialize, serde::Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct HistoryEntryDto {
    pub seq: i64,
    pub created_at: String,
    pub op_kind: String,
    /// `true` if this step is currently applied (an `undo` would revert
    /// it), `false` if it's sitting in the redo tail (a `redo`, or a
    /// `history_goto` naming this same `seq`, would reapply it).
    pub applied: bool,
    pub summary: String,
}

#[derive(serde::Deserialize, serde::Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct HistoryDto {
    pub cursor: i64,
    pub can_undo: bool,
    pub can_redo: bool,
    pub entries: Vec<HistoryEntryDto>,
}

/// `GET /api/history?limit=N` — the worker-routed counterpart to
/// `crates/server/src/lib.rs`'s `api_history`.
pub async fn history(port: u16, limit: Option<usize>) -> Result<HistoryDto, String> {
    let mut url = reqwest::Url::parse(&format!("{}/api/history", base_url(port)))
        .map_err(|e| e.to_string())?;
    if let Some(limit) = limit {
        url.query_pairs_mut()
            .append_pair("limit", &limit.to_string());
    }
    let res = client().get(url).timeout(time_limit(QUICK)).send().await.map_err(|e| describe(&e))?;
    if !res.status().is_success() {
        let status = res.status();
        let text = res.text().await.unwrap_or_default();
        return Err(if text.is_empty() {
            status.to_string()
        } else {
            text
        });
    }
    res.json().await.map_err(|e| e.to_string())
}

/// A non-2xx response's body is already a plain-text error message on every
/// mutating endpoint here (`ApiError`'s own `IntoResponse`) — surface it
/// as-is rather than wrapping it in a generic "request failed".
async fn into_result(res: reqwest::Response) -> Result<(), String> {
    if res.status().is_success() {
        return Ok(());
    }
    let status = res.status();
    let text = res.text().await.unwrap_or_default();
    Err(if text.is_empty() {
        status.to_string()
    } else {
        describe_error_body(status, text)
    })
}

/// A `409` body conflict (see the server's `body_conflict`) comes back as
/// JSON; turn it into text a person or an agent can act on — the message,
/// the revision to retry against, and the body as it is now. Any other
/// error body is passed through as it came.
fn describe_error_body(status: reqwest::StatusCode, text: String) -> String {
    if status == reqwest::StatusCode::CONFLICT {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) {
            if value["error"] == "bodyConflict" {
                return format!(
                    "{}\ncurrent revision: {}\ncurrent body:\n{}",
                    value["message"].as_str().unwrap_or("body conflict"),
                    value["currentRev"].as_str().unwrap_or("?"),
                    value["currentText"].as_str().unwrap_or("")
                );
            }
        }
    }
    text
}

fn base_url(port: u16) -> String {
    format!("http://127.0.0.1:{port}")
}

// =======================================================================
// TUI-as-client: canvas load, running, services, whole-file save. Shares
// the `node_url`/`into_result` primitives above with the
// CLI/MCP node-op routing already in this module — one client module for
// every way a frontend here talks to a worker, not one per frontend.
// =======================================================================

/// `GET /api/canvas/raw` — the worker's own in-memory copy of the primary
/// document's raw Markdown text (`state.raw`), unresolved (no `include`
/// splicing) — the same "raw-file-only scope" `App.raw`/`App.canvas`
/// already have. This is what `App::new` loads instead of
/// `std::fs::read_to_string` when a worker is reachable; `display_canvas`
/// (the include-resolved tree) is still built locally from it
/// (`meshfox_core::include::resolve`) —
/// there's no separate "fetch the resolved tree" round trip, since local
/// include-resolution is already needed either way (e.g. for edits that
/// land in an `include` target file).
pub async fn get_canvas_raw(port: u16) -> Result<String, String> {
    let res = client().get(format!("{}/api/canvas/raw", base_url(port))).timeout(time_limit(QUICK)).send()
        .await
        .map_err(|e| describe(&e))?;
    if !res.status().is_success() {
        let status = res.status();
        let text = res.text().await.unwrap_or_default();
        return Err(if text.is_empty() {
            status.to_string()
        } else {
            text
        });
    }
    res.text().await.map_err(|e| e.to_string())
}

/// Why a whole-file `PUT /api/canvas/raw` wrote nothing.
pub enum PutRawError {
    /// The file changed since `base_rev` was read (`412`): its current
    /// revision, to write over it on an explicit second attempt.
    Conflict { current_rev: String },
    /// Anything else (a parse failure's `422`, a transport error, …), as
    /// text for the user.
    Other(String),
}

/// `PUT /api/canvas/raw` — whole-document replace, what the source
/// editor's own `Ctrl-s` save routes through for the primary canvas, or
/// for a canvas-valued include target via that target's worker.
/// A `422` (parse failure) comes back as plain text, same `ApiError`
/// `IntoResponse` every other mutating endpoint here already uses.
///
/// `base_rev` is the `meshfox_core::body_rev` of the file's text as the
/// caller read it, sent as `If-Match`; the server refuses the write if the
/// file has changed since.
pub async fn put_canvas_raw(port: u16, text: &str, base_rev: &str) -> Result<(), PutRawError> {
    let res = client()
        .put(format!("{}/api/canvas/raw", base_url(port)))
        .header("if-match", format!("\"{base_rev}\""))
        .body(text.to_string())
        .timeout(time_limit(WHOLE_DOCUMENT))
        .send()
        .await
        .map_err(|e| PutRawError::Other(describe(&e)))?;
    if res.status() == reqwest::StatusCode::PRECONDITION_FAILED {
        let body = res.text().await.unwrap_or_default();
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&body) {
            if value["error"] == "sourceConflict" {
                return Err(PutRawError::Conflict {
                    current_rev: value["currentRev"].as_str().unwrap_or_default().to_string(),
                });
            }
        }
        return Err(PutRawError::Other(body));
    }
    into_result(res).await.map_err(PutRawError::Other)
}

/// `GET /api/canvas` — the worker's own `include`-resolved tree (same
/// `meshfox_core::include::resolve` the server already runs before serving
/// this, plus constraint-status annotation — see `canvas_response` in
/// `crates/server/src/lib.rs`), unlike `get_canvas_raw`'s unresolved text.
/// A mutating client (the TUI) deliberately avoids this and resolves
/// includes locally instead (see `get_canvas_raw`'s own doc comment) so it
/// still knows which file an edit inside an include target should land in —
/// a read-only export has no such concern, so `meshfox static` uses this
/// directly instead of re-deriving the same resolved tree itself from a raw
/// fetch plus a local `include::resolve` call.
pub async fn get_canvas(port: u16) -> Result<Canvas, String> {
    let res = client().get(format!("{}/api/canvas", base_url(port))).timeout(time_limit(QUICK)).send()
        .await
        .map_err(|e| describe(&e))?;
    if !res.status().is_success() {
        let status = res.status();
        let text = res.text().await.unwrap_or_default();
        return Err(if text.is_empty() {
            status.to_string()
        } else {
            text
        });
    }
    res.json().await.map_err(|e| e.to_string())
}

#[derive(serde::Deserialize)]
struct FileContentResponse {
    content: String,
    truncated: bool,
}

/// `GET /api/nodes/:id/file-content` — a `file`-type node's `display="code"`
/// target, read fresh off disk by the worker and confined to the canvas's
/// own directory (`crates/server/src/lib.rs::get_node_file_content`). Same
/// route the web UI's `FileCodePreview` already fetches live from; `meshfox
/// static`'s worker-routed path calls this once per such node ahead of
/// `staticgen::build_with_code_previews` instead of reading the target off
/// local disk itself.
pub async fn get_node_file_content(port: u16, node_id: &str) -> Result<(String, bool), String> {
    let mut url = node_url(port, node_id)?;
    url.path_segments_mut()
        .map_err(|_| "couldn't build the worker's file-content URL".to_string())?
        .push("file-content");
    let res = client().get(url).timeout(time_limit(QUICK)).send().await.map_err(|e| describe(&e))?;
    if !res.status().is_success() {
        let status = res.status();
        let text = res.text().await.unwrap_or_default();
        return Err(if text.is_empty() {
            status.to_string()
        } else {
            text
        });
    }
    let body: FileContentResponse = res.json().await.map_err(|e| e.to_string())?;
    Ok((body.content, body.truncated))
}

/// A confined file's raw bytes, off the worker's own static-asset fallback
/// route (`serve_canvas_relative_file`/`serve_embedded` in
/// `crates/server/src/lib.rs`, wired as the router's catch-all `fallback`) —
/// the same route a live `meshfox view` tab already resolves a relative
/// Markdown image/`file`-node link against. `path` is relative to the
/// canvas's own directory, forward-slash-separated (`staticgen::Asset::
/// dest_rel`'s own shape) — each segment is percent-encoded individually via
/// `path_segments_mut`, same as `node_url`, so a path component with
/// spaces/non-ASCII round-trips correctly. `meshfox static`'s worker-routed
/// asset-copy step calls this once per `Asset` instead of `std::fs::read`ing
/// `Asset::source` itself.
pub async fn get_relative_file(port: u16, path: &str) -> Result<Vec<u8>, String> {
    let mut url = reqwest::Url::parse(&base_url(port)).map_err(|e| e.to_string())?;
    url.path_segments_mut()
        .map_err(|_| "couldn't build the worker's asset URL".to_string())?
        .clear()
        .extend(path.split('/'));
    let res = client().get(url).timeout(time_limit(QUICK)).send().await.map_err(|e| describe(&e))?;
    if !res.status().is_success() {
        let status = res.status();
        let text = res.text().await.unwrap_or_default();
        return Err(if text.is_empty() {
            status.to_string()
        } else {
            text
        });
    }
    res.bytes()
        .await
        .map(|b| b.to_vec())
        .map_err(|e| e.to_string())
}

/// A confined file's raw bytes, off the worker's own `GET /api/include-asset
/// ?dir=&file=` route (`crates/server/src/lib.rs::get_include_asset`) — the
/// *only* worker route that can serve an asset whose real path lives outside
/// `canvas_dir` (an image referenced from inside an `include`-dumped body
/// whose own directory isn't nested under the primary canvas's own
/// directory), since `get_relative_file`'s plain fallback route is confined
/// to `canvas_dir` and can't reach it. `dir` must be exactly some node's own
/// `Node::asset_base` string, unmodified — the server re-derives the
/// document's current resolved tree and only serves `dir`s that are still
/// one of its nodes' actual `asset_base`s (see that route's own doc
/// comment), so a stale or hand-crafted `dir` 404s rather than reading
/// arbitrary files. `staticgen::Asset::include_asset`, when set, is exactly
/// this `(dir, file)` pair.
pub async fn get_include_asset(port: u16, dir: &str, file: &str) -> Result<Vec<u8>, String> {
    let mut url = reqwest::Url::parse(&format!("{}/api/include-asset", base_url(port)))
        .map_err(|e| e.to_string())?;
    url.query_pairs_mut()
        .append_pair("dir", dir)
        .append_pair("file", file);
    let res = client().get(url).timeout(time_limit(QUICK)).send().await.map_err(|e| describe(&e))?;
    if !res.status().is_success() {
        let status = res.status();
        let text = res.text().await.unwrap_or_default();
        return Err(if text.is_empty() {
            status.to_string()
        } else {
            text
        });
    }
    res.bytes()
        .await
        .map(|b| b.to_vec())
        .map_err(|e| e.to_string())
}

/// One notification off `GET /api/watch` — mirrors
/// `crates/server/src/lib.rs`'s own `ServerEvent`, minus the fields
/// `watch()`'s caller doesn't need (`seq`, used only for a reconnecting
/// browser tab's own backlog replay — TUI always connects with `since`
/// unset, see `watch`'s own doc comment).
#[derive(Debug, Clone)]
pub enum WatchEvent {
    /// The canvas itself changed — go re-`GET /api/canvas/raw`.
    Changed,
    /// A plain-block run started somewhere else (another frontend's manual
    /// run, `force_run`, or a server-triggered `autorun`) — `mod.rs` reacts
    /// by calling `subscribe_run` for this exact address, the same "watch
    /// a run I didn't start" flow the web UI's `watchAutorunBlock` already
    /// has.
    RunStarted { node_id: String, block: String },
    /// A run (plain or `tty`) started or ended somewhere, or the session was
    /// reset: refetch `GET /api/runs`.
    RunsChanged,
}

/// How long a WebSocket from a worker may deliver nothing — not even the
/// worker's own periodic heartbeat (`WS_HEARTBEAT_SECS` in the server crate, 15 s) —
/// before the client treats the connection as lost. Three heartbeats' worth: a
/// run or a shell that is merely quiet still produces heartbeats, so only a hung
/// worker or a half-open connection reaches it. `MESHFOX_WS_SILENCE_SECS`
/// overrides it (test hook).
const WS_SILENCE_SECS: u64 = 45;

pub fn ws_silence_limit() -> Duration {
    time_limit(
        std::env::var("MESHFOX_WS_SILENCE_SECS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .filter(|secs| *secs > 0)
            .map(Duration::from_secs)
            .unwrap_or(Duration::from_secs(WS_SILENCE_SECS)),
    )
}

/// What one read of a worker WebSocket produced.
pub enum WsNext {
    Frame(tokio_tungstenite::tungstenite::Message),
    /// The worker closed the stream (or the connection broke).
    Closed,
    /// Nothing at all — no data, no heartbeat — for `ws_silence_limit()`.
    Silent,
}

/// Reads the next frame, giving up after `ws_silence_limit()` of silence.
/// Every frame, including the worker's heartbeats, restarts the clock, so a
/// stream that is quiet but alive never trips it. Not for use as one arm of
/// a `select!` that gets restarted by its other arms — each restart would
/// reset the clock too; such a loop tracks the time of the last frame itself
/// and compares it with `ws_silence_limit()`.
pub async fn ws_next<S>(ws: &mut S) -> WsNext
where
    S: futures_util::Stream<
            Item = Result<
                tokio_tungstenite::tungstenite::Message,
                tokio_tungstenite::tungstenite::Error,
            >,
        > + Unpin,
{
    use futures_util::StreamExt;
    match tokio::time::timeout(ws_silence_limit(), ws.next()).await {
        Err(_) => WsNext::Silent,
        Ok(Some(Ok(msg))) => WsNext::Frame(msg),
        Ok(Some(Err(_)) | None) => WsNext::Closed,
    }
}

fn worker_silent_message() -> String {
    format!(
        "the worker stopped responding (nothing received for {}s)",
        ws_silence_limit().as_secs()
    )
}

/// `GET /api/watch` — the WS client for canvas-change notifications, the
/// worker-routed replacement for `mod.rs`'s own mtime-poll file watcher
/// (`spawn_file_watcher`) once a worker is reachable. Always connects with
/// `since` unset (start-from-now): unlike a browser tab, TUI has no
/// backlog to resume — every notification just means "go re-`GET
/// /api/canvas/raw`", so there's nothing a missed backlog entry would have
/// added. Reconnects (2s backoff) for as long as the returned receiver
/// stays alive, so a worker restart or a transient drop doesn't silently
/// stop reload notifications for the rest of the session. `"connected"`
/// acks are swallowed here; `"resync"` (a live-tail buffer overrun, not
/// meaningful without a backlog to resync from) is treated the same as
/// `"changed"` — a full reload is always a safe fallback. `"node-
/// upserted"`/`"node-removed"`/`"nodes-reordered"` (see
/// `crates/server/src/lib.rs`'s own `ServerEvent` doc comment) are the web
/// UI's own precise per-operation events, for applying a tree mutation in
/// place instead of reloading — TUI doesn't (yet) have that incremental
/// apply logic of its own, so for now it just treats each of these the
/// same as `"changed"` too: correct (a full reload always picks up
/// whatever they'd have described), just not as smooth. `#[serde(other)]`
/// on `Other` is what keeps a *future* event type this client doesn't
/// know about yet from silently vanishing (a closed enum would fail to
/// deserialize the whole message and skip it via the `let Ok(...) else
/// {{ continue }}` below) — it falls back to the same safe `Changed`
/// reload path everything else here already does.
pub fn watch(port: u16) -> tokio::sync::mpsc::UnboundedReceiver<WatchEvent> {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(async move {
        #[derive(serde::Deserialize)]
        #[serde(
            tag = "type",
            rename_all = "kebab-case",
            rename_all_fields = "camelCase"
        )]
        enum WatchMsg {
            Connected,
            Heartbeat,
            Changed,
            Resync,
            RunStarted {
                node_id: String,
                block: String,
            },
            RunsChanged,
            NodeUpserted,
            NodeRemoved,
            NodesReordered,
            #[serde(other)]
            Other,
        }
        loop {
            let url = format!("ws://127.0.0.1:{port}/api/watch");
            let Ok((mut ws, _)) = tokio_tungstenite::connect_async(&url).await else {
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                continue;
            };
            // A silent connection is dropped and re-established below, the
            // same as one the worker closed.
            while let WsNext::Frame(msg) = ws_next(&mut ws).await {
                let tokio_tungstenite::tungstenite::Message::Text(text) = msg else {
                    continue;
                };
                let Ok(parsed) = serde_json::from_str::<WatchMsg>(&text) else {
                    continue;
                };
                let event = match parsed {
                    WatchMsg::Connected | WatchMsg::Heartbeat => continue,
                    WatchMsg::Changed
                    | WatchMsg::Resync
                    | WatchMsg::NodeUpserted
                    | WatchMsg::NodeRemoved
                    | WatchMsg::NodesReordered
                    | WatchMsg::Other => WatchEvent::Changed,
                    WatchMsg::RunStarted { node_id, block } => {
                        WatchEvent::RunStarted { node_id, block }
                    }
                    WatchMsg::RunsChanged => WatchEvent::RunsChanged,
                };
                if tx.send(event).is_err() {
                    return;
                }
            }
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        }
    });
    rx
}

/// One line of `/api/run/subscribe`'s streamed NDJSON response — mirrors
/// `crates/server/src/lib.rs`'s own `SubscribeEvent`: a much smaller
/// vocabulary than `RunEvent` (no `StepStart`/chain concepts at all),
/// since this watches exactly one address's own run independent of
/// whatever request originally started it.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum SubscribeEvent {
    Line { stream: OutputStream, text: String },
    Done { exit_code: Option<i32> },
}

/// `GET /api/run/subscribe?nodeId=..&block=..` — watches one address's own
/// most recent run independent of whoever started it (a passive tab/TUI
/// session reacting to `WatchEvent::RunStarted`, mirroring the web UI's
/// own `watchAutorunBlock`). A WebSocket, not an HTTP-streamed response —
/// replays every buffered line, then tails live output until the run's own
/// terminal outcome, at which point the socket closes. `since_seq` is
/// always `0` here (unlike the web UI, which reconnects with its own
/// last-seen `seq` — TUI only ever opens one subscription per run, ended by
/// the server's own terminal event, so there's no reconnect case to
/// resume). This address never having run at all (or the reservation
/// racing ahead of `runs_registry` somehow) is a deliberate silent no-op
/// server-side (the socket upgrades, then closes immediately with zero
/// messages) — indistinguishable here from a connect failure, and handled
/// the same way: just an empty channel, best-effort, same as the web UI's
/// own `.catch()` on this call.
pub fn subscribe_run(
    port: u16,
    node_id: String,
    block: String,
) -> tokio::sync::mpsc::UnboundedReceiver<SubscribeEvent> {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(async move {
        let url = reqwest::Url::parse_with_params(
            &format!("ws://127.0.0.1:{port}/api/run/subscribe"),
            &[("nodeId", node_id.as_str()), ("block", block.as_str())],
        );
        let Ok(url) = url else { return };
        let Ok((mut ws, _)) = tokio_tungstenite::connect_async(url.as_str()).await else {
            return;
        };
        loop {
            let msg = match ws_next(&mut ws).await {
                WsNext::Frame(msg) => msg,
                WsNext::Closed => return,
                // The run's own end never arrived: report the stream as
                // ended abnormally (exit code unknown) rather than leave the
                // block showing "running" for ever.
                WsNext::Silent => {
                    let _ = tx.send(SubscribeEvent::Done { exit_code: None });
                    return;
                }
            };
            let tokio_tungstenite::tungstenite::Message::Text(text) = msg else {
                continue;
            };
            let Ok(event) = serde_json::from_str::<SubscribeEvent>(&text) else {
                continue;
            };
            if tx.send(event).is_err() {
                return;
            }
        }
    });
    rx
}

/// One finished run of a block, as `GET /api/run/history` lists it — mirrors
/// `crates/server/src/run_ledger.rs`'s `RunSummary`. `stale` means the run no
/// longer describes the document (its block, something it depends on, or a
/// variable it used changed since, or the session was reset), not that it
/// failed.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunHistoryEntryDto {
    pub id: i64,
    /// `"exited"` or `"killed"`.
    pub outcome: String,
    pub exit_code: Option<i32>,
    pub started_at: String,
    pub duration_ms: Option<u64>,
    pub stale: bool,
}

/// `GET /api/run/history?nodeId=..&block=..` — the finished runs of one
/// block the worker's session database still keeps (`[session]
/// max_runs_per_block`), newest first.
pub async fn run_history(
    port: u16,
    node_id: &str,
    block: &str,
) -> Result<Vec<RunHistoryEntryDto>, String> {
    let url = reqwest::Url::parse_with_params(
        &format!("{}/api/run/history", base_url(port)),
        &[("nodeId", node_id), ("block", block)],
    )
    .map_err(|e| e.to_string())?;
    let res = client().get(url).timeout(time_limit(QUICK)).send().await.map_err(|e| describe(&e))?;
    if !res.status().is_success() {
        let status = res.status();
        let text = res.text().await.unwrap_or_default();
        return Err(if text.is_empty() {
            status.to_string()
        } else {
            text
        });
    }
    res.json().await.map_err(|e| e.to_string())
}

/// The stored output of one historical run — `/api/run/subscribe` with a
/// `runId`, read to its end. A finished run replays its lines and closes, so
/// this simply collects them; a run the worker no longer has (rotated out
/// since the list was fetched) closes without a message and comes back as an
/// empty output.
pub async fn run_output(
    port: u16,
    node_id: &str,
    block: &str,
    run_id: i64,
) -> Result<Vec<(OutputStream, String)>, String> {
    let url = reqwest::Url::parse_with_params(
        &format!("ws://127.0.0.1:{port}/api/run/subscribe"),
        &[
            ("nodeId", node_id),
            ("block", block),
            ("runId", &run_id.to_string()),
        ],
    )
    .map_err(|e| e.to_string())?;
    let (mut ws, _) = tokio_tungstenite::connect_async(url.as_str())
        .await
        .map_err(|e| e.to_string())?;
    let mut lines = Vec::new();
    loop {
        let msg = match ws_next(&mut ws).await {
            WsNext::Frame(msg) => msg,
            WsNext::Closed => break,
            WsNext::Silent => return Err(worker_silent_message()),
        };
        let tokio_tungstenite::tungstenite::Message::Text(text) = msg else {
            continue;
        };
        match serde_json::from_str::<SubscribeEvent>(&text) {
            Ok(SubscribeEvent::Line { stream, text }) => lines.push((stream, text)),
            Ok(SubscribeEvent::Done { .. }) => break,
            Err(_) => {}
        }
    }
    Ok(lines)
}

/// One declared `meshfox:var`'s current status — mirrors
/// `crates/server/src/lib.rs`'s own `VarStatus`/`VarOrigin` wire shape
/// (`GET /api/vars`), the same pre-run gate the web UI's `handleRun`
/// already checks before ever calling `POST /api/run`.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VarStatus {
    pub name: String,
    #[serde(rename = "type")]
    pub var_type: String,
    pub prompt: String,
    #[serde(default)]
    pub choices: Vec<String>,
    pub secret: bool,
    pub resolved: bool,
    #[serde(default)]
    pub value: Option<String>,
    #[serde(default)]
    pub inherited_from: Option<VarOrigin>,
    /// `"plaintext"` or `"keychain"` for a `secret` field — where saving it
    /// would put the value.
    #[serde(default)]
    pub secret_store: Option<String>,
    /// Why the secret store couldn't be read for this field, if it failed.
    #[serde(default)]
    pub secret_error: Option<String>,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(tag = "scope", rename_all = "camelCase")]
pub enum VarOrigin {
    Project,
    Global { path: Option<String> },
}

/// `GET /api/vars?path=..&block=..&noDeps=..` — `path` is the node-id path
/// joined with commas, empty for a root-level block (matches
/// `VarsQuery::path`'s own "comma-joined" convention server-side).
pub async fn get_vars(
    port: u16,
    path: &[String],
    block: &str,
    no_deps: bool,
) -> Result<Vec<VarStatus>, String> {
    let url = reqwest::Url::parse_with_params(
        &format!("{}/api/vars", base_url(port)),
        &[
            ("path", path.join(",")),
            ("block", block.to_string()),
            ("noDeps", no_deps.to_string()),
        ],
    )
    .map_err(|e| e.to_string())?;
    let res = client().get(url).timeout(time_limit(QUICK)).send().await.map_err(|e| describe(&e))?;
    if !res.status().is_success() {
        let status = res.status();
        let text = res.text().await.unwrap_or_default();
        return Err(if text.is_empty() {
            status.to_string()
        } else {
            text
        });
    }
    res.json().await.map_err(|e| e.to_string())
}

/// `GET /api/vars/configure` — every declared non-secret, non-session,
/// non-`from=` variable in the whole document, the worker-routed
/// counterpart to `App::trigger_configure`'s local-mode branch (which
/// reads `self.decls`/`self.var_cache` directly). Unlike `get_vars`, never
/// scoped to one block's own chain.
pub async fn get_configure_vars(port: u16) -> Result<Vec<VarStatus>, String> {
    let res = client().get(format!("{}/api/vars/configure", base_url(port))).timeout(time_limit(QUICK)).send()
        .await
        .map_err(|e| describe(&e))?;
    if !res.status().is_success() {
        let status = res.status();
        let text = res.text().await.unwrap_or_default();
        return Err(if text.is_empty() {
            status.to_string()
        } else {
            text
        });
    }
    res.json().await.map_err(|e| e.to_string())
}

/// `POST /api/vars/configure` — writes every entry in `vars` naming a
/// declared non-secret variable to the worker's own on-disk cache
/// (`state.vars_cache`), *even if unchanged* from what was already there
/// — same as `meshfox configure`/the local-mode branch this replaces in
/// worker mode. Doesn't run anything. A `422` (an invalid value for its
/// declared type) comes back as plain text, same `ApiError` posture every
/// other mutating endpoint here already has.
pub async fn post_configure_vars(
    port: u16,
    vars: HashMap<String, String>,
) -> Result<usize, String> {
    let res = client()
        .post(format!("{}/api/vars/configure", base_url(port)))
        .json(&serde_json::json!({ "vars": vars }))
        .timeout(time_limit(QUICK))
        .send()
        .await
        .map_err(|e| describe(&e))?;
    if !res.status().is_success() {
        let status = res.status();
        let text = res.text().await.unwrap_or_default();
        return Err(if text.is_empty() {
            status.to_string()
        } else {
            text
        });
    }
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Response {
        saved: usize,
    }
    let response: Response = res.json().await.map_err(|e| e.to_string())?;
    Ok(response.saved)
}

/// `POST /api/form/submit` — the worker-routed counterpart to
/// `App::submit_inline_form`'s local-mode branch: persists `values` into
/// the worker's own `state.session_vars` and runs every `autorun` block
/// they reach (`crates/server/src/lib.rs`'s own `trigger_autorun`) before
/// this call returns, rather than the TUI trying to run them itself the
/// way local mode does — a form-triggered run must happen where
/// `state.session_vars` actually lives, or the run server-side would still
/// see the *old* value (or none at all). Returns every triggered
/// `(node_id, block)` — this session's own `spawn_worker_watcher`/
/// `App::on_external_run_event` (already built to show *any* passively-
/// discovered run's live output, not just an autorun's) picks each one up
/// via the same `WatchEvent::RunStarted` a different tab/TUI submitting
/// the exact same form would rely on, so nothing further needs doing with
/// this return value beyond a status message — unlike the web UI's own
/// `handleSubmitForm`, which calls `subscribeRun` on it directly for
/// slightly lower latency than waiting on the `/api/watch` round-trip.
pub async fn submit_form(
    port: u16,
    node_id: &str,
    block: &str,
    values: HashMap<String, String>,
) -> Result<Vec<(String, String)>, String> {
    let res = client()
        .post(format!("{}/api/form/submit", base_url(port)))
        .json(&serde_json::json!({ "nodeId": node_id, "block": block, "values": values }))
        .timeout(time_limit(QUICK))
        .send()
        .await
        .map_err(|e| describe(&e))?;
    if !res.status().is_success() {
        let status = res.status();
        let text = res.text().await.unwrap_or_default();
        return Err(if text.is_empty() {
            status.to_string()
        } else {
            text
        });
    }
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Triggered {
        node_id: String,
        block: String,
    }
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Response {
        autorun_triggered: Vec<Triggered>,
    }
    let response: Response = res.json().await.map_err(|e| e.to_string())?;
    Ok(response
        .autorun_triggered
        .into_iter()
        .map(|t| (t.node_id, t.block))
        .collect())
}

/// Mirrors `crates/server/src/lib.rs`'s own `RunEvent` — the WebSocket
/// vocabulary `GET /api/run`'s streamed response speaks (one `Message::
/// Text` per event). `stream` reuses `meshfox_server::stream_exec::
/// OutputStream` directly (already the exact type the server itself
/// serializes there) rather than a second copy of the same two-variant
/// enum. Every variant's fields mirror the wire shape exactly, even ones no
/// current consumer reads (`Started::run_id`, most variants' own `node_id`/
/// `block` once `App::on_run_event`/`print_tty_transcript_event` only need
/// a handful) — trimming fields a future consumer would want back out just
/// to silence `dead_code` isn't worth it for a type whose only job is
/// matching the server's own shape. `LockConflict` is the one variant with
/// no local-mode/`RunState` equivalent at all — see `run_stream`'s own doc
/// comment on why it arrives as a stream event rather than a connect-time
/// error.
#[allow(dead_code)]
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(
    tag = "type",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase"
)]
pub enum RunEvent {
    Started {
        run_id: String,
    },
    StepStart {
        node_id: String,
        block: String,
    },
    StepSkipped {
        node_id: String,
        block: String,
        output: String,
        duration_ms: u64,
    },
    Output {
        node_id: String,
        block: String,
        stream: OutputStream,
        text: String,
    },
    TtyStart {
        node_id: String,
        block: String,
    },
    ServiceStarted {
        node_id: String,
        block: String,
        pid: u32,
    },
    LockConflict {
        node_id: String,
        block: String,
        owner_pid: u32,
        owner_desc: String,
    },
    StepEnd {
        node_id: String,
        block: String,
        exit_code: i32,
        duration_ms: u64,
    },
    Killed {
        node_id: String,
        block: String,
    },
    Error {
        message: String,
    },
    Done {
        exit_code: i32,
    },
}

/// A `409` from `POST /api/run/tty`/`force-start` — mirrors
/// `crates/server/src/lib.rs`'s own `LockConflict` struct, reported before
/// any streaming ever starts there (queued-time locking checks every
/// address a chain will touch up front — see that struct's own doc
/// comment). `run_stream`'s own conflict instead arrives as a
/// `RunEvent::LockConflict` stream event (same fields) — this struct is
/// only still needed for `tty_connect_error`'s pre-upgrade `409`, which
/// `/api/run/tty` still uses (out of scope for the `/api/run` WS
/// conversion — see TODO.canvas.md).
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LockConflict {
    pub node_id: String,
    pub block: String,
    pub owner_pid: u32,
    pub owner_desc: String,
}

/// `GET /api/run` (or `GET /api/run/force` when `force` is `Some`) —
/// starts a whole chain run server-side and streams it back over a
/// WebSocket as `RunEvent`s, forwarded one at a time on the returned
/// channel by a background task (so the caller can `select!`/`recv()` it
/// the same way `RunState::proc`'s own `output_rx` already works, rather
/// than holding the socket open on the caller's own poll). The channel
/// closes (returns `None`) once a terminal event (`Done`/`Killed`/`Error`)
/// has been forwarded, or if the connection drops early — a caller that
/// cares about the difference should have already seen the terminal event
/// by then in the normal case.
///
/// Unlike the old HTTP-chunked version, a lock conflict is no longer a
/// connect-time error at all — a browser's native `WebSocket` can't read a
/// failed handshake's own status/body, so the server always completes the
/// upgrade and reports a conflict as the very first streamed
/// `RunEvent::LockConflict` instead (see `crates/server/src/lib.rs`'s own
/// `pump_run_response_into_ws` doc comment). This function can now only
/// fail on a genuine transport-level problem (the worker isn't listening,
/// a malformed URL) — the caller (`App::begin_http_run`) is the one that
/// inspects the very first event on the returned channel for `LockConflict`.
///
/// Always `persist: false` on the wire — see [`run_stream_persisted`] for
/// the CLI's own `persist: true` variant and why the two callers differ.
pub async fn run_stream(
    port: u16,
    path: &[String],
    block: &str,
    no_deps: bool,
    vars: HashMap<String, String>,
    save_secrets: HashSet<String>,
    force: Option<(String, String)>,
) -> Result<tokio::sync::mpsc::UnboundedReceiver<RunEvent>, String> {
    run_stream_inner(port, path, block, no_deps, false, false, vars, save_secrets, force).await
}

/// Same as [`run_stream`], but with `persist: true` — the CLI's own
/// `run_via_worker` (`crates/cli/src/main.rs`) needs this so a `cache`
/// block's output actually lands back in the file the way `meshfox run`
/// has always guaranteed, in-process or not; TUI's own `run_stream` calls
/// stay on the persist-`false` default above (a TUI run is closer to the
/// web UI's own preview-first posture — explicit "save to file" is its own
/// separate action there, not implied by every run).
#[allow(clippy::too_many_arguments)]
pub async fn run_stream_persisted(
    port: u16,
    path: &[String],
    block: &str,
    no_deps: bool,
    fresh: bool,
    vars: HashMap<String, String>,
    save_secrets: HashSet<String>,
    force: Option<(String, String)>,
) -> Result<tokio::sync::mpsc::UnboundedReceiver<RunEvent>, String> {
    run_stream_inner(port, path, block, no_deps, fresh, true, vars, save_secrets, force).await
}

#[allow(clippy::too_many_arguments)]
async fn run_stream_inner(
    port: u16,
    path: &[String],
    block: &str,
    no_deps: bool,
    fresh: bool,
    persist: bool,
    vars: HashMap<String, String>,
    save_secrets: HashSet<String>,
    force: Option<(String, String)>,
) -> Result<tokio::sync::mpsc::UnboundedReceiver<RunEvent>, String> {
    let vars_json = serde_json::to_string(&vars).map_err(|e| e.to_string())?;
    let secrets_json = serde_json::to_string(&save_secrets).map_err(|e| e.to_string())?;
    let route = if force.is_some() {
        "/api/run/force"
    } else {
        "/api/run"
    };
    let mut params = vec![
        ("path", path.join(",")),
        ("block", block.to_string()),
        ("noDeps", no_deps.to_string()),
        ("fresh", fresh.to_string()),
        ("persist", persist.to_string()),
        ("vars", vars_json),
        ("saveSecrets", secrets_json),
    ];
    if let Some((force_node_id, force_block)) = &force {
        params.push(("forceNodeId", force_node_id.clone()));
        params.push(("forceBlock", force_block.clone()));
    }
    let url = reqwest::Url::parse_with_params(&format!("ws://127.0.0.1:{port}{route}"), &params)
        .map_err(|e| e.to_string())?;
    let (mut ws, _) = tokio_tungstenite::connect_async(url.as_str())
        .await
        .map_err(|e| e.to_string())?;

    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(async move {
        loop {
            let msg = match ws_next(&mut ws).await {
                WsNext::Frame(msg) => msg,
                WsNext::Closed => return,
                WsNext::Silent => {
                    let _ = tx.send(RunEvent::Error {
                        message: worker_silent_message(),
                    });
                    return;
                }
            };
            let tokio_tungstenite::tungstenite::Message::Text(text) = msg else {
                continue;
            };
            let Ok(event) = serde_json::from_str::<RunEvent>(&text) else {
                continue;
            };
            if tx.send(event).is_err() {
                return;
            }
        }
    });
    Ok(rx)
}

/// `GET /api/nodes/:id/run` — the file-node counterpart to [`run_stream`]/
/// [`run_stream_persisted`]: runs a runnable `file` node's own `interpreter
/// target` and streams the exact same `RunEvent` vocabulary a fenced-block
/// chain does (`crates/server/src/lib.rs`'s own `run_file_node` doc comment
/// — `nodeId`/`block` both set to the node's own id, no `deps=`/`cache`/
/// `env=`/`tty` concepts to worry about here, so no query params at all).
/// Used by `crate::run_via_worker`'s own fallback for exactly the case a
/// fenced-block chain's own `get_vars`/`run_stream_persisted` can't
/// address: `path`+`name` naming a node whose body is just a link to an
/// external target rather than a fenced block.
pub async fn run_file_node_stream(
    port: u16,
    node_id: &str,
) -> Result<tokio::sync::mpsc::UnboundedReceiver<RunEvent>, String> {
    let mut url = reqwest::Url::parse(&format!("ws://127.0.0.1:{port}/api/nodes"))
        .map_err(|e| e.to_string())?;
    url.path_segments_mut()
        .map_err(|_| "couldn't build the worker's node-run URL".to_string())?
        .push(node_id)
        .push("run");
    let (mut ws, _) = tokio_tungstenite::connect_async(url.as_str())
        .await
        .map_err(|e| e.to_string())?;

    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(async move {
        loop {
            let msg = match ws_next(&mut ws).await {
                WsNext::Frame(msg) => msg,
                WsNext::Closed => return,
                WsNext::Silent => {
                    let _ = tx.send(RunEvent::Error {
                        message: worker_silent_message(),
                    });
                    return;
                }
            };
            let tokio_tungstenite::tungstenite::Message::Text(text) = msg else {
                continue;
            };
            let Ok(event) = serde_json::from_str::<RunEvent>(&text) else {
                continue;
            };
            if tx.send(event).is_err() {
                return;
            }
        }
    });
    Ok(rx)
}

/// `POST /api/kill` — cancels whatever's currently registered for this
/// address (see `crates/server/src/lib.rs`'s own `KillRequest` doc
/// comment: usable even by a caller that never itself started the run).
/// `POST /api/session/reset` — forgets every block's session-freshness
/// record and submitted `form` values (`reset_session` on the server side;
/// the web UI's "reset session" button), so the next chain run executes every
/// block for real. Finished runs stay as history; the canvas file and any
/// saved `<!-- meshfox:output -->` cache are never touched.
pub async fn reset_session(port: u16) -> Result<(), String> {
    let res = client()
        .post(format!("{}/api/session/reset", base_url(port)))
        .timeout(time_limit(CONTROL))
        .send()
        .await
        .map_err(|e| describe(&e))?;
    into_result(res).await
}

pub async fn kill_run(port: u16, node_id: &str, block: &str) -> Result<(), String> {
    let res = client()
        .post(format!("{}/api/kill", base_url(port)))
        .json(&serde_json::json!({ "nodeId": node_id, "block": block }))
        .timeout(time_limit(CONTROL))
        .send()
        .await
        .map_err(|e| describe(&e))?;
    into_result(res).await
}

/// A live `/api/run/tty`/`/api/run/tty/attach` connection — binary frames
/// are raw pty bytes each direction, text frames are a `RunEvent` (server
/// to client, before and interleaved around the actual pty relay) or a
/// `{"cols":..,"rows":..}` resize (client to server, only meaningful once
/// `RunEvent::TtyStart` has arrived). See `crate::tui::mod`'s own
/// `bridge_http_tty` for the actual byte-relay loop this feeds.
pub type TtySocket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

#[derive(Debug)]
pub enum TtyConnectError {
    /// A `409` — same meaning as a plain run's `RunEvent::LockConflict`,
    /// just returned pre-upgrade as a plain HTTP response rather than a
    /// streamed event (`/api/run/tty` keeps its own older pre-upgrade-`409`
    /// shape — out of scope for the `/api/run` WS conversion, see
    /// TODO.canvas.md). Retried the same way a plain run's conflict is —
    /// `tty_connect`'s own `force` parameter, set to the exact `(nodeId,
    /// block)` this conflict names.
    Conflict(LockConflict),
    Other(String),
}

impl std::fmt::Display for TtyConnectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TtyConnectError::Conflict(c) => write!(
                f,
                "{:?}/{:?} is locked by pid {} ({})",
                c.node_id, c.block, c.owner_pid, c.owner_desc
            ),
            TtyConnectError::Other(e) => write!(f, "{e}"),
        }
    }
}

/// Turns a failed `connect_async` into a `TtyConnectError` — used by
/// `tty_connect`. A non-101 upgrade response comes back as
/// `tungstenite::Error::Http`, carrying the original HTTP response
/// (status + body) rather than a transport-level failure.
fn tty_connect_error(err: tokio_tungstenite::tungstenite::Error) -> TtyConnectError {
    let tokio_tungstenite::tungstenite::Error::Http(resp) = err else {
        return TtyConnectError::Other(err.to_string());
    };
    let body = resp.body().clone().unwrap_or_default();
    if resp.status().as_u16() == 409 {
        match serde_json::from_slice::<LockConflict>(&body) {
            Ok(conflict) => return TtyConnectError::Conflict(conflict),
            Err(e) => return TtyConnectError::Other(e.to_string()),
        }
    }
    let text = String::from_utf8_lossy(&body).into_owned();
    TtyConnectError::Other(if text.is_empty() {
        resp.status().to_string()
    } else {
        text
    })
}

/// `GET /api/run/tty` — the WS client that starts (and, for the duration of
/// its own pty step, owns) an interactive `tty` chain run. `path`/`block`/
/// `no_deps`/`vars`/`save_secrets` mean exactly what they do for
/// `run_stream`; `cols`/`rows` seed the pty's *initial* size (the real
/// terminal's current size — later resizes go over this same socket as a
/// `{"cols":..,"rows":..}` text frame, see `TtySocket`'s own doc comment).
/// `force`, when given, names the exact `(nodeId, block)` a prior
/// `TtyConnectError::Conflict` reported — same `forceNodeId`/`forceBlock`
/// pair `run_stream`'s own `force` sends to `/api/run/force`, just as query
/// params on this same endpoint instead of a separate one (see
/// `TtyRunQuery`'s own doc comment on the server side for why).
#[allow(clippy::too_many_arguments)]
pub async fn tty_connect(
    port: u16,
    path: &[String],
    block: &str,
    no_deps: bool,
    fresh: bool,
    vars: HashMap<String, String>,
    save_secrets: HashSet<String>,
    cols: u16,
    rows: u16,
    force: Option<(String, String)>,
) -> Result<TtySocket, TtyConnectError> {
    let vars_json =
        serde_json::to_string(&vars).map_err(|e| TtyConnectError::Other(e.to_string()))?;
    let secrets_json =
        serde_json::to_string(&save_secrets).map_err(|e| TtyConnectError::Other(e.to_string()))?;
    let mut params = vec![
        ("path", path.join(",")),
        ("block", block.to_string()),
        ("noDeps", no_deps.to_string()),
        ("fresh", fresh.to_string()),
        ("vars", vars_json),
        ("saveSecrets", secrets_json),
        ("cols", cols.to_string()),
        ("rows", rows.to_string()),
    ];
    if let Some((force_node_id, force_block)) = &force {
        params.push(("forceNodeId", force_node_id.clone()));
        params.push(("forceBlock", force_block.clone()));
    }
    let url =
        reqwest::Url::parse_with_params(&format!("ws://127.0.0.1:{port}/api/run/tty"), &params)
            .map_err(|e| TtyConnectError::Other(e.to_string()))?;
    let (socket, _) = tokio_tungstenite::connect_async(url.as_str())
        .await
        .map_err(tty_connect_error)?;
    Ok(socket)
}

/// `GET /api/run/tty/attach?nodeId=..&block=..&cols=..&rows=..` — joins an
/// *already-running* `tty` session as an additional viewer, without
/// starting anything (see `crates/server/src/lib.rs`'s own `attach_tty`) —
/// the counterpart to `tty_connect` for a session discovered via
/// `list_active_runs` rather than one this call itself starts. The socket
/// is already mid-session the moment it opens: no `RunEvent` prelude at
/// all (unlike `tty_connect`'s pre-tty transcript phase) — the very first
/// frame is either the session's still-buffered byte history (binary) or,
/// if it's already finished, an immediate close. `404` (`TtyConnectError::
/// Other`, via `tty_connect_error`) if this address has no live session
/// right now; never `Conflict` — attach has no lock to contend for.
pub async fn tty_attach(
    port: u16,
    node_id: &str,
    block: &str,
    cols: u16,
    rows: u16,
) -> Result<TtySocket, TtyConnectError> {
    let url = reqwest::Url::parse_with_params(
        &format!("ws://127.0.0.1:{port}/api/run/tty/attach"),
        &[
            ("nodeId", node_id.to_string()),
            ("block", block.to_string()),
            ("cols", cols.to_string()),
            ("rows", rows.to_string()),
        ],
    )
    .map_err(|e| TtyConnectError::Other(e.to_string()))?;
    let (socket, _) = tokio_tungstenite::connect_async(url.as_str())
        .await
        .map_err(tty_connect_error)?;
    Ok(socket)
}

/// One `GET /api/runs` entry — mirrors `crates/server/src/lib.rs`'s own
/// `ActiveRunDto`: every plain-block run and `tty` session this worker
/// process currently knows about, regardless of which connection (if any)
/// started or is watching it. `kind` is `"plain"` or `"tty"`, `status` is
/// `"running"`/`"exited"`/`"killed"` — used here to build the `t` live-
/// terminals view's own list (filtered to `kind == "tty" && status ==
/// "running"`, same as the web UI's `TtySessionsPanel` filters it).
/// `exit_code` is kept for wire-shape parity with the server's own DTO
/// even though it's always `None` on every entry the `t` view actually
/// keeps (a `"running"` session has no exit code yet) — dropping it would
/// just mean re-adding it later if a future caller ever wants the
/// unfiltered list.
#[allow(dead_code)]
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActiveRunDto {
    pub node_id: String,
    pub block: String,
    pub kind: String,
    pub status: String,
    #[serde(default)]
    pub exit_code: Option<i32>,
    pub uptime_ms: u64,
}

pub async fn list_active_runs(port: u16) -> Result<Vec<ActiveRunDto>, String> {
    let res = client().get(format!("{}/api/runs", base_url(port))).timeout(time_limit(QUICK)).send()
        .await
        .map_err(|e| describe(&e))?;
    if !res.status().is_success() {
        let status = res.status();
        let text = res.text().await.unwrap_or_default();
        return Err(if text.is_empty() {
            status.to_string()
        } else {
            text
        });
    }
    res.json().await.map_err(|e| e.to_string())
}

/// One `GET /api/services` entry — mirrors `crates/server/src/lib.rs`'s own
/// `ServiceDto`. Address-keyed, data-only — driving stop/restart/force-start
/// needs nothing from this beyond `node_id`/`block`, no local handle object
/// the way `services::ServiceHandle` used to be. `uptime_ms`/`cpu_percent`/
/// `mem_bytes` aren't shown anywhere in the TUI's own services view yet
/// (see `ui::render_services_view`) — kept anyway since dropping them would
/// mean re-adding them later just to match the wire shape again.
#[allow(dead_code)]
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServiceDto {
    pub node_id: String,
    pub block: String,
    pub status: String,
    #[serde(default)]
    pub exit_code: Option<i32>,
    pub pid: u32,
    pub uptime_ms: u64,
    #[serde(default)]
    pub cpu_percent: Option<f32>,
    #[serde(default)]
    pub mem_bytes: Option<u64>,
}

pub async fn list_services(port: u16) -> Result<Vec<ServiceDto>, String> {
    let res = client().get(format!("{}/api/services", base_url(port))).timeout(time_limit(QUICK)).send()
        .await
        .map_err(|e| describe(&e))?;
    if !res.status().is_success() {
        let status = res.status();
        let text = res.text().await.unwrap_or_default();
        return Err(if text.is_empty() {
            status.to_string()
        } else {
            text
        });
    }
    res.json().await.map_err(|e| e.to_string())
}

/// `GET /api/services/log?nodeId=..&block=..` — the worker-routed
/// equivalent of `ServiceHandle::log_snapshot()`, polled on demand (see
/// `App::refresh_service_log`) rather than streamed, same as the server's
/// own doc comment on `get_service_log` explains for why the web UI does
/// the same.
pub async fn get_service_log(
    port: u16,
    node_id: &str,
    block: &str,
) -> Result<Vec<(OutputStream, String)>, String> {
    let url = reqwest::Url::parse_with_params(
        &format!("{}/api/services/log", base_url(port)),
        &[("nodeId", node_id), ("block", block)],
    )
    .map_err(|e| e.to_string())?;
    let res = client().get(url).timeout(time_limit(QUICK)).send().await.map_err(|e| describe(&e))?;
    if !res.status().is_success() {
        let status = res.status();
        let text = res.text().await.unwrap_or_default();
        return Err(if text.is_empty() {
            status.to_string()
        } else {
            text
        });
    }
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct ServiceLogLine {
        stream: OutputStream,
        text: String,
    }
    let lines: Vec<ServiceLogLine> = res.json().await.map_err(|e| e.to_string())?;
    Ok(lines.into_iter().map(|l| (l.stream, l.text)).collect())
}

pub async fn stop_service(port: u16, node_id: &str, block: &str) -> Result<(), String> {
    let res = client()
        .post(format!("{}/api/services/stop", base_url(port)))
        .json(&serde_json::json!({ "nodeId": node_id, "block": block }))
        .timeout(time_limit(CONTROL))
        .send()
        .await
        .map_err(|e| describe(&e))?;
    into_result(res).await
}

/// Returns the restarted instance's own pid (`ServiceActionResponse::pid`).
pub async fn restart_service(port: u16, node_id: &str, block: &str) -> Result<u32, String> {
    let res = client()
        .post(format!("{}/api/services/restart", base_url(port)))
        .json(&serde_json::json!({ "nodeId": node_id, "block": block }))
        .timeout(time_limit(CONTROL))
        .send()
        .await
        .map_err(|e| describe(&e))?;
    if !res.status().is_success() {
        let status = res.status();
        let text = res.text().await.unwrap_or_default();
        return Err(if text.is_empty() {
            status.to_string()
        } else {
            text
        });
    }
    #[derive(serde::Deserialize)]
    struct Resp {
        pid: u32,
    }
    res.json::<Resp>()
        .await
        .map(|r| r.pid)
        .map_err(|e| e.to_string())
}

// =======================================================================
// Debug sessions — `mcp.rs`'s `DebugHandle::Remote` routes here once
// `coordinator::resolve` finds a live worker, instead of owning a
// `meshfox_server::debug_session::DebugSession` directly. See
// `crates/server/src/lib.rs`'s `/api/debug/*` handlers for the exact wire
// shape these mirror.
// =======================================================================

/// `POST /api/debug/start` — `(session_id, block_name, cwd)`. `node_id`
/// isn't returned separately since the caller already has it (it's an
/// input, echoed back unchanged server-side).
pub async fn debug_start(
    port: u16,
    node_id: &str,
    block_name: Option<&str>,
    vars: HashMap<String, String>,
) -> Result<(String, Option<String>, String), String> {
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Response {
        session_id: String,
        block_name: Option<String>,
        cwd: String,
    }
    let res = client()
        .post(format!("{}/api/debug/start", base_url(port)))
        .json(&serde_json::json!({ "nodeId": node_id, "blockName": block_name, "vars": vars }))
        .timeout(time_limit(QUICK))
        .send()
        .await
        .map_err(|e| describe(&e))?;
    if !res.status().is_success() {
        let status = res.status();
        let text = res.text().await.unwrap_or_default();
        return Err(if text.is_empty() {
            status.to_string()
        } else {
            text
        });
    }
    let response: Response = res.json().await.map_err(|e| e.to_string())?;
    Ok((response.session_id, response.block_name, response.cwd))
}

/// Mirrors `crates/server/src/lib.rs`'s own `DebugSendResponse` exactly.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DebugSendOutcome {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
    pub timed_out: bool,
    pub session_ended: bool,
}

/// `POST /api/debug/send`.
pub async fn debug_send(
    port: u16,
    session_id: &str,
    code: &str,
    timeout_ms: u64,
) -> Result<DebugSendOutcome, String> {
    let res = client()
        .post(format!("{}/api/debug/send", base_url(port)))
        .json(
            &serde_json::json!({ "sessionId": session_id, "code": code, "timeoutMs": timeout_ms }),
        )
        .timeout(time_limit(debug_send_limit(timeout_ms)))
        .send()
        .await
        .map_err(|e| describe(&e))?;
    if !res.status().is_success() {
        let status = res.status();
        let text = res.text().await.unwrap_or_default();
        return Err(if text.is_empty() {
            status.to_string()
        } else {
            text
        });
    }
    res.json().await.map_err(|e| e.to_string())
}

/// `POST /api/debug/stop`.
pub async fn debug_stop(port: u16, session_id: &str) -> Result<(), String> {
    let res = client()
        .post(format!("{}/api/debug/stop", base_url(port)))
        .json(&serde_json::json!({ "sessionId": session_id }))
        .timeout(time_limit(QUICK))
        .send()
        .await
        .map_err(|e| describe(&e))?;
    into_result(res).await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A worker that accepted the connection and then never answers must not
    /// hang its caller: the call ends with an error saying what happened,
    /// within the limit.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_worker_that_never_answers_ends_the_call_with_a_timeout_error() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        // Accepts and keeps the sockets open, never answering.
        let held = tokio::spawn(async move {
            let mut sockets = Vec::new();
            while let Ok((socket, _)) = listener.accept().await {
                sockets.push(socket);
            }
        });

        let started = std::time::Instant::now();
        let err = LIMIT_OVERRIDE
            .scope(Duration::from_millis(300), async {
                update_node(port, "a", &NodeUpdate::default()).await
            })
            .await
            .unwrap_err();
        assert!(err.contains("did not answer in time"), "{err}");
        assert!(started.elapsed() < Duration::from_secs(5), "took {:?}", started.elapsed());

        // The same goes for the reads that used to be bare `reqwest::get`s.
        let err = LIMIT_OVERRIDE
            .scope(Duration::from_millis(300), get_canvas_raw(port))
            .await
            .unwrap_err();
        assert!(err.contains("did not answer in time"), "{err}");
        held.abort();
    }

    #[tokio::test]
    async fn a_worker_that_is_gone_says_it_could_not_be_reached() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        // A port nothing listens on any more.
        let port = {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            listener.local_addr().unwrap().port()
        };
        let err = update_node(port, "a", &NodeUpdate::default()).await.unwrap_err();
        assert!(err.contains("could not connect"), "{err}");
    }

    #[test]
    fn a_debug_command_gets_its_own_timeout_plus_a_margin() {
        assert_eq!(debug_send_limit(60_000), Duration::from_secs(75));
        assert_eq!(debug_send_limit(0), Duration::from_secs(15));
    }

    /// Every HTTP request in this module carries a limit. A request added
    /// without one would hang its caller on a stuck worker again, so this
    /// reads the module's own source and refuses it.
    #[test]
    fn every_http_request_in_this_module_has_a_time_limit() {
        let source = include_str!("worker_client.rs");
        let code = source.split("#[cfg(test)]\nmod tests").next().unwrap();
        let lines: Vec<&str> = code.lines().collect();
        for (i, line) in lines.iter().enumerate() {
            let trimmed = line.trim_start();
            if trimmed.starts_with("//") || !line.contains(".send()") {
                continue;
            }
            let statement = lines[i.saturating_sub(6)..=i].join("\n");
            assert!(
                statement.contains(".timeout("),
                "line {} sends a request with no time limit:\n{statement}",
                i + 1
            );
        }
    }

    /// A whole-file write names the revision it was read at (`If-Match`): a
    /// current one is accepted, a stale one is a `Conflict` carrying what the
    /// file is now at, and nothing is written.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn put_canvas_raw_is_checked_against_the_revision_it_names() {
        // The developer's own `server_socket` (a real daemon) must not stand
        // in for the worker this test starts.
        std::env::set_var("MESHFOX_SERVER_SOCKET", "");
        // `main` installs the process-wide TLS provider reqwest needs; a
        // unit test never runs `main`.
        let _ = rustls::crypto::ring::default_provider().install_default();
        let dir = std::env::temp_dir().join(format!(
            "meshfox-put-raw-conflict-test-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("doc.canvas.md");
        let original = "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n\nbody\n";
        std::fs::write(&path, original).unwrap();
        let port = crate::coordinator::get_or_spawn(&path).await.unwrap();

        let read = get_canvas_raw(port).await.unwrap();
        assert_eq!(read, original);
        let held = meshfox_core::body_rev(&read);

        let first = original.replace("body", "first");
        assert!(put_canvas_raw(port, &first, &held).await.is_ok());

        // The revision just read is stale now.
        let second = original.replace("body", "second");
        match put_canvas_raw(port, &second, &held).await {
            Err(PutRawError::Conflict { current_rev }) => {
                assert_eq!(current_rev, meshfox_core::body_rev(&first));
            }
            Err(PutRawError::Other(e)) => panic!("expected a conflict, got {e}"),
            Ok(()) => panic!("a stale revision must not be accepted"),
        }
        assert_eq!(std::fs::read_to_string(&path).unwrap(), first);

        // Writing over it is an explicit second attempt, against the
        // revision the conflict reported.
        let current = meshfox_core::body_rev(&first);
        assert!(put_canvas_raw(port, &second, &current).await.is_ok());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), second);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A WebSocket server for one connection: upgrades, then runs `script`
    /// against the socket, and returns the port.
    async fn fake_ws_worker<F, Fut>(script: F) -> u16
    where
        F: FnOnce(tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>) -> Fut
            + Send
            + 'static,
        Fut: std::future::Future<Output = ()> + Send + 'static,
    {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            script(ws).await;
        });
        port
    }

    async fn connect_fake(port: u16) -> TtySocket {
        tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}/x"))
            .await
            .unwrap()
            .0
    }

    /// A worker that goes quiet without closing — hung, or the connection
    /// half-open — is reported as `Silent` after the limit, not waited on for
    /// ever.
    #[tokio::test]
    async fn ws_next_reports_a_stream_that_went_silent() {
        use tokio_tungstenite::tungstenite::Message;
        let port = fake_ws_worker(|mut ws| async move {
            use futures_util::SinkExt;
            ws.send(Message::Text("first".into())).await.unwrap();
            tokio::time::sleep(Duration::from_secs(30)).await;
        })
        .await;
        let mut ws = connect_fake(port).await;
        LIMIT_OVERRIDE
            .scope(Duration::from_millis(300), async {
                assert!(matches!(ws_next(&mut ws).await, WsNext::Frame(Message::Text(_))));
                let started = std::time::Instant::now();
                assert!(matches!(ws_next(&mut ws).await, WsNext::Silent));
                assert!(started.elapsed() < Duration::from_secs(5));
            })
            .await;
    }

    /// A stream that is quiet but alive — only the worker's heartbeats arrive,
    /// for longer than the limit in total — never trips it: each frame
    /// restarts the clock.
    #[tokio::test]
    async fn ws_next_is_not_tripped_by_a_quiet_stream_that_keeps_sending_heartbeats() {
        use tokio_tungstenite::tungstenite::Message;
        let port = fake_ws_worker(|mut ws| async move {
            use futures_util::{SinkExt, StreamExt};
            for _ in 0..8 {
                tokio::time::sleep(Duration::from_millis(100)).await;
                ws.send(Message::Text(r#"{"type":"heartbeat"}"#.into())).await.unwrap();
            }
            ws.send(Message::Text("done".into())).await.unwrap();
            let _ = ws.close(None).await;
            // Read what the client sent back (its pongs): closing with unread
            // data would reset the connection and lose the frames above.
            while ws.next().await.is_some() {}
        })
        .await;
        let mut ws = connect_fake(port).await;
        let got = LIMIT_OVERRIDE
            .scope(Duration::from_millis(400), async {
                let started = std::time::Instant::now();
                loop {
                    match ws_next(&mut ws).await {
                        WsNext::Frame(Message::Text(t)) if !t.contains("heartbeat") => break Some((t.to_string(), started.elapsed())),
                        WsNext::Frame(_) => {}
                        WsNext::Closed | WsNext::Silent => break None,
                    }
                }
            })
            .await;
        let (text, elapsed) = got.expect("a stream with heartbeats must not read as silent or closed");
        assert_eq!(text, "done");
        assert!(
            elapsed > Duration::from_millis(400),
            "the test only means something if the stream was quiet for longer than the limit"
        );
    }

    #[tokio::test]
    async fn ws_next_reports_a_closed_stream() {
        let port = fake_ws_worker(|mut ws| async move {
            let _ = ws.close(None).await;
        })
        .await;
        let mut ws = connect_fake(port).await;
        loop {
            match ws_next(&mut ws).await {
                WsNext::Frame(_) => continue,
                WsNext::Closed => break,
                WsNext::Silent => panic!("a closed stream is not a silent one"),
            }
        }
    }


    /// The worker's heartbeat is liveness only: `watch` must not turn it into
    /// a "canvas changed" reload (an unknown message type does that), yet a
    /// real change right after it still comes through.
    #[tokio::test]
    async fn watch_ignores_heartbeats_but_still_reports_changes() {
        use tokio_tungstenite::tungstenite::Message;
        let port = fake_ws_worker(|mut ws| async move {
            use futures_util::SinkExt;
            let beat = || Message::Text(r#"{"type":"heartbeat"}"#.into());
            ws.send(Message::Text(r#"{"type":"connected","resync":false}"#.into())).await.unwrap();
            ws.send(beat()).await.unwrap();
            ws.send(beat()).await.unwrap();
            tokio::time::sleep(Duration::from_millis(400)).await;
            ws.send(Message::Text(r#"{"type":"changed","seq":1}"#.into())).await.unwrap();
            tokio::time::sleep(Duration::from_secs(5)).await;
        })
        .await;
        let mut events = watch(port);
        tokio::time::sleep(Duration::from_millis(250)).await;
        assert!(events.try_recv().is_err(), "a heartbeat was reported as an event");
        let event = tokio::time::timeout(Duration::from_secs(3), events.recv())
            .await
            .expect("the later change never arrived")
            .unwrap();
        assert!(matches!(event, WatchEvent::Changed));
    }

}
