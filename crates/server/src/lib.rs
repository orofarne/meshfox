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
    mdcanvas, worker_lock, Canvas, ExecOutput, ExtraEdge, FenceAttrsPatch, FileDisplay, NodeMeta,
    NodeType, RunError, VarCache,
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
pub mod debug_session;
pub mod link_preview;
mod pty_exec;
/// `pub` so `meshfox-cli`'s TUI can share the exact same live-service
/// registry primitives (spawn/stop/restart/resource-sampling) the webui
/// server uses — see its own module doc comment for why the TUI links this
/// crate as a library rather than talking to it over HTTP.
pub mod services;
/// Webui-only (not shared with the CLI/TUI, unlike `services` — see this
/// module's own doc comment for why a plain block's reconnect-safe output
/// is specifically a multi-tab-webui concern) registry for a plain
/// block's own most recent run.
mod run_registry;
/// Webui-only multi-viewer registry for a `tty` block's own live pty
/// session — see its own module doc comment.
mod tty_registry;
/// Generic seq + ring-buffer + `subscribe_from` primitive — see its own
/// module doc comment.
mod seq_log;
/// Sequenced broadcast log backing `/api/watch` — see its own module doc
/// comment.
mod canvas_events;
/// `pub` so `meshfox-cli` can reuse the same async spawn/kill primitives
/// for `meshfox run`'s real-time output — see its `main.rs`.
pub mod stream_exec;
/// `pub` so `meshfox-cli`'s watcher (the coordinator this crate's own
/// workers talk to — see `open_node_file`/`run`) can speak the exact same
/// wire types without duplicating them.
pub mod watcher_protocol;

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
    /// Sequenced log of every `ServerEvent` (canvas changes, autorun
    /// triggers) — `watch_changes` forwards each one to its connected
    /// `/api/watch` client, plus enough backlog for a reconnecting client to
    /// tell "nothing missed" from "do a full resync" instead of just
    /// resuming blind. See `canvas_events`'s own module doc comment.
    canvas_events: canvas_events::CanvasEventLog,
    /// Whether the process should exit on its own once every `/api/watch`
    /// connection has gone (see `TabGuard`) — off for e.g. the e2e test
    /// server, which cycles through pages with brief all-tabs-closed gaps
    /// between tests that a real user closing their last tab wouldn't have.
    auto_exit: bool,
    /// `link` node social-preview cache (see `link_preview`) — one entry
    /// per URL, alive for exactly this process's lifetime.
    link_preview_cache: link_preview::PreviewCache,
    /// Every block that has completed successfully at least once during
    /// this `meshfox view` process's own lifetime, keyed by its address —
    /// consulted (and updated) by `run_block` so a "⛓ run chain" request
    /// can skip re-running a dependency that's already run this session
    /// *and* hasn't changed since (see `SessionRun`, `TODO.canvas.md`:
    /// "Не перезапускать уже выполненные в этой сессии зависимости").
    /// Never persisted anywhere and never touched by anything but this
    /// process's own runs — restarting `meshfox view` starts fresh.
    session_runs: Mutex<HashMap<(String, String), SessionRun>>,
    /// Resolved values for a `form` fence's own `field var=` entries (see
    /// SPEC.md's "Form fences"), submitted via `POST /api/form/submit` —
    /// same "never persisted, never touched by anything but this process's
    /// own runs" lifetime as `session_runs` (cleared alongside it in
    /// `reset_session`), since every variable a form targets is itself
    /// implicitly `session`-scoped by its own `meshfox:var` declaration
    /// (node-scoped, per `meshfox_core::declared_vars`) — there'd be
    /// nothing for an on-disk cache entry to mean here. Folded into the
    /// `overrides` map at every variable-resolution call site, underneath
    /// whatever that specific call's own one-shot override already
    /// supplies — see `effective_overrides`.
    session_vars: Mutex<HashMap<String, String>>,
    /// Every `service` block this process has spawned and still knows
    /// about (running, crashed, or just-stopped — never removed on its
    /// own), keyed by `(node_id, block_name)` — see `crate::services`'s own
    /// module doc comment for why this is a real persistent registry, not
    /// a per-request kill-switch like `runs` above.
    services: Mutex<HashMap<(String, String), services::ServiceHandle>>,
    /// A plain (non-`service`, non-`tty`) block's most recently
    /// started run, keyed by address — lets its live/final output survive
    /// a reconnect or a second tab watching the same block, the same way
    /// `services` already does for `service` blocks (see
    /// `run_registry`'s own module doc comment). Only ever holds the
    /// *current* run for a given address; Part B's queued-time locking
    /// already guarantees there's never more than one in flight, so a
    /// fresh run of the same address simply replaces the old entry here —
    /// no separate expiry/GC needed.
    runs_registry: Mutex<HashMap<(String, String), Arc<run_registry::RunHandle>>>,
    /// Same idea as `runs_registry`, for a `tty` block's own live pty
    /// session instead of a plain block's own line-oriented output — see
    /// `tty_registry`'s own module doc comment. Also only ever holds the
    /// *current* session for a given address, for the same reason.
    tty_registry: Mutex<HashMap<(String, String), Arc<tty_registry::TtySessionHandle>>>,
    /// Every live `debug_session::DebugSession` a `crate::coordinator`-routed
    /// MCP/CLI client has started against this worker, keyed by a
    /// server-issued session id — see `/api/debug/*`. `tokio::sync::Mutex`,
    /// not `std::sync::Mutex`, since `DebugSession::send`/`stop` are
    /// `async` and can run for up to a `send`'s own timeout (default one
    /// minute); a per-session lock held that long must never be a
    /// blocking-thread lock. The outer map's own lock is only ever held
    /// just long enough to look up/insert/remove an `Arc`, never across an
    /// `.await`.
    debug_sessions: Mutex<HashMap<String, Arc<tokio::sync::Mutex<debug_session::DebugSession>>>>,
    /// Where this worker's own coordinating watcher (or, eventually, a
    /// persistent GUI daemon) is listening — `None` only for a worker
    /// started without one (the `#[cfg(test)]` server, or a hand-run
    /// `meshfox-server` embedder that doesn't need cross-canvas
    /// navigation). `open_node_file` forwards a `.canvas.md` target's
    /// "↗ open" here (`watcher_protocol::request_open`) instead of
    /// spawning/tracking anything itself — see that module's own doc
    /// comment for why.
    watcher_socket: Option<PathBuf>,
}

impl AppState {
    /// Writes `raw` to disk *and* updates the server's own in-memory view
    /// to match, in one step — every call site used to do the two
    /// separately, which meant a successful save never told any other tab
    /// connected to this same worker (VS Code's own "Open in Browser"
    /// reopens the exact same worker, and so does opening the same canvas
    /// twice) that anything had changed: `spawn_file_watcher`'s polling
    /// thread is the only thing that ever sends a `changed` event over
    /// `/api/watch`, and it deliberately skips exactly this case — by the
    /// time it polls, `state.raw` already matches the file it just wrote,
    /// so nothing looks different to *it* even though a sibling tab never
    /// saw this write at all. Broadcasting here, right where the write
    /// that every other tab needs to hear about actually happens, covers
    /// that gap without touching the watcher's own external-change logic.
    fn save(&self, raw: &str) -> std::io::Result<()> {
        self.save_with_event(raw, ServerEvent::Changed)
    }

    /// Same as `save`, but broadcasts `event` instead of the generic
    /// `Changed` — `commit_located` uses this to push a precise
    /// `NodeUpserted`/`NodeRemoved`/`NodesReordered` for a mutation that
    /// landed in the primary canvas file, so a client watching for those
    /// can apply it in place instead of reloading (see `ServerEvent`'s own
    /// doc comment). Still always updates `self.raw` and writes the file
    /// exactly like `save` — `event` only changes what gets broadcast, not
    /// what gets persisted.
    fn save_with_event(&self, raw: &str, event: ServerEvent) -> std::io::Result<()> {
        std::fs::write(&self.canvas_path, raw)?;
        *self.raw.lock().unwrap() = raw.to_string();
        self.canvas_events.push(event);
        Ok(())
    }
}

/// `session_vars` (a form's own Send, see `submit_form`) folded underneath
/// `request_vars` (whatever one-shot override this specific call already
/// carries — a `RunRequest.vars`/`TtyRunQuery.vars` entry, or none at all)
/// — the map every variable-resolution call site actually passes as its
/// `overrides` argument. A request-specific override still wins over a
/// stored session value, same "most specific wins" precedence every other
/// override tier here already has.
fn effective_overrides(
    state: &AppState,
    request_vars: &HashMap<String, String>,
) -> HashMap<String, String> {
    let mut overrides = state.session_vars.lock().unwrap().clone();
    overrides.extend(request_vars.iter().map(|(k, v)| (k.clone(), v.clone())));
    overrides
}

/// One block's most recent successful run this session — see
/// `AppState::session_runs`.
#[derive(Clone)]
struct SessionRun {
    /// `meshfox_core::session_fingerprint` of the block *as it stood* (code/
    /// lang/interpreter/env=/deps=, same as `crate::output`'s cached-output
    /// staleness mechanism) *and* the resolved values of whatever variables
    /// it referenced, on that successful run — a later run_block call only
    /// treats this as "already fresh" (skippable) if both still match. A
    /// re-answered `meshfox:var`/changed `--set` override/different
    /// upstream `from=` value makes it stale again just as much as an edit
    /// to the block itself would.
    fingerprint: String,
    /// Whatever this block wrote to its own vars-out file last time it
    /// actually ran (only ever non-empty for a block that's a `from=`
    /// source for something) — folded into `resolved_vars` in place of
    /// re-running it when this run is skipped, so a later step that
    /// declared `from=` this block still gets a value. Only ever recorded
    /// from a `0`-exit run, same trust boundary `run_block`'s own live path
    /// already has for `from=` values.
    produced_vars: HashMap<String, String>,
    /// Whatever this block printed (merged stdout/stderr) the last time it
    /// actually ran — there's no fresh output to show from a skipped run
    /// (it didn't run), so this is what `RunEvent::StepSkipped` sends
    /// instead, letting the client still show it (typically collapsed by
    /// default — see `web/src/MeshNode.tsx`'s `LiveRunOutput`). Empty for a
    /// `tty` step, which never populates `full_output` to begin with (see
    /// the `!block.tty` guard around `run_tty_chain`'s own `ExecOutput`).
    output: String,
    /// That same earlier run's own wall-clock duration, in milliseconds —
    /// mirrors `RunEvent::StepEnd`'s `duration_ms`.
    duration_ms: u64,
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

/// True if any `service` this process has spawned is still `Running` —
/// consulted by `TabGuard`'s auto-exit re-check so closing the last open
/// tab doesn't kill a service the whole point of the service panel is to
/// keep tracking across page reloads/tab closures. A service is tied to
/// its owning process (see SPEC.md's "Service blocks (experimental)"), and
/// this *is* that process, so exiting anyway would kill it too.
fn has_running_services(state: &AppState) -> bool {
    state
        .services
        .lock()
        .unwrap()
        .values()
        .any(|s| matches!(s.status(), services::ServiceStatus::Running))
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
                if state.open_tabs.load(Ordering::SeqCst) == 0 && !has_running_services(&state) {
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
                state.canvas_events.push(ServerEvent::Changed);
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

/// Resolves `run_block_impl`'s own target-address reservation (see that
/// call site's own doc comment) if the chain's execution never actually
/// reaches the requested block's own step — an earlier dependency failing
/// and breaking the loop, say, or the client disconnecting before then.
/// `run_registry::RunHandle::resolve_if_unreached` is itself the guard
/// against a double-resolve (a no-op once `run_registry::attach` already
/// took over) — this struct just guarantees *something* calls it on every
/// way this generator can end, the same reasoning `RunGuard` above exists
/// for. `None` once the target step's own `attach` call has already
/// consumed it (see that call site).
struct ResolveReservationOnDrop(Option<Arc<run_registry::RunHandle>>);

impl Drop for ResolveReservationOnDrop {
    fn drop(&mut self) {
        if let Some(handle) = self.0.take() {
            handle.resolve_if_unreached(run_registry::RunOutcome::Exited { exit_code: -1 });
        }
    }
}

/// Who already holds a contested address's lock — enough for a client to
/// show "X is already running (pid N, started via webui)" and offer a
/// force-retry (`force_run`, below). Internally still built as a plain
/// `409 Conflict` HTTP response (`lock_conflict_response`) — the same
/// shape `acquire_chain_locks`'s own doc comment describes, known *before*
/// any step actually runs — but `pump_run_response_into_ws` (the WS layer
/// every run-starting endpoint upgrades through) recognizes that status and
/// re-emits it as a `RunEvent::LockConflict` first message instead of a
/// rejected upgrade, since a browser `WebSocket` can't read a pre-upgrade
/// HTTP status/body at all. `Deserialize` here is for that same
/// `pump_run_response_into_ws` to read the body back out of the `409`
/// response it just built.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LockConflict {
    node_id: String,
    block: String,
    owner_pid: u32,
    owner_desc: String,
}

fn lock_conflict_response(conflict: &LockConflict) -> Response {
    let body = serde_json::to_string(conflict).expect("LockConflict always serializes");
    Response::builder()
        .status(StatusCode::CONFLICT)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body))
        .unwrap()
}

/// Every address in `chain` that actually needs its own lock claimed before
/// this run can start — every entry `forced_reruns` says won't be skipped
/// as already-fresh, minus a `service` address that's already a live,
/// `Running` instance in this very process (its own lock is already held
/// from when it was first spawned — nothing new to acquire, see
/// `AppState.services`). Deliberately built from `forced_reruns` alone
/// (already computed by `compute_forced_reruns` before this is ever called)
/// rather than re-resolving each block's own kind/fingerprint a second
/// time: neither `service` nor `tty` ever populates `session_runs` (see
/// `SessionRun`'s own doc comment), so `compute_forced_reruns` already
/// treats every such address as "no skip mechanism, always forced" on its
/// own — exactly the set this needs, service-liveness aside. Still needs
/// `locate_node` (not a full block scan) per address, purely to find which
/// file it actually lives in — `meshfox_core::service_lock_path` needs a
/// canvas path, and an `include`d node's can differ from the primary
/// document's own.
fn steps_needing_a_lock(
    state: &AppState,
    raw_snapshot: &str,
    chain: &[meshfox_core::BlockAddr],
    forced_reruns: &std::collections::HashSet<meshfox_core::BlockAddr>,
) -> Vec<(meshfox_core::BlockAddr, PathBuf)> {
    let mut out = Vec::new();
    for addr in chain {
        if !forced_reruns.contains(addr) {
            continue;
        }
        let key = (addr.node_id.clone(), addr.block_name.clone());
        let already_live_service = state
            .services
            .lock()
            .unwrap()
            .get(&key)
            .is_some_and(|s| matches!(s.status(), services::ServiceStatus::Running));
        if already_live_service {
            continue;
        }
        // A bad node/block reference surfaces as a normal `RunEvent::Error`
        // once the real per-step loop re-locates it for real — nothing to
        // lock for an address that doesn't even resolve.
        let Ok(located) = locate_node(state, raw_snapshot, &addr.node_id) else {
            continue;
        };
        let canvas_path_for_step = located.origin.clone().unwrap_or_else(|| state.canvas_path.clone());
        let lock_path = meshfox_core::service_lock_path(&canvas_path_for_step, &addr.node_id, &addr.block_name);
        out.push((addr.clone(), lock_path));
    }
    out
}

/// What `acquire_chain_locks` failed with — a real conflict (someone/
/// something already holds one of the needed locks) versus a plain I/O
/// problem acquiring one (surfaced as a `500`, not a `409`, since it isn't
/// really "in use").
enum ChainLockError {
    Conflict(LockConflict),
    Io(io::Error),
}

/// Attempts to acquire every lock `steps_needing_a_lock` returned, in
/// address order — all-or-nothing: the first conflict releases every lock
/// this same call already won, so a run either starts with *every* step it
/// will need already exclusively claimed by this process, or doesn't start
/// at all (the "queued" locking this crate's own design notes describe —
/// a step is never partway locked once some *other* step later in the same
/// chain turns out contested). Because this always runs before the
/// response even begins (both `run_block`'s plain NDJSON body and
/// `run_block_tty`'s WebSocket upgrade call it from their own pre-stream
/// setup), a conflict can be reported as an ordinary HTTP error rather
/// than a streamed terminal event — nothing has been sent to the client
/// yet either way.
///
/// On success, returns the paths actually acquired, in the same order —
/// the caller's own per-step loop removes an address from this set the
/// moment its fate is decided (released immediately once a *plain* step
/// finishes; left alone, without releasing, once a `service`/`tty` step
/// actually starts running under it, since that lock's lifetime now
/// tracks the running process, not this one request) and, at the very
/// end, releases whatever is still left — every address whose turn never
/// came because an earlier step failed or the run was killed.
fn acquire_chain_locks(
    targets: &[(meshfox_core::BlockAddr, PathBuf)],
    owner: &str,
) -> Result<HashMap<(String, String), PathBuf>, ChainLockError> {
    let mut acquired: HashMap<(String, String), PathBuf> = HashMap::new();
    for (addr, path) in targets {
        match meshfox_core::service_lock::acquire(path, std::process::id(), owner) {
            Ok(()) => {
                acquired.insert((addr.node_id.clone(), addr.block_name.clone()), path.clone());
            }
            Err(meshfox_core::service_lock::AcquireError::Conflict(info)) => {
                for path in acquired.values() {
                    let _ = meshfox_core::service_lock::release(path);
                }
                return Err(ChainLockError::Conflict(LockConflict {
                    node_id: addr.node_id.clone(),
                    block: addr.block_name.clone(),
                    owner_pid: info.pid,
                    owner_desc: info.owner,
                }));
            }
            Err(meshfox_core::service_lock::AcquireError::Io(e)) => {
                for path in acquired.values() {
                    let _ = meshfox_core::service_lock::release(path);
                }
                return Err(ChainLockError::Io(e));
            }
        }
    }
    Ok(acquired)
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
    /// Names (a subset of `vars`' own keys) of `secret`-declared variables
    /// the user explicitly opted into persisting anyway, via `VarsForm`'s
    /// own "save (plaintext)" checkbox — TODO.canvas.md: "Галочка
    /// 'сохранить' у secret". A name here only actually overrides anything
    /// for a declaration that's both `secret` *and* named here; harmless
    /// (ignored) for a non-secret name, since those already persist
    /// unconditionally. Never overrides `session` — see this override's own
    /// use below.
    #[serde(default)]
    save_secrets: std::collections::HashSet<String>,
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
    /// Set when `resolved`'s winning value actually came from a shared
    /// project-/global-config `[[env]]` section (see
    /// `meshfox_core::shared_env`) rather than an override/process env/
    /// the per-document cache — lets the UI badge a field as "inherited"
    /// while still letting the user type their own value to override it
    /// (which persists as a normal document-cache entry, same mechanism
    /// as any other answer). Shown even for a `secret` field — it only
    /// says *where* the value came from, never the value itself.
    #[serde(skip_serializing_if = "Option::is_none")]
    inherited_from: Option<VarOrigin>,
}

/// The web-facing shape of `meshfox_core::SharedOrigin` — see
/// `VarStatus::inherited_from`.
#[derive(Debug, Serialize)]
#[serde(tag = "scope", rename_all = "camelCase")]
enum VarOrigin {
    Project,
    Global {
        #[serde(skip_serializing_if = "Option::is_none")]
        path: Option<String>,
    },
}

impl From<&meshfox_core::SharedOrigin> for VarOrigin {
    fn from(origin: &meshfox_core::SharedOrigin) -> Self {
        match origin {
            meshfox_core::SharedOrigin::Project => VarOrigin::Project,
            meshfox_core::SharedOrigin::Global { path } => VarOrigin::Global { path: path.clone() },
        }
    }
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
    // Include-resolved (not just `parse_or_error`) so `path`/`block` below
    // can address a node spliced in from an `include`, same as `run_block`
    // — see `resolved_canvas`'s own doc comment.
    let canvas = resolved_canvas(&raw, &state.canvas_path)?;
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
        .iter()
        .filter(|d| needed.contains(&d.name) && d.from.is_none())
        .cloned()
        .collect();

    // A `choices_var`/`default_var` chain reaching a `from=`-computed
    // variable can't be known without actually running that variable's
    // own source block — nothing else about a status check like this one
    // ever executes anything, but there's no other way to show real
    // choices instead of an empty dropdown. Scoped to only the source
    // blocks `relevant`'s own fields actually need this way (see
    // `materialize_choices_and_defaults`), never a `from=` variable only
    // ever reached through an ordinary `env=` — that one's value is still
    // computed during the real run, same as always.
    let computed =
        materialize_choices_and_defaults(&canvas, &decls, &relevant, &state.canvas_path).await;

    let cache = state.vars_cache.lock().unwrap();
    let closure =
        meshfox_core::close_over_var_refs(&decls, relevant.iter().map(|d| d.name.as_str()));
    let decls_for_resolve: Vec<_> = decls
        .iter()
        .filter(|d| closure.contains(d.name.as_str()))
        .cloned()
        .collect();
    let shared = meshfox_core::load_shared_env(canvas_root_dir(&state.canvas_path));
    let overrides = effective_overrides(&state, &HashMap::new());
    let resolved = meshfox_core::resolve_with_shared(
        &decls_for_resolve,
        &overrides,
        &cache,
        &computed,
        &shared,
    );
    // A decl that needed prompting carries its own *materialized*
    // default/choices (substituted from `default_var`/`choices_var`) in
    // `resolved.missing`, not the raw `relevant` copy — see
    // `vars::resolve`'s own doc comment.
    let missing_by_name: HashMap<&str, &meshfox_core::VarDecl> =
        resolved.missing.iter().map(|d| (d.name.as_str(), d)).collect();
    let statuses = relevant
        .into_iter()
        .map(|d| {
            let materialized = missing_by_name
                .get(d.name.as_str())
                .map(|m| (*m).clone())
                .unwrap_or(d);
            var_status(materialized, &resolved)
        })
        .collect();
    Ok(Json(statuses))
}

/// Runs whichever `from=` source blocks are needed — transitively, via
/// `default_var`/`choices_var` (see `meshfox_core::close_over_var_refs`) —
/// to materialize real `choices`/`default` for `for_decls`, the variables
/// about to be shown as fields. A `from=` variable only ever reached
/// through an ordinary `env=` (not through another displayed field's own
/// `default_var`/`choices_var`) is deliberately left alone — its value is
/// still computed during the real run, never speculatively during a
/// status check.
///
/// Each source's own full `deps=` chain is run too (in order), same as a
/// real run would — but with no `env=` of its own resolved for any of
/// these steps (an empty environment, besides the usual
/// `MESHFOX_VARS_OUT`): a script meant to populate a dropdown's choices
/// is expected to be a self-contained, read-only query (`aws
/// list-regions`, `git branch -l`, ...), not one needing its own
/// document-declared input. A step that fails, or that itself needs
/// input this can't supply, just means whichever field(s) depended on it
/// stay without real choices this round — the same graceful "not yet
/// resolvable" fallback `vars::resolve` already has for any other
/// unresolvable `default_var`/`choices_var` reference, not a hard error
/// for the whole status check.
async fn materialize_choices_and_defaults(
    canvas: &Canvas,
    decls: &[meshfox_core::VarDecl],
    for_decls: &[meshfox_core::VarDecl],
    canvas_path: &std::path::Path,
) -> HashMap<String, String> {
    let mut chain: Vec<meshfox_core::BlockAddr> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for d in for_decls {
        for name in meshfox_core::close_over_var_refs(decls, [d.name.as_str()]) {
            let Some(from) = decls
                .iter()
                .find(|x| x.name == name)
                .and_then(|x| x.from.clone())
            else {
                continue;
            };
            let addr =
                meshfox_core::BlockAddr::new(from.node_id.unwrap_or_default(), from.block_name);
            if let Ok(steps) = meshfox_core::deps::resolve_chain(canvas, addr) {
                for step in steps {
                    let key = (step.node_id.clone(), step.block_name.clone());
                    if seen.insert(key) {
                        chain.push(step);
                    }
                }
            }
        }
    }

    let mut computed = HashMap::new();
    for addr in chain {
        if let Some(values) = run_from_source_for_status(canvas, &addr, canvas_path).await {
            computed.extend(values);
        }
    }
    computed
}

/// Runs a single block to completion for `materialize_choices_and_defaults`
/// — no streaming (the caller only needs whatever it wrote to its own
/// vars-out file, not to show progress), and its output is otherwise
/// discarded. `None` on a nonzero exit, an unsupported language, or any
/// I/O failure — never fatal for the caller, which just leaves the
/// affected field(s) without real choices/a default this round.
async fn run_from_source_for_status(
    canvas: &Canvas,
    addr: &meshfox_core::BlockAddr,
    canvas_path: &std::path::Path,
) -> Option<HashMap<String, String>> {
    let node = canvas.node(&addr.node_id)?;
    let cwd = node.cwd(canvas_root_dir(canvas_path));
    let block = meshfox_core::scan_runnable_blocks(&addr.node_id, &node.text)
        .into_iter()
        .find(|b| b.name.as_deref() == Some(addr.block_name.as_str()))?;
    if !stream_exec::supports(&block) {
        return None;
    }
    let path = meshfox_core::allocate_vars_out_path();
    let mut env = HashMap::new();
    env.insert(
        meshfox_core::VARS_OUT_ENV.to_string(),
        path.display().to_string(),
    );
    let mut proc = stream_exec::spawn_block(&block, &env, Some(&cwd), Some(canvas_path)).ok()?;
    while proc.output_rx.recv().await.is_some() {}
    let status = proc.child.wait().await.ok()?;
    if !status.success() {
        let _ = meshfox_core::read_and_cleanup_vars_out(&path);
        return None;
    }
    meshfox_core::read_and_cleanup_vars_out(&path).ok()
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
    let inherited_from = resolved.origins.get(&d.name).map(VarOrigin::from);
    VarStatus {
        name: d.name,
        var_type: d.var_type.as_str(),
        prompt: d.prompt,
        choices: d.choices,
        secret: d.secret,
        resolved: is_resolved,
        value: if d.secret { None } else { value },
        inherited_from,
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
    // Include-resolved so a `meshfox:var` declared inside an `include` is
    // offered here too, same reasoning as `get_vars`.
    let canvas = resolved_canvas(&raw, &state.canvas_path)?;
    let decls = meshfox_core::declared_vars(&canvas)
        .map_err(|e| ApiError(StatusCode::UNPROCESSABLE_ENTITY, e.to_string()))?;
    // Same reasoning as `get_vars`: a `from`-declared variable is computed,
    // never something to configure by hand. A `session` variable is never
    // cached at all, so there's nothing here to configure either.
    let configurable: Vec<_> = decls
        .iter()
        .filter(|d| !d.secret && !d.session && d.from.is_none())
        .cloned()
        .collect();

    // Same reasoning as `get_vars`: a `choices_var`/`default_var` chain
    // reaching a `from=`-computed variable needs that variable's own
    // source block actually run to show real choices instead of an empty
    // dropdown.
    let computed =
        materialize_choices_and_defaults(&canvas, &decls, &configurable, &state.canvas_path).await;

    let cache = state.vars_cache.lock().unwrap();
    let closure =
        meshfox_core::close_over_var_refs(&decls, configurable.iter().map(|d| d.name.as_str()));
    let decls_for_resolve: Vec<_> = decls
        .iter()
        .filter(|d| closure.contains(d.name.as_str()))
        .cloned()
        .collect();
    let shared = meshfox_core::load_shared_env(canvas_root_dir(&state.canvas_path));
    let resolved = meshfox_core::resolve_with_shared(
        &decls_for_resolve,
        &HashMap::new(),
        &cache,
        &computed,
        &shared,
    );
    let missing_by_name: HashMap<&str, &meshfox_core::VarDecl> =
        resolved.missing.iter().map(|d| (d.name.as_str(), d)).collect();
    let statuses = configurable
        .into_iter()
        .map(|d| {
            let materialized = missing_by_name
                .get(d.name.as_str())
                .map(|m| (*m).clone())
                .unwrap_or(d);
            var_status(materialized, &resolved)
        })
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
    // Include-resolved so a `meshfox:var` declared inside an `include` can
    // be saved here too, same reasoning as `get_vars`.
    let canvas = resolved_canvas(&raw, &state.canvas_path)?;
    let decls = meshfox_core::declared_vars(&canvas)
        .map_err(|e| ApiError(StatusCode::UNPROCESSABLE_ENTITY, e.to_string()))?;
    let configurable: Vec<_> = decls
        .into_iter()
        .filter(|d| !d.secret && !d.session)
        .collect();

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
    /// The connection-scoped kill switch `RunEvent::Started` handed back —
    /// still the only way to kill a `service`/`tty` step still inside its
    /// own *pre-registry* setup, or a chain step that hasn't reached the
    /// plain-block registry path at all (a still-running node/var
    /// resolution, vanishingly brief in practice). Mutually exclusive with
    /// `node_id`/`block` below — a request names one or the other, not
    /// both (both absent, or both present, is a client bug).
    #[serde(default)]
    run_id: Option<String>,
    /// Kills whatever's currently registered for this address in
    /// `AppState::runs_registry` — usable by a tab that never itself
    /// started the run (a reconnect, or a second tab watching via
    /// `/api/run/subscribe`) and so never had a `runId` to begin with.
    #[serde(default)]
    node_id: Option<String>,
    #[serde(default)]
    block: Option<String>,
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
    /// Terminal for *this step only* (the chain keeps going) — emitted
    /// instead of `StepStart`/`Output`/`StepEnd` when this step is a
    /// pulled-in dependency (never the block actually requested — see
    /// `AppState::session_runs`'s own doc comment) that already ran
    /// successfully earlier in this same `meshfox view` session and hasn't
    /// changed since. Emitted the same way over both the plain NDJSON
    /// `/api/run` path and `/api/run/tty`'s WebSocket — `run_tty_chain`
    /// consults the same `session_runs` map `run_block` does.
    StepSkipped {
        node_id: String,
        block: String,
        /// Whatever this step printed the last time it actually ran (see
        /// `SessionRun::output`) — the client shows this in place of fresh
        /// output, typically collapsed by default.
        output: String,
        /// That same earlier run's own duration, in milliseconds — mirrors
        /// `StepEnd::duration_ms`.
        duration_ms: u64,
    },
    /// One line of stdout/stderr, as it's produced — the two remain
    /// interleaved on this one event stream in roughly their real
    /// emission order (same caveat `stream_exec::OutputStream`'s own doc
    /// comment has: two separate pipes, no ordering guarantee between
    /// them), but `stream` says which pipe each line actually came from,
    /// so a client rendering `output="markdown"` mode's live view can
    /// split stdout (parsed as Markdown) from stderr (its own plain-text
    /// block) the same way a `cache`d run's persisted result already does
    /// (`core::output::ExecOutput::stdout`/`.stderr`) — see
    /// `MeshNode.tsx`'s `LiveRunOutput`.
    Output {
        node_id: String,
        block: String,
        stream: stream_exec::OutputStream,
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
    /// Terminal for *this step only* (the chain keeps going, same as
    /// `StepSkipped`) — emitted for a `service` block (see
    /// `meshfox_core::CodeBlock::service`) right after it's spawned,
    /// instead of `Output`*/`StepEnd`: a service's "done" is defined as
    /// "spawned", not "exited", so nothing here ever waits for an exit
    /// code. See SPEC.md's "Service blocks (experimental)".
    ServiceStarted {
        node_id: String,
        block: String,
        pid: u32,
    },
    /// A lock conflict on any block (not just `service`) is known *before*
    /// a run's actual per-step loop ever starts — queued-time locking (see
    /// `acquire_chain_locks`) means every conflict this whole chain will
    /// ever hit is already known before anything runs. Sent as the very
    /// first message in place of `Started` (`run_block`/`force_run` are
    /// WebSocket endpoints — a browser's native `WebSocket` has no way to
    /// read a rejected/failed handshake's own status or body, so this
    /// can't be reported as an HTTP status the way it briefly was when
    /// these were still plain HTTP-streamed endpoints; see
    /// `pump_run_response_into_ws`). A precursor to this same idea,
    /// `ServiceLockConflict`, lived here once before for an unrelated
    /// reason (service-lock checking used to happen mid-stream, one step
    /// at a time) and was removed once locking became queued-time — this
    /// isn't reviving that old mid-stream case, just moving where an
    /// already-queued-time conflict gets reported from.
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
        /// Wall-clock time this step's own process ran, in milliseconds —
        /// timed from right before it was spawned to right after its exit
        /// code was known, the same figure (for a `cache`d, persisted step)
        /// written into `ExecOutput::duration_ms`/the cached-output header,
        /// so what the client shows live and what a reload later shows from
        /// disk agree. Lets the web UI show a real duration the instant a
        /// step finishes rather than only a client-measured approximation.
        duration_ms: u64,
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

/// Events `GET /api/watch`'s long-lived NDJSON stream carries, one per
/// open browser tab — see `watch_changes`. `Changed` is the event that
/// already existed (a `()` payload before `render=`/`autorun` needed this
/// to carry a second shape); `RunStarted` lets a passive tab or TUI
/// session (one that didn't itself click run) discover a plain-block run
/// it should subscribe to — broadcast by `run_block_impl` for *every*
/// caller (an ordinary manual run, `force_run`, or `trigger_autorun`
/// alike, see its own `target_reservation` doc comment), not just
/// `autorun`. `GET /api/run/subscribe` is already address-keyed, not
/// initiator-keyed, so it already supports being watched by a tab that
/// never started it.
///
/// `NodeUpserted`/`NodeRemoved`/`NodesReordered` are the precise,
/// per-operation counterpart to `Changed`, for a client that wants to
/// apply a tree mutation in place instead of reloading the whole document
/// on every edit — see TODO.canvas.md's "WS push на мутации дерева" for
/// the fuller design rationale. `commit_located` is what actually pushes
/// these, from each mutating `/api/nodes*` handler, in *addition* to
/// `Changed` still covering the cases these three don't (an edit landing
/// in an `include` target file, or one that ripples across more nodes
/// than a single op can cleanly describe — `rename_node_id`/`clear_node_id`,
/// and `remove_node`'s own `?children=reparent` branch, still just push
/// `Changed`, see their own call sites). A client that doesn't know these
/// three yet can simply ignore them — they never replace `Changed` at the
/// same seq, only accompany it, so an old client watching only `Changed`
/// keeps working unmodified.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "kebab-case", rename_all_fields = "camelCase")]
enum ServerEvent {
    Changed,
    RunStarted { node_id: String, block: String },
    /// A node was created, or an existing one's own fields (title/body/
    /// meta/edges/parent) changed — `node.parent` already says where it
    /// belongs, so a fresh id a client hasn't seen is exactly "insert
    /// this", and a known id is "replace what you have for it with this".
    /// Covers `create_node`, `update_node`, `clear_node_layout`, and
    /// `reparent_node` (a structural-parent change is just a `parent`
    /// field change from this shape's point of view).
    NodeUpserted { node: Box<meshfox_core::Node> },
    /// `node_id`'s own subtree was deleted outright (`keep_children:
    /// false` — `mdcanvas::delete_node`) — never sent for the `?children=
    /// reparent` branch (`mdcanvas::delete_node_reparent_children`), which
    /// changes every direct child's own `parent` too and just pushes
    /// `Changed` instead rather than also emitting one `NodeUpserted` per
    /// promoted child.
    NodeRemoved { node_id: String, keep_children: bool },
    /// `parent_id`'s direct children are now ordered exactly as
    /// `child_ids` lists them (`move_sibling`) — sent as the *whole* new
    /// order rather than a "moved X before/after Y" delta so a client
    /// never has to reconstruct one from the other.
    NodesReordered { parent_id: String, child_ids: Vec<String> },
}

fn ndjson_line<T: Serialize>(event: &T) -> Bytes {
    let mut line = serde_json::to_string(event).expect("event always serializes");
    line.push('\n');
    Bytes::from(line)
}

fn run_event_msg(event: &RunEvent) -> Message {
    Message::Text(serde_json::to_string(event).expect("RunEvent always serializes"))
}

/// Drives `run_block_impl`/`run_file_node`'s own `Result<Response, ApiError>` (an
/// ordinary NDJSON-streaming `application/x-ndjson` body on success, a plain HTTP
/// error status otherwise) into an *already-upgraded* WebSocket — one
/// `Message::Text` per NDJSON line on success, or the pre-stream failure
/// converted into a single first-and-only message otherwise. Neither of those
/// two functions' own bodies change at all for this: whatever they already
/// build as an HTTP response is exactly what this reads back out and re-sends
/// as WS frames, so `run_block`/`force_run`/`run_file_node` are the only
/// functions that actually need to know they're WebSocket endpoints now, not
/// plain HTTP-streamed ones.
///
/// Why every pre-stream failure has to become a message instead of staying a
/// rejected upgrade (a `409`/`422`/`404` HTTP status, same as before this
/// existed): a browser's native `WebSocket` has no way to read a failed
/// handshake's own status code or body at all — only a content-free
/// `onerror` + `onclose`. A Rust WS client (`tokio-tungstenite`, TUI's own)
/// *can* read that (confirmed already working for `/api/run/tty`'s own
/// pre-upgrade `409`), but the web UI can't, so relying on it here would
/// make every one of these failures silently invisible in a browser. Once
/// the socket is open, both clients can read an ordinary JSON text message
/// equally well.
async fn pump_run_response_into_ws(mut socket: WebSocket, result: Result<Response, ApiError>) {
    use futures_util::StreamExt;
    let response = match result {
        Ok(r) => r,
        Err(e) => e.into_response(),
    };
    let status = response.status();
    if status == StatusCode::OK {
        let mut body = response.into_body().into_data_stream();
        while let Some(Ok(bytes)) = body.next().await {
            // Each item is already exactly one `ndjson_line()`'s worth (always
            // ends in `\n`) — reading the body's own stream items directly,
            // not a re-parsed wire transfer, so items are never split/merged
            // the way reading a real HTTP connection byte-by-byte could.
            let text = String::from_utf8_lossy(&bytes);
            if socket.send(Message::Text(text.trim_end().to_string())).await.is_err() {
                return;
            }
        }
        // A plain `drop(socket)` here never sends a WS `Close` frame —
        // just the underlying TCP connection going away, which a strict
        // client (`tokio-tungstenite`, TUI's own) reports as `Protocol(
        // ResetWithoutClosingHandshake)` instead of a clean end of stream.
        let _ = socket.close().await;
        return;
    }
    let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap_or_default();
    let event = if status == StatusCode::CONFLICT {
        match serde_json::from_slice::<LockConflict>(&body_bytes) {
            Ok(c) => RunEvent::LockConflict {
                node_id: c.node_id,
                block: c.block,
                owner_pid: c.owner_pid,
                owner_desc: c.owner_desc,
            },
            Err(e) => RunEvent::Error { message: e.to_string() },
        }
    } else {
        RunEvent::Error { message: String::from_utf8_lossy(&body_bytes).into_owned() }
    };
    let _ = socket.send(run_event_msg(&event)).await;
    let _ = socket.close().await;
}

#[derive(Debug)]
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
            RunError::Deps(_) => StatusCode::UNPROCESSABLE_ENTITY,
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
    meshfox_core::constraint::annotate_status(&mut canvas, Some(canvas_root_dir(canvas_path)));
    // Best-effort: a malformed `meshfox:option` declaration shouldn't break
    // *viewing* the canvas (falls back to no options declared, same as if
    // there were none at all) — `meshfox validate` is what surfaces that
    // loudly, same split `vars`/constraint fences already have between
    // "parses enough to view" and "fully valid".
    canvas.options = meshfox_core::declared_options(&canvas).unwrap_or_default();
    // Same best-effort split for `meshfox:tag-color` — a malformed
    // declaration just means no node falls back to a tag-derived color
    // this fetch, not a broken canvas view.
    meshfox_core::annotate_effective_colors(&mut canvas);
    Ok(Json(canvas))
}

// TODO.canvas.md: "Node colour by tag" — `canvas_response`'s own
// `annotate_effective_colors` call.
#[cfg(test)]
mod canvas_response_tag_color_tests {
    use super::*;

    fn expect_ok(result: Result<Json<Canvas>, ApiError>) -> Canvas {
        match result {
            Ok(Json(canvas)) => canvas,
            Err(e) => panic!("unexpected error: {}", e.1),
        }
    }

    #[test]
    fn a_node_with_no_explicit_color_gets_effective_color_from_its_tag() {
        let raw = concat!(
            "# Root\n<!-- meshfox:node id=\"root\" -->\n",
            "<!-- meshfox:tag-color tag=\"bug\" color=\"1\" -->\n\n",
            "## Child\n<!-- meshfox:node id=\"child\" tags=\"bug\" -->\n\nbody\n",
        );
        let canvas = expect_ok(canvas_response(raw, std::path::Path::new("test.canvas.md")));
        let child = canvas.nodes.iter().find(|n| n.id == "child").unwrap();
        assert_eq!(child.color, None);
        assert_eq!(child.effective_color.as_deref(), Some("1"));
    }

    #[test]
    fn a_malformed_tag_color_declaration_does_not_break_the_response() {
        // Missing color= makes `declared_tag_colors` error — best-effort
        // display shouldn't break the whole canvas fetch over it.
        let raw = concat!(
            "# Root\n<!-- meshfox:node id=\"root\" -->\n",
            "<!-- meshfox:tag-color tag=\"bug\" -->\n\n",
            "## Child\n<!-- meshfox:node id=\"child\" tags=\"bug\" -->\n\nbody\n",
        );
        let canvas = expect_ok(canvas_response(raw, std::path::Path::new("test.canvas.md")));
        let child = canvas.nodes.iter().find(|n| n.id == "child").unwrap();
        assert_eq!(child.effective_color, None);
    }
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
/// `None` for `state`'s own document (already cached as
/// `state.canvas_path`/`state.raw`); `Some(path)` for an include target —
/// `raw` in that case is read fresh from disk each time rather than
/// cached in `AppState`, since it isn't the file this server session
/// "owns". Same struct `meshfox_core::locate_node`/`meshfox run`/the TUI
/// use — kept as a type alias here rather than a fresh definition so
/// every existing `located.origin`/`.raw`/`.local_id` reference below
/// stays untouched.
type LocatedNode = meshfox_core::LocatedNode;

/// Thin `ApiError`-flavored wrapper around `meshfox_core::locate_node` —
/// same lookup CLI/TUI now share, just with this server's own established
/// HTTP status codes and wording for each failure mode (unchanged from
/// before this was factored out into core).
fn locate_node(state: &AppState, primary_raw: &str, id: &str) -> Result<LocatedNode, ApiError> {
    meshfox_core::locate_node(primary_raw, &state.canvas_path, id).map_err(|e| match e {
        meshfox_core::LocateError::Parse(e) => {
            ApiError(StatusCode::UNPROCESSABLE_ENTITY, e.to_string())
        }
        meshfox_core::LocateError::Include(e) => {
            ApiError(StatusCode::UNPROCESSABLE_ENTITY, e.to_string())
        }
        meshfox_core::LocateError::NotFound(id) => {
            ApiError(StatusCode::NOT_FOUND, format!("no node {id:?}"))
        }
        meshfox_core::LocateError::NoOwnIdentity(id) => ApiError(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!(
                "node {id:?} lives inside an included canvas that can't be edited from here yet \
                 (it was included as plain Markdown rather than a .canvas.md, so it has no node \
                 identity of its own to write back to — open its own file directly to edit it)"
            ),
        ),
        meshfox_core::LocateError::Io(path, e) => ApiError(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to read include target {path}: {e}"),
        ),
    })
}

/// Writes `raw` back to wherever `located` says it actually came from —
/// `state`'s own file (also updating its in-memory cache, same as every
/// mutating endpoint already did) or an include target's file directly
/// (never cached — see `LocatedNode::origin`). The caller still has to
/// re-read `state.raw`/call `canvas_response` afterward for the response:
/// for an include target, that's what actually picks the edit back up
/// (`include::resolve` always reads the target fresh from disk), the same
/// way a follow-up `GET /api/canvas` would. `op` is the precise
/// `NodeUpserted`/`NodeRemoved`/`NodesReordered` this particular mutation
/// is (see `ServerEvent`'s own doc comment) — broadcast instead of the
/// generic `Changed` when this lands in the *primary* file, so a client
/// watching for it can apply it in place. Ignored for an include target
/// (`Some(path)` below): that write doesn't go through `AppState::save`
/// at all (there's no single in-memory `state.raw` for an include target
/// to update), and always broadcasts the generic `Changed` regardless of
/// `op` — teaching every op's client-side apply logic about "which file"
/// isn't worth it yet for what's still a rare edit path.
fn commit_located(state: &AppState, located: &LocatedNode, raw: &str, op: ServerEvent) -> Result<(), ApiError> {
    match &located.origin {
        None => {
            state
                .save_with_event(raw, op)
                .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        }
        Some(path) => {
            std::fs::write(path, raw)
                .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
            // Unlike the `None` arm above, this write doesn't go through
            // `AppState::save` (there's no single in-memory `state.raw` for
            // an include target to update) — broadcast here explicitly, or
            // an edit to an included node would otherwise go unnoticed by
            // every connected tab until the next 500ms file-watcher poll.
            state.canvas_events.push(ServerEvent::Changed);
        }
    }
    Ok(())
}

/// Builds a `NodeUpserted` event carrying `local_id`'s own current,
/// fully-annotated state — the shared last step every `commit_located`
/// call site that adds/changes a single node's own fields uses, since each
/// mutation function itself only produces the *patched document text*, not
/// the node's own final `meshfox_core::Node` in isolation. Resolves
/// `raw` the same way `canvas_response` does (`include` splicing,
/// `constraint::annotate_status`, `annotate_effective_colors`) so a
/// client applying this incrementally sees exactly the same node shape a
/// full `GET /api/canvas` would have given it — skipping either
/// annotation here would silently show a tag-derived color or a
/// constraint pass/fail badge as unset until the next full reload
/// happened to refresh it. Doing this full a pass *again* right after
/// `commit_located`'s caller already did its own `canvas_response` (or is
/// about to) is redundant work, not a correctness concern — an accepted
/// cost of the two computations not sharing a call site, given each
/// happens for a different reason (persisting the response's own snapshot
/// vs. building this broadcast's single-node payload). Falls back to
/// `ServerEvent::Changed` if `local_id` somehow isn't in `raw` (shouldn't
/// happen — every call site already validated `raw` parses and contains
/// this id before reaching here) or if resolution fails, rather than
/// panicking over what would only ever be a broadcast-payload oddity, not
/// a request failure the client already got its own `200` response for.
fn node_upserted_event(raw: &str, canvas_path: &std::path::Path, local_id: &str) -> ServerEvent {
    let Ok(mut canvas) = resolved_canvas(raw, canvas_path) else {
        return ServerEvent::Changed;
    };
    meshfox_core::constraint::annotate_status(&mut canvas, Some(canvas_root_dir(canvas_path)));
    meshfox_core::annotate_effective_colors(&mut canvas);
    match canvas.node(local_id).cloned() {
        Some(node) => ServerEvent::NodeUpserted { node: Box::new(node) },
        None => ServerEvent::Changed,
    }
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
        }
        SourceFile::Include { path, .. } => {
            std::fs::write(&path, &body)
                .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        }
    }
    Ok(StatusCode::NO_CONTENT)
}

/// One entry of `PutCanvasRequest::layout_hints` — a node's current
/// on-screen position, per `reorder_by_position`'s own `hints` parameter.
#[derive(Debug, Clone, Copy, Deserialize)]
struct LayoutHint {
    x: f64,
    y: f64,
}

/// `PUT /api/canvas`'s request body — `canvas` flattened in directly (same
/// shape this endpoint always took) plus `layoutHints`, a same-request-only
/// sort hint for `mdcanvas::reorder_by_position` (see that function's own
/// doc comment for why it exists): the web UI's own live auto-layout
/// position for a node it did *not* itself just drag/resize this save (see
/// `App.tsx`'s `handleSaveLayout`), keyed by that node's own possibly-
/// namespaced client-visible id — same id space `canvas.nodes[].id`
/// already uses. Never treated as authored data; `put_canvas` below splits
/// it into one local-id-keyed map per file (primary vs. each `include`
/// target) the same way it already does for `canvas.nodes` itself, since
/// `reorder_by_position` runs once per file and only knows that file's own
/// local ids.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PutCanvasRequest {
    #[serde(flatten)]
    canvas: Canvas,
    #[serde(default)]
    layout_hints: HashMap<String, LayoutHint>,
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
    Json(req): Json<PutCanvasRequest>,
) -> Result<StatusCode, ApiError> {
    let canvas = req.canvas;
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
            created_at: node.created_at.clone(),
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

    // Split the flat, client-namespaced `layout_hints` into one local-id-
    // keyed map per file — same routing every node in the main loop above
    // already went through, since `reorder_by_position` runs once per file
    // and only knows that file's own local ids. A hint naming a node that
    // no longer resolves to anything is silently dropped, same tolerance
    // the main loop above already has.
    let mut primary_hints: HashMap<String, (f64, f64)> = HashMap::new();
    let mut included_hints: HashMap<PathBuf, HashMap<String, (f64, f64)>> = HashMap::new();
    for (id, hint) in &req.layout_hints {
        let Ok(located) = locate_node(&state, &primary_raw, id) else {
            continue;
        };
        match &located.origin {
            None => {
                primary_hints.insert(located.local_id, (hint.x, hint.y));
            }
            Some(path) => {
                included_hints
                    .entry(path.clone())
                    .or_default()
                    .insert(located.local_id, (hint.x, hint.y));
            }
        }
    }

    if let Some(reordered) = mdcanvas::reorder_by_position(&primary_out, &primary_hints) {
        primary_out = reordered;
    }
    for (path, raw) in included_out.iter_mut() {
        let hints = included_hints.get(path).cloned().unwrap_or_default();
        if let Some(reordered) = mdcanvas::reorder_by_position(raw, &hints) {
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
            created_at: node.created_at.clone(),
        };
        if let Some(patched) = mdcanvas::set_node_meta(&raw, &node.id, &meta) {
            raw = patched;
        }
    }
    state
        .save(&raw)
        .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    canvas_response(&raw, &state.canvas_path)
}

/// Re-sorts every parent's own structural children by their real/auto-
/// placed position (`mdcanvas::reorder_by_position`) — the same pass
/// `PUT /api/canvas`'s own save flow already runs automatically on every
/// save, exposed as its own standalone endpoint so a worker-routed CLI/MCP
/// `node reorder` (which has no client-side auto-layout to hint from) can
/// trigger it directly, mirroring `apply_node_reorder`'s own direct-file
/// behavior exactly: no layout hints, so an unpositioned sibling sorts
/// last, stably. Scoped to the primary document only, same as
/// `clear_layout` above — an include target's own sibling order lives in
/// a separate file, untouched by this.
async fn reorder_siblings(State(state): State<Arc<AppState>>) -> Result<Json<Canvas>, ApiError> {
    let raw = state.raw.lock().unwrap().clone();
    let updated = mdcanvas::reorder_by_position(&raw, &HashMap::new())
        .ok_or_else(|| ApiError(StatusCode::UNPROCESSABLE_ENTITY, "failed to parse".to_string()))?;
    state
        .save(&updated)
        .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    canvas_response(&updated, &state.canvas_path)
}

/// Clears node `id`'s own authored `x`/`y`/`w`/`h`, reverting it to
/// auto-placement — the same per-node operation `clear_layout` above runs
/// over every node in the document at once, narrowed to just this one
/// (every other field — color/type/tags/...— is preserved exactly, same
/// as there). `404` if `id` doesn't exist. Unlike `clear_layout`, this
/// routes through `locate_node`/`commit_located` so it works on a node
/// spliced in from an `include` too.
async fn clear_node_layout(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<Canvas>, ApiError> {
    let primary_raw = state.raw.lock().unwrap().clone();
    let located = locate_node(&state, &primary_raw, &id)?;
    let canvas = parse_or_error(&located.raw)?;
    let node = canvas
        .node(&located.local_id)
        .ok_or_else(|| ApiError(StatusCode::NOT_FOUND, format!("no node {id:?}")))?;
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
        created_at: node.created_at.clone(),
    };
    let updated = mdcanvas::set_node_meta(&located.raw, &located.local_id, &meta)
        .ok_or_else(|| ApiError(StatusCode::NOT_FOUND, format!("no node {id:?}")))?;
    let op = node_upserted_event(&updated, &state.canvas_path, &located.local_id);
    commit_located(&state, &located, &updated, op)?;
    let response_raw = state.raw.lock().unwrap().clone();
    canvas_response(&response_raw, &state.canvas_path)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateNodeRequest {
    parent_id: String,
    title: String,
    /// `false` (the default — the field the existing web client never
    /// sends at all) keeps today's `insert_child_node_random_id` behavior.
    /// `true` uses the plain title-slug `insert_child_node` instead — for
    /// `crate::coordinator`-routed CLI/MCP `node add` (TODO.canvas.md:
    /// "worker-routing для node add/meta отложен"), which has always
    /// produced a readable, title-derived id for a *direct-file* `node
    /// add` and needs the exact same id scheme once routed through a
    /// worker instead — a caller that can't predict which scheme it'll
    /// get back can't reliably use the new node's id for anything
    /// afterward (a follow-up `node body`, say).
    #[serde(default)]
    title_slug_id: bool,
}

/// The new node's own id, alongside the whole (already-updated) canvas —
/// `#[serde(flatten)]` so every existing field `Json<Canvas>` alone used to
/// return is still there, at the same top level, for the web client's own
/// unchanged parsing; `newId` is simply new surface next to it. Neither
/// `insert_child_node` nor `insert_child_node_random_id` lets a caller
/// predict the id it's about to produce, so this is the only way a
/// worker-routed `node add` can learn it at all.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CreateNodeResponse {
    new_id: String,
    #[serde(flatten)]
    canvas: Canvas,
}

/// Adds a new, empty-bodied child heading node under `parentId`, as the
/// last item in its existing subtree. Deliberately doesn't set a
/// position — the web client's own auto-layout places it using the same
/// tree-aware default any other position-less node gets, which for a fresh
/// child means "to the right of its parent", exactly what the UI's "add
/// child" button wants without any extra placement logic here.
///
/// Uses `insert_child_node_random_id` by default, not the plain title-slug
/// `insert_child_node` CLI/MCP `node add` uses directly — the web UI's "add
/// child" button no longer opens a settings dialog first (TODO.canvas.md:
/// "Позволить редактировать заголовок прямо на канвасе"), so `req.title`
/// here is only ever the placeholder the client is about to let the user
/// overwrite inline, never a real title worth deriving an id from (see
/// TODO.canvas.md: "Id-хэши вместо new-node-X по умолчанию") — unless
/// `req.title_slug_id` opts into the other scheme instead (see
/// `CreateNodeRequest::title_slug_id`'s own doc comment).
async fn create_node(
    State(state): State<Arc<AppState>>,
    Json(req): Json<CreateNodeRequest>,
) -> Result<Json<CreateNodeResponse>, ApiError> {
    let primary_raw = state.raw.lock().unwrap().clone();
    // `parentId` may itself name a node spliced in from an include (e.g.
    // adding a child under something inside an included canvas) — locate
    // it first so the new node is actually written into the file the
    // parent lives in, same as editing an existing node there already is.
    let located = locate_node(&state, &primary_raw, &req.parent_id)?;
    let insert = if req.title_slug_id {
        mdcanvas::insert_child_node
    } else {
        mdcanvas::insert_child_node_random_id
    };
    let (updated, new_id) = insert(&located.raw, &located.local_id, &req.title).ok_or_else(|| {
        ApiError(
            StatusCode::NOT_FOUND,
            format!("no node {:?}", req.parent_id),
        )
    })?;
    // Insertion can't actually break parsing, but validate anyway — same
    // validate-before-commit shape every other mutating endpoint here uses.
    parse_or_error(&updated)?;
    let op = node_upserted_event(&updated, &state.canvas_path, &new_id);
    commit_located(&state, &located, &updated, op)?;
    let response_raw = state.raw.lock().unwrap().clone();
    let Json(canvas) = canvas_response(&response_raw, &state.canvas_path)?;
    Ok(Json(CreateNodeResponse { new_id, canvas }))
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
    /// Absolute position/size — `None` leaves each untouched, same
    /// convention as every other field here. Added for `crate::coordinator`-
    /// routed CLI/MCP `node meta` (TODO.canvas.md: "worker-routing для
    /// node add/meta отложен"): the web UI itself never sends these through
    /// this endpoint (a node is only ever moved/resized by drag, which has
    /// its own dedicated endpoints), so this is new surface for a caller
    /// that already has to pass *some* value, not something the existing
    /// web client needs to change to keep using. `width`/`height` are
    /// rejected below for a group node (real or becoming one via
    /// `nodeType` in the same request) — its box is always derived from
    /// its children, never stored, same invariant `crate::main`'s own
    /// `apply_node_meta` already enforces client-side for the direct-file
    /// path this is the worker-routed counterpart to.
    x: Option<f64>,
    y: Option<f64>,
    width: Option<f64>,
    height: Option<f64>,
    /// `None` leaves it untouched; explicit RFC3339 string (already
    /// validated CLI-side by `apply_node_meta` for the direct-file path,
    /// validated here too since this endpoint has its own callers now)
    /// replaces it. No "clear" sentinel — same as the direct-file path,
    /// which only ever adds/overwrites this, never removes it.
    created_at: Option<String>,
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
        existing_created_at,
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
        initial_node.created_at.clone(),
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

    // Same invariant `crate::main`'s own `apply_node_meta` enforces for the
    // direct-file path: a group's box is always derived from its children,
    // never stored — reject an explicit width/height for one outright
    // (whether it already is a group, or is becoming one via `nodeType` in
    // this same request) rather than silently writing (and immediately
    // ignoring) a size nothing will ever read back.
    if final_type == NodeType::Group && (req.width.is_some() || req.height.is_some()) {
        return Err(ApiError(
            StatusCode::UNPROCESSABLE_ENTITY,
            "group nodes never store a size — their box is always derived from their children"
                .to_string(),
        ));
    }
    if let Some(created_at) = &req.created_at {
        if !meshfox_core::timestamp::is_valid_rfc3339(created_at) {
            return Err(ApiError(
                StatusCode::UNPROCESSABLE_ENTITY,
                format!("invalid createdAt {created_at:?} — expected RFC3339"),
            ));
        }
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
        || req.x.is_some()
        || req.y.is_some()
        || req.width.is_some()
        || req.height.is_some()
        || req.created_at.is_some()
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
        // Groups never store a width/height regardless of what's in `x`/
        // `y` here (already rejected above if the request tried to set
        // one) — carrying `existing` width/height forward for a group
        // would just re-write whatever stray value was already there
        // instead of actually clearing it, so force both to `None` for
        // one, same as `crate::main`'s own `apply_node_meta`.
        let (final_width, final_height) = if final_type == NodeType::Group {
            (None, None)
        } else {
            (req.width.or(width), req.height.or(height))
        };
        let meta = NodeMeta {
            x: req.x.or(x),
            y: req.y.or(y),
            width: final_width,
            height: final_height,
            color: req.color.clone().or(existing_color),
            node_type: req.node_type,
            display,
            lang,
            interpreter,
            preview,
            edge_label,
            fold: resolve_fold_override(req.fold.as_deref(), existing_fold)?,
            tags: req.tags.clone().unwrap_or(existing_tags),
            created_at: req.created_at.clone().or(existing_created_at),
        };
        raw = mdcanvas::set_node_meta(&raw, &local_id, &meta).ok_or_else(not_found)?;
    }

    if let Some(target) = &req.target {
        // Preserves an existing caption (see `Node::caption`) rather than
        // silently dropping it — this patch only ever means "change the
        // link", never "clear whatever explanatory text was under it".
        let body = match &initial_node.caption {
            Some(caption) => format!("[{title}]({target})\n\n{caption}"),
            None => format!("[{title}]({target})"),
        };
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

    let op = node_upserted_event(&raw, &state.canvas_path, &local_id);
    commit_located(&state, &located, &raw, op)?;
    let response_raw = state.raw.lock().unwrap().clone();
    canvas_response(&response_raw, &state.canvas_path)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdateBlockAttrsRequest {
    name: Option<String>,
    lang: Option<String>,
    cache: Option<bool>,
    always: Option<bool>,
    default: Option<bool>,
    tty: Option<bool>,
    autoclose: Option<bool>,
    service: Option<bool>,
    /// Comma-separated `deps=` list, same syntax `--deps`/CLI's own `node
    /// block` already accepts (parsed via `meshfox_core::parse_deps_list`)
    /// — `None` leaves it untouched, `Some("")` clears it, anything else
    /// replaces it outright. A plain `Option<String>` carries this instead
    /// of a typed list since `meshfox_core::fence::BlockRef` has no
    /// `Serialize`/`Deserialize` of its own — reusing the exact text
    /// syntax the CLI already parses avoids needing one just for this.
    deps: Option<String>,
    /// Same convention as `deps`, for `env=` (`meshfox_core::parse_env_list`).
    env: Option<String>,
    /// `None` (the field not sent) leaves the interpreter untouched.
    /// `clearInterpreter: true` clears it back to unset (mutually
    /// exclusive with `interpreter` being set at the same time, same as
    /// the CLI's own `--interpreter`/`--clear-interpreter`) — plain
    /// `Option<String>` can't reach that third "explicitly cleared" state
    /// on its own, so this mirrors the CLI's own two-field shape instead
    /// of inventing a new sentinel convention.
    interpreter: Option<String>,
    #[serde(default)]
    clear_interpreter: bool,
    /// New code, replacing everything between the fence's own delimiter
    /// lines. `None` leaves it untouched.
    code: Option<String>,
}

/// Rewrites one runnable fence's own info-string attributes (and,
/// optionally, its code) inside node `id` — the worker-routed counterpart
/// to CLI/MCP `node block`'s own direct-file `apply_node_block`, which
/// this mirrors field-for-field (see that function's own doc comment for
/// why each tri-state is shaped the way it is). No web UI caller exists
/// for this yet — the browser UI has no fence-attribute editor of its own
/// — so every field here is new surface, not a behavior change for
/// anything already using `/api/nodes*`.
async fn update_block_attrs(
    State(state): State<Arc<AppState>>,
    Path((id, block_name)): Path<(String, String)>,
    Json(req): Json<UpdateBlockAttrsRequest>,
) -> Result<Json<Canvas>, ApiError> {
    if req.interpreter.is_some() && req.clear_interpreter {
        return Err(ApiError(
            StatusCode::UNPROCESSABLE_ENTITY,
            "interpreter is mutually exclusive with clearInterpreter".to_string(),
        ));
    }
    let interpreter = if req.clear_interpreter {
        Some(None)
    } else {
        req.interpreter.clone().map(Some)
    };
    let deps = req
        .deps
        .as_deref()
        .map(meshfox_core::parse_deps_list);
    let env = req
        .env
        .as_deref()
        .map(meshfox_core::parse_env_list);

    let primary_raw = state.raw.lock().unwrap().clone();
    let located = locate_node(&state, &primary_raw, &id)?;
    let not_found = || {
        ApiError(
            StatusCode::NOT_FOUND,
            format!("no runnable code block named {block_name:?} in node {id:?}"),
        )
    };

    let canvas = parse_or_error(&located.raw)?;
    let node = canvas.node(&located.local_id).ok_or_else(not_found)?;
    if !meshfox_core::scan_runnable_blocks(&located.local_id, &node.text)
        .iter()
        .any(|b| b.name.as_deref() == Some(block_name.as_str()))
    {
        return Err(not_found());
    }

    let deps_touched = req.deps.is_some();
    let patch = FenceAttrsPatch {
        name: req.name.clone(),
        lang: req.lang.clone(),
        cache: req.cache,
        always: req.always,
        default: req.default,
        tty: req.tty,
        autoclose: req.autoclose,
        service: req.service,
        deps,
        env,
        interpreter,
        code: req.code.clone(),
    };
    let updated = mdcanvas::set_fence_attrs(&located.raw, &located.local_id, &block_name, &patch)
        .ok_or_else(not_found)?;
    parse_or_error(&updated)?;
    if deps_touched {
        let updated_canvas = parse_or_error(&updated)?;
        meshfox_core::deps::validate(&updated_canvas)
            .map_err(|e| ApiError(StatusCode::UNPROCESSABLE_ENTITY, e.to_string()))?;
    }

    let op = node_upserted_event(&updated, &state.canvas_path, &located.local_id);
    commit_located(&state, &located, &updated, op)?;
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

/// A canvas file's own directory — `.` when `canvas_path` is a bare
/// filename with no directory component (`Path::parent()` on one of those
/// returns `Some("")`, not `None`, so a plain `unwrap_or(".")` never fires
/// and callers would otherwise try to canonicalize/chdir into an empty
/// path, which fails with ENOENT). This is the fallback half of a node's
/// own `cwd`/asset resolution (see `meshfox_core::canvas::Node::cwd`) —
/// what a node not spliced in from an `include` elsewhere on disk uses.
fn canvas_root_dir(canvas_path: &std::path::Path) -> &std::path::Path {
    canvas_path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(std::path::Path::new("."))
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
    let canvas_dir = canvas_root_dir(canvas_path);
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
    let primary_raw = state.raw.lock().unwrap().clone();
    let located = locate_node(&state, &primary_raw, &id)?;
    let canvas = parse_or_error(&located.raw)?;
    let node = canvas
        .node(&located.local_id)
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

    let canvas_path = located.origin.as_deref().unwrap_or(&state.canvas_path);
    let canvas_dir = canvas_root_dir(canvas_path);
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

/// One entry in `GET /api/syntax`'s listing — enough for the browser to
/// know what's available and fetch each one's raw grammar via
/// `GET /api/syntax/:name`. See `meshfox_core::syntax_dirs` for where these
/// come from (shared with the TUI's own `crate::syntax_registry` on the
/// `meshfox-cli` side, so both front ends see the same custom grammars).
#[derive(Serialize)]
struct SyntaxGrammarEntry {
    /// Filename — also the path segment `GET /api/syntax/:name` expects.
    name: String,
    /// `"local"` (`.meshfox/syntax/` next to the canvas) or `"global"`
    /// (`~/.meshfox/syntax/`) — whichever one this entry was actually
    /// resolved from once local/global name clashes are settled.
    source: &'static str,
}

/// `.tmLanguage.json`/`.sublime-syntax` filenames directly in `dir` (no
/// recursion) — empty, not an error, if `dir` doesn't exist.
fn list_grammar_files(dir: &std::path::Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            (name.ends_with(".tmLanguage.json") || name.ends_with(".sublime-syntax")).then_some(name)
        })
        .collect()
}

/// Lists custom syntax-highlighting grammars available to this canvas —
/// the "shared grammar repository" the browser reads from so Shiki/Monaco
/// can register the same local/global grammars the TUI already loads into
/// its own `syntect::parsing::SyntaxSet`. Local wins over global on a name
/// clash (same precedence `syntax_registry::build_syntax_set` uses on the
/// TUI side).
async fn get_syntax_list(State(state): State<Arc<AppState>>) -> Json<Vec<SyntaxGrammarEntry>> {
    let canvas_dir = canvas_root_dir(&state.canvas_path);
    let mut by_name: HashMap<String, &'static str> = HashMap::new();
    if let Some(dir) = meshfox_core::syntax_dirs::global_syntax_dir() {
        for name in list_grammar_files(&dir) {
            by_name.insert(name, "global");
        }
    }
    for name in list_grammar_files(&meshfox_core::syntax_dirs::local_syntax_dir(canvas_dir)) {
        by_name.insert(name, "local"); // overwrites a same-named global entry
    }
    let mut entries: Vec<SyntaxGrammarEntry> = by_name
        .into_iter()
        .map(|(name, source)| SyntaxGrammarEntry { name, source })
        .collect();
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    Json(entries)
}

/// Raw content of one custom grammar named by `GET /api/syntax`'s own
/// listing. `name` is never joined onto a filesystem path unchecked — it's
/// only ever used to look up an entry this handler already enumerated
/// itself via `list_grammar_files`, so there's no directory-traversal
/// surface here regardless of what a caller passes.
async fn get_syntax_file(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Result<String, ApiError> {
    let canvas_dir = canvas_root_dir(&state.canvas_path);
    let local_dir = meshfox_core::syntax_dirs::local_syntax_dir(canvas_dir);
    for dir in [Some(local_dir), meshfox_core::syntax_dirs::global_syntax_dir()]
        .into_iter()
        .flatten()
    {
        if list_grammar_files(&dir).contains(&name) {
            return std::fs::read_to_string(dir.join(&name))
                .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()));
        }
    }
    Err(ApiError(
        StatusCode::NOT_FOUND,
        format!("no custom grammar {name:?}"),
    ))
}

/// `GET /api/nodes/:id/run` — the WS upgrade wrapper around `run_file_node_
/// impl`: see `pump_run_response_into_ws`'s own doc comment for why this
/// always upgrades and reports pre-stream failures as a first message
/// instead of a rejected upgrade.
async fn run_file_node(
    ws: WebSocketUpgrade,
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    ws.on_upgrade(move |socket| async move {
        pump_run_response_into_ws(socket, run_file_node_impl(state, id).await).await;
    })
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
async fn run_file_node_impl(state: Arc<AppState>, id: String) -> Result<Response, ApiError> {
    let primary_raw = state.raw.lock().unwrap().clone();
    let located = locate_node(&state, &primary_raw, &id)?;
    let canvas = parse_or_error(&located.raw)?;
    let node = canvas
        .node(&located.local_id)
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
    let (interpreter_program, interpreter_args) =
        meshfox_core::split_interpreter(&interpreter).ok_or_else(|| {
            ApiError(
                StatusCode::UNPROCESSABLE_ENTITY,
                format!("node {id:?}'s interpreter={interpreter:?} isn't a valid shell-word command"),
            )
        })?;
    let target = node.target.as_deref().expect("checked by is_runnable_file");
    let canvas_path = located.origin.as_deref().unwrap_or(&state.canvas_path);
    let resolved_path = resolve_confined_target(canvas_path, target)?;
    // Same file `canvas_path` above already resolved to (the primary
    // document, or the `include` target this node actually lives in) —
    // its own directory is this node's `PWD`, not wherever `meshfox view`
    // itself happens to be running from.
    let node_cwd = canvas_root_dir(canvas_path).to_path_buf();

    let run_id = uuid::Uuid::new_v4().to_string();
    let (kill_tx, mut kill_rx) = oneshot::channel::<()>();
    state.runs.lock().unwrap().insert(run_id.clone(), kill_tx);

    let stream = async_stream::stream! {
        let _guard = RunGuard { state: Arc::clone(&state), run_id: run_id.clone() };
        yield Ok::<_, io::Error>(ndjson_line(&RunEvent::Started { run_id: run_id.clone() }));
        yield Ok(ndjson_line(&RunEvent::StepStart { node_id: id.clone(), block: id.clone() }));
        let step_started = std::time::Instant::now();

        let mut proc = match stream_exec::spawn_process(
            &interpreter_program,
            interpreter_args.iter().map(std::ffi::OsStr::new).chain([resolved_path.as_os_str()]),
            Some(&node_cwd),
        ) {
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
                        Some((stream, text)) => {
                            yield Ok(ndjson_line(&RunEvent::Output {
                                node_id: id.clone(),
                                block: id.clone(),
                                stream,
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

        let duration_ms = step_started.elapsed().as_millis() as u64;
        yield Ok(ndjson_line(&RunEvent::StepEnd { node_id: id.clone(), block: id.clone(), exit_code, duration_ms }));
        yield Ok(ndjson_line(&RunEvent::Done { exit_code }));
    };

    Ok(Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/x-ndjson")
        .body(Body::from_stream(stream))
        .unwrap())
}

/// Whether `path` (already resolved/confined on disk) counts as a canvas —
/// `meshfox_core::mdcanvas::is_canvas_path`'s cheap suffix check first, and
/// only falling back to actually reading the file (for a plain `.md` that
/// might carry the `meshfox:canvas` marker) when that's inconclusive, so a
/// `.png`/`.pdf`/etc. target never gets read just to be rejected.
fn is_canvas_file(path: &std::path::Path) -> bool {
    meshfox_core::mdcanvas::is_canvas_path(path)
        || (path.extension().is_some_and(|ext| ext == "md")
            && std::fs::read_to_string(path)
                .is_ok_and(|contents| meshfox_core::mdcanvas::has_marker(&contents)))
}

/// Opens a `file` node's target — the web UI's "↗ open" button. Always
/// `204`, fire-and-forget from the caller's own point of view (the client
/// never gets a URL — or a success/failure of the open itself — back to
/// act on any more, see `crate::watcher_protocol`'s own doc comment for
/// why): both a plain file and a `.canvas.md` (or marker-carrying `.md`)
/// target are handed to this worker's own coordinator
/// (`watcher_protocol::request_open`/`request_open_file`) — get-or-spawn-
/// and-show for a canvas, "open however this coordinator opens plain
/// files" for anything else, entirely that coordinator's decision either
/// way (the OS's default application for `crate::cli`'s own watcher and the
/// macOS daemon, a fresh editor tab for the VS Code extension's). No
/// coordinator reachable *is* the one thing this endpoint reports as a real
/// error, since without one there's nobody left to open anything at all.
async fn open_node_file(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let primary_raw = state.raw.lock().unwrap().clone();
    let located = locate_node(&state, &primary_raw, &id)?;
    let canvas = parse_or_error(&located.raw)?;
    let node = canvas
        .node(&located.local_id)
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
    let (target_path, fragment) = meshfox_core::mdcanvas::split_target_fragment(target);
    let canvas_path = located.origin.as_deref().unwrap_or(&state.canvas_path);
    let resolved = resolve_confined_target(canvas_path, target_path)?;

    let socket = state.watcher_socket.as_deref().ok_or_else(|| {
        ApiError(
            StatusCode::SERVICE_UNAVAILABLE,
            "no watcher to ask — this worker wasn't started with one, so opening files isn't \
             available"
                .to_string(),
        )
    })?;

    if is_canvas_file(&resolved) {
        watcher_protocol::request_open(socket, &resolved, fragment.map(str::to_string))
            .await
            .map_err(|e| {
                ApiError(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("couldn't reach the watcher to open the canvas: {e}"),
                )
            })?;
    } else {
        watcher_protocol::request_open_file(socket, &resolved)
            .await
            .map_err(|e| {
                ApiError(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("couldn't reach the watcher to open the file: {e}"),
                )
            })?;
    }

    Ok(StatusCode::NO_CONTENT)
}

/// Opens the directory containing a `file` node's target — the web UI's
/// "show folder" icon next to a file link. Same resolution/confinement as
/// `open_node_file` above, just handed the resolved target's parent
/// directory instead of the target itself, and always via
/// `watcher_protocol::request_open_file` (a containing folder is never a
/// canvas to get-or-spawn-and-show, even when the file inside it happens to
/// be one).
async fn open_node_file_folder(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let primary_raw = state.raw.lock().unwrap().clone();
    let located = locate_node(&state, &primary_raw, &id)?;
    let canvas = parse_or_error(&located.raw)?;
    let node = canvas
        .node(&located.local_id)
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
    let (target_path, _fragment) = meshfox_core::mdcanvas::split_target_fragment(target);
    let canvas_path = located.origin.as_deref().unwrap_or(&state.canvas_path);
    let resolved = resolve_confined_target(canvas_path, target_path)?;
    let folder = resolved.parent().ok_or_else(|| {
        ApiError(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("{}: has no containing directory", resolved.display()),
        )
    })?;

    let socket = state.watcher_socket.as_deref().ok_or_else(|| {
        ApiError(
            StatusCode::SERVICE_UNAVAILABLE,
            "no watcher to ask — this worker wasn't started with one, so opening files isn't \
             available"
                .to_string(),
        )
    })?;

    watcher_protocol::request_open_file(socket, folder)
        .await
        .map_err(|e| {
            ApiError(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("couldn't reach the watcher to open the folder: {e}"),
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
    // `reparent` changes every direct child's own `parent` too, not just
    // removing `id` — describing that fully would mean one `NodeUpserted`
    // per promoted child on top of the removal itself. Not worth it yet
    // for what's the less common of the two delete modes — falls back to
    // the generic `Changed` (a full reload), same as before this feature.
    let op = if reparent {
        ServerEvent::Changed
    } else {
        ServerEvent::NodeRemoved { node_id: local_id.clone(), keep_children: false }
    };
    commit_located(&state, &located, &updated, op)?;
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
                        created_at: new_node.created_at.clone(),
                    };
                    if let Some(patched) = mdcanvas::set_node_meta(&updated, local_id, &meta) {
                        updated = patched;
                    }
                }
            }
        }
    }
    let op = node_upserted_event(&updated, &state.canvas_path, local_id);
    commit_located(&state, &located, &updated, op)?;
    let response_raw = state.raw.lock().unwrap().clone();
    canvas_response(&response_raw, &state.canvas_path)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MoveSiblingRequest {
    /// Exactly one of these must be set — the id of the sibling to move
    /// `id` immediately next to.
    before: Option<String>,
    after: Option<String>,
}

/// Moves node `id`'s whole subtree to sit immediately before or after
/// another sibling under the same structural parent
/// (`mdcanvas::move_sibling`) — the web UI's up/down reorder buttons for
/// an auto-placed (unpositioned) node, and `meshfox node move`'s server
/// counterpart. Exactly one of `before`/`after` must be given. `404` if
/// either id doesn't exist; `422` if the request names neither or both
/// fields, or if the two nodes aren't siblings (same structural parent) —
/// moving to sit among a *different* parent's children is
/// `reparent_node`'s job, not this one's.
async fn move_sibling(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(req): Json<MoveSiblingRequest>,
) -> Result<Json<Canvas>, ApiError> {
    let (target_id, position) = match (req.before, req.after) {
        (Some(t), None) => (t, mdcanvas::MoveSiblingPosition::Before),
        (None, Some(t)) => (t, mdcanvas::MoveSiblingPosition::After),
        _ => {
            return Err(ApiError(
                StatusCode::UNPROCESSABLE_ENTITY,
                "exactly one of `before`/`after` must be set".to_string(),
            ));
        }
    };

    let primary_raw = state.raw.lock().unwrap().clone();
    let located = locate_node(&state, &primary_raw, &id)?;
    let located_target = locate_node(&state, &primary_raw, &target_id)?;
    // Same defensive check `reparent_node` makes: in practice this can't
    // actually happen, since `mdcanvas::move_sibling` requires the two
    // ids to share a structural parent, and a parent/child relationship
    // never crosses an include boundary — checked anyway rather than
    // relying on that invariant holding.
    if located.origin != located_target.origin {
        return Err(ApiError(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!(
                "can't move {id:?} relative to {target_id:?} — they live in different files"
            ),
        ));
    }

    let updated = mdcanvas::move_sibling(
        &located.raw,
        &located.local_id,
        &located_target.local_id,
        position,
    )
    .map_err(|e| {
        let status = match e {
            mdcanvas::MoveSiblingError::NotFound(_) => StatusCode::NOT_FOUND,
            mdcanvas::MoveSiblingError::NotSiblings(_, _) | mdcanvas::MoveSiblingError::SameNode => {
                StatusCode::UNPROCESSABLE_ENTITY
            }
        };
        ApiError(status, e.to_string())
    })?;
    // Sent as the *whole* new sibling order (not a "moved before/after"
    // delta) so a client never has to reconstruct one from the other —
    // see `ServerEvent::NodesReordered`'s own doc comment. `new_canvas.
    // nodes` is already in document order, which for siblings *is* tree
    // order, so this is just "every node whose `parent` matches, in the
    // order they already come back in" — no separate sort needed.
    let new_canvas = parse_or_error(&updated)?;
    let op = match new_canvas.node(&located.local_id).and_then(|n| n.parent.clone()) {
        Some(parent_id) => {
            let child_ids: Vec<String> = new_canvas
                .nodes
                .iter()
                .filter(|n| n.parent.as_deref() == Some(parent_id.as_str()))
                .map(|n| n.id.clone())
                .collect();
            ServerEvent::NodesReordered { parent_id, child_ids }
        }
        // No parent (the root) — `move_sibling` requires two siblings
        // under a common parent, so this can't actually happen; falls
        // back to a full reload rather than assuming.
        None => ServerEvent::Changed,
    };
    commit_located(&state, &located, &updated, op)?;
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
    // An id rename ripples into every other node's own `parent=`/
    // `meshfox:edge from=` reference plus any `deps=` fence text — too
    // much for `NodeUpserted`'s single-node shape to describe accurately;
    // falls back to the generic `Changed` (a full reload), same as before
    // this feature.
    commit_located(&state, &located, &updated, ServerEvent::Changed)?;
    let response_raw = state.raw.lock().unwrap().clone();
    canvas_response(&response_raw, &state.canvas_path)
}

#[derive(Debug, Serialize)]
struct ClearNodeIdResponse {
    /// The id node `id` (the path param) actually has *after* clearing —
    /// usually unchanged in practice (an untouched auto-generated id is
    /// already `slug(title)`, so there's nothing to rename, just the now-
    /// redundant attribute to drop), but potentially a freshly-derived
    /// slug if the title's since diverged. Composed with its include
    /// namespace already, same shape as every id in `canvas` — the client
    /// has no other way to learn it, since it isn't necessarily the id it
    /// asked to clear.
    id: String,
    canvas: Canvas,
}

/// Removes node `id`'s own explicit `id="..."` attribute
/// (`mdcanvas::clear_node_id`), handing it back to the parser's title-slug
/// fallback — the same rule a hand-written `meshfox:node` comment with no
/// `id=` at all already gets. `404` if `id` doesn't exist; can't otherwise
/// fail the way `rename_node_id` can (empty/invalid/colliding), since the
/// derived id is always a slug of the node's own already-valid title,
/// deduplicated against every other id already in the document.
async fn clear_node_id(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<ClearNodeIdResponse>, ApiError> {
    let primary_raw = state.raw.lock().unwrap().clone();
    let located = locate_node(&state, &primary_raw, &id)?;
    let (updated, local_new_id) = mdcanvas::clear_node_id(&located.raw, &located.local_id)
        .map_err(|e| ApiError(StatusCode::NOT_FOUND, e.to_string()))?;
    parse_or_error(&updated)?;
    // Same reasoning as `rename_node_id`: an id change ripples too far for
    // `NodeUpserted` — falls back to the generic `Changed`.
    commit_located(&state, &located, &updated, ServerEvent::Changed)?;
    let response_raw = state.raw.lock().unwrap().clone();
    let Json(canvas) = canvas_response(&response_raw, &state.canvas_path)?;
    let new_id = match &located.origin {
        None => local_new_id,
        Some(origin) => canvas
            .nodes
            .iter()
            .find(|n| {
                n.origin_id.as_deref() == Some(local_new_id.as_str())
                    && n.origin_path.as_deref() == origin.to_str()
            })
            .map(|n| n.id.clone())
            .unwrap_or(local_new_id),
    };
    Ok(Json(ClearNodeIdResponse { id: new_id, canvas }))
}

/// Query params for `run_block`'s own `GET /api/run` WebSocket upgrade —
/// same fields `RunRequest`'s JSON body used to carry, query-string-encoded
/// the same way `TtyRunQuery` already does for `/api/run/tty` (`vars`/
/// `saveSecrets` as JSON-stringified query values, since a `GET` upgrade has
/// no body to put them in).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RunWsQuery {
    #[serde(default)]
    path: String,
    block: String,
    #[serde(default)]
    no_deps: bool,
    #[serde(default)]
    persist: bool,
    #[serde(default)]
    vars: String,
    #[serde(default)]
    save_secrets: String,
}

/// Parses the query-string-encoded fields every run-starting WS endpoint
/// shares (`RunWsQuery`/`ForceRunWsQuery`) into the `RunRequest` `run_block_
/// impl` already expects — shared so the JSON-parsing/error-shape for `vars`/
/// `saveSecrets` lives in exactly one place despite the two query structs
/// themselves being separate (query-string `Deserialize` doesn't reliably
/// support `#[serde(flatten)]` the way a JSON body's `ForceRunRequest` used
/// to rely on, so `TtyRunQuery`'s own precedent — a flat, fully-duplicated
/// struct — is what `ForceRunWsQuery` below follows instead).
fn parse_run_request(
    path: &str,
    block: String,
    no_deps: bool,
    persist: bool,
    vars: &str,
    save_secrets: &str,
) -> Result<RunRequest, ApiError> {
    let path: Vec<String> = if path.is_empty() {
        Vec::new()
    } else {
        path.split(',').map(String::from).collect()
    };
    let vars: HashMap<String, String> = if vars.is_empty() {
        HashMap::new()
    } else {
        serde_json::from_str(vars)
            .map_err(|e| ApiError(StatusCode::BAD_REQUEST, format!("invalid `vars`: {e}")))?
    };
    let save_secrets: std::collections::HashSet<String> = if save_secrets.is_empty() {
        std::collections::HashSet::new()
    } else {
        serde_json::from_str(save_secrets)
            .map_err(|e| ApiError(StatusCode::BAD_REQUEST, format!("invalid `saveSecrets`: {e}")))?
    };
    Ok(RunRequest { path, block, persist, no_deps, vars, save_secrets })
}

/// `GET /api/run` — runs the requested block plus — automatically, same as
/// the CLI, unless `noDeps` is set — every block it transitively `deps=`-
/// depends on, in dependency order, stopping early if a step exits non-zero
/// (running what depends on a failed step wouldn't mean anything). A
/// WebSocket, not a plain HTTP-streamed response (see `pump_run_response_
/// into_ws`'s own doc comment for why): the socket always opens, and every
/// `RunEvent` — `Started` first on success, or a single `LockConflict`/
/// `Error` instead if the chain can't even start — arrives as a `Message::
/// Text` frame.
async fn run_block(
    ws: WebSocketUpgrade,
    State(state): State<Arc<AppState>>,
    Query(query): Query<RunWsQuery>,
) -> Response {
    ws.on_upgrade(move |socket| async move {
        let result = match parse_run_request(
            &query.path,
            query.block,
            query.no_deps,
            query.persist,
            &query.vars,
            &query.save_secrets,
        ) {
            Ok(req) => run_block_impl(state, req).await,
            Err(e) => Err(e),
        };
        pump_run_response_into_ws(socket, result).await;
    })
}

/// Query params for `force_run`'s own `GET /api/run/force` WebSocket
/// upgrade — `RunWsQuery`'s own fields (duplicated, not shared via
/// `#[serde(flatten)]` — see `parse_run_request`'s own doc comment) plus
/// `forceNodeId`/`forceBlock`, the exact `(nodeId, block)` a prior
/// `LockConflict` message named. Not necessarily the block the run itself
/// targets — a chain's own dependency can just as easily be the one that's
/// contested.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ForceRunWsQuery {
    #[serde(default)]
    path: String,
    block: String,
    #[serde(default)]
    no_deps: bool,
    #[serde(default)]
    persist: bool,
    #[serde(default)]
    vars: String,
    #[serde(default)]
    save_secrets: String,
    force_node_id: String,
    force_block: String,
}

/// The force-kill prep `force_run` does before its own `run_block_impl`
/// call, extracted so the WS wrapper below can run it *inside* the post-
/// upgrade closure — same "every pre-stream failure becomes a message, not
/// a rejected upgrade" reasoning `pump_run_response_into_ws` documents,
/// applied here too (a bad `force` address, or a failed
/// `kill_and_acquire`, used to be a plain HTTP error before the socket
/// ever opened).
async fn force_run_kill_prep(state: &AppState, force: &ForceTarget) -> Result<(), ApiError> {
    let raw_snapshot = state.raw.lock().unwrap().clone();
    let located = locate_node(state, &raw_snapshot, &force.node_id)?;
    let canvas_path_for_target = located.origin.unwrap_or_else(|| state.canvas_path.clone());
    let lock_path =
        meshfox_core::service_lock_path(&canvas_path_for_target, &force.node_id, &force.block);
    // Snapshot the stale owner's own descendants *before* killing it — see
    // `services::kill_orphaned_descendants`'s own doc comment: a tool that
    // daemonizes internally (forks, then the fork `setsid`s into its own
    // new process group) can leave a live process `kill_and_acquire`'s
    // whole-group `SIGKILL` below never reaches, because it was never a
    // member of that group to begin with — and once the *recorded* pid is
    // actually dead, anything it daemonized off may already have been
    // reparented away (to pid 1) and become unreachable this way, so this
    // has to run while there's still a chance the old owner (and whatever
    // it forked) is genuinely alive. A second sweep right after covers
    // anything that only shows up once the group-kill itself lands.
    let stale_pid = match meshfox_core::service_lock::check(&lock_path) {
        Ok(meshfox_core::service_lock::LockState::Held { info, .. }) => Some(info.pid),
        _ => None,
    };
    if let Some(pid) = stale_pid {
        services::kill_orphaned_descendants(pid);
    }

    // Claim it just long enough to prove the old owner is actually gone,
    // then release it again immediately — `run_block_impl` below runs its
    // own full up-front locking pass over the whole chain (this address
    // included) from scratch, and would otherwise conflict with *this*
    // call's own claim (an atomic `acquire` can't tell "already held by us,
    // moments ago, on purpose" from a genuine second claimant). A fresh
    // claimant slipping in during the brief gap is an acceptable, rare
    // race — the client's own cue to force again, same as any other
    // conflict `run_block_impl` might report.
    meshfox_core::service_lock::kill_and_acquire(&lock_path, std::process::id(), "webui")
        .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let _ = meshfox_core::service_lock::release(&lock_path);
    if let Some(pid) = stale_pid {
        services::kill_orphaned_descendants(pid);
    }
    Ok(())
}

/// `GET /api/run/force` — the generalized escape hatch for a `LockConflict`
/// `run_block`/`run_block_tty` reported: force-kills whatever the
/// conflict's own lock file names, then just re-runs the exact same
/// request that hit it. Only ever takes over *one* specific address at a
/// time — if the same chain turns out to conflict on a *different* address
/// too (a second concurrent run elsewhere in the chain, or a fresh race
/// since the first conflict was reported), this reports that as a fresh
/// `LockConflict` the same way `run_block_impl` always does, for the
/// client to force again.
async fn force_run(
    ws: WebSocketUpgrade,
    State(state): State<Arc<AppState>>,
    Query(query): Query<ForceRunWsQuery>,
) -> Response {
    ws.on_upgrade(move |socket| async move {
        let force = ForceTarget { node_id: query.force_node_id, block: query.force_block };
        let result: Result<Response, ApiError> = async {
            force_run_kill_prep(&state, &force).await?;
            let req = parse_run_request(
                &query.path,
                query.block,
                query.no_deps,
                query.persist,
                &query.vars,
                &query.save_secrets,
            )?;
            run_block_impl(state, req).await
        }
        .await;
        pump_run_response_into_ws(socket, result).await;
    })
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ForceTarget {
    node_id: String,
    block: String,
}

async fn run_block_impl(state: Arc<AppState>, req: RunRequest) -> Result<Response, ApiError> {
    let raw_snapshot = state.raw.lock().unwrap().clone();
    // Include-resolved (not just `parse_or_error`) so `path`/`block` below
    // can address a node spliced in from an `include` — its id in the
    // resolved tree is namespaced (`{include_id}/{original_id}`), same as
    // what `GET /api/canvas` already sends the browser, so a path/block
    // the UI read off that response resolves the same way here.
    let canvas = resolved_canvas(&raw_snapshot, &state.canvas_path)?;
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
    // happens to reference the same variable) doesn't ask again — as is a
    // `secret` one explicitly named in `req.save_secrets` (see that field's
    // own doc comment), still in plaintext (no encryption yet — see
    // TODO.canvas.md's own follow-up item on that).
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
    let overrides = effective_overrides(&state, &req.vars);
    let mut resolved_vars = {
        let mut cache = state.vars_cache.lock().unwrap();
        let shared = meshfox_core::load_shared_env(canvas_root_dir(&state.canvas_path));
        let resolved = meshfox_core::resolve_with_shared(
            &relevant_decls,
            &overrides,
            &cache,
            &HashMap::new(),
            &shared,
        );
        if !resolved.missing.is_empty() {
            let names: Vec<&str> = resolved.missing.iter().map(|d| d.name.as_str()).collect();
            return Err(ApiError(
                StatusCode::UNPROCESSABLE_ENTITY,
                format!("missing required variable(s): {}", names.join(", ")),
            ));
        }
        for (name, value) in &req.vars {
            if relevant_decls
                .iter()
                .any(|d| &d.name == name && (!d.secret || req.save_secrets.contains(name)) && !d.session)
            {
                let _ = cache.set(name, value);
            }
        }
        resolved.values
    };

    // Dry-run pass over the whole chain, up front, so a `!` (sync) `deps=`
    // edge (see `meshfox_core::fence::BlockRef::sync`) can force its
    // dependency to run for real when the block that declared the edge is
    // about to — that block comes *later* in `chain`'s dependency order, so
    // this can't be decided incrementally inside the loop below the way the
    // rest of the skip logic is. See `compute_forced_reruns`'s own doc
    // comment for what "dry run" means here and its one known gap.
    let forced_reruns = {
        let session_runs = state.session_runs.lock().unwrap();
        // The real loop below fingerprints against the *whole* accumulated
        // `resolved_vars` map, not a per-block-filtered subset (see its own
        // `session_fingerprint` call) — mirror that here: this dry run's
        // own seed plus whatever a skipped step's `produced_vars` folds in.
        meshfox_core::compute_forced_reruns(
            &canvas,
            &chain,
            |_block, sim_computed| {
                let mut vars = resolved_vars.clone();
                vars.extend(sim_computed.clone());
                vars
            },
            |addr| {
                session_runs
                    .get(&(addr.node_id.clone(), addr.block_name.clone()))
                    .map(|run| (run.fingerprint.clone(), run.produced_vars.clone()))
            },
        )
        .map_err(|e| ApiError(StatusCode::UNPROCESSABLE_ENTITY, e.to_string()))?
    };

    // Queued-time, transactional locking: claim every address this chain
    // will actually need to run *before* anything starts, all-or-nothing —
    // see `acquire_chain_locks`'s own doc comment. Nothing has been sent to
    // the client yet at this point, so a conflict is just an ordinary HTTP
    // response, not a streamed event.
    let lock_targets = steps_needing_a_lock(&state, &raw_snapshot, &chain, &forced_reruns);
    let mut held_locks = match acquire_chain_locks(&lock_targets, "webui") {
        Ok(locks) => locks,
        Err(ChainLockError::Conflict(conflict)) => return Ok(lock_conflict_response(&conflict)),
        Err(ChainLockError::Io(e)) => {
            return Err(ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
        }
    };

    // Reserves the requested block's own next-run identity in
    // `runs_registry` *now*, synchronously, before this function returns —
    // closes a race a passive watcher (an `autorun`-triggered run this tab
    // never itself started, see `trigger_autorun`/the web UI's
    // `watchAutorunBlock`) can otherwise hit: this chain's own dependencies
    // (a `deps=`-only readiness check, say) can take real wall-clock time
    // before this requested block's own step is ever reached in the loop
    // below, and `GET /api/run/subscribe` looks this exact address up in
    // `runs_registry` the moment it's asked — if that still held the
    // *previous* run's already-finished handle in the meantime, a
    // subscriber that raced ahead of this chain's own dependencies would
    // attach to stale, already-`Done` data and give up right there, never
    // learning the real (fresh) output this run goes on to produce
    // (confirmed directly against a real `form`+`autorun` block whose
    // target depends on a slow readiness check: the *first* Send after a
    // value change silently kept showing the previous value's own result,
    // and only a second, redundant Send — racing against an already-
    // finished first run instead of an in-flight one — happened to show
    // the right thing). Reserving this address's own next-run identity
    // *before* returning closes that window: a subscriber that races ahead
    // now finds an empty, still-`Running` placeholder to wait on instead of
    // a finished one to (wrongly) trust. `run_registry::attach`, below in
    // the loop, fills it in once a real process for this specific step
    // actually spawns; `ResolveReservationOnDrop` resolves it if the chain
    // never reaches that point at all (an earlier dependency failing and
    // breaking the loop, say), so an already-waiting subscriber isn't left
    // hanging on a `Running` outcome forever.
    let target_reservation = chain.last().map(|target| {
        let reserved = run_registry::reserve(target.node_id.clone(), target.block_name.clone());
        state
            .runs_registry
            .lock()
            .unwrap()
            .insert((target.node_id.clone(), target.block_name.clone()), Arc::clone(&reserved));
        // Broadcast right after reservation, not before — same
        // race-freedom reasoning as the reservation itself: a passive
        // watcher (`GET /api/watch`) that reacts to this by immediately
        // calling `GET /api/run/subscribe` needs `runs_registry` to
        // already hold this address's entry by the time this event
        // reaches it. Every caller of `run_block_impl` goes through here —
        // an ordinary manual run, `force_run`, and `trigger_autorun` alike
        // — so a passive tab/TUI session sees *any* rerun of a block it's
        // displaying, not just an autorun-triggered one (the only case
        // this event used to cover, back when `submit_form` broadcast it
        // itself after `trigger_autorun` returned).
        state.canvas_events.push(ServerEvent::RunStarted {
            node_id: target.node_id.clone(),
            block: target.block_name.clone(),
        });
        reserved
    });

    let run_id = uuid::Uuid::new_v4().to_string();
    let (kill_tx, mut kill_rx) = oneshot::channel::<()>();
    state.runs.lock().unwrap().insert(run_id.clone(), kill_tx);

    let stream = async_stream::stream! {
        // Dropped at the end of this block (however it ends — see
        // `RunGuard`'s own doc comment) to remove the registry entry.
        let _guard = RunGuard { state: Arc::clone(&state), run_id: run_id.clone() };
        // Whatever's still left in `held_locks` once this generator ends —
        // whether by reaching the bottom normally, `break`ing out of the
        // main loop early, or being dropped mid-stream by a client
        // disconnect — gets released here: a `service`/`tty` step that
        // actually started running is removed from this map (not released)
        // the moment it starts, so this only ever sweeps up addresses whose
        // turn never came, or that were mid-execution as a *plain* step
        // when the stream ended one way or another. Runs before `_guard`
        // (declared after it), same reverse-declaration-order `Drop` every
        // other guard here already relies on. `ReleaseRemainingLocks` is
        // shared with `run_tty_chain`'s own use of it, see that struct's
        // own doc comment.
        let mut _release_guard = ReleaseRemainingLocks(std::mem::take(&mut held_locks));
        let held_locks = &mut _release_guard.0;
        // See `target_reservation`'s own doc comment above — resolved here
        // if this generator ends (however it ends) without the requested
        // block's own step ever being reached below.
        let mut _reservation_guard = ResolveReservationOnDrop(target_reservation.clone());

        yield Ok::<_, io::Error>(ndjson_line(&RunEvent::Started { run_id: run_id.clone() }));

        let mut final_exit_code = 0;
        let mut killed = false;
        // Each chain step's own file — `None` for the primary document
        // (`state.canvas_path`/`state.raw`), `Some(path)` for a node
        // spliced in from an `include` elsewhere on disk — keyed the same
        // way `locate_node`'s own `LocatedNode::origin` is, and populated
        // lazily as each file is actually touched, in the same "read
        // fresh once, then keep this run's own freshly-patched copy for
        // any later step in the same file" shape `raw` alone used to be
        // for the primary-only case. Never contains an entry for a file
        // no step in this chain actually caches output into.
        let mut file_raws: HashMap<Option<PathBuf>, String> = HashMap::new();

        for addr in &chain {
            yield Ok(ndjson_line(&RunEvent::StepStart {
                node_id: addr.node_id.clone(),
                block: addr.block_name.clone(),
            }));

            let primary_raw_now = file_raws
                .get(&None)
                .cloned()
                .unwrap_or_else(|| state.raw.lock().unwrap().clone());
            let mut located = match locate_node(&state, &primary_raw_now, &addr.node_id) {
                Ok(l) => l,
                Err(e) => {
                    yield Ok(ndjson_line(&RunEvent::Error { message: e.1 }));
                    break;
                }
            };
            // An earlier step in *this* chain may have already patched
            // this exact file's own cache (below) — `locate_node` itself
            // has no way to know that (it always reads a non-primary
            // file fresh off disk), so override with this run's own copy
            // when there is one, same reasoning `raw` alone used to
            // carry across iterations before per-file tracking existed.
            if let Some(cached) = file_raws.get(&located.origin) {
                located.raw = cached.clone();
            }

            // Re-parse so an earlier step's freshly-patched cache (above)
            // is visible before this one runs — same reasoning `meshfox
            // run`'s CLI loop already has.
            let node_text = match Canvas::from_markdown(&located.raw)
                .ok()
                .and_then(|c| c.node(&located.local_id).map(|n| n.text.clone()))
            {
                Some(text) => text,
                None => {
                    yield Ok(ndjson_line(&RunEvent::Error {
                        message: format!("node {:?} not found", addr.node_id),
                    }));
                    break;
                }
            };
            let canvas_path_for_step = located.origin.as_deref().unwrap_or(&state.canvas_path);
            let cwd = canvas_root_dir(canvas_path_for_step).to_path_buf();
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
            if !stream_exec::supports(&block) {
                yield Ok(ndjson_line(&RunEvent::Error {
                    message: format!("no executor registered for language {:?}", block.lang),
                }));
                break;
            }

            // The block actually requested (always the chain's own last
            // entry — see `resolve_run_chain`'s doc comment) always runs
            // for real; only a pulled-in dependency is ever eligible to be
            // skipped as "already fresh this session" — see
            // `AppState::session_runs`. A block's own `always` flag opts it
            // out of the skip entirely, even as a pulled-in dependency —
            // for a step whose side effect isn't captured by "looks
            // unchanged" (a migration that always drops and recreates a
            // table, say). `forced_reruns` (computed once, up front — see
            // above) adds a third way in: a `!` `deps=` edge whose
            // declaring block is itself running for real this pass.
            let is_requested_target = Some(addr) == chain.last();
            let live_fingerprint = meshfox_core::session_fingerprint(&block, &resolved_vars);
            if !is_requested_target && !block.always && !forced_reruns.contains(addr) {
                let already_fresh = state
                    .session_runs
                    .lock()
                    .unwrap()
                    .get(&(addr.node_id.clone(), addr.block_name.clone()))
                    .filter(|run| run.fingerprint == live_fingerprint)
                    .cloned();
                if let Some(session_run) = already_fresh {
                    // Whatever this block wrote to its own vars-out file the
                    // last time it *actually* ran still applies unchanged
                    // (the block itself hasn't) — folded in exactly as if it
                    // had just run again, so a later step that declared
                    // `from=` this one still resolves.
                    resolved_vars.extend(session_run.produced_vars);
                    yield Ok(ndjson_line(&RunEvent::StepSkipped {
                        node_id: addr.node_id.clone(),
                        block: addr.block_name.clone(),
                        output: session_run.output,
                        duration_ms: session_run.duration_ms,
                    }));
                    continue;
                }
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
            // `spawn_block` reads `interpreter` off the `CodeBlock` it's
            // given rather than as a separate parameter — a
            // block-with-substituted-interpreter clone is how a `$NAME`
            // reference (`meshfox_core::interpreter_var_refs`) actually
            // reaches it, already resolved against this chain's own
            // `resolved_vars` (which `env_var_names_for_chain` already
            // makes sure includes whatever `interpreter=` itself needs).
            let mut resolved_block = block.clone();
            if let Some(spec) = &block.interpreter {
                resolved_block.interpreter = Some(meshfox_core::resolve_interpreter(spec, &resolved_vars));
            }

            // `service` blocks branch out here, before the normal
            // spawn-and-wait-for-exit path below: "done" for a service is
            // "spawned", not "exited" — see SPEC.md's "Service blocks
            // (experimental)" and `crate::services`'s own module doc
            // comment for the persistent registry this populates.
            if resolved_block.service {
                let key = (addr.node_id.clone(), addr.block_name.clone());
                let already_running = state
                    .services
                    .lock()
                    .unwrap()
                    .get(&key)
                    .filter(|s| matches!(s.status(), services::ServiceStatus::Running))
                    .map(|s| s.pid);
                if let Some(pid) = already_running {
                    // This process already owns a live instance of this
                    // exact service (e.g. a "run chain" that pulls it in
                    // as a dependency, run twice) — nothing to do, just
                    // report it as started again. `steps_needing_a_lock`
                    // already excluded this address from `held_locks` for
                    // exactly this reason — its own lock, held since it was
                    // first spawned, is untouched by this request.
                    yield Ok(ndjson_line(&RunEvent::ServiceStarted {
                        node_id: addr.node_id.clone(),
                        block: addr.block_name.clone(),
                        pid,
                    }));
                    continue;
                }

                // The lock for this address was already claimed up front
                // (`acquire_chain_locks`, before this response even
                // started) — `services::spawn` no longer acquires it
                // itself (see that function's own doc comment), so this
                // just spawns and hands ownership of the lock's lifetime to
                // the new `ServiceHandle` (removed from `held_locks`
                // without releasing — `ServiceHandle::stop`/its own crash
                // detection release it from here on).
                match services::spawn(
                    addr.node_id.clone(),
                    addr.block_name.clone(),
                    resolved_block.clone(),
                    block_env.clone(),
                    cwd.clone(),
                    canvas_path_for_step.to_path_buf(),
                    "webui",
                ) {
                    Ok(handle) => {
                        let pid = handle.pid;
                        held_locks.remove(&key);
                        state.services.lock().unwrap().insert(key, handle);
                        yield Ok(ndjson_line(&RunEvent::ServiceStarted {
                            node_id: addr.node_id.clone(),
                            block: addr.block_name.clone(),
                            pid,
                        }));
                    }
                    Err(e) => {
                        if let Some(path) = held_locks.remove(&key) {
                            let _ = meshfox_core::service_lock::release(&path);
                        }
                        yield Ok(ndjson_line(&RunEvent::Error { message: e.to_string() }));
                        break;
                    }
                }
                continue;
            }

            let step_started = std::time::Instant::now();
            let proc = match stream_exec::spawn_block(
                &resolved_block,
                &block_env,
                Some(&cwd),
                Some(canvas_path_for_step),
            ) {
                Ok(p) => p,
                Err(e) => {
                    if let Some(path) = held_locks.remove(&(addr.node_id.clone(), addr.block_name.clone())) {
                        let _ = meshfox_core::service_lock::release(&path);
                    }
                    yield Ok(ndjson_line(&RunEvent::Error { message: e.to_string() }));
                    break;
                }
            };
            // This address's lock (if `steps_needing_a_lock` claimed one)
            // was necessarily acquired with a placeholder pid before this
            // process existed — correct it now so a concurrent force-run
            // against it targets the real child, not whatever placeholder
            // won the original race.
            if let Some(path) = held_locks.get(&(addr.node_id.clone(), addr.block_name.clone())) {
                let _ = meshfox_core::service_lock::update_owner_pid(path, proc.child.id().unwrap_or(0));
            }

            let mut full_output = String::new();
            let mut stdout_only = String::new();
            let mut stderr_only = String::new();
            // Registering this run — instead of draining `proc` directly
            // here — is what lets its output survive this one connection:
            // a reconnect or a second tab can subscribe to the exact same
            // `run_registry::RunHandle` via `/api/run/subscribe` and see
            // everything from wherever it left off, not just whatever this
            // one stream happens to relay. This connection is itself just
            // the *first* subscriber, from seq 0 (nothing buffered yet).
            //
            // This address's lock (if `steps_needing_a_lock` claimed one)
            // is handed to `track` below rather than released here once
            // this step's own `StepEnd` is reached — the process it
            // protects now outlives this one connection, so its lock has
            // to too: releasing it whenever *this generator* happens to
            // end (a client disconnect long before the real process
            // exits, say) would free the address while the process is
            // still genuinely running under it, letting a second, truly
            // concurrent request start right on top of it. `track`'s own
            // background task releases it once the process actually ends,
            // regardless of who is or isn't still watching.
            let lock_path_for_run =
                held_locks.remove(&(addr.node_id.clone(), addr.block_name.clone()));
            // The requested block's own step reuses the reservation made
            // before this stream even started (see `target_reservation`'s
            // own doc comment) — a subscriber that raced ahead of this
            // chain's own dependencies and is already waiting on it needs
            // to see *this* process's own output, not a brand-new handle
            // it never had a chance to find. Any other (pulled-in
            // dependency) step has no such reservation and just tracks a
            // fresh handle as before.
            let run_handle = match (is_requested_target, &target_reservation) {
                (true, Some(reserved)) => {
                    run_registry::attach(reserved, proc, lock_path_for_run);
                    _reservation_guard.0 = None;
                    Arc::clone(reserved)
                }
                _ => run_registry::track(
                    addr.node_id.clone(),
                    addr.block_name.clone(),
                    proc,
                    lock_path_for_run,
                ),
            };
            state.runs_registry.lock().unwrap().insert(
                (addr.node_id.clone(), addr.block_name.clone()),
                Arc::clone(&run_handle),
            );
            let (_backlog, mut run_rx) = run_handle.subscribe_from(0);
            let mut kill_requested = false;
            let exit_code = loop {
                tokio::select! {
                    event = run_rx.recv() => {
                        match event {
                            Ok(run_registry::RunEvent::Line(line)) => {
                                full_output.push_str(&line.text);
                                full_output.push('\n');
                                match line.stream {
                                    stream_exec::OutputStream::Stdout => {
                                        stdout_only.push_str(&line.text);
                                        stdout_only.push('\n');
                                    }
                                    stream_exec::OutputStream::Stderr => {
                                        stderr_only.push_str(&line.text);
                                        stderr_only.push('\n');
                                    }
                                }
                                yield Ok(ndjson_line(&RunEvent::Output {
                                    node_id: addr.node_id.clone(),
                                    block: addr.block_name.clone(),
                                    stream: line.stream,
                                    text: line.text,
                                }));
                            }
                            Ok(run_registry::RunEvent::Done(run_registry::RunOutcome::Exited { exit_code })) => {
                                break exit_code;
                            }
                            Ok(run_registry::RunEvent::Done(run_registry::RunOutcome::Killed)) => {
                                yield Ok(ndjson_line(&RunEvent::Killed {
                                    node_id: addr.node_id.clone(),
                                    block: addr.block_name.clone(),
                                }));
                                killed = true;
                                break -1;
                            }
                            // `Done` never actually carries `Running` (see
                            // `run_registry::track`'s own loop) and a
                            // dropped/lagged channel only happens if this
                            // subscription itself got badly behind — either
                            // way there's nothing meaningful left to relay.
                            Ok(run_registry::RunEvent::Done(run_registry::RunOutcome::Running)) | Err(_) => {
                                break -1;
                            }
                        }
                    }
                    // Only armed once — after firing, this connection's own
                    // kill request has been delivered to the registry
                    // (which every subscriber, not just this one, will see
                    // resolve as `Done(Killed)` above); re-arming it would
                    // just fire again immediately for no reason.
                    _ = &mut kill_rx, if !kill_requested => {
                        kill_requested = true;
                        run_handle.kill();
                    }
                }
            };
            if killed {
                break;
            }

            let duration_ms = step_started.elapsed().as_millis() as u64;
            yield Ok(ndjson_line(&RunEvent::StepEnd {
                node_id: addr.node_id.clone(),
                block: addr.block_name.clone(),
                exit_code,
                duration_ms,
            }));
            final_exit_code = exit_code;
            // This step's own lock (if any) was already handed off to
            // `run_handle`'s own background task above — nothing left to
            // release here (see that call site's own comment on why).

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
                let result = ExecOutput {
                    exit_code,
                    output: full_output.clone(),
                    duration_ms,
                    stdout: stdout_only,
                    stderr: stderr_only,
                };
                if let Some(updated) = meshfox_core::write_output(&node_text, &addr.block_name, &result) {
                    if let Some(patched) = mdcanvas::set_node_body(&located.raw, &located.local_id, &updated) {
                        file_raws.insert(located.origin.clone(), patched);
                    }
                }
            }

            if exit_code == 0 && !from_value_error {
                let produced_vars = from_decls
                    .iter()
                    .filter_map(|decl| resolved_vars.get(&decl.name).map(|v| (decl.name.clone(), v.clone())))
                    .collect();
                state.session_runs.lock().unwrap().insert(
                    (addr.node_id.clone(), addr.block_name.clone()),
                    SessionRun { fingerprint: live_fingerprint, produced_vars, output: full_output, duration_ms },
                );
            }

            if exit_code != 0 || from_value_error {
                break;
            }
        }

        // Persist whatever completed, even if the chain was killed partway
        // through — a step that had already finished and been folded into
        // `file_raws` (above) shouldn't lose its freshly-cached output
        // just because a *later* step in the same chain got killed. Every
        // touched file is persisted, not just the primary one — a step
        // that lives in an `include` target writes straight to that
        // file's own path (mirrors `commit_located`).
        if persist {
            for (origin, content) in file_raws {
                let result = match &origin {
                    None => state.save(&content),
                    Some(path) => std::fs::write(path, &content),
                };
                if let Err(e) = result {
                    yield Ok(ndjson_line(&RunEvent::Error { message: e.to_string() }));
                }
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

/// Runs `addr`'s own full chain, unattended — triggered by `submit_form`
/// for every `autorun`-flagged block a just-submitted form's values affect
/// (see `meshfox_core::autorun_blocks_for_changed_vars`). Reuses
/// `run_block_impl` wholesale (session-freshness skip, locking, `cache`
/// write-back, everything) rather than duplicating any of its chain-
/// execution logic — the only new part is building a synthetic
/// `RunRequest` addressed via `Canvas::id_path_to` (the inverse of the
/// path-walk `run_block_impl` itself does to resolve `path`+`block`, since
/// `addr` is already a flat, resolved address with no path of its own —
/// see this function's own call site for why a hand-rolled
/// `vec![addr.node_id]` would be wrong for anything but a root-level
/// node).
///
/// Deliberately `.await`ed by `submit_form` itself (not `tokio::spawn`ed
/// wholesale) up through the *first* `run_block_impl` call — that's the
/// call that (barring a lock conflict, the rare case handled below)
/// synchronously reserves this address's own next-run identity in
/// `runs_registry` (see `run_block_impl`'s own `target_reservation` doc
/// comment) before this function returns at all. Only once that's settled
/// does the actual chain execution get handed off to a background task —
/// confirmed directly that skipping this and backgrounding the whole
/// thing (the original shape here) reopens exactly the race
/// `target_reservation` exists to close: a subscriber that calls
/// `/api/run/subscribe` right after `submit_form`'s own HTTP response
/// (which is every passive `autorun` watcher, having no stream of its own
/// to wait on first) could still race ahead of even a *scheduling* delay
/// before the spawned task's own `run_block_impl` call ever ran, not just
/// the execution-time delay the reservation itself was built to survive.
///
/// Always `persist: false`, even for a `cache` block: a variable changing
/// through a form submission is not the same thing as a user clicking
/// "Edit" and explicitly asking for the canvas file itself to be
/// rewritten — see SPEC.md's "Form fences".
/// How many times a `409` lock conflict is retried before giving up on
/// that one trigger — see the retry loop's own doc comment for why a
/// conflict here is expected to be transient far more often than a
/// user-initiated run's own conflict is.
const AUTORUN_CONFLICT_RETRIES: u32 = 20;
const AUTORUN_CONFLICT_RETRY_DELAY: Duration = Duration::from_millis(100);

async fn trigger_autorun(state: Arc<AppState>, addr: meshfox_core::BlockAddr) {
    let raw = state.raw.lock().unwrap().clone();
    let Ok(canvas) = resolved_canvas(&raw, &state.canvas_path) else {
        return;
    };
    let Some(path) = canvas.id_path_to(&addr.node_id) else {
        return;
    };
    let build_req = move || RunRequest {
        path: path.clone(),
        block: addr.block_name.clone(),
        persist: false,
        no_deps: false,
        vars: HashMap::new(),
        save_secrets: std::collections::HashSet::new(),
    };

    let Ok(response) = run_block_impl(Arc::clone(&state), build_req()).await else {
        return;
    };
    if response.status() != StatusCode::CONFLICT {
        drain_autorun_response_in_background(response);
        return;
    }

    // Conflicted on this very first, synchronously-awaited attempt — from
    // here on, retrying is fully backgrounded again: `submit_form`'s own
    // caller only needs the reservation guarantee above for the common
    // uncontended case, and a *contended* address is rare enough (see the
    // loop's own doc comment on why) that it staying exactly as racy as it
    // always has been is an acceptable trade for not blocking every
    // form's own Send on a rare retry loop's worst case (up to
    // `AUTORUN_CONFLICT_RETRIES * AUTORUN_CONFLICT_RETRY_DELAY`).
    tokio::spawn(async move {
        // A chain-lock conflict on this exact address (`acquire_chain_locks`,
        // inside `run_block_impl`) is expected to be transient far more
        // often than a user-initiated run's own conflict is: the most
        // likely cause is this *same* block's own previous autorun-
        // triggered run (or a manual run of it) still finishing up, which
        // releases the lock itself within moments of its own chain ending
        // — not a stuck process someone needs to confirm force-killing,
        // the way a person watching a `409` dialog would decide. Unlike a
        // manual run, there's no one watching this trigger to ask, so
        // retry a few times, briefly, rather than silently dropping a
        // form's own Send on the floor the moment two submissions land
        // close together (e.g. resubmitting again right after the
        // previous table finished rendering, before the server's own
        // chain-execution task has actually returned and released its
        // lock — see `web/e2e/form-autorun-output.spec.ts`, written
        // specifically to repeat a submission and catch this).
        for attempt in 1..AUTORUN_CONFLICT_RETRIES {
            tokio::time::sleep(AUTORUN_CONFLICT_RETRY_DELAY).await;
            let Ok(response) = run_block_impl(Arc::clone(&state), build_req()).await else {
                return;
            };
            if response.status() == StatusCode::CONFLICT {
                if attempt + 1 < AUTORUN_CONFLICT_RETRIES {
                    continue;
                }
                return;
            }
            let mut body = response.into_body().into_data_stream();
            while futures_util::StreamExt::next(&mut body).await.is_some() {}
            return;
        }
    });
}

/// Drives a non-conflict `run_block_impl` response to completion without
/// anyone reading its body — nothing inside `run_block_impl`'s own
/// `async_stream::stream!` executes until the stream is actually polled,
/// so a response nobody drains would otherwise just sit there having done
/// nothing. Backgrounded: `trigger_autorun`'s own caller only needed this
/// response to exist (see its doc comment), not to finish.
fn drain_autorun_response_in_background(response: Response) {
    tokio::spawn(async move {
        let mut body = response.into_body().into_data_stream();
        while futures_util::StreamExt::next(&mut body).await.is_some() {}
    });
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SubmitFormRequest {
    /// The form's own owning node, addressed by flat id — see
    /// `FormFieldsQuery::node_id`'s own doc comment for why this (unlike
    /// `RunRequest`) addresses by id rather than a root-relative path.
    node_id: String,
    block: String,
    /// Raw field values the client is submitting — only entries whose key
    /// matches one of this *specific* form's own `field var=` names are
    /// ever looked at; everything else is silently ignored, same posture
    /// `post_configure_vars` already takes toward an unrecognized key.
    /// Never trusted blindly: the form's own field list is re-derived here
    /// from the canvas itself, not from anything the client claims.
    values: HashMap<String, String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct TriggeredRun {
    node_id: String,
    block: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SubmitFormResponse {
    saved: usize,
    /// Every `autorun` block this submission just kicked off, so the
    /// client can start watching each one (`GET /api/run/subscribe`)
    /// immediately rather than waiting on the `run-started` event over
    /// `/api/watch` — see `ServerEvent::RunStarted`'s own doc comment for
    /// why that event exists too (a *different* tab, one that didn't
    /// submit this form itself, still needs a way to find out).
    autorun_triggered: Vec<TriggeredRun>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FormFieldsQuery {
    /// The form's own owning node, addressed by flat id (the same
    /// possibly-`{include_id}/{original_id}`-namespaced id `GET
    /// /api/canvas` already sends the browser) — not a root-relative
    /// `path` the way `RunRequest`/`VarsQuery` address a block, since
    /// neither of this endpoint (nor `submit_form`) ever needs to know
    /// which real file on disk owns the node (they never write to the
    /// canvas file at all) — just to read its current declarations out of
    /// the already-include-resolved `canvas`.
    node_id: String,
    block: String,
}

/// One `field var=...` entry, with its declared variable's current
/// display status (`VarStatus`, same shape `GET /api/vars` already uses)
/// plus whatever `label=` override the field itself carries.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct FormFieldStatus {
    #[serde(skip_serializing_if = "Option::is_none")]
    label: Option<String>,
    #[serde(flatten)]
    var: VarStatus,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct FormFieldsResponse {
    /// `send="..."` caption, defaulted to a plain `"Send"` here (rather
    /// than leaving it `None` for the client to default) so every
    /// consumer agrees on the fallback.
    send: String,
    fields: Vec<FormFieldStatus>,
}

/// `GET /api/form/fields` — resolves one `form`-lang fence's own `field
/// var=` list into display-ready status, server-side (same defensive
/// posture `submit_form` takes: the client names a block, never a field
/// list of its own). Scoped to exactly this one form, unlike `GET
/// /api/vars`/`/api/vars/configure` — a form only ever shows its own
/// declared fields, not a whole chain's or the whole document's.
async fn get_form_fields(
    State(state): State<Arc<AppState>>,
    Query(query): Query<FormFieldsQuery>,
) -> Result<Json<FormFieldsResponse>, ApiError> {
    let raw = state.raw.lock().unwrap().clone();
    let canvas = resolved_canvas(&raw, &state.canvas_path)?;
    let node = canvas.node(&query.node_id).ok_or_else(|| {
        ApiError(StatusCode::NOT_FOUND, format!("no node {:?}", query.node_id))
    })?;
    let block = meshfox_core::scan_runnable_blocks(&node.id, &node.text)
        .into_iter()
        .find(|b| b.name.as_deref() == Some(query.block.as_str()))
        .ok_or_else(|| {
            ApiError(
                StatusCode::NOT_FOUND,
                format!(
                    "no runnable block named {:?} in node {:?}",
                    query.block, node.id
                ),
            )
        })?;
    if !meshfox_core::is_form(&block.lang) {
        return Err(ApiError(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!(
                "block {:?} in node {:?} isn't a `form` fence",
                query.block, node.id
            ),
        ));
    }
    let form = meshfox_core::form_block(&block)
        .map_err(|e| ApiError(StatusCode::UNPROCESSABLE_ENTITY, e.to_string()))?;
    let decls = meshfox_core::declared_vars(&canvas)
        .map_err(|e| ApiError(StatusCode::UNPROCESSABLE_ENTITY, e.to_string()))?;

    // The form's own fields, in order, paired with their own `label=` (if
    // any) — a `field var=` naming something that somehow isn't declared
    // (shouldn't happen past `meshfox validate`, but this endpoint doesn't
    // re-run that check) is just skipped rather than erroring the whole
    // form out.
    let mut field_decls: Vec<meshfox_core::VarDecl> = Vec::new();
    let mut labels: Vec<Option<String>> = Vec::new();
    for field in &form.fields {
        if let Some(decl) = decls.iter().find(|d| d.name == field.var) {
            field_decls.push(decl.clone());
            labels.push(field.label.clone());
        }
    }

    // Same reasoning as `get_vars`: a `choices_var`/`default_var` chain
    // reaching a `from=`-computed variable needs that variable's own
    // source block actually run to show real choices.
    let computed =
        materialize_choices_and_defaults(&canvas, &decls, &field_decls, &state.canvas_path).await;
    let closure =
        meshfox_core::close_over_var_refs(&decls, field_decls.iter().map(|d| d.name.as_str()));
    let decls_for_resolve: Vec<_> = decls
        .iter()
        .filter(|d| closure.contains(d.name.as_str()))
        .cloned()
        .collect();
    let cache = state.vars_cache.lock().unwrap();
    let shared = meshfox_core::load_shared_env(canvas_root_dir(&state.canvas_path));
    let overrides = effective_overrides(&state, &HashMap::new());
    let resolved = meshfox_core::resolve_with_shared(
        &decls_for_resolve,
        &overrides,
        &cache,
        &computed,
        &shared,
    );
    let missing_by_name: HashMap<&str, &meshfox_core::VarDecl> =
        resolved.missing.iter().map(|d| (d.name.as_str(), d)).collect();
    let fields = field_decls
        .into_iter()
        .zip(labels)
        .map(|(d, label)| {
            let materialized = missing_by_name
                .get(d.name.as_str())
                .map(|m| (*m).clone())
                .unwrap_or(d);
            FormFieldStatus {
                label,
                var: var_status(materialized, &resolved),
            }
        })
        .collect();
    Ok(Json(FormFieldsResponse {
        send: form.send.unwrap_or_else(|| "Send".to_string()),
        fields,
    }))
}

/// `POST /api/form/submit` — the submit side of a `form`-lang fence's own
/// Send button (see SPEC.md's "Form fences"): commits `req.values` into
/// `AppState::session_vars` (never the on-disk
/// `vars_cache` — every variable a form targets is implicitly `session`,
/// see `meshfox_core::declared_vars`), then kicks off every `autorun`
/// block whose own variable closure the just-changed values actually
/// reach (`meshfox_core::autorun_blocks_for_changed_vars`).
async fn submit_form(
    State(state): State<Arc<AppState>>,
    Json(req): Json<SubmitFormRequest>,
) -> Result<Json<SubmitFormResponse>, ApiError> {
    let raw = state.raw.lock().unwrap().clone();
    let canvas = resolved_canvas(&raw, &state.canvas_path)?;
    let node = canvas
        .node(&req.node_id)
        .ok_or_else(|| ApiError(StatusCode::NOT_FOUND, format!("no node {:?}", req.node_id)))?;
    let block = meshfox_core::scan_runnable_blocks(&node.id, &node.text)
        .into_iter()
        .find(|b| b.name.as_deref() == Some(req.block.as_str()))
        .ok_or_else(|| {
            ApiError(
                StatusCode::NOT_FOUND,
                format!(
                    "no runnable block named {:?} in node {:?}",
                    req.block, node.id
                ),
            )
        })?;
    if !meshfox_core::is_form(&block.lang) {
        return Err(ApiError(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!(
                "block {:?} in node {:?} isn't a `form` fence",
                req.block, node.id
            ),
        ));
    }
    let form = meshfox_core::form_block(&block)
        .map_err(|e| ApiError(StatusCode::UNPROCESSABLE_ENTITY, e.to_string()))?;
    let decls = meshfox_core::declared_vars(&canvas)
        .map_err(|e| ApiError(StatusCode::UNPROCESSABLE_ENTITY, e.to_string()))?;
    let decls_by_name: HashMap<&str, &meshfox_core::VarDecl> =
        decls.iter().map(|d| (d.name.as_str(), d)).collect();

    let mut changed = std::collections::HashSet::new();
    let mut saved = 0usize;
    {
        let mut session_vars = state.session_vars.lock().unwrap();
        for field in &form.fields {
            let Some(value) = req.values.get(&field.var) else {
                continue;
            };
            let Some(decl) = decls_by_name.get(field.var.as_str()) else {
                continue;
            };
            meshfox_core::validate_value(decl, value)
                .map_err(|e| ApiError(StatusCode::UNPROCESSABLE_ENTITY, e))?;
            session_vars.insert(field.var.clone(), value.clone());
            changed.insert(field.var.clone());
            saved += 1;
        }
    }

    let triggered = meshfox_core::autorun_blocks_for_changed_vars(&canvas, &changed);
    for addr in &triggered {
        // Awaited (not `tokio::spawn`ed away) — see `trigger_autorun`'s
        // own doc comment for why this needs to run synchronously far
        // enough to reserve the address before this handler returns. The
        // `RunStarted` broadcast itself now happens inside
        // `run_block_impl` (see its own `target_reservation` doc comment)
        // — `trigger_autorun` calls that for every triggered address, so
        // there's nothing left to broadcast here.
        trigger_autorun(Arc::clone(&state), addr.clone()).await;
    }

    Ok(Json(SubmitFormResponse {
        saved,
        autorun_triggered: triggered
            .into_iter()
            .map(|a| TriggeredRun {
                node_id: a.node_id,
                block: a.block_name,
            })
            .collect(),
    }))
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
    /// JSON-encoded array of secret variable names to persist anyway — same
    /// meaning as `RunRequest::save_secrets`, just query-string-encoded
    /// (like `vars` itself) since this is a `GET` for the WebSocket
    /// upgrade, not a JSON body.
    #[serde(default)]
    save_secrets: String,
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
    // Include-resolved for the same reason `run_block` is — see its own
    // doc comment on the equivalent line.
    let canvas = resolved_canvas(&raw_snapshot, &state.canvas_path)?;
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
    let save_secrets: std::collections::HashSet<String> = if query.save_secrets.is_empty() {
        std::collections::HashSet::new()
    } else {
        serde_json::from_str(&query.save_secrets)
            .map_err(|e| ApiError(StatusCode::BAD_REQUEST, format!("invalid `saveSecrets`: {e}")))?
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
    let overrides = effective_overrides(&state, &requested_vars);
    let resolved_vars = {
        let mut cache = state.vars_cache.lock().unwrap();
        let shared = meshfox_core::load_shared_env(canvas_root_dir(&state.canvas_path));
        let resolved = meshfox_core::resolve_with_shared(
            &relevant_decls,
            &overrides,
            &cache,
            &HashMap::new(),
            &shared,
        );
        if !resolved.missing.is_empty() {
            let names: Vec<&str> = resolved.missing.iter().map(|d| d.name.as_str()).collect();
            return Err(ApiError(
                StatusCode::UNPROCESSABLE_ENTITY,
                format!("missing required variable(s): {}", names.join(", ")),
            ));
        }
        for (name, value) in &requested_vars {
            if relevant_decls
                .iter()
                .any(|d| &d.name == name && (!d.secret || save_secrets.contains(name)) && !d.session)
            {
                let _ = cache.set(name, value);
            }
        }
        resolved.values
    };

    // Same up-front dry-run pass `run_block` does — see its own doc
    // comment on the equivalent line.
    let forced_reruns = {
        let session_runs = state.session_runs.lock().unwrap();
        // The real loop below fingerprints against the *whole* accumulated
        // `resolved_vars` map, not a per-block-filtered subset (see its own
        // `session_fingerprint` call) — mirror that here: this dry run's
        // own seed plus whatever a skipped step's `produced_vars` folds in.
        meshfox_core::compute_forced_reruns(
            &canvas,
            &chain,
            |_block, sim_computed| {
                let mut vars = resolved_vars.clone();
                vars.extend(sim_computed.clone());
                vars
            },
            |addr| {
                session_runs
                    .get(&(addr.node_id.clone(), addr.block_name.clone()))
                    .map(|run| (run.fingerprint.clone(), run.produced_vars.clone()))
            },
        )
        .map_err(|e| ApiError(StatusCode::UNPROCESSABLE_ENTITY, e.to_string()))?
    };

    // Same queued-time, transactional locking `run_block` does — see its
    // own doc comment on the equivalent line. Still entirely pre-upgrade,
    // so a conflict is a plain HTTP response, not a WebSocket frame.
    let lock_targets = steps_needing_a_lock(&state, &raw_snapshot, &chain, &forced_reruns);
    let held_locks = match acquire_chain_locks(&lock_targets, "webui") {
        Ok(locks) => locks,
        Err(ChainLockError::Conflict(conflict)) => return Ok(lock_conflict_response(&conflict)),
        Err(ChainLockError::Io(e)) => {
            return Err(ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
        }
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
            forced_reruns,
            persist,
            cols,
            rows,
            kill_rx,
            held_locks,
        )
    }))
}

/// Releases every lock still left in `0` on drop — same "sweep whatever
/// wasn't explicitly resolved" guard `run_block`'s own
/// `ReleaseRemainingLocks` is, factored out here so `run_tty_chain` (a
/// plain `async fn` with several early `return`s, not a `stream!`
/// generator) gets the exact same "released no matter how this function
/// actually ends" guarantee without duplicating the `Drop` impl.
struct ReleaseRemainingLocks(HashMap<(String, String), PathBuf>);

impl Drop for ReleaseRemainingLocks {
    fn drop(&mut self) {
        for path in self.0.values() {
            let _ = meshfox_core::service_lock::release(path);
        }
    }
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
    forced_reruns: std::collections::HashSet<meshfox_core::BlockAddr>,
    persist: bool,
    cols: u16,
    rows: u16,
    mut kill_rx: oneshot::Receiver<()>,
    held_locks: HashMap<(String, String), PathBuf>,
) {
    let _guard = RunGuard {
        state: Arc::clone(&state),
        run_id: run_id.clone(),
    };
    // Released on drop, however this function actually ends (a clean
    // finish, `killed`, or an early `return` on a dead socket) — see
    // `ReleaseRemainingLocks`'s own doc comment. No `service` step can ever
    // reach this function (`tty`/`service` are mutually exclusive, see
    // `DepsError::ServiceTtyConflict`), so — unlike `run_block` — nothing
    // here ever needs to *remove* an address from this map without
    // releasing it: every address this function locks (plain or `tty`)
    // is released the moment its own step ends, and this only ever
    // sweeps up whatever a kill/disconnect/failure left unresolved.
    let mut held_locks = ReleaseRemainingLocks(held_locks);

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

    let mut final_exit_code = 0;
    let mut killed = false;
    // Same per-file tracking `run_block` uses — see its own doc comment.
    let mut file_raws: HashMap<Option<PathBuf>, String> = HashMap::new();

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

        let primary_raw_now = file_raws
            .get(&None)
            .cloned()
            .unwrap_or_else(|| state.raw.lock().unwrap().clone());
        let mut located = match locate_node(&state, &primary_raw_now, &addr.node_id) {
            Ok(l) => l,
            Err(e) => {
                send_event(&mut socket, &RunEvent::Error { message: e.1 }).await;
                break;
            }
        };
        if let Some(cached) = file_raws.get(&located.origin) {
            located.raw = cached.clone();
        }

        // Re-parse so an earlier step's freshly-patched cache (above) is
        // visible before this one runs — same reasoning `run_block`
        // already has.
        let node_text = match Canvas::from_markdown(&located.raw)
            .ok()
            .and_then(|c| c.node(&located.local_id).map(|n| n.text.clone()))
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
        let canvas_path_for_step = located.origin.as_deref().unwrap_or(&state.canvas_path);
        let cwd = canvas_root_dir(canvas_path_for_step).to_path_buf();
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

        // Same session-freshness skip `run_block` already has (see its own
        // doc comment) — previously missing here entirely, so a `⛓ run
        // chain` through this WebSocket path always re-ran every pulled-in
        // dependency regardless of whether it had already run successfully
        // this session, unlike the plain (non-`tty`) `/api/run` path.
        let is_requested_target = Some(addr) == chain.last();
        let live_fingerprint = meshfox_core::session_fingerprint(&block, &resolved_vars);
        if !is_requested_target && !block.always && !forced_reruns.contains(addr) {
            let already_fresh = state
                .session_runs
                .lock()
                .unwrap()
                .get(&(addr.node_id.clone(), addr.block_name.clone()))
                .filter(|run| run.fingerprint == live_fingerprint)
                .cloned();
            if let Some(session_run) = already_fresh {
                resolved_vars.extend(session_run.produced_vars);
                if !send_event(
                    &mut socket,
                    &RunEvent::StepSkipped {
                        node_id: addr.node_id.clone(),
                        block: addr.block_name.clone(),
                        output: session_run.output,
                        duration_ms: session_run.duration_ms,
                    },
                )
                .await
                {
                    return;
                }
                continue;
            }
        }

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
        let mut stdout_only = String::new();
        let mut stderr_only = String::new();
        let step_started = std::time::Instant::now();

        // Same block-with-substituted-interpreter clone `run_block` uses —
        // see its own comment on the equivalent line.
        let mut resolved_block = block.clone();
        if let Some(spec) = &block.interpreter {
            resolved_block.interpreter = Some(meshfox_core::resolve_interpreter(spec, &resolved_vars));
        }

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
                &state,
                addr.node_id.clone(),
                addr.block_name.clone(),
                &mut socket,
                &block.code,
                resolved_block.interpreter.as_deref(),
                &block_env,
                Some(&cwd),
                Some(canvas_path_for_step),
                cols,
                rows,
                &mut kill_rx,
                // Handed off by value, not borrowed — ownership (and the
                // responsibility to release it once the session actually
                // ends, not whenever this one connection does) transfers
                // to the `tty_registry::TtySessionHandle` this call
                // registers, same reasoning `run_registry::track`'s own
                // callers already follow for a plain block's lock.
                held_locks.0.remove(&(addr.node_id.clone(), addr.block_name.clone())),
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
            let proc = match stream_exec::spawn_block(
                &resolved_block,
                &block_env,
                Some(&cwd),
                Some(canvas_path_for_step),
            ) {
                Ok(p) => p,
                Err(e) => {
                    if let Some(path) = held_locks.0.remove(&(addr.node_id.clone(), addr.block_name.clone())) {
                        let _ = meshfox_core::service_lock::release(&path);
                    }
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
            if let Some(path) = held_locks.0.get(&(addr.node_id.clone(), addr.block_name.clone())) {
                let _ = meshfox_core::service_lock::update_owner_pid(path, proc.child.id().unwrap_or(0));
            }
            // Same registry-backed detachment `run_block_impl` uses for
            // its own plain steps — see its own doc comment, including on
            // why this address's lock (if any) is handed to `track` below
            // instead of being released once this step's own loop ends —
            // a client disconnect here must not free a lock the process
            // is still genuinely running under.
            let lock_path_for_run =
                held_locks.0.remove(&(addr.node_id.clone(), addr.block_name.clone()));
            let run_handle =
                run_registry::track(addr.node_id.clone(), addr.block_name.clone(), proc, lock_path_for_run);
            state.runs_registry.lock().unwrap().insert(
                (addr.node_id.clone(), addr.block_name.clone()),
                Arc::clone(&run_handle),
            );
            let (_backlog, mut run_rx) = run_handle.subscribe_from(0);
            let mut kill_requested = false;
            loop {
                tokio::select! {
                    event = run_rx.recv() => {
                        match event {
                            Ok(run_registry::RunEvent::Line(line)) => {
                                full_output.push_str(&line.text);
                                full_output.push('\n');
                                match line.stream {
                                    stream_exec::OutputStream::Stdout => {
                                        stdout_only.push_str(&line.text);
                                        stdout_only.push('\n');
                                    }
                                    stream_exec::OutputStream::Stderr => {
                                        stderr_only.push_str(&line.text);
                                        stderr_only.push('\n');
                                    }
                                }
                                if !send_event(&mut socket, &RunEvent::Output {
                                    node_id: addr.node_id.clone(),
                                    block: addr.block_name.clone(),
                                    stream: line.stream,
                                    text: line.text,
                                }).await {
                                    // Client gone — this run keeps going in
                                    // the background regardless (any other
                                    // subscriber, or a reconnect, still
                                    // sees it through), same as
                                    // `run_block_impl`'s stream simply not
                                    // being polled any more.
                                    return;
                                }
                            }
                            Ok(run_registry::RunEvent::Done(run_registry::RunOutcome::Exited { exit_code })) => {
                                break exit_code;
                            }
                            Ok(run_registry::RunEvent::Done(run_registry::RunOutcome::Killed)) => {
                                killed = true;
                                break -1;
                            }
                            Ok(run_registry::RunEvent::Done(run_registry::RunOutcome::Running)) | Err(_) => {
                                break -1;
                            }
                        }
                    }
                    _ = &mut kill_rx, if !kill_requested => {
                        kill_requested = true;
                        run_handle.kill();
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

        let duration_ms = step_started.elapsed().as_millis() as u64;
        if !send_event(
            &mut socket,
            &RunEvent::StepEnd {
                node_id: addr.node_id.clone(),
                block: addr.block_name.clone(),
                exit_code,
                duration_ms,
            },
        )
        .await
        {
            return;
        }
        final_exit_code = exit_code;
        // This step's own lock (`tty` or plain) was already handed off —
        // to `relay_tty_step`'s own `tty_registry::track` call or this
        // loop's own `run_registry::track` call, above — nothing left to
        // release here (see either call site's own comment on why).

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
                output: full_output.clone(),
                duration_ms,
                stdout: stdout_only,
                stderr: stderr_only,
            };
            if let Some(updated) = meshfox_core::write_output(&node_text, &addr.block_name, &result)
            {
                if let Some(patched) = mdcanvas::set_node_body(&located.raw, &located.local_id, &updated) {
                    file_raws.insert(located.origin.clone(), patched);
                }
            }
        }

        if exit_code == 0 && !from_value_error {
            let produced_vars = from_decls
                .iter()
                .filter_map(|decl| resolved_vars.get(&decl.name).map(|v| (decl.name.clone(), v.clone())))
                .collect();
            state.session_runs.lock().unwrap().insert(
                (addr.node_id.clone(), addr.block_name.clone()),
                SessionRun { fingerprint: live_fingerprint, produced_vars, output: full_output, duration_ms },
            );
        }

        if exit_code != 0 || from_value_error {
            break;
        }
    }

    if persist {
        for (origin, content) in file_raws {
            let result = match &origin {
                None => state.save(&content),
                Some(path) => std::fs::write(path, &content),
            };
            if let Err(e) = result {
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
///
/// Relays one `tty` step over `socket` once it's already in "raw I/O" mode
/// (right after `RunEvent::TtyStart`), *as one viewer of* the address's own
/// `tty_registry::TtySessionHandle` — this connection is never the pty's
/// sole owner any more (see that module's own doc comment): closing it
/// (`Message::Close`/a dropped socket) only removes *this* viewer from the
/// session's size negotiation and returns `Disconnected`, it doesn't kill
/// anything — the session keeps running for any other attached viewer, or
/// for a future `/api/run/tty/attach` reconnect, exactly like a real `tmux`
/// pane surviving a detach. Only an explicit `/api/kill` (the `kill_rx`
/// arm below) or the process exiting on its own actually ends it.
#[allow(clippy::too_many_arguments)]
async fn relay_tty_step(
    state: &Arc<AppState>,
    node_id: String,
    block_name: String,
    socket: &mut WebSocket,
    code: &str,
    interpreter: Option<&str>,
    envs: &HashMap<String, String>,
    cwd: Option<&std::path::Path>,
    canvas_path: Option<&std::path::Path>,
    cols: u16,
    rows: u16,
    kill_rx: &mut oneshot::Receiver<()>,
    lock_path: Option<PathBuf>,
) -> TtyStepOutcome {
    let pty = match pty_exec::spawn(code, interpreter, envs, cwd, canvas_path, cols, rows) {
        Ok(p) => p,
        Err(e) => {
            if let Some(path) = &lock_path {
                let _ = meshfox_core::service_lock::release(path);
            }
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
    // Same placeholder-pid correction `run_block`'s own plain-block spawn
    // path does — this address's lock (if any) was necessarily claimed
    // before this pty existed.
    if let Some(path) = &lock_path {
        let _ = meshfox_core::service_lock::update_owner_pid(path, pty.pid() as u32);
    }

    // `lock_path` (if any) is handed to `track` below by value — its
    // background task releases it once the session actually ends, not
    // whenever this one connection does (see `tty_registry::track`'s own
    // doc comment on why: this session outlives whatever connection
    // started it, so its lock has to too).
    let handle = tty_registry::track(node_id.clone(), block_name.clone(), pty, lock_path);
    state.tty_registry.lock().unwrap().insert((node_id, block_name), Arc::clone(&handle));

    let (backlog, mut rx) = handle.attach();
    if !backlog.is_empty() && socket.send(Message::Binary(backlog)).await.is_err() {
        return TtyStepOutcome::Disconnected;
    }
    let viewer_id = handle.register_viewer(cols, rows);
    let mut kill_requested = false;

    let outcome = loop {
        tokio::select! {
            event = rx.recv() => {
                match event {
                    Ok(tty_registry::TtyEvent::Bytes(bytes)) => {
                        if socket.send(Message::Binary(bytes.to_vec())).await.is_err() {
                            handle.forget_viewer(viewer_id);
                            break TtyStepOutcome::Disconnected;
                        }
                    }
                    Ok(tty_registry::TtyEvent::Done(tty_registry::RunOutcome::Exited { exit_code })) => {
                        handle.forget_viewer(viewer_id);
                        break TtyStepOutcome::Exited(exit_code);
                    }
                    Ok(tty_registry::TtyEvent::Done(tty_registry::RunOutcome::Killed)) => {
                        handle.forget_viewer(viewer_id);
                        break TtyStepOutcome::Killed;
                    }
                    Ok(tty_registry::TtyEvent::Done(tty_registry::RunOutcome::Running)) | Err(broadcast::error::RecvError::Closed) => {
                        handle.forget_viewer(viewer_id);
                        break TtyStepOutcome::Exited(-1);
                    }
                    // This connection's own receiver fell more than
                    // `tty_registry`'s broadcast capacity behind the live
                    // tail (a fast-redrawing full-screen program like
                    // `tabiew` can produce enough chunks per redraw for
                    // this to happen even to the *originating* connection
                    // under load) — see `relay_tty_viewer`'s identical arm
                    // for the full reasoning on why this can't just be
                    // folded into the `Err(_) => exited(-1)` case above:
                    // that would falsely report a still-running pty as
                    // having exited (a lie `run_tty_chain`'s own caller
                    // would act on), not merely drop a passive viewer.
                    // Re-subscribing and replaying the current backlog
                    // recovers the same way; `handle.outcome()` catches
                    // the case where the session *also* genuinely ended in
                    // the gap this missed.
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        let (backlog, fresh_rx) = handle.attach();
                        rx = fresh_rx;
                        if !backlog.is_empty() && socket.send(Message::Binary(backlog)).await.is_err() {
                            handle.forget_viewer(viewer_id);
                            break TtyStepOutcome::Disconnected;
                        }
                        match handle.outcome() {
                            tty_registry::RunOutcome::Running => {}
                            tty_registry::RunOutcome::Exited { exit_code } => {
                                handle.forget_viewer(viewer_id);
                                break TtyStepOutcome::Exited(exit_code);
                            }
                            tty_registry::RunOutcome::Killed => {
                                handle.forget_viewer(viewer_id);
                                break TtyStepOutcome::Killed;
                            }
                        }
                    }
                }
            }
            msg = socket.recv() => {
                match msg {
                    Some(Ok(Message::Binary(bytes))) => handle.write(bytes),
                    Some(Ok(Message::Text(text))) => {
                        if let Ok(resize) = serde_json::from_str::<ResizeMessage>(&text) {
                            handle.resize_viewer(viewer_id, resize.cols.max(1), resize.rows.max(1));
                        }
                    }
                    Some(Ok(Message::Close(_))) | None | Some(Err(_)) => {
                        handle.forget_viewer(viewer_id);
                        break TtyStepOutcome::Disconnected;
                    }
                    // Ping/Pong: axum answers Pings internally; nothing to
                    // relay to the pty either way.
                    Some(Ok(_)) => {}
                }
            }
            // Only armed once — after firing, this connection's own kill
            // request has been delivered to the registry, which every
            // attached viewer (not just this one) will see resolve as
            // `Done(Killed)` above; re-arming would just fire again
            // immediately for no reason (same pattern `run_block_impl`'s
            // own plain-step loop uses).
            _ = &mut *kill_rx, if !kill_requested => {
                kill_requested = true;
                handle.kill();
            }
        }
    };
    outcome
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AttachTtyQuery {
    node_id: String,
    block: String,
    #[serde(default = "default_tty_cols")]
    cols: u16,
    #[serde(default = "default_tty_rows")]
    rows: u16,
}

fn default_tty_cols() -> u16 {
    80
}

fn default_tty_rows() -> u16 {
    24
}

/// `GET /api/run/tty/attach?nodeId=..&block=..&cols=..&rows=..` — joins an
/// *already-running* `tty` session as an additional viewer, without
/// spawning anything: replays its full still-buffered byte history, then
/// relays live output/input exactly like the connection that originally
/// started it (see `relay_tty_step`'s own doc comment — this is the exact
/// same "just a viewer" relationship to the session, just without ever
/// having spawned it). `404` if this address has no live session right
/// now (never started, or it's already finished and nothing is tracking
/// it any more — a finished session's own final byte history is still
/// attachable for as long as it stays the *current* entry for this
/// address, same as `run_registry`'s equivalent for a plain block).
async fn attach_tty(
    State(state): State<Arc<AppState>>,
    Query(query): Query<AttachTtyQuery>,
    ws: WebSocketUpgrade,
) -> Result<Response, ApiError> {
    let key = (query.node_id.clone(), query.block.clone());
    let Some(handle) = state.tty_registry.lock().unwrap().get(&key).cloned() else {
        return Err(ApiError(
            StatusCode::NOT_FOUND,
            format!("no tty session for {:?}/{:?}", query.node_id, query.block),
        ));
    };
    let cols = query.cols.max(1);
    let rows = query.rows.max(1);
    Ok(ws.on_upgrade(move |socket| relay_tty_viewer(handle, socket, cols, rows)))
}

/// The actual attach loop `attach_tty` upgrades into — see its own doc
/// comment. No local kill switch here (unlike `relay_tty_step`'s own
/// `kill_rx`): an attach-only viewer never has a `runId` of its own to
/// begin with, so killing this session is always done by address, via the
/// ordinary `POST /api/kill {nodeId, block}` path (already reaches the
/// exact same `TtySessionHandle` through `AppState::tty_registry`) rather
/// than anything this socket needs to carry itself.
async fn relay_tty_viewer(
    handle: Arc<tty_registry::TtySessionHandle>,
    mut socket: WebSocket,
    cols: u16,
    rows: u16,
) {
    let (backlog, mut rx) = handle.attach();
    if !backlog.is_empty() && socket.send(Message::Binary(backlog)).await.is_err() {
        return;
    }
    // Already finished by the time this attached — tell this late viewer
    // right away instead of waiting on a broadcast that already fired
    // before `rx` existed (same race-freedom reasoning `subscribe_run`
    // documents for a plain block's own equivalent).
    if !matches!(handle.outcome(), tty_registry::RunOutcome::Running) {
        let _ = socket.send(Message::Close(None)).await;
        return;
    }

    let viewer_id = handle.register_viewer(cols, rows);
    loop {
        tokio::select! {
            event = rx.recv() => {
                match event {
                    Ok(tty_registry::TtyEvent::Bytes(bytes)) => {
                        if socket.send(Message::Binary(bytes.to_vec())).await.is_err() {
                            handle.forget_viewer(viewer_id);
                            return;
                        }
                    }
                    Ok(tty_registry::TtyEvent::Done(_)) | Err(broadcast::error::RecvError::Closed) => {
                        handle.forget_viewer(viewer_id);
                        let _ = socket.send(Message::Close(None)).await;
                        return;
                    }
                    // This viewer's own receiver fell more than
                    // `tty_registry`'s broadcast capacity (1024 messages)
                    // behind the live tail — a slow WebSocket/browser
                    // watching a fast-redrawing full-screen program
                    // (`tabiew`, say) is exactly the case this handles.
                    // Lumping this in with the `Err(_) => close` arm below
                    // (as this used to) treated falling behind as if the
                    // session itself had ended: this viewer's socket got
                    // silently closed (a blank terminal, then
                    // "disconnected") the moment it couldn't keep up,
                    // while the session — and every *other* viewer, an
                    // originating TUI connection's own `relay_tty_step`
                    // included — kept going completely fine. Re-
                    // subscribing via `attach()` (same call the initial
                    // connection above used) gets a fresh receiver plus
                    // whatever's *currently* in the byte-history ring
                    // buffer — replaying that lets xterm.js's own parser
                    // resync from a recent, coherent state instead of
                    // wherever the skipped chunk happened to leave it.
                    // `attach()` alone can't tell whether the session
                    // *itself* also ended in the gap this missed (a
                    // `Done` broadcast the old, now-abandoned `rx` would
                    // never see) — `handle.outcome()` is what actually
                    // answers that, same check the pre-loop code above
                    // already makes for a viewer that attaches after the
                    // fact.
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        let (backlog, fresh_rx) = handle.attach();
                        rx = fresh_rx;
                        if !backlog.is_empty() && socket.send(Message::Binary(backlog)).await.is_err() {
                            handle.forget_viewer(viewer_id);
                            return;
                        }
                        if !matches!(handle.outcome(), tty_registry::RunOutcome::Running) {
                            handle.forget_viewer(viewer_id);
                            let _ = socket.send(Message::Close(None)).await;
                            return;
                        }
                    }
                }
            }
            msg = socket.recv() => {
                match msg {
                    Some(Ok(Message::Binary(bytes))) => handle.write(bytes),
                    Some(Ok(Message::Text(text))) => {
                        if let Ok(resize) = serde_json::from_str::<ResizeMessage>(&text) {
                            handle.resize_viewer(viewer_id, resize.cols.max(1), resize.rows.max(1));
                        }
                    }
                    Some(Ok(Message::Close(_))) | None | Some(Err(_)) => {
                        handle.forget_viewer(viewer_id);
                        return;
                    }
                    Some(Ok(_)) => {}
                }
            }
        }
    }
}

/// One entry of `GET /api/services`'s response — see `crate::services`'s own
/// module doc comment for the registry this is read off of.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ServiceDto {
    node_id: String,
    block: String,
    status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    exit_code: Option<i32>,
    pid: u32,
    /// Milliseconds since this instance was spawned — recomputed fresh on
    /// every response rather than sending an absolute timestamp, since
    /// `ServiceHandle::started_at` is a monotonic `Instant`, not wall-clock
    /// time.
    uptime_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    cpu_percent: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    mem_bytes: Option<u64>,
}

fn service_dto(node_id: &str, block: &str, handle: &services::ServiceHandle) -> ServiceDto {
    let (status, exit_code) = match handle.status() {
        services::ServiceStatus::Running => ("running", None),
        services::ServiceStatus::Crashed { exit_code } => ("crashed", Some(exit_code)),
        services::ServiceStatus::Stopped => ("stopped", None),
    };
    let sample = services::sample(handle.pid);
    ServiceDto {
        node_id: node_id.to_string(),
        block: block.to_string(),
        status,
        exit_code,
        pid: handle.pid,
        uptime_ms: handle.started_at.elapsed().as_millis() as u64,
        cpu_percent: sample.as_ref().map(|s| s.cpu_percent),
        mem_bytes: sample.as_ref().map(|s| s.mem_bytes),
    }
}

/// `GET /api/services` — every `service` block this process has ever
/// spawned and still knows about (running, crashed, or explicitly
/// stopped), for a freshly-loaded/refreshed webui tab to repopulate the
/// service panel from — see `crate::services`'s own module doc comment for
/// why this can't just be inferred from an in-flight `/api/run` stream.
async fn get_services(State(state): State<Arc<AppState>>) -> Json<Vec<ServiceDto>> {
    let services = state.services.lock().unwrap();
    Json(
        services
            .iter()
            .map(|((node_id, block), handle)| service_dto(node_id, block, handle))
            .collect(),
    )
}

/// One entry of `GET /api/runs`'s response — see that handler's own doc
/// comment.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ActiveRunDto {
    node_id: String,
    block: String,
    /// `"plain"` (`run_registry`) or `"tty"` (`tty_registry`) — a client
    /// only ever wants to reconcile/reattach to one or the other
    /// differently (a plain block's live output vs. a real terminal), so
    /// this is what it switches on rather than trying to infer the kind
    /// from anything else in this DTO.
    kind: &'static str,
    status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    exit_code: Option<i32>,
    uptime_ms: u64,
}

/// `GET /api/runs` — every plain-block run and `tty` session this process
/// currently knows about (running, or the most recent one for that
/// address — see `run_registry`/`tty_registry`'s own module doc comments
/// on why an entry outlives any single request), regardless of which
/// connection (if any) is currently watching it. What a freshly-loaded or
/// reloaded tab reconciles its own live-output state against on mount —
/// without this, reloading a tab mid-run shows nothing for a block that's
/// actually still (or again) running, even though its output has been
/// sitting in the server's own registry the whole time — same role
/// `GET /api/services` already plays for `service` blocks, generalized to
/// the two other kinds of run this server now separately tracks.
async fn get_active_runs(State(state): State<Arc<AppState>>) -> Json<Vec<ActiveRunDto>> {
    let mut out = Vec::new();
    for handle in state.runs_registry.lock().unwrap().values() {
        let (status, exit_code) = match handle.outcome() {
            run_registry::RunOutcome::Running => ("running", None),
            run_registry::RunOutcome::Exited { exit_code } => ("exited", Some(exit_code)),
            run_registry::RunOutcome::Killed => ("killed", None),
        };
        out.push(ActiveRunDto {
            node_id: handle.node_id.clone(),
            block: handle.block_name.clone(),
            kind: "plain",
            status,
            exit_code,
            uptime_ms: handle.uptime_ms(),
        });
    }
    for handle in state.tty_registry.lock().unwrap().values() {
        let (status, exit_code) = match handle.outcome() {
            tty_registry::RunOutcome::Running => ("running", None),
            tty_registry::RunOutcome::Exited { exit_code } => ("exited", Some(exit_code)),
            tty_registry::RunOutcome::Killed => ("killed", None),
        };
        out.push(ActiveRunDto {
            node_id: handle.node_id.clone(),
            block: handle.block_name.clone(),
            kind: "tty",
            status,
            exit_code,
            uptime_ms: handle.uptime_ms(),
        });
    }
    Json(out)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ServiceKeyQuery {
    node_id: String,
    block: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ServiceLogLine {
    stream: stream_exec::OutputStream,
    text: String,
}

/// `GET /api/services/log?nodeId=..&block=..` — the service's full retained
/// output buffer (see `crate::services`'s `RingBuffer`), polled rather than
/// streamed: simple and proportionate at this scale, no second live
/// transport alongside `/api/run`'s NDJSON one.
async fn get_service_log(
    State(state): State<Arc<AppState>>,
    Query(q): Query<ServiceKeyQuery>,
) -> Result<Json<Vec<ServiceLogLine>>, ApiError> {
    let services = state.services.lock().unwrap();
    let handle = services.get(&(q.node_id.clone(), q.block.clone())).ok_or_else(|| {
        ApiError(StatusCode::NOT_FOUND, format!("no service {:?}/{:?}", q.node_id, q.block))
    })?;
    Ok(Json(
        handle
            .log_snapshot()
            .into_iter()
            .map(|(stream, text)| ServiceLogLine { stream, text })
            .collect(),
    ))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ServiceKeyRequest {
    node_id: String,
    block: String,
}

/// `POST /api/services/stop` — kills the service's whole process group
/// (`ServiceHandle::stop`) and releases its lock file. The registry entry
/// itself is kept (now `Stopped`), not removed, so the panel can still show
/// "stopped" rather than the service just vanishing.
async fn stop_service(
    State(state): State<Arc<AppState>>,
    Json(req): Json<ServiceKeyRequest>,
) -> Result<StatusCode, ApiError> {
    let services = state.services.lock().unwrap();
    let handle = services.get(&(req.node_id.clone(), req.block.clone())).ok_or_else(|| {
        ApiError(StatusCode::NOT_FOUND, format!("no service {:?}/{:?}", req.node_id, req.block))
    })?;
    handle
        .stop()
        .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ServiceActionResponse {
    pid: u32,
}

/// `POST /api/services/restart` — stops the running instance and spawns a
/// fresh one with the exact parameters it was last started with (see
/// `crate::services::restart`'s own doc comment: "local only", never
/// touches anything that depends on this service).
async fn restart_service(
    State(state): State<Arc<AppState>>,
    Json(req): Json<ServiceKeyRequest>,
) -> Result<Json<ServiceActionResponse>, ApiError> {
    let mut services = state.services.lock().unwrap();
    let key = (req.node_id.clone(), req.block.clone());
    let old = services.get(&key).ok_or_else(|| {
        ApiError(StatusCode::NOT_FOUND, format!("no service {:?}/{:?}", req.node_id, req.block))
    })?;
    let restarted =
        services::restart(old).map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let pid = restarted.pid;
    services.insert(key, restarted);
    Ok(Json(ServiceActionResponse { pid }))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ForceStartServiceRequest {
    path: Vec<String>,
    block: String,
    #[serde(default)]
    vars: HashMap<String, String>,
}

/// `POST /api/services/force-start` — the confirm side of a `RunEvent::
/// ServiceLockConflict`: kills whatever process the lock file currently
/// names as owner, releases the lock, and starts the service fresh. Only
/// resolves this *one* block's own `env=` (not the whole `deps=` chain —
/// by the time a conflict was hit, everything upstream of it in the
/// original request had already run) against `req.vars` plus whatever's
/// already cached/session-known; a variable only ever produced by a
/// `from=` source that hasn't run in *this* request fails with a clear
/// message telling the caller to re-run the chain instead, rather than
/// silently guessing.
async fn force_start_service(
    State(state): State<Arc<AppState>>,
    Json(req): Json<ForceStartServiceRequest>,
) -> Result<Json<ServiceActionResponse>, ApiError> {
    let raw_snapshot = state.raw.lock().unwrap().clone();
    let canvas = resolved_canvas(&raw_snapshot, &state.canvas_path)?;
    let path: Vec<&str> = req.path.iter().map(String::as_str).collect();
    let chain = meshfox_core::resolve_run_chain(&canvas, &path, &req.block, false)?;
    let target = chain
        .last()
        .cloned()
        .ok_or_else(|| ApiError(StatusCode::INTERNAL_SERVER_ERROR, "empty chain".to_string()))?;

    let needed = meshfox_core::env_var_names_for_chain(&canvas, &chain);
    let decls = meshfox_core::declared_vars(&canvas)
        .map_err(|e| ApiError(StatusCode::UNPROCESSABLE_ENTITY, e.to_string()))?;
    let relevant_decls: Vec<_> = decls.iter().filter(|d| needed.contains(&d.name)).cloned().collect();
    validate_var_overrides(&relevant_decls, &req.vars)?;
    let resolved_vars = {
        let cache = state.vars_cache.lock().unwrap();
        let shared = meshfox_core::load_shared_env(canvas_root_dir(&state.canvas_path));
        let resolved = meshfox_core::resolve_with_shared(
            &relevant_decls,
            &req.vars,
            &cache,
            &HashMap::new(),
            &shared,
        );
        if !resolved.missing.is_empty() {
            let names: Vec<&str> = resolved.missing.iter().map(|d| d.name.as_str()).collect();
            return Err(ApiError(
                StatusCode::UNPROCESSABLE_ENTITY,
                format!(
                    "can't force-start without re-running the chain — missing variable(s): {} \
                     (only available once their own `from=` source has actually run in this request)",
                    names.join(", ")
                ),
            ));
        }
        resolved.values
    };

    let located = locate_node(&state, &raw_snapshot, &target.node_id)?;
    let node_text = Canvas::from_markdown(&located.raw)
        .ok()
        .and_then(|c| c.node(&located.local_id).map(|n| n.text.clone()))
        .ok_or_else(|| ApiError(StatusCode::NOT_FOUND, format!("node {:?} not found", target.node_id)))?;
    let canvas_path_for_step = located.origin.clone().unwrap_or_else(|| state.canvas_path.clone());
    let cwd = canvas_root_dir(&canvas_path_for_step).to_path_buf();
    let block = meshfox_core::scan_runnable_blocks(&target.node_id, &node_text)
        .into_iter()
        .find(|b| b.name.as_deref() == Some(target.block_name.as_str()))
        .ok_or_else(|| {
            ApiError(
                StatusCode::NOT_FOUND,
                format!("no runnable block named {:?} in node {:?}", target.block_name, target.node_id),
            )
        })?;
    if !block.service {
        return Err(ApiError(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("block {:?} is not a `service` block", target.block_name),
        ));
    }

    let block_env = meshfox_core::map_block_env(&block.env, &resolved_vars);
    let mut resolved_block = block.clone();
    if let Some(spec) = &block.interpreter {
        resolved_block.interpreter = Some(meshfox_core::resolve_interpreter(spec, &resolved_vars));
    }

    let lock_path =
        meshfox_core::service_lock_path(&canvas_path_for_step, &target.node_id, &target.block_name);
    // Whole-process-group `SIGKILL` on whatever pid the lock file names
    // (even one this process has no live `ServiceHandle` for), release,
    // reacquire — a no-op straight to acquiring if the lock's already
    // free — same shared helper `force_run`'s own generalized conflict
    // path uses now (see `meshfox_core::service_lock::kill_and_acquire`'s
    // own doc comment); `services::spawn` no longer acquires this itself
    // (see its own doc comment), so this must run before it either way.
    meshfox_core::service_lock::kill_and_acquire(&lock_path, std::process::id(), "webui")
        .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let handle = services::spawn(
        target.node_id.clone(),
        target.block_name.clone(),
        resolved_block,
        block_env,
        cwd,
        canvas_path_for_step,
        "webui",
    )
    .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let pid = handle.pid;
    state.services.lock().unwrap().insert((target.node_id, target.block_name), handle);
    Ok(Json(ServiceActionResponse { pid }))
}

/// Default `debug_send` timeout — mirrors `crate::mcp`'s own
/// `DEFAULT_SEND_TIMEOUT_MS` (`crates/cli/src/mcp.rs`) exactly, since a
/// `coordinator`-routed `debug_send` should behave identically to the
/// in-process one it replaces.
const DEFAULT_DEBUG_SEND_TIMEOUT_MS: u64 = 60_000;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DebugStartRequest {
    node_id: String,
    #[serde(default)]
    block_name: Option<String>,
    #[serde(default)]
    vars: HashMap<String, String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DebugStartResponse {
    session_id: String,
    node_id: String,
    block_name: Option<String>,
    cwd: String,
}

/// `POST /api/debug/start` — the `crate::coordinator`-routed counterpart to
/// `crate::mcp`'s own in-process `debug_start` (deliberately plain
/// POST/JSON, not a WS upgrade: this is one request, one reply, nothing to
/// stream — unlike `/api/run/tty`'s own reason for upgrading). Same
/// resolution `mcp.rs::debug_start` does today: a node's runnable blocks,
/// its declared `env=` variables against `req.vars`/the on-disk var cache/
/// shared env, and its own resolved `cwd` — deliberately *not*
/// `locate_node`'s include-aware resolution (mirrors `mcp.rs`'s own
/// simpler `Canvas::from_markdown(&raw).node(...)`, which never resolves
/// includes either, so a session's identity doesn't quietly gain a
/// capability the in-process path it replaces never had).
async fn debug_start(
    State(state): State<Arc<AppState>>,
    Json(req): Json<DebugStartRequest>,
) -> Result<Json<DebugStartResponse>, ApiError> {
    let raw = state.raw.lock().unwrap().clone();
    let canvas = Canvas::from_markdown(&raw)
        .map_err(|e| ApiError(StatusCode::UNPROCESSABLE_ENTITY, e.to_string()))?;
    let node = canvas
        .node(&req.node_id)
        .ok_or_else(|| ApiError(StatusCode::NOT_FOUND, format!("no node {:?}", req.node_id)))?;
    let blocks = meshfox_core::scan_runnable_blocks(&req.node_id, &node.text);
    let block = match &req.block_name {
        Some(name) => blocks
            .iter()
            .find(|b| b.name.as_deref() == Some(name.as_str()))
            .ok_or_else(|| {
                ApiError(
                    StatusCode::NOT_FOUND,
                    format!("no runnable block named {name:?} in node {:?}", req.node_id),
                )
            })?,
        None => meshfox_core::fence::default_block(&req.node_id, &blocks)
            .map_err(|names| {
                ApiError(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    format!(
                        "node {:?} has more than one default-eligible block ({}); pass blockName explicitly",
                        req.node_id,
                        names.join(", ")
                    ),
                )
            })?
            .ok_or_else(|| {
                ApiError(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    format!("node {:?} has no default block; pass blockName explicitly", req.node_id),
                )
            })?,
    };

    let needed: std::collections::HashSet<String> =
        block.env.iter().map(|r| r.var_name.clone()).collect();
    let decls = meshfox_core::declared_vars(&canvas)
        .map_err(|e| ApiError(StatusCode::UNPROCESSABLE_ENTITY, e.to_string()))?;
    let relevant: Vec<_> = decls.iter().filter(|d| needed.contains(&d.name)).cloned().collect();
    validate_var_overrides(&relevant, &req.vars)?;

    let mut cache = state.vars_cache.lock().unwrap();
    let shared = meshfox_core::load_shared_env(canvas_root_dir(&state.canvas_path));
    let resolved =
        meshfox_core::resolve_with_shared(&relevant, &req.vars, &cache, &HashMap::new(), &shared);
    if !resolved.missing.is_empty() {
        let names: Vec<&str> = resolved.missing.iter().map(|d| d.name.as_str()).collect();
        return Err(ApiError(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("missing required variable(s): {} — pass them in `vars`", names.join(", ")),
        ));
    }
    for (name, value) in &req.vars {
        if relevant.iter().any(|d| &d.name == name && !d.secret && !d.session) {
            let _ = cache.set(name, value);
        }
    }
    drop(cache);

    let envs = meshfox_core::map_block_env(&block.env, &resolved.values);
    let cwd = node.cwd(canvas_root_dir(&state.canvas_path));

    let session = debug_session::DebugSession::spawn(&cwd, envs)
        .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, format!("failed to start debug session: {e}")))?;
    let session_id = uuid::Uuid::new_v4().to_string();
    state
        .debug_sessions
        .lock()
        .unwrap()
        .insert(session_id.clone(), Arc::new(tokio::sync::Mutex::new(session)));

    Ok(Json(DebugStartResponse {
        session_id,
        node_id: req.node_id,
        block_name: block.name.clone(),
        cwd: cwd.display().to_string(),
    }))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DebugSendRequest {
    session_id: String,
    code: String,
    #[serde(default)]
    timeout_ms: Option<u64>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DebugSendResponse {
    stdout: String,
    stderr: String,
    exit_code: i32,
    timed_out: bool,
    session_ended: bool,
}

/// `POST /api/debug/send` — the `crate::coordinator`-routed counterpart to
/// `crate::mcp`'s own in-process `debug_send`. `404` if `sessionId` is
/// unknown to *this* worker specifically — a caller that resolved a
/// different worker (or hit this one after it restarted) gets a real error
/// rather than silently doing nothing, same "no silent fallback" posture
/// every other coordinator-routed operation this session added has.
async fn debug_send(
    State(state): State<Arc<AppState>>,
    Json(req): Json<DebugSendRequest>,
) -> Result<Json<DebugSendResponse>, ApiError> {
    let session = state
        .debug_sessions
        .lock()
        .unwrap()
        .get(&req.session_id)
        .cloned()
        .ok_or_else(|| ApiError(StatusCode::NOT_FOUND, format!("no debug session {:?}", req.session_id)))?;
    let timeout = Duration::from_millis(req.timeout_ms.unwrap_or(DEFAULT_DEBUG_SEND_TIMEOUT_MS));
    let outcome = {
        let mut session = session.lock().await;
        session
            .send(&req.code, timeout)
            .await
            .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, format!("debug session write/read failed: {e}")))?
    };
    if outcome.session_ended {
        state.debug_sessions.lock().unwrap().remove(&req.session_id);
    }
    Ok(Json(DebugSendResponse {
        stdout: outcome.stdout,
        stderr: outcome.stderr,
        exit_code: outcome.exit_code,
        timed_out: outcome.timed_out,
        session_ended: outcome.session_ended,
    }))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DebugStopRequest {
    session_id: String,
}

/// `POST /api/debug/stop` — the `crate::coordinator`-routed counterpart to
/// `crate::mcp`'s own in-process `debug_stop`.
async fn debug_stop(
    State(state): State<Arc<AppState>>,
    Json(req): Json<DebugStopRequest>,
) -> Result<StatusCode, ApiError> {
    let removed = state.debug_sessions.lock().unwrap().remove(&req.session_id);
    match removed {
        Some(session) => {
            session.lock().await.stop().await;
            Ok(StatusCode::NO_CONTENT)
        }
        None => Err(ApiError(StatusCode::NOT_FOUND, format!("no debug session {:?}", req.session_id))),
    }
}

/// Cancels an in-flight run started by `run_block` — kills whichever step
/// is currently executing (`SIGKILL`, via `stream_exec::SpawnedProcess::
/// kill`, which reaches the whole process group a hung script spawned, not
/// just `bash` itself) and stops the rest of its dependency chain. `404`
/// if `runId` is unknown, which just as often
/// means "it already finished" as "it never existed" — the client treats
/// either the same way (nothing left to kill).
async fn kill_run(State(state): State<Arc<AppState>>, Json(req): Json<KillRequest>) -> StatusCode {
    if let Some(run_id) = &req.run_id {
        return match state.runs.lock().unwrap().remove(run_id) {
            Some(tx) => {
                let _ = tx.send(());
                StatusCode::NO_CONTENT
            }
            None => StatusCode::NOT_FOUND,
        };
    }
    if let (Some(node_id), Some(block)) = (&req.node_id, &req.block) {
        let key = (node_id.clone(), block.clone());
        // A plain block and a `tty` block can never share an address (see
        // `DepsError::ServiceTtyConflict` — this crate never registers a
        // `tty` block in `runs_registry` or a plain one in `tty_registry`
        // to begin with), so checking both registries here is never
        // ambiguous about which one this address actually belongs to.
        let plain_handle = state.runs_registry.lock().unwrap().get(&key).cloned();
        // `RunHandle`/`TtySessionHandle::kill` are both idempotent/harmless
        // if the run already finished on its own between this lookup and
        // the call — still `204` either way, since there's no meaningful
        // difference to the caller between "killed it" and "it was
        // already done".
        if let Some(handle) = plain_handle {
            handle.kill();
            return StatusCode::NO_CONTENT;
        }
        let tty_handle = state.tty_registry.lock().unwrap().get(&key).cloned();
        return match tty_handle {
            Some(handle) => {
                handle.kill();
                StatusCode::NO_CONTENT
            }
            None => StatusCode::NOT_FOUND,
        };
    }
    StatusCode::BAD_REQUEST
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SubscribeRunQuery {
    node_id: String,
    block: String,
    /// Replay every buffered line at or after this sequence number, then
    /// keep tailing live — `0` (the default, and what a client's very
    /// first subscribe always uses) replays everything still buffered.
    /// A reconnecting client instead sends whatever `seq` its own
    /// last-seen `line` event carried, so it never sees a line twice.
    #[serde(default)]
    since_seq: u64,
}

/// One line of `/api/run/subscribe`'s streamed NDJSON response — a much
/// smaller vocabulary than `RunEvent` (no `StepStart`/chain-level
/// concepts at all): this endpoint watches exactly one address's own
/// `run_registry::RunHandle`, independent of whatever request originally
/// started it or which step of a larger chain it was.
// `rename_all` on an enum only renames the *variant* tag (`"line"`/
// `"done"`) — it does *not* also rename each struct variant's own fields;
// that needs the separate `rename_all_fields` (same combo `RunEvent`,
// above, already uses). Missing it here meant `exit_code` shipped to the
// client as literal snake_case instead of `exitCode`, so `event.exitCode`
// read back as `undefined` on every `"done"` event — indistinguishable
// from a genuinely missing value, which is exactly what let a
// *successful* reconciled run get treated as failed (`undefined !== 0`).
#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase")]
enum SubscribeEvent {
    Line {
        seq: u64,
        stream: stream_exec::OutputStream,
        text: String,
    },
    /// Terminal — always the last line this endpoint ever sends for a
    /// given connection. `exit_code` is only present for `outcome:
    /// "exited"`.
    Done {
        outcome: &'static str,
        #[serde(skip_serializing_if = "Option::is_none")]
        exit_code: Option<i32>,
    },
}

fn done_event(outcome: &run_registry::RunOutcome) -> SubscribeEvent {
    match outcome {
        run_registry::RunOutcome::Exited { exit_code } => {
            SubscribeEvent::Done { outcome: "exited", exit_code: Some(*exit_code) }
        }
        run_registry::RunOutcome::Killed => SubscribeEvent::Done { outcome: "killed", exit_code: None },
        // Never actually reached from `subscribe_run` below (only called
        // once `outcome()` has already been checked to not be `Running`)
        // — `"exited"`-with-no-code is a harmless, honest fallback rather
        // than a silent lie about the process having actually exited.
        run_registry::RunOutcome::Running => SubscribeEvent::Done { outcome: "exited", exit_code: None },
    }
}

fn subscribe_ndjson_line(event: &SubscribeEvent) -> Bytes {
    let mut line = serde_json::to_string(event).expect("SubscribeEvent always serializes");
    line.push('\n');
    Bytes::from(line)
}

/// `GET /api/run/subscribe?nodeId=..&block=..&sinceSeq=0` — watches one
/// address's most recent run independent of whatever request originally
/// started it (see `run_registry`'s own module doc comment): replays
/// every buffered line at or after `sinceSeq`, then tails live output
/// until the run's own terminal outcome, at which point the stream ends.
/// `404` if this address has never been run at all (or its process this
/// server knows about has since been replaced by a fresh run under a
/// different `RunHandle` — same "at most one live entry per address"
/// invariant `AppState::runs_registry` documents).
async fn subscribe_run(
    ws: WebSocketUpgrade,
    State(state): State<Arc<AppState>>,
    Query(query): Query<SubscribeRunQuery>,
) -> Response {
    ws.on_upgrade(move |socket| async move {
        // A `404` here (no such run) is a deliberate silent no-op —
        // `subscribeRun`'s own client-side doc comment already treats "the
        // stream ended having sent nothing" as equivalent to "nothing to
        // show", so this just upgrades and immediately drops the socket
        // with zero messages rather than routing through
        // `pump_run_response_into_ws`'s generic `Error`-message behavior.
        match subscribe_run_impl(state, query).await {
            Ok(response) => pump_run_response_into_ws(socket, Ok(response)).await,
            // Same "always send a real WS Close frame, never just drop the
            // TCP connection" reasoning `pump_run_response_into_ws` itself
            // documents — otherwise a strict client sees `Protocol(
            // ResetWithoutClosingHandshake)` instead of a clean, empty
            // stream.
            Err(_) => {
                let _ = socket.close().await;
            }
        }
    })
}

async fn subscribe_run_impl(
    state: Arc<AppState>,
    query: SubscribeRunQuery,
) -> Result<Response, ApiError> {
    let key = (query.node_id.clone(), query.block.clone());
    let Some(handle) = state.runs_registry.lock().unwrap().get(&key).cloned() else {
        return Err(ApiError(
            StatusCode::NOT_FOUND,
            format!("no run for {:?}/{:?}", query.node_id, query.block),
        ));
    };

    let stream = async_stream::stream! {
        // Subscribing *before* checking whether this run has already
        // finished (rather than the other way around) is what makes this
        // race-free — see `subscribe_from`'s own doc comment: if `outcome`
        // below still reads `Running`, the drain task hasn't reached
        // "record outcome, then broadcast Done" yet, so this subscription
        // (already registered) is guaranteed to still receive that
        // eventual broadcast.
        let (backlog, mut rx) = handle.subscribe_from(query.since_seq);
        // Tracks the highest `seq` actually yielded so far — what a
        // `Lagged` recovery below re-subscribes from, the same gap-free
        // resync a reconnecting client already gets via `since_seq`
        // itself, just driven from inside this one long-lived connection
        // instead of a fresh request.
        let mut last_seq = query.since_seq;
        for line in backlog {
            last_seq = line.seq;
            yield Ok::<_, io::Error>(subscribe_ndjson_line(&SubscribeEvent::Line {
                seq: line.seq,
                stream: line.stream,
                text: line.text,
            }));
        }
        let already_done = handle.outcome();
        if !matches!(already_done, run_registry::RunOutcome::Running) {
            yield Ok(subscribe_ndjson_line(&done_event(&already_done)));
            return;
        }
        loop {
            match rx.recv().await {
                Ok(run_registry::RunEvent::Line(line)) => {
                    last_seq = line.seq;
                    yield Ok(subscribe_ndjson_line(&SubscribeEvent::Line {
                        seq: line.seq,
                        stream: line.stream,
                        text: line.text,
                    }));
                }
                Ok(run_registry::RunEvent::Done(outcome)) => {
                    yield Ok(subscribe_ndjson_line(&done_event(&outcome)));
                    break;
                }
                // Channel closed (shouldn't happen — the drain task always
                // sends `Done` before its own `tx` is dropped): nothing
                // meaningful left to relay.
                Err(broadcast::error::RecvError::Closed) => break,
                // This subscriber fell more than the registry's own
                // broadcast capacity behind the live tail — treating this
                // the same as the run actually ending (as this used to)
                // silently truncated a busy/verbose block's own output the
                // moment a slow consumer (this exact endpoint's own TUI
                // caller, `App::on_external_run_event`, folding output
                // into a redraw-driven UI) couldn't keep up, well before
                // the run was actually done. Re-subscribing from
                // `last_seq + 1` (same backlog-replay path a reconnecting
                // client's own `since_seq` already exercises, just driven
                // from inside this connection instead of a fresh request)
                // recovers the missed lines instead of just giving up.
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    let (backlog, fresh_rx) = handle.subscribe_from(last_seq + 1);
                    rx = fresh_rx;
                    for line in backlog {
                        last_seq = line.seq;
                        yield Ok(subscribe_ndjson_line(&SubscribeEvent::Line {
                            seq: line.seq,
                            stream: line.stream,
                            text: line.text,
                        }));
                    }
                    let outcome = handle.outcome();
                    if !matches!(outcome, run_registry::RunOutcome::Running) {
                        yield Ok(subscribe_ndjson_line(&done_event(&outcome)));
                        break;
                    }
                }
            }
        }
    };

    Ok(Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/x-ndjson")
        .body(Body::from_stream(stream))
        .unwrap())
}

/// Forgets every block's session-freshness record (`AppState::session_runs`)
/// — the next "⛓ run chain" re-runs every pulled-in dependency for real
/// instead of skipping whichever ones still look unchanged since their last
/// run this session. Purely in-memory bookkeeping, so this never touches
/// the canvas file itself or any persisted `<!-- meshfox:output ... -->`
/// cache (`crate::output`/a block's own `cache` flag) — those are a
/// separate, deliberately-persisted mechanism, not what "session" refers
/// to here. See TODO.canvas.md: "Сброс сессии".
async fn reset_session(State(state): State<Arc<AppState>>) -> StatusCode {
    state.session_runs.lock().unwrap().clear();
    state.session_vars.lock().unwrap().clear();
    StatusCode::NO_CONTENT
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WatchQuery {
    /// The last `seq` this client already saw, from a previous connection —
    /// omitted (or, equivalently, any value at or past the buffer's own
    /// live edge) means "no backlog interest, just start me from now", the
    /// shape a first-time (non-reconnecting) client wants. See
    /// `canvas_events::CanvasEventLog::subscribe_from`.
    #[serde(default = "default_watch_since")]
    since: u64,
}

fn default_watch_since() -> u64 {
    u64::MAX
}

/// `{"type":"connected","resync":bool}` — sent once, right after upgrade.
/// `resync: true` means the client's requested `since` had already fallen
/// out of the backlog buffer, so whatever backlog follows (if any) can't be
/// trusted as complete: the client should do a full canvas refetch right
/// away rather than wait for a future event to prompt it.
fn watch_connected_msg(resync: bool) -> Message {
    Message::Text(
        serde_json::json!({"type": "connected", "resync": resync}).to_string(),
    )
}

/// One `ServerEvent` off `canvas_events`, still exactly the same
/// `{"type":"changed"}`/`{"type":"run-started",...}` shape the old NDJSON
/// stream already sent — just with `seq` spliced into the same object so a
/// later reconnect can resume from exactly here. `ServerEvent`'s own
/// `#[serde(tag = "type")]` and a plain extra field can't both come from one
/// `#[derive(Serialize)]` type without fighting serde's enum tagging, so
/// this splices `seq` into the already-serialized value directly instead.
fn watch_event_msg(item: &canvas_events::SeqEvent) -> Message {
    let mut value = serde_json::to_value(&item.item).expect("ServerEvent always serializes");
    if let serde_json::Value::Object(map) = &mut value {
        map.insert("seq".to_string(), serde_json::Value::from(item.seq));
    }
    Message::Text(value.to_string())
}

/// The live broadcast receiver itself fell behind (unrelated to the initial
/// backlog gap `watch_connected_msg` reports — this is the *live* tail
/// lagging, a receiver-side buffer overrun) — same "can't promise
/// completeness, resync" signal, sent mid-stream instead of just at connect
/// time.
fn watch_resync_msg() -> Message {
    Message::Text(serde_json::json!({"type": "resync"}).to_string())
}

/// `GET /api/watch?since=<seq>` — a WebSocket, one long-lived connection per
/// open browser tab. Forwards every `ServerEvent` (`canvas_events`) —
/// canvas mutations from any client, autorun triggers, an externally-edited
/// file the watcher thread noticed — so the UI can reload, and doubles as
/// both tab tracking (`TabGuard` above) and the client's own liveness check
/// on the server (the socket simply closes the moment this process exits).
/// `since` lets a reconnecting client ask "did I miss anything" instead of
/// just resuming blind — see `WatchWireMessage::Connected`.
async fn watch_changes(
    State(state): State<Arc<AppState>>,
    Query(query): Query<WatchQuery>,
    ws: WebSocketUpgrade,
) -> Response {
    state.open_tabs.fetch_add(1, Ordering::SeqCst);
    state.ever_connected.store(true, Ordering::SeqCst);
    ws.on_upgrade(move |socket| relay_canvas_events(state, socket, query.since))
}

async fn relay_canvas_events(state: Arc<AppState>, mut socket: WebSocket, since: u64) {
    // Dropped when this task ends (i.e. the client disconnects) — see
    // `TabGuard`'s own doc comment.
    let _guard = TabGuard { state: Arc::clone(&state) };
    let (backlog, mut rx, gap) = state.canvas_events.subscribe_from(since);

    if socket.send(watch_connected_msg(gap)).await.is_err() {
        return;
    }
    for item in &backlog {
        if socket.send(watch_event_msg(item)).await.is_err() {
            return;
        }
    }

    loop {
        tokio::select! {
            event = rx.recv() => {
                let msg = match event {
                    Ok(item) => watch_event_msg(&item),
                    // Fell behind the *live* broadcast channel's own buffer
                    // (distinct from the backlog-buffer gap checked above) —
                    // same "can't guarantee completeness" posture.
                    Err(broadcast::error::RecvError::Lagged(_)) => watch_resync_msg(),
                    // Never actually fires: `canvas_events`'s sender lives in
                    // `AppState`, which outlives every connection.
                    Err(broadcast::error::RecvError::Closed) => break,
                };
                if socket.send(msg).await.is_err() {
                    break;
                }
            }
            msg = socket.recv() => {
                match msg {
                    // This channel is server→client only; a client never
                    // needs to send anything meaningful, just closes when
                    // it's done watching.
                    Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                    Some(Ok(_)) => {}
                }
            }
        }
    }
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
        .any(|base| *base == requested_dir);
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
async fn build_state(
    canvas_path: PathBuf,
    auto_exit: bool,
    watcher_socket: Option<PathBuf>,
) -> std::io::Result<Arc<AppState>> {
    let raw = std::fs::read_to_string(&canvas_path)?;
    if let Err(e) = Canvas::from_markdown(&raw) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            e.to_string(),
        ));
    }

    let vars_cache = VarCache::load(&canvas_path)?;

    Ok(Arc::new(AppState {
        canvas_path,
        raw: Mutex::new(raw),
        runs: Mutex::new(HashMap::new()),
        vars_cache: Mutex::new(vars_cache),
        open_tabs: AtomicUsize::new(0),
        ever_connected: AtomicBool::new(false),
        canvas_events: canvas_events::CanvasEventLog::new(),
        auto_exit,
        link_preview_cache: link_preview::PreviewCache::new(),
        session_runs: Mutex::new(HashMap::new()),
        session_vars: Mutex::new(HashMap::new()),
        services: Mutex::new(HashMap::new()),
        runs_registry: Mutex::new(HashMap::new()),
        tty_registry: Mutex::new(HashMap::new()),
        debug_sessions: Mutex::new(HashMap::new()),
        watcher_socket,
    }))
}

fn build_app(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/api/canvas", get(get_canvas).put(put_canvas))
        .route("/api/canvas/raw", get(get_canvas_raw).put(put_canvas_raw))
        .route("/api/includes", get(get_includes))
        .route("/api/canvas/clear-layout", post(clear_layout))
        .route("/api/canvas/reorder-siblings", post(reorder_siblings))
        .route("/api/nodes", post(create_node))
        .route("/api/nodes/:id", patch(update_node).delete(remove_node))
        .route("/api/nodes/:id/reparent", post(reparent_node))
        .route("/api/nodes/:id/move", post(move_sibling))
        .route("/api/nodes/:id/block/:block_name", patch(update_block_attrs))
        .route("/api/nodes/:id/clear-layout", post(clear_node_layout))
        .route("/api/nodes/:id/rename-id", post(rename_node_id))
        .route("/api/nodes/:id/clear-id", post(clear_node_id))
        .route("/api/nodes/:id/file-content", get(get_node_file_content))
        .route("/api/nodes/:id/run", get(run_file_node))
        .route("/api/nodes/:id/open", post(open_node_file))
        .route("/api/nodes/:id/open-folder", post(open_node_file_folder))
        .route("/api/options", put(put_options))
        .route("/api/vars", get(get_vars))
        .route(
            "/api/vars/configure",
            get(get_configure_vars).post(post_configure_vars),
        )
        .route("/api/link-preview", get(get_link_preview))
        .route("/api/run", get(run_block))
        .route("/api/run/force", get(force_run))
        .route("/api/form/fields", get(get_form_fields))
        .route("/api/form/submit", post(submit_form))
        .route("/api/run/subscribe", get(subscribe_run))
        .route("/api/run/tty", get(run_block_tty))
        .route("/api/run/tty/attach", get(attach_tty))
        .route("/api/kill", post(kill_run))
        .route("/api/runs", get(get_active_runs))
        .route("/api/services", get(get_services))
        .route("/api/services/log", get(get_service_log))
        .route("/api/services/stop", post(stop_service))
        .route("/api/services/restart", post(restart_service))
        .route("/api/services/force-start", post(force_start_service))
        .route("/api/debug/start", post(debug_start))
        .route("/api/debug/send", post(debug_send))
        .route("/api/debug/stop", post(debug_stop))
        .route("/api/session/reset", post(reset_session))
        .route("/api/watch", get(watch_changes))
        .route("/api/include-asset", get(get_include_asset))
        .route("/api/syntax", get(get_syntax_list))
        .route("/api/syntax/:name", get(get_syntax_file))
        .fallback(serve_embedded)
        .with_state(state)
        .layer(CorsLayer::permissive())
}

/// Serves `canvas_path` on `127.0.0.1:<port>` until the process is killed
/// (or, when `auto_exit` is on, until every `/api/watch`-connected tab has
/// closed — see `TabGuard`). `port` of `0` asks the OS to assign a free
/// port instead — the actual bound port is read back from the listener
/// below.
///
/// Before doing any of that, contends for `canvas_path`'s own
/// `worker_lock` (see that module's doc comment) — if another, unrelated
/// `meshfox view` invocation (a *different* watcher process, not just a
/// second tab of this one) is already serving this exact file, this call
/// reports that worker's existing port to its own watcher instead of
/// binding a second listener, and returns immediately without ever
/// building state or serving anything itself.
///
/// `watcher_socket`, when given, names the coordinator this worker reports
/// its own bound port to (`watcher_protocol::notify_ready`) and forwards a
/// `.canvas.md` "↗ open" to (`open_node_file`) — see that module's own doc
/// comment. This worker never opens a browser tab itself, for itself or
/// anything else — that's the coordinator's job, always, uniformly,
/// whether this is the very first canvas a `meshfox view <path>` invocation
/// asked for or one navigated to afterward.
///
/// `quiet` suppresses every direct `println!`/`eprintln!` below — for a
/// caller that embeds this as a background worker inside some other
/// terminal UI of its own (the TUI, spawning this unconditionally at
/// startup so CLI/MCP/webui have something real to discover — see
/// `crates/cli/src/tui/mod.rs::run`) rather than being the whole process,
/// where a stray write to the shared stdout would corrupt its own
/// rendering. `meshfox view`'s own worker (`view_worker`, `main.rs`) passes
/// `false`, unchanged from before this parameter existed.
///
/// Thin wrapper around `serve_as_worker`: does the `worker_lock` decision
/// itself, then hands off. A caller that needs to make that same decision
/// *before* deciding whether to call this at all (the TUI, which needs to
/// know synchronously whether it's the worker or just found someone else's
/// — see `serve_as_worker`'s own doc comment) should call
/// `worker_lock::try_acquire` and `serve_as_worker` directly instead of
/// this, to avoid two separate `flock` attempts on the same file racing
/// each other from two tasks in the same process.
pub async fn run(
    canvas_path: PathBuf,
    port: u16,
    auto_exit: bool,
    watcher_socket: Option<PathBuf>,
    quiet: bool,
) -> std::io::Result<()> {
    let lock_guard = match worker_lock::try_acquire(&canvas_path) {
        Ok(worker_lock::Acquired::Us(guard)) => Some(guard),
        Ok(worker_lock::Acquired::Other { port: existing_port }) => {
            if !quiet {
                println!(
                    "meshfox: {} is already served on port {existing_port} — reusing that worker instead of starting a new one",
                    canvas_path.display()
                );
            }
            if let Some(socket) = &watcher_socket {
                if let Err(e) =
                    watcher_protocol::notify_ready(socket, &canvas_path, existing_port).await
                {
                    if !quiet {
                        eprintln!(
                            "meshfox: couldn't reach the watcher to report the existing worker's port ({e})"
                        );
                    }
                }
            }
            return Ok(());
        }
        Err(e) => {
            if !quiet {
                eprintln!(
                    "meshfox: couldn't acquire the worker lock for {} ({e}) — serving anyway, without cross-invocation dedup",
                    canvas_path.display()
                );
            }
            None
        }
    };

    serve_as_worker(canvas_path, port, auto_exit, watcher_socket, quiet, lock_guard, None).await
}

/// Actually binds and serves `canvas_path`, given a `worker_lock` decision
/// the caller already made (`run`'s own `try_acquire` match, above, or the
/// TUI's identical one at startup — see `crates/cli/src/tui/mod.rs::run`).
/// `lock_guard` is `Some` when the caller confirmed it's the one to serve
/// this file (holding it keeps that true for as long as this future lives
/// — see `worker_lock::LockGuard`'s own doc comment), `None` only for
/// `run`'s own "the lock itself couldn't even be read — serve anyway,
/// without cross-invocation dedup" degrade path.
///
/// `ready_tx`, when given, is sent the actual bound port the moment it's
/// known — for a caller that needs it synchronously to make its own HTTP
/// calls back to this same worker (again, the TUI: it spawns this as a
/// background task and awaits `ready_tx`'s receiver once, right after, to
/// learn its own port — see that module's own doc comment). `run`'s own
/// callers don't need this (they learn the port via stdout or
/// `watcher_protocol::notify_ready` instead), so it passes `None`.
pub async fn serve_as_worker(
    canvas_path: PathBuf,
    port: u16,
    auto_exit: bool,
    watcher_socket: Option<PathBuf>,
    quiet: bool,
    mut lock_guard: Option<worker_lock::LockGuard>,
    ready_tx: Option<tokio::sync::oneshot::Sender<u16>>,
) -> std::io::Result<()> {
    let state = build_state(canvas_path.clone(), auto_exit, watcher_socket.clone()).await?;
    spawn_file_watcher(Arc::clone(&state));
    spawn_shutdown_signal_handler(Arc::clone(&state));
    let app = build_app(state);

    let listener = tokio::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], port))).await?;
    let addr = listener.local_addr()?;
    if !quiet {
        println!(
            "meshfox: serving {} on http://{addr}",
            canvas_path.display()
        );
    }

    if let Some(guard) = &mut lock_guard {
        if let Err(e) = guard.write_port(addr.port()) {
            if !quiet {
                eprintln!("meshfox: couldn't record this worker's port in its lock file ({e})");
            }
        }
    }

    if let Some(tx) = ready_tx {
        let _ = tx.send(addr.port());
    }

    // Best-effort, same reasoning `open_browser` used to have for
    // `open::that` failing: a watcher that's gone, or was never given at
    // all, shouldn't stop this worker from serving its own canvas — it
    // just means nobody opens a browser tab for it, and this worker's own
    // future cross-canvas navigation attempts will fail too (that failure
    // *does* surface, in `open_node_file`).
    if let Some(socket) = &watcher_socket {
        if let Err(e) = watcher_protocol::notify_ready(socket, &canvas_path, addr.port()).await {
            if !quiet {
                eprintln!("meshfox: couldn't reach the watcher to report this worker's port ({e})");
            }
        }
    }

    axum::serve(listener, app).await
}

/// Ties every process this worker owns to this process's own lifetime for
/// real — not just the graceful "no tabs left" path (`TabGuard`, which
/// deliberately stays *up* while a service runs), but an involuntary
/// external termination too: `SIGTERM` (what a VS Code webview tab closing
/// sends this worker, `editors/vscode/src/coordinator.ts`'s
/// `killWorker`/`dispose` — deliberately *not* changed to check for
/// running services first, see SPEC.md's "Service blocks (experimental)"),
/// `SIGINT` (Ctrl-C), `SIGHUP` (the owning terminal closing). Covers all
/// three registries this process tracks a live child process under —
/// `services` (a `service` block, `ServiceHandle::stop`'s whole-process-
/// group kill, same as the panel's own Stop button), `runs_registry` (a
/// *plain* block still mid-run, `RunHandle::kill`, same as the web UI's
/// own Kill button), `tty_registry` (an attached interactive terminal,
/// `TtySessionHandle::kill`) — every one of these spawns its own process
/// (`stream_exec::spawn_bash`'s own `process_group(0)`) in its *own*
/// process group, deliberately not this one (so each can be killed
/// selectively, by address, without also reaching an unrelated sibling or
/// this process itself) — which also means none of them dies on its own
/// just because this process does; nothing but this explicit sweep stops
/// any of them from surviving as an orphan (confirmed directly: before
/// `runs_registry`/`tty_registry` were added here, a plain block that was
/// still mid-run when the worker got `SIGTERM`'d kept right on running,
/// see `crates/cli/tests/service_shutdown_cmd.rs`'s own two tests).
/// `SIGKILL` can't be handled here or anywhere — POSIX makes it
/// uncatchable by design, so a hard `kill -9` (or the OOM killer) is the
/// one termination path nothing can stop from orphaning any of these.
fn spawn_shutdown_signal_handler(state: Arc<AppState>) {
    tokio::spawn(async move {
        let Ok(mut sigterm) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        else {
            return;
        };
        let Ok(mut sighup) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup())
        else {
            return;
        };
        tokio::select! {
            _ = sigterm.recv() => {}
            _ = sighup.recv() => {}
            _ = tokio::signal::ctrl_c() => {}
        }
        let service_handles: Vec<_> = state.services.lock().unwrap().drain().map(|(_, h)| h).collect();
        for handle in service_handles {
            let _ = handle.stop();
        }
        let run_handles: Vec<_> = state.runs_registry.lock().unwrap().drain().map(|(_, h)| h).collect();
        for handle in run_handles {
            handle.kill();
        }
        let tty_handles: Vec<_> = state.tty_registry.lock().unwrap().drain().map(|(_, h)| h).collect();
        for handle in tty_handles {
            handle.kill();
        }
        // Unlike `ServiceHandle::stop` above (a synchronous `libc::kill`
        // call, already fully done by the time its loop returns),
        // `RunHandle::kill`/`TtySessionHandle::kill` only *signal* their
        // own already-spawned drain task via a oneshot channel — the
        // actual `libc::kill` for one of these only happens once that
        // task is next polled and notices it (see `run_registry::attach`'s
        // own `tokio::select!` loop). `std::process::exit` right after
        // sending those signals would very likely beat the scheduler to
        // it and exit before any of them ran at all (confirmed directly —
        // without this, a plain block kept right on running after the
        // signals above were sent). A brief real sleep, not just a
        // cooperative `yield_now`, gives the runtime's other worker
        // threads an actual window to pick up and finish each one first.
        tokio::time::sleep(Duration::from_millis(200)).await;
        std::process::exit(0);
    });
}

/// Binds `canvas_path` to an OS-assigned local port and serves it in the
/// background (`tokio::spawn`), returning the bound address — the
/// `#[cfg(test)]`-only entry point integration tests use to drive the real
/// HTTP/WebSocket API without going through `run`'s CLI-oriented setup
/// (`println!`, browser-opening, auto-exit).
#[cfg(test)]
async fn spawn_test_server(canvas_path: PathBuf) -> SocketAddr {
    let state = build_state(canvas_path, false, None)
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
        let state = build_state(canvas_path.clone(), false, None)
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

    // TODO.canvas.md: "Способ удалить координаты для конкретной ноды" —
    // `clear_node_layout` is `clear_layout` narrowed to one id.
    const TWO_POSITIONED: &str = concat!(
        "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
        "## A\n<!-- meshfox:node id=\"a\" x=10 y=20 w=100 h=60 color=\"1\" tags=\"keep-me\" -->\n\nbody a\n\n",
        "## B\n<!-- meshfox:node id=\"b\" x=200 y=300 w=100 h=60 -->\n\nbody b\n",
    );

    #[tokio::test]
    async fn clear_node_layout_clears_only_the_target_nodes_position() {
        let canvas_path = write_test_canvas(TWO_POSITIONED);
        let state = build_state(canvas_path.clone(), false, None)
            .await
            .expect("valid test canvas");

        let Json(updated) = match clear_node_layout(State(state), Path("a".to_string())).await {
            Ok(json) => json,
            Err(e) => panic!("clear-layout failed: {}", e.1),
        };

        let a = updated.node("a").expect("a still present");
        assert_eq!(a.x, None);
        assert_eq!(a.y, None);
        assert_eq!(a.width, None);
        assert_eq!(a.height, None);
        // Every other field survives untouched.
        assert_eq!(a.color.as_deref(), Some("1"));
        assert_eq!(a.tags, vec!["keep-me".to_string()]);
        // The sibling's own position is a separate node — untouched.
        let b = updated.node("b").expect("b still present");
        assert_eq!(b.x, Some(200.0));
        assert_eq!(b.y, Some(300.0));

        let _ = std::fs::remove_file(&canvas_path);
    }

    // Regression test for TODO.canvas.md: "VSCode: не подтягиваются
    // изменения файла, сделанные через другой коннект" — every mutation
    // used to update `state.raw` in memory but never tell any *other*
    // already-connected `/api/watch` tab about it (only
    // `spawn_file_watcher`'s polling thread ever broadcast a `changed`
    // event, and it deliberately skips this exact case — see `AppState::
    // save`'s own doc comment). A second tab on the same worker (VS Code's
    // "Open in Browser" reopens the very same worker, and so does opening
    // the same canvas twice) never found out about a sibling tab's edit
    // until manually reloaded.
    #[tokio::test]
    async fn save_broadcasts_a_changed_event_to_other_tabs() {
        let canvas_path = write_test_canvas(TWO_POSITIONED);
        let state = build_state(canvas_path.clone(), false, None)
            .await
            .expect("valid test canvas");
        // Subscribed *before* the mutation, exactly like a real `/api/watch`
        // connection that was already open when a sibling tab saved.
        let (_backlog, mut change_rx, _gap) = state.canvas_events.subscribe_from(0);

        if let Err(e) = clear_node_layout(State(state), Path("a".to_string())).await {
            panic!("clear-layout failed: {}", e.1);
        }

        assert!(
            change_rx.try_recv().is_ok(),
            "a save from one client should broadcast `changed` so every other \
             /api/watch-connected tab reloads too, not just the file-watcher's \
             own external-change poll"
        );

        let _ = std::fs::remove_file(&canvas_path);
    }

    #[tokio::test]
    async fn clear_node_layout_on_an_unknown_id_404s() {
        let canvas_path = write_test_canvas(TWO_POSITIONED);
        let state = build_state(canvas_path.clone(), false, None)
            .await
            .expect("valid test canvas");

        let err = match clear_node_layout(State(state), Path("does-not-exist".to_string())).await {
            Ok(_) => panic!("expected an error"),
            Err(e) => e,
        };
        assert_eq!(err.0, StatusCode::NOT_FOUND);

        let _ = std::fs::remove_file(&canvas_path);
    }
}

/// Every mutating `/api/nodes*` endpoint's own precise `ServerEvent`
/// broadcast (`NodeUpserted`/`NodeRemoved`/`NodesReordered`, or a
/// deliberate `Changed` fallback) — see `ServerEvent`'s own doc comment
/// and TODO.canvas.md's "WS push на мутации дерева" for the design this
/// implements. Same direct-call-the-handler-and-inspect-the-broadcast
/// pattern `clear_layout_tests::save_broadcasts_a_changed_event_to_other_tabs`
/// already established, just asserting on the specific variant/payload
/// instead of merely "something fired".
#[cfg(test)]
mod node_op_broadcast_tests {
    use super::*;

    fn write_test_canvas(contents: &str) -> PathBuf {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "meshfox-node-op-broadcast-test-{}.canvas.md",
            uuid::Uuid::new_v4()
        ));
        std::fs::write(&path, contents).unwrap();
        path
    }

    const TWO_SIBLINGS: &str = concat!(
        "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
        "## A\n<!-- meshfox:node id=\"a\" -->\n\nbody a\n\n",
        "## B\n<!-- meshfox:node id=\"b\" -->\n\nbody b\n",
    );

    /// Pulls the one `ServerEvent` a mutation just broadcast — panics if
    /// none arrived, same "a save must always broadcast something"
    /// invariant `save_broadcasts_a_changed_event_to_other_tabs` already
    /// checks, just also handing the event back for the caller's own
    /// variant/payload assertion.
    fn recv_event(rx: &mut broadcast::Receiver<canvas_events::SeqEvent>) -> ServerEvent {
        rx.try_recv().expect("mutation should have broadcast an event").item
    }

    #[tokio::test]
    async fn create_node_broadcasts_node_upserted_for_the_new_node() {
        let canvas_path = write_test_canvas(TWO_SIBLINGS);
        let state = build_state(canvas_path.clone(), false, None)
            .await
            .expect("valid test canvas");
        let (_backlog, mut rx, _gap) = state.canvas_events.subscribe_from(0);

        let req = CreateNodeRequest {
            parent_id: "root".to_string(),
            title: "New Child".to_string(),
            title_slug_id: false,
        };
        let Json(CreateNodeResponse { canvas, .. }) =
            create_node(State(state), Json(req)).await.expect("create should succeed");

        match recv_event(&mut rx) {
            ServerEvent::NodeUpserted { node } => {
                assert_eq!(node.title, "New Child");
                assert_eq!(node.parent.as_deref(), Some("root"));
                // The broadcast node's own id should be a real, addressable
                // id in the response the same request just got back — not
                // some placeholder a client couldn't actually look up.
                assert!(canvas.node(&node.id).is_some());
            }
            other => panic!("expected NodeUpserted, got {other:?}"),
        }

        let _ = std::fs::remove_file(&canvas_path);
    }

    #[tokio::test]
    async fn update_node_body_broadcasts_node_upserted_with_the_new_body() {
        let canvas_path = write_test_canvas(TWO_SIBLINGS);
        let state = build_state(canvas_path.clone(), false, None)
            .await
            .expect("valid test canvas");
        let (_backlog, mut rx, _gap) = state.canvas_events.subscribe_from(0);

        let req = UpdateNodeRequest {
            title: None,
            node_type: None,
            color: None,
            target: None,
            text: Some("new body a".to_string()),
            extra_parents: None,
            display: None,
            lang: None,
            interpreter: None,
            preview: None,
            tags: None,
            edge_label: None,
            fold: None,
            x: None,
            y: None,
            width: None,
            height: None,
            created_at: None,
        };
        let _ = update_node(State(state), Path("a".to_string()), Json(req))
            .await
            .expect("update should succeed");

        match recv_event(&mut rx) {
            ServerEvent::NodeUpserted { node } => {
                assert_eq!(node.id, "a");
                assert_eq!(node.text, "new body a");
            }
            other => panic!("expected NodeUpserted, got {other:?}"),
        }

        let _ = std::fs::remove_file(&canvas_path);
    }

    #[tokio::test]
    async fn remove_node_default_broadcasts_node_removed() {
        let canvas_path = write_test_canvas(TWO_SIBLINGS);
        let state = build_state(canvas_path.clone(), false, None)
            .await
            .expect("valid test canvas");
        let (_backlog, mut rx, _gap) = state.canvas_events.subscribe_from(0);

        let _ = remove_node(State(state), Path("a".to_string()), Query(DeleteNodeQuery { children: None }))
            .await
            .expect("remove should succeed");

        match recv_event(&mut rx) {
            ServerEvent::NodeRemoved { node_id, keep_children } => {
                assert_eq!(node_id, "a");
                assert!(!keep_children);
            }
            other => panic!("expected NodeRemoved, got {other:?}"),
        }

        let _ = std::fs::remove_file(&canvas_path);
    }

    // The `?children=reparent` branch changes every promoted child's own
    // `parent` too, not just removing the target — too much for a single
    // `NodeRemoved` to describe accurately (see that call site's own
    // comment), so it deliberately falls back to a plain `Changed` instead.
    #[tokio::test]
    async fn remove_node_with_reparent_children_falls_back_to_changed() {
        let canvas_path = write_test_canvas(concat!(
            "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
            "## A\n<!-- meshfox:node id=\"a\" -->\n\nbody a\n\n",
            "### A1\n<!-- meshfox:node id=\"a1\" -->\n\nbody a1\n",
        ));
        let state = build_state(canvas_path.clone(), false, None)
            .await
            .expect("valid test canvas");
        let (_backlog, mut rx, _gap) = state.canvas_events.subscribe_from(0);

        let _ = remove_node(
            State(state),
            Path("a".to_string()),
            Query(DeleteNodeQuery { children: Some("reparent".to_string()) }),
        )
        .await
        .expect("remove should succeed");

        match recv_event(&mut rx) {
            ServerEvent::Changed => {}
            other => panic!("expected a Changed fallback, got {other:?}"),
        }

        let _ = std::fs::remove_file(&canvas_path);
    }

    #[tokio::test]
    async fn move_sibling_broadcasts_the_whole_new_sibling_order() {
        let canvas_path = write_test_canvas(TWO_SIBLINGS);
        let state = build_state(canvas_path.clone(), false, None)
            .await
            .expect("valid test canvas");
        let (_backlog, mut rx, _gap) = state.canvas_events.subscribe_from(0);

        // "a" starts before "b" — move it to sit after "b" instead.
        let req = MoveSiblingRequest { before: None, after: Some("b".to_string()) };
        let _ = move_sibling(State(state), Path("a".to_string()), Json(req))
            .await
            .expect("move should succeed");

        match recv_event(&mut rx) {
            ServerEvent::NodesReordered { parent_id, child_ids } => {
                assert_eq!(parent_id, "root");
                assert_eq!(child_ids, vec!["b".to_string(), "a".to_string()]);
            }
            other => panic!("expected NodesReordered, got {other:?}"),
        }

        let _ = std::fs::remove_file(&canvas_path);
    }

    // An id rename ripples into every other node's own `parent=`/`meshfox:
    // edge from=` reference — too much for `NodeUpserted`'s single-node
    // shape, so it deliberately keeps the generic `Changed` (a full
    // reload) instead of a precise op.
    #[tokio::test]
    async fn rename_node_id_falls_back_to_changed() {
        let canvas_path = write_test_canvas(TWO_SIBLINGS);
        let state = build_state(canvas_path.clone(), false, None)
            .await
            .expect("valid test canvas");
        let (_backlog, mut rx, _gap) = state.canvas_events.subscribe_from(0);

        let req = RenameNodeIdRequest { new_id: "a-renamed".to_string() };
        let _ = rename_node_id(State(state), Path("a".to_string()), Json(req))
            .await
            .expect("rename should succeed");

        match recv_event(&mut rx) {
            ServerEvent::Changed => {}
            other => panic!("expected a Changed fallback, got {other:?}"),
        }

        let _ = std::fs::remove_file(&canvas_path);
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
        let state = build_state(canvas_path.clone(), false, None)
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
        let state = build_state(canvas_path.clone(), false, None)
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
        let state = build_state(canvas_path.clone(), false, None)
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

    const ABC_CANVAS: &str = concat!(
        "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
        "## A\n<!-- meshfox:node id=\"a\" -->\n\nbody a\n\n",
        "## B\n<!-- meshfox:node id=\"b\" -->\n\nbody b\n\n",
        "## C\n<!-- meshfox:node id=\"c\" -->\n\nbody c\n",
    );

    fn order(canvas: &Canvas) -> Vec<&str> {
        canvas.nodes.iter().map(|n| n.id.as_str()).collect()
    }

    #[tokio::test]
    async fn move_sibling_moves_a_node_before_a_target() {
        let canvas_path = write_test_canvas(ABC_CANVAS);
        let state = build_state(canvas_path.clone(), false, None)
            .await
            .expect("valid test canvas");

        let updated = expect_ok(
            move_sibling(
                State(state),
                Path("c".to_string()),
                Json(MoveSiblingRequest {
                    before: Some("a".to_string()),
                    after: None,
                }),
            )
            .await,
        );
        assert_eq!(order(&updated), vec!["root", "c", "a", "b"]);

        let _ = std::fs::remove_file(&canvas_path);
    }

    #[tokio::test]
    async fn move_sibling_moves_a_node_after_a_target() {
        let canvas_path = write_test_canvas(ABC_CANVAS);
        let state = build_state(canvas_path.clone(), false, None)
            .await
            .expect("valid test canvas");

        let updated = expect_ok(
            move_sibling(
                State(state),
                Path("a".to_string()),
                Json(MoveSiblingRequest {
                    before: None,
                    after: Some("c".to_string()),
                }),
            )
            .await,
        );
        assert_eq!(order(&updated), vec!["root", "b", "c", "a"]);

        let _ = std::fs::remove_file(&canvas_path);
    }

    #[tokio::test]
    async fn move_sibling_rejects_a_non_sibling_target() {
        let canvas: &str = concat!(
            "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
            "## A\n<!-- meshfox:node id=\"a\" -->\n\n",
            "### A Child\n<!-- meshfox:node id=\"a-child\" -->\n\nbody\n\n",
            "## B\n<!-- meshfox:node id=\"b\" -->\n\nbody b\n",
        );
        let canvas_path = write_test_canvas(canvas);
        let state = build_state(canvas_path.clone(), false, None)
            .await
            .expect("valid test canvas");

        let err = match move_sibling(
            State(state),
            Path("a-child".to_string()),
            Json(MoveSiblingRequest {
                before: Some("b".to_string()),
                after: None,
            }),
        )
        .await
        {
            Ok(_) => panic!("expected an error"),
            Err(e) => e,
        };
        assert_eq!(err.0, StatusCode::UNPROCESSABLE_ENTITY);

        let _ = std::fs::remove_file(&canvas_path);
    }

    #[tokio::test]
    async fn move_sibling_rejects_a_request_naming_neither_or_both_of_before_after() {
        let canvas_path = write_test_canvas(ABC_CANVAS);
        let state = build_state(canvas_path.clone(), false, None)
            .await
            .expect("valid test canvas");

        let err = match move_sibling(
            State(state.clone()),
            Path("c".to_string()),
            Json(MoveSiblingRequest {
                before: None,
                after: None,
            }),
        )
        .await
        {
            Ok(_) => panic!("expected an error"),
            Err(e) => e,
        };
        assert_eq!(err.0, StatusCode::UNPROCESSABLE_ENTITY);

        let err = match move_sibling(
            State(state),
            Path("c".to_string()),
            Json(MoveSiblingRequest {
                before: Some("a".to_string()),
                after: Some("b".to_string()),
            }),
        )
        .await
        {
            Ok(_) => panic!("expected an error"),
            Err(e) => e,
        };
        assert_eq!(err.0, StatusCode::UNPROCESSABLE_ENTITY);

        let _ = std::fs::remove_file(&canvas_path);
    }

    #[tokio::test]
    async fn move_sibling_on_an_unknown_id_404s() {
        let canvas_path = write_test_canvas(ABC_CANVAS);
        let state = build_state(canvas_path.clone(), false, None)
            .await
            .expect("valid test canvas");

        let err = match move_sibling(
            State(state),
            Path("does-not-exist".to_string()),
            Json(MoveSiblingRequest {
                before: Some("a".to_string()),
                after: None,
            }),
        )
        .await
        {
            Ok(_) => panic!("expected an error"),
            Err(e) => e,
        };
        assert_eq!(err.0, StatusCode::NOT_FOUND);

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
            x: None,
            y: None,
            width: None,
            height: None,
            created_at: None,
        }
    }

    fn expect_ok(result: Result<Json<Canvas>, ApiError>) -> Canvas {
        match result {
            Ok(Json(canvas)) => canvas,
            Err(e) => panic!("request failed: {}", e.1),
        }
    }

    fn expect_ok_create(result: Result<Json<CreateNodeResponse>, ApiError>) -> Canvas {
        match result {
            Ok(Json(response)) => response.canvas,
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

    /// A plain, single-file canvas — for tests here that don't care about
    /// include-splicing at all, unlike this module's own
    /// `write_base_and_child_canvas`.
    fn write_simple_canvas(contents: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "meshfox-include-edit-test-simple-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("doc.canvas.md");
        std::fs::write(&path, contents).unwrap();
        path
    }

    const TWO_SIBLINGS: &str = concat!(
        "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
        "## A\n<!-- meshfox:node id=\"a\" -->\n\nbody a\n\n",
        "## B\n<!-- meshfox:node id=\"b\" -->\n\nbody b\n",
    );

    #[tokio::test]
    async fn update_node_edits_an_included_nodes_own_file_not_the_primary_one() {
        let base_path = write_base_and_child_canvas();
        let child_path = base_path.parent().unwrap().join("child.canvas.md");
        let base_before = std::fs::read_to_string(&base_path).unwrap();
        let state = build_state(base_path.clone(), false, None)
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
        let state = build_state(base_path.clone(), false, None)
            .await
            .expect("valid test canvas");

        let updated = expect_ok_create(
            create_node(
                State(state),
                Json(CreateNodeRequest {
                    parent_id: "child/root".to_string(),
                    title: "New Kid".to_string(),
                    title_slug_id: false,
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
        let state = build_state(base_path.clone(), false, None)
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
        let state = build_state(base_path.clone(), false, None)
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
    async fn move_sibling_resolves_within_the_included_file() {
        // `write_base_and_child_canvas`'s child only has one child of its
        // own ("leaf") — not enough siblings to move anything relative to.
        // Give it a second one first via the ordinary create-node path.
        let base_path = write_base_and_child_canvas();
        let child_path = base_path.parent().unwrap().join("child.canvas.md");
        let state = build_state(base_path.clone(), false, None)
            .await
            .expect("valid test canvas");

        let created = expect_ok_create(
            create_node(
                State(state.clone()),
                Json(CreateNodeRequest {
                    parent_id: "child/root".to_string(),
                    title: "Second Leaf".to_string(),
                    title_slug_id: false,
                }),
            )
            .await,
        );
        // `create_node` assigns a random id, not a slug of the title (see
        // `insert_child_node_random_id`) — look it up by title instead of
        // assuming "child/second-leaf".
        let second_leaf_id = created
            .nodes
            .iter()
            .find(|n| n.title == "Second Leaf")
            .expect("new node present in the response")
            .id
            .clone();

        let updated = expect_ok(
            move_sibling(
                State(state),
                Path(second_leaf_id.clone()),
                Json(MoveSiblingRequest {
                    before: Some("child/leaf".to_string()),
                    after: None,
                }),
            )
            .await,
        );

        let root = updated.node("child/root").expect("child/root present");
        let order: Vec<&str> = updated
            .nodes
            .iter()
            .filter(|n| n.parent.as_deref() == Some(&root.id))
            .map(|n| n.id.as_str())
            .collect();
        assert_eq!(order, vec![second_leaf_id.as_str(), "child/leaf"]);
        // The move landed in the include target's own file, not the
        // primary document — same "writes into the include target, not
        // base.canvas.md" contract every other mutating endpoint here
        // already has — with the reordering visible in its raw heading
        // order too, not just the resolved response.
        let local_id = second_leaf_id.strip_prefix("child/").unwrap();
        let child_raw = std::fs::read_to_string(&child_path).unwrap();
        assert!(
            child_raw.find(&format!("id=\"{local_id}\"")).unwrap() < child_raw.find("id=\"leaf\"").unwrap()
        );

        let _ = std::fs::remove_dir_all(base_path.parent().unwrap());
    }

    #[tokio::test]
    async fn clear_node_layout_resolves_within_the_included_file() {
        let dir = std::env::temp_dir().join(format!(
            "meshfox-clear-node-layout-test-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let child_path = dir.join("child.canvas.md");
        std::fs::write(
            &child_path,
            concat!(
                "<!-- meshfox:canvas -->\n# Child\n<!-- meshfox:node id=\"root\" -->\n\nintro\n\n",
                "## Leaf\n<!-- meshfox:node id=\"leaf\" x=50 y=60 w=100 h=60 -->\n\nleaf body\n",
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
        let base_before = std::fs::read_to_string(&base_path).unwrap();
        let state = build_state(base_path.clone(), false, None)
            .await
            .expect("valid test canvas");

        let Json(updated) =
            match clear_node_layout(State(state), Path("child/leaf".to_string())).await {
                Ok(json) => json,
                Err(e) => panic!("clear-layout failed: {}", e.1),
            };
        let leaf = updated.node("child/leaf").expect("child/leaf present");
        assert_eq!(leaf.x, None);
        assert_eq!(leaf.y, None);
        // Landed in the include target's own file, not the primary
        // document — same contract every other mutating endpoint here has.
        assert_eq!(std::fs::read_to_string(&base_path).unwrap(), base_before);
        assert!(!std::fs::read_to_string(&child_path).unwrap().contains("x=50"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    fn expect_ok_clear(result: Result<Json<ClearNodeIdResponse>, ApiError>) -> ClearNodeIdResponse {
        match result {
            Ok(Json(body)) => body,
            Err(e) => panic!("request failed: {}", e.1),
        }
    }

    #[tokio::test]
    async fn clear_node_id_drops_the_attribute_on_the_primary_document() {
        let base_path = write_base_and_child_canvas();
        let state = build_state(base_path.clone(), false, None)
            .await
            .expect("valid test canvas");

        let body = expect_ok_clear(clear_node_id(State(state), Path("base".to_string())).await);

        assert_eq!(body.id, "base");
        assert!(body.canvas.node("base").is_some());
        assert!(!std::fs::read_to_string(&base_path)
            .unwrap()
            .contains(r#"id="base""#));

        let _ = std::fs::remove_dir_all(base_path.parent().unwrap());
    }

    #[tokio::test]
    async fn clear_node_id_resolves_within_the_included_file_and_reports_the_namespaced_id() {
        let base_path = write_base_and_child_canvas();
        let child_path = base_path.parent().unwrap().join("child.canvas.md");
        let state = build_state(base_path.clone(), false, None)
            .await
            .expect("valid test canvas");

        // "leaf"'s id is already `slugify("Leaf")` — clearing it should be
        // a no-op rename, just dropping the now-redundant attribute, still
        // reachable under the same namespaced id afterward.
        let body = expect_ok_clear(clear_node_id(State(state), Path("child/leaf".to_string())).await);

        assert_eq!(body.id, "child/leaf");
        assert!(body.canvas.node("child/leaf").is_some());
        assert!(!std::fs::read_to_string(&child_path).unwrap().contains(r#"id="leaf""#));
        // The primary document itself is untouched — same "writes into the
        // include target, not base.canvas.md" contract every other
        // mutating endpoint here already has.
        assert!(!std::fs::read_to_string(&base_path).unwrap().contains("leaf"));

        let _ = std::fs::remove_dir_all(base_path.parent().unwrap());
    }

    #[tokio::test]
    async fn clear_node_id_rederives_from_the_title_when_the_id_had_diverged() {
        let base_path = write_base_and_child_canvas();
        let state = build_state(base_path.clone(), false, None)
            .await
            .expect("valid test canvas");

        expect_ok(
            rename_node_id(
                State(state.clone()),
                Path("child/leaf".to_string()),
                Json(RenameNodeIdRequest {
                    new_id: "custom-id".to_string(),
                }),
            )
            .await,
        );

        let body =
            expect_ok_clear(clear_node_id(State(state), Path("child/custom-id".to_string())).await);

        assert_eq!(body.id, "child/leaf");
        assert!(body.canvas.node("child/leaf").is_some());
        assert!(body.canvas.node("child/custom-id").is_none());

        let _ = std::fs::remove_dir_all(base_path.parent().unwrap());
    }

    #[tokio::test]
    async fn clear_node_id_on_an_unknown_id_404s() {
        let base_path = write_base_and_child_canvas();
        let state = build_state(base_path.clone(), false, None)
            .await
            .expect("valid test canvas");

        let err = match clear_node_id(State(state), Path("does-not-exist".to_string())).await {
            Ok(_) => panic!("expected an error"),
            Err(e) => e,
        };
        assert_eq!(err.0, StatusCode::NOT_FOUND);

        let _ = std::fs::remove_dir_all(base_path.parent().unwrap());
    }

    #[tokio::test]
    async fn a_node_id_containing_a_space_round_trips_through_rename_and_update() {
        // TODO.canvas.md: ids in arbitrary scripts/with spaces shouldn't
        // break routing (`Path<String>` extraction) or reference tracking
        // — exercised here on the primary document; the client's own
        // `encodeURIComponent` on every id-bearing request handles the
        // transport side (see `web/src/api.ts`).
        let base_path = write_base_and_child_canvas();
        let state = build_state(base_path.clone(), false, None)
            .await
            .expect("valid test canvas");

        expect_ok(
            rename_node_id(
                State(state.clone()),
                Path("base".to_string()),
                Json(RenameNodeIdRequest {
                    new_id: "has space".to_string(),
                }),
            )
            .await,
        );

        let mut req = blank_update_request();
        req.text = Some("updated via a spaced id".to_string());
        let updated = expect_ok(update_node(State(state), Path("has space".to_string()), Json(req)).await);
        assert_eq!(
            updated.node("has space").unwrap().text,
            "updated via a spaced id"
        );

        let _ = std::fs::remove_dir_all(base_path.parent().unwrap());
    }

    #[tokio::test]
    async fn update_node_patching_target_preserves_an_existing_caption() {
        // `NodeSettings`' "URL/File path" field only ever means "change the
        // link" — this used to silently drop whatever caption (see
        // `Node::caption`) was already there, since the old code rebuilt
        // the whole body as just `[title](target)`.
        let dir = std::env::temp_dir().join(format!(
            "meshfox-caption-preserve-test-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let base_path = dir.join("base.canvas.md");
        std::fs::write(
            &base_path,
            concat!(
                "<!-- meshfox:canvas -->\n# Base\n<!-- meshfox:node id=\"base\" -->\n\n",
                "## LinkedIn\n<!-- meshfox:node id=\"linkedin\" type=\"link\" -->\n\n",
                "[post](https://old.example)\n\nA short note.\n",
            ),
        )
        .unwrap();
        let state = build_state(base_path.clone(), false, None)
            .await
            .expect("valid test canvas");

        let mut req = blank_update_request();
        req.target = Some("https://new.example".to_string());
        let updated =
            expect_ok(update_node(State(state), Path("linkedin".to_string()), Json(req)).await);
        let node = updated.node("linkedin").unwrap();
        assert_eq!(node.target.as_deref(), Some("https://new.example"));
        assert_eq!(node.caption.as_deref(), Some("A short note."));

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `x`/`y`/`width`/`height`/`createdAt` — added for `crate::coordinator`-
    /// routed CLI/MCP `node meta` (see `UpdateNodeRequest`'s own doc
    /// comment); the existing web client never sends these through this
    /// endpoint at all, so this is purely new surface, not a behavior
    /// change for anything already using it.
    #[tokio::test]
    async fn update_node_sets_position_size_and_created_at() {
        let canvas_path = write_simple_canvas(TWO_SIBLINGS);
        let state = build_state(canvas_path.clone(), false, None)
            .await
            .expect("valid test canvas");

        let mut req = blank_update_request();
        req.x = Some(10.0);
        req.y = Some(20.0);
        req.width = Some(200.0);
        req.height = Some(100.0);
        req.created_at = Some("2026-01-01T00:00:00Z".to_string());
        let updated = expect_ok(update_node(State(state), Path("a".to_string()), Json(req)).await);
        let node = updated.node("a").unwrap();
        assert_eq!(node.x, Some(10.0));
        assert_eq!(node.y, Some(20.0));
        assert_eq!(node.width, Some(200.0));
        assert_eq!(node.height, Some(100.0));
        assert_eq!(node.created_at.as_deref(), Some("2026-01-01T00:00:00Z"));

        let _ = std::fs::remove_file(&canvas_path);
    }

    #[tokio::test]
    async fn update_node_rejects_width_or_height_on_a_group() {
        let canvas_path = write_simple_canvas(concat!(
            "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
            "## Bag\n<!-- meshfox:node id=\"bag\" type=\"group\" -->\n",
        ));
        let state = build_state(canvas_path.clone(), false, None)
            .await
            .expect("valid test canvas");

        let mut req = blank_update_request();
        req.width = Some(200.0);
        let err = expect_err(update_node(State(state), Path("bag".to_string()), Json(req)).await);
        assert_eq!(err.0, StatusCode::UNPROCESSABLE_ENTITY);

        let _ = std::fs::remove_file(&canvas_path);
    }

    #[tokio::test]
    async fn update_node_rejects_an_invalid_created_at() {
        let canvas_path = write_simple_canvas(TWO_SIBLINGS);
        let state = build_state(canvas_path.clone(), false, None)
            .await
            .expect("valid test canvas");

        let mut req = blank_update_request();
        req.created_at = Some("not-a-date".to_string());
        let err = expect_err(update_node(State(state), Path("a".to_string()), Json(req)).await);
        assert_eq!(err.0, StatusCode::UNPROCESSABLE_ENTITY);

        let _ = std::fs::remove_file(&canvas_path);
    }

    /// `titleSlugId: true` — for `crate::coordinator`-routed CLI/MCP `node
    /// add`, which has always produced a title-slug id and needs the exact
    /// same scheme once routed through a worker (see `CreateNodeRequest`'s
    /// own doc comment). Default (`false`, or omitted) keeps today's random
    /// id, checked by the existing `create_node_broadcasts_node_upserted_
    /// for_the_new_node` test elsewhere.
    #[tokio::test]
    async fn create_node_with_title_slug_id_uses_a_readable_id() {
        let canvas_path = write_simple_canvas(TWO_SIBLINGS);
        let state = build_state(canvas_path.clone(), false, None)
            .await
            .expect("valid test canvas");

        let req = CreateNodeRequest {
            parent_id: "root".to_string(),
            title: "My New Child".to_string(),
            title_slug_id: true,
        };
        let Json(response) = create_node(State(state), Json(req))
            .await
            .expect("create should succeed");
        assert_eq!(response.new_id, "my-new-child");
        assert!(response.canvas.node("my-new-child").is_some());

        let _ = std::fs::remove_file(&canvas_path);
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
        let state = build_state(base_path.clone(), false, None)
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
        let state = build_state(base_path.clone(), false, None)
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
        let state = build_state(base_path.clone(), false, None)
            .await
            .expect("valid test canvas");

        let primary_raw = state.raw.lock().unwrap().clone();
        let mut canvas = resolved_canvas(&primary_raw, &state.canvas_path)
            .unwrap_or_else(|e| panic!("resolve failed: {}", e.1));
        canvas.node_mut("child/leaf").expect("child/leaf present").x = Some(123.0);
        canvas.node_mut("child/leaf").expect("child/leaf present").y = Some(456.0);

        let status = put_canvas(State(state), Json(PutCanvasRequest { canvas, layout_hints: HashMap::new() }))
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

    // TODO.canvas.md: "Одна перетащенная нода становится первой в списке" —
    // `layout_hints` lets `put_canvas` slot a freshly-positioned node in
    // among its still-unpositioned siblings by where the client's own
    // auto-layout actually rendered them, instead of `reorder_by_position`
    // always sorting it before every one of them (any real number beats
    // the implicit `f64::INFINITY` an unpositioned sibling sorts by
    // without a hint).
    #[tokio::test]
    async fn put_canvas_uses_layout_hints_to_place_a_positioned_node_among_auto_siblings() {
        let mut canvas_path = std::env::temp_dir();
        canvas_path.push(format!("meshfox-layout-hints-test-{}.canvas.md", uuid::Uuid::new_v4()));
        std::fs::write(
            &canvas_path,
            concat!(
                "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
                "## One\n<!-- meshfox:node id=\"one\" -->\n\nbody\n\n",
                "## Two\n<!-- meshfox:node id=\"two\" -->\n\nbody\n\n",
                "## Target\n<!-- meshfox:node id=\"target\" -->\n\nbody\n",
            ),
        )
        .unwrap();
        let state = build_state(canvas_path.clone(), false, None)
            .await
            .expect("valid test canvas");

        let primary_raw = state.raw.lock().unwrap().clone();
        let mut canvas =
            resolved_canvas(&primary_raw, &state.canvas_path).unwrap_or_else(|e| panic!("resolve failed: {}", e.1));
        // The client's own auto-layout rendered `one` at y=0 and `two` at
        // y=100 — `target`'s own newly-authored y=50 should land between
        // them, not before both.
        canvas.node_mut("target").expect("target present").x = Some(0.0);
        canvas.node_mut("target").expect("target present").y = Some(50.0);
        let layout_hints = HashMap::from([
            ("one".to_string(), LayoutHint { x: 0.0, y: 0.0 }),
            ("two".to_string(), LayoutHint { x: 0.0, y: 100.0 }),
        ]);

        let status = put_canvas(State(state), Json(PutCanvasRequest { canvas, layout_hints }))
            .await
            .unwrap_or_else(|e| panic!("put_canvas failed: {}", e.1));
        assert_eq!(status, StatusCode::NO_CONTENT);

        let after = std::fs::read_to_string(&canvas_path).unwrap();
        let pos = |id: &str| after.find(&format!("id=\"{id}\"")).unwrap();
        assert!(pos("one") < pos("target"), "after: {after}");
        assert!(pos("target") < pos("two"), "after: {after}");
        // A hint is never persisted as a real x/y on the nodes it's about.
        assert!(!after.contains("id=\"one\" x=") && !after.contains("id=\"one\" y="));
        assert!(!after.contains("id=\"two\" x=") && !after.contains("id=\"two\" y="));

        let _ = std::fs::remove_file(&canvas_path);
    }

    // Same mechanism as the primary-document test above, but for a node
    // living inside an `include` target — `put_canvas` has to split the
    // flat, client-namespaced `layout_hints` map into one *local*-id-keyed
    // map per file before handing it to that file's own
    // `reorder_by_position` call, the same routing `canvas.nodes` itself
    // already goes through.
    #[tokio::test]
    async fn put_canvas_layout_hints_are_routed_to_the_right_included_file() {
        let dir = std::env::temp_dir().join(format!(
            "meshfox-layout-hints-include-test-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("child.canvas.md"),
            concat!(
                "<!-- meshfox:canvas -->\n# Child\n<!-- meshfox:node id=\"root\" -->\n\n",
                "## One\n<!-- meshfox:node id=\"one\" -->\n\nbody\n\n",
                "## Two\n<!-- meshfox:node id=\"two\" -->\n\nbody\n\n",
                "## Target\n<!-- meshfox:node id=\"target\" -->\n\nbody\n",
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
        let state = build_state(base_path.clone(), false, None)
            .await
            .expect("valid test canvas");

        let primary_raw = state.raw.lock().unwrap().clone();
        let mut canvas =
            resolved_canvas(&primary_raw, &state.canvas_path).unwrap_or_else(|e| panic!("resolve failed: {}", e.1));
        canvas.node_mut("child/target").expect("child/target present").x = Some(0.0);
        canvas.node_mut("child/target").expect("child/target present").y = Some(50.0);
        // Keyed by the *namespaced* id the client actually sees, same as
        // every other node-addressed field in this request.
        let layout_hints = HashMap::from([
            ("child/one".to_string(), LayoutHint { x: 0.0, y: 0.0 }),
            ("child/two".to_string(), LayoutHint { x: 0.0, y: 100.0 }),
        ]);

        let status = put_canvas(State(state), Json(PutCanvasRequest { canvas, layout_hints }))
            .await
            .unwrap_or_else(|e| panic!("put_canvas failed: {}", e.1));
        assert_eq!(status, StatusCode::NO_CONTENT);

        let child_after = std::fs::read_to_string(dir.join("child.canvas.md")).unwrap();
        let pos = |id: &str| child_after.find(&format!("id=\"{id}\"")).unwrap();
        assert!(pos("one") < pos("target"), "child file: {child_after}");
        assert!(pos("target") < pos("two"), "child file: {child_after}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn get_includes_lists_the_child_canvas_include() {
        let base_path = write_base_and_child_canvas();
        let state = build_state(base_path.clone(), false, None)
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
        let state = build_state(base_path.clone(), false, None)
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
        let state = build_state(base_path.clone(), false, None)
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
        let state = build_state(base_path.clone(), false, None)
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

    async fn get(addr: SocketAddr, path: &str) -> (u16, String) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
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
    async fn tty_websocket_skips_an_unchanged_dependency_already_run_this_session() {
        // A non-`tty` `dep` pulled in as a dependency of a `tty` target —
        // regression coverage for the gap the plain (non-`tty`) `/api/run`
        // path already had `session_runs` skip-checking for, but this
        // WebSocket path didn't: previously a `tty` chain always re-ran
        // every dependency regardless of whether it had already succeeded
        // this session.
        let canvas_path = write_test_canvas(concat!(
            "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
            "```bash name=\"dep\" cache\necho dep-ran\n```\n\n",
            "```bash name=\"target\" tty deps=\"dep\"\necho ready; read line\n```\n",
        ));
        let addr = spawn_test_server(canvas_path.clone()).await;
        let url = format!("ws://{addr}/api/run/tty?path=&block=target&cols=80&rows=24");

        let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.expect("connect");
        next_event(&mut ws).await; // started
        let step_start = next_event(&mut ws).await;
        assert_eq!(step_start["type"], "step-start");
        assert_eq!(step_start["block"], "dep");
        let output = next_event(&mut ws).await;
        assert_eq!(output["type"], "output");
        assert_eq!(output["text"], "dep-ran");
        let step_end = next_event(&mut ws).await;
        assert_eq!(step_end["type"], "step-end");
        assert_eq!(step_end["block"], "dep");
        next_event(&mut ws).await; // step-start for target
        next_event(&mut ws).await; // tty-start
        read_until(&mut ws, "ready").await;
        ws.send(WsMessage::Binary(b"\n".to_vec().into())).await.expect("send input");
        next_event(&mut ws).await; // step-end for target
        drop(ws);

        let (mut ws2, _) = tokio_tungstenite::connect_async(&url).await.expect("connect");
        next_event(&mut ws2).await; // started
        // `step-start` is sent unconditionally for every chain step, even
        // one about to be skipped a moment later (same "brief flash" the
        // plain `/api/run` path already has — see `RunEvent::StepSkipped`'s
        // own doc comment) — the real signal is the `step-skipped` right
        // after it, not the absence of `step-start`.
        let dep_step_start = next_event(&mut ws2).await;
        assert_eq!(dep_step_start["type"], "step-start");
        assert_eq!(dep_step_start["block"], "dep");
        let second_step = next_event(&mut ws2).await;
        assert_eq!(
            second_step["type"], "step-skipped",
            "dep should be skipped the second time: {second_step:?}"
        );
        assert_eq!(second_step["block"], "dep");
        next_event(&mut ws2).await; // step-start for target
        next_event(&mut ws2).await; // tty-start
        read_until(&mut ws2, "ready").await;
        ws2.send(WsMessage::Binary(b"\n".to_vec().into())).await.expect("send input");
        next_event(&mut ws2).await; // step-end for target

        let _ = std::fs::remove_file(&canvas_path);
    }

    #[tokio::test]
    async fn tty_websocket_runs_a_tty_blocks_own_interpreter_not_bash() {
        // Skipped, not failed, where python3 isn't installed — same
        // graceful-skip convention `stream_exec`'s own interpreter tests
        // use.
        if std::process::Command::new("python3")
            .arg("--version")
            .output()
            .is_err()
        {
            return;
        }

        let canvas_path = write_test_canvas(concat!(
            "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
            "## Shell\n<!-- meshfox:node id=\"shell\" -->\n\n",
            "```python name=\"interactive\" interpreter=\"python3\" tty\n",
            "print(\"ready\")\n",
            "line = input()\n",
            "print(\"got: \" + line)\n",
            "```\n",
        ));
        let addr = spawn_test_server(canvas_path.clone()).await;
        let url = format!("ws://{addr}/api/run/tty?path=shell&block=interactive&cols=80&rows=24");
        let (mut ws, _) = tokio_tungstenite::connect_async(url)
            .await
            .expect("connect");

        next_event(&mut ws).await; // started
        next_event(&mut ws).await; // step-start
        next_event(&mut ws).await; // tty-start

        read_until(&mut ws, "ready").await;
        ws.send(WsMessage::Binary(b"hello\n".to_vec().into()))
            .await
            .expect("send input");
        read_until(&mut ws, "got: hello").await;

        let step_end = next_event(&mut ws).await;
        assert_eq!(step_end["type"], "step-end");
        assert_eq!(step_end["exitCode"], 0);

        let _ = std::fs::remove_file(&canvas_path);
    }

    #[tokio::test]
    async fn a_second_viewer_attaches_to_the_same_live_session_and_survives_the_first_leaving() {
        let canvas_path = write_test_canvas(TTY_CANVAS);
        let addr = spawn_test_server(canvas_path.clone()).await;
        let url = format!("ws://{addr}/api/run/tty?path=shell&block=interactive&cols=80&rows=24");
        let (mut ws1, _) = tokio_tungstenite::connect_async(url).await.expect("connect");

        next_event(&mut ws1).await; // started
        next_event(&mut ws1).await; // step-start
        next_event(&mut ws1).await; // tty-start
        read_until(&mut ws1, "ready").await;

        // A second viewer attaches to the *same* address — no chain
        // resolution, no spawning, just joining what's already running.
        let attach_url =
            format!("ws://{addr}/api/run/tty/attach?nodeId=shell&block=interactive&cols=80&rows=24");
        let (mut ws2, _) = tokio_tungstenite::connect_async(attach_url).await.expect("attach");
        // The attacher gets the already-buffered "ready" as backlog,
        // without the process ever having to print it again.
        read_until(&mut ws2, "ready").await;

        // Close the *originating* connection entirely — the session must
        // keep running for the still-attached second viewer, not die with
        // it (the whole point of this feature).
        ws1.close(None).await.ok();

        // Typing into the *second* connection still reaches the same pty.
        ws2.send(WsMessage::Binary(b"hello\n".to_vec().into()))
            .await
            .expect("send input");
        read_until(&mut ws2, "got: hello").await;

        let _ = std::fs::remove_file(&canvas_path);
    }

    #[tokio::test]
    async fn killing_a_tty_session_by_address_closes_every_attached_viewer() {
        let canvas_path = write_test_canvas(concat!(
            "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
            "## Shell\n<!-- meshfox:node id=\"shell\" -->\n\n",
            "```bash name=\"interactive\" tty\necho ready; sleep 30\n```\n",
        ));
        let addr = spawn_test_server(canvas_path.clone()).await;
        let url = format!("ws://{addr}/api/run/tty?path=shell&block=interactive&cols=80&rows=24");
        let (mut ws1, _) = tokio_tungstenite::connect_async(url).await.expect("connect");
        next_event(&mut ws1).await; // started
        next_event(&mut ws1).await; // step-start
        next_event(&mut ws1).await; // tty-start
        read_until(&mut ws1, "ready").await;

        let attach_url =
            format!("ws://{addr}/api/run/tty/attach?nodeId=shell&block=interactive&cols=80&rows=24");
        let (mut ws2, _) = tokio_tungstenite::connect_async(attach_url).await.expect("attach");
        read_until(&mut ws2, "ready").await;

        // Kill by address alone — this test never even looks at `runId`,
        // matching a client that only ever knew about this session via
        // `attach`, never the request that originally started it.
        let mut stream = TcpStream::connect(addr).await.expect("connect");
        let body = r#"{"nodeId":"shell","block":"interactive"}"#;
        let request = format!(
            "POST /api/kill HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        stream.write_all(request.as_bytes()).await.expect("write");
        let mut response = String::new();
        stream.read_to_string(&mut response).await.expect("read");
        assert!(response.starts_with("HTTP/1.1 204"), "unexpected response: {response}");

        // The attached viewer sees the session end too, even though it
        // never held any kill switch of its own — either a close frame or
        // the socket just ending are both "it's over" from this side.
        let next = ws2.next().await;
        assert!(
            matches!(next, Some(Ok(WsMessage::Close(_))) | None),
            "expected the attached viewer's socket to close, got: {next:?}"
        );

        let _ = std::fs::remove_file(&canvas_path);
    }

    #[tokio::test]
    async fn an_attached_viewer_that_falls_behind_recovers_instead_of_disconnecting() {
        // Regression coverage for a real bug: a slow viewer (a browser tab
        // over a real network, xterm.js parsing every frame — nowhere near
        // as fast as a loopback TUI socket reading straight into a raw
        // buffer) watching a fast-redrawing full-screen program could fall
        // behind `tty_registry`'s 1024-message broadcast capacity, and
        // `relay_tty_viewer`/`relay_tty_step` used to treat *any*
        // `rx.recv()` error — a lagged receiver included — as if the
        // session itself had ended, silently closing that viewer's socket
        // (a blank terminal, then "disconnected") while the session and
        // every other viewer kept going fine. `read()`'s own 4096-byte cap
        // (`pty_exec::spawn`) means any burst over `1024 * 4096` bytes
        // guarantees at least 1024 separate broadcast sends *regardless*
        // of how the kernel batches the writes that produced it — this
        // burst is comfortably past that.
        let canvas_path = write_test_canvas(concat!(
            "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
            "## Shell\n<!-- meshfox:node id=\"shell\" -->\n\n",
            "```bash name=\"interactive\" tty\n",
            "echo ready; read line\n",
            "head -c 6000000 /dev/zero | tr '\\0' 'x'; echo\n",
            "echo BURST-DONE\n",
            "read line2; echo \"got: $line2\"\n",
            "```\n",
        ));
        let addr = spawn_test_server(canvas_path.clone()).await;
        let url = format!("ws://{addr}/api/run/tty?path=shell&block=interactive&cols=80&rows=24");
        let (mut ws1, _) = tokio_tungstenite::connect_async(url).await.expect("connect");
        next_event(&mut ws1).await; // started
        next_event(&mut ws1).await; // step-start
        next_event(&mut ws1).await; // tty-start
        read_until(&mut ws1, "ready").await;

        let attach_url =
            format!("ws://{addr}/api/run/tty/attach?nodeId=shell&block=interactive&cols=80&rows=24");
        let (mut ws2, _) = tokio_tungstenite::connect_async(attach_url).await.expect("attach");
        // Give `relay_tty_viewer` a moment to actually reach `handle.
        // attach()` (subscribing) before the burst below starts — a WS
        // client's own `connect_async` returning doesn't itself guarantee
        // the server's `on_upgrade` callback has already run that far.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        // Deliberately never reading from `ws2` here is what makes this
        // deterministic: its own relay task's `socket.send()` eventually
        // blocks on ordinary TCP backpressure once the OS send buffer
        // fills, which stalls that task's own `rx.recv()` loop for the
        // whole burst — guaranteeing it falls behind, rather than merely
        // *risking* it under an ordinary timing race.
        ws1.send(WsMessage::Binary(b"\n".to_vec().into())).await.expect("unblock read");
        // Draining `ws1` (the *originating* connection, read continuously
        // throughout) confirms the several-MB burst has fully landed
        // server-side before `ws2` ever looks at it.
        read_until(&mut ws1, "BURST-DONE").await;

        // *Now* start reading `ws2` — before the fix, its relay task's own
        // `rx.recv()` would see this as the session having ended and close
        // the socket having shown nothing at all; after it, it resyncs via
        // a fresh `attach()` snapshot and keeps going.
        read_until(&mut ws2, "BURST-DONE").await;
        ws2.send(WsMessage::Binary(b"hello\n".to_vec().into())).await.expect("send input");
        read_until(&mut ws2, "got: hello").await;

        let _ = std::fs::remove_file(&canvas_path);
    }

    #[tokio::test]
    async fn active_runs_lists_a_live_tty_session() {
        let canvas_path = write_test_canvas(concat!(
            "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
            "## Shell\n<!-- meshfox:node id=\"shell\" -->\n\n",
            "```bash name=\"interactive\" tty\necho ready; sleep 30\n```\n",
        ));
        let addr = spawn_test_server(canvas_path.clone()).await;
        let url = format!("ws://{addr}/api/run/tty?path=shell&block=interactive&cols=80&rows=24");
        let (mut ws, _) = tokio_tungstenite::connect_async(url).await.expect("connect");
        next_event(&mut ws).await; // started
        next_event(&mut ws).await; // step-start
        next_event(&mut ws).await; // tty-start
        read_until(&mut ws, "ready").await;

        let (status, body) = get(addr, "/api/runs").await;
        assert_eq!(status, 200, "unexpected body: {body}");
        let runs: serde_json::Value = serde_json::from_str(&body).unwrap();
        let entry = runs
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["nodeId"] == "shell" && r["block"] == "interactive")
            .unwrap_or_else(|| panic!("expected an active-runs entry for shell/interactive, got: {body}"));
        assert_eq!(entry["kind"], "tty");
        assert_eq!(entry["status"], "running");

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

    #[tokio::test]
    async fn tty_websocket_runs_a_step_that_lives_inside_an_included_canvas() {
        let dir = std::env::temp_dir().join(format!(
            "meshfox-tty-include-test-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("child.canvas.md"),
            concat!(
                "<!-- meshfox:canvas -->\n# Child\n<!-- meshfox:node id=\"root\" -->\n\n",
                "## Shell\n<!-- meshfox:node id=\"shell\" -->\n\n",
                "```bash name=\"interactive\" tty\necho ready; pwd -P\n```\n",
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

        let addr = spawn_test_server(base_path.clone()).await;
        let url = format!(
            "ws://{addr}/api/run/tty?path=child,child%2Froot,child%2Fshell&block=interactive&cols=80&rows=24"
        );
        let (mut ws, _) = tokio_tungstenite::connect_async(url)
            .await
            .expect("connect");

        next_event(&mut ws).await; // started
        let step_start = next_event(&mut ws).await;
        assert_eq!(step_start["nodeId"], "child/shell");
        next_event(&mut ws).await; // tty-start

        let want_cwd = dir.canonicalize().unwrap().to_string_lossy().into_owned();
        let output = read_until(&mut ws, &want_cwd).await;
        assert!(
            output.contains(&want_cwd),
            "expected the included file's own directory ({want_cwd}) as PWD, got: {output}"
        );

        let _ = std::fs::remove_dir_all(&dir);
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
        while let Some(nl) = rest.find("\r\n") {
            let Ok(size) = usize::from_str_radix(rest[..nl].trim(), 16) else {
                break;
            };
            rest = &rest[nl + 2..];
            if size == 0 || size > rest.len() {
                break;
            }
            out.push_str(&rest[..size]);
            rest = rest[size..].strip_prefix("\r\n").unwrap_or(rest);
        }
        out
    }

    /// Connects to `path` (a WS-upgrading run endpoint) and collects every
    /// `RunEvent` text frame until the socket closes — `run_file_node`'s
    /// own equivalent of `ws_tests::next_event`, just collecting the whole
    /// sequence at once since these tests don't need to interleave sends.
    async fn run_ws_events(addr: SocketAddr, path: &str) -> Vec<serde_json::Value> {
        use futures_util::StreamExt;
        use tokio_tungstenite::tungstenite::Message as WsMessage;
        let url = format!("ws://{addr}{path}");
        let (mut ws, _) = tokio_tungstenite::connect_async(url).await.expect("connect");
        let mut events = Vec::new();
        while let Some(msg) = ws.next().await {
            match msg.expect("no ws error") {
                WsMessage::Text(t) => {
                    events.push(serde_json::from_str(&t).expect("valid RunEvent JSON"));
                }
                WsMessage::Close(_) => break,
                _ => continue,
            }
        }
        events
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
        let events = run_ws_events(addr, "/api/nodes/seed/run").await;
        assert_eq!(events[0]["type"], "started");
        assert_eq!(events[1]["type"], "step-start");
        assert_eq!(events[1]["nodeId"], "seed");
        assert_eq!(events[1]["block"], "seed");
        assert!(
            events
                .iter()
                .any(|e| e["type"] == "output" && e["text"] == "hi from seed" && e["stream"] == "stdout"),
            "expected a stdout-tagged output event with the script's own stdout, got: {events:?}"
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
        let events = run_ws_events(addr, "/api/nodes/seed/run").await;
        assert_eq!(events[0]["type"], "error");
        let message = events[0]["message"].as_str().unwrap_or_default();
        assert!(
            message.contains("isn't a runnable file node"),
            "unexpected message: {message}"
        );

        let _ = std::fs::remove_file(&canvas_path);
    }

    #[tokio::test]
    async fn run_file_node_rejects_an_unknown_node() {
        let canvas_path = write_test_canvas(
            "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n",
        );
        let addr = spawn_test_server(canvas_path.clone()).await;
        let events = run_ws_events(addr, "/api/nodes/nope/run").await;
        assert_eq!(events[0]["type"], "error");
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

    #[tokio::test]
    async fn open_node_file_resolves_a_nodes_id_across_an_include() {
        // `open_node_file` used to look `id` up only in the primary
        // document's own raw text, so a node spliced in from a canvas
        // `include` — addressed by its namespaced id, e.g. "child/note" —
        // always 404'd even though it exists (TODO.canvas.md: "Открытие
        // файала во вложенном канвасе"). It should route through
        // `locate_node`, same as every mutating endpoint already does.
        // `note` isn't a `file` node, so a *found*-but-rejected 422 proves
        // the lookup now succeeds, without this test ever invoking the OS
        // opener the way a real `file` node target would.
        let dir = std::env::temp_dir().join(format!(
            "meshfox-open-include-test-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("child.canvas.md"),
            concat!(
                "<!-- meshfox:canvas -->\n# Child\n<!-- meshfox:node id=\"root\" -->\n\n",
                "## Note\n<!-- meshfox:node id=\"note\" -->\n\nnot a file node\n",
            ),
        )
        .unwrap();
        let canvas_path = dir.join("base.canvas.md");
        std::fs::write(
            &canvas_path,
            concat!(
                "<!-- meshfox:canvas -->\n# Base\n<!-- meshfox:node id=\"base\" -->\n\n",
                "## Child\n<!-- meshfox:node id=\"child\" type=\"include\" -->\n\n[child](./child.canvas.md)\n",
            ),
        )
        .unwrap();

        let addr = spawn_test_server(canvas_path.clone()).await;
        let (status, body) = post(addr, "/api/nodes/child%2Fnote/open").await;
        assert_eq!(
            status, 422,
            "expected the included node to be found (and rejected only for not \
             being a file node), got: {body}"
        );
        assert!(body.contains("not a file node"), "unexpected body: {body}");

        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod run_block_include_tests {
    use super::*;

    /// `/api/run` is a WS upgrade now — connects to `/api/run?{query}` and
    /// collects every `RunEvent` text frame until the socket closes, same
    /// role `ndjson_events` used to play for the old chunked-HTTP body.
    async fn run_ws_events(addr: SocketAddr, query: &str) -> Vec<serde_json::Value> {
        use futures_util::StreamExt;
        use tokio_tungstenite::tungstenite::Message as WsMessage;
        let url = format!("ws://{addr}/api/run?{query}");
        let (mut ws, _) = tokio_tungstenite::connect_async(url).await.expect("connect");
        let mut events = Vec::new();
        while let Some(msg) = ws.next().await {
            match msg.expect("no ws error") {
                WsMessage::Text(t) => {
                    events.push(serde_json::from_str(&t).expect("valid RunEvent JSON"));
                }
                WsMessage::Close(_) => break,
                _ => continue,
            }
        }
        events
    }

    /// A primary `base.canvas.md` including `child.canvas.md`, the child's
    /// own `leaf` node carrying one runnable, cacheable block that reports
    /// its own `pwd` — namespaced `child/leaf`/`report` once resolved.
    fn write_base_and_child_canvas() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "meshfox-run-include-test-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("child.canvas.md"),
            concat!(
                "<!-- meshfox:canvas -->\n# Child\n<!-- meshfox:node id=\"root\" -->\n\n",
                "## Leaf\n<!-- meshfox:node id=\"leaf\" -->\n\n",
                "```bash name=\"report\" cache\npwd -P\n```\n",
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
    async fn runs_a_block_that_lives_inside_an_included_canvas() {
        let base_path = write_base_and_child_canvas();
        let child_path = base_path.parent().unwrap().join("child.canvas.md");
        let addr = spawn_test_server(base_path.clone()).await;

        let events = run_ws_events(
            addr,
            "path=child,child/root,child/leaf&block=report&persist=true",
        )
        .await;
        assert!(
            events.iter().any(|e| e["type"] == "step-start" && e["nodeId"] == "child/leaf"),
            "expected a step-start for child/leaf, got: {events:?}"
        );
        let step_end = events
            .iter()
            .find(|e| e["type"] == "step-end")
            .unwrap_or_else(|| panic!("expected a step-end event, got: {events:?}"));
        assert_eq!(step_end["exitCode"], 0);
        let output_event = events
            .iter()
            .find(|e| e["type"] == "output")
            .unwrap_or_else(|| panic!("expected an output event, got: {events:?}"));
        // `pwd -P` ran with the *included* file's own directory as `PWD` —
        // same directory `child.canvas.md` itself lives in, not wherever
        // `base.canvas.md` (the primary document) is.
        assert_eq!(
            output_event["text"],
            child_path
                .parent()
                .unwrap()
                .canonicalize()
                .unwrap()
                .to_string_lossy()
                .as_ref(),
        );

        // Cache landed in `child.canvas.md`, addressed there by its own
        // local id `leaf` — the primary document is untouched.
        let base_after = std::fs::read_to_string(&base_path).unwrap();
        assert!(!base_after.contains("meshfox:output"));
        let child_after = std::fs::read_to_string(&child_path).unwrap();
        assert!(child_after.contains("meshfox:output name=\"report\""));

        let _ = std::fs::remove_dir_all(base_path.parent().unwrap());
    }

    /// Not include-specific — just reuses this module's own `post_json`/
    /// `ndjson_events` helpers. Each `"output"` `RunEvent` over `/api/run`'s
    /// NDJSON stream now carries a `stream` field (`stream_exec::OutputStream`,
    /// see its own doc comment) alongside `text`, so the web UI's live view
    /// can tell stdout from stderr apart the same way a `cache`d run's
    /// persisted `ExecOutput.stdout`/`.stderr` already lets it once reloaded
    /// (`MeshNode.tsx`'s `LiveRunOutput`/`App.tsx`'s `appendOutputLine`).
    #[tokio::test]
    async fn output_events_over_ndjson_are_tagged_with_their_own_stream() {
        let canvas_path = std::env::temp_dir().join(format!(
            "meshfox-run-stream-tag-test-{}.canvas.md",
            uuid::Uuid::new_v4()
        ));
        std::fs::write(
            &canvas_path,
            concat!(
                "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
                "```bash name=\"root\" cache\necho out1; sleep 0.05; echo err1 >&2\n```\n",
            ),
        )
        .unwrap();

        let addr = spawn_test_server(canvas_path.clone()).await;
        let events = run_ws_events(addr, "block=root&persist=false").await;
        let output_events: Vec<(Option<&str>, Option<&str>)> = events
            .iter()
            .filter(|e| e["type"] == "output")
            .map(|e| (e["stream"].as_str(), e["text"].as_str()))
            .collect();
        assert_eq!(
            output_events,
            vec![(Some("stdout"), Some("out1")), (Some("stderr"), Some("err1"))],
            "events: {events:?}"
        );

        let _ = std::fs::remove_file(&canvas_path);
    }
}

#[cfg(test)]
mod session_skip_tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpStream;

    // Same request/dechunk/ndjson-parsing shape as `run_block_include_tests`
    // above (kept local rather than shared — that module is itself
    // `#[cfg(test)]`-private, nothing to import from).
    async fn request(addr: SocketAddr, method: &str, path: &str, content_type: &str, body: &str) -> (u16, String) {
        let request = format!(
            "{method} {path} HTTP/1.1\r\nHost: {addr}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
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
        let body = if head.to_ascii_lowercase().contains("transfer-encoding: chunked") {
            let mut out = String::new();
            let mut rest = raw_body;
            while let Some(nl) = rest.find("\r\n") {
                let Ok(size) = usize::from_str_radix(rest[..nl].trim(), 16) else { break };
                rest = &rest[nl + 2..];
                if size == 0 || size > rest.len() {
                    break;
                }
                out.push_str(&rest[..size]);
                rest = rest[size..].strip_prefix("\r\n").unwrap_or(rest);
            }
            out
        } else {
            raw_body.to_string()
        };
        (status, body)
    }

    // `step-start` is emitted unconditionally, even for a step that turns
    // out to be skipped a moment later (see `run_block`'s own doc comment
    // on `RunEvent::StepSkipped`) -- "actually ran" means it reached a real
    // `step-end`, not just a `step-start`.
    fn really_ran(events: &[serde_json::Value], block: &str) -> bool {
        events.iter().any(|e| e["type"] == "step-end" && e["block"] == block)
    }

    fn skipped_for(events: &[serde_json::Value], block: &str) -> bool {
        events.iter().any(|e| e["type"] == "step-skipped" && e["block"] == block)
    }

    fn write_dep_chain_canvas(dep_code: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("meshfox-session-skip-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("canvas.md");
        std::fs::write(
            &path,
            format!(
                "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n\n\
                 ```bash name=\"dep\" cache\n{dep_code}\n```\n\n\
                 ```bash name=\"target\" deps=\"dep\"\necho target-ran\n```\n",
            ),
        )
        .unwrap();
        path
    }

    async fn run_target(addr: SocketAddr) -> Vec<serde_json::Value> {
        use futures_util::StreamExt;
        use tokio_tungstenite::tungstenite::Message as WsMessage;
        let url = format!("ws://{addr}/api/run?block=target&persist=true");
        let (mut ws, _) = tokio_tungstenite::connect_async(url).await.expect("connect");
        let mut events = Vec::new();
        while let Some(msg) = ws.next().await {
            match msg.expect("no ws error") {
                WsMessage::Text(t) => {
                    events.push(serde_json::from_str(&t).expect("valid RunEvent JSON"));
                }
                WsMessage::Close(_) => break,
                _ => continue,
            }
        }
        events
    }

    #[tokio::test]
    async fn a_second_chain_run_skips_an_unchanged_dependency() {
        let path = write_dep_chain_canvas("echo dep-ran");
        let addr = spawn_test_server(path.clone()).await;

        let first = run_target(addr).await;
        assert!(really_ran(&first, "dep"), "expected dep to actually run the first time: {first:?}");
        assert!(!skipped_for(&first, "dep"), "dep shouldn't be skipped before it's ever run: {first:?}");
        // The target itself always runs for real, first time and every time.
        assert!(really_ran(&first, "target"));

        let second = run_target(addr).await;
        assert!(skipped_for(&second, "dep"), "expected dep to be skipped the second time: {second:?}");
        assert!(!really_ran(&second, "dep"), "a skipped dep must not also get a real step-start: {second:?}");
        assert!(
            really_ran(&second, "target"),
            "the requested block itself must never be skipped, even when unchanged: {second:?}"
        );

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[tokio::test]
    async fn a_skipped_dependency_still_reports_its_last_real_output() {
        // `step-skipped` carries whatever `dep` printed the last time it
        // *actually* ran (see `SessionRun::output`) — there's no fresh
        // output from the skipped run itself, so the client needs this to
        // show anything at all instead of a bare status line (see
        // `web/src/MeshNode.tsx`'s `SkippedRunOutput`).
        let path = write_dep_chain_canvas("echo dep-ran");
        let addr = spawn_test_server(path.clone()).await;

        let _first = run_target(addr).await;
        let second = run_target(addr).await;
        let skip_event = second
            .iter()
            .find(|e| e["type"] == "step-skipped" && e["block"] == "dep")
            .expect("dep should be skipped the second time");
        assert_eq!(skip_event["output"], "dep-ran\n");
        assert!(
            skip_event["durationMs"].as_u64().is_some(),
            "expected a numeric durationMs: {skip_event:?}"
        );

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[tokio::test]
    async fn editing_the_dependencys_code_makes_it_rerun_instead_of_skipping() {
        let path = write_dep_chain_canvas("echo dep-ran");
        let addr = spawn_test_server(path.clone()).await;

        let first = run_target(addr).await;
        assert!(really_ran(&first, "dep"));

        // Edit `dep`'s own code in place, same file shape `write_dep_chain_canvas`
        // wrote, so its fence's fingerprint (see `meshfox_core::fingerprint`)
        // changes — the same mechanism `crate::output`'s cached-output
        // staleness uses, here driving "already ran this session" instead.
        let edited = std::fs::read_to_string(&path).unwrap().replacen("echo dep-ran", "echo dep-ran-again", 1);
        let (put_status, put_body) = request(addr, "PUT", "/api/canvas/raw", "text/plain", &edited).await;
        assert_eq!(put_status, 204, "unexpected body: {put_body}");

        let second = run_target(addr).await;
        assert!(
            really_ran(&second, "dep"),
            "an edited dependency must actually rerun, not be skipped as fresh: {second:?}"
        );
        assert!(!skipped_for(&second, "dep"));

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[tokio::test]
    async fn an_always_flagged_dependency_never_gets_skipped() {
        // Same shape as `write_dep_chain_canvas`, but `dep` carries `always`
        // — a migration-style step whose side effect (here: writing a
        // marker file) needs to happen on every chain run regardless of
        // whether its own code looks unchanged.
        let dir = std::env::temp_dir().join(format!("meshfox-session-skip-always-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("canvas.md");
        std::fs::write(
            &path,
            concat!(
                "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
                "```bash name=\"dep\" cache always\necho dep-ran\n```\n\n",
                "```bash name=\"target\" deps=\"dep\"\necho target-ran\n```\n",
            ),
        )
        .unwrap();
        let addr = spawn_test_server(path.clone()).await;

        let first = run_target(addr).await;
        assert!(really_ran(&first, "dep"));

        let second = run_target(addr).await;
        assert!(
            really_ran(&second, "dep"),
            "an `always` dependency must rerun even though it's unchanged and already \
             succeeded this session: {second:?}"
        );
        assert!(!skipped_for(&second, "dep"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn reset_session_makes_a_previously_skippable_dependency_rerun() {
        let path = write_dep_chain_canvas("echo dep-ran");
        let addr = spawn_test_server(path.clone()).await;

        let first = run_target(addr).await;
        assert!(really_ran(&first, "dep"));

        let second = run_target(addr).await;
        assert!(
            skipped_for(&second, "dep"),
            "expected dep to be skipped before any reset: {second:?}"
        );

        let (status, body) = request(addr, "POST", "/api/session/reset", "application/json", "").await;
        assert_eq!(status, 204, "unexpected body: {body}");

        let third = run_target(addr).await;
        assert!(
            really_ran(&third, "dep"),
            "a dependency must rerun for real right after a session reset, even though \
             it's unchanged and had already run this session before the reset: {third:?}"
        );
        assert!(!skipped_for(&third, "dep"));

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
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
            session: false,
            default_var: None,
            choices_var: None,
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

    /// `/api/run` is a WS upgrade now — `params` are query-string params
    /// (`Url::parse_with_params` handles percent-encoding, same convention
    /// `worker_client::tty_connect` already uses for its own `vars`/
    /// `saveSecrets` JSON-encoded query values). Collects every `RunEvent`
    /// text frame until the socket closes.
    async fn run_ws_events(addr: SocketAddr, params: &[(&str, &str)]) -> Vec<serde_json::Value> {
        use futures_util::StreamExt;
        use tokio_tungstenite::tungstenite::Message as WsMessage;
        let url = reqwest::Url::parse_with_params(&format!("ws://{addr}/api/run"), params)
            .expect("valid url");
        let (mut ws, _) = tokio_tungstenite::connect_async(url.as_str())
            .await
            .expect("connect");
        let mut events = Vec::new();
        while let Some(msg) = ws.next().await {
            match msg.expect("no ws error") {
                WsMessage::Text(t) => {
                    events.push(serde_json::from_str(&t).expect("valid RunEvent JSON"));
                }
                WsMessage::Close(_) => break,
                _ => continue,
            }
        }
        events
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

    const DYNAMIC_CHOICES_CANVAS: &str = concat!(
        "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
        "<!-- meshfox:var name=\"REGIONS_LIST\" from=\"root/list-regions\" -->\n",
        "<!-- meshfox:var name=\"REGION\" type=\"select\" choices_var=\"REGIONS_LIST\" -->\n\n",
        "```bash name=\"list-regions\"\necho \"REGIONS_LIST=us-east-1,eu-west-1\" >> \"$MESHFOX_VARS_OUT\"\n```\n\n",
        "```bash name=\"use-region\" env=\"$REGION\"\necho \"$REGION\"\n```\n",
    );

    // Regression test for a bug reported against `examples/vars.canvas.md`'s
    // "Dynamic choices" node: the web UI's "Configure variables" modal
    // showed `REGION`'s `<select>` with zero options, because `GET
    // /api/vars` never executes anything, so a `choices_var` chain
    // through a `from=`-computed variable had no way to ever resolve —
    // unlike the CLI/TUI, which resolve lazily mid-chain and so had
    // already run `list-regions` by the time `REGION` needed its choices.
    #[tokio::test]
    async fn get_vars_materializes_choices_var_through_a_from_computed_variable() {
        let canvas_path = write_test_canvas(DYNAMIC_CHOICES_CANVAS);
        let addr = spawn_test_server(canvas_path.clone()).await;

        let (status, body) = get(addr, "/api/vars?block=use-region").await;
        assert_eq!(status, 200);
        let statuses: Vec<serde_json::Value> =
            serde_json::from_str(&body).expect("valid VarStatus JSON");
        assert_eq!(statuses.len(), 1);
        assert_eq!(statuses[0]["name"], "REGION");
        assert_eq!(
            statuses[0]["choices"],
            serde_json::json!(["us-east-1", "eu-west-1"])
        );

        let _ = std::fs::remove_file(&canvas_path);
        let _ = std::fs::remove_file(meshfox_core::varcache::cache_path(&canvas_path));
    }

    #[tokio::test]
    async fn get_configure_vars_materializes_choices_var_through_a_from_computed_variable() {
        let canvas_path = write_test_canvas(DYNAMIC_CHOICES_CANVAS);
        let addr = spawn_test_server(canvas_path.clone()).await;

        let (status, body) = get(addr, "/api/vars/configure").await;
        assert_eq!(status, 200);
        let statuses: Vec<serde_json::Value> =
            serde_json::from_str(&body).expect("valid VarStatus JSON");
        assert_eq!(statuses.len(), 1);
        assert_eq!(statuses[0]["name"], "REGION");
        assert_eq!(
            statuses[0]["choices"],
            serde_json::json!(["us-east-1", "eu-west-1"])
        );

        let _ = std::fs::remove_file(&canvas_path);
        let _ = std::fs::remove_file(meshfox_core::varcache::cache_path(&canvas_path));
    }

    const SESSION_CANVAS: &str = concat!(
        "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
        "<!-- meshfox:var name=\"DEPLOY_CONFIG\" type=\"select\" choices=\"staging,prod\" required session -->\n\n",
        "```bash name=\"deploy\" env=\"$DEPLOY_CONFIG\"\necho \"$DEPLOY_CONFIG\"\n```\n",
    );

    #[tokio::test]
    async fn get_vars_still_offers_a_session_variable_before_a_run() {
        let canvas_path = write_test_canvas(SESSION_CANVAS);
        let addr = spawn_test_server(canvas_path.clone()).await;

        let (status, body) = get(addr, "/api/vars?block=deploy").await;
        assert_eq!(status, 200);
        let statuses: Vec<serde_json::Value> =
            serde_json::from_str(&body).expect("valid VarStatus JSON");
        assert_eq!(statuses.len(), 1);
        assert_eq!(statuses[0]["name"], "DEPLOY_CONFIG");
        assert_eq!(statuses[0]["resolved"], false);

        let _ = std::fs::remove_file(&canvas_path);
        let _ = std::fs::remove_file(meshfox_core::varcache::cache_path(&canvas_path));
    }

    #[tokio::test]
    async fn get_configure_vars_excludes_a_session_variable() {
        let canvas_path = write_test_canvas(SESSION_CANVAS);
        let addr = spawn_test_server(canvas_path.clone()).await;

        let (status, body) = get(addr, "/api/vars/configure").await;
        assert_eq!(status, 200);
        let statuses: Vec<serde_json::Value> =
            serde_json::from_str(&body).expect("valid VarStatus JSON");
        assert!(statuses.is_empty());

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

    const SECRET_ENV_CANVAS: &str = concat!(
        "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
        "<!-- meshfox:var name=\"API_TOKEN\" secret -->\n\n",
        "```bash name=\"use-token\" env=\"$API_TOKEN\"\necho \"$API_TOKEN\"\n```\n",
    );

    // TODO.canvas.md: "Галочка \"сохранить\" у secret" — a `secret`
    // declaration is never persisted to the on-disk cache by default, same
    // as before `save_secrets` existed.
    #[tokio::test]
    async fn run_block_does_not_persist_a_secret_answer_by_default() {
        let canvas_path = write_test_canvas(SECRET_ENV_CANVAS);
        let addr = spawn_test_server(canvas_path.clone()).await;

        let events = run_ws_events(
            addr,
            &[
                ("block", "use-token"),
                ("vars", r#"{"API_TOKEN":"sk-secret"}"#),
            ],
        )
        .await;
        assert_eq!(events[0]["type"], "started", "unexpected events: {events:?}");

        let cache = VarCache::load(&canvas_path).expect("load cache");
        assert_eq!(cache.get("API_TOKEN"), None);

        let _ = std::fs::remove_file(&canvas_path);
        let _ = std::fs::remove_file(meshfox_core::varcache::cache_path(&canvas_path));
    }

    #[tokio::test]
    async fn run_block_persists_a_secret_answer_when_save_secrets_names_it() {
        let canvas_path = write_test_canvas(SECRET_ENV_CANVAS);
        let addr = spawn_test_server(canvas_path.clone()).await;

        let events = run_ws_events(
            addr,
            &[
                ("block", "use-token"),
                ("vars", r#"{"API_TOKEN":"sk-secret"}"#),
                ("saveSecrets", r#"["API_TOKEN"]"#),
            ],
        )
        .await;
        assert_eq!(events[0]["type"], "started", "unexpected events: {events:?}");

        let cache = VarCache::load(&canvas_path).expect("load cache");
        assert_eq!(
            cache.get("API_TOKEN"),
            Some("sk-secret"),
            "naming API_TOKEN in saveSecrets should have persisted it, in plaintext, anyway"
        );

        let _ = std::fs::remove_file(&canvas_path);
        let _ = std::fs::remove_file(meshfox_core::varcache::cache_path(&canvas_path));
    }

    // Regression: saving a secret via `saveSecrets` used to write it to the
    // on-disk cache but never actually read it back (`vars::resolve` still
    // unconditionally skipped the cache for any `secret` declaration,
    // regardless of what was in it) — so a *second* run still reported the
    // variable as unresolved and asked again, even though the value was
    // sitting right there in the `.env` file the whole time. Covers the
    // real end-to-end path a browser tab actually takes (`GET /api/vars`
    // between two `/api/run` calls, not just `crate::vars::resolve` in
    // isolation), since that's the shape of the report that caught this.
    #[tokio::test]
    async fn a_saved_secret_is_reported_resolved_and_reused_on_a_later_run() {
        let canvas_path = write_test_canvas(SECRET_ENV_CANVAS);
        let addr = spawn_test_server(canvas_path.clone()).await;

        let events = run_ws_events(
            addr,
            &[
                ("block", "use-token"),
                ("vars", r#"{"API_TOKEN":"sk-secret"}"#),
                ("saveSecrets", r#"["API_TOKEN"]"#),
            ],
        )
        .await;
        assert_eq!(events[0]["type"], "started", "unexpected events: {events:?}");

        // `GET /api/vars` (what the pre-run form checks before ever
        // opening) now sees it as resolved — so the browser wouldn't even
        // ask again — and still never puts the actual value on the wire.
        let (status, body) = get(addr, "/api/vars?block=use-token").await;
        assert_eq!(status, 200);
        let statuses: Vec<serde_json::Value> = serde_json::from_str(&body).expect("valid VarStatus JSON");
        assert_eq!(statuses[0]["name"], "API_TOKEN");
        assert_eq!(
            statuses[0]["resolved"], true,
            "a saved secret should show up as already resolved: {body}"
        );
        assert!(
            statuses[0]["value"].is_null(),
            "a secret's actual value must never be sent to the browser, saved or not: {body}"
        );

        // A later run supplying *no* `vars` at all (the client never re-asks
        // for something already resolved) must still succeed by reading the
        // saved value back from the cache, not fail with "missing required
        // variable(s)".
        let events = run_ws_events(addr, &[("block", "use-token")]).await;
        assert_eq!(events[0]["type"], "started", "unexpected events: {events:?}");

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

        let events = run_ws_events(
            addr,
            &[
                ("block", "run"),
                (
                    "vars",
                    r#"{"COUNT":"not-a-number","VERBOSE":"true","LEVEL":"debug"}"#,
                ),
            ],
        )
        .await;
        assert_eq!(events[0]["type"], "error", "unexpected events: {events:?}");
        let message = events[0]["message"].as_str().unwrap_or_default();
        assert!(message.contains("COUNT"), "unexpected message: {message}");

        let _ = std::fs::remove_file(&canvas_path);
        let _ = std::fs::remove_file(meshfox_core::varcache::cache_path(&canvas_path));
    }

    /// Regression test: `get_vars` used to parse only the primary document
    /// (`parse_or_error`, no include splicing), so a `path` addressing a
    /// node spliced in from an `include` could never be found there —
    /// `resolve_target` fails with `RunError::Tree`, which `get_vars`
    /// surfaced as a 404. `run_block` already resolved includes correctly
    /// (see its own `resolved_canvas` call) — this only ever broke the
    /// pre-run "what's still missing" check, not the run itself.
    #[tokio::test]
    async fn get_vars_resolves_a_block_that_lives_inside_an_included_canvas() {
        let dir = std::env::temp_dir().join(format!(
            "meshfox-vars-include-test-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("env.canvas.md"),
            concat!(
                "<!-- meshfox:canvas -->\n# Env\n<!-- meshfox:node id=\"root\" -->\n\n",
                "<!-- meshfox:var name=\"API_KEY\" default=\"dev-key\" -->\n\n",
                "## Setup\n<!-- meshfox:node id=\"setup\" -->\n\n",
                "```bash name=\"prepare\"\necho prepare\n```\n\n",
                "```bash name=\"resolve\" env=\"$API_KEY\"\necho $API_KEY\n```\n",
            ),
        )
        .unwrap();
        let base_path = dir.join("base.canvas.md");
        std::fs::write(
            &base_path,
            concat!(
                "<!-- meshfox:canvas -->\n# Base\n<!-- meshfox:node id=\"base\" -->\n\n",
                "## Env\n<!-- meshfox:node id=\"env\" type=\"include\" -->\n\n[env](./env.canvas.md)\n",
            ),
        )
        .unwrap();

        let addr = spawn_test_server(base_path.clone()).await;
        let (status, body) = get(addr, "/api/vars?path=env,env/root,env/setup&block=resolve").await;
        assert_eq!(status, 200, "unexpected body: {body}");
        let statuses: Vec<serde_json::Value> =
            serde_json::from_str(&body).expect("valid VarStatus JSON");
        assert_eq!(statuses.len(), 1);
        assert_eq!(statuses[0]["name"], "API_KEY");
        assert_eq!(statuses[0]["value"], "dev-key");

        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod form_endpoint_tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpStream;

    fn write_test_canvas(contents: &str) -> PathBuf {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "meshfox-form-test-{}.canvas.md",
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
        let body = parts.next().unwrap_or_default();
        let status = head
            .lines()
            .next()
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        (status, body.to_string())
    }

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
        let body = parts.next().unwrap_or_default();
        let status = head
            .lines()
            .next()
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        (status, body.to_string())
    }

    /// `/api/run` is a WS upgrade now — `params` are query-string params.
    /// Collects every `RunEvent` text frame until the socket closes (i.e.
    /// until the run itself is done), same "block until finished" shape
    /// the old chunked-HTTP `post_json`/`/api/run` call used to have.
    async fn run_ws_events(addr: SocketAddr, params: &[(&str, &str)]) -> Vec<serde_json::Value> {
        use futures_util::StreamExt;
        use tokio_tungstenite::tungstenite::Message as WsMessage;
        let url = reqwest::Url::parse_with_params(&format!("ws://{addr}/api/run"), params)
            .expect("valid url");
        let (mut ws, _) = tokio_tungstenite::connect_async(url.as_str())
            .await
            .expect("connect");
        let mut events = Vec::new();
        while let Some(msg) = ws.next().await {
            match msg.expect("no ws error") {
                WsMessage::Text(t) => {
                    events.push(serde_json::from_str(&t).expect("valid RunEvent JSON"));
                }
                WsMessage::Close(_) => break,
                _ => continue,
            }
        }
        events
    }

    /// `/api/run/subscribe` is a WS upgrade now — connects to `path` (a
    /// full `/api/run/subscribe?...` query string) and concatenates every
    /// `SubscribeEvent` text frame (one per line) until the socket closes,
    /// the same shape the old chunked-NDJSON body gave callers that just
    /// wanted to substring-search the whole transcript.
    async fn subscribe_ws_body(addr: SocketAddr, path: &str) -> String {
        use futures_util::StreamExt;
        use tokio_tungstenite::tungstenite::Message as WsMessage;
        let url = format!("ws://{addr}{path}");
        let (mut ws, _) = tokio_tungstenite::connect_async(url).await.expect("connect");
        let mut body = String::new();
        while let Some(msg) = ws.next().await {
            match msg.expect("no ws error") {
                WsMessage::Text(t) => {
                    body.push_str(&t);
                    body.push('\n');
                }
                WsMessage::Close(_) => break,
                _ => continue,
            }
        }
        body
    }

    /// Polls `GET /api/runs` until it reports `block` as `exited` (or the
    /// deadline passes) — `submit_form`'s own autorun trigger is
    /// deliberately fire-and-forget (the HTTP response returns the moment
    /// values are saved, not once every triggered chain finishes), so a
    /// test needs to wait for it the same way a real passive tab watching
    /// `/api/run/subscribe` would.
    async fn wait_for_exit(addr: SocketAddr, node_id: &str, block: &str) -> Option<i64> {
        for _ in 0..100 {
            let (status, body) = get(addr, "/api/runs").await;
            if status == 200 {
                if let Ok(runs) = serde_json::from_str::<Vec<serde_json::Value>>(&body) {
                    if let Some(run) = runs
                        .iter()
                        .find(|r| r["nodeId"] == node_id && r["block"] == block)
                    {
                        if run["status"] == "exited" {
                            return run["exitCode"].as_i64();
                        }
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        None
    }

    const FORM_CANVAS: &str = concat!(
        "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
        "## Config\n<!-- meshfox:node id=\"config\" -->\n\n",
        "<!-- meshfox:var name=\"REGION\" type=\"select\" choices=\"us,eu\" -->\n\n",
        "```form name=\"pick-region\" send=\"Apply\"\n",
        "field var=\"REGION\" label=\"AWS Region\"\n",
        "```\n\n",
        "```bash name=\"show-region\" env=\"$REGION\" autorun\necho \"region is $REGION\"\n```\n",
    );

    #[tokio::test]
    async fn get_form_fields_returns_the_forms_own_fields_and_send_caption() {
        let canvas_path = write_test_canvas(FORM_CANVAS);
        let addr = spawn_test_server(canvas_path.clone()).await;

        let (status, body) =
            get(addr, "/api/form/fields?nodeId=config&block=pick-region").await;
        assert_eq!(status, 200, "unexpected body: {body}");
        let resp: serde_json::Value = serde_json::from_str(&body).expect("valid response JSON");
        assert_eq!(resp["send"], "Apply");
        assert_eq!(resp["fields"].as_array().unwrap().len(), 1);
        assert_eq!(resp["fields"][0]["label"], "AWS Region");
        assert_eq!(resp["fields"][0]["name"], "REGION");

        let _ = std::fs::remove_file(&canvas_path);
    }

    #[tokio::test]
    async fn get_form_fields_404s_for_an_unknown_block() {
        let canvas_path = write_test_canvas(FORM_CANVAS);
        let addr = spawn_test_server(canvas_path.clone()).await;

        let (status, _) = get(addr, "/api/form/fields?nodeId=config&block=nope").await;
        assert_eq!(status, 404);

        let _ = std::fs::remove_file(&canvas_path);
    }

    #[tokio::test]
    async fn get_form_fields_422s_for_a_non_form_block() {
        let canvas_path = write_test_canvas(FORM_CANVAS);
        let addr = spawn_test_server(canvas_path.clone()).await;

        let (status, _) = get(addr, "/api/form/fields?nodeId=config&block=show-region").await;
        assert_eq!(status, 422);

        let _ = std::fs::remove_file(&canvas_path);
    }

    #[tokio::test]
    async fn submit_form_saves_only_the_forms_own_recognized_fields() {
        let canvas_path = write_test_canvas(FORM_CANVAS);
        let addr = spawn_test_server(canvas_path.clone()).await;

        let (status, body) = post_json(
            addr,
            "/api/form/submit",
            r#"{"nodeId":"config","block":"pick-region","values":{"REGION":"eu","UNRELATED":"ignored"}}"#,
        )
        .await;
        assert_eq!(status, 200, "unexpected body: {body}");
        let resp: serde_json::Value = serde_json::from_str(&body).expect("valid response JSON");
        assert_eq!(resp["saved"], 1);
        assert_eq!(resp["autorunTriggered"].as_array().unwrap().len(), 1);
        assert_eq!(resp["autorunTriggered"][0]["nodeId"], "config");
        assert_eq!(resp["autorunTriggered"][0]["block"], "show-region");

        let _ = std::fs::remove_file(&canvas_path);
    }

    #[tokio::test]
    async fn submit_form_rejects_an_invalid_typed_value() {
        let canvas_path = write_test_canvas(FORM_CANVAS);
        let addr = spawn_test_server(canvas_path.clone()).await;

        let (status, _) = post_json(
            addr,
            "/api/form/submit",
            r#"{"nodeId":"config","block":"pick-region","values":{"REGION":"not-a-choice"}}"#,
        )
        .await;
        // `select`'s own `validate_value` only ever rejects membership once
        // `choices`/`choices_var` is actually substituted -- a literal
        // `choices=` list (this canvas's case) always is, so this should
        // be a 422, same as `POST /api/vars/configure` would report.
        assert_eq!(status, 422);

        let _ = std::fs::remove_file(&canvas_path);
    }

    #[tokio::test]
    async fn submit_form_never_writes_to_the_on_disk_cache() {
        let canvas_path = write_test_canvas(FORM_CANVAS);
        let addr = spawn_test_server(canvas_path.clone()).await;

        let (status, _) = post_json(
            addr,
            "/api/form/submit",
            r#"{"nodeId":"config","block":"pick-region","values":{"REGION":"eu"}}"#,
        )
        .await;
        assert_eq!(status, 200);

        // A node-scoped var is implicitly `session` -- `submit_form` must
        // never persist it the way `POST /api/vars/configure` would.
        let cache_path = meshfox_core::varcache::cache_path(&canvas_path);
        assert!(!cache_path.exists(), "REGION should never reach the on-disk cache");

        let _ = std::fs::remove_file(&canvas_path);
    }

    #[tokio::test]
    async fn submit_form_triggers_the_autorun_block_and_it_actually_runs() {
        let canvas_path = write_test_canvas(FORM_CANVAS);
        let addr = spawn_test_server(canvas_path.clone()).await;

        let (status, _) = post_json(
            addr,
            "/api/form/submit",
            r#"{"nodeId":"config","block":"pick-region","values":{"REGION":"eu"}}"#,
        )
        .await;
        assert_eq!(status, 200);

        let exit_code = wait_for_exit(addr, "config", "show-region").await;
        assert_eq!(exit_code, Some(0), "autorun-triggered block never finished");

        let _ = std::fs::remove_file(&canvas_path);
    }

    #[tokio::test]
    async fn a_later_get_vars_sees_the_session_value_a_form_just_submitted() {
        let canvas_path = write_test_canvas(FORM_CANVAS);
        let addr = spawn_test_server(canvas_path.clone()).await;

        // Before submitting, REGION has no default -- unresolved.
        let (_, before) = get(addr, "/api/vars?path=config&block=show-region").await;
        let before: Vec<serde_json::Value> = serde_json::from_str(&before).unwrap();
        assert_eq!(before[0]["resolved"], false);

        let (status, _) = post_json(
            addr,
            "/api/form/submit",
            r#"{"nodeId":"config","block":"pick-region","values":{"REGION":"eu"}}"#,
        )
        .await;
        assert_eq!(status, 200);

        let (_, after) = get(addr, "/api/vars?path=config&block=show-region").await;
        let after: Vec<serde_json::Value> = serde_json::from_str(&after).unwrap();
        assert_eq!(after[0]["resolved"], true);
        assert_eq!(after[0]["value"], "eu");

        let _ = std::fs::remove_file(&canvas_path);
    }

    #[tokio::test]
    async fn reset_session_clears_a_forms_submitted_value_too() {
        let canvas_path = write_test_canvas(FORM_CANVAS);
        let addr = spawn_test_server(canvas_path.clone()).await;

        let (status, _) = post_json(
            addr,
            "/api/form/submit",
            r#"{"nodeId":"config","block":"pick-region","values":{"REGION":"eu"}}"#,
        )
        .await;
        assert_eq!(status, 200);

        let (status, _) = post_json(addr, "/api/session/reset", "{}").await;
        assert_eq!(status, 204);

        let (_, after) = get(addr, "/api/vars?path=config&block=show-region").await;
        let after: Vec<serde_json::Value> = serde_json::from_str(&after).unwrap();
        assert_eq!(
            after[0]["resolved"], false,
            "reset-session should forget a form's submitted session value too"
        );

        let _ = std::fs::remove_file(&canvas_path);
    }

    /// Regression test for a real race reported against a live document
    /// (an amulettie/search block: a `form`+`autorun` table depending on a
    /// slow "is the backend up yet" readiness check): the *first* Send
    /// after changing the query kept showing the *previous* query's own
    /// result, and only a second, redundant Send (which happened to race
    /// an already-finished run instead of a genuinely in-flight one)
    /// showed the right thing. Root cause: `submit_form`'s own HTTP
    /// response returns the moment values are saved, long before this
    /// chain's own slow dependency (`wait`, standing in for the real
    /// readiness check) lets the requested block's own step actually
    /// spawn — a subscriber that races ahead of that (as a passive
    /// `autorun` watcher always does, having no stream of its own to wait
    /// on first) used to find the *previous* run's already-`Done` handle
    /// still sitting in `runs_registry` and trust it as current. See
    /// `run_block_impl`'s own `target_reservation` doc comment for the fix.
    #[tokio::test]
    async fn a_subscriber_racing_an_autoruns_slow_dependency_sees_the_fresh_output_not_stale() {
        const RACE_CANVAS: &str = concat!(
            "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
            "## Config\n<!-- meshfox:node id=\"config\" -->\n\n",
            "<!-- meshfox:var name=\"QUERY\" default=\"old\" -->\n\n",
            "```form name=\"query-form\" send=\"Apply\"\n",
            "field var=\"QUERY\" label=\"Query\"\n",
            "```\n\n",
            // `always` — otherwise this dependency's own fingerprint is
            // unchanged from the seed run below and it gets skipped as
            // "already fresh this session" on the form-triggered run,
            // collapsing the race window this test exists to exercise
            // down to nothing (confirmed directly: without this, the test
            // still passed even with the fix reverted).
            "```bash name=\"wait\" always\nsleep 0.3\necho waited\n```\n\n",
            "```bash name=\"table\" env=\"QUERY\" deps=\"wait\" autorun\necho \"value is $QUERY\"\n```\n",
        );
        let canvas_path = write_test_canvas(RACE_CANVAS);
        let addr = spawn_test_server(canvas_path.clone()).await;

        // Seed a stale, already-finished run of `table` with the *old*
        // value — a real completed run already sitting in the registry
        // before the form is ever touched, same as the bug report.
        let events = run_ws_events(addr, &[("path", "config"), ("block", "table")]).await;
        assert_eq!(events[0]["type"], "started", "unexpected events: {events:?}");

        // Submit a *new* value, then subscribe immediately — no delay —
        // so this reliably races `table`'s own slow `wait` dependency,
        // which hasn't even started yet by the time this subscribes.
        let (status, body) = post_json(
            addr,
            "/api/form/submit",
            r#"{"nodeId":"config","block":"query-form","values":{"QUERY":"new"}}"#,
        )
        .await;
        assert_eq!(status, 200, "unexpected body: {body}");

        let sub_body =
            subscribe_ws_body(addr, "/api/run/subscribe?nodeId=config&block=table&sinceSeq=0")
                .await;
        assert!(
            sub_body.contains(r#""text":"value is new""#),
            "a subscriber that raced the slow dependency should still see the fresh run's own output: {sub_body}"
        );
        assert!(
            !sub_body.contains(r#""text":"value is old""#),
            "a subscriber should never see the stale run's own output as if it were current: {sub_body}"
        );

        let _ = std::fs::remove_file(&canvas_path);
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

#[cfg(test)]
mod syntax_endpoint_tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpStream;

    /// A fresh scratch *directory* per call (not just a bare filename in
    /// the shared system temp dir) — `.meshfox/syntax/` lives next to the
    /// canvas file, so two tests sharing one parent directory would
    /// otherwise race on (and pollute each other's view of) the same
    /// `.meshfox/syntax/` folder under parallel test execution.
    fn write_test_canvas(contents: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("meshfox-syntax-endpoint-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("root.canvas.md");
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
        let body = parts.next().unwrap_or_default();
        let status = head
            .lines()
            .next()
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        (status, body.to_string())
    }

    const CANVAS: &str = "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n";
    const TINY_TMLANGUAGE: &str = r#"{"scopeName": "source.meshfox-endpoint-test", "name": "Meshfox Endpoint Test"}"#;

    /// Only exercises the *local* `.meshfox/syntax/` directory — the
    /// global `~/.meshfox/syntax/` one shares the exact same
    /// `list_grammar_files`/lookup code path (see `meshfox_core::syntax_dirs`),
    /// so it isn't separately covered here to avoid depending on/mutating
    /// whatever's actually in the real test-runner's `$HOME`.
    #[tokio::test]
    async fn lists_and_serves_a_local_custom_grammar() {
        let canvas_path = write_test_canvas(CANVAS);
        let canvas_dir = canvas_path.parent().unwrap().to_path_buf();
        let syntax_dir = canvas_dir.join(".meshfox").join("syntax");
        std::fs::create_dir_all(&syntax_dir).unwrap();
        std::fs::write(syntax_dir.join("test.tmLanguage.json"), TINY_TMLANGUAGE).unwrap();

        let addr = spawn_test_server(canvas_path.clone()).await;

        let (status, body) = get(addr, "/api/syntax").await;
        assert_eq!(status, 200);
        let entries: Vec<serde_json::Value> = serde_json::from_str(&body).expect("valid JSON");
        assert!(
            entries
                .iter()
                .any(|e| e["name"] == "test.tmLanguage.json" && e["source"] == "local"),
            "expected a local test.tmLanguage.json entry, got {body}"
        );

        let (status, body) = get(addr, "/api/syntax/test.tmLanguage.json").await;
        assert_eq!(status, 200);
        assert_eq!(body, TINY_TMLANGUAGE);

        std::fs::remove_dir_all(&canvas_dir).ok();
    }

    #[tokio::test]
    async fn unknown_grammar_name_is_a_404_not_a_path_traversal_attempt() {
        let canvas_path = write_test_canvas(CANVAS);
        let canvas_dir = canvas_path.parent().unwrap().to_path_buf();
        let addr = spawn_test_server(canvas_path.clone()).await;

        // `:name` is one path segment, so a literal `/` in the request path
        // (unencoded) doesn't even reach `get_syntax_file` — it just fails
        // to match this route at all, same as any other unmatched path.
        // The real thing to check is what happens once traversal-looking
        // *content* does reach the handler as a `name` value — a `%2F`-
        // encoded slash decodes to one within a single segment. Either way,
        // `get_syntax_file` only ever serves a name it already found via
        // its own `read_dir` listing (see its own doc comment), so this
        // should 404 exactly like any other name that isn't a real file.
        let (status, _) = get(addr, "/api/syntax/..%2F..%2F..%2F..%2Fetc%2Fpasswd").await;
        assert_eq!(status, 404);
        let (status, _) = get(addr, "/api/syntax/does-not-exist.tmLanguage.json").await;
        assert_eq!(status, 404);

        std::fs::remove_dir_all(&canvas_dir).ok();
    }
}

#[cfg(test)]
mod service_endpoint_tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpStream;

    fn write_test_canvas(contents: &str) -> PathBuf {
        // Each test gets its *own* directory, not just a uniquely-named
        // file directly in the shared OS temp dir — `service_lock_path`
        // keys a lock file by canvas file name within a `.meshfox/
        // services/` sibling of the canvas's own *directory*, and
        // `cleanup` (below) `remove_dir_all`s that whole directory; sharing
        // one flat temp dir across every test in this module means any one
        // test's cleanup nukes every other concurrently-running test's own
        // still-in-use lock files out from under it (a real, observed
        // source of cross-test flakiness once more than a couple of tests
        // here started exercising locks concurrently).
        let dir = std::env::temp_dir().join(format!("meshfox-services-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("doc.canvas.md");
        std::fs::write(&path, contents).unwrap();
        path
    }

    fn cleanup(canvas_path: &std::path::Path) {
        if let Some(dir) = canvas_path.parent() {
            let _ = std::fs::remove_dir_all(dir);
        }
    }

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

    const SERVICE_CANVAS: &str = concat!(
        "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
        "```bash name=\"srv\" service\necho starting\nsleep 30\n```\n",
    );

    /// `/api/run` is a WS upgrade now — `params` are query-string params.
    /// Collects every `RunEvent` text frame until the socket closes.
    async fn run_ws_events(addr: SocketAddr, params: &[(&str, &str)]) -> Vec<serde_json::Value> {
        use futures_util::StreamExt;
        use tokio_tungstenite::tungstenite::Message as WsMessage;
        let url = reqwest::Url::parse_with_params(&format!("ws://{addr}/api/run"), params)
            .expect("valid url");
        let (mut ws, _) = tokio_tungstenite::connect_async(url.as_str())
            .await
            .expect("connect");
        let mut events = Vec::new();
        while let Some(msg) = ws.next().await {
            match msg.expect("no ws error") {
                WsMessage::Text(t) => {
                    events.push(serde_json::from_str(&t).expect("valid RunEvent JSON"));
                }
                WsMessage::Close(_) => break,
                _ => continue,
            }
        }
        events
    }

    #[tokio::test]
    async fn run_block_spawns_a_service_and_it_shows_up_as_running() {
        let canvas_path = write_test_canvas(SERVICE_CANVAS);
        let addr = spawn_test_server(canvas_path.clone()).await;

        let events = run_ws_events(addr, &[("block", "srv")]).await;
        assert!(
            events.iter().any(|e| e["type"] == "service-started"),
            "expected a service-started event, got: {events:?}"
        );
        // The request itself returns fast (well under the block's own
        // `sleep 30`) with a normal `done` — proof the chain didn't wait
        // for the service to exit, just for it to spawn.
        assert!(
            events.iter().any(|e| e["type"] == "done"),
            "chain should complete normally right after spawning the service: {events:?}"
        );
        assert!(
            !events.iter().any(|e| e["type"] == "step-end"),
            "a service step should never report an exit code — it never waits for one: {events:?}"
        );

        let (status, list_body) = get(addr, "/api/services").await;
        assert_eq!(status, 200);
        let services: Vec<serde_json::Value> = serde_json::from_str(&list_body).unwrap();
        assert_eq!(services.len(), 1);
        assert_eq!(services[0]["nodeId"], "root");
        assert_eq!(services[0]["block"], "srv");
        assert_eq!(services[0]["status"], "running");
        assert!(services[0]["pid"].as_u64().unwrap() > 0);

        let _ = post_json(
            addr,
            "/api/services/stop",
            r#"{"nodeId":"root","block":"srv"}"#,
        )
        .await;
        cleanup(&canvas_path);
    }

    #[tokio::test]
    async fn running_a_chain_twice_does_not_spawn_a_second_instance() {
        let canvas_path = write_test_canvas(SERVICE_CANVAS);
        let addr = spawn_test_server(canvas_path.clone()).await;

        let events1 = run_ws_events(addr, &[("block", "srv")]).await;
        assert_eq!(events1[0]["type"], "started", "unexpected events: {events1:?}");
        let (_, list1) = get(addr, "/api/services").await;
        let pid1 = serde_json::from_str::<Vec<serde_json::Value>>(&list1).unwrap()[0]["pid"]
            .as_u64()
            .unwrap();

        let events2 = run_ws_events(addr, &[("block", "srv")]).await;
        assert_eq!(events2[0]["type"], "started", "unexpected events: {events2:?}");
        let (_, list2) = get(addr, "/api/services").await;
        let services2: Vec<serde_json::Value> = serde_json::from_str(&list2).unwrap();
        assert_eq!(services2.len(), 1, "must not spawn a duplicate instance");
        assert_eq!(services2[0]["pid"].as_u64().unwrap(), pid1, "same pid, not restarted");

        let _ = post_json(
            addr,
            "/api/services/stop",
            r#"{"nodeId":"root","block":"srv"}"#,
        )
        .await;
        cleanup(&canvas_path);
    }

    #[tokio::test]
    async fn stop_marks_the_service_stopped_not_crashed() {
        let canvas_path = write_test_canvas(SERVICE_CANVAS);
        let addr = spawn_test_server(canvas_path.clone()).await;
        let _ = run_ws_events(addr, &[("block", "srv")]).await;

        let (status, _) = post_json(
            addr,
            "/api/services/stop",
            r#"{"nodeId":"root","block":"srv"}"#,
        )
        .await;
        assert_eq!(status, 204);

        // Poll briefly for the background drain task to record the exit.
        let mut services: Vec<serde_json::Value> = Vec::new();
        for _ in 0..50 {
            let (_, list) = get(addr, "/api/services").await;
            services = serde_json::from_str(&list).unwrap();
            if services[0]["status"] != "running" {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert_eq!(services[0]["status"], "stopped");

        cleanup(&canvas_path);
    }

    #[tokio::test]
    async fn restart_gives_the_service_a_new_pid() {
        let canvas_path = write_test_canvas(SERVICE_CANVAS);
        let addr = spawn_test_server(canvas_path.clone()).await;
        let _ = run_ws_events(addr, &[("block", "srv")]).await;
        let (_, list) = get(addr, "/api/services").await;
        let old_pid = serde_json::from_str::<Vec<serde_json::Value>>(&list).unwrap()[0]["pid"]
            .as_u64()
            .unwrap();

        let (status, body) = post_json(
            addr,
            "/api/services/restart",
            r#"{"nodeId":"root","block":"srv"}"#,
        )
        .await;
        assert_eq!(status, 200, "unexpected body: {body}");
        let resp: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_ne!(resp["pid"].as_u64().unwrap(), old_pid);

        let (_, list) = get(addr, "/api/services").await;
        let services: Vec<serde_json::Value> = serde_json::from_str(&list).unwrap();
        assert_eq!(services[0]["status"], "running");

        let _ = post_json(
            addr,
            "/api/services/stop",
            r#"{"nodeId":"root","block":"srv"}"#,
        )
        .await;
        cleanup(&canvas_path);
    }

    #[tokio::test]
    async fn service_log_reports_captured_output_lines() {
        let canvas_path = write_test_canvas(SERVICE_CANVAS);
        let addr = spawn_test_server(canvas_path.clone()).await;
        let _ = run_ws_events(addr, &[("block", "srv")]).await;

        let mut lines: Vec<serde_json::Value> = Vec::new();
        for _ in 0..50 {
            let (_, body) = get(addr, "/api/services/log?nodeId=root&block=srv").await;
            lines = serde_json::from_str(&body).unwrap();
            if !lines.is_empty() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert_eq!(lines[0]["text"], "starting");
        assert_eq!(lines[0]["stream"], "stdout");

        let _ = post_json(
            addr,
            "/api/services/stop",
            r#"{"nodeId":"root","block":"srv"}"#,
        )
        .await;
        cleanup(&canvas_path);
    }

    #[tokio::test]
    async fn a_pre_existing_lock_from_another_process_is_reported_as_a_conflict() {
        let canvas_path = write_test_canvas(SERVICE_CANVAS);
        // Simulate another process already owning this service — some
        // large, never-really-assigned pid stands in for "definitely not
        // us and definitely not alive", same trick `service_lock`'s own
        // tests use.
        let lock_path = meshfox_core::service_lock_path(&canvas_path, "root", "srv");
        meshfox_core::service_lock::acquire(&lock_path, 999_999, "tui").unwrap();

        let addr = spawn_test_server(canvas_path.clone()).await;
        let events = run_ws_events(addr, &[("block", "srv")]).await;
        // Queued-time locking means every lock this run would need is
        // claimed *before* any step actually runs — the socket still
        // opens (a browser `WebSocket` can't read a pre-upgrade status),
        // but the very first message is a `lock-conflict`, not `started`.
        assert_eq!(events[0]["type"], "lock-conflict", "unexpected events: {events:?}");
        assert_eq!(events[0]["nodeId"], "root");
        assert_eq!(events[0]["block"], "srv");
        assert_eq!(events[0]["ownerPid"], 999_999);
        assert_eq!(events[0]["ownerDesc"], "tui");

        let (status, list_body) = get(addr, "/api/services").await;
        assert_eq!(status, 200);
        assert_eq!(list_body, "[]", "nothing should have actually been spawned");

        cleanup(&canvas_path);
    }

    #[tokio::test]
    async fn force_start_kills_the_stale_owner_and_starts_the_service() {
        let canvas_path = write_test_canvas(SERVICE_CANVAS);
        let lock_path = meshfox_core::service_lock_path(&canvas_path, "root", "srv");
        meshfox_core::service_lock::acquire(&lock_path, 999_999, "tui").unwrap();

        let addr = spawn_test_server(canvas_path.clone()).await;
        let (status, body) = post_json(
            addr,
            "/api/services/force-start",
            r#"{"path":[],"block":"srv","vars":{}}"#,
        )
        .await;
        assert_eq!(status, 200, "unexpected body: {body}");
        let resp: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert!(resp["pid"].as_u64().unwrap() > 0);

        let (_, list_body) = get(addr, "/api/services").await;
        let services: Vec<serde_json::Value> = serde_json::from_str(&list_body).unwrap();
        assert_eq!(services.len(), 1);
        assert_eq!(services[0]["status"], "running");

        let _ = post_json(
            addr,
            "/api/services/stop",
            r#"{"nodeId":"root","block":"srv"}"#,
        )
        .await;
        cleanup(&canvas_path);
    }
}

#[cfg(test)]
mod debug_endpoint_tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpStream;

    fn write_test_canvas(contents: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("meshfox-debug-endpoint-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("doc.canvas.md");
        std::fs::write(&path, contents).unwrap();
        path
    }

    fn cleanup(canvas_path: &std::path::Path) {
        if let Some(dir) = canvas_path.parent() {
            let _ = std::fs::remove_dir_all(dir);
        }
    }

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

    const DEBUG_CANVAS: &str = concat!(
        "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
        "```bash name=\"snippet\"\necho hi\n```\n",
    );

    /// A `/api/debug/start` → `/api/debug/send` → `/api/debug/stop` round
    /// trip against a real spawned worker — the `crate::coordinator`-routed
    /// counterpart to `crate::mcp`'s own in-process debug-session tests
    /// (`debug_send_timeout_kills_a_command_that_exits_on_sigterm` etc.,
    /// which cover `DebugSession`'s own timeout/escalation logic directly
    /// and don't need repeating here — this only checks the HTTP plumbing
    /// around it: state survives between separate requests, keyed by
    /// `sessionId`, exactly like the in-process `self.sessions` map does
    /// across separate MCP tool calls).
    #[tokio::test]
    async fn start_send_stop_round_trip_persists_state_between_calls() {
        let canvas_path = write_test_canvas(DEBUG_CANVAS);
        let addr = spawn_test_server(canvas_path.clone()).await;

        let (status, body) = post_json(
            addr,
            "/api/debug/start",
            r#"{"nodeId":"root","blockName":"snippet","vars":{}}"#,
        )
        .await;
        assert_eq!(status, 200, "unexpected body: {body}");
        let start: serde_json::Value = serde_json::from_str(&body).unwrap();
        let session_id = start["sessionId"].as_str().unwrap().to_string();
        assert_eq!(start["blockName"], "snippet");

        // A variable exported in one `send` call is still visible in the
        // next — the whole point of a debug session over a one-shot `run`.
        let (status, _) = post_json(
            addr,
            "/api/debug/send",
            &serde_json::json!({"sessionId": session_id, "code": "export FOO=bar"}).to_string(),
        )
        .await;
        assert_eq!(status, 200);

        let (status, body) = post_json(
            addr,
            "/api/debug/send",
            &serde_json::json!({"sessionId": session_id, "code": "echo $FOO"}).to_string(),
        )
        .await;
        assert_eq!(status, 200, "unexpected body: {body}");
        let outcome: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(outcome["stdout"], "bar");
        assert_eq!(outcome["exitCode"], 0);
        assert_eq!(outcome["sessionEnded"], false);

        let (status, _) = post_json(
            addr,
            "/api/debug/stop",
            &serde_json::json!({"sessionId": session_id}).to_string(),
        )
        .await;
        assert_eq!(status, 204);

        // The session is gone — a further `send` against the same id 404s.
        let (status, _) = post_json(
            addr,
            "/api/debug/send",
            &serde_json::json!({"sessionId": session_id, "code": "echo again"}).to_string(),
        )
        .await;
        assert_eq!(status, 404);

        cleanup(&canvas_path);
    }

    #[tokio::test]
    async fn start_404s_for_an_unknown_node() {
        let canvas_path = write_test_canvas(DEBUG_CANVAS);
        let addr = spawn_test_server(canvas_path.clone()).await;

        let (status, _) = post_json(
            addr,
            "/api/debug/start",
            r#"{"nodeId":"nope","vars":{}}"#,
        )
        .await;
        assert_eq!(status, 404);

        cleanup(&canvas_path);
    }
}

/// Queued-time, address-scoped locking for *any* runnable block — not just
/// `service` (see `service_endpoint_tests`, above, for the service-specific
/// cases) — covers the explicit "forbid parallel execution of the same
/// block" behavior (`acquire_chain_locks`/`steps_needing_a_lock`) and its
/// generalized force-run escape hatch (`force_run`).
#[cfg(test)]
mod run_lock_tests {
    use super::*;
    use futures_util::StreamExt;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpStream;
    use tokio_tungstenite::tungstenite::Message as WsMessage;
    use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

    type TestSocket = WebSocketStream<MaybeTlsStream<TcpStream>>;

    /// `/api/run` is a WS upgrade now — connects to `/api/run?{query}` and
    /// returns the still-open socket, for a test that needs to read a few
    /// events and then act (drop the connection, race a second one) rather
    /// than just collecting everything to the end.
    async fn connect_run_ws(addr: SocketAddr, query: &str) -> TestSocket {
        let url = format!("ws://{addr}/api/run?{query}");
        let (ws, _) = tokio_tungstenite::connect_async(url).await.expect("connect");
        ws
    }

    async fn next_run_event(ws: &mut TestSocket) -> serde_json::Value {
        match ws.next().await.expect("socket open").expect("no ws error") {
            WsMessage::Text(t) => serde_json::from_str(&t).expect("valid RunEvent JSON"),
            other => panic!("unexpected message: {other:?}"),
        }
    }

    /// Collects every `RunEvent` text frame until the socket closes.
    async fn run_ws_events(addr: SocketAddr, query: &str) -> Vec<serde_json::Value> {
        let mut ws = connect_run_ws(addr, query).await;
        let mut events = Vec::new();
        while let Some(msg) = ws.next().await {
            match msg.expect("no ws error") {
                WsMessage::Text(t) => {
                    events.push(serde_json::from_str(&t).expect("valid RunEvent JSON"));
                }
                WsMessage::Close(_) => break,
                _ => continue,
            }
        }
        events
    }

    /// `/api/run/subscribe` is a WS upgrade now too — collects every
    /// `SubscribeEvent` text frame until the socket closes.
    async fn subscribe_ws_events(addr: SocketAddr, query: &str) -> Vec<serde_json::Value> {
        let url = format!("ws://{addr}{query}");
        let (mut ws, _) = tokio_tungstenite::connect_async(url).await.expect("connect");
        let mut events = Vec::new();
        while let Some(msg) = ws.next().await {
            match msg.expect("no ws error") {
                WsMessage::Text(t) => {
                    events.push(serde_json::from_str(&t).expect("valid RunEvent JSON"));
                }
                WsMessage::Close(_) => break,
                _ => continue,
            }
        }
        events
    }

    fn write_test_canvas(contents: &str) -> PathBuf {
        // Own directory per test, not a flat shared temp dir — see
        // `service_endpoint_tests::write_test_canvas`'s own doc comment on
        // why a shared `.meshfox/services/` directory across concurrently-
        // running tests is a real cross-test-flakiness hazard once more
        // than a couple of tests touch locks at once (this whole module's
        // entire point).
        let dir = std::env::temp_dir().join(format!("meshfox-run-lock-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("doc.canvas.md");
        std::fs::write(&path, contents).unwrap();
        path
    }

    fn cleanup(canvas_path: &std::path::Path) {
        if let Some(dir) = canvas_path.parent() {
            let _ = std::fs::remove_dir_all(dir);
        }
    }

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

    const PLAIN_CANVAS: &str = concat!(
        "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
        "```bash name=\"slow\"\nsleep 0.3\necho done\n```\n",
    );

    #[tokio::test]
    async fn a_client_disconnecting_mid_run_does_not_free_the_lock_while_it_keeps_running() {
        // Regression test: the up-front lock used to be released whenever
        // *this generator* ended (including a client disconnect), not
        // when the process it protects actually finished — since the
        // process itself now survives a disconnect (that's the whole
        // point of `run_registry`), that meant a disconnect could free the
        // address's lock while a real process was still genuinely running
        // under it, letting a second, truly concurrent request start right
        // on top of it.
        let canvas_path = write_test_canvas(PLAIN_CANVAS);
        let addr = spawn_test_server(canvas_path.clone()).await;

        {
            // A WS connection, dropped abruptly partway through —
            // simulating a tab reload/close mid-stream, unlike
            // `run_ws_events`'s own read-to-close helper.
            let mut ws = connect_run_ws(addr, "block=slow").await;
            // Enough to know the server actually started running the
            // block (past `started`/`step-start`) before this connection
            // gets dropped, unread, at the end of this block.
            let started = next_run_event(&mut ws).await;
            assert_eq!(started["type"], "started");
        } // `ws` dropped here — an abrupt client disconnect.

        // The block's own `sleep 0.3` means it's still running at this
        // point — a second, genuinely concurrent request right now must
        // still see the lock held, not incorrectly freed by the first
        // request's own connection having just dropped.
        let events = run_ws_events(addr, "block=slow").await;
        assert_eq!(
            events[0]["type"], "lock-conflict",
            "the lock should still be held by the still-running first request: {events:?}"
        );

        // Once the (disconnected, but still server-side-running) first
        // request's own block actually finishes, the lock frees up again
        // normally.
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        let events = run_ws_events(addr, "block=slow").await;
        assert!(
            events.iter().any(|e| e["type"] == "done"),
            "unexpected events: {events:?}"
        );

        cleanup(&canvas_path);
    }

    #[tokio::test]
    async fn a_run_already_in_progress_anywhere_rejects_a_second_one() {
        let canvas_path = write_test_canvas(PLAIN_CANVAS);
        // Simulates this exact address already being mid-execution — same
        // deterministic stand-in `service_endpoint_tests`'s own lock-
        // conflict test uses (a real concurrent HTTP race would work too,
        // but is inherently timing-sensitive; this exercises the exact
        // same `acquire_chain_locks` conflict path without depending on
        // scheduling). Our own test pid stands in for "a live process" —
        // unlike the *stale*-owner tests elsewhere, `is_alive` here is
        // true, matching "genuinely still running", not "abandoned".
        let lock_path = meshfox_core::service_lock_path(&canvas_path, "root", "slow");
        meshfox_core::service_lock::acquire(&lock_path, std::process::id(), "webui").unwrap();

        let addr = spawn_test_server(canvas_path.clone()).await;
        let events = run_ws_events(addr, "block=slow").await;
        assert_eq!(events[0]["type"], "lock-conflict", "unexpected events: {events:?}");
        assert_eq!(events[0]["nodeId"], "root");
        assert_eq!(events[0]["block"], "slow");
        assert_eq!(events[0]["ownerPid"], std::process::id());

        meshfox_core::service_lock::release(&lock_path).unwrap();
        cleanup(&canvas_path);
    }

    // A real two-concurrent-`tokio::join!`-requests variant was tried here
    // and deliberately removed: under enough system load (this whole test
    // module's own real subprocess spawns are plenty), the *second* request
    // can genuinely not reach the server until well after the first has
    // already finished and released its lock — both then legitimately
    // succeed, which isn't a locking bug, just this test's own assumption
    // that `tokio::join!` guarantees server-side overlap not holding up
    // under contention. `a_run_already_in_progress_anywhere_rejects_a_
    // second_one` (above) exercises the exact same `acquire_chain_locks`
    // conflict path deterministically instead, without depending on real
    // scheduling — that's the reliable regression test for this guarantee.

    #[tokio::test]
    async fn once_the_first_run_finishes_the_lock_is_free_again() {
        let canvas_path = write_test_canvas(PLAIN_CANVAS);
        let addr = spawn_test_server(canvas_path.clone()).await;

        let events = run_ws_events(addr, "block=slow").await;
        assert_eq!(events[0]["type"], "started", "unexpected events: {events:?}");

        // The first run has already fully completed (`run_ws_events` only
        // returns once the socket closes, i.e. after `Done`) — its own
        // execution-scoped lock must already be released, so a fresh run
        // right after succeeds rather than conflicting with itself.
        let events = run_ws_events(addr, "block=slow").await;
        assert_eq!(events[0]["type"], "started", "unexpected events: {events:?}");
        assert!(events.iter().any(|e| e["type"] == "done"));

        cleanup(&canvas_path);
    }

    #[tokio::test]
    async fn force_run_takes_over_a_stale_conflict_and_completes() {
        let canvas_path = write_test_canvas(PLAIN_CANVAS);
        // Same "definitely not us, definitely not alive" stand-in
        // `service_endpoint_tests` already uses for a stale owner.
        let lock_path = meshfox_core::service_lock_path(&canvas_path, "root", "slow");
        meshfox_core::service_lock::acquire(&lock_path, 999_999, "tui").unwrap();

        let addr = spawn_test_server(canvas_path.clone()).await;
        let events = run_ws_events(addr, "block=slow").await;
        assert_eq!(events[0]["type"], "lock-conflict", "unexpected events: {events:?}");
        assert_eq!(events[0]["ownerPid"], 999_999);

        let url = reqwest::Url::parse_with_params(
            &format!("ws://{addr}/api/run/force"),
            &[
                ("block", "slow"),
                ("forceNodeId", "root"),
                ("forceBlock", "slow"),
            ],
        )
        .expect("valid url");
        let (mut ws, _) = tokio_tungstenite::connect_async(url.as_str())
            .await
            .expect("connect");
        let mut events = Vec::new();
        while let Some(msg) = ws.next().await {
            match msg.expect("no ws error") {
                WsMessage::Text(t) => {
                    events.push(serde_json::from_str::<serde_json::Value>(&t).expect("valid RunEvent JSON"));
                }
                WsMessage::Close(_) => break,
                _ => continue,
            }
        }
        assert!(
            events.iter().any(|e| e["type"] == "done"),
            "force-run should have completed the block normally: {events:?}"
        );

        cleanup(&canvas_path);
    }

    #[tokio::test]
    async fn subscribe_to_an_address_that_never_ran_is_a_404() {
        let canvas_path = write_test_canvas(PLAIN_CANVAS);
        let addr = spawn_test_server(canvas_path.clone()).await;

        // A 404 here is a deliberate silent no-op: the socket still
        // upgrades (a browser `WebSocket` can't read a pre-upgrade
        // status), but closes immediately with zero messages.
        let events =
            subscribe_ws_events(addr, "/api/run/subscribe?nodeId=root&block=slow&sinceSeq=0")
                .await;
        assert!(events.is_empty(), "expected no events, got: {events:?}");

        cleanup(&canvas_path);
    }

    #[tokio::test]
    async fn a_second_connection_watches_the_same_run_via_subscribe() {
        let canvas_path = write_test_canvas(PLAIN_CANVAS);
        let addr = spawn_test_server(canvas_path.clone()).await;

        let run = tokio::spawn(async move { run_ws_events(addr, "block=slow").await });
        // Give the run a moment to actually register itself in
        // `runs_registry` (near-instant once the request's own preamble
        // clears) before subscribing to it from a second, independent
        // connection — well short of its own 0.3s sleep either way.
        tokio::time::sleep(std::time::Duration::from_millis(60)).await;

        let sub_events =
            subscribe_ws_events(addr, "/api/run/subscribe?nodeId=root&block=slow&sinceSeq=0")
                .await;
        assert!(
            sub_events.iter().any(|e| e["type"] == "line" && e["text"] == "done"),
            "expected the block's own output line replayed/tailed, got: {sub_events:?}"
        );
        assert!(
            sub_events
                .iter()
                .any(|e| e["type"] == "done" && e["outcome"] == "exited"),
            "expected a terminal done/exited event once the run finished, got: {sub_events:?}"
        );

        let run_events = run.await.unwrap();
        assert!(
            run_events.iter().any(|e| e["type"] == "done"),
            "the originating connection's own run should be unaffected by being watched: {run_events:?}"
        );

        cleanup(&canvas_path);
    }

    #[tokio::test]
    async fn subscribing_after_a_run_already_finished_still_replays_it() {
        let canvas_path = write_test_canvas(PLAIN_CANVAS);
        let addr = spawn_test_server(canvas_path.clone()).await;

        let run_events = run_ws_events(addr, "block=slow").await;
        assert!(run_events.iter().any(|e| e["type"] == "done"));

        // The run is long over by now (`run_ws_events` only returns once
        // the socket closes, i.e. after `done`) — its `RunHandle` should
        // still be sitting in the registry with its buffered log intact.
        let sub_events =
            subscribe_ws_events(addr, "/api/run/subscribe?nodeId=root&block=slow&sinceSeq=0")
                .await;
        assert!(sub_events.iter().any(|e| e["type"] == "line" && e["text"] == "done"));
        assert!(
            sub_events
                .iter()
                .any(|e| e["type"] == "done" && e["outcome"] == "exited")
        );
        // Regression test: `SubscribeEvent` used to derive `rename_all =
        // "camelCase"` alone, which (unlike `RunEvent`'s own combo further
        // up this file) only renames the *variant* tag on an enum, not the
        // fields inside a struct variant — missing the separate
        // `rename_all_fields` meant `exit_code` shipped as literal
        // snake_case, which the frontend's camelCase-typed `exitCode` read
        // back as `undefined` — indistinguishable from a genuinely missing
        // value, and enough to make a *successful* reconciled run
        // (`undefined !== 0`) show up as failed.
        let done_event = sub_events
            .iter()
            .find(|e| e["type"] == "done")
            .expect("a done event");
        assert_eq!(
            done_event["exitCode"], 0,
            "expected a camelCase exitCode field, got: {done_event:?}"
        );
        assert!(
            done_event.get("exit_code").is_none(),
            "exit_code leaked through in snake_case: {done_event:?}"
        );

        cleanup(&canvas_path);
    }

    #[tokio::test]
    async fn killing_by_address_stops_a_run_no_run_id_needed() {
        let canvas_path = write_test_canvas(PLAIN_CANVAS);
        let addr = spawn_test_server(canvas_path.clone()).await;

        let run = tokio::spawn(async move { run_ws_events(addr, "block=slow").await });
        tokio::time::sleep(std::time::Duration::from_millis(60)).await;

        let (kill_status, _) = post_json(
            addr,
            "/api/kill",
            r#"{"nodeId":"root","block":"slow"}"#,
        )
        .await;
        assert_eq!(kill_status, 204);

        let run_events = run.await.unwrap();
        assert!(
            run_events.iter().any(|e| e["type"] == "killed"),
            "expected the run to report killed, got: {run_events:?}"
        );

        cleanup(&canvas_path);
    }

    #[tokio::test]
    async fn active_runs_lists_a_still_running_plain_block() {
        let canvas_path = write_test_canvas(PLAIN_CANVAS);
        let addr = spawn_test_server(canvas_path.clone()).await;

        let run = tokio::spawn(async move { run_ws_events(addr, "block=slow").await });
        tokio::time::sleep(std::time::Duration::from_millis(60)).await;

        let (status, body) = get(addr, "/api/runs").await;
        assert_eq!(status, 200, "unexpected body: {body}");
        let runs: serde_json::Value = serde_json::from_str(&body).unwrap();
        let entry = runs
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["nodeId"] == "root" && r["block"] == "slow")
            .unwrap_or_else(|| panic!("expected an active-runs entry for root/slow, got: {body}"));
        assert_eq!(entry["kind"], "plain");
        assert_eq!(entry["status"], "running");

        let run_events = run.await.unwrap();
        assert!(run_events.iter().any(|e| e["type"] == "done"));

        cleanup(&canvas_path);
    }

    #[tokio::test]
    async fn active_runs_still_lists_a_just_finished_run() {
        let canvas_path = write_test_canvas(PLAIN_CANVAS);
        let addr = spawn_test_server(canvas_path.clone()).await;

        let run_events = run_ws_events(addr, "block=slow").await;
        assert!(run_events.iter().any(|e| e["type"] == "done"));

        let (status, body) = get(addr, "/api/runs").await;
        assert_eq!(status, 200, "unexpected body: {body}");
        let runs: serde_json::Value = serde_json::from_str(&body).unwrap();
        let entry = runs
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["nodeId"] == "root" && r["block"] == "slow")
            .unwrap_or_else(|| panic!("expected an active-runs entry for root/slow, got: {body}"));
        assert_eq!(entry["kind"], "plain");
        assert_eq!(entry["status"], "exited");
        assert_eq!(entry["exitCode"], 0);

        cleanup(&canvas_path);
    }
}

#[cfg(test)]
mod node_block_and_reorder_endpoint_tests {
    use super::*;

    fn write_test_canvas(contents: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "meshfox-node-block-reorder-test-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("doc.canvas.md");
        std::fs::write(&path, contents).unwrap();
        path
    }

    fn blank_block_request() -> UpdateBlockAttrsRequest {
        UpdateBlockAttrsRequest {
            name: None,
            lang: None,
            cache: None,
            always: None,
            default: None,
            tty: None,
            autoclose: None,
            service: None,
            deps: None,
            env: None,
            interpreter: None,
            clear_interpreter: false,
            code: None,
        }
    }

    const ONE_BLOCK: &str = concat!(
        "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
        "```bash name=\"hi\"\necho hi\n```\n",
    );

    #[tokio::test]
    async fn update_block_attrs_sets_cache() {
        let canvas_path = write_test_canvas(ONE_BLOCK);
        let state = build_state(canvas_path.clone(), false, None)
            .await
            .expect("valid test canvas");

        let mut req = blank_block_request();
        req.cache = Some(true);
        let Json(updated) = update_block_attrs(
            State(state),
            Path(("root".to_string(), "hi".to_string())),
            Json(req),
        )
        .await
        .expect("update should succeed");
        let node = updated.node("root").unwrap();
        let blocks = meshfox_core::scan_runnable_blocks("root", &node.text);
        let block = blocks.iter().find(|b| b.name.as_deref() == Some("hi")).unwrap();
        assert!(block.cache);

        let _ = std::fs::remove_file(&canvas_path);
    }

    #[tokio::test]
    async fn update_block_attrs_sets_and_clears_deps() {
        let canvas_path = write_test_canvas(concat!(
            "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
            "```bash name=\"a\"\necho a\n```\n\n",
            "```bash name=\"b\"\necho b\n```\n",
        ));
        let state = build_state(canvas_path.clone(), false, None)
            .await
            .expect("valid test canvas");

        let mut req = blank_block_request();
        req.deps = Some("a".to_string());
        let Json(updated) = update_block_attrs(
            State(state.clone()),
            Path(("root".to_string(), "b".to_string())),
            Json(req),
        )
        .await
        .expect("update should succeed");
        let node = updated.node("root").unwrap();
        let blocks = meshfox_core::scan_runnable_blocks("root", &node.text);
        let block_b = blocks.iter().find(|b| b.name.as_deref() == Some("b")).unwrap();
        assert_eq!(block_b.deps.len(), 1);
        assert_eq!(block_b.deps[0].block_name, "a");

        let mut clear_req = blank_block_request();
        clear_req.deps = Some(String::new());
        let Json(cleared) = update_block_attrs(
            State(state),
            Path(("root".to_string(), "b".to_string())),
            Json(clear_req),
        )
        .await
        .expect("clear should succeed");
        let node = cleared.node("root").unwrap();
        let blocks = meshfox_core::scan_runnable_blocks("root", &node.text);
        let block_b = blocks.iter().find(|b| b.name.as_deref() == Some("b")).unwrap();
        assert!(block_b.deps.is_empty());

        let _ = std::fs::remove_file(&canvas_path);
    }

    #[tokio::test]
    async fn update_block_attrs_404s_for_an_unknown_block_name() {
        let canvas_path = write_test_canvas(ONE_BLOCK);
        let state = build_state(canvas_path.clone(), false, None)
            .await
            .expect("valid test canvas");

        let req = blank_block_request();
        let err = update_block_attrs(
            State(state),
            Path(("root".to_string(), "nope".to_string())),
            Json(req),
        )
        .await
        .expect_err("expected a 404");
        assert_eq!(err.0, StatusCode::NOT_FOUND);

        let _ = std::fs::remove_file(&canvas_path);
    }

    #[tokio::test]
    async fn update_block_attrs_rejects_interpreter_with_clear_interpreter() {
        let canvas_path = write_test_canvas(ONE_BLOCK);
        let state = build_state(canvas_path.clone(), false, None)
            .await
            .expect("valid test canvas");

        let mut req = blank_block_request();
        req.interpreter = Some("python3".to_string());
        req.clear_interpreter = true;
        let err = update_block_attrs(
            State(state),
            Path(("root".to_string(), "hi".to_string())),
            Json(req),
        )
        .await
        .expect_err("expected a 422");
        assert_eq!(err.0, StatusCode::UNPROCESSABLE_ENTITY);

        let _ = std::fs::remove_file(&canvas_path);
    }

    #[tokio::test]
    async fn reorder_siblings_resorts_children_by_position() {
        let canvas_path = write_test_canvas(concat!(
            "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
            "## B\n<!-- meshfox:node id=\"b\" x=0 y=0 -->\n\n",
            "## A\n<!-- meshfox:node id=\"a\" x=0 y=-100 -->\n\n",
        ));
        let state = build_state(canvas_path.clone(), false, None)
            .await
            .expect("valid test canvas");

        let Json(updated) = reorder_siblings(State(state))
            .await
            .expect("reorder should succeed");
        let ids: Vec<&str> = updated.nodes.iter().map(|n| n.id.as_str()).collect();
        let pos_a = ids.iter().position(|&id| id == "a").unwrap();
        let pos_b = ids.iter().position(|&id| id == "b").unwrap();
        assert!(pos_a < pos_b, "expected a (y=-100) before b (y=0), got {ids:?}");

        let _ = std::fs::remove_file(&canvas_path);
    }
}
