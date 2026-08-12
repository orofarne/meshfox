//! The local HTTP backend behind `meshfox view`: a tiny API over a single
//! `.canvas.md` file, plus the built web UI, embedded into whatever binary
//! links this crate (via `rust-embed`, from `web/dist` at compile time —
//! run `cd web && npm run build` before building anything that depends on
//! this crate, or the UI route just serves a "not built" message).
//!
//! No database — the `.canvas.md` file on disk is the source of truth. The
//! server keeps the raw text in memory for a session and patches it
//! surgically (via `meshfox_core::mdcanvas`) on every edit / block run, so
//! saves never reformat parts of the file the user didn't touch.

use axum::{
    body::{Body, Bytes},
    extract::ws::{Message, WebSocket, WebSocketUpgrade},
    extract::{Path, Query, State},
    http::{header, StatusCode, Uri},
    response::{IntoResponse, Response},
    routing::{get, patch, post},
    Json, Router,
};
use meshfox_core::{
    mdcanvas, Canvas, ExecOutput, ExtraEdge, FileDisplay, NodeMeta, NodeType, RunError, VarCache,
};
use rust_embed::RustEmbed;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{broadcast, oneshot};
use tower_http::cors::CorsLayer;

/// `pub` so `meshfox-cli` can reuse the same async spawn/kill primitives
/// for `meshfox run`'s real-time output — see its `main.rs`.
pub mod stream_exec;
mod pty_exec;

#[derive(RustEmbed)]
#[folder = "../../web/dist"]
struct WebAssets;

struct AppState {
    canvas_path: PathBuf,
    raw: Mutex<String>,
    /// In-flight runs, keyed by `runId` — a `Sender` fired by `/api/kill`
    /// to cancel that run. Entries are removed by `RunGuard` (below)
    /// whenever a run's stream ends, however it ends (finished, a step
    /// failed, killed, or the client just disconnected mid-stream).
    runs: Mutex<HashMap<String, oneshot::Sender<()>>>,
    /// Resolved `meshfox:var` answers (see `meshfox_core::vars`), loaded
    /// once at startup and updated in place (immediately persisted to
    /// disk on every write) whenever a run supplies a fresh non-secret
    /// value — see `run_block`/`get_vars`.
    vars_cache: Mutex<VarCache>,
    /// Number of currently-connected `/api/watch` streams — one per open
    /// browser tab (or other long-lived client), see `watch_changes`/
    /// `TabGuard`. Used to auto-exit the process once every tab has closed.
    open_tabs: AtomicUsize,
    /// Whether `/api/watch` has ever been connected to at all — guards
    /// against `TabGuard` exiting the process before the auto-opened (or
    /// manually visited) tab has even had a chance to connect yet.
    ever_connected: AtomicBool,
    /// Fires whenever the on-disk file changes for a reason other than this
    /// server's own writes (see `spawn_file_watcher`) — `watch_changes`
    /// forwards each one to its connected client as a `changed` event so
    /// the UI can reload.
    change_tx: broadcast::Sender<()>,
    /// Whether the process should exit on its own once every `/api/watch`
    /// connection has gone (see `TabGuard`) — off for e.g. the e2e test
    /// server, which cycles through pages with brief all-tabs-closed gaps
    /// between tests that a real user closing their last tab wouldn't have.
    auto_exit: bool,
}

impl AppState {
    fn save(&self, raw: &str) -> std::io::Result<()> {
        std::fs::write(&self.canvas_path, raw)
    }
}

/// How long `TabGuard` waits, after the last `/api/watch` connection drops,
/// before actually exiting — long enough that a page reload (which briefly
/// closes the old connection before the new page opens one) or a test
/// runner's own brief between-pages gap never trips it, short enough that a
/// user closing their last real tab doesn't leave the process lingering.
const AUTO_EXIT_GRACE: Duration = Duration::from_secs(10);

/// Dropped when a `/api/watch` client disconnects for any reason, including
/// simply closing its browser tab — axum notices when the underlying
/// stream stops being polled and drops the `async_stream::stream!`
/// generator, which drops this along with it. Decrements the open-tab
/// count and, if that was the last one and `auto_exit` is on, schedules a
/// delayed re-check that actually exits the process if it's still zero.
struct TabGuard {
    state: Arc<AppState>,
}

impl Drop for TabGuard {
    fn drop(&mut self) {
        let remaining = self.state.open_tabs.fetch_sub(1, Ordering::SeqCst) - 1;
        if remaining == 0 && self.state.auto_exit && self.state.ever_connected.load(Ordering::SeqCst) {
            let state = Arc::clone(&self.state);
            tokio::spawn(async move {
                tokio::time::sleep(AUTO_EXIT_GRACE).await;
                if state.open_tabs.load(Ordering::SeqCst) == 0 {
                    println!("meshfox: last open tab closed, exiting");
                    std::process::exit(0);
                }
            });
        }
    }
}

/// Polls `canvas_path`'s mtime on a plain OS thread — simpler and more
/// portable than pulling in a filesystem-event-notification dependency, and
/// cheap enough at this interval for a single file — reloading `state.raw`
/// whenever the file's content actually differs from what the server
/// already has in memory, and broadcasting a `changed` event over
/// `/api/watch` so every open tab reloads. Comparing content, not just
/// mtime, is what keeps this from re-broadcasting the server's own writes
/// back to itself: by the time this notices the mtime bump from
/// `AppState::save`, `state.raw` already matches what's now on disk, so
/// nothing looks different and nothing is sent.
fn spawn_file_watcher(state: Arc<AppState>) {
    std::thread::spawn(move || {
        let mut last_mtime = std::fs::metadata(&state.canvas_path).and_then(|m| m.modified()).ok();
        loop {
            std::thread::sleep(Duration::from_millis(500));
            let Ok(meta) = std::fs::metadata(&state.canvas_path) else { continue };
            let Ok(mtime) = meta.modified() else { continue };
            if Some(mtime) == last_mtime {
                continue;
            }
            last_mtime = Some(mtime);
            let Ok(contents) = std::fs::read_to_string(&state.canvas_path) else { continue };
            let mut raw = state.raw.lock().unwrap();
            if *raw != contents {
                *raw = contents;
                drop(raw);
                let _ = state.change_tx.send(());
            }
        }
    });
}

/// Removes this run's registry entry when dropped — covers every way a
/// run's stream can end, including the client disconnecting mid-stream
/// (which drops the `async_stream::stream!` generator without running any
/// more of its body), not just the "reached the end normally" case a plain
/// cleanup call at the bottom of the loop would miss.
struct RunGuard {
    state: Arc<AppState>,
    run_id: String,
}

impl Drop for RunGuard {
    fn drop(&mut self) {
        self.state.runs.lock().unwrap().remove(&self.run_id);
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RunRequest {
    /// Node-id path from the root's children down to the node that owns
    /// the block, e.g. `["tests", "smoke-test"]`.
    path: Vec<String>,
    /// `name` attribute of the runnable code fence to execute.
    block: String,
    /// Whether a `cache`d block's output should actually be written into
    /// the file. Running is always allowed (view-only or not); this is
    /// what the UI's Edit toggle actually gates — a read-only view can run
    /// a block to see its output without touching the file.
    #[serde(default)]
    persist: bool,
    /// Skip `block`'s `deps=` chain and run only `block` itself — what the
    /// UI's plain "run" button (as opposed to "⛓ run chain") sends.
    #[serde(default)]
    no_deps: bool,
    /// Answers for any `meshfox:var` the UI's pre-run form just collected
    /// (see `GET /api/vars`) — takes precedence over the process
    /// environment/cache/default, same as the CLI's `--set`. Every
    /// non-secret entry here is persisted to the var cache before the run
    /// starts, so the next run doesn't ask again.
    #[serde(default)]
    vars: HashMap<String, String>,
}

/// One declared `meshfox:var`'s current status — what `GET /api/vars`
/// returns, so the UI can pre-fill a form for whatever's already resolved
/// and only actually prompt for what's `resolved: false`. A `secret`
/// variable's `value` is always omitted (even if it happens to already be
/// resolved via the server process's own environment) — no reason to ever
/// put a secret on the wire if the browser doesn't need to ask for it.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct VarStatus {
    name: String,
    #[serde(rename = "type")]
    var_type: &'static str,
    prompt: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    choices: Vec<String>,
    secret: bool,
    resolved: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    value: Option<String>,
}

/// Addresses the same block `RunRequest` does, flattened into query
/// params since a `GET` has no JSON body — `path` is the node-id path
/// joined with commas (empty for a root-level block).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct VarsQuery {
    #[serde(default)]
    path: String,
    block: String,
    #[serde(default)]
    no_deps: bool,
}

/// Only the declared `meshfox:var`s the requested block's chain actually
/// references (via `env=` — see `meshfox_core::env_var_names_for_chain`),
/// each with its current resolve-without-prompting status (env/cache/
/// default, no overrides, since this is the pre-run "what do you still
/// need to ask" check) — see SPEC.md's "Variables". A block (and its
/// `deps=` chain) that declares no `env=` at all yields an empty list, so
/// the UI never prompts for anything it doesn't need.
async fn get_vars(
    State(state): State<Arc<AppState>>,
    Query(query): Query<VarsQuery>,
) -> Result<Json<Vec<VarStatus>>, ApiError> {
    let raw = state.raw.lock().unwrap().clone();
    let canvas = parse_or_error(&raw)?;
    let path: Vec<&str> = if query.path.is_empty() { Vec::new() } else { query.path.split(',').collect() };
    let chain = meshfox_core::resolve_run_chain(&canvas, &path, &query.block, !query.no_deps)?;
    let needed = meshfox_core::env_var_names_for_chain(&canvas, &chain);

    let decls = meshfox_core::declared_vars(&canvas)
        .map_err(|e| ApiError(StatusCode::UNPROCESSABLE_ENTITY, e.to_string()))?;
    let relevant: Vec<_> = decls.into_iter().filter(|d| needed.contains(&d.name)).collect();

    let cache = state.vars_cache.lock().unwrap();
    let resolved = meshfox_core::resolve_vars(&relevant, &HashMap::new(), &cache);
    let statuses = relevant
        .into_iter()
        .map(|d| {
            let resolved_value = resolved.values.get(&d.name).cloned();
            VarStatus {
                name: d.name.clone(),
                var_type: d.var_type.as_str(),
                prompt: d.prompt,
                choices: d.choices,
                secret: d.secret,
                resolved: resolved_value.is_some(),
                value: if d.secret { None } else { resolved_value },
            }
        })
        .collect();
    Ok(Json(statuses))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct KillRequest {
    run_id: String,
}

/// One line of `/api/run`'s streamed NDJSON response body (`application/
/// x-ndjson`, one `RunEvent` per line) — see SPEC.md's "Runnable code
/// fences" section for the full protocol. Emitted for the requested block
/// and, automatically, every block its `deps=` chain pulls in first.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "kebab-case", rename_all_fields = "camelCase")]
enum RunEvent {
    /// Always first. `runId` is what `/api/kill` takes to cancel this run.
    Started { run_id: String },
    StepStart { node_id: String, block: String },
    /// One line of merged stdout/stderr, as it's produced.
    Output { node_id: String, block: String, text: String },
    /// `/api/run/tty`'s WebSocket only: emitted right after `StepStart` for
    /// a `tty` step, instead of any `Output`. From here until the matching
    /// `StepEnd`, every other WebSocket frame for this run is raw pty I/O,
    /// not a `RunEvent` — binary frames are pty output bytes (server to
    /// client) or input bytes to type into the pty (client to server); a
    /// client text frame in this window is a resize control message
    /// (`{"cols":..,"rows":..}`), not a `RunEvent`. See SPEC.md's
    /// "Interactive (`tty`) blocks" — `/api/run`'s plain NDJSON stream
    /// never runs a `tty` block to begin with, so it never emits this.
    TtyStart { node_id: String, block: String },
    StepEnd { node_id: String, block: String, exit_code: i32 },
    /// Terminal for this run — no `Done` follows. Emitted for whichever
    /// step was actively running when `/api/kill` fired; later chain steps
    /// (if any) never start.
    Killed { node_id: String, block: String },
    /// Terminal for this run — something failed before/without a step
    /// producing a normal exit code (bad node/block reference, no
    /// executor for the language, an I/O error spawning the process).
    Error { message: String },
    /// Terminal for this run. `exitCode` mirrors whichever step ran last —
    /// the requested block's own, unless an earlier dependency failed and
    /// stopped the chain first (same stop-on-failure rule `meshfox run`
    /// already has).
    Done { exit_code: i32 },
}

fn ndjson_line(event: &RunEvent) -> Bytes {
    let mut line = serde_json::to_string(event).expect("RunEvent always serializes");
    line.push('\n');
    Bytes::from(line)
}

struct ApiError(StatusCode, String);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, self.1).into_response()
    }
}

impl From<RunError> for ApiError {
    fn from(err: RunError) -> Self {
        let status = match &err {
            RunError::Tree(_) | RunError::BlockNotFound(_, _) => StatusCode::NOT_FOUND,
            RunError::NoExecutor(_) | RunError::Deps(_) => StatusCode::UNPROCESSABLE_ENTITY,
            RunError::Io(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };
        ApiError(status, err.to_string())
    }
}

fn parse_or_error(raw: &str) -> Result<Canvas, ApiError> {
    Canvas::from_markdown(raw).map_err(|e| ApiError(StatusCode::UNPROCESSABLE_ENTITY, e.to_string()))
}

/// Builds the same response shape `GET /api/canvas` returns (parse, splice
/// in `include`s) from a given raw document — shared by `get_canvas` and
/// every mutating endpoint below, so a create/update response always looks
/// exactly like a fresh `GET` would. Unpositioned nodes are sent over the
/// wire exactly as parsed (no server-computed layout suggestion) — the web
/// client lays those out itself, client-side, since it's the one that
/// actually knows the browser's viewport size and each node's real
/// rendered content height (see `web/src/autolayout.ts`); `meshfox fmt` is
/// `crate::layout`'s only remaining caller.
fn canvas_response(raw: &str, canvas_path: &std::path::Path) -> Result<Json<Canvas>, ApiError> {
    let canvas = parse_or_error(raw)?;
    // Dynamically splice in every `include` node's target — never written
    // back to the file (see `meshfox_core::include`), so a client editing
    // and PUTting this response back would silently drop any include-only
    // content; the UI treats included subtrees as read-only for now.
    let mut canvas = meshfox_core::include::resolve(&canvas, canvas_path)
        .map_err(|e| ApiError(StatusCode::UNPROCESSABLE_ENTITY, e.to_string()))?;
    // Every `constraint` node's script is meant to be cheap and pure (no
    // I/O, tick/heap/callstack-bounded — see `constraint::evaluate`), so
    // running them all on every fetch (rather than only on an explicit
    // `meshfox check`) is safe and keeps the UI's pass/fail badges current
    // without a separate endpoint or a stale on-disk cache to invalidate.
    meshfox_core::constraint::annotate_status(&mut canvas);
    Ok(Json(canvas))
}

async fn get_canvas(State(state): State<Arc<AppState>>) -> Result<Json<Canvas>, ApiError> {
    let raw = state.raw.lock().unwrap().clone();
    canvas_response(&raw, &state.canvas_path)
}

/// The document's raw Markdown text, verbatim — what the UI's Source-mode
/// editor loads. Unlike `get_canvas`, this does *not* splice in `include`s;
/// it's the actual on-disk bytes this file owns.
async fn get_canvas_raw(State(state): State<Arc<AppState>>) -> String {
    state.raw.lock().unwrap().clone()
}

/// Overwrites the whole document with `body`, verbatim — the Source-mode
/// editor's Save button. Rejects (422, nothing written) anything that
/// doesn't parse, same validate-before-commit guarantee every other
/// mutating endpoint here gives; that's what keeps the editor from ever
/// persisting invalid Markdown.
async fn put_canvas_raw(
    State(state): State<Arc<AppState>>,
    body: String,
) -> Result<StatusCode, ApiError> {
    parse_or_error(&body)?;
    state
        .save(&body)
        .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    *state.raw.lock().unwrap() = body;
    Ok(StatusCode::NO_CONTENT)
}

/// Saves the position/size/color of every node in `canvas` back into the
/// file, patching each node's `meshfox:node` comment line in place (see
/// `mdcanvas::set_node_meta`) rather than regenerating the whole document —
/// then reorders each parent's children in the document to match the new
/// layout (top-to-bottom, then left-to-right; see
/// `mdcanvas::reorder_by_position`), so the on-disk heading order always
/// tracks the canvas instead of just freezing whatever order a node was
/// first created in.
async fn put_canvas(
    State(state): State<Arc<AppState>>,
    Json(canvas): Json<Canvas>,
) -> Result<StatusCode, ApiError> {
    let mut raw = state.raw.lock().unwrap().clone();
    for node in &canvas.nodes {
        // A group's box is always derived from its children by the UI, never
        // authored — ignore whatever position it reports rather than let a
        // computed value get written into the file as if it were real data.
        // Include nodes never reach the client as such (`get_canvas`
        // resolves them away first) but are skipped here too for the same
        // reason a stray unknown id already is below: `set_node_meta` finds
        // nothing to patch and no-ops, so this is belt-and-suspenders.
        if node.node_type == NodeType::Group || node.node_type == NodeType::Include {
            continue;
        }
        let meta = NodeMeta {
            x: node.x,
            y: node.y,
            width: node.width,
            height: node.height,
            color: node.color.clone(),
            node_type: None,
            display: node.display,
            lang: node.lang.clone(),
            tags: node.tags.clone(),
        };
        // Nodes spliced in from an included file have no `meshfox:node`
        // comment in *this* file, so this is a no-op for them too — their
        // layout isn't persisted anywhere yet (see `get_canvas`).
        if let Some(patched) = mdcanvas::set_node_meta(&raw, &node.id, &meta) {
            raw = patched;
        }
    }
    if let Some(reordered) = mdcanvas::reorder_by_position(&raw) {
        raw = reordered;
    }
    state
        .save(&raw)
        .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    *state.raw.lock().unwrap() = raw;
    Ok(StatusCode::NO_CONTENT)
}

/// Clears every non-group node's stored `x`/`y`/`width`/`height` back to
/// unset, reverting the whole document to auto-placed (see
/// `web/src/autolayout.ts`) — the web UI's "Auto-layout" button, for
/// starting over after a canvas's manual layout has gotten tangled.
/// Destructive (there's no undo beyond the file's own version control), so
/// the client is expected to confirm before calling this. Reuses
/// `mdcanvas::set_node_meta` exactly like `put_canvas` does, just with
/// `x`/`y`/`width`/`height` always `None` (which *omits* those attributes
/// from the rewritten `meshfox:node` line, rather than the "leave
/// unchanged" meaning callers get by passing the node's own current value
/// through, the way `put_canvas` does) — every other field is carried over
/// from the node's current value so only the layout is actually cleared. A
/// group's box is always derived, never stored, so there's nothing to clear
/// on one. Unlike `put_canvas` (which only ever sees the *resolved* canvas,
/// where an `include` node has already been rewritten to `text`/`group` by
/// `include::resolve`), this reads straight off the raw, unresolved parse —
/// here, an `include` node is still the node that *declares* the include
/// right in this file, with its own real `meshfox:node` comment (position
/// and all), so it must be cleared exactly like any other node.
async fn clear_layout(State(state): State<Arc<AppState>>) -> Result<Json<Canvas>, ApiError> {
    let mut raw = state.raw.lock().unwrap().clone();
    let canvas = parse_or_error(&raw)?;
    for node in &canvas.nodes {
        if node.node_type == NodeType::Group {
            continue;
        }
        let meta = NodeMeta {
            x: None,
            y: None,
            width: None,
            height: None,
            color: node.color.clone(),
            node_type: None,
            display: node.display,
            lang: node.lang.clone(),
            tags: node.tags.clone(),
        };
        if let Some(patched) = mdcanvas::set_node_meta(&raw, &node.id, &meta) {
            raw = patched;
        }
    }
    state
        .save(&raw)
        .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    *state.raw.lock().unwrap() = raw.clone();
    canvas_response(&raw, &state.canvas_path)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateNodeRequest {
    parent_id: String,
    title: String,
}

/// Adds a new, empty-bodied child heading node under `parentId`, as the
/// last item in its existing subtree (see `mdcanvas::insert_child_node`).
/// Deliberately doesn't set a position — the web client's own auto-layout
/// places it using the same tree-aware default any other position-less
/// node gets, which for a fresh child means "to the right of its parent",
/// exactly what the UI's "add child" button wants without any extra
/// placement logic here.
async fn create_node(
    State(state): State<Arc<AppState>>,
    Json(req): Json<CreateNodeRequest>,
) -> Result<Json<Canvas>, ApiError> {
    let raw = state.raw.lock().unwrap().clone();
    let (updated, _new_id) = mdcanvas::insert_child_node(&raw, &req.parent_id, &req.title)
        .ok_or_else(|| ApiError(StatusCode::NOT_FOUND, format!("no node {:?}", req.parent_id)))?;
    // Insertion can't actually break parsing, but validate anyway — same
    // validate-before-commit shape every other mutating endpoint here uses.
    parse_or_error(&updated)?;
    state
        .save(&updated)
        .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    *state.raw.lock().unwrap() = updated.clone();
    canvas_response(&updated, &state.canvas_path)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdateNodeRequest {
    title: Option<String>,
    node_type: Option<NodeType>,
    color: Option<String>,
    /// New link target for a `file`/`link` node — written as the node's
    /// whole body (`[title](target)`), replacing whatever body was there.
    target: Option<String>,
    /// New raw Markdown body for a `text` node — what the web UI's
    /// CodeMirror editor sends. Mutually exclusive with `target` in
    /// practice (the client only ever sends one, depending on node type),
    /// but nothing here enforces that beyond whichever one is applied last
    /// winning.
    text: Option<String>,
    /// Full replacement list of extra incoming edges (`meshfox:edge
    /// from="..."`) — `None` leaves them untouched, `Some(vec![])` removes
    /// them all.
    extra_parents: Option<Vec<ExtraEdge>>,
    /// `file`-node display mode (see `FileDisplay`) — `None` leaves the
    /// existing value untouched, same "not sent" convention as every other
    /// field here.
    display: Option<FileDisplay>,
    /// `file`-node syntax-highlighting language hint.
    lang: Option<String>,
    /// Full replacement list of tags — `None` leaves them untouched,
    /// `Some(vec![])` clears them, same convention as `extraParents`.
    tags: Option<Vec<String>>,
}

/// Applies any of `title`/`nodeType`/`color`/`target`/`text`/`extraParents`/
/// `display`/`lang`/`tags` present in the request to node `id`, validating
/// the fully-patched
/// document parses before saving anything — an invalid combination (e.g.
/// `target` on a still-`text` node, or `nodeType: group` with a non-empty
/// body) is rejected with `422` and leaves the file untouched, rather than
/// partially applying edits.
async fn update_node(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(req): Json<UpdateNodeRequest>,
) -> Result<Json<Canvas>, ApiError> {
    let mut raw = state.raw.lock().unwrap().clone();

    let not_found = || ApiError(StatusCode::NOT_FOUND, format!("no node {id:?}"));

    // Read the node's current fields once, up front — every mutation below
    // is a surgical splice that doesn't require (and, for a type change
    // that hasn't had its body fixed up yet, must NOT require) the
    // document to still parse cleanly in between. Only the fully-patched
    // result is validated, at the very end. Re-parsing after e.g. the type
    // change below (as an earlier version of this function did, to look up
    // the node's title for the `target` step) would spuriously reject a
    // still-in-progress edit — switching a node to `file`/`link` and
    // supplying its target in the same request — since the type would
    // already say `file`/`link` while the body briefly still isn't a
    // single link.
    let initial = parse_or_error(&raw)?;
    let initial_node = initial.node(&id).ok_or_else(not_found)?;
    let (x, y, width, height, existing_color, existing_display, existing_lang, existing_tags) = (
        initial_node.x,
        initial_node.y,
        initial_node.width,
        initial_node.height,
        initial_node.color.clone(),
        initial_node.display,
        initial_node.lang.clone(),
        initial_node.tags.clone(),
    );
    // `display`/`lang` only mean anything on a `file` node — clear them
    // (rather than leave a stale attribute behind) whenever this request
    // moves the node to some other type.
    let final_type = req.node_type.unwrap_or(initial_node.node_type);
    let mut title = initial_node.title.clone();

    if let Some(new_title) = &req.title {
        raw = mdcanvas::set_node_title(&raw, &id, new_title).ok_or_else(not_found)?;
        title = new_title.clone();
    }

    if req.title.is_some()
        || req.node_type.is_some()
        || req.color.is_some()
        || req.display.is_some()
        || req.lang.is_some()
        || req.tags.is_some()
    {
        // This also has the side effect of pinning the node's `id=`
        // attribute explicitly the moment any of its metadata changes,
        // same as any other first write-back (see `canvas.rs`'s doc
        // comment on `id`).
        let (display, lang) = if final_type == NodeType::File {
            (req.display.or(existing_display), req.lang.clone().or(existing_lang))
        } else {
            (None, None)
        };
        let meta = NodeMeta {
            x,
            y,
            width,
            height,
            color: req.color.clone().or(existing_color),
            node_type: req.node_type,
            display,
            lang,
            tags: req.tags.clone().unwrap_or(existing_tags),
        };
        raw = mdcanvas::set_node_meta(&raw, &id, &meta).ok_or_else(not_found)?;
    }

    if let Some(target) = &req.target {
        let body = format!("[{title}]({target})");
        raw = mdcanvas::set_node_body(&raw, &id, &body).ok_or_else(not_found)?;
    }

    if let Some(text) = &req.text {
        raw = mdcanvas::set_node_body(&raw, &id, text).ok_or_else(not_found)?;
    }

    if let Some(extra_parents) = &req.extra_parents {
        raw = mdcanvas::set_node_edges(&raw, &id, extra_parents).ok_or_else(not_found)?;
    }

    // Validate the whole patched document before committing anything —
    // none of the writes above touched `state.raw`/disk yet.
    parse_or_error(&raw)?;

    state
        .save(&raw)
        .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    *state.raw.lock().unwrap() = raw.clone();
    canvas_response(&raw, &state.canvas_path)
}

/// Cap on how much of a `file` node's target we'll read for a `display=
/// "code"` preview — large enough for any real source file, small enough
/// that a node accidentally pointed at, say, a data dump doesn't try to hand
/// the whole thing to the browser.
const FILE_CONTENT_MAX_BYTES: usize = 1_000_000;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct FileContentResponse {
    content: String,
    /// `true` if `content` was cut off at `FILE_CONTENT_MAX_BYTES` — the UI
    /// uses this to show a "truncated" note rather than implying the
    /// preview is the whole file.
    truncated: bool,
}

/// Read-only preview of a `file` node's target, for `display="code"`
/// (see `SPEC.md`). Reads the file fresh from disk on every call — nothing
/// here is cached or written back. The target is resolved relative to the
/// canvas's own directory and confined to it (same boundary
/// `meshfox_core::include` enforces for include targets): a `../../etc/passwd`
/// or an absolute path pointing outside that tree is rejected rather than
/// read, since the target string comes from the (possibly hand-edited)
/// canvas file, not from a trusted source.
async fn get_node_file_content(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<FileContentResponse>, ApiError> {
    let raw = state.raw.lock().unwrap().clone();
    let canvas = parse_or_error(&raw)?;
    let node = canvas
        .node(&id)
        .ok_or_else(|| ApiError(StatusCode::NOT_FOUND, format!("no node {id:?}")))?;
    if node.node_type != NodeType::File {
        return Err(ApiError(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("node {id:?} is not a file node"),
        ));
    }
    let target = node.target.as_deref().ok_or_else(|| {
        ApiError(StatusCode::UNPROCESSABLE_ENTITY, format!("node {id:?} has no target"))
    })?;

    // `Path::parent()` on a bare filename (e.g. `canvas.md`, no directory
    // component) returns `Some("")`, not `None` — so a plain `unwrap_or(".")`
    // never fires and we'd try to canonicalize an empty path, which fails
    // with ENOENT. Treat that empty-parent case as "." too.
    let canvas_dir = state.canvas_path.parent().filter(|p| !p.as_os_str().is_empty());
    let canvas_dir = canvas_dir.unwrap_or(std::path::Path::new("."));
    let canvas_dir = canvas_dir
        .canonicalize()
        .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let candidate = canvas_dir.join(target);
    let resolved = candidate
        .canonicalize()
        .map_err(|e| ApiError(StatusCode::NOT_FOUND, format!("{}: {e}", candidate.display())))?;
    if !resolved.starts_with(&canvas_dir) {
        return Err(ApiError(
            StatusCode::FORBIDDEN,
            format!("{target:?} resolves outside the canvas directory"),
        ));
    }

    let bytes = std::fs::read(&resolved)
        .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    // A null byte anywhere in a representative prefix is a cheap, standard
    // "this isn't text" heuristic (same one `file`/git use) — good enough to
    // keep an accidental image/binary target from getting shoved into a
    // code editor as mangled text.
    let sample_len = bytes.len().min(8000);
    if bytes[..sample_len].contains(&0) {
        return Err(ApiError(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "target looks like a binary file, can't preview it as code".to_string(),
        ));
    }

    let truncated = bytes.len() > FILE_CONTENT_MAX_BYTES;
    let slice = &bytes[..bytes.len().min(FILE_CONTENT_MAX_BYTES)];
    let content = String::from_utf8_lossy(slice).into_owned();

    Ok(Json(FileContentResponse { content, truncated }))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DeleteNodeQuery {
    /// `"reparent"` promotes the node's direct children to its own parent
    /// instead of deleting them too (see
    /// `mdcanvas::delete_node_reparent_children`) — the web UI's
    /// delete-confirmation dialog's second choice. Absent, or anything
    /// else, keeps the original all-or-nothing behavior (the whole subtree
    /// goes, via `mdcanvas::delete_node`).
    #[serde(default)]
    children: Option<String>,
}

/// Deletes node `id` — either its entire subtree (`mdcanvas::delete_node`,
/// the default) or just itself, promoting its direct children up to its own
/// parent instead (`?children=reparent`, `mdcanvas::delete_node_reparent_children`)
/// — the root is rejected (`422`) rather than producing a rootless document.
async fn remove_node(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Query(query): Query<DeleteNodeQuery>,
) -> Result<Json<Canvas>, ApiError> {
    let raw = state.raw.lock().unwrap().clone();
    let canvas = parse_or_error(&raw)?;
    let node = canvas
        .node(&id)
        .ok_or_else(|| ApiError(StatusCode::NOT_FOUND, format!("no node {id:?}")))?;
    if node.parent.is_none() {
        return Err(ApiError(
            StatusCode::UNPROCESSABLE_ENTITY,
            "can't delete the root node".to_string(),
        ));
    }
    let reparent = query.children.as_deref() == Some("reparent");
    let updated = if reparent {
        mdcanvas::delete_node_reparent_children(&raw, &id)
    } else {
        mdcanvas::delete_node(&raw, &id)
    }
    .ok_or_else(|| ApiError(StatusCode::NOT_FOUND, format!("no node {id:?}")))?;
    parse_or_error(&updated)?;
    state
        .save(&updated)
        .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    *state.raw.lock().unwrap() = updated.clone();
    canvas_response(&updated, &state.canvas_path)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReparentNodeRequest {
    new_parent_id: String,
}

/// Deletes node `id`'s structural (nesting) parent edge, promoting its
/// existing extra edge from `newParentId` to take its place instead — the
/// web UI's "delete the main parent-child link" action on a node that has
/// at least one other incoming edge to fall back on (see
/// `mdcanvas::reparent_node`). `newParentId` must already be one of `id`'s
/// declared extra parents — this never invents a new relationship, only
/// promotes one the document already states.
async fn reparent_node(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(req): Json<ReparentNodeRequest>,
) -> Result<Json<Canvas>, ApiError> {
    let raw = state.raw.lock().unwrap().clone();
    let canvas = parse_or_error(&raw)?;
    let node = canvas
        .node(&id)
        .ok_or_else(|| ApiError(StatusCode::NOT_FOUND, format!("no node {id:?}")))?;
    if node.parent.is_none() {
        return Err(ApiError(
            StatusCode::UNPROCESSABLE_ENTITY,
            "can't reparent the root node".to_string(),
        ));
    }
    canvas
        .node(&req.new_parent_id)
        .ok_or_else(|| ApiError(StatusCode::NOT_FOUND, format!("no node {:?}", req.new_parent_id)))?;
    if !node.extra_parents.iter().any(|e| e.from == req.new_parent_id) {
        return Err(ApiError(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("{:?} is not one of {id:?}'s extra parents", req.new_parent_id),
        ));
    }
    let updated = mdcanvas::reparent_node(&raw, &id, &req.new_parent_id).ok_or_else(|| {
        ApiError(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("can't reparent {id:?} onto {:?} (would create a cycle)", req.new_parent_id),
        )
    })?;
    parse_or_error(&updated)?;
    state
        .save(&updated)
        .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    *state.raw.lock().unwrap() = updated.clone();
    canvas_response(&updated, &state.canvas_path)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RenameNodeIdRequest {
    new_id: String,
}

/// Changes node `id`'s own id to `newId` (`mdcanvas::rename_node_id`) —
/// rewrites every reference this crate's parser tracks structurally
/// (other nodes' `parent=`/`meshfox:edge from=` attributes) exactly, and
/// best-effort rewrites `deps="id/block"` fence references elsewhere in
/// the document (plain text, not parser-validated — a reference that was
/// already stale is left as-is). `404` if `id` doesn't exist, `422` if
/// `newId` is empty, contains a `"`, or collides with an existing node's
/// id — same status split this file uses for every other endpoint.
async fn rename_node_id(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(req): Json<RenameNodeIdRequest>,
) -> Result<Json<Canvas>, ApiError> {
    let raw = state.raw.lock().unwrap().clone();
    let updated = mdcanvas::rename_node_id(&raw, &id, &req.new_id).map_err(|e| {
        let status = match e {
            mdcanvas::RenameIdError::NotFound(_) => StatusCode::NOT_FOUND,
            mdcanvas::RenameIdError::AlreadyExists(_)
            | mdcanvas::RenameIdError::Empty
            | mdcanvas::RenameIdError::InvalidChar => StatusCode::UNPROCESSABLE_ENTITY,
        };
        ApiError(status, e.to_string())
    })?;
    parse_or_error(&updated)?;
    state
        .save(&updated)
        .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    *state.raw.lock().unwrap() = updated.clone();
    canvas_response(&updated, &state.canvas_path)
}

/// Runs the requested block plus — automatically, same as the CLI, unless
/// `no_deps` is set — every block it transitively `deps=`-depends on, in
/// dependency order, stopping early if a step exits non-zero (running what
/// depends on a failed step wouldn't mean anything). Streams progress as
/// `RunEvent`s (NDJSON, one per line) rather than waiting for everything to
/// finish: chain
/// resolution happens up front and still fails with a normal HTTP error if
/// the request doesn't even make sense (dangling block, a cycle) — nothing
/// has started yet at that point — but once resolved, the response is
/// `200 OK` immediately and every subsequent failure (missing node/block,
/// no executor, a step exiting non-zero, a kill) is reported in-stream
/// instead of as an HTTP status, since headers are already sent.
async fn run_block(
    State(state): State<Arc<AppState>>,
    Json(req): Json<RunRequest>,
) -> Result<Response, ApiError> {
    let raw_snapshot = state.raw.lock().unwrap().clone();
    let canvas = parse_or_error(&raw_snapshot)?;
    let path: Vec<&str> = req.path.iter().map(String::as_str).collect();
    let chain = meshfox_core::resolve_run_chain(&canvas, &path, &req.block, !req.no_deps)?;
    if let Some(addr) = find_tty_block(&canvas, &chain) {
        return Err(ApiError(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!(
                "block {:?} in node {:?} is a `tty` block — run it via the WebSocket endpoint (`/api/run/tty`), not `/api/run`",
                addr.block_name, addr.node_id
            ),
        ));
    }
    let persist = req.persist;

    // Resolve only the declared variables this chain's blocks actually
    // reference (via `env=` — see `env_var_names_for_chain`), against
    // this request's `vars` (the UI's pre-run form answers) plus the
    // process env/cache/default — same precedence and same scoping the
    // CLI uses. Anything still missing means the client ran the block
    // without checking `GET /api/vars` first (or raced another tab) —
    // fail before starting, same as an unresolvable chain above. Every
    // non-secret answer the request actually supplied is persisted right
    // away, so the next run (CLI or UI, even for a different block that
    // happens to reference the same variable) doesn't ask again.
    let needed = meshfox_core::env_var_names_for_chain(&canvas, &chain);
    let decls = meshfox_core::declared_vars(&canvas)
        .map_err(|e| ApiError(StatusCode::UNPROCESSABLE_ENTITY, e.to_string()))?;
    let relevant_decls: Vec<_> = decls.into_iter().filter(|d| needed.contains(&d.name)).collect();
    let resolved_vars = {
        let mut cache = state.vars_cache.lock().unwrap();
        let resolved = meshfox_core::resolve_vars(&relevant_decls, &req.vars, &cache);
        if !resolved.missing.is_empty() {
            let names: Vec<&str> = resolved.missing.iter().map(|d| d.name.as_str()).collect();
            return Err(ApiError(
                StatusCode::UNPROCESSABLE_ENTITY,
                format!("missing required variable(s): {}", names.join(", ")),
            ));
        }
        for (name, value) in &req.vars {
            if relevant_decls.iter().any(|d| &d.name == name && !d.secret) {
                let _ = cache.set(name, value);
            }
        }
        resolved.values
    };

    let run_id = uuid::Uuid::new_v4().to_string();
    let (kill_tx, mut kill_rx) = oneshot::channel::<()>();
    state.runs.lock().unwrap().insert(run_id.clone(), kill_tx);

    let stream = async_stream::stream! {
        // Dropped at the end of this block (however it ends — see
        // `RunGuard`'s own doc comment) to remove the registry entry.
        let _guard = RunGuard { state: Arc::clone(&state), run_id: run_id.clone() };

        yield Ok::<_, io::Error>(ndjson_line(&RunEvent::Started { run_id: run_id.clone() }));

        let mut raw = state.raw.lock().unwrap().clone();
        let mut final_exit_code = 0;
        let mut killed = false;

        for addr in &chain {
            yield Ok(ndjson_line(&RunEvent::StepStart {
                node_id: addr.node_id.clone(),
                block: addr.block_name.clone(),
            }));

            // Re-parse so an earlier step's freshly-patched cache (below)
            // is visible before this one runs — same reasoning `meshfox
            // run`'s CLI loop already has.
            let node_text = match Canvas::from_markdown(&raw)
                .ok()
                .and_then(|c| c.node(&addr.node_id).map(|n| n.text.clone()))
            {
                Some(text) => text,
                None => {
                    yield Ok(ndjson_line(&RunEvent::Error {
                        message: format!("node {:?} not found", addr.node_id),
                    }));
                    break;
                }
            };
            let Some(block) = meshfox_core::scan_runnable_blocks(&addr.node_id, &node_text)
                .into_iter()
                .find(|b| b.name.as_deref() == Some(addr.block_name.as_str()))
            else {
                yield Ok(ndjson_line(&RunEvent::Error {
                    message: format!(
                        "no runnable block named {:?} in node {:?}",
                        addr.block_name, addr.node_id
                    ),
                }));
                break;
            };
            if !stream_exec::supports(&block.lang) {
                yield Ok(ndjson_line(&RunEvent::Error {
                    message: format!("no executor registered for language {:?}", block.lang),
                }));
                break;
            }

            // Only this block's own `env=` list, relabeled to its local
            // names — not the whole chain's resolved variables — same
            // "opt-in per block" scoping the CLI applies.
            let block_env = meshfox_core::map_block_env(&block.env, &resolved_vars);
            let mut proc = match stream_exec::spawn_bash(&block.code, &block_env) {
                Ok(p) => p,
                Err(e) => {
                    yield Ok(ndjson_line(&RunEvent::Error { message: e.to_string() }));
                    break;
                }
            };

            let mut full_output = String::new();
            let exit_code = loop {
                tokio::select! {
                    line = proc.output_rx.recv() => {
                        match line {
                            Some(text) => {
                                full_output.push_str(&text);
                                full_output.push('\n');
                                yield Ok(ndjson_line(&RunEvent::Output {
                                    node_id: addr.node_id.clone(),
                                    block: addr.block_name.clone(),
                                    text,
                                }));
                            }
                            None => {
                                let status = proc.child.wait().await;
                                break status.ok().and_then(|s| s.code()).unwrap_or(-1);
                            }
                        }
                    }
                    _ = &mut kill_rx => {
                        let _ = proc.kill();
                        let _ = proc.child.wait().await;
                        yield Ok(ndjson_line(&RunEvent::Killed {
                            node_id: addr.node_id.clone(),
                            block: addr.block_name.clone(),
                        }));
                        killed = true;
                        break -1;
                    }
                }
            };
            if killed {
                break;
            }

            yield Ok(ndjson_line(&RunEvent::StepEnd {
                node_id: addr.node_id.clone(),
                block: addr.block_name.clone(),
                exit_code,
            }));
            final_exit_code = exit_code;

            if persist && block.cache {
                let result = ExecOutput { exit_code, output: full_output };
                if let Some(updated) = meshfox_core::write_output(&node_text, &addr.block_name, &result) {
                    if let Some(patched) = mdcanvas::set_node_body(&raw, &addr.node_id, &updated) {
                        raw = patched;
                    }
                }
            }

            if exit_code != 0 {
                break;
            }
        }

        // Persist whatever completed, even if the chain was killed partway
        // through — a step that had already finished and been folded into
        // `raw` (above) shouldn't lose its freshly-cached output just
        // because a *later* step in the same chain got killed.
        if persist {
            match state.save(&raw) {
                Ok(()) => *state.raw.lock().unwrap() = raw,
                Err(e) => yield Ok(ndjson_line(&RunEvent::Error { message: e.to_string() })),
            }
        }

        // `killed` was already emitted as this run's terminal event above —
        // no `Done` follows it.
        if !killed {
            yield Ok(ndjson_line(&RunEvent::Done { exit_code: final_exit_code }));
        }
    };

    Ok(Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/x-ndjson")
        .body(Body::from_stream(stream))
        .unwrap())
}

/// The first block in `chain` (if any) flagged `tty` — `meshfox validate`'s
/// rule that a `tty` block may only be a `deps=` target of *another* `tty`
/// block (enforced inside `resolve_run_chain` itself, via
/// `deps::visit`) means a chain can only ever contain one if the
/// originally-requested block itself is `tty` too, but this checks every
/// entry anyway rather than leaning on that invariant staying true forever.
fn find_tty_block(canvas: &Canvas, chain: &[meshfox_core::BlockAddr]) -> Option<meshfox_core::BlockAddr> {
    chain
        .iter()
        .find(|addr| {
            canvas
                .node(&addr.node_id)
                .map(|node| meshfox_core::scan_runnable_blocks(&addr.node_id, &node.text))
                .unwrap_or_default()
                .iter()
                .any(|b| b.name.as_deref() == Some(addr.block_name.as_str()) && b.tty)
        })
        .cloned()
}

/// Addresses a block the same way `RunRequest`/`VarsQuery` do, flattened
/// into query params since a WebSocket upgrade request is a plain `GET`
/// with no JSON body — same convention `VarsQuery` already uses. `vars` is
/// a JSON-encoded `{name: value}` object (empty string treated as `{}`) —
/// the UI's pre-run form answers, same role `RunRequest.vars` plays for
/// `/api/run`, just serialized into the query string since a `GET` has
/// nowhere else to put it. `cols`/`rows` are the browser terminal's current
/// size (`xterm.js`'s fit-addon), used as the pty's *initial* size — later
/// resizes go through the WebSocket itself once the `tty` step starts (see
/// `RunEvent::TtyStart`'s doc comment).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TtyRunQuery {
    #[serde(default)]
    path: String,
    block: String,
    #[serde(default)]
    no_deps: bool,
    #[serde(default)]
    persist: bool,
    #[serde(default)]
    vars: String,
    #[serde(default = "default_pty_cols")]
    cols: u16,
    #[serde(default = "default_pty_rows")]
    rows: u16,
}

fn default_pty_cols() -> u16 {
    80
}

fn default_pty_rows() -> u16 {
    24
}

/// A client's resize control message during an active `tty` step (see
/// `RunEvent::TtyStart`) — the only thing a text frame can mean once a
/// `tty` step has started; every other client-to-server frame in that
/// window is a binary frame of raw input bytes to type into the pty.
#[derive(Debug, Deserialize)]
struct ResizeMessage {
    cols: u16,
    rows: u16,
}

/// `GET /api/run/tty` — the WebSocket counterpart to `run_block`, for a
/// chain that ends in (or, via a `tty`-only `deps=` chain, passes through)
/// a `tty` block. Chain resolution, `tty`/`cache` well-formedness (already
/// enforced by `resolve_run_chain` itself — see `deps::visit`), and
/// variable resolution all happen *before* the WebSocket upgrade, so a
/// request that doesn't even make sense still fails as a normal HTTP error
/// (same guarantee `run_block` gives) instead of upgrading and then
/// immediately closing.
async fn run_block_tty(
    State(state): State<Arc<AppState>>,
    Query(query): Query<TtyRunQuery>,
    ws: WebSocketUpgrade,
) -> Result<Response, ApiError> {
    let raw_snapshot = state.raw.lock().unwrap().clone();
    let canvas = parse_or_error(&raw_snapshot)?;
    let path: Vec<&str> = if query.path.is_empty() { Vec::new() } else { query.path.split(',').collect() };
    let chain = meshfox_core::resolve_run_chain(&canvas, &path, &query.block, !query.no_deps)?;
    let persist = query.persist;
    let (cols, rows) = (query.cols.max(1), query.rows.max(1));

    let requested_vars: HashMap<String, String> = if query.vars.is_empty() {
        HashMap::new()
    } else {
        serde_json::from_str(&query.vars)
            .map_err(|e| ApiError(StatusCode::BAD_REQUEST, format!("invalid `vars`: {e}")))?
    };

    // Same resolution this chain's `env=` needs as `run_block` does — see
    // its own doc comment for why only the chain's actually-referenced
    // variables are ever looked at.
    let needed = meshfox_core::env_var_names_for_chain(&canvas, &chain);
    let decls = meshfox_core::declared_vars(&canvas)
        .map_err(|e| ApiError(StatusCode::UNPROCESSABLE_ENTITY, e.to_string()))?;
    let relevant_decls: Vec<_> = decls.into_iter().filter(|d| needed.contains(&d.name)).collect();
    let resolved_vars = {
        let mut cache = state.vars_cache.lock().unwrap();
        let resolved = meshfox_core::resolve_vars(&relevant_decls, &requested_vars, &cache);
        if !resolved.missing.is_empty() {
            let names: Vec<&str> = resolved.missing.iter().map(|d| d.name.as_str()).collect();
            return Err(ApiError(
                StatusCode::UNPROCESSABLE_ENTITY,
                format!("missing required variable(s): {}", names.join(", ")),
            ));
        }
        for (name, value) in &requested_vars {
            if relevant_decls.iter().any(|d| &d.name == name && !d.secret) {
                let _ = cache.set(name, value);
            }
        }
        resolved.values
    };

    let run_id = uuid::Uuid::new_v4().to_string();
    let (kill_tx, kill_rx) = oneshot::channel::<()>();
    state.runs.lock().unwrap().insert(run_id.clone(), kill_tx);

    Ok(ws.on_upgrade(move |socket| {
        run_tty_chain(socket, state, run_id, chain, resolved_vars, persist, cols, rows, kill_rx)
    }))
}

/// Sends one `RunEvent` as a WebSocket text frame (JSON) — the WS
/// equivalent of `run_block`'s `ndjson_line`. Returns `false` if the send
/// failed (client gone), the signal callers use to give up on the rest of
/// the run entirely rather than attempt any further sends on a dead socket.
async fn send_event(socket: &mut WebSocket, event: &RunEvent) -> bool {
    let text = serde_json::to_string(event).expect("RunEvent always serializes");
    socket.send(Message::Text(text)).await.is_ok()
}

/// Runs `chain` over `socket` — every step captured/streamed as `RunEvent`
/// text frames exactly like `run_block`'s NDJSON body, except a `tty` step
/// (there can be more than one, each running its own `deps=` first —
/// see SPEC.md's "Interactive (`tty`) blocks") instead hands the process a
/// real pty and relays it raw (see `relay_tty_step`), bracketed by
/// `RunEvent::TtyStart`/`StepEnd`. Mirrors `run_block`'s
/// `async_stream::stream!` body closely — same guard, same
/// stop-the-chain-on-first-failure/kill rules, same cache-persist-at-the-end
/// behavior — just pushed over a WebSocket instead of yielded into an
/// NDJSON stream.
#[allow(clippy::too_many_arguments)]
async fn run_tty_chain(
    mut socket: WebSocket,
    state: Arc<AppState>,
    run_id: String,
    chain: Vec<meshfox_core::BlockAddr>,
    resolved_vars: HashMap<String, String>,
    persist: bool,
    cols: u16,
    rows: u16,
    mut kill_rx: oneshot::Receiver<()>,
) {
    let _guard = RunGuard { state: Arc::clone(&state), run_id: run_id.clone() };

    if !send_event(&mut socket, &RunEvent::Started { run_id: run_id.clone() }).await {
        return;
    }

    let mut raw = state.raw.lock().unwrap().clone();
    let mut final_exit_code = 0;
    let mut killed = false;

    for addr in &chain {
        if !send_event(&mut socket, &RunEvent::StepStart { node_id: addr.node_id.clone(), block: addr.block_name.clone() }).await
        {
            return;
        }

        // Re-parse so an earlier step's freshly-patched cache is visible
        // before this one runs — same reasoning `run_block` already has.
        let node_text = match Canvas::from_markdown(&raw).ok().and_then(|c| c.node(&addr.node_id).map(|n| n.text.clone())) {
            Some(text) => text,
            None => {
                send_event(&mut socket, &RunEvent::Error { message: format!("node {:?} not found", addr.node_id) }).await;
                break;
            }
        };
        let Some(block) = meshfox_core::scan_runnable_blocks(&addr.node_id, &node_text)
            .into_iter()
            .find(|b| b.name.as_deref() == Some(addr.block_name.as_str()))
        else {
            send_event(
                &mut socket,
                &RunEvent::Error { message: format!("no runnable block named {:?} in node {:?}", addr.block_name, addr.node_id) },
            )
            .await;
            break;
        };

        let block_env = meshfox_core::map_block_env(&block.env, &resolved_vars);
        let mut full_output = String::new();

        let exit_code = if block.tty {
            if !send_event(&mut socket, &RunEvent::TtyStart { node_id: addr.node_id.clone(), block: addr.block_name.clone() }).await
            {
                return;
            }
            match relay_tty_step(&mut socket, &block.code, &block_env, cols, rows, &mut kill_rx).await {
                TtyStepOutcome::Exited(code) => code,
                TtyStepOutcome::Killed => {
                    killed = true;
                    -1
                }
                // Client disconnected mid-session — nothing left to send.
                TtyStepOutcome::Disconnected => return,
            }
        } else {
            let mut proc = match stream_exec::spawn_bash(&block.code, &block_env) {
                Ok(p) => p,
                Err(e) => {
                    send_event(&mut socket, &RunEvent::Error { message: e.to_string() }).await;
                    break;
                }
            };
            loop {
                tokio::select! {
                    line = proc.output_rx.recv() => {
                        match line {
                            Some(text) => {
                                full_output.push_str(&text);
                                full_output.push('\n');
                                if !send_event(&mut socket, &RunEvent::Output {
                                    node_id: addr.node_id.clone(),
                                    block: addr.block_name.clone(),
                                    text,
                                }).await {
                                    let _ = proc.kill();
                                    return;
                                }
                            }
                            None => {
                                let status = proc.child.wait().await;
                                break status.ok().and_then(|s| s.code()).unwrap_or(-1);
                            }
                        }
                    }
                    _ = &mut kill_rx => {
                        let _ = proc.kill();
                        let _ = proc.child.wait().await;
                        killed = true;
                        break -1;
                    }
                }
            }
        };

        if killed {
            send_event(&mut socket, &RunEvent::Killed { node_id: addr.node_id.clone(), block: addr.block_name.clone() }).await;
            break;
        }

        if !send_event(&mut socket, &RunEvent::StepEnd { node_id: addr.node_id.clone(), block: addr.block_name.clone(), exit_code })
            .await
        {
            return;
        }
        final_exit_code = exit_code;

        // `tty` and `cache` are mutually exclusive (a `meshfox validate`
        // error) — `!block.tty` here is belt-and-suspenders against ever
        // writing a `tty` step's (empty) `full_output` into the file for a
        // document that reached this endpoint without being validated.
        if persist && block.cache && !block.tty {
            let result = ExecOutput { exit_code, output: full_output };
            if let Some(updated) = meshfox_core::write_output(&node_text, &addr.block_name, &result) {
                if let Some(patched) = mdcanvas::set_node_body(&raw, &addr.node_id, &updated) {
                    raw = patched;
                }
            }
        }

        if exit_code != 0 {
            break;
        }
    }

    if persist {
        match state.save(&raw) {
            Ok(()) => *state.raw.lock().unwrap() = raw,
            Err(e) => {
                send_event(&mut socket, &RunEvent::Error { message: e.to_string() }).await;
            }
        }
    }

    if !killed {
        send_event(&mut socket, &RunEvent::Done { exit_code: final_exit_code }).await;
    }
}

/// How one `tty` step (see `relay_tty_step`) ended, distinguishing the
/// three ways that matters to `run_tty_chain`: a normal exit (still emits
/// `StepEnd` and may continue the chain), an explicit kill (emits `Killed`
/// instead, same as a captured step's kill path, and always stops the
/// chain), or the client disconnecting (nothing left to send to `socket`
/// at all — `run_tty_chain` must return immediately without attempting
/// any further `RunEvent`).
enum TtyStepOutcome {
    Exited(i32),
    Killed,
    Disconnected,
}

/// Relays one `tty` step over `socket` once it's already in "raw I/O" mode
/// (right after `RunEvent::TtyStart`): pty output bytes go out as binary
/// frames, incoming binary frames go to the pty's stdin, incoming text
/// frames are parsed as a `ResizeMessage`.
async fn relay_tty_step(
    socket: &mut WebSocket,
    code: &str,
    envs: &HashMap<String, String>,
    cols: u16,
    rows: u16,
    kill_rx: &mut oneshot::Receiver<()>,
) -> TtyStepOutcome {
    let mut pty = match pty_exec::spawn_bash(code, envs, cols, rows) {
        Ok(p) => p,
        Err(e) => {
            send_event(socket, &RunEvent::Error { message: e.to_string() }).await;
            return TtyStepOutcome::Exited(-1);
        }
    };

    loop {
        tokio::select! {
            chunk = pty.output_rx.recv() => {
                match chunk {
                    Some(bytes) => {
                        if socket.send(Message::Binary(bytes)).await.is_err() {
                            let _ = pty.kill();
                            return TtyStepOutcome::Disconnected;
                        }
                    }
                    // Pty closed — the step's process (and anything it
                    // spawned) has exited; `pty.wait()` below resolves
                    // right away.
                    None => break,
                }
            }
            msg = socket.recv() => {
                match msg {
                    Some(Ok(Message::Binary(bytes))) => pty.write(bytes),
                    Some(Ok(Message::Text(text))) => {
                        if let Ok(resize) = serde_json::from_str::<ResizeMessage>(&text) {
                            pty.resize(resize.cols.max(1), resize.rows.max(1));
                        }
                    }
                    Some(Ok(Message::Close(_))) | None => {
                        let _ = pty.kill();
                        return TtyStepOutcome::Disconnected;
                    }
                    Some(Err(_)) => {
                        let _ = pty.kill();
                        return TtyStepOutcome::Disconnected;
                    }
                    // Ping/Pong: axum answers Pings internally; nothing to
                    // relay to the pty either way.
                    Some(Ok(_)) => {}
                }
            }
            _ = &mut *kill_rx => {
                let _ = pty.kill();
                pty.wait().await;
                return TtyStepOutcome::Killed;
            }
        }
    }

    TtyStepOutcome::Exited(pty.wait().await)
}

/// Cancels an in-flight run started by `run_block` — kills whichever step
/// is currently executing (`SIGKILL`, via `stream_exec::SpawnedProcess::
/// kill`, which reaches the whole process group a hung script spawned, not
/// just `bash` itself) and stops the rest of its dependency chain. `404`
/// if `runId` is unknown, which just as often
/// means "it already finished" as "it never existed" — the client treats
/// either the same way (nothing left to kill).
async fn kill_run(State(state): State<Arc<AppState>>, Json(req): Json<KillRequest>) -> StatusCode {
    match state.runs.lock().unwrap().remove(&req.run_id) {
        Some(tx) => {
            let _ = tx.send(());
            StatusCode::NO_CONTENT
        }
        None => StatusCode::NOT_FOUND,
    }
}

/// Streams NDJSON `{"type": "..."}` lines for as long as the client stays
/// connected — one long-lived connection per open browser tab. Serves two
/// purposes at once: forwards every `changed` event the file-watcher thread
/// broadcasts (see `spawn_file_watcher`) so the UI can reload after an
/// external edit, and doubles as both tab tracking (`TabGuard` above) and
/// the client's own liveness check on the server — the stream simply ends
/// (the fetch's reader sees `done`) the moment this process exits, which is
/// how the UI notices the server itself has stopped, no separate polling
/// needed.
async fn watch_changes(State(state): State<Arc<AppState>>) -> Response {
    state.open_tabs.fetch_add(1, Ordering::SeqCst);
    state.ever_connected.store(true, Ordering::SeqCst);
    let mut rx = state.change_tx.subscribe();

    let stream = async_stream::stream! {
        // Dropped when this generator is (i.e. the client disconnects) —
        // see `TabGuard`'s own doc comment.
        let _guard = TabGuard { state: Arc::clone(&state) };
        yield Ok::<_, io::Error>(Bytes::from_static(b"{\"type\":\"connected\"}\n"));
        loop {
            match rx.recv().await {
                Ok(()) | Err(broadcast::error::RecvError::Lagged(_)) => {
                    yield Ok(Bytes::from_static(b"{\"type\":\"changed\"}\n"));
                }
                // Never actually fires: `change_tx`'s sender lives in
                // `AppState`, which outlives every connection.
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    };

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/x-ndjson")
        .body(Body::from_stream(stream))
        .unwrap()
}

/// Serves the embedded web UI, falling back to `index.html` for any path
/// that isn't a real asset (client-side routes) — or, if the UI was never
/// built into this binary at all, a message saying so instead of a bare
/// 404.
/// Serves `path` (a request path, already stripped of its leading `/`) off
/// disk, resolved relative to the canvas file's own directory — `None` if
/// it doesn't exist, isn't a plain file, or resolves outside that
/// directory (`..` traversal).
async fn serve_canvas_relative_file(state: &AppState, path: &str) -> Option<Response> {
    let canvas_dir = state
        .canvas_path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(std::path::Path::new("."));
    let canvas_dir = canvas_dir.canonicalize().ok()?;

    let candidate = canvas_dir.join(path);
    let resolved = candidate.canonicalize().ok()?;
    if !resolved.starts_with(&canvas_dir) || !resolved.is_file() {
        return None;
    }

    let bytes = std::fs::read(&resolved).ok()?;
    let mime = mime_guess::from_path(&resolved).first_or_octet_stream();
    Some((
        [(header::CONTENT_TYPE, mime.as_ref().to_string())],
        bytes,
    )
        .into_response())
}

async fn serve_embedded(State(state): State<Arc<AppState>>, uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };

    if let Some(file) = WebAssets::get(path) {
        let mime = mime_guess::from_path(path).first_or_octet_stream();
        return (
            [(header::CONTENT_TYPE, mime.as_ref().to_string())],
            file.data,
        )
            .into_response();
    }

    // Not a built-in UI asset — try it as a file next to the canvas (e.g.
    // an image pulled in by a plain `![](screenshot.webp)` link), so
    // relative asset references render the same in `meshfox view` as they
    // do on GitHub. Same canonicalize + `starts_with` traversal guard as
    // `get_node_file_content` above, so this can't be used to read
    // arbitrary files outside the canvas directory.
    if let Some(file_response) = serve_canvas_relative_file(&state, path).await {
        return file_response;
    }

    match WebAssets::get("index.html") {
        Some(file) => ([(header::CONTENT_TYPE, "text/html")], file.data).into_response(),
        None => (
            StatusCode::NOT_FOUND,
            "web UI assets not built into this binary — run `cd web && npm install && npm run build` \
             and rebuild meshfox",
        )
            .into_response(),
    }
}

/// Serves `canvas_path` on `127.0.0.1:<port>` until the process is killed
/// (or, when `auto_exit` is on, until every `/api/watch`-connected tab has
/// closed — see `TabGuard`). `port` of `0` asks the OS to assign a free
/// port instead — the actual bound port is read back from the listener
/// below.
async fn build_state(canvas_path: PathBuf, auto_exit: bool) -> std::io::Result<Arc<AppState>> {
    let raw = std::fs::read_to_string(&canvas_path)?;
    if let Err(e) = Canvas::from_markdown(&raw) {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()));
    }

    let vars_cache = VarCache::load(&canvas_path)?;
    let (change_tx, _) = broadcast::channel(16);

    Ok(Arc::new(AppState {
        canvas_path,
        raw: Mutex::new(raw),
        runs: Mutex::new(HashMap::new()),
        vars_cache: Mutex::new(vars_cache),
        open_tabs: AtomicUsize::new(0),
        ever_connected: AtomicBool::new(false),
        change_tx,
        auto_exit,
    }))
}

fn build_app(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/api/canvas", get(get_canvas).put(put_canvas))
        .route("/api/canvas/raw", get(get_canvas_raw).put(put_canvas_raw))
        .route("/api/canvas/clear-layout", post(clear_layout))
        .route("/api/nodes", post(create_node))
        .route("/api/nodes/:id", patch(update_node).delete(remove_node))
        .route("/api/nodes/:id/reparent", post(reparent_node))
        .route("/api/nodes/:id/rename-id", post(rename_node_id))
        .route("/api/nodes/:id/file-content", get(get_node_file_content))
        .route("/api/vars", get(get_vars))
        .route("/api/run", post(run_block))
        .route("/api/run/tty", get(run_block_tty))
        .route("/api/kill", post(kill_run))
        .route("/api/watch", get(watch_changes))
        .fallback(serve_embedded)
        .with_state(state)
        .layer(CorsLayer::permissive())
}

/// Serves `canvas_path` on `127.0.0.1:<port>` until the process is killed
/// (or, when `auto_exit` is on, until every `/api/watch`-connected tab has
/// closed — see `TabGuard`). `port` of `0` asks the OS to assign a free
/// port instead — the actual bound port is read back from the listener
/// below.
pub async fn run(canvas_path: PathBuf, port: u16, open_browser: bool, auto_exit: bool) -> std::io::Result<()> {
    let state = build_state(canvas_path.clone(), auto_exit).await?;
    spawn_file_watcher(Arc::clone(&state));
    let app = build_app(state);

    let listener = tokio::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], port))).await?;
    let addr = listener.local_addr()?;
    println!("meshfox: serving {} on http://{addr}", canvas_path.display());

    if open_browser {
        // Best-effort: no browser, no display, or an unsupported platform
        // shouldn't stop the server from running headless.
        if let Err(e) = open::that(format!("http://{addr}")) {
            eprintln!("meshfox: couldn't open a browser automatically ({e}) — open http://{addr} yourself");
        }
    }

    axum::serve(listener, app).await
}

/// Binds `canvas_path` to an OS-assigned local port and serves it in the
/// background (`tokio::spawn`), returning the bound address — the
/// `#[cfg(test)]`-only entry point integration tests use to drive the real
/// HTTP/WebSocket API without going through `run`'s CLI-oriented setup
/// (`println!`, browser-opening, auto-exit).
#[cfg(test)]
async fn spawn_test_server(canvas_path: PathBuf) -> SocketAddr {
    let state = build_state(canvas_path, false).await.expect("valid test canvas");
    let app = build_app(state);
    let listener = tokio::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0))).await.expect("bind");
    let addr = listener.local_addr().expect("local_addr");
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("server");
    });
    addr
}

#[cfg(test)]
mod clear_layout_tests {
    use super::*;

    fn write_test_canvas(contents: &str) -> PathBuf {
        let mut path = std::env::temp_dir();
        path.push(format!("meshfox-clear-layout-test-{}.canvas.md", uuid::Uuid::new_v4()));
        std::fs::write(&path, contents).unwrap();
        path
    }

    // Mirrors the real-world shape that triggered this: an `include` node
    // (e.g. a "README" card pointing at another file) carries its own real,
    // authored `x`/`y`/`w`/`h` right on its own `meshfox:node` comment, same
    // as any other node — clicking the web UI's "Auto-layout" button is
    // supposed to clear every node's stored position, this one included.
    const CANVAS: &str = concat!(
        "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
        "## Section\n<!-- meshfox:node id=\"section\" -->\n\n",
        "### README\n<!-- meshfox:node id=\"readme\" type=\"include\" x=392 y=300 w=280 h=108 -->\n\n",
        "[readme](./other/README.md)\n",
    );

    #[tokio::test]
    async fn clear_layout_clears_an_include_nodes_own_authored_position() {
        let canvas_path = write_test_canvas(CANVAS);
        let target_path = canvas_path.with_file_name("other-readme.md");
        std::fs::write(&target_path, "included body\n").unwrap();
        let canvas_path = write_test_canvas(&CANVAS.replace("./other/README.md", &target_path.display().to_string()));
        let state = build_state(canvas_path.clone(), false).await.expect("valid test canvas");

        let Json(cleared) = match clear_layout(State(state)).await {
            Ok(json) => json,
            Err(e) => panic!("clear-layout failed: {}", e.1),
        };
        let readme = cleared.node("readme").expect("readme node still present");

        assert_eq!(readme.x, None, "include node's own authored x should be cleared");
        assert_eq!(readme.y, None, "include node's own authored y should be cleared");

        let _ = std::fs::remove_file(&canvas_path);
        let _ = std::fs::remove_file(&target_path);
    }
}

#[cfg(test)]
mod ws_tests {
    use super::*;
    use futures_util::{SinkExt, StreamExt};
    use tokio::net::TcpStream;
    use tokio_tungstenite::tungstenite::Message as WsMessage;
    use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

    type TestSocket = WebSocketStream<MaybeTlsStream<TcpStream>>;

    fn write_test_canvas(contents: &str) -> PathBuf {
        let mut path = std::env::temp_dir();
        path.push(format!("meshfox-tty-test-{}.canvas.md", uuid::Uuid::new_v4()));
        std::fs::write(&path, contents).unwrap();
        path
    }

    /// Reads WebSocket frames until the next `RunEvent` text frame (JSON),
    /// silently skipping any binary (raw pty output) frames in between —
    /// what every assertion below actually cares about.
    async fn next_event(ws: &mut TestSocket) -> serde_json::Value {
        loop {
            match ws.next().await.expect("socket open").expect("no ws error") {
                WsMessage::Text(t) => return serde_json::from_str(&t).expect("valid RunEvent JSON"),
                _ => continue,
            }
        }
    }

    /// Reads binary frames, accumulating them, until the combined bytes
    /// contain `needle` — how tests wait for a specific bit of a `tty`
    /// step's pty output to show up, since it can arrive split across
    /// several frames.
    async fn read_until(ws: &mut TestSocket, needle: &str) -> String {
        let mut collected = Vec::new();
        loop {
            match ws.next().await.expect("socket open").expect("no ws error") {
                WsMessage::Binary(bytes) => {
                    collected.extend_from_slice(&bytes);
                    if String::from_utf8_lossy(&collected).contains(needle) {
                        return String::from_utf8_lossy(&collected).into_owned();
                    }
                }
                WsMessage::Text(t) => panic!("unexpected RunEvent while waiting for pty output: {t}"),
                _ => continue,
            }
        }
    }

    const TTY_CANVAS: &str = concat!(
        "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
        "## Shell\n<!-- meshfox:node id=\"shell\" -->\n\n",
        "```bash name=\"interactive\" tty\n",
        "echo ready; read line; echo \"got: $line\"\n",
        "```\n",
    );

    #[tokio::test]
    async fn tty_websocket_runs_an_interactive_step_end_to_end() {
        let canvas_path = write_test_canvas(TTY_CANVAS);
        let addr = spawn_test_server(canvas_path.clone()).await;
        let url = format!("ws://{addr}/api/run/tty?path=shell&block=interactive&cols=80&rows=24");
        let (mut ws, _) = tokio_tungstenite::connect_async(url).await.expect("connect");

        let started = next_event(&mut ws).await;
        assert_eq!(started["type"], "started");
        assert!(started["runId"].is_string());

        let step_start = next_event(&mut ws).await;
        assert_eq!(step_start["type"], "step-start");
        assert_eq!(step_start["block"], "interactive");

        let tty_start = next_event(&mut ws).await;
        assert_eq!(tty_start["type"], "tty-start");
        assert_eq!(tty_start["block"], "interactive");

        // Real terminal semantics: the pty echoes typed input back itself,
        // in addition to the process's own output — just wait for the
        // process's own "ready" line before typing anything.
        read_until(&mut ws, "ready").await;

        ws.send(WsMessage::Binary(b"hello\n".to_vec().into())).await.expect("send input");
        read_until(&mut ws, "got: hello").await;

        let step_end = next_event(&mut ws).await;
        assert_eq!(step_end["type"], "step-end");
        assert_eq!(step_end["exitCode"], 0);

        let done = next_event(&mut ws).await;
        assert_eq!(done["type"], "done");
        assert_eq!(done["exitCode"], 0);

        let _ = std::fs::remove_file(&canvas_path);
    }

    #[tokio::test]
    async fn tty_websocket_kill_stops_the_session() {
        let canvas_path = write_test_canvas(concat!(
            "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
            "## Shell\n<!-- meshfox:node id=\"shell\" -->\n\n",
            "```bash name=\"interactive\" tty\necho ready; sleep 30\n```\n",
        ));
        let addr = spawn_test_server(canvas_path.clone()).await;
        let url = format!("ws://{addr}/api/run/tty?path=shell&block=interactive&cols=80&rows=24");
        let (mut ws, _) = tokio_tungstenite::connect_async(url).await.expect("connect");

        let started = next_event(&mut ws).await;
        let run_id = started["runId"].as_str().expect("runId").to_string();
        next_event(&mut ws).await; // step-start
        next_event(&mut ws).await; // tty-start
        read_until(&mut ws, "ready").await;

        // Same `/api/kill` a captured (non-`tty`) run already uses — `tty`
        // runs register into the same `state.runs` map, so no separate
        // kill mechanism was needed for the WebSocket endpoint.
        let client = reqwest_free_kill(addr, &run_id).await;
        assert_eq!(client, 204);

        let killed = next_event(&mut ws).await;
        assert_eq!(killed["type"], "killed");
        assert_eq!(killed["block"], "interactive");

        let _ = std::fs::remove_file(&canvas_path);
    }

    /// Posts to `/api/kill` without pulling in a full HTTP client crate —
    /// a bare `TcpStream` with a hand-written request is enough for this
    /// one call. Returns the response status code.
    async fn reqwest_free_kill(addr: SocketAddr, run_id: &str) -> u16 {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let body = format!(r#"{{"runId":{run_id:?}}}"#);
        let request = format!(
            "POST /api/kill HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let mut stream = TcpStream::connect(addr).await.expect("connect");
        stream.write_all(request.as_bytes()).await.expect("write");
        let mut response = String::new();
        stream.read_to_string(&mut response).await.expect("read");
        let status_line = response.lines().next().expect("status line");
        status_line.split_whitespace().nth(1).expect("status code").parse().expect("numeric status")
    }
}
