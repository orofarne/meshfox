//! Thin HTTP client so `node <op>` (CLI) and MCP's leaf tool handlers can
//! route a mutation through an already-running `meshfox view` worker (see
//! `meshfox_core::worker_lock`) instead of editing the canvas file directly
//! — closes the lost-update race between a worker's own in-memory state and
//! an independent CLI/MCP read-modify-write (TODO.canvas.md's
//! "Оптимистичная конкурентность при записи файла" /
//! "MCP-редактирование файла (batch/транзакционно)"): when a worker already
//! exists, there's no race to detect in the first place, since it's the
//! only thing touching the file at all.
//!
//! Only wired into ops that map cleanly onto an existing `/api/nodes*`
//! endpoint with identical semantics to their direct-file counterpart —
//! `node body` (PATCH `text`) and `node rm` (DELETE) so far. Deliberately
//! NOT `node add`/`node meta` yet: `POST /api/nodes` always assigns a
//! random id (`mdcanvas::insert_child_node_random_id`), not the title-slug
//! id CLI/MCP's own `node add` has always produced
//! (`mdcanvas::insert_child_node`) — routing it through a worker as-is
//! would make `node add`'s id scheme depend on whether a worker happens to
//! be running, which is worse than the race it would close. `PATCH
//! /api/nodes/:id` also has no way to set a node's position/size/
//! `createdAt` at all (the web UI only ever moves a node by drag, never by
//! absolute value through this endpoint), so `node meta`'s fields have no
//! server-side equivalent to route through yet. Both logged as follow-ups
//! in TODO.canvas.md rather than silently done differently than direct-file
//! editing does them.

use meshfox_core::worker_lock::{self, Acquired};
use meshfox_server::stream_exec::OutputStream;
use std::collections::{HashMap, HashSet};
use std::path::Path;

/// `Some(port)` if another process is already serving `canvas_path` as a
/// `meshfox view` worker; `None` if there's no live worker (the common
/// case) or the lock itself couldn't even be read (permissions, ...) —
/// treated the same as "no worker", since the caller always has a working
/// direct-file fallback for that case. A non-blocking peek: this never
/// becomes the worker itself, even momentarily beyond the instant
/// `try_acquire` takes to check — an `Us` guard is dropped immediately,
/// releasing the flock right back.
pub fn discover(canvas_path: &Path) -> Option<u16> {
    match worker_lock::try_acquire(canvas_path) {
        Ok(Acquired::Other { port }) => Some(port),
        Ok(Acquired::Us(_guard)) => None,
        Err(_) => None,
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

/// PATCH `/api/nodes/:id` with just `{"text": body}` — the same
/// `mdcanvas::set_node_body` primitive `update_node`'s own `req.text`
/// handling already applies (`crates/server/src/lib.rs::update_node`), so
/// this has identical semantics to `apply_node_body`'s direct-file version.
pub async fn update_node_body(port: u16, node_id: &str, body: &str) -> Result<(), String> {
    let url = node_url(port, node_id)?;
    let res = reqwest::Client::new()
        .patch(url)
        .json(&serde_json::json!({ "text": body }))
        .send()
        .await
        .map_err(|e| e.to_string())?;
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
    let res = reqwest::Client::new()
        .delete(url)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    into_result(res).await
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
        text
    })
}

fn base_url(port: u16) -> String {
    format!("http://127.0.0.1:{port}")
}

// =======================================================================
// TUI-as-client: canvas load, running, services, whole-file save. Shares
// the `discover`/`node_url`/`into_result` primitives above with the
// CLI/MCP node-op routing already in this module — one client module for
// every way a frontend here talks to a worker, not one per frontend.
// =======================================================================

/// `GET /api/canvas/raw` — the worker's own in-memory copy of the primary
/// document's raw Markdown text (`state.raw`), unresolved (no `include`
/// splicing) — the same "raw-file-only scope" `App.raw`/`App.canvas`
/// already have. This is what `App::new` loads instead of
/// `std::fs::read_to_string` when a worker is reachable; `display_canvas`
/// (the include-resolved tree) is still built locally from it
/// (`meshfox_core::include::resolve`), same as the no-worker fallback —
/// there's no separate "fetch the resolved tree" round trip, since local
/// include-resolution is already needed either way (e.g. for edits that
/// land in an `include` target file).
pub async fn get_canvas_raw(port: u16) -> Result<String, String> {
    let res = reqwest::get(format!("{}/api/canvas/raw", base_url(port)))
        .await
        .map_err(|e| e.to_string())?;
    if !res.status().is_success() {
        let status = res.status();
        let text = res.text().await.unwrap_or_default();
        return Err(if text.is_empty() { status.to_string() } else { text });
    }
    res.text().await.map_err(|e| e.to_string())
}

/// `PUT /api/canvas/raw` — whole-document replace, what the source
/// editor's own `Ctrl-s` save routes through when the edited path is the
/// worker's own primary canvas (an `include` target file isn't addressable
/// through this endpoint — see `App::save_source_editor`'s own fallback).
/// A `422` (parse failure) comes back as plain text, same `ApiError`
/// `IntoResponse` every other mutating endpoint here already uses.
pub async fn put_canvas_raw(port: u16, text: &str) -> Result<(), String> {
    let res = reqwest::Client::new()
        .put(format!("{}/api/canvas/raw", base_url(port)))
        .body(text.to_string())
        .send()
        .await
        .map_err(|e| e.to_string())?;
    into_result(res).await
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
        use futures_util::StreamExt;
        #[derive(serde::Deserialize)]
        #[serde(tag = "type", rename_all = "kebab-case", rename_all_fields = "camelCase")]
        enum WatchMsg {
            Connected,
            Changed,
            Resync,
            RunStarted { node_id: String, block: String },
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
            while let Some(Ok(msg)) = ws.next().await {
                let tokio_tungstenite::tungstenite::Message::Text(text) = msg else { continue };
                let Ok(parsed) = serde_json::from_str::<WatchMsg>(&text) else { continue };
                let event = match parsed {
                    WatchMsg::Connected => continue,
                    WatchMsg::Changed
                    | WatchMsg::Resync
                    | WatchMsg::NodeUpserted
                    | WatchMsg::NodeRemoved
                    | WatchMsg::NodesReordered
                    | WatchMsg::Other => WatchEvent::Changed,
                    WatchMsg::RunStarted { node_id, block } => WatchEvent::RunStarted { node_id, block },
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
#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase")]
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
pub fn subscribe_run(port: u16, node_id: String, block: String) -> tokio::sync::mpsc::UnboundedReceiver<SubscribeEvent> {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(async move {
        use futures_util::StreamExt;
        let url = reqwest::Url::parse_with_params(
            &format!("ws://127.0.0.1:{port}/api/run/subscribe"),
            &[("nodeId", node_id.as_str()), ("block", block.as_str())],
        );
        let Ok(url) = url else { return };
        let Ok((mut ws, _)) = tokio_tungstenite::connect_async(url.as_str()).await else {
            return;
        };
        while let Some(Ok(msg)) = ws.next().await {
            let tokio_tungstenite::tungstenite::Message::Text(text) = msg else { continue };
            let Ok(event) = serde_json::from_str::<SubscribeEvent>(&text) else { continue };
            if tx.send(event).is_err() {
                return;
            }
        }
    });
    rx
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
pub async fn get_vars(port: u16, path: &[String], block: &str, no_deps: bool) -> Result<Vec<VarStatus>, String> {
    let url = reqwest::Url::parse_with_params(
        &format!("{}/api/vars", base_url(port)),
        &[
            ("path", path.join(",")),
            ("block", block.to_string()),
            ("noDeps", no_deps.to_string()),
        ],
    )
    .map_err(|e| e.to_string())?;
    let res = reqwest::get(url).await.map_err(|e| e.to_string())?;
    if !res.status().is_success() {
        let status = res.status();
        let text = res.text().await.unwrap_or_default();
        return Err(if text.is_empty() { status.to_string() } else { text });
    }
    res.json().await.map_err(|e| e.to_string())
}

/// `GET /api/vars/configure` — every declared non-secret, non-session,
/// non-`from=` variable in the whole document, the worker-routed
/// counterpart to `App::trigger_configure`'s local-mode branch (which
/// reads `self.decls`/`self.var_cache` directly). Unlike `get_vars`, never
/// scoped to one block's own chain.
pub async fn get_configure_vars(port: u16) -> Result<Vec<VarStatus>, String> {
    let res = reqwest::get(format!("{}/api/vars/configure", base_url(port)))
        .await
        .map_err(|e| e.to_string())?;
    if !res.status().is_success() {
        let status = res.status();
        let text = res.text().await.unwrap_or_default();
        return Err(if text.is_empty() { status.to_string() } else { text });
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
pub async fn post_configure_vars(port: u16, vars: HashMap<String, String>) -> Result<usize, String> {
    let res = reqwest::Client::new()
        .post(format!("{}/api/vars/configure", base_url(port)))
        .json(&serde_json::json!({ "vars": vars }))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !res.status().is_success() {
        let status = res.status();
        let text = res.text().await.unwrap_or_default();
        return Err(if text.is_empty() { status.to_string() } else { text });
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
    let res = reqwest::Client::new()
        .post(format!("{}/api/form/submit", base_url(port)))
        .json(&serde_json::json!({ "nodeId": node_id, "block": block, "values": values }))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !res.status().is_success() {
        let status = res.status();
        let text = res.text().await.unwrap_or_default();
        return Err(if text.is_empty() { status.to_string() } else { text });
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
    Ok(response.autorun_triggered.into_iter().map(|t| (t.node_id, t.block)).collect())
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
#[serde(tag = "type", rename_all = "kebab-case", rename_all_fields = "camelCase")]
pub enum RunEvent {
    Started { run_id: String },
    StepStart { node_id: String, block: String },
    StepSkipped { node_id: String, block: String, output: String, duration_ms: u64 },
    Output { node_id: String, block: String, stream: OutputStream, text: String },
    TtyStart { node_id: String, block: String },
    ServiceStarted { node_id: String, block: String, pid: u32 },
    LockConflict { node_id: String, block: String, owner_pid: u32, owner_desc: String },
    StepEnd { node_id: String, block: String, exit_code: i32, duration_ms: u64 },
    Killed { node_id: String, block: String },
    Error { message: String },
    Done { exit_code: i32 },
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
pub async fn run_stream(
    port: u16,
    path: &[String],
    block: &str,
    no_deps: bool,
    vars: HashMap<String, String>,
    save_secrets: HashSet<String>,
    force: Option<(String, String)>,
) -> Result<tokio::sync::mpsc::UnboundedReceiver<RunEvent>, String> {
    let vars_json = serde_json::to_string(&vars).map_err(|e| e.to_string())?;
    let secrets_json = serde_json::to_string(&save_secrets).map_err(|e| e.to_string())?;
    let route = if force.is_some() { "/api/run/force" } else { "/api/run" };
    let mut params = vec![
        ("path", path.join(",")),
        ("block", block.to_string()),
        ("noDeps", no_deps.to_string()),
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
        use futures_util::StreamExt;
        while let Some(Ok(msg)) = ws.next().await {
            let tokio_tungstenite::tungstenite::Message::Text(text) = msg else { continue };
            let Ok(event) = serde_json::from_str::<RunEvent>(&text) else { continue };
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
pub async fn kill_run(port: u16, node_id: &str, block: &str) -> Result<(), String> {
    let res = reqwest::Client::new()
        .post(format!("{}/api/kill", base_url(port)))
        .json(&serde_json::json!({ "nodeId": node_id, "block": block }))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    into_result(res).await
}

/// A live `/api/run/tty`/`/api/run/tty/attach` connection — binary frames
/// are raw pty bytes each direction, text frames are a `RunEvent` (server
/// to client, before and interleaved around the actual pty relay) or a
/// `{"cols":..,"rows":..}` resize (client to server, only meaningful once
/// `RunEvent::TtyStart` has arrived). See `crate::tui::mod`'s own
/// `bridge_http_tty` for the actual byte-relay loop this feeds.
pub type TtySocket = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

#[derive(Debug)]
pub enum TtyConnectError {
    /// A `409` — same meaning as a plain run's `RunEvent::LockConflict`,
    /// just returned pre-upgrade as a plain HTTP response rather than a
    /// streamed event (`/api/run/tty` keeps its own older pre-upgrade-`409`
    /// shape — out of scope for the `/api/run` WS conversion, see
    /// TODO.canvas.md). Unlike a plain run's conflict, there's no
    /// `/api/run/tty/force` to retry
    /// through (`TtyRunQuery` has no `force` field at all) — the confirm
    /// flow instead kills the stale/foreign owner directly via
    /// `meshfox_core::service_lock` against the same on-disk lock file the
    /// worker itself would have written (computed locally via
    /// `meshfox_core::locate_node`/`service_lock_path`, same as local-mode
    /// conflict handling already does), then simply retries this exact
    /// same `tty_connect` call — see `App::on_service_conflict_key`'s
    /// `is_tty` branch.
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
    TtyConnectError::Other(if text.is_empty() { resp.status().to_string() } else { text })
}

/// `GET /api/run/tty` — the WS client that starts (and, for the duration of
/// its own pty step, owns) an interactive `tty` chain run. `path`/`block`/
/// `no_deps`/`vars`/`save_secrets` mean exactly what they do for
/// `run_stream`; `cols`/`rows` seed the pty's *initial* size (the real
/// terminal's current size — later resizes go over this same socket as a
/// `{"cols":..,"rows":..}` text frame, see `TtySocket`'s own doc comment).
/// No `force` parameter — see `TtyConnectError::Conflict`'s own doc
/// comment for why a conflict retries differently here than a plain run's
/// does.
#[allow(clippy::too_many_arguments)]
pub async fn tty_connect(
    port: u16,
    path: &[String],
    block: &str,
    no_deps: bool,
    vars: HashMap<String, String>,
    save_secrets: HashSet<String>,
    cols: u16,
    rows: u16,
) -> Result<TtySocket, TtyConnectError> {
    let vars_json = serde_json::to_string(&vars).map_err(|e| TtyConnectError::Other(e.to_string()))?;
    let secrets_json =
        serde_json::to_string(&save_secrets).map_err(|e| TtyConnectError::Other(e.to_string()))?;
    let url = reqwest::Url::parse_with_params(
        &format!("ws://127.0.0.1:{port}/api/run/tty"),
        &[
            ("path", path.join(",")),
            ("block", block.to_string()),
            ("noDeps", no_deps.to_string()),
            ("vars", vars_json),
            ("saveSecrets", secrets_json),
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
    let res = reqwest::get(format!("{}/api/runs", base_url(port)))
        .await
        .map_err(|e| e.to_string())?;
    if !res.status().is_success() {
        let status = res.status();
        let text = res.text().await.unwrap_or_default();
        return Err(if text.is_empty() { status.to_string() } else { text });
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
    let res = reqwest::get(format!("{}/api/services", base_url(port)))
        .await
        .map_err(|e| e.to_string())?;
    if !res.status().is_success() {
        let status = res.status();
        let text = res.text().await.unwrap_or_default();
        return Err(if text.is_empty() { status.to_string() } else { text });
    }
    res.json().await.map_err(|e| e.to_string())
}

/// `GET /api/services/log?nodeId=..&block=..` — the worker-routed
/// equivalent of `ServiceHandle::log_snapshot()`, polled on demand (see
/// `App::refresh_service_log`) rather than streamed, same as the server's
/// own doc comment on `get_service_log` explains for why the web UI does
/// the same.
pub async fn get_service_log(port: u16, node_id: &str, block: &str) -> Result<Vec<(OutputStream, String)>, String> {
    let url = reqwest::Url::parse_with_params(
        &format!("{}/api/services/log", base_url(port)),
        &[("nodeId", node_id), ("block", block)],
    )
    .map_err(|e| e.to_string())?;
    let res = reqwest::get(url).await.map_err(|e| e.to_string())?;
    if !res.status().is_success() {
        let status = res.status();
        let text = res.text().await.unwrap_or_default();
        return Err(if text.is_empty() { status.to_string() } else { text });
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
    let res = reqwest::Client::new()
        .post(format!("{}/api/services/stop", base_url(port)))
        .json(&serde_json::json!({ "nodeId": node_id, "block": block }))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    into_result(res).await
}

/// Returns the restarted instance's own pid (`ServiceActionResponse::pid`).
pub async fn restart_service(port: u16, node_id: &str, block: &str) -> Result<u32, String> {
    let res = reqwest::Client::new()
        .post(format!("{}/api/services/restart", base_url(port)))
        .json(&serde_json::json!({ "nodeId": node_id, "block": block }))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !res.status().is_success() {
        let status = res.status();
        let text = res.text().await.unwrap_or_default();
        return Err(if text.is_empty() { status.to_string() } else { text });
    }
    #[derive(serde::Deserialize)]
    struct Resp {
        pid: u32,
    }
    res.json::<Resp>().await.map(|r| r.pid).map_err(|e| e.to_string())
}

