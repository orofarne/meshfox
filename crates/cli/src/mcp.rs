//! `meshfox mcp` — an MCP stdio server for AI agents (see TODO.canvas.md's
//! "MCP-сессия"/"Несколько канвасов в одном MCP-сервере?" nodes). Takes no
//! arguments: whatever directory it's started in becomes its root. One
//! process: [`MeshfoxMcpRoot`] is what a host launches, and it keeps a
//! registry of open canvases, one [`CanvasCtx`] each.
//!
//! A `CanvasCtx` owns no canvas file and does no editing itself — every read
//! and every edit goes to that canvas's **worker** (`meshfox view`, found or
//! started through `coordinator`), which owns the file and is shared with the
//! web UI, the TUI and the CLI. What a context does own is the little state
//! the worker does not: the debug shells this server started
//! (`debug_start`/`debug_send`/`debug_stop`, a persistent `bash` kept alive in
//! a node/block's own resolved cwd/env, so state between calls survives the
//! way a one-shot `meshfox run` never could) and the last worker port it
//! talked to, so `canvas_list` can ask whether that worker still answers.
//!
//! Calls on one canvas run one at a time ([`CanvasCtx::call_lock`]); calls on
//! different canvases never wait for each other. Every call has a deadline, so
//! a hung worker costs one call an honest error rather than the session. The
//! worker is shared with other clients, so a hung one is reported, never
//! killed. A canvas that sat idle is dropped from the registry (ending its
//! debug shells) and is reopened by the next call.
//!
//! `canvas_open` only ever resolves paths under the **root directory** — the
//! canonicalized directory `meshfox mcp` was started in — rejecting anything
//! that escapes it (`..`, an absolute path elsewhere, a symlink pointing
//! out). A canvas id is that file's path relative to the root; opening an
//! already-open file just returns the same id.

use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_util::FutureExt;
use meshfox_core::Canvas;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ServerCapabilities, ServerInfo};
use rmcp::{tool, tool_handler, tool_router, ErrorData, ServerHandler, ServiceExt};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::Mutex;

const DEFAULT_IDLE_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const DEFAULT_SWEEP_INTERVAL: Duration = Duration::from_secs(5 * 60);

/// How long an open canvas may go without a call before it is dropped from
/// the registry (ending its debug shells; the next call reopens it, see
/// `MeshfoxMcpRoot::lookup`). `MESHFOX_MCP_IDLE_SECS` overrides it — a test
/// hook, same spirit as `MESHFOX_TEST_UNTOUCHED_TIMEOUT_SECS`, so the idle
/// path can be exercised without waiting half an hour.
fn idle_timeout() -> Duration {
    env_secs("MESHFOX_MCP_IDLE_SECS").unwrap_or(DEFAULT_IDLE_TIMEOUT)
}

/// How often the sweep looks for idle canvases. `MESHFOX_MCP_SWEEP_SECS`
/// overrides it (test hook).
fn sweep_interval() -> Duration {
    env_secs("MESHFOX_MCP_SWEEP_SECS").unwrap_or(DEFAULT_SWEEP_INTERVAL)
}

/// What a canvas's call says when its worker is the one not answering.
const WORKER_UNRESPONSIVE: &str = "its worker is not answering";

/// How long one tool call may take before it is abandoned. A backstop: the
/// calls to the worker have their own time limits, and `debug_send`/`run`
/// manage a command timeout of their own. Generous, since a call may first
/// have to find or start a worker (up to 31 s: two tries of the coordinator's
/// 15 s) and then wait on it (up to 60 s). `MESHFOX_MCP_CALL_DEADLINE_SECS`
/// overrides every deadline (test hook).
fn call_deadline(own_budget: Option<Duration>) -> Duration {
    if let Some(overridden) = env_secs("MESHFOX_MCP_CALL_DEADLINE_SECS") {
        return overridden;
    }
    match own_budget {
        Some(budget) => budget + Duration::from_secs(60),
        None => Duration::from_secs(90),
    }
}

/// How long a `run` call lets its chain run before killing the step that is
/// still going: the call's own `timeout_ms`, default 10 minutes, at most an
/// hour (the call's deadline is this plus a minute — see `call_deadline`).
const DEFAULT_RUN_TIMEOUT_MS: u64 = 600_000;
const MAX_RUN_TIMEOUT_MS: u64 = 3_600_000;
/// Per step, the tail of its output kept in a `run` result.
const RUN_OUTPUT_CAP_BYTES: usize = 20_000;

fn env_secs(name: &str) -> Option<Duration> {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .map(Duration::from_secs)
}
const DEFAULT_SEND_TIMEOUT_MS: u64 = 60_000;
/// How long ending one debug shell may take when its canvas is closed.
const CANVAS_CLOSE_TIMEOUT: Duration = Duration::from_secs(5);

pub async fn run() -> Result<(), String> {
    let cwd = std::env::current_dir()
        .map_err(|e| format!("failed to resolve the current directory: {e}"))?;
    let root = cwd.canonicalize().map_err(|e| {
        format!(
            "failed to resolve {} as a root directory: {e}",
            cwd.display()
        )
    })?;
    let server = MeshfoxMcpRoot::new(root);
    server.spawn_idle_sweep();
    let registry = server.clone();
    let service = server
        .serve(rmcp::transport::stdio())
        .await
        .map_err(|e| format!("failed to start MCP server: {e}"))?;
    let ended = service.waiting().await;
    // The host went away: the debug shells it started go with it.
    registry.shutdown().await;
    ended.map_err(|e| format!("MCP server ended unexpectedly: {e}"))?;
    Ok(())
}

fn invalid_params(msg: impl Into<String>) -> ErrorData {
    ErrorData::invalid_params(msg.into(), None)
}

// =======================================================================
// One open canvas: what a call on it needs.
// =======================================================================

/// Address of a debug session owned by the canvas worker.
enum DebugHandle {
    /// The worker's own session id (usually — but not necessarily, if a
    /// caller supplied its own — equal to this map's own key) and which
    /// port it lives on. `debug_send`/`debug_stop` must reach *this exact*
    /// worker, not whatever `coordinator::resolve` happens to return next
    /// time — resolving once at `debug_start` and remembering it here is
    /// what guarantees that.
    Remote { port: u16, session_id: String },
}

/// One open canvas. Everything the registry has to know about it *without
/// waiting for a call to finish* (busy, idle, health) is readable without
/// `call_lock`: a call holds it for as long as it runs, so anything that
/// needed it to look would stall behind a call that is stuck, which is exactly
/// when it matters.
struct CanvasCtx {
    canvas_path: PathBuf,
    sessions: Mutex<HashMap<String, DebugHandle>>,
    /// The worker this canvas last talked to, so `canvas_list` can ask whether
    /// it is still answering — without starting one if there never was one.
    last_worker_port: std::sync::Mutex<Option<u16>>,
    /// One call at a time on a canvas: held for the call's whole duration.
    call_lock: Mutex<()>,
    last_used: std::sync::Mutex<Instant>,
}

impl CanvasCtx {
    fn new(canvas_path: PathBuf) -> Self {
        Self {
            canvas_path,
            sessions: Mutex::new(HashMap::new()),
            last_worker_port: std::sync::Mutex::new(None),
            call_lock: Mutex::new(()),
            last_used: std::sync::Mutex::new(Instant::now()),
        }
    }

    fn touch(&self) {
        *self.last_used.lock().unwrap() = Instant::now();
    }

    fn idle_for(&self) -> Duration {
        self.last_used.lock().unwrap().elapsed()
    }

    /// A call is in flight right now.
    fn is_busy(&self) -> bool {
        self.call_lock.try_lock().is_err()
    }

    /// What `canvas_list` says about it: whether the worker it last talked to
    /// still answers. A worker that has exited is no problem (the next call
    /// finds or starts another), and none is started to find out; one that is
    /// there and not answering is reported — never stopped, it is shared with
    /// the web UI, the TUI and the CLI.
    async fn health(&self) -> String {
        let port = *self.last_worker_port.lock().unwrap();
        let Some(port) = port else {
            return "ok".to_string();
        };
        match crate::worker_client::ping(port).await {
            crate::worker_client::WorkerHealth::Answering => "ok".to_string(),
            crate::worker_client::WorkerHealth::Gone => {
                *self.last_worker_port.lock().unwrap() = None;
                "ok".to_string()
            }
            crate::worker_client::WorkerHealth::Unresponsive => WORKER_UNRESPONSIVE.to_string(),
        }
    }

    /// Ends every debug shell this canvas started, each within
    /// `CANVAS_CLOSE_TIMEOUT` — a hung worker must not hold up closing.
    async fn stop_debug_sessions(&self) {
        let handles: Vec<DebugHandle> =
            self.sessions.lock().await.drain().map(|(_, h)| h).collect();
        for DebugHandle::Remote { port, session_id } in handles {
            let _ = tokio::time::timeout(
                CANVAS_CLOSE_TIMEOUT,
                crate::worker_client::debug_stop(port, &session_id),
            )
            .await;
        }
    }

    async fn read_raw(&self) -> Result<String, ErrorData> {
        let port = self.worker_port().await?;
        crate::worker_client::get_canvas_raw(port)
            .await
            .map_err(|e| ErrorData::internal_error(format!("worker read failed: {e}"), None))
    }

    /// The port of this canvas's worker, starting one if none is running.
    /// A worker that did not come up in time (the coordinator gives it 15
    /// seconds and then kills it — a large canvas on a busy machine can
    /// miss that) is tried once more before the call fails: the next
    /// attempt starts a fresh one.
    async fn worker_port(&self) -> Result<u16, ErrorData> {
        let first = crate::coordinator::get_or_spawn(&self.canvas_path).await;
        let result = match first {
            Ok(port) => Ok(port),
            Err(_) => {
                tokio::time::sleep(Duration::from_millis(500)).await;
                crate::coordinator::get_or_spawn(&self.canvas_path).await
            }
        };
        let port = result
            .map_err(|e| ErrorData::internal_error(format!("worker unavailable: {e}"), None))?;
        *self.last_worker_port.lock().unwrap() = Some(port);
        Ok(port)
    }
}

// ---------------------------------------------------------------------
// Leaf tool parameter types
// ---------------------------------------------------------------------

#[derive(Deserialize, Serialize, JsonSchema)]
struct DebugStartParams {
    /// The node whose own cwd/env this session runs in.
    node_id: String,
    /// The runnable block whose `env=` to resolve for this session's
    /// environment. Omit to use the node's sole/default block, same
    /// convention `meshfox run` uses (an explicit `default` flag, or a
    /// node with exactly one unnamed/implicitly-named block).
    #[serde(default)]
    block_name: Option<String>,
    /// Explicit values for `meshfox:var` declarations this block's `env=`
    /// references — same role as `meshfox run --set NAME=VALUE`. A
    /// `required` variable with no default and no override here comes
    /// back as a structured error (`missing_vars`), never a hang or a
    /// guess — there's no interactive terminal on the other end of this
    /// call to prompt.
    #[serde(default)]
    vars: HashMap<String, String>,
}

#[derive(Deserialize, Serialize, JsonSchema)]
struct DebugSendParams {
    session_id: String,
    /// Shell code to run in this session's own shell — the same process,
    /// cwd, and exported variables every earlier `debug_send` in this
    /// session left behind.
    code: String,
    /// How long to wait for `code` to finish before giving up. On timeout
    /// the whole session is killed (`SIGTERM`, then `SIGKILL` if it's still
    /// alive shortly after) and ends — see `timed_out`/`session_ended` in
    /// the result; call `debug_start` again for a fresh one. Defaults to
    /// 60000 (one minute).
    #[serde(default)]
    timeout_ms: Option<u64>,
}

#[derive(Deserialize, Serialize, JsonSchema)]
struct DebugStopParams {
    session_id: String,
}

#[derive(Deserialize, Serialize, JsonSchema)]
struct NodeIdParams {
    node_id: String,
    /// Include the node's own Markdown body (its `text`, between this
    /// heading and the next) in the result. Omitted by default since most
    /// callers only want the structural metadata.
    #[serde(default)]
    include_body: bool,
}

#[derive(Deserialize, Serialize, JsonSchema, Default)]
struct McpNodeFields {
    #[serde(default)]
    x: Option<f64>,
    #[serde(default)]
    y: Option<f64>,
    #[serde(default)]
    width: Option<f64>,
    #[serde(default)]
    height: Option<f64>,
    #[serde(default)]
    color: Option<String>,
    /// `text` (the default), `file`, `link`, `group`, or `include`.
    #[serde(default, rename = "type")]
    node_type: Option<String>,
    /// `file`-node display mode: `link` (the default), `code` (a read-only code
    /// preview) or `table` (an interactive, read-only table — needs the `duckdb` CLI).
    #[serde(default)]
    display: Option<String>,
    #[serde(default)]
    lang: Option<String>,
    #[serde(default)]
    interpreter: Option<String>,
    #[serde(default)]
    preview: Option<bool>,
    /// `true`/`false` for an explicit per-node override, `"default"` to
    /// clear it back to following the document's own default.
    #[serde(default)]
    fold: Option<String>,
    /// Override `createdAt=` (RFC3339, e.g. `2026-08-29T10:15:00Z` or with
    /// an explicit offset). Meant for backfilling/importing existing data —
    /// meshfox only stamps a fresh one automatically at creation time when
    /// the document declares the `auto-timestamps` option (off by default,
    /// see SPEC.md's "Timestamps"), so this is the only way to get a
    /// `createdAt` on a document that doesn't. Omit to leave whatever's
    /// already there untouched.
    #[serde(default, rename = "createdAt")]
    created_at: Option<String>,
}

impl McpNodeFields {
    fn into_node_meta_fields(self) -> crate::NodeMetaFields {
        crate::NodeMetaFields {
            x: self.x,
            y: self.y,
            width: self.width,
            height: self.height,
            color: self.color,
            node_type: self.node_type,
            display: self.display,
            lang: self.lang,
            interpreter: self.interpreter,
            preview: self.preview,
            fold: self.fold,
            tags: None,
            add_tag: None,
            remove_tag: None,
            created_at: self.created_at,
        }
    }
}

#[derive(Deserialize, Serialize, JsonSchema)]
struct NodeAddParams {
    parent_id: String,
    title: String,
    /// Sets the new node's body in the same call — the fenced code, for
    /// instance — instead of a separate follow-up `node_body` call.
    #[serde(default)]
    body: Option<String>,
    /// Comma-separated tags to give the new node.
    #[serde(default)]
    tags: Option<String>,
    #[serde(flatten)]
    fields: McpNodeFields,
}

#[derive(Deserialize, Serialize, JsonSchema)]
struct NodeMetaParams {
    node_id: String,
    /// Clears x/y/width/height back to unset. Mutually exclusive with
    /// passing any of them in `fields`.
    #[serde(default)]
    clear_position: bool,
    /// Tags to add to the node; tags it already has stay where they are.
    /// There is deliberately no way to replace the whole tag list: a
    /// replacement written from a stale read silently drops tags someone
    /// else added in between.
    #[serde(default)]
    add_tags: Vec<String>,
    /// Tags to remove from the node; a tag it doesn't have is ignored.
    #[serde(default)]
    remove_tags: Vec<String>,
    #[serde(flatten)]
    fields: McpNodeFields,
}

#[derive(Deserialize, Serialize, JsonSchema)]
struct NodeBodyParams {
    node_id: String,
    body: String,
    /// The `body_rev` `node_show`/`node_find` returned when you read the
    /// node's body — required. If the body has changed since, nothing is
    /// written and the error carries the current body and revision to merge
    /// against and retry with.
    base_rev: String,
}

#[derive(Deserialize, Serialize, JsonSchema)]
struct NodeAppendParams {
    node_id: String,
    /// Text to append after whatever's already in the node's body.
    addition: String,
}

#[derive(Deserialize, Serialize, JsonSchema)]
struct NodeRmParams {
    node_id: String,
    /// Promote direct children to this node's own parent instead of
    /// deleting them too.
    #[serde(default)]
    keep_children: bool,
}

#[derive(Deserialize, Serialize, JsonSchema)]
struct NodeMvParams {
    node_id: String,
    new_parent_id: String,
}

#[derive(Deserialize, Serialize, JsonSchema, Default)]
struct NodeBlockParams {
    node_id: String,
    block_name: String,
    #[serde(default)]
    rename: Option<String>,
    #[serde(default)]
    lang: Option<String>,
    #[serde(default)]
    cache: Option<bool>,
    #[serde(default)]
    always: Option<bool>,
    #[serde(default)]
    default: Option<bool>,
    #[serde(default)]
    tty: Option<bool>,
    #[serde(default)]
    autoclose: Option<bool>,
    /// **Experimental** — see SPEC.md's "Service blocks (experimental)".
    #[serde(default)]
    service: Option<bool>,
    /// Comma-separated `deps=` list, replacing the whole thing (same
    /// syntax as the fence attribute itself — bare `name` or
    /// `node-id/name`). Mutually exclusive with `clear_deps`.
    #[serde(default)]
    deps: Option<String>,
    #[serde(default)]
    clear_deps: bool,
    /// Comma-separated `env=` list, same syntax as the fence attribute.
    #[serde(default)]
    env: Option<String>,
    #[serde(default)]
    clear_env: bool,
    #[serde(default)]
    interpreter: Option<String>,
    #[serde(default)]
    clear_interpreter: bool,
    /// Replaces the fence's own code. Omit to leave the code untouched.
    #[serde(default)]
    code: Option<String>,
}

#[derive(Deserialize, Serialize, JsonSchema)]
struct RunParams {
    /// Explicit user approval for this invocation's confirm-marked blocks.
    /// On confirmation-required errors, ask the user before retrying with true.
    #[serde(default)]
    confirm: bool,
    /// The node that owns the block to run.
    node_id: String,
    /// The block's `name=`. Omit it to run the node's default block (the one
    /// flagged `default`, or named like the node).
    #[serde(default)]
    block: Option<String>,
    /// Run only this block, skipping its `deps=` chain.
    #[serde(default)]
    no_deps: bool,
    /// Run every block of the chain for real, ignoring "already ran this
    /// session and hasn't changed" for this call only — for a build/test
    /// step whose result depends on files rather than on the block's own
    /// text. The results are recorded as usual; nothing is forgotten (see
    /// session_reset). Not combinable with no_deps.
    #[serde(default)]
    fresh: bool,
    /// Values for declared `meshfox:var`s that aren't already resolvable
    /// (from the on-disk cache, the environment or a default). Never for a
    /// `secret` variable — secrets are not passed through this tool.
    #[serde(default)]
    vars: HashMap<String, String>,
    /// Literal local block arguments. Required arguments must be supplied,
    /// even when their declaration carries a UI default. Never cached as vars.
    #[serde(default)]
    args: std::collections::BTreeMap<String, String>,
    /// Give up after this many milliseconds, killing the step that is still
    /// running (default 600000, at most 3600000).
    #[serde(default)]
    timeout_ms: Option<u64>,
}

#[derive(Deserialize, Serialize, JsonSchema, Default)]
struct SessionResetParams {}

#[derive(Deserialize, Serialize, JsonSchema)]
struct NodeRenameParams {
    node_id: String,
    /// New heading text. The node's id, heading level, and body are left
    /// untouched — an id is pinned the first time it's written and never
    /// follows later title edits. (The root is always `root` unless it
    /// declares another id; a non-root node with no explicit id — its id is
    /// derived from its title — is refused: pin one first with `node_set_id`.)
    title: String,
}

#[derive(Deserialize, Serialize, JsonSchema)]
struct NodeSetIdParams {
    node_id: String,
    /// The new stable id. Every `parent=`/`meshfox:edge from=` reference to
    /// the old id is rewritten exactly; `deps=` references are rewritten
    /// best-effort (plain text, not parser-validated).
    new_id: String,
}

#[derive(Deserialize, Serialize, JsonSchema, Default)]
struct NodeEdgesParams {
    node_id: String,
    /// Ids to add as extra parents (`meshfox:edge from="..."` lines). An
    /// edge that's already there is left exactly as it is, label and route
    /// included.
    #[serde(default)]
    add: Vec<String>,
    /// Ids to remove from the extra parents.
    #[serde(default)]
    remove: Vec<String>,
}

#[derive(Deserialize, Serialize, JsonSchema)]
struct NodeMoveParams {
    node_id: String,
    /// Move `node_id` to sit immediately before this sibling. Exactly one
    /// of `before`/`after` is required; both must share `node_id`'s own
    /// structural parent.
    #[serde(default)]
    before: Option<String>,
    /// Move `node_id` to sit immediately after this sibling.
    #[serde(default)]
    after: Option<String>,
}

#[derive(Deserialize, Serialize, JsonSchema, Default)]
struct NodeReorderParams {}

#[derive(Deserialize, Serialize, JsonSchema, Default)]
struct UndoParams {}

#[derive(Deserialize, Serialize, JsonSchema, Default)]
struct RedoParams {}

fn default_history_limit() -> usize {
    50
}

#[derive(Deserialize, Serialize, JsonSchema)]
struct HistoryParams {
    /// How many applied steps to list, most recent first. The current
    /// redo tail (if any) is always listed in full regardless of this.
    #[serde(default = "default_history_limit")]
    limit: usize,
}

#[derive(Deserialize, Serialize, JsonSchema)]
struct HistoryGotoParams {
    /// The target step to jump to, as listed by `history`'s own `seq`
    /// field — `0` means "undo everything".
    seq: i64,
}

#[derive(Deserialize, Serialize, JsonSchema)]
struct ValidateParams {}

#[derive(Deserialize, Serialize, JsonSchema)]
struct CheckParams {}

#[derive(Deserialize, Serialize, JsonSchema)]
struct NodeFindParams {
    /// A CSS selector matched against the canvas tree: a node is an
    /// element, each tag is a class (`.bag`), `id`/`type`/`color` are
    /// ordinary attributes (`[type="file"]`), structural nesting is DOM
    /// nesting (`#todo > .bag` for direct children, `#todo .bag` for
    /// descendants at any depth). Omit (or pass `"*"`) to match every
    /// node — useful when filtering purely by `text`/the date fields
    /// below, which AND together with this as independent predicates,
    /// not a CSS extension (CSS selectors have no substring-search or
    /// numeric-range primitives to begin with).
    #[serde(default = "default_selector")]
    selector: String,
    /// Include each match's full `node_show`-equivalent metadata instead of
    /// just its id.
    #[serde(default)]
    show: bool,
    /// With `show`, also include each match's own Markdown body. No effect
    /// without `show`, since a bare id list has no metadata to attach it to.
    #[serde(default)]
    include_body: bool,
    /// Case-insensitive substring match against each node's title or body
    /// text. A match's own result carries a short `excerpt` of the
    /// matching line, so a caller doesn't need a separate `node_show` just
    /// to see why it matched.
    #[serde(default)]
    text: Option<String>,
    /// Keep only nodes `createdAt`'d at or after this instant — RFC3339
    /// (`2026-08-29T10:00:00Z`), or a relative duration ago (`7d`, `2w`,
    /// `1h`, `30m`, `45s`). A node with no `createdAt` at all never
    /// matches any of these five date fields — "unset" isn't "in range".
    #[serde(default, rename = "createdAfter")]
    created_after: Option<String>,
    /// Keep only nodes `createdAt`'d strictly before this instant. Same
    /// RFC3339-or-relative parsing as `createdAfter`.
    #[serde(default, rename = "createdBefore")]
    created_before: Option<String>,
    /// Keep only nodes `updatedAt`'d at or after this instant. Same
    /// RFC3339-or-relative parsing as `createdAfter`.
    #[serde(default, rename = "updatedAfter")]
    updated_after: Option<String>,
    /// Keep only nodes `updatedAt`'d strictly before this instant. Same
    /// RFC3339-or-relative parsing as `createdAfter`.
    #[serde(default, rename = "updatedBefore")]
    updated_before: Option<String>,
    /// Keep only nodes touched (created OR updated) at or after this
    /// instant — shorthand for "what changed recently", equivalent to
    /// `createdAfter`/`updatedAfter` together. Same RFC3339-or-relative
    /// parsing.
    #[serde(default)]
    since: Option<String>,
}

fn default_selector() -> String {
    "*".to_string()
}

fn bool_pair(v: Option<bool>) -> (bool, bool) {
    match v {
        Some(true) => (true, false),
        Some(false) => (false, true),
        None => (false, false),
    }
}

// ---------------------------------------------------------------------
// Leaf tools
// ---------------------------------------------------------------------

impl CanvasCtx {
    /// Starts a persistent debug shell in a node/block's own resolved cwd and env — state (exported vars, files written) survives across debug_send calls, unlike a one-shot `run`. Returns a session_id.
    async fn debug_start(&self, params: DebugStartParams) -> Result<CallToolResult, ErrorData> {
        let port = self.worker_port().await?;
        self.debug_start_remote(port, params).await
    }

    /// Starts the session at the worker (`POST /api/debug/start`), tracked under `DebugHandle::
    /// Remote` keyed by the *worker's own* session id (no separate local id
    /// needed on top of it). The worker itself knows the session is wanted —
    /// it keeps its debug sessions in a registry it consults before exiting,
    /// like its runs, services and `tty` sessions — so nothing here has to
    /// hold a connection open on its behalf.
    async fn debug_start_remote(
        &self,
        port: u16,
        params: DebugStartParams,
    ) -> Result<CallToolResult, ErrorData> {
        let (session_id, block_name, cwd) = crate::worker_client::debug_start(
            port,
            &params.node_id,
            params.block_name.as_deref(),
            params.vars,
        )
        .await
        .map_err(invalid_params)?;
        self.sessions.lock().await.insert(
            session_id.clone(),
            DebugHandle::Remote {
                port,
                session_id: session_id.clone(),
            },
        );
        Ok(CallToolResult::structured(json!({
            "session_id": session_id,
            "node_id": params.node_id,
            "block_name": block_name,
            "cwd": cwd,
        })))
    }

    /// Runs shell code in an already-started debug session's own shell (same process, cwd, and exported variables every earlier debug_send in this session left behind). Returns stdout, stderr, exit_code.
    async fn debug_send(&self, params: DebugSendParams) -> Result<CallToolResult, ErrorData> {
        let (port, session_id) = {
            let sessions = self.sessions.lock().await;
            match sessions.get(&params.session_id) {
                Some(DebugHandle::Remote { port, session_id }) => (*port, session_id.clone()),
                None => {
                    return Err(invalid_params(format!(
                        "no debug session {:?}",
                        params.session_id
                    )))
                }
            }
        };
        let timeout_ms = params.timeout_ms.unwrap_or(DEFAULT_SEND_TIMEOUT_MS);
        let outcome = crate::worker_client::debug_send(port, &session_id, &params.code, timeout_ms)
            .await
            .map_err(invalid_params)?;
        let (stdout, stderr, exit_code, timed_out, session_ended) = (
            outcome.stdout,
            outcome.stderr,
            outcome.exit_code,
            outcome.timed_out,
            outcome.session_ended,
        );
        if session_ended {
            self.sessions.lock().await.remove(&params.session_id);
        }
        Ok(CallToolResult::structured(json!({
            "stdout": stdout,
            "stderr": stderr,
            "exit_code": exit_code,
            "timed_out": timed_out,
            "session_ended": session_ended,
        })))
    }

    /// Stops a debug session, killing its shell and every process it started.
    async fn debug_stop(&self, params: DebugStopParams) -> Result<CallToolResult, ErrorData> {
        let removed = self.sessions.lock().await.remove(&params.session_id);
        match removed {
            Some(DebugHandle::Remote { port, session_id }) => {
                crate::worker_client::debug_stop(port, &session_id)
                    .await
                    .map_err(invalid_params)?;
                Ok(CallToolResult::structured(
                    json!({ "stopped": params.session_id }),
                ))
            }
            None => Err(invalid_params(format!(
                "no debug session {:?}",
                params.session_id
            ))),
        }
    }

    /// Reads one node's structured metadata — id, title, type, parent, children, position, color, tags, and, for a file/link node, target/display. Pass include_body to also get its Markdown body.
    async fn node_show(&self, params: NodeIdParams) -> Result<CallToolResult, ErrorData> {
        let raw = self.read_raw().await?;
        let canvas = Canvas::from_markdown(&raw).map_err(|e| invalid_params(e.to_string()))?;
        let node = canvas
            .node(&params.node_id)
            .ok_or_else(|| invalid_params(format!("no node {:?}", params.node_id)))?;
        Ok(CallToolResult::structured(node_json(
            &canvas,
            node,
            params.include_body,
        )))
    }

    /// Adds a new child node under parent_id, as the last item in its subtree. Optionally sets its body and meta fields in the same call. Returns the new node's id.
    async fn node_add(&self, params: NodeAddParams) -> Result<CallToolResult, ErrorData> {
        let port = self.worker_port().await?;
        // The body goes in with the node itself — a node that doesn't exist
        // yet has no earlier body revision to be stale against.
        let new_id = crate::worker_client::create_node(
            port,
            &params.parent_id,
            &params.title,
            true,
            params.body.as_deref(),
        )
        .await
        .map_err(invalid_params)?;
        let mut meta_fields = params.fields.into_node_meta_fields();
        meta_fields.tags = params.tags;
        if meta_fields.is_set() {
            let update =
                crate::node_update_from_fields(&meta_fields, None).map_err(invalid_params)?;
            crate::worker_client::update_node(port, &new_id, &update)
                .await
                .map_err(invalid_params)?;
        }
        Ok(CallToolResult::structured(json!({ "node_id": new_id })))
    }

    /// Updates a node's position/style/type fields. Any field left unset (or absent) keeps its current value.
    async fn node_meta(&self, params: NodeMetaParams) -> Result<CallToolResult, ErrorData> {
        let port = self.worker_port().await?;
        let mut meta_fields = params.fields.into_node_meta_fields();
        if !params.add_tags.is_empty() {
            meta_fields.add_tag = Some(params.add_tags.join(","));
        }
        if !params.remove_tags.is_empty() {
            meta_fields.remove_tag = Some(params.remove_tags.join(","));
        }
        let mut update =
            crate::node_update_from_fields(&meta_fields, None).map_err(invalid_params)?;
        update.clear_position = params.clear_position;
        crate::worker_client::update_node(port, &params.node_id, &update)
            .await
            .map_err(invalid_params)?;
        Ok(CallToolResult::structured(
            json!({ "updated": params.node_id }),
        ))
    }

    /// Replaces a node's whole Markdown body. Requires `base_rev`, the `body_rev` node_show/node_find returned when you read it; if the body changed since, nothing is written and the error carries the current body and revision so you can merge and retry. To add to a body without reading it first, use node_append.
    async fn node_body(&self, params: NodeBodyParams) -> Result<CallToolResult, ErrorData> {
        let port = self.worker_port().await?;
        crate::worker_client::update_node_body(
            port,
            &params.node_id,
            &params.body,
            &params.base_rev,
        )
        .await
        .map_err(invalid_params)?;
        Ok(CallToolResult::structured(
            json!({ "updated": params.node_id }),
        ))
    }

    /// Appends text to the end of a node's existing Markdown body — after whatever's already there, still before its first child's own heading — without having to read the current body back first just to resend it unchanged.
    async fn node_append(&self, params: NodeAppendParams) -> Result<CallToolResult, ErrorData> {
        let port = self.worker_port().await?;
        crate::worker_client::append_node_body(port, &params.node_id, &params.addition)
            .await
            .map_err(invalid_params)?;
        Ok(CallToolResult::structured(
            json!({ "updated": params.node_id }),
        ))
    }

    /// Rewrites just one runnable fence's own attributes (and, optionally, its code) inside a node, leaving the rest of the node's body untouched.
    async fn node_block(&self, params: NodeBlockParams) -> Result<CallToolResult, ErrorData> {
        let (cache, no_cache) = bool_pair(params.cache);
        let (always, no_always) = bool_pair(params.always);
        let (default, no_default) = bool_pair(params.default);
        let (tty, no_tty) = bool_pair(params.tty);
        let (autoclose, no_autoclose) = bool_pair(params.autoclose);
        let (service, no_service) = bool_pair(params.service);
        let args = crate::BlockArgs {
            rename: params.rename.clone(),
            lang: params.lang.clone(),
            cache,
            no_cache,
            always,
            no_always,
            default,
            no_default,
            tty,
            no_tty,
            autoclose,
            no_autoclose,
            service,
            no_service,
            deps: params.deps.clone(),
            clear_deps: params.clear_deps,
            env: params.env.clone(),
            clear_env: params.clear_env,
            interpreter: params.interpreter.clone(),
            clear_interpreter: params.clear_interpreter,
            code_file: None,
        };
        let port = self.worker_port().await?;
        let update = crate::block_attrs_update_from_args(&args, params.code.as_deref())
            .map_err(invalid_params)?;
        crate::worker_client::set_block_attrs(port, &params.node_id, &params.block_name, &update)
            .await
            .map_err(invalid_params)?;
        Ok(CallToolResult::structured(json!({
            "updated": params.node_id,
            "block": params.block_name,
        })))
    }

    /// Deletes a node. By default its whole subtree goes with it; keep_children promotes its direct children to its own former parent instead.
    async fn node_rm(&self, params: NodeRmParams) -> Result<CallToolResult, ErrorData> {
        let port = self.worker_port().await?;
        crate::worker_client::remove_node(port, &params.node_id, params.keep_children)
            .await
            .map_err(invalid_params)?;
        Ok(CallToolResult::structured(
            json!({ "deleted": params.node_id, "keep_children": params.keep_children }),
        ))
    }

    /// Moves a node to a new structural parent.
    async fn node_mv(&self, params: NodeMvParams) -> Result<CallToolResult, ErrorData> {
        let port = self.worker_port().await?;
        crate::node_mv_via_worker(
            port,
            &self.canvas_path,
            &params.node_id,
            &params.new_parent_id,
        )
        .await
        .map_err(invalid_params)?;
        Ok(CallToolResult::structured(json!({
            "moved": params.node_id,
            "new_parent_id": params.new_parent_id,
        })))
    }

    /// Runs a runnable block (a code fence with a name=) together with its deps= chain, like `meshfox run`, and returns every step's exit code, duration and output (the tail of each, up to 20 KB), plus an overall success flag. A dependency that already ran this session and looks unchanged is skipped — pass fresh: true to run the whole chain for real this once (for builds/tests that depend on files, not on the block's text), or no_deps: true to run just this block. `cache`d output is saved into the canvas file, as with the CLI. Not for interactive (`tty`) blocks. Variables not yet resolved must be given in `vars` (never secrets). A step still running after timeout_ms (default 10 minutes) is killed.
    async fn run(&self, params: RunParams) -> Result<CallToolResult, ErrorData> {
        if params.fresh && params.no_deps {
            return Err(invalid_params(
                "fresh and no_deps can't be combined — no_deps already runs just the named block"
                    .to_string(),
            ));
        }
        let port = self.worker_port().await?;
        let raw = crate::worker_client::get_canvas_raw(port)
            .await
            .map_err(|e| ErrorData::internal_error(format!("worker read failed: {e}"), None))?;
        let primary = Canvas::from_markdown(&raw).map_err(|e| invalid_params(e.to_string()))?;
        let canvas = meshfox_core::include::resolve(&primary, &self.canvas_path).unwrap_or(primary);
        let mut path = canvas
            .id_path_to(&params.node_id)
            .ok_or_else(|| invalid_params(format!("no node {:?}", params.node_id)))?;
        // `meshfox run a b` addresses a node's default block by giving the
        // node's own id where the block name goes (see `resolve_run_chain`).
        let name = match &params.block {
            Some(block) => block.clone(),
            None => {
                path.pop();
                params.node_id.clone()
            }
        };

        let prepared = crate::worker_client::prepare_arguments(
            port,
            &path,
            &name,
            &params.args,
            params.no_deps,
        )
        .await
        .map_err(invalid_params)?;
        let path = prepared.path;
        let name = prepared.block.ok_or_else(|| {
            invalid_params(format!(
                "missing required argument(s): {} — supply args or use a named application",
                prepared
                    .fields
                    .iter()
                    .filter(|field| !field.resolved)
                    .map(|field| field.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ))
        })?;

        let path_refs: Vec<&str> = path.iter().map(String::as_str).collect();
        if crate::chain_contains_tty(
            &self.canvas_path,
            &raw,
            &path_refs,
            &[&name],
            params.no_deps,
        ) {
            return Err(invalid_params(
                "this chain contains an interactive (`tty`) block, which needs a real terminal \
                 — run it with `meshfox run` yourself"
                    .to_string(),
            ));
        }

        let gates = crate::worker_client::confirmation_blocks(port, &path, &name, params.no_deps)
            .await
            .map_err(invalid_params)?;
        if !params.confirm && !gates.is_empty() {
            return Err(invalid_params(format!(
                "confirmation required for {} — ask the user for explicit approval, then retry with confirm: true",
                gates.join(", "))));
        }

        // Variables: only what the caller supplied for a declared, non-secret
        // variable; anything still unresolved is reported rather than asked.
        let statuses = crate::worker_client::get_vars(port, &path, &name, params.no_deps)
            .await
            .map_err(invalid_params)?;
        let mut vars = HashMap::new();
        let mut missing = Vec::new();
        for status in &statuses {
            match params.vars.get(&status.name) {
                Some(_) if status.secret => {
                    return Err(invalid_params(format!(
                        "{} is a secret variable — secrets are not passed through this tool; \
                         set it in the environment or the secret store",
                        status.name
                    )))
                }
                Some(value) => {
                    vars.insert(status.name.clone(), value.clone());
                }
                None if !status.resolved => missing.push(status.name.clone()),
                None => {}
            }
        }
        if !missing.is_empty() {
            return Err(invalid_params(format!(
                "missing required variable(s): {} — pass the non-secret ones in `vars`; a secret \
                 has to be set in the environment or the secret store",
                missing.join(", ")
            )));
        }

        let mut rx = crate::worker_client::run_stream_persisted(
            port,
            &path,
            &name,
            params.no_deps,
            params.fresh,
            vars,
            std::collections::HashSet::new(),
            None,
            params.confirm,
        )
        .await
        .map_err(|e| ErrorData::internal_error(format!("couldn't start the run: {e}"), None))?;

        let timeout_ms = params
            .timeout_ms
            .unwrap_or(DEFAULT_RUN_TIMEOUT_MS)
            .clamp(1, MAX_RUN_TIMEOUT_MS);
        let deadline = tokio::time::Instant::now() + Duration::from_millis(timeout_ms);
        let mut report = RunReport::default();
        loop {
            let event = if report.timed_out {
                // Killed: give the worker a moment to say how it ended.
                match tokio::time::timeout(Duration::from_secs(10), rx.recv()).await {
                    Ok(event) => event,
                    Err(_) => break,
                }
            } else {
                match tokio::time::timeout_at(deadline, rx.recv()).await {
                    Ok(event) => event,
                    Err(_) => {
                        report.timed_out = true;
                        if let Some(step) = report.running_step() {
                            let _ = crate::worker_client::kill_run(
                                port,
                                &step.node_id.clone(),
                                &step.block.clone(),
                            )
                            .await;
                        }
                        continue;
                    }
                }
            };
            let Some(event) = event else { break };
            if report.apply(event) {
                break;
            }
        }
        report.finish()
    }

    /// Forgets what this canvas's session remembers — every block's "already ran this session" record and every submitted form value — so the next run executes every block for real. Finished runs stay as history; the canvas file and saved output are untouched. For a single run use run's fresh: true instead.
    async fn session_reset(
        &self,
        _params: SessionResetParams,
    ) -> Result<CallToolResult, ErrorData> {
        let port = self.worker_port().await?;
        crate::worker_client::reset_session(port)
            .await
            .map_err(invalid_params)?;
        Ok(CallToolResult::structured(json!({ "reset": true })))
    }

    /// Renames a node's heading text, leaving its id, heading level, and body untouched.
    async fn node_rename(&self, params: NodeRenameParams) -> Result<CallToolResult, ErrorData> {
        let port = self.worker_port().await?;
        let update = crate::worker_client::NodeUpdate {
            title: Some(params.title.clone()),
            ..Default::default()
        };
        crate::worker_client::update_node(port, &params.node_id, &update)
            .await
            .map_err(invalid_params)?;
        Ok(CallToolResult::structured(json!({
            "renamed": params.node_id,
            "title": params.title,
        })))
    }

    /// Changes a node's id, the stable handle used for addressing, meshfox:edge/parent= references, and deps= references. Rewrites every reference it can find (best-effort for deps=). Fails if new_id is empty, contains a disallowed character, or is already used.
    async fn node_set_id(&self, params: NodeSetIdParams) -> Result<CallToolResult, ErrorData> {
        let port = self.worker_port().await?;
        crate::worker_client::rename_node_id(port, &params.node_id, &params.new_id)
            .await
            .map_err(invalid_params)?;
        Ok(CallToolResult::structured(json!({
            "old_id": params.node_id,
            "new_id": params.new_id,
        })))
    }

    /// Adds or removes extra incoming edges on a node (meshfox:edge from="..." lines) — the non-structural, non-nesting cross-references. Never replaces the whole set: `add` leaves an edge that already exists exactly as it is, `remove` drops just the ones named.
    async fn node_edges(&self, params: NodeEdgesParams) -> Result<CallToolResult, ErrorData> {
        let port = self.worker_port().await?;
        if params.add.is_empty() && params.remove.is_empty() {
            return Err(invalid_params(
                "nothing to do — pass `add` and/or `remove`".to_string(),
            ));
        }
        let update = crate::worker_client::NodeUpdate {
            edges: Some(crate::worker_client::EdgeOps {
                add: params.add.clone(),
                remove: params.remove.clone(),
            }),
            ..Default::default()
        };
        crate::worker_client::update_node(port, &params.node_id, &update)
            .await
            .map_err(invalid_params)?;
        Ok(CallToolResult::structured(json!({
            "updated": params.node_id,
            "added": params.add,
            "removed": params.remove,
        })))
    }

    /// Moves a node's whole subtree to sit immediately before or after another sibling under the same structural parent — the on-disk heading order, which is a node's only sibling order until it also has a real x/y. Exactly one of before/after is required.
    async fn node_move(&self, params: NodeMoveParams) -> Result<CallToolResult, ErrorData> {
        let port = self.worker_port().await?;
        let (target_id, is_before) = match (&params.before, &params.after) {
            (Some(t), None) => (t.clone(), true),
            (None, Some(t)) => (t.clone(), false),
            (None, None) => {
                return Err(invalid_params(
                    "exactly one of before/after is required".to_string(),
                ))
            }
            (Some(_), Some(_)) => {
                return Err(invalid_params(
                    "before and after are mutually exclusive".to_string(),
                ))
            }
        };
        crate::worker_client::move_sibling(port, &params.node_id, &target_id, is_before)
            .await
            .map_err(invalid_params)?;
        Ok(CallToolResult::structured(json!({
            "moved": params.node_id,
            "position": if is_before { "before" } else { "after" },
            "target_id": target_id,
        })))
    }

    /// Reorders every parent's direct children in the file to match their canvas layout (sorted by y then x among ties) — the same resync the server runs on every web-UI save, exposed standalone for whenever positions changed by hand (or via node_meta) and the on-disk heading order should catch up.
    async fn node_reorder(&self, _params: NodeReorderParams) -> Result<CallToolResult, ErrorData> {
        let port = self.worker_port().await?;
        crate::worker_client::reorder_document(port)
            .await
            .map_err(invalid_params)?;
        Ok(CallToolResult::structured(json!({ "reordered": true })))
    }

    /// Reverts the most recent still-undoable edit to this canvas — any node/edge/reorder change, from any client (web UI, CLI, another MCP call), since undo history lives with the canvas's own worker, not with whoever made the edit. A no-op (changed: false), not an error, when there's nothing left to undo — safe to call speculatively.
    async fn undo(&self, _params: UndoParams) -> Result<CallToolResult, ErrorData> {
        let port = self.worker_port().await?;
        let result = crate::worker_client::undo(port)
            .await
            .map_err(invalid_params)?;
        Ok(CallToolResult::structured(
            serde_json::to_value(result)
                .map_err(|e| ErrorData::internal_error(e.to_string(), None))?,
        ))
    }

    /// The mirror image of undo: reapplies the most recent still-redoable edit. A no-op (changed: false), not an error, when there's nothing left to redo — including right after any fresh edit, which always drops whatever redo history existed before it.
    async fn redo(&self, _params: RedoParams) -> Result<CallToolResult, ErrorData> {
        let port = self.worker_port().await?;
        let result = crate::worker_client::redo(port)
            .await
            .map_err(invalid_params)?;
        Ok(CallToolResult::structured(
            serde_json::to_value(result)
                .map_err(|e| ErrorData::internal_error(e.to_string(), None))?,
        ))
    }

    /// Lists the last `limit` applied edits plus the entire current redo tail (never capped by `limit`), each with a human-readable summary and its own seq — pass that seq to history_goto to jump straight to it, forward or backward.
    async fn history(&self, params: HistoryParams) -> Result<CallToolResult, ErrorData> {
        let port = self.worker_port().await?;
        let dto = crate::worker_client::history(port, Some(params.limit))
            .await
            .map_err(invalid_params)?;
        Ok(CallToolResult::structured(
            serde_json::to_value(dto)
                .map_err(|e| ErrorData::internal_error(e.to_string(), None))?,
        ))
    }

    /// Jumps directly to a specific history step (a seq from history), in whichever direction that is from the current position — undoing or redoing as many steps as needed in one call. An unreachable or stale seq lands as far as it can rather than erroring (changed says whether anything actually moved).
    async fn history_goto(&self, params: HistoryGotoParams) -> Result<CallToolResult, ErrorData> {
        let port = self.worker_port().await?;
        let result = crate::worker_client::history_goto(port, params.seq)
            .await
            .map_err(invalid_params)?;
        Ok(CallToolResult::structured(
            serde_json::to_value(result)
                .map_err(|e| ErrorData::internal_error(e.to_string(), None))?,
        ))
    }

    /// Validates that the canvas parses as well-formed meshfox — no duplicate ids, no dangling include/deps=/env=/var references, valid meshfox:option declarations, no unrecognized attribute names. Doesn't execute anything or write the file back. Returns the node count on success; a validation failure comes back as a tool error naming the specific problem, same as node_add etc. already do for a bad write.
    async fn validate(&self, _params: ValidateParams) -> Result<CallToolResult, ErrorData> {
        let raw = self.read_raw().await?;
        let node_count = crate::validate_canvas(&raw, &self.canvas_path).map_err(invalid_params)?;
        let canvas =
            meshfox_core::Canvas::from_markdown(&raw).map_err(|e| invalid_params(e.to_string()))?;
        let warnings = meshfox_core::diagnostics::warnings(&canvas)
            .map_err(|e| invalid_params(e.to_string()))?;
        Ok(CallToolResult::structured(json!({
            "ok": true,
            "node_count": node_count,
            "warnings": warnings,
        })))
    }

    /// Runs every embedded `starlark constraint` fence's Starlark contract against the document (implies validate first, over the fully include-resolved tree) and reports pass/fail per fence. Unlike validate, one constraint failing isn't a tool error — it comes back as structured data (ok: false, with that fence's own fail() messages) so a caller can inspect what's wrong without try/catch. A parse/include failure (the document doesn't even reach a checkable state) is still a tool error, same as validate's.
    async fn check(&self, _params: CheckParams) -> Result<CallToolResult, ErrorData> {
        let raw = self.read_raw().await?;
        let results = crate::check_canvas(&raw, &self.canvas_path).map_err(invalid_params)?;
        let ok = results.iter().all(|r| r.ok);
        Ok(CallToolResult::structured(json!({
            "ok": ok,
            "results": results,
        })))
    }

    /// Finds every node matching a CSS selector — answers "which nodes have tag X" / "children of node Y" without grepping the raw file or walking node_show one node at a time. Matching runs against a synthetic document built from the canvas tree, via the same CSS engine a browser uses.
    async fn node_find(&self, params: NodeFindParams) -> Result<CallToolResult, ErrorData> {
        let raw = self.read_raw().await?;
        let canvas = Canvas::from_markdown(&raw).map_err(|e| invalid_params(e.to_string()))?;
        let ids = crate::find_node_ids(&canvas, &params.selector).map_err(invalid_params)?;
        let ids = crate::filter_by_text(&canvas, ids, params.text.as_deref());
        let ids = crate::filter_by_dates(
            &canvas,
            ids,
            params.created_after.as_deref(),
            params.created_before.as_deref(),
            params.updated_after.as_deref(),
            params.updated_before.as_deref(),
            params.since.as_deref(),
        )
        .map_err(invalid_params)?;
        let mut result = json!({ "ids": ids });
        if let Some(needle) = params.text.as_deref() {
            let excerpts: serde_json::Map<String, serde_json::Value> = ids
                .iter()
                .filter_map(|id| {
                    let n = canvas.node(id)?;
                    let e = crate::excerpt_for(n, needle)?;
                    Some((id.clone(), json!(e)))
                })
                .collect();
            result["excerpts"] = json!(excerpts);
        }
        if params.show {
            let nodes: Vec<serde_json::Value> = ids
                .iter()
                .filter_map(|id| canvas.node(id))
                .map(|n| node_json(&canvas, n, params.include_body))
                .collect();
            result["nodes"] = json!(nodes);
        }
        Ok(CallToolResult::structured(result))
    }
}

fn node_json(canvas: &Canvas, node: &meshfox_core::Node, include_body: bool) -> serde_json::Value {
    let children: Vec<&str> = canvas
        .children(&node.id)
        .iter()
        .map(|n| n.id.as_str())
        .collect();
    let extra_parents: Vec<&str> = node.extra_parents.iter().map(|e| e.from.as_str()).collect();
    let mut result = json!({
        "id": node.id,
        "title": node.title,
        "type": node.node_type.as_str(),
        "parent": node.parent,
        "children": children,
        "extra_parents": extra_parents,
        "x": node.x,
        "y": node.y,
        "width": node.width,
        "height": node.height,
        "color": node.color,
        "tags": node.tags,
        // What `node_body` needs as `base_rev` to replace this node's body.
        "body_rev": meshfox_core::body_rev(&node.text),
        "target": node.target,
        "display": node.display.map(|d| d.as_str()),
        "preview": node.preview,
        "lang": node.lang,
        "createdAt": node.created_at,
        "updatedAt": node.updated_at,
    });
    if include_body {
        result["body"] = json!(node.text);
    }
    result
}

// =======================================================================
// Root: what a host actually launches. Owns no canvas file itself — every
// canvas-scoped tool goes to that canvas's [`CanvasCtx`], which goes to its
// worker.
// =======================================================================

#[derive(Clone)]
struct MeshfoxMcpRoot {
    root: PathBuf,
    canvases: Arc<Mutex<HashMap<String, Arc<CanvasCtx>>>>,
    tool_router: ToolRouter<Self>,
}

impl MeshfoxMcpRoot {
    fn new(root: PathBuf) -> Self {
        Self {
            root,
            canvases: Arc::new(Mutex::new(HashMap::new())),
            tool_router: Self::tool_router(),
        }
    }

    /// Every open canvas, as `(id, canvas)` — a snapshot taken under the
    /// registry lock and used after it is released.
    async fn snapshot(&self) -> Vec<(String, Arc<CanvasCtx>)> {
        self.canvases
            .lock()
            .await
            .iter()
            .map(|(id, canvas)| (id.clone(), Arc::clone(canvas)))
            .collect()
    }

    /// Drops canvases that sat idle too long, ending their debug shells —
    /// that is all an open canvas holds, so it is all there is to free. One
    /// with a call in flight is not idle (the call has its own deadline).
    fn spawn_idle_sweep(&self) {
        let this = self.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(sweep_interval()).await;
                // Decided and removed under the registry lock, which `lookup`
                // also holds while it touches a canvas: a call that has just
                // found one cannot be swept out from under it.
                let idle: Vec<Arc<CanvasCtx>> = {
                    let mut canvases = this.canvases.lock().await;
                    let ids: Vec<String> = canvases
                        .iter()
                        .filter(|(_, c)| !c.is_busy() && c.idle_for() > idle_timeout())
                        .map(|(id, _)| id.clone())
                        .collect();
                    ids.into_iter()
                        .filter_map(|id| canvases.remove(&id))
                        .collect()
                };
                for canvas in idle {
                    canvas.stop_debug_sessions().await;
                }
            }
        });
    }

    /// Ends every debug shell of every open canvas: the host is gone.
    async fn shutdown(&self) {
        let all: Vec<Arc<CanvasCtx>> = self.canvases.lock().await.drain().map(|(_, c)| c).collect();
        for canvas in all {
            canvas.stop_debug_sessions().await;
        }
    }

    /// Resolves `requested` (relative to this server's own root, or
    /// absolute) to a canonical path that must live under that root —
    /// rejects `..`/absolute/symlink escapes — and derives its canvas id
    /// (the resolved path relative to the root, forward-slash separated).
    fn resolve_under_root(&self, requested: &str) -> Result<(PathBuf, String), ErrorData> {
        let requested_path = Path::new(requested);
        let joined = if requested_path.is_absolute() {
            requested_path.to_path_buf()
        } else {
            self.root.join(requested_path)
        };
        let canonical = joined.canonicalize().map_err(|e| {
            invalid_params(format!(
                "cannot resolve {requested:?} under {}: {e}",
                self.root.display()
            ))
        })?;
        if !canonical.starts_with(&self.root) {
            return Err(invalid_params(format!(
                "{requested:?} resolves outside this server's root directory ({}) — \
                 canvas_open is limited to files under it",
                self.root.display()
            )));
        }
        let rel = canonical
            .strip_prefix(&self.root)
            .expect("just checked starts_with the same root");
        let canvas_id = rel.to_string_lossy().replace('\\', "/");
        Ok((canonical, canvas_id))
    }

    /// Same boundary check as `resolve_under_root`, but for a path that
    /// doesn't have to exist yet (`canvas_open`'s own `create` flag) — the
    /// file itself can't be canonicalized before it's written, so this
    /// canonicalizes its *parent* directory instead (which does have to
    /// already exist) and rejects a missing/escaping parent the same way
    /// `resolve_under_root` rejects a missing/escaping file.
    fn resolve_under_root_for_create(
        &self,
        requested: &str,
    ) -> Result<(PathBuf, String), ErrorData> {
        let requested_path = Path::new(requested);
        let joined = if requested_path.is_absolute() {
            requested_path.to_path_buf()
        } else {
            self.root.join(requested_path)
        };
        let file_name = joined
            .file_name()
            .ok_or_else(|| invalid_params(format!("{requested:?} has no file name")))?;
        let parent = joined.parent().unwrap_or(Path::new("."));
        let canonical_parent = parent.canonicalize().map_err(|e| {
            invalid_params(format!(
                "cannot resolve the directory for {requested:?} under {}: {e}",
                self.root.display()
            ))
        })?;
        if !canonical_parent.starts_with(&self.root) {
            return Err(invalid_params(format!(
                "{requested:?} resolves outside this server's root directory ({}) — \
                 canvas_open is limited to files under it",
                self.root.display()
            )));
        }
        let canonical = canonical_parent.join(file_name);
        let rel = canonical
            .strip_prefix(&self.root)
            .expect("just checked starts_with the same root");
        let canvas_id = rel.to_string_lossy().replace('\\', "/");
        Ok((canonical, canvas_id))
    }

    /// The registry's entry for `canvas_id`, registering `resolved` under it
    /// first if there is none. No process is started: the canvas's worker is
    /// found or started by its first call.
    async fn open_canvas(&self, resolved: PathBuf, canvas_id: String) -> Arc<CanvasCtx> {
        let mut canvases = self.canvases.lock().await;
        let canvas = canvases
            .entry(canvas_id)
            .or_insert_with(|| Arc::new(CanvasCtx::new(resolved)));
        canvas.touch();
        Arc::clone(canvas)
    }

    /// The entry for `canvas_id`, opening the canvas first if it isn't open —
    /// never opened, or dropped by the idle sweep. A `canvas_id` is the canvas
    /// file's path relative to this server's root (that is all `canvas_open`
    /// ever derived it from), so it names what to open as well as what is
    /// open; a tool call therefore never needs a preceding `canvas_open` to
    /// succeed, and a canvas that went away between two calls costs the
    /// caller nothing.
    async fn lookup(&self, canvas_id: &str) -> Result<Arc<CanvasCtx>, ErrorData> {
        if let Some(canvas) = self.canvases.lock().await.get(canvas_id) {
            canvas.touch();
            return Ok(Arc::clone(canvas));
        }
        let (resolved, canonical_id) = self.resolve_under_root(canvas_id).map_err(|e| {
            invalid_params(format!(
                "canvas {canvas_id:?} is not open, and there is no canvas file by that path \
                 under {} to open on demand: {}",
                self.root.display(),
                e.message
            ))
        })?;
        Ok(self.open_canvas(resolved, canonical_id).await)
    }

    /// Runs one tool call on `canvas_id`'s context: after any call already in
    /// flight on that canvas, bounded by `deadline`, and with a panic in the
    /// handler turned into an error for this call alone.
    ///
    /// A call that outlives its deadline is abandoned, not retried: whether it
    /// was applied is unknown, and an edit could land twice. The canvas stays
    /// open; a hung worker is reported, never killed (it is shared).
    async fn call<F, Fut>(
        &self,
        canvas_id: &str,
        tool_name: &'static str,
        deadline: Duration,
        run: F,
    ) -> Result<CallToolResult, ErrorData>
    where
        F: FnOnce(Arc<CanvasCtx>) -> Fut,
        Fut: Future<Output = Result<CallToolResult, ErrorData>>,
    {
        let canvas = self.lookup(canvas_id).await?;
        let _turn = canvas.call_lock.lock().await;
        canvas.touch();
        let outcome =
            std::panic::AssertUnwindSafe(tokio::time::timeout(deadline, run(Arc::clone(&canvas))))
                .catch_unwind()
                .await;
        canvas.touch();
        match outcome {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(ErrorData::internal_error(
                format!(
                    "canvas {canvas_id:?}: {tool_name} did not finish within {}s and was \
                     abandoned; the call may or may not have been applied — read the current \
                     state before repeating it.",
                    deadline.as_secs()
                ),
                None,
            )),
            Err(_) => Err(ErrorData::internal_error(
                format!(
                    "canvas {canvas_id:?}: {tool_name} panicked; the call may or may not have \
                     been applied — read the current state before repeating it."
                ),
                None,
            )),
        }
    }
}

// ---------------------------------------------------------------------
// Root tool parameter types
// ---------------------------------------------------------------------

/// One step of a `run` call, as reported back to the agent.
#[derive(Serialize)]
struct RunStepReport {
    node_id: String,
    block: String,
    /// `ran`, `failed`, `skipped` (already fresh this session), `running`
    /// (still going when the call ended), `service_started` or `killed`.
    status: &'static str,
    exit_code: Option<i32>,
    duration_ms: Option<u64>,
    /// The tail of the step's output (stdout and stderr merged).
    output: String,
    output_truncated: bool,
}

/// What a `run` call collected from the worker's `RunEvent` stream.
#[derive(Default)]
struct RunReport {
    steps: Vec<RunStepReport>,
    timed_out: bool,
    done: Option<i32>,
    error: Option<String>,
    conflict: Option<String>,
}

impl RunReport {
    fn step_mut(&mut self, node_id: &str, block: &str) -> &mut RunStepReport {
        if let Some(i) = self
            .steps
            .iter()
            .rposition(|s| s.node_id == node_id && s.block == block && s.status == "running")
        {
            return &mut self.steps[i];
        }
        self.steps.push(RunStepReport {
            node_id: node_id.to_string(),
            block: block.to_string(),
            status: "running",
            exit_code: None,
            duration_ms: None,
            output: String::new(),
            output_truncated: false,
        });
        self.steps.last_mut().expect("just pushed")
    }

    fn running_step(&self) -> Option<&RunStepReport> {
        self.steps.iter().rev().find(|s| s.status == "running")
    }

    /// Folds one event in; `true` once the stream is over.
    fn apply(&mut self, event: crate::worker_client::RunEvent) -> bool {
        use crate::worker_client::RunEvent;
        match event {
            RunEvent::Started { .. } | RunEvent::TtyStart { .. } => {}
            RunEvent::StepStart { node_id, block } => {
                self.step_mut(&node_id, &block);
            }
            RunEvent::StepSkipped {
                node_id,
                block,
                duration_ms,
                ..
            } => {
                let step = self.step_mut(&node_id, &block);
                step.status = "skipped";
                step.duration_ms = Some(duration_ms);
            }
            RunEvent::Output {
                node_id,
                block,
                text,
                ..
            } => {
                let step = self.step_mut(&node_id, &block);
                step.output.push_str(&text);
                step.output.push('\n');
                if step.output.len() > RUN_OUTPUT_CAP_BYTES * 2 {
                    cap_tail(&mut step.output, RUN_OUTPUT_CAP_BYTES);
                    step.output_truncated = true;
                }
            }
            RunEvent::ServiceStarted { node_id, block, .. } => {
                self.step_mut(&node_id, &block).status = "service_started";
            }
            RunEvent::StepEnd {
                node_id,
                block,
                exit_code,
                duration_ms,
            } => {
                let step = self.step_mut(&node_id, &block);
                step.status = if exit_code == 0 { "ran" } else { "failed" };
                step.exit_code = Some(exit_code);
                step.duration_ms = Some(duration_ms);
            }
            RunEvent::Killed { node_id, block } => {
                self.step_mut(&node_id, &block).status = "killed";
            }
            RunEvent::LockConflict {
                node_id,
                block,
                owner_pid,
                owner_desc,
            } => {
                self.conflict = Some(format!(
                    "{node_id}/{block} is already running (pid {owner_pid}, started via {owner_desc})"
                ));
                return true;
            }
            RunEvent::Error { message } => {
                self.error = Some(message);
                return true;
            }
            RunEvent::Done { exit_code } => {
                self.done = Some(exit_code);
                return true;
            }
        }
        false
    }

    fn finish(mut self) -> Result<CallToolResult, ErrorData> {
        if let Some(conflict) = self.conflict.take() {
            return Err(invalid_params(format!(
                "can't start: {conflict} — wait for it, or stop it, then try again"
            )));
        }
        for step in &mut self.steps {
            if step.output.len() > RUN_OUTPUT_CAP_BYTES {
                cap_tail(&mut step.output, RUN_OUTPUT_CAP_BYTES);
                step.output_truncated = true;
            }
        }
        let success = !self.timed_out
            && self.error.is_none()
            && self.done == Some(0)
            && self
                .steps
                .iter()
                .all(|s| matches!(s.status, "ran" | "skipped" | "service_started"));
        let value = json!({
            "success": success,
            "exit_code": self.done,
            "timed_out": self.timed_out,
            "error": self.error,
            "steps": self.steps,
        });
        Ok(if success {
            CallToolResult::structured(value)
        } else {
            CallToolResult::structured_error(value)
        })
    }
}

/// Keeps the last `max` bytes of `text` (cut on a char boundary).
fn cap_tail(text: &mut String, max: usize) {
    if text.len() <= max {
        return;
    }
    let mut cut = text.len() - max;
    while !text.is_char_boundary(cut) {
        cut += 1;
    }
    text.drain(..cut);
}

/// Wraps any canvas tool's own parameter type with the `canvas_id` every
/// canvas-scoped tool requires — one generic wrapper instead of a bespoke
/// `canvas_id`-plus-everything-else struct per tool.
#[derive(Deserialize, JsonSchema)]
struct WithCanvas<T> {
    /// Which open canvas to operate on — from `canvas_open`.
    canvas_id: String,
    #[serde(flatten)]
    inner: T,
}

#[derive(Deserialize, JsonSchema)]
struct CanvasOpenParams {
    /// Path to the canvas file, relative to this server's own root
    /// directory (absolute is fine too, as long as it still resolves under
    /// that same root). Escaping above the root — `..`, an absolute path
    /// elsewhere, a symlink pointing out — is rejected.
    path: String,
    /// If the file doesn't exist yet, create it first (same empty
    /// `meshfox:canvas` template `meshfox create`/`meshfox view --create`
    /// use) rather than failing. A no-op if the file already exists.
    #[serde(default)]
    create: bool,
}

#[derive(Deserialize, JsonSchema)]
struct CanvasIdOnlyParams {
    canvas_id: String,
}

#[derive(Deserialize, JsonSchema, Default)]
struct EmptyParams {}

// ---------------------------------------------------------------------
// Root tools
// ---------------------------------------------------------------------

#[tool_router]
impl MeshfoxMcpRoot {
    #[tool(
        description = "Opens a canvas file for editing/debugging, registering it if it isn't already open. `path` must resolve under this server's own root directory. Returns a canvas_id — the file's path relative to that root, which every other tool takes. Opening an already-open file just returns its existing id. Calling any other tool with the canvas_id of a canvas that isn't open (never opened, or closed after sitting idle) reopens it on demand, so calling canvas_open first is only needed to create a file or to see its id. Pass `create: true` to create the file first (an empty canvas) if it doesn't exist yet — a no-op if it already does."
    )]
    async fn canvas_open(
        &self,
        Parameters(params): Parameters<CanvasOpenParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let (resolved, canvas_id) = if params.create {
            let (resolved, canvas_id) = self.resolve_under_root_for_create(&params.path)?;
            if !resolved.exists() {
                use std::io::Write;
                let content = crate::canvas_template_content(&resolved);
                std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&resolved)
                    .and_then(|mut file| file.write_all(content.as_bytes()))
                    .map_err(|e| {
                        ErrorData::internal_error(
                            format!("failed to create {}: {e}", resolved.display()),
                            None,
                        )
                    })?;
            }
            (resolved, canvas_id)
        } else {
            self.resolve_under_root(&params.path)?
        };

        // Already open: just its id.
        self.open_canvas(resolved, canvas_id.clone()).await;
        Ok(CallToolResult::structured(
            json!({ "canvas_id": canvas_id, "path": canvas_id }),
        ))
    }

    #[tool(description = "Closes an open canvas (any live debug sessions on it end too).")]
    async fn canvas_close(
        &self,
        Parameters(params): Parameters<CanvasIdOnlyParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let canvas = self
            .canvases
            .lock()
            .await
            .remove(&params.canvas_id)
            .ok_or_else(|| invalid_params(format!("no open canvas {:?}", params.canvas_id)))?;
        canvas.stop_debug_sessions().await;
        Ok(CallToolResult::structured(
            json!({ "closed": params.canvas_id }),
        ))
    }

    #[tool(
        description = "Lists every currently open canvas: its id, whether a call is in flight (busy), and its health — ok, or its worker not answering. Never waits for a busy canvas."
    )]
    async fn canvas_list(
        &self,
        Parameters(_params): Parameters<EmptyParams>,
    ) -> Result<CallToolResult, ErrorData> {
        // Nothing below waits on a canvas's call lock: a call stuck in one
        // canvas must not stop this from answering about all the others. The
        // workers are asked at once, each within the ping's own 3 seconds.
        let items = futures_util::future::join_all(self.snapshot().await.into_iter().map(
            |(id, canvas)| async move {
                json!({
                    "canvas_id": id,
                    "path": id,
                    "resolved_path": canvas.canvas_path.display().to_string(),
                    "busy": canvas.is_busy(),
                    "health": canvas.health().await,
                })
            },
        ))
        .await;
        Ok(CallToolResult::structured(json!({ "canvases": items })))
    }

    #[tool(
        description = "Lists the secrets meshfox has stored in the system secret store, from its local index: scope, name, created-at and status (ok, orphan-decl, orphan-path, unknown). Never returns values. Only entries for canvases and projects under this server's root directory are listed, plus global ones."
    )]
    async fn secret_list(
        &self,
        Parameters(_params): Parameters<EmptyParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let index = meshfox_core::secret_index::SecretIndex::default_location()
            .ok_or_else(|| ErrorData::internal_error("HOME isn't set", None))?;
        let reports = meshfox_core::secret_index::report(&index)
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
        let visible: Vec<_> = reports
            .into_iter()
            .filter(|r| secret_visible_under(&r.entry.scope, &self.root))
            .collect();
        Ok(CallToolResult::structured(
            json!({ "secrets": crate::secret_cmd::reports_json(&visible) }),
        ))
    }

    #[tool(
        description = "Same as debug_start, scoped to canvas_id (see canvas_open) — starts a persistent debug shell in that canvas."
    )]
    async fn debug_start(
        &self,
        Parameters(WithCanvas { canvas_id, inner }): Parameters<WithCanvas<DebugStartParams>>,
    ) -> Result<CallToolResult, ErrorData> {
        self.call(
            &canvas_id,
            "debug_start",
            call_deadline(None),
            |ctx| async move { ctx.debug_start(inner).await },
        )
        .await
    }

    #[tool(
        description = "Same as debug_send, scoped to canvas_id (see canvas_open) — runs code in that canvas's debug session."
    )]
    async fn debug_send(
        &self,
        Parameters(WithCanvas { canvas_id, inner }): Parameters<WithCanvas<DebugSendParams>>,
    ) -> Result<CallToolResult, ErrorData> {
        let budget = Duration::from_millis(inner.timeout_ms.unwrap_or(DEFAULT_SEND_TIMEOUT_MS));
        self.call(
            &canvas_id,
            "debug_send",
            call_deadline(Some(budget)),
            |ctx| async move { ctx.debug_send(inner).await },
        )
        .await
    }

    #[tool(
        description = "Same as debug_stop, scoped to canvas_id (see canvas_open) — stops a debug session in that canvas."
    )]
    async fn debug_stop(
        &self,
        Parameters(WithCanvas { canvas_id, inner }): Parameters<WithCanvas<DebugStopParams>>,
    ) -> Result<CallToolResult, ErrorData> {
        self.call(
            &canvas_id,
            "debug_stop",
            call_deadline(None),
            |ctx| async move { ctx.debug_stop(inner).await },
        )
        .await
    }

    #[tool(
        description = "Same as node_show, scoped to canvas_id (see canvas_open) — reads one node's structured metadata in that canvas."
    )]
    async fn node_show(
        &self,
        Parameters(WithCanvas { canvas_id, inner }): Parameters<WithCanvas<NodeIdParams>>,
    ) -> Result<CallToolResult, ErrorData> {
        self.call(
            &canvas_id,
            "node_show",
            call_deadline(None),
            |ctx| async move { ctx.node_show(inner).await },
        )
        .await
    }

    #[tool(
        description = "Same as node_add, scoped to canvas_id (see canvas_open) — adds a new child node in that canvas."
    )]
    async fn node_add(
        &self,
        Parameters(WithCanvas { canvas_id, inner }): Parameters<WithCanvas<NodeAddParams>>,
    ) -> Result<CallToolResult, ErrorData> {
        self.call(
            &canvas_id,
            "node_add",
            call_deadline(None),
            |ctx| async move { ctx.node_add(inner).await },
        )
        .await
    }

    #[tool(
        description = "Same as node_meta, scoped to canvas_id (see canvas_open) — updates a node's position/style/type fields in that canvas."
    )]
    async fn node_meta(
        &self,
        Parameters(WithCanvas { canvas_id, inner }): Parameters<WithCanvas<NodeMetaParams>>,
    ) -> Result<CallToolResult, ErrorData> {
        self.call(
            &canvas_id,
            "node_meta",
            call_deadline(None),
            |ctx| async move { ctx.node_meta(inner).await },
        )
        .await
    }

    #[tool(
        description = "Same as node_body, scoped to canvas_id (see canvas_open) — replaces a node's whole Markdown body in that canvas."
    )]
    async fn node_body(
        &self,
        Parameters(WithCanvas { canvas_id, inner }): Parameters<WithCanvas<NodeBodyParams>>,
    ) -> Result<CallToolResult, ErrorData> {
        self.call(
            &canvas_id,
            "node_body",
            call_deadline(None),
            |ctx| async move { ctx.node_body(inner).await },
        )
        .await
    }

    #[tool(
        description = "Same as node_append, scoped to canvas_id (see canvas_open) — appends to a node's Markdown body in that canvas."
    )]
    async fn node_append(
        &self,
        Parameters(WithCanvas { canvas_id, inner }): Parameters<WithCanvas<NodeAppendParams>>,
    ) -> Result<CallToolResult, ErrorData> {
        self.call(
            &canvas_id,
            "node_append",
            call_deadline(None),
            |ctx| async move { ctx.node_append(inner).await },
        )
        .await
    }

    #[tool(
        description = "Same as node_block, scoped to canvas_id (see canvas_open) — rewrites one runnable fence's attributes/code in that canvas."
    )]
    async fn node_block(
        &self,
        Parameters(WithCanvas { canvas_id, inner }): Parameters<WithCanvas<NodeBlockParams>>,
    ) -> Result<CallToolResult, ErrorData> {
        self.call(
            &canvas_id,
            "node_block",
            call_deadline(None),
            |ctx| async move { ctx.node_block(inner).await },
        )
        .await
    }

    #[tool(
        description = "Same as node_rm, scoped to canvas_id (see canvas_open) — deletes a node in that canvas."
    )]
    async fn node_rm(
        &self,
        Parameters(WithCanvas { canvas_id, inner }): Parameters<WithCanvas<NodeRmParams>>,
    ) -> Result<CallToolResult, ErrorData> {
        self.call(
            &canvas_id,
            "node_rm",
            call_deadline(None),
            |ctx| async move { ctx.node_rm(inner).await },
        )
        .await
    }

    #[tool(
        description = "Same as node_mv, scoped to canvas_id (see canvas_open) — moves a node to a new structural parent in that canvas."
    )]
    async fn node_mv(
        &self,
        Parameters(WithCanvas { canvas_id, inner }): Parameters<WithCanvas<NodeMvParams>>,
    ) -> Result<CallToolResult, ErrorData> {
        self.call(
            &canvas_id,
            "node_mv",
            call_deadline(None),
            |ctx| async move { ctx.node_mv(inner).await },
        )
        .await
    }

    #[tool(
        description = "Same as run, scoped to canvas_id (see canvas_open) — runs a block and its deps= chain in that canvas; fresh: true runs the whole chain for real."
    )]
    async fn run(
        &self,
        Parameters(WithCanvas { canvas_id, inner }): Parameters<WithCanvas<RunParams>>,
    ) -> Result<CallToolResult, ErrorData> {
        let budget = Duration::from_millis(
            inner
                .timeout_ms
                .unwrap_or(DEFAULT_RUN_TIMEOUT_MS)
                .clamp(1, MAX_RUN_TIMEOUT_MS),
        );
        self.call(
            &canvas_id,
            "run",
            call_deadline(Some(budget)),
            |ctx| async move { ctx.run(inner).await },
        )
        .await
    }

    #[tool(
        description = "Same as session_reset, scoped to canvas_id (see canvas_open) — forgets what that canvas's session remembers."
    )]
    async fn session_reset(
        &self,
        Parameters(WithCanvas { canvas_id, inner }): Parameters<WithCanvas<SessionResetParams>>,
    ) -> Result<CallToolResult, ErrorData> {
        self.call(
            &canvas_id,
            "session_reset",
            call_deadline(None),
            |ctx| async move { ctx.session_reset(inner).await },
        )
        .await
    }

    #[tool(
        description = "Same as node_rename, scoped to canvas_id (see canvas_open) — renames a node's heading text in that canvas."
    )]
    async fn node_rename(
        &self,
        Parameters(WithCanvas { canvas_id, inner }): Parameters<WithCanvas<NodeRenameParams>>,
    ) -> Result<CallToolResult, ErrorData> {
        self.call(
            &canvas_id,
            "node_rename",
            call_deadline(None),
            |ctx| async move { ctx.node_rename(inner).await },
        )
        .await
    }

    #[tool(
        description = "Same as node_set_id, scoped to canvas_id (see canvas_open) — changes a node's id in that canvas."
    )]
    async fn node_set_id(
        &self,
        Parameters(WithCanvas { canvas_id, inner }): Parameters<WithCanvas<NodeSetIdParams>>,
    ) -> Result<CallToolResult, ErrorData> {
        self.call(
            &canvas_id,
            "node_set_id",
            call_deadline(None),
            |ctx| async move { ctx.node_set_id(inner).await },
        )
        .await
    }

    #[tool(
        description = "Same as node_edges, scoped to canvas_id (see canvas_open) — adds or removes a node's extra incoming edges in that canvas."
    )]
    async fn node_edges(
        &self,
        Parameters(WithCanvas { canvas_id, inner }): Parameters<WithCanvas<NodeEdgesParams>>,
    ) -> Result<CallToolResult, ErrorData> {
        self.call(
            &canvas_id,
            "node_edges",
            call_deadline(None),
            |ctx| async move { ctx.node_edges(inner).await },
        )
        .await
    }

    #[tool(
        description = "Same as node_move, scoped to canvas_id (see canvas_open) — reorders a node among its siblings in that canvas."
    )]
    async fn node_move(
        &self,
        Parameters(WithCanvas { canvas_id, inner }): Parameters<WithCanvas<NodeMoveParams>>,
    ) -> Result<CallToolResult, ErrorData> {
        self.call(
            &canvas_id,
            "node_move",
            call_deadline(None),
            |ctx| async move { ctx.node_move(inner).await },
        )
        .await
    }

    #[tool(
        description = "Same as node_reorder, scoped to canvas_id (see canvas_open) — resyncs sibling order to canvas layout in that canvas."
    )]
    async fn node_reorder(
        &self,
        Parameters(WithCanvas { canvas_id, inner }): Parameters<WithCanvas<NodeReorderParams>>,
    ) -> Result<CallToolResult, ErrorData> {
        self.call(
            &canvas_id,
            "node_reorder",
            call_deadline(None),
            |ctx| async move { ctx.node_reorder(inner).await },
        )
        .await
    }

    #[tool(
        description = "Same as undo, scoped to canvas_id (see canvas_open) — reverts the most recent still-undoable edit in that canvas."
    )]
    async fn undo(
        &self,
        Parameters(WithCanvas { canvas_id, inner }): Parameters<WithCanvas<UndoParams>>,
    ) -> Result<CallToolResult, ErrorData> {
        self.call(&canvas_id, "undo", call_deadline(None), |ctx| async move {
            ctx.undo(inner).await
        })
        .await
    }

    #[tool(description = "Same as redo, scoped to canvas_id (see canvas_open).")]
    async fn redo(
        &self,
        Parameters(WithCanvas { canvas_id, inner }): Parameters<WithCanvas<RedoParams>>,
    ) -> Result<CallToolResult, ErrorData> {
        self.call(&canvas_id, "redo", call_deadline(None), |ctx| async move {
            ctx.redo(inner).await
        })
        .await
    }

    #[tool(
        description = "Same as history, scoped to canvas_id (see canvas_open) — lists that canvas's own recent edits."
    )]
    async fn history(
        &self,
        Parameters(WithCanvas { canvas_id, inner }): Parameters<WithCanvas<HistoryParams>>,
    ) -> Result<CallToolResult, ErrorData> {
        self.call(
            &canvas_id,
            "history",
            call_deadline(None),
            |ctx| async move { ctx.history(inner).await },
        )
        .await
    }

    #[tool(description = "Same as history_goto, scoped to canvas_id (see canvas_open).")]
    async fn history_goto(
        &self,
        Parameters(WithCanvas { canvas_id, inner }): Parameters<WithCanvas<HistoryGotoParams>>,
    ) -> Result<CallToolResult, ErrorData> {
        self.call(
            &canvas_id,
            "history_goto",
            call_deadline(None),
            |ctx| async move { ctx.history_goto(inner).await },
        )
        .await
    }

    #[tool(
        description = "Same as validate, scoped to canvas_id (see canvas_open) — checks that canvas parses as well-formed meshfox."
    )]
    async fn validate(
        &self,
        Parameters(WithCanvas { canvas_id, inner }): Parameters<WithCanvas<ValidateParams>>,
    ) -> Result<CallToolResult, ErrorData> {
        self.call(
            &canvas_id,
            "validate",
            call_deadline(None),
            |ctx| async move { ctx.validate(inner).await },
        )
        .await
    }

    #[tool(
        description = "Same as check, scoped to canvas_id (see canvas_open) — runs that canvas's own constraint fences and reports pass/fail per fence."
    )]
    async fn check(
        &self,
        Parameters(WithCanvas { canvas_id, inner }): Parameters<WithCanvas<CheckParams>>,
    ) -> Result<CallToolResult, ErrorData> {
        self.call(&canvas_id, "check", call_deadline(None), |ctx| async move {
            ctx.check(inner).await
        })
        .await
    }

    #[tool(
        description = "Same as node_find, scoped to canvas_id (see canvas_open) — finds nodes matching a CSS selector in that canvas."
    )]
    async fn node_find(
        &self,
        Parameters(WithCanvas { canvas_id, inner }): Parameters<WithCanvas<NodeFindParams>>,
    ) -> Result<CallToolResult, ErrorData> {
        self.call(
            &canvas_id,
            "node_find",
            call_deadline(None),
            |ctx| async move { ctx.node_find(inner).await },
        )
        .await
    }
}

/// Whether an index scope may be shown to an MCP client rooted at `root`:
/// a `doc:`/`project:` scope only when its path is under `root`, so an agent
/// working in one tree can't enumerate the rest of the machine; `global`
/// scopes always.
fn secret_visible_under(scope: &str, root: &std::path::Path) -> bool {
    match meshfox_core::secret_index::scope_path(scope) {
        Some(path) => path.starts_with(root),
        None => true,
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for MeshfoxMcpRoot {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build()).with_instructions(
            "Multi-canvas meshfox MCP server. Every canvas-scoped tool requires a canvas_id from \
             canvas_open first — there is no implicit 'current' canvas. canvas_open/canvas_close/ \
             canvas_list manage a registry of canvases, each served \
             by that file's own worker (a hung worker is reported by canvas_list and fails only \
             that canvas's calls). canvas_open only resolves paths under this server's own root directory \
             (the directory of the canvas path meshfox mcp was launched with) — it refuses to open \
             anything above that. Every other tool mirrors its single-canvas equivalent exactly, just \
             with canvas_id added as the first argument. Prefer these node_* tools over hand-editing \
             a .canvas.md file's text directly for structural changes — ids, parent=, meshfox:edge \
             targets, heading depth, sibling order. mdcanvas validates the whole resulting document \
             on every write, so a bad hand-edit can land as a corrupt file instead of failing loudly. \
             Editing prose inside an existing node's body by hand is fine; node_body does the same \
             thing through this surface.",
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{node_json, secret_visible_under, MeshfoxMcpRoot};
    use meshfox_core::Canvas;
    use meshfox_server::debug_session::DebugSession;
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Duration;

    #[test]
    fn secret_list_only_shows_scopes_under_the_root_plus_global() {
        let root = std::path::Path::new("/work/proj");
        assert!(secret_visible_under("doc:/work/proj/a.canvas.md", root));
        assert!(secret_visible_under("project:/work/proj/sub", root));
        assert!(secret_visible_under("global", root));
        assert!(secret_visible_under("global:~/work", root));
        assert!(!secret_visible_under("doc:/work/other/a.canvas.md", root));
        assert!(!secret_visible_under(
            "doc:/work/project2/a.canvas.md",
            root
        ));
        assert!(!secret_visible_under("project:/elsewhere", root));
    }

    #[tokio::test]
    async fn debug_session_has_no_controlling_terminal() {
        let cwd = std::env::temp_dir();
        let mut session = DebugSession::spawn(&cwd, HashMap::new()).unwrap();
        // Without a controlling terminal, `/dev/tty` can't be opened at all
        // (`ENXIO`) — this is what stops libpq's password prompt from
        // blocking forever on a tty nobody will ever write to (see
        // TODO.canvas.md's "PGPASSWORD-подстановка..." node).
        let outcome = session
            .send(": > /dev/tty", Duration::from_secs(5))
            .await
            .unwrap();
        assert!(
            !outcome.timed_out && outcome.exit_code != 0,
            "writing to /dev/tty should fail fast, not hang or succeed: {outcome:?}"
        );
        session.stop().await;
    }

    #[tokio::test]
    async fn debug_send_timeout_kills_a_command_that_exits_on_sigterm() {
        let cwd = std::env::temp_dir();
        let mut session = DebugSession::spawn(&cwd, HashMap::new()).unwrap();
        let outcome = session
            .send("sleep 30", Duration::from_millis(200))
            .await
            .unwrap();
        assert!(outcome.timed_out);
        assert!(outcome.session_ended);
        assert!(
            session.has_exited(),
            "the shell should already be reaped by the time send() returns"
        );
    }

    #[tokio::test]
    async fn debug_send_timeout_escalates_to_sigkill_when_sigterm_is_ignored() {
        let cwd = std::env::temp_dir();
        let mut session = DebugSession::spawn(&cwd, HashMap::new()).unwrap();
        let outcome = session
            .send("trap '' TERM; sleep 30", Duration::from_millis(200))
            .await
            .unwrap();
        assert!(outcome.timed_out);
        assert!(outcome.session_ended);
        assert!(
            session.has_exited(),
            "SIGKILL after the grace period should have reaped the shell by now"
        );
    }

    #[test]
    fn node_json_omits_body_by_default_and_includes_it_when_asked() {
        let markdown = "<!-- meshfox:canvas -->\n# Root\n\n## Child\n<!-- meshfox:node id=\"child\" -->\n\nsome node text\n";
        let canvas = Canvas::from_markdown(markdown).unwrap();
        let node = canvas.node("child").unwrap();

        let without_body = node_json(&canvas, node, false);
        assert!(without_body.get("body").is_none());

        let with_body = node_json(&canvas, node, true);
        assert_eq!(with_body["body"], "some node text");
    }

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    /// A fresh `<root>/a.canvas.md`, `<root>/sub/b.canvas.md`, and a sibling
    /// `<root>/../outside.canvas.md` — enough to exercise every
    /// `resolve_under_root` outcome without spawning any real MCP process.
    fn fixture() -> (std::path::PathBuf, std::path::PathBuf) {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let base = std::env::temp_dir().join(format!("meshfox-mcp-root-test-{nanos}-{n}"));
        let root = base.join("root");
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::write(root.join("a.canvas.md"), "").unwrap();
        std::fs::write(root.join("sub/b.canvas.md"), "").unwrap();
        std::fs::write(base.join("outside.canvas.md"), "").unwrap();
        (base, root.canonicalize().unwrap())
    }

    #[test]
    fn resolve_under_root_accepts_a_file_directly_in_the_root() {
        let (_base, root) = fixture();
        let server = MeshfoxMcpRoot::new(root.clone());
        let (resolved, id) = server.resolve_under_root("a.canvas.md").unwrap();
        assert_eq!(resolved, root.join("a.canvas.md"));
        assert_eq!(id, "a.canvas.md");
    }

    #[test]
    fn resolve_under_root_accepts_a_nested_file_and_normalizes_the_id() {
        let (_base, root) = fixture();
        let server = MeshfoxMcpRoot::new(root.clone());
        let (resolved, id) = server.resolve_under_root("sub/b.canvas.md").unwrap();
        assert_eq!(resolved, root.join("sub").join("b.canvas.md"));
        assert_eq!(id, "sub/b.canvas.md");
    }

    #[test]
    fn resolve_under_root_rejects_a_dot_dot_escape() {
        let (_base, root) = fixture();
        let server = MeshfoxMcpRoot::new(root);
        let err = server
            .resolve_under_root("../outside.canvas.md")
            .unwrap_err();
        assert!(
            err.message.contains("outside this server's root directory"),
            "{err:?}"
        );
    }

    #[test]
    fn resolve_under_root_rejects_an_absolute_path_outside_the_root() {
        let (base, root) = fixture();
        let server = MeshfoxMcpRoot::new(root);
        let outside = base.join("outside.canvas.md");
        let err = server
            .resolve_under_root(outside.to_str().unwrap())
            .unwrap_err();
        assert!(
            err.message.contains("outside this server's root directory"),
            "{err:?}"
        );
    }

    #[test]
    fn resolve_under_root_rejects_a_nonexistent_path() {
        let (_base, root) = fixture();
        let server = MeshfoxMcpRoot::new(root);
        assert!(server.resolve_under_root("no-such-file.canvas.md").is_err());
    }

    #[test]
    fn resolve_under_root_same_path_yields_the_same_id_every_time() {
        let (_base, root) = fixture();
        let server = MeshfoxMcpRoot::new(root);
        let (_, id1) = server.resolve_under_root("sub/b.canvas.md").unwrap();
        let (_, id2) = server.resolve_under_root("sub/b.canvas.md").unwrap();
        assert_eq!(id1, id2);
    }

    #[test]
    fn resolve_under_root_for_create_accepts_a_new_file_in_an_existing_subdir() {
        let (_base, root) = fixture();
        let server = MeshfoxMcpRoot::new(root.clone());
        let (resolved, id) = server
            .resolve_under_root_for_create("sub/new.canvas.md")
            .unwrap();
        assert_eq!(resolved, root.join("sub").join("new.canvas.md"));
        assert_eq!(id, "sub/new.canvas.md");
        assert!(!resolved.exists());
    }

    #[test]
    fn resolve_under_root_for_create_rejects_a_dot_dot_escape() {
        let (_base, root) = fixture();
        let server = MeshfoxMcpRoot::new(root);
        let err = server
            .resolve_under_root_for_create("../new.canvas.md")
            .unwrap_err();
        assert!(
            err.message.contains("outside this server's root directory"),
            "{err:?}"
        );
    }

    #[test]
    fn resolve_under_root_for_create_rejects_a_missing_parent_directory() {
        let (_base, root) = fixture();
        let server = MeshfoxMcpRoot::new(root);
        assert!(server
            .resolve_under_root_for_create("no-such-dir/new.canvas.md")
            .is_err());
    }
}
