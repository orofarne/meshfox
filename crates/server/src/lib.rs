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
    routing::{get, patch, post, put},
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

/// `pub` so `meshfox-cli`'s TUI can reuse the same SSRF-safe fetch +
/// OpenGraph parsing + cache in-process (see its own `App` struct) rather
/// than duplicating it — the TUI doesn't talk to `meshfox serve` over
/// HTTP, it links this crate as a plain library.
pub mod link_preview;
mod pty_exec;
/// `pub` so `meshfox-cli` can reuse the same async spawn/kill primitives
/// for `meshfox run`'s real-time output — see its `main.rs`.
pub mod stream_exec;

#[derive(RustEmbed)]
#[folder = "../../web/dist"]
struct WebAssets;

/// Finds the first embedded `web/dist` asset whose own filename starts with
/// `prefix` and ends with `suffix` — `pub` so `meshfox-cli`'s `pdf` command
/// can reuse the exact same embedded Fira Code bytes this crate already
/// carries for the web UI (`web/src/main.tsx`'s own `@fontsource/fira-code`
/// imports), rather than a second `include_bytes!`-embedded copy of its
/// own. A prefix/suffix match (not an exact path) because Vite
/// content-hashes every built asset's filename (`fira-code-latin-400-
/// normal-DGosTW8U.woff2`) — there's no fixed path to ask for directly.
///
/// `None` either because nothing matches, or because the web UI hasn't
/// been built into this binary yet (`web/dist` empty — the exact same gap
/// `serve_embedded`'s own `WebAssets::get("index.html")` fallback already
/// handles) — a caller embedding a font this way should degrade
/// gracefully (skip that `@font-face`, let the page's own CSS fallback
/// stack take over) rather than treat this as an error: `cargo build
/// -p meshfox-cli` on its own, without `web/` ever being built, is a
/// supported workflow (see this crate's own module doc comment) that
/// shouldn't break `meshfox pdf` just because of that.
pub fn find_web_asset(prefix: &str, suffix: &str) -> Option<Vec<u8>> {
    WebAssets::iter()
        .find(|path| {
            let name = path.rsplit('/').next().unwrap_or(path);
            name.starts_with(prefix) && name.ends_with(suffix)
        })
        .and_then(|path| WebAssets::get(&path))
        .map(|file| file.data.into_owned())
}

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
    /// `link` node social-preview cache (see `link_preview`) — one entry
    /// per URL, alive for exactly this process's lifetime.
    link_preview_cache: link_preview::PreviewCache,
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
        if remaining == 0
            && self.state.auto_exit
            && self.state.ever_connected.load(Ordering::SeqCst)
        {
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
        let mut last_mtime = std::fs::metadata(&state.canvas_path)
            .and_then(|m| m.modified())
            .ok();
        loop {
            std::thread::sleep(Duration::from_millis(500));
            let Ok(meta) = std::fs::metadata(&state.canvas_path) else {
                continue;
            };
            let Ok(mtime) = meta.modified() else { continue };
            if Some(mtime) == last_mtime {
                continue;
            }
            last_mtime = Some(mtime);
            let Ok(contents) = std::fs::read_to_string(&state.canvas_path) else {
                continue;
            };
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
/// A `required` variable that's still unconfirmed shows up as
/// `resolved: false` with its own `default` carried in `value` anyway —
/// not because it's resolved, but so the form the UI opens for it can
/// still offer that default as a pre-filled suggestion instead of a blank
/// field (see `meshfox_core::vars::resolve`).
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
    let path: Vec<&str> = if query.path.is_empty() {
        Vec::new()
    } else {
        query.path.split(',').collect()
    };
    let chain = meshfox_core::resolve_run_chain(&canvas, &path, &query.block, !query.no_deps)?;
    let needed = meshfox_core::env_var_names_for_chain(&canvas, &chain);

    let decls = meshfox_core::declared_vars(&canvas)
        .map_err(|e| ApiError(StatusCode::UNPROCESSABLE_ENTITY, e.to_string()))?;
    // A `from`-declared (computed) variable is never something a human
    // fills in — it's produced by running its own source block mid-chain
    // (see `meshfox_core::varout`) — so the pre-run form must never offer
    // one as a field, regardless of whether it's currently "resolved".
    let relevant: Vec<_> = decls
        .into_iter()
        .filter(|d| needed.contains(&d.name) && d.from.is_none())
        .collect();

    let cache = state.vars_cache.lock().unwrap();
    let resolved = meshfox_core::resolve_vars(&relevant, &HashMap::new(), &cache, &HashMap::new());
    let statuses = relevant
        .into_iter()
        .map(|d| var_status(d, &resolved))
        .collect();
    Ok(Json(statuses))
}

/// Builds one `VarStatus` from a declaration and the already-computed
/// `ResolvedVars` for the whole batch — split out from `get_vars` so this
/// (pure, `State`/`Query`-free) mapping is unit-testable on its own.
fn var_status(d: meshfox_core::VarDecl, resolved: &meshfox_core::ResolvedVars) -> VarStatus {
    let resolved_value = resolved.values.get(&d.name).cloned();
    let is_resolved = resolved_value.is_some();
    // A `required` declaration with nothing else supplying it lands in
    // `resolved.values` as absent (see `vars::resolve`) even though it has
    // a `default` — fall back to that `default` here purely so the form
    // still has something to pre-fill, without marking it `resolved`.
    let value = resolved_value.or_else(|| d.default.clone());
    VarStatus {
        name: d.name,
        var_type: d.var_type.as_str(),
        prompt: d.prompt,
        choices: d.choices,
        secret: d.secret,
        resolved: is_resolved,
        value: if d.secret { None } else { value },
    }
}

/// Validates every entry in `overrides` naming one of `decls` against that
/// declaration's own `type` (`meshfox_core::validate_value`) — the one
/// place a run request's `vars` bypasses whatever control the form that
/// collected them used (a `select` dropdown, a `bool` checkbox), same
/// concern `post_configure_vars` has its own copy of this check for.
/// Shared by `run_block`/`run_block_tty`, both of which resolve `vars`
/// straight into a spawned block's environment — an `int` field a client
/// (or a hand-typed curl request) sent as `"not-a-number"` should fail the
/// request outright, not run the block with a garbage value.
fn validate_var_overrides(
    decls: &[meshfox_core::VarDecl],
    overrides: &HashMap<String, String>,
) -> Result<(), ApiError> {
    for decl in decls {
        if let Some(value) = overrides.get(&decl.name) {
            meshfox_core::validate_value(decl, value)
                .map_err(|e| ApiError(StatusCode::UNPROCESSABLE_ENTITY, e))?;
        }
    }
    Ok(())
}

/// `GET /api/vars/configure` — every declared *non-secret* `meshfox:var`
/// in the whole document, in declaration order, regardless of which (if
/// any) block's `env=` actually references it. The browser counterpart to
/// `meshfox configure` (see `crates/cli/src/main.rs`'s `configure`, and
/// the TUI's `c` key): unlike `GET /api/vars`, this is never scoped to one
/// block's chain, and a `secret` declaration is left out entirely — same
/// as the CLI, asking for one that's never cached and immediately
/// discarded again wouldn't do anything useful.
async fn get_configure_vars(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Vec<VarStatus>>, ApiError> {
    let raw = state.raw.lock().unwrap().clone();
    let canvas = parse_or_error(&raw)?;
    let decls = meshfox_core::declared_vars(&canvas)
        .map_err(|e| ApiError(StatusCode::UNPROCESSABLE_ENTITY, e.to_string()))?;
    // Same reasoning as `get_vars`: a `from`-declared variable is computed,
    // never something to configure by hand.
    let configurable: Vec<_> = decls
        .into_iter()
        .filter(|d| !d.secret && d.from.is_none())
        .collect();

    let cache = state.vars_cache.lock().unwrap();
    let resolved = meshfox_core::resolve_vars(&configurable, &HashMap::new(), &cache, &HashMap::new());
    let statuses = configurable
        .into_iter()
        .map(|d| var_status(d, &resolved))
        .collect();
    Ok(Json(statuses))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ConfigureVarsRequest {
    vars: HashMap<String, String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ConfigureVarsResponse {
    saved: usize,
}

/// `POST /api/vars/configure` — the submit side of the form `GET`'s own
/// endpoint feeds: every entry in `req.vars` naming a declared non-secret
/// variable is written to the cache, *even if unchanged* from what was
/// already there — same as `meshfox configure` always rewriting the
/// cache with whatever's answered, confirmed or not, rather than only on
/// an actual change. Doesn't run anything; this only ever updates the
/// cache.
async fn post_configure_vars(
    State(state): State<Arc<AppState>>,
    Json(req): Json<ConfigureVarsRequest>,
) -> Result<Json<ConfigureVarsResponse>, ApiError> {
    let raw = state.raw.lock().unwrap().clone();
    let canvas = parse_or_error(&raw)?;
    let decls = meshfox_core::declared_vars(&canvas)
        .map_err(|e| ApiError(StatusCode::UNPROCESSABLE_ENTITY, e.to_string()))?;
    let configurable: Vec<_> = decls.into_iter().filter(|d| !d.secret).collect();

    // Validated before anything is saved — this is the one boundary none
    // of the form's own controls (a `bool` checkbox, a `select` dropdown)
    // can actually enforce, since a request can always bypass them
    // entirely. An invalid entry anywhere in the batch fails the whole
    // request rather than saving some fields and silently skipping
    // others.
    for decl in &configurable {
        if let Some(value) = req.vars.get(&decl.name) {
            meshfox_core::validate_value(decl, value)
                .map_err(|e| ApiError(StatusCode::UNPROCESSABLE_ENTITY, e))?;
        }
    }

    let mut cache = state.vars_cache.lock().unwrap();
    let mut saved = 0;
    for decl in &configurable {
        if let Some(value) = req.vars.get(&decl.name) {
            cache.set(&decl.name, value).map_err(|e| {
                ApiError(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("failed to save {}: {e}", decl.name),
                )
            })?;
            saved += 1;
        }
    }
    Ok(Json(ConfigureVarsResponse { saved }))
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
#[serde(
    tag = "type",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase"
)]
enum RunEvent {
    /// Always first. `runId` is what `/api/kill` takes to cancel this run.
    Started {
        run_id: String,
    },
    StepStart {
        node_id: String,
        block: String,
    },
    /// One line of merged stdout/stderr, as it's produced.
    Output {
        node_id: String,
        block: String,
        text: String,
    },
    /// `/api/run/tty`'s WebSocket only: emitted right after `StepStart` for
    /// a `tty` step, instead of any `Output`. From here until the matching
    /// `StepEnd`, every other WebSocket frame for this run is raw pty I/O,
    /// not a `RunEvent` — binary frames are pty output bytes (server to
    /// client) or input bytes to type into the pty (client to server); a
    /// client text frame in this window is a resize control message
    /// (`{"cols":..,"rows":..}`), not a `RunEvent`. See SPEC.md's
    /// "Interactive (`tty`) blocks" — `/api/run`'s plain NDJSON stream
    /// never runs a `tty` block to begin with, so it never emits this.
    TtyStart {
        node_id: String,
        block: String,
    },
    StepEnd {
        node_id: String,
        block: String,
        exit_code: i32,
    },
    /// Terminal for this run — no `Done` follows. Emitted for whichever
    /// step was actively running when `/api/kill` fired; later chain steps
    /// (if any) never start.
    Killed {
        node_id: String,
        block: String,
    },
    /// Terminal for this run — something failed before/without a step
    /// producing a normal exit code (bad node/block reference, no
    /// executor for the language, an I/O error spawning the process).
    Error {
        message: String,
    },
    /// Terminal for this run. `exitCode` mirrors whichever step ran last —
    /// the requested block's own, unless an earlier dependency failed and
    /// stopped the chain first (same stop-on-failure rule `meshfox run`
    /// already has).
    Done {
        exit_code: i32,
    },
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
    Canvas::from_markdown(raw)
        .map_err(|e| ApiError(StatusCode::UNPROCESSABLE_ENTITY, e.to_string()))
}

/// Builds the same response shape `GET /api/canvas` returns (parse, splice
/// in `include`s) from a given raw document — shared by `get_canvas` and
/// every mutating endpoint below, so a create/update response always looks
/// exactly like a fresh `GET` would. Unpositioned nodes are sent over the
/// wire exactly as parsed (no server-computed layout suggestion) — the web
/// client lays those out itself, client-side, since it's the one that
/// actually knows the browser's viewport size and each node's real
/// rendered content height (see `web/src/autolayout.ts`).
/// Parse + splice in every `include` node's target (see
/// `meshfox_core::include`) — never written back to the file, so a client
/// editing and PUTting this response back would silently drop any
/// include-only content; the UI treats included subtrees as read-only for
/// now. Shared by `canvas_response` and `get_include_asset` (the latter
/// needs the resolved tree's `asset_base`s, not the JSON response itself).
fn resolved_canvas(raw: &str, canvas_path: &std::path::Path) -> Result<Canvas, ApiError> {
    let canvas = parse_or_error(raw)?;
    meshfox_core::include::resolve(&canvas, canvas_path)
        .map_err(|e| ApiError(StatusCode::UNPROCESSABLE_ENTITY, e.to_string()))
}

fn canvas_response(raw: &str, canvas_path: &std::path::Path) -> Result<Json<Canvas>, ApiError> {
    let mut canvas = resolved_canvas(raw, canvas_path)?;
    // Every embedded constraint fence's script itself is cheap and pure
    // (tick/heap/callstack-bounded — see `constraint::evaluate`); the only
    // I/O it can trigger is a `file`-type node's own already-declared
    // target, capped the same way the `display="code"` preview is (see
    // `meshfox_core::file_read`). Running every constraint on every fetch
    // (rather than only on an explicit `meshfox check`) is still safe and
    // keeps the UI's pass/fail badges current without a separate endpoint
    // or a stale on-disk cache to invalidate — just no longer free of disk
    // reads for a document whose constraints reach into `file` nodes.
    let canvas_dir = canvas_path.parent().filter(|p| !p.as_os_str().is_empty());
    let canvas_dir = Some(canvas_dir.unwrap_or(std::path::Path::new(".")));
    meshfox_core::constraint::annotate_status(&mut canvas, canvas_dir);
    // Best-effort: a malformed `meshfox:option` declaration shouldn't break
    // *viewing* the canvas (falls back to no options declared, same as if
    // there were none at all) — `meshfox validate` is what surfaces that
    // loudly, same split `vars`/constraint fences already have between
    // "parses enough to view" and "fully valid".
    canvas.options = meshfox_core::declared_options(&canvas).unwrap_or_default();
    Ok(Json(canvas))
}

async fn get_canvas(State(state): State<Arc<AppState>>) -> Result<Json<Canvas>, ApiError> {
    let raw = state.raw.lock().unwrap().clone();
    canvas_response(&raw, &state.canvas_path)
}

/// Which physical file (and node id *in that file*) a mutating endpoint
/// should actually read/patch/write for a given node id as seen in the
/// composed/resolved canvas the UI displays — `state`'s own document
/// itself, or, for a node spliced in from a canvas `include`, the include
/// target's own file, addressed by its un-namespaced id there
/// (`Node::origin_path`/`origin_id`, set by `include::resolve`). Every
/// mutating endpoint below routes through this first instead of assuming
/// `id` lives in `state`'s own raw text — that assumption is what made
/// editing an included subtree fail with a spurious "no node" before.
struct LocatedNode {
    /// `None` for `state`'s own document (already cached as
    /// `state.canvas_path`/`state.raw`); `Some(path)` for an include
    /// target — `raw` below is read fresh from disk each time rather than
    /// cached in `AppState`, since it isn't the file this server session
    /// "owns".
    origin: Option<PathBuf>,
    raw: String,
    /// `id`, translated to whatever it's actually called in `raw`.
    local_id: String,
}

fn locate_node(state: &AppState, primary_raw: &str, id: &str) -> Result<LocatedNode, ApiError> {
    let primary = parse_or_error(primary_raw)?;
    if primary.node(id).is_some() {
        return Ok(LocatedNode {
            origin: None,
            raw: primary_raw.to_string(),
            local_id: id.to_string(),
        });
    }
    // Not in `state`'s own raw text — maybe it's spliced in from an
    // include. Resolve fully (splices every include, however deeply
    // nested) and look it up there instead.
    let resolved = resolved_canvas(primary_raw, &state.canvas_path)?;
    let node = resolved
        .node(id)
        .ok_or_else(|| ApiError(StatusCode::NOT_FOUND, format!("no node {id:?}")))?;
    let origin_path = node.origin_path.clone().ok_or_else(|| {
        ApiError(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!(
                "node {id:?} lives inside an included canvas that can't be edited from here yet \
                 (it was included as plain Markdown rather than a .canvas.md, so it has no node \
                 identity of its own to write back to — open its own file directly to edit it)"
            ),
        )
    })?;
    let local_id = node
        .origin_id
        .clone()
        .expect("origin_path is always set together with origin_id");
    let raw = std::fs::read_to_string(&origin_path).map_err(|e| {
        ApiError(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to read include target {origin_path}: {e}"),
        )
    })?;
    Ok(LocatedNode {
        origin: Some(PathBuf::from(origin_path)),
        raw,
        local_id,
    })
}

/// Writes `raw` back to wherever `located` says it actually came from —
/// `state`'s own file (also updating its in-memory cache, same as every
/// mutating endpoint already did) or an include target's file directly
/// (never cached — see `LocatedNode::origin`). The caller still has to
/// re-read `state.raw`/call `canvas_response` afterward for the response:
/// for an include target, that's what actually picks the edit back up
/// (`include::resolve` always reads the target fresh from disk), the same
/// way a follow-up `GET /api/canvas` would.
fn commit_located(state: &AppState, located: &LocatedNode, raw: &str) -> Result<(), ApiError> {
    match &located.origin {
        None => {
            state
                .save(raw)
                .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
            *state.raw.lock().unwrap() = raw.to_string();
        }
        Some(path) => {
            std::fs::write(path, raw)
                .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        }
    }
    Ok(())
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct IncludeManifestEntry {
    node_id: String,
    title: String,
    target: String,
    depth: u32,
    is_canvas: bool,
}

/// Every `include` reachable from the document (however deeply nested),
/// resolved to the file it points at but without splicing anything in
/// (`meshfox_core::include::list_includes`) — what powers the Source-mode
/// editor's file picker (see `get_canvas_raw`/`put_canvas_raw`'s own
/// `?include=` param): the primary document's own entry is implicit (the
/// picker's own "this document" option), everything here is an
/// alternative to it.
async fn get_includes(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Vec<IncludeManifestEntry>>, ApiError> {
    let raw = state.raw.lock().unwrap().clone();
    let canvas = parse_or_error(&raw)?;
    let entries = meshfox_core::include::list_includes(&canvas, &state.canvas_path)
        .into_iter()
        .map(|i| IncludeManifestEntry {
            node_id: i.node_id,
            title: i.title,
            target: i.target,
            depth: i.depth,
            is_canvas: i.is_canvas,
        })
        .collect();
    Ok(Json(entries))
}

#[derive(Debug, Deserialize)]
struct SourceFileQuery {
    /// An include's own `nodeId` (see `get_includes`) — absent means the
    /// primary document itself.
    #[serde(default)]
    include: Option<String>,
}

/// Which file Source mode is actually pointed at — `state`'s own document,
/// always canvas-shaped, or an include target, which is only required to
/// parse as a canvas itself when it's a *canvas* include (`is_canvas`) —
/// a plain-Markdown include target is, by definition, ordinary prose with
/// no `meshfox:canvas` structure to hold it to (see `crate::include`'s own
/// module docs), so validating it as one would reject perfectly good text.
enum SourceFile {
    Primary,
    Include { path: PathBuf, is_canvas: bool },
}

/// Resolves `query`'s optional `?include=<nodeId>` to the file Source mode
/// should actually read/write. Recomputes the include's target path fresh
/// every call (via `list_includes`) rather than trusting a client-supplied
/// path — `include` naming something that no longer resolves (a
/// since-removed or now-broken include) is a 404, same as any other
/// stale-id case elsewhere in this file.
fn resolve_source_file(state: &AppState, include: Option<&str>) -> Result<SourceFile, ApiError> {
    let Some(include_id) = include else {
        return Ok(SourceFile::Primary);
    };
    let raw = state.raw.lock().unwrap().clone();
    let canvas = parse_or_error(&raw)?;
    meshfox_core::include::list_includes(&canvas, &state.canvas_path)
        .into_iter()
        .find(|i| i.node_id == include_id)
        .map(|i| SourceFile::Include {
            path: i.path,
            is_canvas: i.is_canvas,
        })
        .ok_or_else(|| ApiError(StatusCode::NOT_FOUND, format!("no include {include_id:?}")))
}

/// The document's raw Markdown text, verbatim — what the UI's Source-mode
/// editor loads. Unlike `get_canvas`, this does *not* splice in `include`s;
/// it's the actual on-disk bytes this file owns. `?include=<nodeId>` (see
/// `get_includes`) switches to an include target's own raw text instead,
/// read fresh from disk (never cached, unlike the primary document).
async fn get_canvas_raw(
    State(state): State<Arc<AppState>>,
    Query(query): Query<SourceFileQuery>,
) -> Result<String, ApiError> {
    match resolve_source_file(&state, query.include.as_deref())? {
        SourceFile::Primary => Ok(state.raw.lock().unwrap().clone()),
        SourceFile::Include { path, .. } => std::fs::read_to_string(&path).map_err(|e| {
            ApiError(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("failed to read {}: {e}", path.display()),
            )
        }),
    }
}

/// Overwrites the whole document (or, with `?include=<nodeId>`, an include
/// target's own file — see `get_canvas_raw`) with `body`, verbatim — the
/// Source-mode editor's Save button. Rejects (422, nothing written)
/// anything that doesn't parse *as a canvas* — skipped for a plain-Markdown
/// include target (see `SourceFile`), which has no such requirement to
/// begin with — same validate-before-commit guarantee every other
/// mutating endpoint here gives for what it does validate.
async fn put_canvas_raw(
    State(state): State<Arc<AppState>>,
    Query(query): Query<SourceFileQuery>,
    body: String,
) -> Result<StatusCode, ApiError> {
    let target = resolve_source_file(&state, query.include.as_deref())?;
    if !matches!(
        target,
        SourceFile::Include {
            is_canvas: false,
            ..
        }
    ) {
        parse_or_error(&body)?;
    }
    match target {
        SourceFile::Primary => {
            state
                .save(&body)
                .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
            *state.raw.lock().unwrap() = body;
        }
        SourceFile::Include { path, .. } => {
            std::fs::write(&path, &body)
                .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        }
    }
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
    let primary_raw = state.raw.lock().unwrap().clone();
    // Same routing every other mutating endpoint does (see `locate_node`):
    // a node here may have been spliced in from an include, in which case
    // its `meshfox:node` comment lives in a different file entirely.
    // Batched up per file (rather than one `locate_node`/write per node)
    // so a drag/resize touching several nodes in the same included canvas
    // patches that file's raw text incrementally and writes it once.
    let mut primary_out = primary_raw.clone();
    let mut included_out: HashMap<PathBuf, String> = HashMap::new();

    for node in &canvas.nodes {
        // Include nodes never reach the client as such (`get_canvas`
        // resolves them away first) but are skipped here too for the same
        // reason a stray unknown id already is below: `set_node_meta` finds
        // nothing to patch and no-ops, so this is belt-and-suspenders.
        if node.node_type == NodeType::Include {
            continue;
        }
        // A node the client posted back that no longer resolves to
        // anything (deleted meanwhile, or an include target that can't be
        // located — e.g. plain-Markdown include content, which has no
        // `meshfox:node` identity of its own) is skipped rather than
        // failing the whole batch, same "no-op for what doesn't apply"
        // tolerance `set_node_meta` returning `None` already had here.
        let Ok(located) = locate_node(&state, &primary_raw, &node.id) else {
            continue;
        };
        // A group's *size* is always derived from its members, never
        // authored — ignore whatever width/height it reports rather than
        // let a computed value get written into the file as if it were
        // real data. Its own *position*, though, is now a real anchor a
        // member's own `x`/`y` is relative to (see
        // `Canvas::resolve_absolute_position`), draggable like any other
        // node's — so only width/height are forced back to "unset" here.
        let is_group = node.node_type == NodeType::Group;
        let meta = NodeMeta {
            x: node.x,
            y: node.y,
            width: if is_group { None } else { node.width },
            height: if is_group { None } else { node.height },
            color: node.color.clone(),
            node_type: None,
            display: node.display,
            lang: node.lang.clone(),
            interpreter: node.interpreter.clone(),
            preview: Some(node.preview),
            edge_label: node.edge_label.clone(),
            fold: node.fold,
            tags: node.tags.clone(),
        };
        match &located.origin {
            None => {
                if let Some(patched) =
                    mdcanvas::set_node_meta(&primary_out, &located.local_id, &meta)
                {
                    primary_out = patched;
                }
            }
            Some(path) => {
                let current = included_out
                    .entry(path.clone())
                    .or_insert_with(|| located.raw.clone());
                if let Some(patched) = mdcanvas::set_node_meta(current, &located.local_id, &meta) {
                    *current = patched;
                }
            }
        }
    }

    if let Some(reordered) = mdcanvas::reorder_by_position(&primary_out) {
        primary_out = reordered;
    }
    for raw in included_out.values_mut() {
        if let Some(reordered) = mdcanvas::reorder_by_position(raw) {
            *raw = reordered;
        }
    }

    parse_or_error(&primary_out)?;
    for raw in included_out.values() {
        parse_or_error(raw)?;
    }

    state
        .save(&primary_out)
        .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    *state.raw.lock().unwrap() = primary_out;
    for (path, raw) in &included_out {
        std::fs::write(path, raw)
            .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    }
    Ok(StatusCode::NO_CONTENT)
}

/// Clears every node's stored `x`/`y`/`width`/`height` back to unset,
/// reverting the whole document to auto-placed (see
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
/// group's own *size* is always derived, never stored, so there's nothing
/// extra to clear on one there — but its own *position* (a real anchor its
/// members' own `x`/`y` are relative to, see
/// `Canvas::resolve_absolute_position`) is now clearable exactly like any
/// other node's, so this no longer skips groups: clearing layout should
/// fully revert a group to synthetic placement too, not leave a stale
/// dragged anchor behind. Unlike `put_canvas` (which only ever sees the
/// *resolved* canvas, where an `include` node has already been rewritten to
/// `text`/`group` by `include::resolve`), this reads straight off the raw,
/// unresolved parse — here, an `include` node is still the node that
/// *declares* the include right in this file, with its own real
/// `meshfox:node` comment (position and all), so it must be cleared exactly
/// like any other node.
async fn clear_layout(State(state): State<Arc<AppState>>) -> Result<Json<Canvas>, ApiError> {
    let mut raw = state.raw.lock().unwrap().clone();
    let canvas = parse_or_error(&raw)?;
    for node in &canvas.nodes {
        let meta = NodeMeta {
            x: None,
            y: None,
            width: None,
            height: None,
            color: node.color.clone(),
            node_type: None,
            display: node.display,
            lang: node.lang.clone(),
            interpreter: node.interpreter.clone(),
            preview: Some(node.preview),
            edge_label: node.edge_label.clone(),
            fold: node.fold,
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
    let primary_raw = state.raw.lock().unwrap().clone();
    // `parentId` may itself name a node spliced in from an include (e.g.
    // adding a child under something inside an included canvas) — locate
    // it first so the new node is actually written into the file the
    // parent lives in, same as editing an existing node there already is.
    let located = locate_node(&state, &primary_raw, &req.parent_id)?;
    let (updated, _new_id) =
        mdcanvas::insert_child_node(&located.raw, &located.local_id, &req.title).ok_or_else(
            || {
                ApiError(
                    StatusCode::NOT_FOUND,
                    format!("no node {:?}", req.parent_id),
                )
            },
        )?;
    // Insertion can't actually break parsing, but validate anyway — same
    // validate-before-commit shape every other mutating endpoint here uses.
    parse_or_error(&updated)?;
    commit_located(&state, &located, &updated)?;
    let response_raw = state.raw.lock().unwrap().clone();
    canvas_response(&response_raw, &state.canvas_path)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdateOptionsRequest {
    options: Vec<String>,
}

/// `PUT /api/options` — replaces the document's whole set of declared
/// `meshfox:option` names (see SPEC.md's "Options") with exactly
/// `req.options`, in the given order; an empty list removes every
/// declaration. The browser's "document options" toolbar button/modal is
/// the only caller — the write-path counterpart to `GET /api/canvas`
/// already surfacing `canvas.options` (`declared_options`, see
/// `canvas_response` above). Unlike `meshfox:var` (never written by any
/// endpoint — see `POST /api/vars/configure`'s own doc comment), an option
/// is a bare presence flag with nothing to prompt for, so there's no
/// reason not to let the UI toggle it directly rather than requiring a
/// hand-edit of the file.
async fn put_options(
    State(state): State<Arc<AppState>>,
    Json(req): Json<UpdateOptionsRequest>,
) -> Result<Json<Canvas>, ApiError> {
    let raw = state.raw.lock().unwrap().clone();
    let updated = mdcanvas::set_document_options(&raw, &req.options).ok_or_else(|| {
        ApiError(
            StatusCode::UNPROCESSABLE_ENTITY,
            "document has no root node".to_string(),
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
    /// `file`-node interpreter (see `meshfox_core::Node::is_runnable_file`)
    /// — makes the node runnable via the web UI's "▷ run" button.
    interpreter: Option<String>,
    /// `link`-node social preview toggle (see `meshfox_core::Node::preview`)
    /// — `None` leaves the existing value untouched, same "not sent"
    /// convention as every other field here.
    preview: Option<bool>,
    /// Full replacement list of tags — `None` leaves them untouched,
    /// `Some(vec![])` clears them, same convention as `extraParents`.
    tags: Option<Vec<String>>,
    /// Structural-edge label (see `meshfox_core::Node::edge_label`) — the
    /// text shown on the implicit edge from this node's parent into it.
    /// `None` (key not sent) leaves it untouched; `Some("")` (sent, empty)
    /// clears it back to unset rather than writing a literal `edgeLabel=""`
    /// — see this field's own handling in `update_node`.
    edge_label: Option<String>,
    /// Per-node fold-state override (see `meshfox_core::Node::fold`) —
    /// `None` (the field not sent at all) leaves it untouched, same
    /// convention as every other field here. Unlike those, though, this
    /// one's own *target* type (`Option<bool>`) already has its own
    /// "unset" state to reach — plain JSON `null` is indistinguishable
    /// from an absent field to `serde`'s usual `Option<T>` handling, so
    /// this is a string sentinel instead: `"true"`/`"false"` set an
    /// explicit override, `"default"` clears back to "follow the
    /// document's own default" (see `resolve_fold_override`).
    fold: Option<String>,
}

/// `req.fold`'s string sentinel (see `UpdateNodeRequest::fold`'s own doc
/// comment) resolved against `existing` (the node's current value, kept
/// when nothing was sent) into the `Option<bool>` `Node::fold` itself
/// wants. `422` for anything other than `"true"`/`"false"`/`"default"`.
fn resolve_fold_override(
    raw: Option<&str>,
    existing: Option<bool>,
) -> Result<Option<bool>, ApiError> {
    match raw {
        None => Ok(existing),
        Some(s) => meshfox_core::parse_fold_override(s)
            .map_err(|e| ApiError(StatusCode::UNPROCESSABLE_ENTITY, e)),
    }
}

#[cfg(test)]
mod resolve_fold_override_tests {
    use super::*;

    fn expect_ok(result: Result<Option<bool>, ApiError>) -> Option<bool> {
        match result {
            Ok(v) => v,
            Err(e) => panic!("unexpected error: {}", e.1),
        }
    }

    #[test]
    fn not_sent_keeps_the_existing_value() {
        assert_eq!(
            expect_ok(resolve_fold_override(None, Some(true))),
            Some(true)
        );
        assert_eq!(expect_ok(resolve_fold_override(None, None)), None);
    }

    #[test]
    fn true_and_false_set_an_explicit_override() {
        assert_eq!(
            expect_ok(resolve_fold_override(Some("true"), None)),
            Some(true)
        );
        assert_eq!(
            expect_ok(resolve_fold_override(Some("false"), Some(true))),
            Some(false)
        );
    }

    #[test]
    fn default_clears_back_to_no_override() {
        assert_eq!(
            expect_ok(resolve_fold_override(Some("default"), Some(true))),
            None
        );
    }

    #[test]
    fn garbage_is_rejected() {
        let err = match resolve_fold_override(Some("bogus"), None) {
            Ok(_) => panic!("expected an error"),
            Err(e) => e,
        };
        assert_eq!(err.0, StatusCode::UNPROCESSABLE_ENTITY);
    }
}

/// Applies any of `title`/`nodeType`/`color`/`target`/`text`/`extraParents`/
/// `display`/`lang`/`interpreter`/`tags` present in the request to node
/// `id`, validating the fully-patched
/// document parses before saving anything — an invalid combination (e.g.
/// `target` on a still-`text` node, or `nodeType: group` with a non-empty
/// body) is rejected with `422` and leaves the file untouched, rather than
/// partially applying edits.
async fn update_node(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(req): Json<UpdateNodeRequest>,
) -> Result<Json<Canvas>, ApiError> {
    let primary_raw = state.raw.lock().unwrap().clone();
    // `id` may name a node spliced in from an include — route the whole
    // edit to the file it actually lives in (see `locate_node`) instead
    // of always patching `state`'s own document.
    let located = locate_node(&state, &primary_raw, &id)?;
    let mut raw = located.raw.clone();
    let local_id = located.local_id.clone();

    // A `from=` in `extraParents` names another node the same way `id`
    // itself was named — the composed/possibly-namespaced id the UI
    // showed it as — so it needs the same translation before it can be
    // written into `local_id`'s own file, and can only ever name a node
    // in that *same* file (an edge can't cross an include boundary: a
    // `meshfox:edge from="..."` is only ever resolved against its own
    // document's own id set — see `mdcanvas::parse`).
    let extra_parents_local = req
        .extra_parents
        .as_ref()
        .map(|edges| {
            edges
                .iter()
                .map(|e| {
                    let from_located = locate_node(&state, &primary_raw, &e.from)?;
                    if from_located.origin != located.origin {
                        return Err(ApiError(
                            StatusCode::UNPROCESSABLE_ENTITY,
                            format!(
                                "can't add an edge from {:?} to {id:?} — they live in different \
                                 files (an edge can't cross an include boundary)",
                                e.from
                            ),
                        ));
                    }
                    Ok(ExtraEdge {
                        from: from_located.local_id,
                        ..e.clone()
                    })
                })
                .collect::<Result<Vec<_>, ApiError>>()
        })
        .transpose()?;

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
    let initial_node = initial.node(&local_id).ok_or_else(not_found)?;
    // A plain-Markdown `include` node keeps its own id post-resolve (see
    // `include::resolve`'s module doc), so `locate_node` above found it
    // right here in the primary document — but its *body*, as the client
    // just saw it via `GET /api/canvas`, is the include target's own
    // (shifted-headings) content, not what's actually stored here (a bare
    // `[label](target)` link). Writing that back as `text` would silently
    // try to overwrite the link with the target's whole content — reject
    // it with a clear reason up front rather than relying on the later
    // `parse_or_error` to incidentally catch it as a mangled link body.
    if initial_node.node_type == NodeType::Include && req.text.is_some() {
        return Err(ApiError(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!(
                "node {id:?} is a plain-Markdown include — its shown body comes from the include \
                 target file, not from here; open that file directly to edit it (use `target` \
                 here to change which file it links to instead)"
            ),
        ));
    }
    let (
        x,
        y,
        width,
        height,
        existing_color,
        existing_display,
        existing_lang,
        existing_interpreter,
        existing_preview,
        existing_tags,
        existing_fold,
        existing_edge_label,
    ) = (
        initial_node.x,
        initial_node.y,
        initial_node.width,
        initial_node.height,
        initial_node.color.clone(),
        initial_node.display,
        initial_node.lang.clone(),
        initial_node.interpreter.clone(),
        initial_node.preview,
        initial_node.tags.clone(),
        initial_node.fold,
        initial_node.edge_label.clone(),
    );
    // `display`/`lang`/`interpreter` only mean anything on a `file` node —
    // clear them (rather than leave a stale attribute behind) whenever this
    // request moves the node to some other type. `preview` is the same idea
    // but for `link` nodes.
    let final_type = req.node_type.unwrap_or(initial_node.node_type);
    let mut title = initial_node.title.clone();

    if let Some(new_title) = &req.title {
        raw = mdcanvas::set_node_title(&raw, &local_id, new_title).ok_or_else(not_found)?;
        title = new_title.clone();
    }

    if req.title.is_some()
        || req.node_type.is_some()
        || req.color.is_some()
        || req.display.is_some()
        || req.lang.is_some()
        || req.interpreter.is_some()
        || req.preview.is_some()
        || req.tags.is_some()
        || req.fold.is_some()
        || req.edge_label.is_some()
    {
        // This also has the side effect of pinning the node's `id=`
        // attribute explicitly the moment any of its metadata changes,
        // same as any other first write-back (see `canvas.rs`'s doc
        // comment on `id`).
        let (display, lang, interpreter) = if final_type == NodeType::File {
            (
                req.display.or(existing_display),
                req.lang.clone().or(existing_lang),
                req.interpreter.clone().or(existing_interpreter),
            )
        } else {
            (None, None, None)
        };
        let preview = if final_type == NodeType::Link {
            Some(req.preview.unwrap_or(existing_preview))
        } else {
            None
        };
        // Unlike `color` (which would happily store and write back a
        // literal `color=""` if sent empty), an empty `edgeLabel` clears
        // the attribute entirely rather than leaving that cruft behind —
        // the client (see `web/src/DeletableEdge.tsx`) always sends this
        // key explicitly (never omitted) whenever the label actually
        // changed, including changing it *to* empty, so there's no
        // "not sent at all" case to conflate this with.
        let edge_label = match &req.edge_label {
            None => existing_edge_label,
            Some(s) if s.trim().is_empty() => None,
            Some(s) => Some(s.clone()),
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
            interpreter,
            preview,
            edge_label,
            fold: resolve_fold_override(req.fold.as_deref(), existing_fold)?,
            tags: req.tags.clone().unwrap_or(existing_tags),
        };
        raw = mdcanvas::set_node_meta(&raw, &local_id, &meta).ok_or_else(not_found)?;
    }

    if let Some(target) = &req.target {
        let body = format!("[{title}]({target})");
        raw = mdcanvas::set_node_body(&raw, &local_id, &body).ok_or_else(not_found)?;
    }

    if let Some(text) = &req.text {
        raw = mdcanvas::set_node_body(&raw, &local_id, text).ok_or_else(not_found)?;
    }

    if let Some(extra_parents) = &extra_parents_local {
        raw = mdcanvas::set_node_edges(&raw, &local_id, extra_parents).ok_or_else(not_found)?;
    }

    // Validate the whole patched document before committing anything —
    // none of the writes above touched `state.raw`/disk (or the include
    // target's) yet.
    parse_or_error(&raw)?;

    commit_located(&state, &located, &raw)?;
    let response_raw = state.raw.lock().unwrap().clone();
    canvas_response(&response_raw, &state.canvas_path)
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct FileContentResponse {
    content: String,
    /// `true` if `content` was cut off at
    /// `meshfox_core::FILE_PREVIEW_MAX_BYTES` — the UI uses this to show a
    /// "truncated" note rather than implying the preview is the whole file.
    truncated: bool,
}

/// Resolves a `file`/`link` node's own link-target string to a real path on
/// disk, relative to the canvas file's own directory, confined to it (same
/// boundary `meshfox_core::include` enforces for include targets): a
/// `../../etc/passwd` or an absolute path pointing outside that tree is
/// rejected rather than read/run/opened, since the target string comes from
/// the (possibly hand-edited) canvas file, not from a trusted source. Shared
/// by every endpoint that touches a file node's target on disk — the
/// `display="code"` preview, running it (a runnable `file` node's
/// `interpreter`), and opening it in the OS's default application. Thin
/// `ApiError`-flavored wrapper around `meshfox_core::file_read::confine`,
/// the one copy of this confinement logic (also used by `staticgen`'s
/// static export and `constraint`'s `.content()`/`.json()`/...).
fn resolve_confined_target(
    canvas_path: &std::path::Path,
    target: &str,
) -> Result<std::path::PathBuf, ApiError> {
    // `Path::parent()` on a bare filename (e.g. `canvas.md`, no directory
    // component) returns `Some("")`, not `None` — so a plain `unwrap_or(".")`
    // never fires and we'd try to canonicalize an empty path, which fails
    // with ENOENT. Treat that empty-parent case as "." too.
    let canvas_dir = canvas_path.parent().filter(|p| !p.as_os_str().is_empty());
    let canvas_dir = canvas_dir.unwrap_or(std::path::Path::new("."));
    meshfox_core::confine(canvas_dir, target).map_err(|e| match e {
        meshfox_core::ConfineError::DirNotFound(_, e) => {
            ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
        }
        meshfox_core::ConfineError::TargetNotFound(p, e) => {
            ApiError(StatusCode::NOT_FOUND, format!("{}: {e}", p.display()))
        }
        meshfox_core::ConfineError::Outside(_) => ApiError(
            StatusCode::FORBIDDEN,
            format!("{target:?} resolves outside the canvas directory"),
        ),
    })
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
        ApiError(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("node {id:?} has no target"),
        )
    })?;

    let canvas_dir = state
        .canvas_path
        .parent()
        .filter(|p| !p.as_os_str().is_empty());
    let canvas_dir = canvas_dir.unwrap_or(std::path::Path::new("."));
    let preview = meshfox_core::preview(canvas_dir, target).map_err(|e| match e {
        meshfox_core::PreviewError::Confine(meshfox_core::ConfineError::DirNotFound(_, e)) => {
            ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
        }
        meshfox_core::PreviewError::Confine(meshfox_core::ConfineError::TargetNotFound(p, e)) => {
            ApiError(StatusCode::NOT_FOUND, format!("{}: {e}", p.display()))
        }
        meshfox_core::PreviewError::Confine(meshfox_core::ConfineError::Outside(_)) => ApiError(
            StatusCode::FORBIDDEN,
            format!("{target:?} resolves outside the canvas directory"),
        ),
        meshfox_core::PreviewError::Read(_, e) => {
            ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
        }
        meshfox_core::PreviewError::Binary => ApiError(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "target looks like a binary file, can't preview it as code".to_string(),
        ),
    })?;

    Ok(Json(FileContentResponse {
        content: preview.content,
        truncated: preview.truncated,
    }))
}

/// Runs a runnable `file` node's `interpreter target` (see
/// `meshfox_core::Node::is_runnable_file`) — the counterpart to `run_block`
/// for a node that has no fenced code of its own to run, just a target file
/// on disk. Streams the same `RunEvent`s `run_block` does (`nodeId`/`block`
/// both set to the node's own id, matching the "sole implicit block shares
/// its node's id" convention `resolve_target` already uses for fenced
/// blocks — see `crate::fence::is_default`), registered in `state.runs` the
/// same way too, so the web UI's existing kill button and live-output
/// handling work unchanged. No `deps=`/`cache`/`env=`/`tty` concepts apply
/// here — a `file` node's body is just a link, nothing to chain, cache, or
/// seize a terminal for.
async fn run_file_node(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    let raw = state.raw.lock().unwrap().clone();
    let canvas = parse_or_error(&raw)?;
    let node = canvas
        .node(&id)
        .ok_or_else(|| ApiError(StatusCode::NOT_FOUND, format!("no node {id:?}")))?;
    if !node.is_runnable_file() {
        return Err(ApiError(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("node {id:?} isn't a runnable file node (needs type=\"file\", a target, and an interpreter)"),
        ));
    }
    let interpreter = node
        .interpreter
        .clone()
        .expect("checked by is_runnable_file");
    let target = node.target.as_deref().expect("checked by is_runnable_file");
    let resolved_path = resolve_confined_target(&state.canvas_path, target)?;

    let run_id = uuid::Uuid::new_v4().to_string();
    let (kill_tx, mut kill_rx) = oneshot::channel::<()>();
    state.runs.lock().unwrap().insert(run_id.clone(), kill_tx);

    let stream = async_stream::stream! {
        let _guard = RunGuard { state: Arc::clone(&state), run_id: run_id.clone() };
        yield Ok::<_, io::Error>(ndjson_line(&RunEvent::Started { run_id: run_id.clone() }));
        yield Ok(ndjson_line(&RunEvent::StepStart { node_id: id.clone(), block: id.clone() }));

        let mut proc = match stream_exec::spawn_process(&interpreter, [resolved_path.as_os_str()]) {
            Ok(p) => p,
            Err(e) => {
                yield Ok(ndjson_line(&RunEvent::Error { message: e.to_string() }));
                return;
            }
        };

        let exit_code = loop {
            tokio::select! {
                line = proc.output_rx.recv() => {
                    match line {
                        Some(text) => {
                            yield Ok(ndjson_line(&RunEvent::Output {
                                node_id: id.clone(),
                                block: id.clone(),
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
                    yield Ok(ndjson_line(&RunEvent::Killed { node_id: id.clone(), block: id.clone() }));
                    return;
                }
            }
        };

        yield Ok(ndjson_line(&RunEvent::StepEnd { node_id: id.clone(), block: id.clone(), exit_code }));
        yield Ok(ndjson_line(&RunEvent::Done { exit_code }));
    };

    Ok(Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/x-ndjson")
        .body(Body::from_stream(stream))
        .unwrap())
}

/// Opens a `file` node's target in the OS's default application for it
/// (`open` on macOS, `xdg-open` on Linux, `start` on Windows — via the
/// `open` crate, same one `meshfox view`'s own `--open` browser-launch
/// already uses) — the web UI's "↗ open" button. Best-effort: spawns the
/// opener and returns as soon as it has (not once whatever it opened has
/// itself finished loading/exited), same as the browser-launch case.
/// `spawn_blocking` because `open::that` shells out synchronously.
async fn open_node_file(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
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
        ApiError(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("node {id:?} has no target"),
        )
    })?;
    let resolved = resolve_confined_target(&state.canvas_path, target)?;

    tokio::task::spawn_blocking(move || open::that(&resolved))
        .await
        .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .map_err(|e| {
            ApiError(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("couldn't open the file: {e}"),
            )
        })?;

    Ok(StatusCode::NO_CONTENT)
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
    let primary_raw = state.raw.lock().unwrap().clone();
    let located = locate_node(&state, &primary_raw, &id)?;
    let local_id = &located.local_id;
    let canvas = parse_or_error(&located.raw)?;
    let node = canvas
        .node(local_id)
        .ok_or_else(|| ApiError(StatusCode::NOT_FOUND, format!("no node {id:?}")))?;
    if node.parent.is_none() {
        return Err(ApiError(
            StatusCode::UNPROCESSABLE_ENTITY,
            "can't delete the root node".to_string(),
        ));
    }
    let reparent = query.children.as_deref() == Some("reparent");
    let updated = if reparent {
        mdcanvas::delete_node_reparent_children(&located.raw, local_id)
    } else {
        mdcanvas::delete_node(&located.raw, local_id)
    }
    .ok_or_else(|| ApiError(StatusCode::NOT_FOUND, format!("no node {id:?}")))?;
    parse_or_error(&updated)?;
    commit_located(&state, &located, &updated)?;
    let response_raw = state.raw.lock().unwrap().clone();
    canvas_response(&response_raw, &state.canvas_path)
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
    let primary_raw = state.raw.lock().unwrap().clone();
    let located = locate_node(&state, &primary_raw, &id)?;
    let located_parent = locate_node(&state, &primary_raw, &req.new_parent_id)?;
    // In practice this can't actually happen — an extra-parent `from=` is
    // only ever resolved against its own document's own id set (see
    // `update_node`'s same check), so `id`'s declared extra parents can
    // never point outside whichever file `id` itself lives in. Checked
    // anyway, defensively, rather than relying on that invariant holding.
    if located.origin != located_parent.origin {
        return Err(ApiError(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!(
                "can't reparent {id:?} onto {:?} — they live in different files \
                 (reparenting across an include boundary isn't supported)",
                req.new_parent_id
            ),
        ));
    }
    let raw = located.raw.clone();
    let local_id = &located.local_id;
    let local_parent_id = &located_parent.local_id;

    let canvas = parse_or_error(&raw)?;
    let node = canvas
        .node(local_id)
        .ok_or_else(|| ApiError(StatusCode::NOT_FOUND, format!("no node {id:?}")))?;
    if node.parent.is_none() {
        return Err(ApiError(
            StatusCode::UNPROCESSABLE_ENTITY,
            "can't reparent the root node".to_string(),
        ));
    }
    canvas.node(local_parent_id).ok_or_else(|| {
        ApiError(
            StatusCode::NOT_FOUND,
            format!("no node {:?}", req.new_parent_id),
        )
    })?;
    if !node
        .extra_parents
        .iter()
        .any(|e| &e.from == local_parent_id)
    {
        return Err(ApiError(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!(
                "{:?} is not one of {id:?}'s extra parents",
                req.new_parent_id
            ),
        ));
    }
    // `mdcanvas::reparent_node` moves `id`'s markdown fragment verbatim,
    // `x`/`y` attribute included — harmless when those are always absolute,
    // but a move into, out of, or between groups now silently flips what
    // those numbers *mean* (see `Canvas::resolve_absolute_position`) unless
    // corrected here. Resolve the pre-move absolute position first, in the
    // *old* parent chain — within `local_id`'s own file is the right frame
    // for this even when it's an include target: any outer group anchor
    // from the including document applies equally before and after a move
    // that (per the check above) never leaves this same file, so it
    // cancels out.
    let abs_before = canvas.resolve_absolute_position(local_id);
    let mut updated =
        mdcanvas::reparent_node(&raw, local_id, local_parent_id).ok_or_else(|| {
            ApiError(
                StatusCode::UNPROCESSABLE_ENTITY,
                format!(
                    "can't reparent {id:?} onto {:?} (would create a cycle)",
                    req.new_parent_id
                ),
            )
        })?;
    let new_canvas = parse_or_error(&updated)?;
    // ...then, once the *new* parent chain is known, convert it back into
    // whatever frame `id` should now store its position in, so it stays
    // visually put across the move instead of teleporting (e.g. jumping by
    // a whole group anchor because its `x`/`y` is now read relative to a
    // *different* group, or no group at all). `None` on either side (an
    // unanchored group ancestor somewhere in the old or new chain — the
    // common case for a group nobody's ever dragged) leaves the node's
    // stored position untouched instead, a documented, bounded limitation
    // rather than inventing a synthetic anchor mid-request.
    if let Some((abs_x, abs_y)) = abs_before {
        if let Some((local_x, local_y)) = new_canvas.absolute_to_local(local_id, abs_x, abs_y) {
            if let Some(new_node) = new_canvas.node(local_id) {
                if new_node.x != Some(local_x) || new_node.y != Some(local_y) {
                    let meta = NodeMeta {
                        x: Some(local_x),
                        y: Some(local_y),
                        width: new_node.width,
                        height: new_node.height,
                        color: new_node.color.clone(),
                        node_type: None,
                        display: new_node.display,
                        lang: new_node.lang.clone(),
                        interpreter: new_node.interpreter.clone(),
                        preview: Some(new_node.preview),
                        edge_label: new_node.edge_label.clone(),
                        fold: new_node.fold,
                        tags: new_node.tags.clone(),
                    };
                    if let Some(patched) = mdcanvas::set_node_meta(&updated, local_id, &meta) {
                        updated = patched;
                    }
                }
            }
        }
    }
    commit_located(&state, &located, &updated)?;
    let response_raw = state.raw.lock().unwrap().clone();
    canvas_response(&response_raw, &state.canvas_path)
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
    let primary_raw = state.raw.lock().unwrap().clone();
    let located = locate_node(&state, &primary_raw, &id)?;
    // `req.newId` is a fresh id being assigned, not a reference to an
    // existing (possibly-namespaced) one — used verbatim as the new local
    // id in whichever file `id` lives in; `include::resolve` re-derives
    // the composed, namespaced form from it next time regardless.
    let updated =
        mdcanvas::rename_node_id(&located.raw, &located.local_id, &req.new_id).map_err(|e| {
            let status = match e {
                mdcanvas::RenameIdError::NotFound(_) => StatusCode::NOT_FOUND,
                mdcanvas::RenameIdError::AlreadyExists(_)
                | mdcanvas::RenameIdError::Empty
                | mdcanvas::RenameIdError::InvalidChar => StatusCode::UNPROCESSABLE_ENTITY,
            };
            ApiError(status, e.to_string())
        })?;
    parse_or_error(&updated)?;
    commit_located(&state, &located, &updated)?;
    let response_raw = state.raw.lock().unwrap().clone();
    canvas_response(&response_raw, &state.canvas_path)
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
    let relevant_decls: Vec<_> = decls
        .iter()
        .filter(|d| needed.contains(&d.name))
        .cloned()
        .collect();
    validate_var_overrides(&relevant_decls, &req.vars)?;
    // `computed` (the empty map here) means every `from`-declared decl in
    // `relevant_decls` lands in `resolved.unresolved_from`, not `missing` —
    // it's not an error yet, just not resolvable until its own source
    // block runs, mid-chain, below.
    let mut resolved_vars = {
        let mut cache = state.vars_cache.lock().unwrap();
        let resolved =
            meshfox_core::resolve_vars(&relevant_decls, &req.vars, &cache, &HashMap::new());
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
            let mut block_env = meshfox_core::map_block_env(&block.env, &resolved_vars);
            // If some declared variable is `from=`-sourced from *this*
            // block, give it a fresh output file to write `NAME=value`
            // lines to (see `meshfox_core::varout`) — read back below,
            // once its exit code is known, and folded into
            // `resolved_vars` for whatever later step needs it.
            let from_decls = meshfox_core::from_targets(&decls, addr);
            let vars_out_path = if from_decls.is_empty() {
                None
            } else {
                let path = meshfox_core::allocate_vars_out_path();
                block_env.insert(
                    meshfox_core::VARS_OUT_ENV.to_string(),
                    path.display().to_string(),
                );
                Some(path)
            };
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

            // Read back whatever this step wrote to its own vars-out file
            // (if it had one) and fold the type-validated values straight
            // into `resolved_vars`, so a later step's `map_block_env` call
            // sees them. Only trusted on a `0` exit.
            let mut from_value_error = false;
            if let Some(path) = &vars_out_path {
                match meshfox_core::read_and_cleanup_vars_out(path) {
                    Ok(produced) if exit_code == 0 => {
                        for decl in &from_decls {
                            match produced.get(&decl.name) {
                                Some(value) => match meshfox_core::validate_value(decl, value) {
                                    Ok(()) => {
                                        resolved_vars.insert(decl.name.clone(), value.clone());
                                    }
                                    Err(e) => {
                                        yield Ok(ndjson_line(&RunEvent::Error {
                                            message: format!(
                                                "computed variable {:?} is invalid: {e}",
                                                decl.name
                                            ),
                                        }));
                                        from_value_error = true;
                                    }
                                },
                                None => {
                                    yield Ok(ndjson_line(&RunEvent::Error {
                                        message: format!(
                                            "block {:?} produced no value for {:?} (declared from=\"{}/{}\")",
                                            addr.block_name, decl.name, addr.node_id, addr.block_name
                                        ),
                                    }));
                                    from_value_error = true;
                                }
                            }
                        }
                    }
                    Ok(_) => {} // nonzero exit — handled by the check below
                    Err(e) => {
                        yield Ok(ndjson_line(&RunEvent::Error {
                            message: format!("failed to read computed variables: {e}"),
                        }));
                        from_value_error = true;
                    }
                }
            }

            if persist && block.cache {
                let result = ExecOutput { exit_code, output: full_output };
                if let Some(updated) = meshfox_core::write_output(&node_text, &addr.block_name, &result) {
                    if let Some(patched) = mdcanvas::set_node_body(&raw, &addr.node_id, &updated) {
                        raw = patched;
                    }
                }
            }

            if exit_code != 0 || from_value_error {
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
fn find_tty_block(
    canvas: &Canvas,
    chain: &[meshfox_core::BlockAddr],
) -> Option<meshfox_core::BlockAddr> {
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
    let path: Vec<&str> = if query.path.is_empty() {
        Vec::new()
    } else {
        query.path.split(',').collect()
    };
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
    let relevant_decls: Vec<_> = decls
        .iter()
        .filter(|d| needed.contains(&d.name))
        .cloned()
        .collect();
    validate_var_overrides(&relevant_decls, &requested_vars)?;
    // `computed` (the empty map here) means a `from`-declared decl in
    // `relevant_decls` lands in `unresolved_from`, not `missing` — it's
    // resolved incrementally, mid-chain, by `run_tty_chain` instead.
    let resolved_vars = {
        let mut cache = state.vars_cache.lock().unwrap();
        let resolved = meshfox_core::resolve_vars(
            &relevant_decls,
            &requested_vars,
            &cache,
            &HashMap::new(),
        );
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
        run_tty_chain(
            socket,
            state,
            run_id,
            chain,
            decls,
            resolved_vars,
            persist,
            cols,
            rows,
            kill_rx,
        )
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
    decls: Vec<meshfox_core::VarDecl>,
    mut resolved_vars: HashMap<String, String>,
    persist: bool,
    cols: u16,
    rows: u16,
    mut kill_rx: oneshot::Receiver<()>,
) {
    let _guard = RunGuard {
        state: Arc::clone(&state),
        run_id: run_id.clone(),
    };

    if !send_event(
        &mut socket,
        &RunEvent::Started {
            run_id: run_id.clone(),
        },
    )
    .await
    {
        return;
    }

    let mut raw = state.raw.lock().unwrap().clone();
    let mut final_exit_code = 0;
    let mut killed = false;

    for addr in &chain {
        if !send_event(
            &mut socket,
            &RunEvent::StepStart {
                node_id: addr.node_id.clone(),
                block: addr.block_name.clone(),
            },
        )
        .await
        {
            return;
        }

        // Re-parse so an earlier step's freshly-patched cache is visible
        // before this one runs — same reasoning `run_block` already has.
        let node_text = match Canvas::from_markdown(&raw)
            .ok()
            .and_then(|c| c.node(&addr.node_id).map(|n| n.text.clone()))
        {
            Some(text) => text,
            None => {
                send_event(
                    &mut socket,
                    &RunEvent::Error {
                        message: format!("node {:?} not found", addr.node_id),
                    },
                )
                .await;
                break;
            }
        };
        let Some(block) = meshfox_core::scan_runnable_blocks(&addr.node_id, &node_text)
            .into_iter()
            .find(|b| b.name.as_deref() == Some(addr.block_name.as_str()))
        else {
            send_event(
                &mut socket,
                &RunEvent::Error {
                    message: format!(
                        "no runnable block named {:?} in node {:?}",
                        addr.block_name, addr.node_id
                    ),
                },
            )
            .await;
            break;
        };

        let mut block_env = meshfox_core::map_block_env(&block.env, &resolved_vars);
        // If some declared variable is `from=`-sourced from *this* block
        // (tty or not), give it a fresh output file to write `NAME=value`
        // lines to (see `meshfox_core::varout`) — read back below, once
        // its exit code is known.
        let from_decls = meshfox_core::from_targets(&decls, addr);
        let vars_out_path = if from_decls.is_empty() {
            None
        } else {
            let path = meshfox_core::allocate_vars_out_path();
            block_env.insert(
                meshfox_core::VARS_OUT_ENV.to_string(),
                path.display().to_string(),
            );
            Some(path)
        };
        let mut full_output = String::new();

        let exit_code = if block.tty {
            if !send_event(
                &mut socket,
                &RunEvent::TtyStart {
                    node_id: addr.node_id.clone(),
                    block: addr.block_name.clone(),
                },
            )
            .await
            {
                return;
            }
            match relay_tty_step(
                &mut socket,
                &block.code,
                &block_env,
                cols,
                rows,
                &mut kill_rx,
            )
            .await
            {
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
                    send_event(
                        &mut socket,
                        &RunEvent::Error {
                            message: e.to_string(),
                        },
                    )
                    .await;
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
            send_event(
                &mut socket,
                &RunEvent::Killed {
                    node_id: addr.node_id.clone(),
                    block: addr.block_name.clone(),
                },
            )
            .await;
            break;
        }

        if !send_event(
            &mut socket,
            &RunEvent::StepEnd {
                node_id: addr.node_id.clone(),
                block: addr.block_name.clone(),
                exit_code,
            },
        )
        .await
        {
            return;
        }
        final_exit_code = exit_code;

        // Read back whatever this step wrote to its own vars-out file (if
        // it had one) and fold the type-validated values straight into
        // `resolved_vars`, so a later step's `map_block_env` call sees
        // them. Only trusted on a `0` exit.
        let mut from_value_error = false;
        if let Some(path) = &vars_out_path {
            match meshfox_core::read_and_cleanup_vars_out(path) {
                Ok(produced) if exit_code == 0 => {
                    for decl in &from_decls {
                        match produced.get(&decl.name) {
                            Some(value) => match meshfox_core::validate_value(decl, value) {
                                Ok(()) => {
                                    resolved_vars.insert(decl.name.clone(), value.clone());
                                }
                                Err(e) => {
                                    send_event(
                                        &mut socket,
                                        &RunEvent::Error {
                                            message: format!(
                                                "computed variable {:?} is invalid: {e}",
                                                decl.name
                                            ),
                                        },
                                    )
                                    .await;
                                    from_value_error = true;
                                }
                            },
                            None => {
                                send_event(
                                    &mut socket,
                                    &RunEvent::Error {
                                        message: format!(
                                            "block {:?} produced no value for {:?} (declared from=\"{}/{}\")",
                                            addr.block_name, decl.name, addr.node_id, addr.block_name
                                        ),
                                    },
                                )
                                .await;
                                from_value_error = true;
                            }
                        }
                    }
                }
                Ok(_) => {} // nonzero exit — handled by the check below
                Err(e) => {
                    send_event(
                        &mut socket,
                        &RunEvent::Error {
                            message: format!("failed to read computed variables: {e}"),
                        },
                    )
                    .await;
                    from_value_error = true;
                }
            }
        }

        // `tty` and `cache` are mutually exclusive (a `meshfox validate`
        // error) — `!block.tty` here is belt-and-suspenders against ever
        // writing a `tty` step's (empty) `full_output` into the file for a
        // document that reached this endpoint without being validated.
        if persist && block.cache && !block.tty {
            let result = ExecOutput {
                exit_code,
                output: full_output,
            };
            if let Some(updated) = meshfox_core::write_output(&node_text, &addr.block_name, &result)
            {
                if let Some(patched) = mdcanvas::set_node_body(&raw, &addr.node_id, &updated) {
                    raw = patched;
                }
            }
        }

        if exit_code != 0 || from_value_error {
            break;
        }
    }

    if persist {
        match state.save(&raw) {
            Ok(()) => *state.raw.lock().unwrap() = raw,
            Err(e) => {
                send_event(
                    &mut socket,
                    &RunEvent::Error {
                        message: e.to_string(),
                    },
                )
                .await;
            }
        }
    }

    if !killed {
        send_event(
            &mut socket,
            &RunEvent::Done {
                exit_code: final_exit_code,
            },
        )
        .await;
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
            send_event(
                socket,
                &RunEvent::Error {
                    message: e.to_string(),
                },
            )
            .await;
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

#[derive(Deserialize)]
struct IncludeAssetQuery {
    /// A directory reported as some resolved node's `asset_base` (see
    /// `meshfox_core::canvas::Node::asset_base`) — i.e. an `include`
    /// target's own directory, which may sit anywhere on disk, not just
    /// under the canvas file's directory. Re-checked against a fresh
    /// resolve of the current document below rather than trusted outright,
    /// so this can't be used to read arbitrary files off disk merely by
    /// naming their directory in the query string.
    dir: String,
    /// Path of the actual asset, relative to `dir`.
    file: String,
}

/// Backs a relative `![](...)` image (or link) inside an `include`d node's
/// body — see `Node::asset_base`. `serve_canvas_relative_file` below only
/// ever resolves against the *main* canvas file's own directory, which is
/// wrong once a node's content was spliced in from a different directory
/// (see `meshfox_core::include::resolve`); this is that directory's
/// counterpart. `dir` is only honored if it's still one of the current
/// document's actual resolved `asset_base`s — re-derived fresh from the
/// on-disk file on every request, same as every other read here, so a
/// stale or hand-crafted `dir` (one this document doesn't currently
/// include) 404s instead of serving whatever happens to live there.
async fn get_include_asset(
    State(state): State<Arc<AppState>>,
    Query(q): Query<IncludeAssetQuery>,
) -> Response {
    let raw = state.raw.lock().unwrap().clone();
    let Ok(canvas) = resolved_canvas(&raw, &state.canvas_path) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let requested_dir = PathBuf::from(&q.dir);
    let known = canvas
        .nodes
        .iter()
        .filter_map(|n| n.asset_base.as_deref())
        .any(|base| PathBuf::from(base) == requested_dir);
    if !known {
        return StatusCode::NOT_FOUND.into_response();
    }

    let Ok(dir) = requested_dir.canonicalize() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let candidate = dir.join(&q.file);
    let Ok(resolved) = candidate.canonicalize() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if !resolved.starts_with(&dir) || !resolved.is_file() {
        return StatusCode::NOT_FOUND.into_response();
    }

    let Ok(bytes) = std::fs::read(&resolved) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let mime = mime_guess::from_path(&resolved).first_or_octet_stream();
    ([(header::CONTENT_TYPE, mime.as_ref().to_string())], bytes).into_response()
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
    Some(([(header::CONTENT_TYPE, mime.as_ref().to_string())], bytes).into_response())
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

#[derive(Debug, Deserialize)]
struct LinkPreviewQuery {
    url: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct LinkPreviewResponse {
    /// `None` means no preview is available — the fetch was blocked (SSRF
    /// check), failed, or the target isn't HTML. Deliberately not
    /// distinguished any further than that in the response (see
    /// `link_preview`'s own module doc): this endpoint takes an
    /// attacker-controllable URL from the canvas, so it must not double as
    /// a network probe an attacker could use to learn *why* a given
    /// internal address failed.
    preview: Option<link_preview::PreviewMeta>,
}

/// `GET /api/link-preview?url=<url>` — fetches (or returns the
/// already-cached) OpenGraph preview for a `link` node's target. Doesn't
/// require `url` to belong to any node in the current document; the web
/// UI only ever calls this for a node whose own `preview` attribute is on,
/// but the endpoint itself just takes a URL, same trust boundary as the
/// node attribute itself (both are only ever attacker-controlled canvas
/// content, never a secret).
async fn get_link_preview(
    State(state): State<Arc<AppState>>,
    Query(query): Query<LinkPreviewQuery>,
) -> Json<LinkPreviewResponse> {
    let preview = state.link_preview_cache.get_or_fetch(&query.url).await;
    Json(LinkPreviewResponse { preview })
}

/// Serves `canvas_path` on `127.0.0.1:<port>` until the process is killed
/// (or, when `auto_exit` is on, until every `/api/watch`-connected tab has
/// closed — see `TabGuard`). `port` of `0` asks the OS to assign a free
/// port instead — the actual bound port is read back from the listener
/// below.
async fn build_state(canvas_path: PathBuf, auto_exit: bool) -> std::io::Result<Arc<AppState>> {
    let raw = std::fs::read_to_string(&canvas_path)?;
    if let Err(e) = Canvas::from_markdown(&raw) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            e.to_string(),
        ));
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
        link_preview_cache: link_preview::PreviewCache::new(),
    }))
}

fn build_app(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/api/canvas", get(get_canvas).put(put_canvas))
        .route("/api/canvas/raw", get(get_canvas_raw).put(put_canvas_raw))
        .route("/api/includes", get(get_includes))
        .route("/api/canvas/clear-layout", post(clear_layout))
        .route("/api/nodes", post(create_node))
        .route("/api/nodes/:id", patch(update_node).delete(remove_node))
        .route("/api/nodes/:id/reparent", post(reparent_node))
        .route("/api/nodes/:id/rename-id", post(rename_node_id))
        .route("/api/nodes/:id/file-content", get(get_node_file_content))
        .route("/api/nodes/:id/run", post(run_file_node))
        .route("/api/nodes/:id/open", post(open_node_file))
        .route("/api/options", put(put_options))
        .route("/api/vars", get(get_vars))
        .route(
            "/api/vars/configure",
            get(get_configure_vars).post(post_configure_vars),
        )
        .route("/api/link-preview", get(get_link_preview))
        .route("/api/run", post(run_block))
        .route("/api/run/tty", get(run_block_tty))
        .route("/api/kill", post(kill_run))
        .route("/api/watch", get(watch_changes))
        .route("/api/include-asset", get(get_include_asset))
        .fallback(serve_embedded)
        .with_state(state)
        .layer(CorsLayer::permissive())
}

/// Serves `canvas_path` on `127.0.0.1:<port>` until the process is killed
/// (or, when `auto_exit` is on, until every `/api/watch`-connected tab has
/// closed — see `TabGuard`). `port` of `0` asks the OS to assign a free
/// port instead — the actual bound port is read back from the listener
/// below.
pub async fn run(
    canvas_path: PathBuf,
    port: u16,
    open_browser: bool,
    auto_exit: bool,
) -> std::io::Result<()> {
    let state = build_state(canvas_path.clone(), auto_exit).await?;
    spawn_file_watcher(Arc::clone(&state));
    let app = build_app(state);

    let listener = tokio::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], port))).await?;
    let addr = listener.local_addr()?;
    println!(
        "meshfox: serving {} on http://{addr}",
        canvas_path.display()
    );

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
    let state = build_state(canvas_path, false)
        .await
        .expect("valid test canvas");
    let app = build_app(state);
    let listener = tokio::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .expect("bind");
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
        path.push(format!(
            "meshfox-clear-layout-test-{}.canvas.md",
            uuid::Uuid::new_v4()
        ));
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
        let canvas_path = write_test_canvas(
            &CANVAS.replace("./other/README.md", &target_path.display().to_string()),
        );
        let state = build_state(canvas_path.clone(), false)
            .await
            .expect("valid test canvas");

        let Json(cleared) = match clear_layout(State(state)).await {
            Ok(json) => json,
            Err(e) => panic!("clear-layout failed: {}", e.1),
        };
        let readme = cleared.node("readme").expect("readme node still present");

        assert_eq!(
            readme.x, None,
            "include node's own authored x should be cleared"
        );
        assert_eq!(
            readme.y, None,
            "include node's own authored y should be cleared"
        );

        let _ = std::fs::remove_file(&canvas_path);
        let _ = std::fs::remove_file(&target_path);
    }
}

/// `reparent_node`'s position-conversion behavior (see its own doc
/// comment) — a group member's real `x`/`y` is relative to its group's own
/// anchor, so moving a node into/out of/between groups has to convert its
/// stored position, or it'd silently teleport (or land relative to the
/// wrong frame) the instant it moves.
#[cfg(test)]
mod reparent_position_tests {
    use super::*;

    fn write_test_canvas(contents: &str) -> PathBuf {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "meshfox-reparent-position-test-{}.canvas.md",
            uuid::Uuid::new_v4()
        ));
        std::fs::write(&path, contents).unwrap();
        path
    }

    fn expect_ok(result: Result<Json<Canvas>, ApiError>) -> Canvas {
        match result {
            Ok(Json(canvas)) => canvas,
            Err(e) => panic!("request failed: {}", e.1),
        }
    }

    #[tokio::test]
    async fn reparenting_into_a_group_converts_the_position_to_be_relative_to_it() {
        // `wanderer` sits at absolute (1050, 1030) today, a plain top-level
        // sibling of `frame` — visually just inside where frame's own
        // anchor (1000, 1000) would place its box. Moving it in should
        // rewrite its stored position to (50, 30): the same visual spot,
        // now expressed relative to frame's own anchor.
        const CANVAS: &str = concat!(
            "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
            "## Frame\n<!-- meshfox:node id=\"frame\" type=\"group\" x=1000 y=1000 -->\n\n",
            "### Existing Member\n<!-- meshfox:node id=\"existing-member\" x=10 y=10 w=100 h=60 -->\n\nbody\n\n",
            "## Wanderer\n<!-- meshfox:node id=\"wanderer\" x=1050 y=1030 w=100 h=60 -->\n",
            "<!-- meshfox:edge from=\"frame\" -->\n\nbody\n",
        );
        let canvas_path = write_test_canvas(CANVAS);
        let state = build_state(canvas_path.clone(), false)
            .await
            .expect("valid test canvas");

        let updated = expect_ok(
            reparent_node(
                State(state),
                Path("wanderer".to_string()),
                Json(ReparentNodeRequest {
                    new_parent_id: "frame".to_string(),
                }),
            )
            .await,
        );
        let wanderer = updated.node("wanderer").expect("wanderer still present");

        assert_eq!(wanderer.parent.as_deref(), Some("frame"));
        assert_eq!(wanderer.x, Some(50.0));
        assert_eq!(wanderer.y, Some(30.0));

        let _ = std::fs::remove_file(&canvas_path);
    }

    #[tokio::test]
    async fn reparenting_out_of_a_group_converts_the_position_back_to_absolute() {
        // `member` sits at (50, 30) relative to `frame`'s own (1000, 1000)
        // anchor today — absolute (1050, 1030). Moving it to root (not a
        // group) should rewrite its stored position to that absolute
        // value, the same visual spot outside any group frame.
        const CANVAS: &str = concat!(
            "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
            "## Frame\n<!-- meshfox:node id=\"frame\" type=\"group\" x=1000 y=1000 -->\n\n",
            "### Member\n<!-- meshfox:node id=\"member\" x=50 y=30 w=100 h=60 -->\n",
            "<!-- meshfox:edge from=\"root\" -->\n\nbody\n\n",
            "## Elsewhere\n<!-- meshfox:node id=\"elsewhere\" -->\n\nbody\n",
        );
        let canvas_path = write_test_canvas(CANVAS);
        let state = build_state(canvas_path.clone(), false)
            .await
            .expect("valid test canvas");

        let updated = expect_ok(
            reparent_node(
                State(state),
                Path("member".to_string()),
                Json(ReparentNodeRequest {
                    new_parent_id: "root".to_string(),
                }),
            )
            .await,
        );
        let member = updated.node("member").expect("member still present");

        assert_eq!(member.parent.as_deref(), Some("root"));
        assert_eq!(member.x, Some(1050.0));
        assert_eq!(member.y, Some(1030.0));

        let _ = std::fs::remove_file(&canvas_path);
    }

    #[tokio::test]
    async fn reparenting_into_an_unanchored_group_leaves_the_position_untouched() {
        // `frame` here has no anchor of its own (never dragged) — there's
        // no frame to be relative *to*, so this is the documented,
        // bounded fallback: `wanderer`'s stored position is left exactly
        // as it was rather than inventing a synthetic anchor mid-request.
        const CANVAS: &str = concat!(
            "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
            "## Frame\n<!-- meshfox:node id=\"frame\" type=\"group\" -->\n\n",
            "### Existing Member\n<!-- meshfox:node id=\"existing-member\" -->\n\nbody\n\n",
            "## Wanderer\n<!-- meshfox:node id=\"wanderer\" x=50 y=30 w=100 h=60 -->\n",
            "<!-- meshfox:edge from=\"frame\" -->\n\nbody\n",
        );
        let canvas_path = write_test_canvas(CANVAS);
        let state = build_state(canvas_path.clone(), false)
            .await
            .expect("valid test canvas");

        let updated = expect_ok(
            reparent_node(
                State(state),
                Path("wanderer".to_string()),
                Json(ReparentNodeRequest {
                    new_parent_id: "frame".to_string(),
                }),
            )
            .await,
        );
        let wanderer = updated.node("wanderer").expect("wanderer still present");

        assert_eq!(wanderer.parent.as_deref(), Some("frame"));
        assert_eq!(wanderer.x, Some(50.0));
        assert_eq!(wanderer.y, Some(30.0));

        let _ = std::fs::remove_file(&canvas_path);
    }
}

#[cfg(test)]
mod include_edit_tests {
    use super::*;

    fn blank_update_request() -> UpdateNodeRequest {
        UpdateNodeRequest {
            title: None,
            node_type: None,
            color: None,
            target: None,
            text: None,
            extra_parents: None,
            display: None,
            lang: None,
            interpreter: None,
            preview: None,
            tags: None,
            edge_label: None,
            fold: None,
        }
    }

    fn expect_ok(result: Result<Json<Canvas>, ApiError>) -> Canvas {
        match result {
            Ok(Json(canvas)) => canvas,
            Err(e) => panic!("request failed: {}", e.1),
        }
    }

    fn expect_err(result: Result<Json<Canvas>, ApiError>) -> ApiError {
        match result {
            Ok(_) => panic!("expected an error"),
            Err(e) => e,
        }
    }

    /// A primary `base.canvas.md` including `child.canvas.md` (namespaced
    /// `child/root`/`child/leaf` once resolved), in a fresh temp dir shared
    /// by both files — returns the primary document's own path.
    fn write_base_and_child_canvas() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "meshfox-include-edit-test-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("child.canvas.md"),
            concat!(
                "<!-- meshfox:canvas -->\n# Child\n<!-- meshfox:node id=\"root\" -->\n\nintro\n\n",
                "## Leaf\n<!-- meshfox:node id=\"leaf\" -->\n\nleaf body\n",
            ),
        )
        .unwrap();
        let base_path = dir.join("base.canvas.md");
        std::fs::write(
            &base_path,
            concat!(
                "<!-- meshfox:canvas -->\n# Base\n<!-- meshfox:node id=\"base\" -->\n\n",
                "## Child\n<!-- meshfox:node id=\"child\" type=\"include\" -->\n\n[child](./child.canvas.md)\n",
            ),
        )
        .unwrap();
        base_path
    }

    #[tokio::test]
    async fn update_node_edits_an_included_nodes_own_file_not_the_primary_one() {
        let base_path = write_base_and_child_canvas();
        let child_path = base_path.parent().unwrap().join("child.canvas.md");
        let base_before = std::fs::read_to_string(&base_path).unwrap();
        let state = build_state(base_path.clone(), false)
            .await
            .expect("valid test canvas");

        let mut req = blank_update_request();
        req.text = Some("new leaf body".to_string());
        let updated =
            expect_ok(update_node(State(state), Path("child/leaf".to_string()), Json(req)).await);

        let leaf = updated
            .node("child/leaf")
            .expect("child/leaf still present");
        assert_eq!(leaf.text, "new leaf body");
        // The primary document itself is untouched — the edit landed in
        // `child.canvas.md`, addressed there by its own local id `leaf`.
        assert_eq!(std::fs::read_to_string(&base_path).unwrap(), base_before);
        assert!(std::fs::read_to_string(&child_path)
            .unwrap()
            .contains("new leaf body"));

        let _ = std::fs::remove_dir_all(base_path.parent().unwrap());
    }

    #[tokio::test]
    async fn create_node_under_an_included_parent_writes_into_the_included_file() {
        let base_path = write_base_and_child_canvas();
        let child_path = base_path.parent().unwrap().join("child.canvas.md");
        let state = build_state(base_path.clone(), false)
            .await
            .expect("valid test canvas");

        let updated = expect_ok(
            create_node(
                State(state),
                Json(CreateNodeRequest {
                    parent_id: "child/root".to_string(),
                    title: "New Kid".to_string(),
                }),
            )
            .await,
        );

        let new_node = updated
            .nodes
            .iter()
            .find(|n| n.title == "New Kid")
            .expect("new node present in the response");
        assert_eq!(new_node.parent.as_deref(), Some("child/root"));
        assert!(std::fs::read_to_string(&child_path)
            .unwrap()
            .contains("New Kid"));

        let _ = std::fs::remove_dir_all(base_path.parent().unwrap());
    }

    #[tokio::test]
    async fn remove_node_deletes_from_the_included_file_and_leaves_the_primary_document_alone() {
        let base_path = write_base_and_child_canvas();
        let child_path = base_path.parent().unwrap().join("child.canvas.md");
        let base_before = std::fs::read_to_string(&base_path).unwrap();
        let state = build_state(base_path.clone(), false)
            .await
            .expect("valid test canvas");

        let updated = expect_ok(
            remove_node(
                State(state),
                Path("child/leaf".to_string()),
                Query(DeleteNodeQuery { children: None }),
            )
            .await,
        );

        assert!(updated.node("child/leaf").is_none());
        assert_eq!(std::fs::read_to_string(&base_path).unwrap(), base_before);
        assert!(!std::fs::read_to_string(&child_path)
            .unwrap()
            .contains("Leaf"));

        let _ = std::fs::remove_dir_all(base_path.parent().unwrap());
    }

    #[tokio::test]
    async fn rename_node_id_renames_within_the_included_file() {
        let base_path = write_base_and_child_canvas();
        let state = build_state(base_path.clone(), false)
            .await
            .expect("valid test canvas");

        let updated = expect_ok(
            rename_node_id(
                State(state),
                Path("child/leaf".to_string()),
                Json(RenameNodeIdRequest {
                    new_id: "renamed-leaf".to_string(),
                }),
            )
            .await,
        );

        assert!(updated.node("child/leaf").is_none());
        let renamed = updated
            .node("child/renamed-leaf")
            .expect("renamed node present under its new namespaced id");
        assert_eq!(renamed.title, "Leaf");

        let _ = std::fs::remove_dir_all(base_path.parent().unwrap());
    }

    #[tokio::test]
    async fn update_node_on_a_plain_markdown_include_rejects_a_text_edit_with_a_clear_reason() {
        let dir = std::env::temp_dir().join(format!(
            "meshfox-include-edit-test-md-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("notes.md"), "# Notes\n\nsome prose\n").unwrap();
        let base_path = dir.join("base.canvas.md");
        std::fs::write(
            &base_path,
            concat!(
                "<!-- meshfox:canvas -->\n# Base\n<!-- meshfox:node id=\"base\" -->\n\n",
                "## Notes\n<!-- meshfox:node id=\"notes\" type=\"include\" -->\n\n[notes](./notes.md)\n",
            ),
        )
        .unwrap();
        let base_before = std::fs::read_to_string(&base_path).unwrap();
        let state = build_state(base_path.clone(), false)
            .await
            .expect("valid test canvas");

        let mut req = blank_update_request();
        req.text = Some("clobbered".to_string());
        let err = expect_err(update_node(State(state), Path("notes".to_string()), Json(req)).await);
        assert_eq!(err.0, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(
            err.1.contains("include target file"),
            "unexpected message: {}",
            err.1
        );
        // Nothing was written — the link is still there, not clobbered.
        assert_eq!(std::fs::read_to_string(&base_path).unwrap(), base_before);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn update_node_on_an_unknown_id_still_404s() {
        let base_path = write_base_and_child_canvas();
        let state = build_state(base_path.clone(), false)
            .await
            .expect("valid test canvas");

        let err = expect_err(
            update_node(
                State(state),
                Path("nope".to_string()),
                Json(blank_update_request()),
            )
            .await,
        );
        assert_eq!(err.0, StatusCode::NOT_FOUND);

        let _ = std::fs::remove_dir_all(base_path.parent().unwrap());
    }

    #[tokio::test]
    async fn put_canvas_persists_a_dragged_included_nodes_position_into_its_own_file() {
        let base_path = write_base_and_child_canvas();
        let child_path = base_path.parent().unwrap().join("child.canvas.md");
        let base_before = std::fs::read_to_string(&base_path).unwrap();
        let state = build_state(base_path.clone(), false)
            .await
            .expect("valid test canvas");

        let primary_raw = state.raw.lock().unwrap().clone();
        let mut canvas = resolved_canvas(&primary_raw, &state.canvas_path)
            .unwrap_or_else(|e| panic!("resolve failed: {}", e.1));
        canvas.node_mut("child/leaf").expect("child/leaf present").x = Some(123.0);
        canvas.node_mut("child/leaf").expect("child/leaf present").y = Some(456.0);

        let status = put_canvas(State(state), Json(canvas))
            .await
            .unwrap_or_else(|e| panic!("put_canvas failed: {}", e.1));
        assert_eq!(status, StatusCode::NO_CONTENT);

        // The primary document is untouched — the position landed in
        // `child.canvas.md`, addressed there by its own local id `leaf`.
        assert_eq!(std::fs::read_to_string(&base_path).unwrap(), base_before);
        let child_after = std::fs::read_to_string(&child_path).unwrap();
        assert!(
            child_after.contains("id=\"leaf\" x=123 y=456"),
            "child file: {child_after}"
        );

        let _ = std::fs::remove_dir_all(base_path.parent().unwrap());
    }

    #[tokio::test]
    async fn get_includes_lists_the_child_canvas_include() {
        let base_path = write_base_and_child_canvas();
        let state = build_state(base_path.clone(), false)
            .await
            .expect("valid test canvas");

        let Json(entries) = get_includes(State(state))
            .await
            .unwrap_or_else(|e| panic!("failed: {}", e.1));

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].node_id, "child");
        assert_eq!(entries[0].title, "Child");
        assert_eq!(entries[0].target, "./child.canvas.md");
        assert_eq!(entries[0].depth, 0);
        assert!(entries[0].is_canvas);

        let _ = std::fs::remove_dir_all(base_path.parent().unwrap());
    }

    #[tokio::test]
    async fn source_mode_reads_and_writes_an_included_files_own_raw_text() {
        let base_path = write_base_and_child_canvas();
        let child_path = base_path.parent().unwrap().join("child.canvas.md");
        let base_before = std::fs::read_to_string(&base_path).unwrap();
        let state = build_state(base_path.clone(), false)
            .await
            .expect("valid test canvas");

        let source = get_canvas_raw(
            State(state.clone()),
            Query(SourceFileQuery {
                include: Some("child".to_string()),
            }),
        )
        .await
        .unwrap_or_else(|e| panic!("get failed: {}", e.1));
        assert_eq!(source, std::fs::read_to_string(&child_path).unwrap());

        let new_source =
            format!("{source}\n## Extra\n<!-- meshfox:node id=\"extra\" -->\n\nmore\n");
        let status = put_canvas_raw(
            State(state),
            Query(SourceFileQuery {
                include: Some("child".to_string()),
            }),
            new_source.clone(),
        )
        .await
        .unwrap_or_else(|e| panic!("put failed: {}", e.1));
        assert_eq!(status, StatusCode::NO_CONTENT);

        assert_eq!(std::fs::read_to_string(&child_path).unwrap(), new_source);
        // The primary document was never touched by any of this.
        assert_eq!(std::fs::read_to_string(&base_path).unwrap(), base_before);

        let _ = std::fs::remove_dir_all(base_path.parent().unwrap());
    }

    #[tokio::test]
    async fn source_mode_rejects_an_unknown_include_id() {
        let base_path = write_base_and_child_canvas();
        let state = build_state(base_path.clone(), false)
            .await
            .expect("valid test canvas");

        let err = match get_canvas_raw(
            State(state),
            Query(SourceFileQuery {
                include: Some("nope".to_string()),
            }),
        )
        .await
        {
            Ok(_) => panic!("expected an error"),
            Err(e) => e,
        };
        assert_eq!(err.0, StatusCode::NOT_FOUND);

        let _ = std::fs::remove_dir_all(base_path.parent().unwrap());
    }

    #[tokio::test]
    async fn source_mode_on_a_plain_markdown_include_skips_canvas_validation() {
        let dir = std::env::temp_dir().join(format!(
            "meshfox-include-edit-test-md-source-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("notes.md"), "# Notes\n\nsome prose\n").unwrap();
        let base_path = dir.join("base.canvas.md");
        std::fs::write(
            &base_path,
            concat!(
                "<!-- meshfox:canvas -->\n# Base\n<!-- meshfox:node id=\"base\" -->\n\n",
                "## Notes\n<!-- meshfox:node id=\"notes\" type=\"include\" -->\n\n[notes](./notes.md)\n",
            ),
        )
        .unwrap();
        let state = build_state(base_path.clone(), false)
            .await
            .expect("valid test canvas");

        // Ordinary prose with no single H1 root and no `meshfox:node`
        // structure at all — would fail `parse_or_error` as a canvas, and
        // must not be asked to pass as one, since a plain-Markdown include
        // target never has to be canvas-shaped in the first place.
        let new_prose = "Just some words.\n\nNo heading here at all.\n";
        let status = put_canvas_raw(
            State(state),
            Query(SourceFileQuery {
                include: Some("notes".to_string()),
            }),
            new_prose.to_string(),
        )
        .await
        .unwrap_or_else(|e| panic!("put failed: {}", e.1));
        assert_eq!(status, StatusCode::NO_CONTENT);
        assert_eq!(
            std::fs::read_to_string(dir.join("notes.md")).unwrap(),
            new_prose
        );

        let _ = std::fs::remove_dir_all(&dir);
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
        path.push(format!(
            "meshfox-tty-test-{}.canvas.md",
            uuid::Uuid::new_v4()
        ));
        std::fs::write(&path, contents).unwrap();
        path
    }

    /// Reads WebSocket frames until the next `RunEvent` text frame (JSON),
    /// silently skipping any binary (raw pty output) frames in between —
    /// what every assertion below actually cares about.
    async fn next_event(ws: &mut TestSocket) -> serde_json::Value {
        loop {
            match ws.next().await.expect("socket open").expect("no ws error") {
                WsMessage::Text(t) => {
                    return serde_json::from_str(&t).expect("valid RunEvent JSON")
                }
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
                WsMessage::Text(t) => {
                    panic!("unexpected RunEvent while waiting for pty output: {t}")
                }
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
        let (mut ws, _) = tokio_tungstenite::connect_async(url)
            .await
            .expect("connect");

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

        ws.send(WsMessage::Binary(b"hello\n".to_vec().into()))
            .await
            .expect("send input");
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
        let (mut ws, _) = tokio_tungstenite::connect_async(url)
            .await
            .expect("connect");

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
        status_line
            .split_whitespace()
            .nth(1)
            .expect("status code")
            .parse()
            .expect("numeric status")
    }
}

#[cfg(test)]
mod run_file_tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpStream;

    fn write_test_canvas(contents: &str) -> PathBuf {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "meshfox-run-file-test-{}.canvas.md",
            uuid::Uuid::new_v4()
        ));
        std::fs::write(&path, contents).unwrap();
        path
    }

    /// Posts an empty-body request to `path` and returns `(status, body)` —
    /// same raw-`TcpStream` approach `ws_tests::reqwest_free_kill` uses.
    /// `run_file_node`'s response streams as chunked transfer-encoding
    /// (its body size isn't known upfront), so this de-chunks it before
    /// handing the body back — a plain (non-streamed) error response is
    /// just a body with no chunk framing at all, `dechunk` leaves that as
    /// pass-through.
    async fn post(addr: SocketAddr, path: &str) -> (u16, String) {
        let request = format!("POST {path} HTTP/1.1\r\nHost: {addr}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        let mut stream = TcpStream::connect(addr).await.expect("connect");
        stream.write_all(request.as_bytes()).await.expect("write");
        let mut response = String::new();
        stream.read_to_string(&mut response).await.expect("read");
        let mut parts = response.splitn(2, "\r\n\r\n");
        let head = parts.next().unwrap_or_default();
        let raw_body = parts.next().unwrap_or_default();
        let status = head
            .lines()
            .next()
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        let body = if head
            .to_ascii_lowercase()
            .contains("transfer-encoding: chunked")
        {
            dechunk(raw_body)
        } else {
            raw_body.to_string()
        };
        (status, body)
    }

    /// Decodes HTTP/1.1 chunked transfer-encoding — `<hex-size>\r\n<data>\r\n`
    /// chunks, terminated by a zero-size chunk. Byte-indexed rather than
    /// char-indexed would be more robust against a multi-byte character
    /// split across a chunk boundary, but every chunk in these tests is
    /// plain-ASCII NDJSON, so this is good enough for test purposes only.
    fn dechunk(raw: &str) -> String {
        let mut out = String::new();
        let mut rest = raw;
        loop {
            let Some(nl) = rest.find("\r\n") else { break };
            let Ok(size) = usize::from_str_radix(rest[..nl].trim(), 16) else {
                break;
            };
            rest = &rest[nl + 2..];
            if size == 0 || size > rest.len() {
                break;
            }
            out.push_str(&rest[..size]);
            rest = &rest[size..].strip_prefix("\r\n").unwrap_or(rest);
        }
        out
    }

    fn ndjson_events(body: &str) -> Vec<serde_json::Value> {
        body.lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str(l).expect("valid RunEvent JSON"))
            .collect()
    }

    #[tokio::test]
    async fn run_file_node_streams_output_and_exit_code() {
        // Every test in this module runs concurrently and shares the same
        // OS temp dir, so the target script's filename is namespaced with
        // its own uuid rather than a fixed literal — otherwise a
        // same-named target file from another test racing its own
        // write/cleanup could shadow this one mid-run.
        let script_name = format!("meshfox-run-file-test-{}-seed.sh", uuid::Uuid::new_v4());
        let canvas_path = write_test_canvas(&format!(
            "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n\n\
             ## Seed\n<!-- meshfox:node id=\"seed\" type=\"file\" interpreter=\"bash\" -->\n\n\
             [seed](./{script_name})\n"
        ));
        let target_path = canvas_path.with_file_name(&script_name);
        std::fs::write(&target_path, "#!/bin/sh\necho hi from seed\n").unwrap();

        let addr = spawn_test_server(canvas_path.clone()).await;
        let (status, body) = post(addr, "/api/nodes/seed/run").await;
        assert_eq!(status, 200);

        let events = ndjson_events(&body);
        assert_eq!(events[0]["type"], "started");
        assert_eq!(events[1]["type"], "step-start");
        assert_eq!(events[1]["nodeId"], "seed");
        assert_eq!(events[1]["block"], "seed");
        assert!(
            events
                .iter()
                .any(|e| e["type"] == "output" && e["text"] == "hi from seed"),
            "expected an output event with the script's own stdout, got: {events:?}"
        );
        let step_end = events
            .iter()
            .find(|e| e["type"] == "step-end")
            .expect("a step-end event");
        assert_eq!(step_end["exitCode"], 0);
        assert_eq!(events.last().unwrap()["type"], "done");

        let _ = std::fs::remove_file(&canvas_path);
        let _ = std::fs::remove_file(&target_path);
    }

    #[tokio::test]
    async fn run_file_node_rejects_a_node_with_no_interpreter() {
        // `is_runnable_file` rejects this before the target is ever
        // resolved on disk, so the (nonexistent) `./seed.sh` target is
        // fine left unwritten — no risk of colliding with another test's
        // own same-named file.
        let canvas_path = write_test_canvas(concat!(
            "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
            "## Seed\n<!-- meshfox:node id=\"seed\" type=\"file\" -->\n\n",
            "[seed](./seed.sh)\n",
        ));

        let addr = spawn_test_server(canvas_path.clone()).await;
        let (status, body) = post(addr, "/api/nodes/seed/run").await;
        assert_eq!(status, 422);
        assert!(
            body.contains("isn't a runnable file node"),
            "unexpected body: {body}"
        );

        let _ = std::fs::remove_file(&canvas_path);
    }

    #[tokio::test]
    async fn run_file_node_rejects_an_unknown_node() {
        let canvas_path = write_test_canvas(
            "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n",
        );
        let addr = spawn_test_server(canvas_path.clone()).await;
        let (status, _) = post(addr, "/api/nodes/nope/run").await;
        assert_eq!(status, 404);
        let _ = std::fs::remove_file(&canvas_path);
    }

    #[tokio::test]
    async fn open_node_file_rejects_a_non_file_node() {
        let canvas_path = write_test_canvas(
            "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n",
        );
        let addr = spawn_test_server(canvas_path.clone()).await;
        let (status, body) = post(addr, "/api/nodes/root/open").await;
        assert_eq!(status, 422);
        assert!(body.contains("not a file node"), "unexpected body: {body}");
        let _ = std::fs::remove_file(&canvas_path);
    }

    #[tokio::test]
    async fn open_node_file_rejects_an_unknown_node() {
        let canvas_path = write_test_canvas(
            "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n",
        );
        let addr = spawn_test_server(canvas_path.clone()).await;
        let (status, _) = post(addr, "/api/nodes/nope/open").await;
        assert_eq!(status, 404);
        let _ = std::fs::remove_file(&canvas_path);
    }
}

#[cfg(test)]
mod var_status_tests {
    use super::*;

    fn decl(
        name: &str,
        default: Option<&str>,
        required: bool,
        secret: bool,
    ) -> meshfox_core::VarDecl {
        meshfox_core::VarDecl {
            name: name.to_string(),
            var_type: meshfox_core::VarType::String,
            prompt: name.to_string(),
            default: default.map(String::from),
            choices: Vec::new(),
            secret,
            required,
            from: None,
        }
    }

    #[test]
    fn required_with_default_offers_it_unresolved() {
        let d = decl("X", Some("default-val"), true, false);
        let resolved = meshfox_core::resolve_vars(
            std::slice::from_ref(&d),
            &HashMap::new(),
            &VarCache::in_memory(),
            &HashMap::new(),
        );
        let status = var_status(d, &resolved);
        assert!(!status.resolved);
        assert_eq!(status.value.as_deref(), Some("default-val"));
    }

    #[test]
    fn required_once_cached_resolves_normally() {
        let d = decl("X", Some("default-val"), true, false);
        let mut cache = VarCache::in_memory();
        cache.set("X", "confirmed-val").unwrap();
        let resolved = meshfox_core::resolve_vars(
            std::slice::from_ref(&d),
            &HashMap::new(),
            &cache,
            &HashMap::new(),
        );
        let status = var_status(d, &resolved);
        assert!(status.resolved);
        assert_eq!(status.value.as_deref(), Some("confirmed-val"));
    }

    #[test]
    fn plain_declaration_with_default_still_resolves_silently() {
        let d = decl("X", Some("default-val"), false, false);
        let resolved = meshfox_core::resolve_vars(
            std::slice::from_ref(&d),
            &HashMap::new(),
            &VarCache::in_memory(),
            &HashMap::new(),
        );
        let status = var_status(d, &resolved);
        assert!(status.resolved);
        assert_eq!(status.value.as_deref(), Some("default-val"));
    }

    #[test]
    fn required_secret_never_sends_its_default_either() {
        let d = decl("TOKEN", Some("default-val"), true, true);
        let resolved = meshfox_core::resolve_vars(
            std::slice::from_ref(&d),
            &HashMap::new(),
            &VarCache::in_memory(),
            &HashMap::new(),
        );
        let status = var_status(d, &resolved);
        assert!(!status.resolved);
        assert_eq!(status.value, None);
    }
}

#[cfg(test)]
mod vars_endpoint_tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpStream;

    fn write_test_canvas(contents: &str) -> PathBuf {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "meshfox-vars-test-{}.canvas.md",
            uuid::Uuid::new_v4()
        ));
        std::fs::write(&path, contents).unwrap();
        path
    }

    /// Plain (non-chunked) GET — `/api/vars`'s response is a single JSON
    /// array, not a stream, unlike `run_file_tests::post`'s target.
    async fn get(addr: SocketAddr, path: &str) -> (u16, String) {
        let request = format!("GET {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n");
        let mut stream = TcpStream::connect(addr).await.expect("connect");
        stream.write_all(request.as_bytes()).await.expect("write");
        let mut response = String::new();
        stream.read_to_string(&mut response).await.expect("read");
        let mut parts = response.splitn(2, "\r\n\r\n");
        let head = parts.next().unwrap_or_default();
        let body = parts.next().unwrap_or_default();
        let status = head
            .lines()
            .next()
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        (status, body.to_string())
    }

    const REQUIRED_CANVAS: &str = concat!(
        "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
        "<!-- meshfox:var name=\"INSTALL_PATH\" default=\"/usr/local/bin\" required -->\n\n",
        "```bash name=\"install\" env=\"$INSTALL_PATH\"\necho hi\n```\n",
    );

    #[tokio::test]
    async fn get_vars_reports_a_required_default_as_unresolved_but_prefilled() {
        let canvas_path = write_test_canvas(REQUIRED_CANVAS);
        let addr = spawn_test_server(canvas_path.clone()).await;

        let (status, body) = get(addr, "/api/vars?block=install").await;
        assert_eq!(status, 200);
        let statuses: Vec<serde_json::Value> =
            serde_json::from_str(&body).expect("valid VarStatus JSON");
        assert_eq!(statuses.len(), 1);
        assert_eq!(statuses[0]["name"], "INSTALL_PATH");
        assert_eq!(statuses[0]["resolved"], false);
        assert_eq!(statuses[0]["value"], "/usr/local/bin");

        let _ = std::fs::remove_file(&canvas_path);
        let _ = std::fs::remove_file(meshfox_core::varcache::cache_path(&canvas_path));
    }

    #[tokio::test]
    async fn get_vars_reports_a_required_var_resolved_once_cached() {
        let canvas_path = write_test_canvas(REQUIRED_CANVAS);
        let mut cache = VarCache::load(&canvas_path).expect("load cache");
        cache
            .set("INSTALL_PATH", "/opt/confirmed")
            .expect("seed cache");

        let addr = spawn_test_server(canvas_path.clone()).await;
        let (status, body) = get(addr, "/api/vars?block=install").await;
        assert_eq!(status, 200);
        let statuses: Vec<serde_json::Value> =
            serde_json::from_str(&body).expect("valid VarStatus JSON");
        assert_eq!(statuses[0]["resolved"], true);
        assert_eq!(statuses[0]["value"], "/opt/confirmed");

        let _ = std::fs::remove_file(&canvas_path);
        let _ = std::fs::remove_file(meshfox_core::varcache::cache_path(&canvas_path));
    }

    /// A JSON POST, same head-parsing as `get` above — `/api/vars/configure`'s
    /// response is a small, non-chunked JSON object too.
    async fn post_json(addr: SocketAddr, path: &str, body: &str) -> (u16, String) {
        let request = format!(
            "POST {path} HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let mut stream = TcpStream::connect(addr).await.expect("connect");
        stream.write_all(request.as_bytes()).await.expect("write");
        let mut response = String::new();
        stream.read_to_string(&mut response).await.expect("read");
        let mut parts = response.splitn(2, "\r\n\r\n");
        let head = parts.next().unwrap_or_default();
        let resp_body = parts.next().unwrap_or_default();
        let status = head
            .lines()
            .next()
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        (status, resp_body.to_string())
    }

    const CONFIGURE_CANVAS: &str = concat!(
        "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
        "<!-- meshfox:var name=\"GREETING\" default=\"Hello\" -->\n",
        "<!-- meshfox:var name=\"INSTALL_PATH\" default=\"/usr/local/bin\" required -->\n",
        "<!-- meshfox:var name=\"API_TOKEN\" secret -->\n\n",
        "```bash name=\"greet\" env=\"$GREETING\"\necho \"$GREETING\"\n```\n",
    );

    #[tokio::test]
    async fn get_configure_vars_lists_every_non_secret_declaration_regardless_of_env_usage() {
        // Unlike `/api/vars`, this isn't scoped to any block's own `env=`
        // chain — `INSTALL_PATH` isn't referenced by any block in this
        // canvas at all, and still shows up.
        let canvas_path = write_test_canvas(CONFIGURE_CANVAS);
        let addr = spawn_test_server(canvas_path.clone()).await;

        let (status, body) = get(addr, "/api/vars/configure").await;
        assert_eq!(status, 200);
        let statuses: Vec<serde_json::Value> =
            serde_json::from_str(&body).expect("valid VarStatus JSON");
        let names: Vec<&str> = statuses
            .iter()
            .map(|s| s["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, vec!["GREETING", "INSTALL_PATH"]);
        assert_eq!(statuses[0]["resolved"], true);
        assert_eq!(statuses[0]["value"], "Hello");
        // required, no default fallback allowed
        assert_eq!(statuses[1]["resolved"], false);
        assert_eq!(statuses[1]["value"], "/usr/local/bin");

        let _ = std::fs::remove_file(&canvas_path);
        let _ = std::fs::remove_file(meshfox_core::varcache::cache_path(&canvas_path));
    }

    #[tokio::test]
    async fn post_configure_vars_saves_every_answered_non_secret_variable() {
        let canvas_path = write_test_canvas(CONFIGURE_CANVAS);
        let addr = spawn_test_server(canvas_path.clone()).await;

        let (status, body) = post_json(
            addr,
            "/api/vars/configure",
            r#"{"vars":{"GREETING":"Hi","INSTALL_PATH":"/opt/app","API_TOKEN":"sk-should-not-be-saved","UNKNOWN":"ignored"}}"#,
        )
        .await;
        assert_eq!(status, 200);
        let resp: serde_json::Value = serde_json::from_str(&body).expect("valid response JSON");
        // Only the two declared non-secret variables actually present in
        // the document are saved — the secret and the unknown name aren't.
        assert_eq!(resp["saved"], 2);

        let cache = VarCache::load(&canvas_path).expect("load cache");
        assert_eq!(cache.get("GREETING"), Some("Hi"));
        assert_eq!(cache.get("INSTALL_PATH"), Some("/opt/app"));
        assert_eq!(cache.get("API_TOKEN"), None);

        let _ = std::fs::remove_file(&canvas_path);
        let _ = std::fs::remove_file(meshfox_core::varcache::cache_path(&canvas_path));
    }

    const TYPED_CANVAS: &str = concat!(
        "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
        "<!-- meshfox:var name=\"COUNT\" type=\"int\" -->\n",
        "<!-- meshfox:var name=\"VERBOSE\" type=\"bool\" -->\n",
        "<!-- meshfox:var name=\"LEVEL\" type=\"select\" choices=\"debug,info\" -->\n\n",
        "```bash name=\"run\" env=\"$COUNT,$VERBOSE,$LEVEL\"\necho \"$COUNT $VERBOSE $LEVEL\"\n```\n",
    );

    #[tokio::test]
    async fn post_configure_vars_rejects_an_invalid_value_for_every_type() {
        for (name, bad) in [
            ("COUNT", "not-a-number"),
            ("VERBOSE", "yes"),
            ("LEVEL", "trace"),
        ] {
            let canvas_path = write_test_canvas(TYPED_CANVAS);
            let addr = spawn_test_server(canvas_path.clone()).await;

            let body_json = format!(r#"{{"vars":{{"{name}":"{bad}"}}}}"#);
            let (status, body) = post_json(addr, "/api/vars/configure", &body_json).await;
            assert_eq!(
                status, 422,
                "{name}={bad:?} should have been rejected, got body: {body}"
            );
            assert!(body.contains(name), "unexpected body: {body}");

            let _ = std::fs::remove_file(&canvas_path);
            let _ = std::fs::remove_file(meshfox_core::varcache::cache_path(&canvas_path));
        }
    }

    #[tokio::test]
    async fn post_configure_vars_saves_nothing_when_one_entry_in_the_batch_is_invalid() {
        let canvas_path = write_test_canvas(TYPED_CANVAS);
        let addr = spawn_test_server(canvas_path.clone()).await;

        let (status, _) = post_json(
            addr,
            "/api/vars/configure",
            r#"{"vars":{"COUNT":"42","VERBOSE":"not-a-bool","LEVEL":"debug"}}"#,
        )
        .await;
        assert_eq!(status, 422);

        // Validated before any of the batch is saved — COUNT/LEVEL being
        // fine doesn't get them saved anyway once VERBOSE fails.
        let cache = VarCache::load(&canvas_path).expect("load cache");
        assert_eq!(cache.get("COUNT"), None);
        assert_eq!(cache.get("LEVEL"), None);

        let _ = std::fs::remove_file(&canvas_path);
        let _ = std::fs::remove_file(meshfox_core::varcache::cache_path(&canvas_path));
    }

    #[tokio::test]
    async fn run_block_rejects_an_invalid_typed_var_override() {
        let canvas_path = write_test_canvas(TYPED_CANVAS);
        let addr = spawn_test_server(canvas_path.clone()).await;

        let (status, body) = post_json(
            addr,
            "/api/run",
            r#"{"path":[],"block":"run","vars":{"COUNT":"not-a-number","VERBOSE":"true","LEVEL":"debug"}}"#,
        )
        .await;
        assert_eq!(status, 422);
        assert!(body.contains("COUNT"), "unexpected body: {body}");

        let _ = std::fs::remove_file(&canvas_path);
        let _ = std::fs::remove_file(meshfox_core::varcache::cache_path(&canvas_path));
    }
}

#[cfg(test)]
mod options_endpoint_tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpStream;

    fn write_test_canvas(contents: &str) -> PathBuf {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "meshfox-options-test-{}.canvas.md",
            uuid::Uuid::new_v4()
        ));
        std::fs::write(&path, contents).unwrap();
        path
    }

    async fn get(addr: SocketAddr, path: &str) -> (u16, String) {
        let request = format!("GET {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n");
        let mut stream = TcpStream::connect(addr).await.expect("connect");
        stream.write_all(request.as_bytes()).await.expect("write");
        let mut response = String::new();
        stream.read_to_string(&mut response).await.expect("read");
        let mut parts = response.splitn(2, "\r\n\r\n");
        let head = parts.next().unwrap_or_default();
        let resp_body = parts.next().unwrap_or_default();
        let status = head
            .lines()
            .next()
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        (status, resp_body.to_string())
    }

    /// Same shape as `vars_endpoint_tests::post_json`, just with a `PUT`
    /// request line — `PUT /api/options`'s response is a single JSON
    /// object (the whole `Canvas`), not a stream.
    async fn put_json(addr: SocketAddr, path: &str, body: &str) -> (u16, String) {
        let request = format!(
            "PUT {path} HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let mut stream = TcpStream::connect(addr).await.expect("connect");
        stream.write_all(request.as_bytes()).await.expect("write");
        let mut response = String::new();
        stream.read_to_string(&mut response).await.expect("read");
        let mut parts = response.splitn(2, "\r\n\r\n");
        let head = parts.next().unwrap_or_default();
        let resp_body = parts.next().unwrap_or_default();
        let status = head
            .lines()
            .next()
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        (status, resp_body.to_string())
    }

    const PLAIN_CANVAS: &str =
        "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n\nSome root prose.\n";

    #[tokio::test]
    async fn put_options_adds_a_declaration_and_persists_it_to_disk() {
        let canvas_path = write_test_canvas(PLAIN_CANVAS);
        let addr = spawn_test_server(canvas_path.clone()).await;

        let (status, body) = put_json(addr, "/api/options", r#"{"options":["unfold"]}"#).await;
        assert_eq!(status, 200);
        let resp: serde_json::Value = serde_json::from_str(&body).expect("valid Canvas JSON");
        assert_eq!(resp["options"], serde_json::json!(["unfold"]));

        let on_disk = std::fs::read_to_string(&canvas_path).unwrap();
        assert!(on_disk.contains(r#"meshfox:option name="unfold""#));
        assert!(
            on_disk.contains("Some root prose."),
            "unrelated body text should survive: {on_disk}"
        );

        let (status, body) = get(addr, "/api/canvas").await;
        assert_eq!(status, 200);
        let resp: serde_json::Value = serde_json::from_str(&body).expect("valid Canvas JSON");
        assert_eq!(resp["options"], serde_json::json!(["unfold"]));

        let _ = std::fs::remove_file(&canvas_path);
    }

    #[tokio::test]
    async fn put_options_removes_every_declaration_when_given_an_empty_list() {
        let doc = "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n<!-- meshfox:option name=\"unfold\" -->\n\nprose\n";
        let canvas_path = write_test_canvas(doc);
        let addr = spawn_test_server(canvas_path.clone()).await;

        let (status, body) = put_json(addr, "/api/options", r#"{"options":[]}"#).await;
        assert_eq!(status, 200);
        let resp: serde_json::Value = serde_json::from_str(&body).expect("valid Canvas JSON");
        // `Canvas.options` is `skip_serializing_if = "Vec::is_empty"` — an
        // empty result omits the field entirely rather than sending `[]`.
        assert!(resp.get("options").is_none(), "unexpected body: {body}");

        let on_disk = std::fs::read_to_string(&canvas_path).unwrap();
        assert!(!on_disk.contains("meshfox:option"));

        let _ = std::fs::remove_file(&canvas_path);
    }
}

#[cfg(test)]
mod link_preview_endpoint_tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpStream;

    fn write_test_canvas(contents: &str) -> PathBuf {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "meshfox-link-preview-endpoint-test-{}.canvas.md",
            uuid::Uuid::new_v4()
        ));
        std::fs::write(&path, contents).unwrap();
        path
    }

    const CANVAS: &str =
        "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n\nbody\n";

    /// Bare `TcpStream` GET, same "no full HTTP client crate" approach
    /// `ws_tests::reqwest_free_kill` uses — returns `(status, body)`.
    async fn get(addr: SocketAddr, path: &str) -> (u16, String) {
        let request = format!("GET {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n");
        let mut stream = TcpStream::connect(addr).await.expect("connect");
        stream.write_all(request.as_bytes()).await.expect("write");
        let mut response = String::new();
        stream.read_to_string(&mut response).await.expect("read");
        let mut parts = response.splitn(2, "\r\n\r\n");
        let head = parts.next().unwrap_or("");
        let body = parts.next().unwrap_or("").to_string();
        let status = head
            .lines()
            .next()
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        (status, body)
    }

    /// A loopback target is rejected by the SSRF check before it's ever
    /// connected to (see `link_preview::tests` for the direct, offline unit
    /// tests of that check) — the endpoint itself degrades this to a null
    /// preview rather than a request-level error, same as any other fetch
    /// failure (see `LinkPreviewResponse`'s own doc comment): a caller
    /// can't tell "blocked" apart from "unreachable" apart from "not
    /// HTML", by design.
    #[tokio::test]
    async fn blocked_target_returns_a_null_preview_not_an_error() {
        let canvas_path = write_test_canvas(CANVAS);
        let addr = spawn_test_server(canvas_path.clone()).await;

        let (status, body) = get(addr, "/api/link-preview?url=http%3A%2F%2F127.0.0.1%3A1%2F").await;
        assert_eq!(status, 200);
        assert_eq!(body, r#"{"preview":null}"#);

        let _ = std::fs::remove_file(&canvas_path);
    }

    /// A malformed `url` query value is still just a `String` to the
    /// extractor (no format validation happens until `fetch_og_preview`
    /// parses it) — same null-preview degradation as any other rejected
    /// target, not a 4xx.
    #[tokio::test]
    async fn malformed_url_also_returns_a_null_preview() {
        let canvas_path = write_test_canvas(CANVAS);
        let addr = spawn_test_server(canvas_path.clone()).await;

        let (status, body) = get(addr, "/api/link-preview?url=not-a-url").await;
        assert_eq!(status, 200);
        assert_eq!(body, r#"{"preview":null}"#);

        let _ = std::fs::remove_file(&canvas_path);
    }
}
