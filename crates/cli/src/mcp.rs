//! `meshfox mcp` — an MCP stdio server for AI agents (see TODO.canvas.md's
//! "MCP-сессия"/"Несколько канвасов в одном MCP-сервере?" nodes). Takes no
//! arguments: whatever directory it's started in becomes its root. Two
//! layers, same binary, distinguished only by [`LEAF_ENV_VAR`]:
//!
//! - **Root** (default — what a host actually launches): [`MeshfoxMcpRoot`].
//!   Doesn't touch any canvas file itself. `canvas_open`/`canvas_close`/
//!   `canvas_list` manage a registry of canvases, each backed by its own
//!   spawned `meshfox mcp` **child process** (via
//!   `rmcp::transport::TokioChildProcess`, the same client transport an MCP
//!   host itself uses to launch a stdio server — this is the exact same
//!   mechanism, just one level up). Every other tool takes a required
//!   `canvas_id` and is a pure proxy: forward the identically-named,
//!   identically-shaped call to that canvas's own child process, return
//!   whatever it says. One file, one process — a crash or a hung
//!   `debug_send` on one canvas can't touch another — while a host still
//!   sees exactly one MCP server.
//! - **Leaf** (`MESHFOX_MCP_LEAF=1` in the environment, canvas path in
//!   [`LEAF_PATH_ENV_VAR`] — only `canvas_open` sets either, never a human):
//!   [`MeshfoxMcp`], the original single-file server, unchanged.
//!   `node_show`/`add`/`meta`/`body`/`block`/`rm`/`mv`/`rename`/`set_id`/
//!   `edges`/`move`/`reorder`/`find` are thin wrappers around the same pure
//!   `apply_node_*`/`find_node_ids` functions `node <op>` already uses;
//!   `debug_start`/`debug_send`/`debug_stop` run a persistent `bash` kept
//!   alive in a node/block's own resolved cwd/env, so state between calls
//!   (exported vars, files a snippet wrote) survives the way a one-shot
//!   `meshfox run` never could. Each call is its own immediate
//!   read-modify-write of the file on disk — no batching, no
//!   optimistic-concurrency conflict detection against a concurrent editor
//!   (see TODO.canvas.md's own still-open "MCP-редактирование файла" node).
//!
//! `canvas_open` only ever resolves paths under the **root directory** — the
//! canonicalized directory `meshfox mcp` was started in — rejecting anything
//! that escapes it (`..`, an absolute path elsewhere, a symlink pointing
//! out). A canvas id is that file's path relative to the root; opening an
//! already-open file just returns the same id rather than spawning a second
//! process for it.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};

use meshfox_core::Canvas;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolRequestParams, CallToolResult, ServerCapabilities, ServerInfo};
use rmcp::service::RunningService;
use rmcp::transport::{ConfigureCommandExt, TokioChildProcess};
use rmcp::{
    tool, tool_handler, tool_router, ErrorData, RoleClient, ServerHandler, ServiceError,
    ServiceExt,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{mpsc, Mutex};

const IDLE_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const SWEEP_INTERVAL: Duration = Duration::from_secs(5 * 60);
const DEFAULT_SEND_TIMEOUT_MS: u64 = 60_000;
const CANVAS_CLOSE_TIMEOUT: Duration = Duration::from_secs(5);

/// Set (to any value) only in the environment of a process `canvas_open`
/// itself spawns — never meant for a human to set. Its presence is the only
/// thing that distinguishes a leaf `meshfox mcp` process from the root one a
/// host actually launches; see this module's own doc comment.
const LEAF_ENV_VAR: &str = "MESHFOX_MCP_LEAF";

/// The canvas path a leaf process serves, passed as an environment variable
/// rather than a CLI argument so `meshfox mcp` itself stays argument-free
/// for a human/host to launch — only `canvas_open` ever sets this, right
/// alongside [`LEAF_ENV_VAR`].
const LEAF_PATH_ENV_VAR: &str = "MESHFOX_MCP_LEAF_PATH";

pub async fn run() -> Result<(), String> {
    if std::env::var_os(LEAF_ENV_VAR).is_some() {
        let canvas_path = std::env::var_os(LEAF_PATH_ENV_VAR)
            .map(PathBuf::from)
            .ok_or_else(|| format!("{LEAF_PATH_ENV_VAR} not set in a leaf process"))?;
        run_leaf(canvas_path).await
    } else {
        run_root().await
    }
}

async fn run_leaf(canvas_path: PathBuf) -> Result<(), String> {
    let server = MeshfoxMcp::new(canvas_path);
    server.spawn_idle_sweep();
    let service = server
        .serve(rmcp::transport::stdio())
        .await
        .map_err(|e| format!("failed to start MCP server: {e}"))?;
    service
        .waiting()
        .await
        .map_err(|e| format!("MCP server ended unexpectedly: {e}"))?;
    Ok(())
}

async fn run_root() -> Result<(), String> {
    let cwd = std::env::current_dir()
        .map_err(|e| format!("failed to resolve the current directory: {e}"))?;
    let root = cwd
        .canonicalize()
        .map_err(|e| format!("failed to resolve {} as a root directory: {e}", cwd.display()))?;
    let server = MeshfoxMcpRoot::new(root);
    server.spawn_idle_sweep();
    let service = server
        .serve(rmcp::transport::stdio())
        .await
        .map_err(|e| format!("failed to start MCP server: {e}"))?;
    service
        .waiting()
        .await
        .map_err(|e| format!("MCP server ended unexpectedly: {e}"))?;
    Ok(())
}

fn invalid_params(msg: impl Into<String>) -> ErrorData {
    ErrorData::invalid_params(msg.into(), None)
}

// =======================================================================
// Leaf: one process, one canvas file — the original implementation.
// =======================================================================

#[derive(Clone)]
struct MeshfoxMcp {
    canvas_path: PathBuf,
    sessions: Arc<Mutex<HashMap<String, Arc<Mutex<DebugSession>>>>>,
    tool_router: ToolRouter<Self>,
}

impl MeshfoxMcp {
    fn new(canvas_path: PathBuf) -> Self {
        Self {
            canvas_path,
            sessions: Arc::new(Mutex::new(HashMap::new())),
            tool_router: Self::tool_router(),
        }
    }

    fn spawn_idle_sweep(&self) {
        let sessions = Arc::clone(&self.sessions);
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(SWEEP_INTERVAL).await;
                let idle_ids: Vec<String> = {
                    let map = sessions.lock().await;
                    let mut idle = Vec::new();
                    for (id, session) in map.iter() {
                        if session.lock().await.last_used.elapsed() > IDLE_TIMEOUT {
                            idle.push(id.clone());
                        }
                    }
                    idle
                };
                for id in idle_ids {
                    let removed = sessions.lock().await.remove(&id);
                    if let Some(session) = removed {
                        session.lock().await.stop().await;
                    }
                }
            }
        });
    }

    fn read_raw(&self) -> Result<String, ErrorData> {
        std::fs::read_to_string(&self.canvas_path).map_err(|e| {
            ErrorData::internal_error(
                format!("failed to read {}: {e}", self.canvas_path.display()),
                None,
            )
        })
    }

    fn write_raw(&self, content: &str) -> Result<(), ErrorData> {
        std::fs::write(&self.canvas_path, content).map_err(|e| {
            ErrorData::internal_error(
                format!("failed to write {}: {e}", self.canvas_path.display()),
                None,
            )
        })
    }
}

// ---------------------------------------------------------------------
// Debug session
// ---------------------------------------------------------------------

enum StreamLine {
    Out(String),
    Err(String),
}

struct DebugSession {
    child: Child,
    stdin: ChildStdin,
    lines_rx: mpsc::UnboundedReceiver<StreamLine>,
    last_used: Instant,
}

struct SendOutcome {
    stdout: String,
    stderr: String,
    exit_code: i32,
    timed_out: bool,
    session_ended: bool,
}

impl DebugSession {
    fn spawn(cwd: &std::path::Path, envs: HashMap<String, String>) -> std::io::Result<Self> {
        let mut command = Command::new("bash");
        // `--noprofile --norc`: a plain interactive-less shell, not a login
        // shell — nothing from the user's own `.bashrc` should silently
        // change how a debug snippet behaves. Reads commands from its own
        // stdin pipe, same as `bash < script.sh` — not a pty (see this
        // module's own doc comment for why: separate stdout/stderr, no
        // ANSI/terminal concerns to strip).
        command.arg("--noprofile").arg("--norc");
        command.current_dir(cwd);
        command.envs(envs);
        command.stdin(Stdio::piped());
        command.stdout(Stdio::piped());
        command.stderr(Stdio::piped());
        command.kill_on_drop(true);
        // Same reasoning as `stream_exec::SpawnedProcess::kill`: a fresh
        // process group so `stop()` can reach everything this shell itself
        // spawns, not just `bash`.
        command.process_group(0);

        let mut child = command.spawn()?;
        let stdin = child.stdin.take().expect("piped stdin");
        let stdout = child.stdout.take().expect("piped stdout");
        let stderr = child.stderr.take().expect("piped stderr");

        let (tx, rx) = mpsc::unbounded_channel();
        let tx_out = tx.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if tx_out.send(StreamLine::Out(line)).is_err() {
                    break;
                }
            }
        });
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if tx.send(StreamLine::Err(line)).is_err() {
                    break;
                }
            }
        });

        Ok(DebugSession {
            child,
            stdin,
            lines_rx: rx,
            last_used: Instant::now(),
        })
    }

    /// Runs `code` in this session's own shell and waits for it to finish —
    /// marked by a unique sentinel line this call appends after `code`,
    /// printed to *both* streams with the real exit code so completion is
    /// only declared once both stdout and stderr have delivered everything
    /// up to that point (they're unrelated pipes with no ordering
    /// guarantee between them). If `code` itself times out or never
    /// finishes, the session is still alive afterward — but this call
    /// can't tell that command's own trailing output apart from whatever
    /// the *next* `debug_send` gets back; `debug_stop`/a fresh session is
    /// the clean way out of that, not something this method tries to
    /// detect or fix.
    async fn send(&mut self, code: &str, timeout: Duration) -> std::io::Result<SendOutcome> {
        self.last_used = Instant::now();
        let marker = format!("__meshfox_done_{}__", uuid::Uuid::new_v4().simple());
        let wrapped = format!(
            "{code}\n__mfx_rc=$?\nprintf '%s %s\\n' '{marker}' \"$__mfx_rc\" >&2\nprintf '%s %s\\n' '{marker}' \"$__mfx_rc\" >&1\n"
        );
        self.stdin.write_all(wrapped.as_bytes()).await?;
        self.stdin.flush().await?;

        let mut stdout_lines = Vec::new();
        let mut stderr_lines = Vec::new();
        let mut exit_code: i32 = -1;
        let mut stdout_done = false;
        let mut stderr_done = false;
        let deadline = tokio::time::Instant::now() + timeout;

        while !(stdout_done && stderr_done) {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return Ok(SendOutcome {
                    stdout: stdout_lines.join("\n"),
                    stderr: stderr_lines.join("\n"),
                    exit_code,
                    timed_out: true,
                    session_ended: false,
                });
            }
            match tokio::time::timeout(remaining, self.lines_rx.recv()).await {
                Ok(Some(StreamLine::Out(line))) => match strip_marker(&line, &marker) {
                    Some(rc) => {
                        exit_code = rc;
                        stdout_done = true;
                    }
                    None => stdout_lines.push(line),
                },
                Ok(Some(StreamLine::Err(line))) => match strip_marker(&line, &marker) {
                    Some(rc) => {
                        exit_code = rc;
                        stderr_done = true;
                    }
                    None => stderr_lines.push(line),
                },
                // Both streams closed — the shell itself exited (e.g. the
                // debug code called `exit`) — nothing more will ever
                // arrive, so stop waiting rather than spin until the
                // timeout for no reason.
                Ok(None) => {
                    return Ok(SendOutcome {
                        stdout: stdout_lines.join("\n"),
                        stderr: stderr_lines.join("\n"),
                        exit_code,
                        timed_out: false,
                        session_ended: true,
                    });
                }
                Err(_) => {
                    return Ok(SendOutcome {
                        stdout: stdout_lines.join("\n"),
                        stderr: stderr_lines.join("\n"),
                        exit_code,
                        timed_out: true,
                        session_ended: false,
                    });
                }
            }
        }
        Ok(SendOutcome {
            stdout: stdout_lines.join("\n"),
            stderr: stderr_lines.join("\n"),
            exit_code,
            timed_out: false,
            session_ended: false,
        })
    }

    async fn stop(&mut self) {
        let _ = self.kill_group();
        let _ = self.child.wait().await;
    }

    fn kill_group(&self) -> std::io::Result<()> {
        let Some(pid) = self.child.id() else {
            return Ok(()); // already reaped
        };
        // SAFETY: `libc::kill` with a negative pid signals every process in
        // that process group; `pid` is this session's own leader pid (see
        // `spawn`'s `process_group(0)`).
        let ret = unsafe { libc::kill(-(pid as libc::pid_t), libc::SIGKILL) };
        if ret != 0 && std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH) {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }
}

/// `line` is the sentinel this session's own `send` appended, if it starts
/// with `marker` — returns the exit code it carries. Anything else is
/// real output from the code that ran, passed through unchanged.
fn strip_marker(line: &str, marker: &str) -> Option<i32> {
    line.strip_prefix(marker)?.trim().parse().ok()
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
    /// How long to wait for `code` to finish before giving up (the session
    /// keeps running either way — see `timed_out` in the result). Defaults
    /// to 60000 (one minute).
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
    /// `file`-node display mode: `link` (the default) or `code`.
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
    /// Comma-separated, replacing the whole list. Pass `""` to clear.
    #[serde(default)]
    tags: Option<String>,
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
            tags: self.tags,
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
    #[serde(flatten)]
    fields: McpNodeFields,
}

#[derive(Deserialize, Serialize, JsonSchema)]
struct NodeBodyParams {
    node_id: String,
    body: String,
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
struct NodeRenameParams {
    node_id: String,
    /// New heading text. The node's id, heading level, and body are left
    /// untouched — an id is pinned the first time it's written and never
    /// follows later title edits.
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
    /// Full replacement list of extra-parent ids (`meshfox:edge from="..."`
    /// lines) — replaces whatever was already there, doesn't add to it.
    /// Pass an empty list to clear them all.
    #[serde(default)]
    from: Vec<String>,
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

#[derive(Deserialize, Serialize, JsonSchema)]
struct NodeFindParams {
    /// A CSS selector matched against the canvas tree: a node is an
    /// element, each tag is a class (`.bag`), `id`/`type`/`color` are
    /// ordinary attributes (`[type="file"]`), structural nesting is DOM
    /// nesting (`#todo > .bag` for direct children, `#todo .bag` for
    /// descendants at any depth).
    selector: String,
    /// Include each match's full `node_show`-equivalent metadata instead of
    /// just its id.
    #[serde(default)]
    show: bool,
    /// With `show`, also include each match's own Markdown body. No effect
    /// without `show`, since a bare id list has no metadata to attach it to.
    #[serde(default)]
    include_body: bool,
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

#[tool_router]
impl MeshfoxMcp {
    #[tool(
        description = "Starts a persistent debug shell in a node/block's own resolved cwd and env — state (exported vars, files written) survives across debug_send calls, unlike a one-shot `run`. Returns a session_id."
    )]
    async fn debug_start(
        &self,
        Parameters(params): Parameters<DebugStartParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let raw = self.read_raw()?;
        let canvas = Canvas::from_markdown(&raw).map_err(|e| invalid_params(e.to_string()))?;
        let node = canvas
            .node(&params.node_id)
            .ok_or_else(|| invalid_params(format!("no node {:?}", params.node_id)))?;
        let blocks = meshfox_core::scan_runnable_blocks(&params.node_id, &node.text);
        let block = match &params.block_name {
            Some(name) => blocks
                .iter()
                .find(|b| b.name.as_deref() == Some(name.as_str()))
                .ok_or_else(|| {
                    invalid_params(format!(
                        "no runnable block named {name:?} in node {:?}",
                        params.node_id
                    ))
                })?,
            None => meshfox_core::fence::default_block(&params.node_id, &blocks)
                .map_err(|names| {
                    invalid_params(format!(
                        "node {:?} has more than one default-eligible block ({}); pass block_name explicitly",
                        params.node_id,
                        names.join(", ")
                    ))
                })?
                .ok_or_else(|| {
                    invalid_params(format!(
                        "node {:?} has no default block; pass block_name explicitly",
                        params.node_id
                    ))
                })?,
        };

        let needed: std::collections::HashSet<String> =
            block.env.iter().map(|r| r.var_name.clone()).collect();
        let decls = meshfox_core::declared_vars(&canvas)
            .map_err(|e| invalid_params(e.to_string()))?;
        let relevant: Vec<_> = decls
            .iter()
            .filter(|d| needed.contains(&d.name))
            .cloned()
            .collect();
        let mut cache = meshfox_core::VarCache::load(&self.canvas_path).map_err(|e| {
            ErrorData::internal_error(format!("failed to load variable cache: {e}"), None)
        })?;
        for (name, value) in &params.vars {
            if let Some(decl) = relevant.iter().find(|d| &d.name == name) {
                meshfox_core::validate_value(decl, value).map_err(|e| {
                    invalid_params(format!("vars.{name}={value:?} is invalid: {e}"))
                })?;
            }
        }
        let resolved = meshfox_core::resolve_vars(&relevant, &params.vars, &cache, &HashMap::new());
        if !resolved.missing.is_empty() {
            let names: Vec<&str> = resolved.missing.iter().map(|d| d.name.as_str()).collect();
            return Err(invalid_params(format!(
                "missing required variable(s): {} — pass them in `vars`",
                names.join(", ")
            )));
        }
        for (name, value) in &params.vars {
            if relevant
                .iter()
                .any(|d| &d.name == name && !d.secret && !d.session)
            {
                let _ = cache.set(name, value);
            }
        }

        let envs = meshfox_core::map_block_env(&block.env, &resolved.values);
        let cwd = node.cwd(crate::canvas_root_dir(&self.canvas_path));

        let session = DebugSession::spawn(&cwd, envs).map_err(|e| {
            ErrorData::internal_error(format!("failed to start debug session: {e}"), None)
        })?;
        let session_id = uuid::Uuid::new_v4().to_string();
        self.sessions
            .lock()
            .await
            .insert(session_id.clone(), Arc::new(Mutex::new(session)));

        Ok(CallToolResult::structured(json!({
            "session_id": session_id,
            "node_id": params.node_id,
            "block_name": block.name,
            "cwd": cwd.display().to_string(),
        })))
    }

    #[tool(
        description = "Runs shell code in an already-started debug session's own shell (same process, cwd, and exported variables every earlier debug_send in this session left behind). Returns stdout, stderr, exit_code."
    )]
    async fn debug_send(
        &self,
        Parameters(params): Parameters<DebugSendParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let session = {
            let sessions = self.sessions.lock().await;
            sessions
                .get(&params.session_id)
                .cloned()
                .ok_or_else(|| invalid_params(format!("no debug session {:?}", params.session_id)))?
        };
        let timeout = Duration::from_millis(params.timeout_ms.unwrap_or(DEFAULT_SEND_TIMEOUT_MS));
        let outcome = {
            let mut session = session.lock().await;
            session.send(&params.code, timeout).await.map_err(|e| {
                ErrorData::internal_error(format!("debug session write/read failed: {e}"), None)
            })?
        };
        if outcome.session_ended {
            self.sessions.lock().await.remove(&params.session_id);
        }
        Ok(CallToolResult::structured(json!({
            "stdout": outcome.stdout,
            "stderr": outcome.stderr,
            "exit_code": outcome.exit_code,
            "timed_out": outcome.timed_out,
            "session_ended": outcome.session_ended,
        })))
    }

    #[tool(description = "Stops a debug session, killing its shell and every process it started.")]
    async fn debug_stop(
        &self,
        Parameters(params): Parameters<DebugStopParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let removed = self.sessions.lock().await.remove(&params.session_id);
        match removed {
            Some(session) => {
                session.lock().await.stop().await;
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

    #[tool(
        description = "Reads one node's structured metadata — id, title, type, parent, children, position, color, tags, and, for a file/link node, target/display. Pass include_body to also get its Markdown body."
    )]
    async fn node_show(
        &self,
        Parameters(params): Parameters<NodeIdParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let raw = self.read_raw()?;
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

    #[tool(
        description = "Adds a new child node under parent_id, as the last item in its subtree. Optionally sets its body and meta fields in the same call. Returns the new node's id."
    )]
    async fn node_add(
        &self,
        Parameters(params): Parameters<NodeAddParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let raw = self.read_raw()?;
        let (updated, new_id) = crate::apply_node_add_with_extras(
            &raw,
            &params.parent_id,
            &params.title,
            params.body.as_deref(),
            params.fields.into_node_meta_fields(),
        )
        .map_err(invalid_params)?;
        self.write_raw(&updated)?;
        Ok(CallToolResult::structured(json!({ "node_id": new_id })))
    }

    #[tool(
        description = "Updates a node's position/style/type fields. Any field left unset (or absent) keeps its current value."
    )]
    async fn node_meta(
        &self,
        Parameters(params): Parameters<NodeMetaParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let raw = self.read_raw()?;
        let f = params.fields;
        let updated = crate::apply_node_meta(
            &raw,
            &params.node_id,
            f.x,
            f.y,
            f.width,
            f.height,
            params.clear_position,
            f.color,
            f.node_type,
            f.display,
            f.lang,
            f.interpreter,
            f.preview,
            f.fold,
            f.tags,
        )
        .map_err(invalid_params)?;
        self.write_raw(&updated)?;
        Ok(CallToolResult::structured(json!({ "updated": params.node_id })))
    }

    #[tool(description = "Replaces a node's whole Markdown body.")]
    async fn node_body(
        &self,
        Parameters(params): Parameters<NodeBodyParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let raw = self.read_raw()?;
        let updated = crate::apply_node_body(&raw, &params.node_id, &params.body)
            .map_err(invalid_params)?;
        self.write_raw(&updated)?;
        Ok(CallToolResult::structured(json!({ "updated": params.node_id })))
    }

    #[tool(
        description = "Rewrites just one runnable fence's own attributes (and, optionally, its code) inside a node, leaving the rest of the node's body untouched."
    )]
    async fn node_block(
        &self,
        Parameters(params): Parameters<NodeBlockParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let raw = self.read_raw()?;
        let (cache, no_cache) = bool_pair(params.cache);
        let (always, no_always) = bool_pair(params.always);
        let (default, no_default) = bool_pair(params.default);
        let (tty, no_tty) = bool_pair(params.tty);
        let (autoclose, no_autoclose) = bool_pair(params.autoclose);
        let args = crate::BlockArgs {
            rename: params.rename,
            lang: params.lang,
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
            deps: params.deps,
            clear_deps: params.clear_deps,
            env: params.env,
            clear_env: params.clear_env,
            interpreter: params.interpreter,
            clear_interpreter: params.clear_interpreter,
            code_file: None,
        };
        let updated = crate::apply_node_block(
            &raw,
            &params.node_id,
            &params.block_name,
            &args,
            params.code.as_deref(),
        )
        .map_err(invalid_params)?;
        self.write_raw(&updated)?;
        Ok(CallToolResult::structured(json!({
            "updated": params.node_id,
            "block": params.block_name,
        })))
    }

    #[tool(
        description = "Deletes a node. By default its whole subtree goes with it; keep_children promotes its direct children to its own former parent instead."
    )]
    async fn node_rm(
        &self,
        Parameters(params): Parameters<NodeRmParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let raw = self.read_raw()?;
        let updated = crate::apply_node_rm(&raw, &params.node_id, params.keep_children)
            .map_err(invalid_params)?;
        self.write_raw(&updated)?;
        Ok(CallToolResult::structured(
            json!({ "deleted": params.node_id, "keep_children": params.keep_children }),
        ))
    }

    #[tool(description = "Moves a node to a new structural parent.")]
    async fn node_mv(
        &self,
        Parameters(params): Parameters<NodeMvParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let raw = self.read_raw()?;
        let updated = crate::apply_node_mv(&raw, &params.node_id, &params.new_parent_id)
            .map_err(invalid_params)?;
        self.write_raw(&updated)?;
        Ok(CallToolResult::structured(json!({
            "moved": params.node_id,
            "new_parent_id": params.new_parent_id,
        })))
    }

    #[tool(
        description = "Renames a node's heading text, leaving its id, heading level, and body untouched."
    )]
    async fn node_rename(
        &self,
        Parameters(params): Parameters<NodeRenameParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let raw = self.read_raw()?;
        let updated =
            crate::apply_node_rename(&raw, &params.node_id, &params.title).map_err(invalid_params)?;
        self.write_raw(&updated)?;
        Ok(CallToolResult::structured(json!({
            "renamed": params.node_id,
            "title": params.title,
        })))
    }

    #[tool(
        description = "Changes a node's id, the stable handle used for addressing, meshfox:edge/parent= references, and deps= references. Rewrites every reference it can find (best-effort for deps=). Fails if new_id is empty, contains a disallowed character, or is already used."
    )]
    async fn node_set_id(
        &self,
        Parameters(params): Parameters<NodeSetIdParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let raw = self.read_raw()?;
        let updated = crate::apply_node_set_id(&raw, &params.node_id, &params.new_id)
            .map_err(invalid_params)?;
        self.write_raw(&updated)?;
        Ok(CallToolResult::structured(json!({
            "old_id": params.node_id,
            "new_id": params.new_id,
        })))
    }

    #[tool(
        description = "Replaces a node's whole set of extra incoming edges (meshfox:edge from=\"...\" lines) — the non-structural, non-nesting cross-references. `from` replaces whatever was already there; an empty list clears them all."
    )]
    async fn node_edges(
        &self,
        Parameters(params): Parameters<NodeEdgesParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let raw = self.read_raw()?;
        let updated =
            crate::apply_node_edges(&raw, &params.node_id, &params.from).map_err(invalid_params)?;
        self.write_raw(&updated)?;
        Ok(CallToolResult::structured(json!({
            "updated": params.node_id,
            "extra_parents": params.from,
        })))
    }

    #[tool(
        description = "Moves a node's whole subtree to sit immediately before or after another sibling under the same structural parent — the on-disk heading order, which is a node's only sibling order until it also has a real x/y. Exactly one of before/after is required."
    )]
    async fn node_move(
        &self,
        Parameters(params): Parameters<NodeMoveParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let raw = self.read_raw()?;
        let (updated, target_id, position) = crate::apply_node_move(
            &raw,
            &params.node_id,
            params.before.as_deref(),
            params.after.as_deref(),
        )
        .map_err(invalid_params)?;
        self.write_raw(&updated)?;
        Ok(CallToolResult::structured(json!({
            "moved": params.node_id,
            "position": position,
            "target_id": target_id,
        })))
    }

    #[tool(
        description = "Reorders every parent's direct children in the file to match their canvas layout (sorted by y then x among ties) — the same resync the server runs on every web-UI save, exposed standalone for whenever positions changed by hand (or via node_meta) and the on-disk heading order should catch up."
    )]
    async fn node_reorder(
        &self,
        Parameters(_params): Parameters<NodeReorderParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let raw = self.read_raw()?;
        let updated = crate::apply_node_reorder(&raw).map_err(invalid_params)?;
        self.write_raw(&updated)?;
        Ok(CallToolResult::structured(json!({ "reordered": true })))
    }

    #[tool(
        description = "Finds every node matching a CSS selector — answers \"which nodes have tag X\" / \"children of node Y\" without grepping the raw file or walking node_show one node at a time. Matching runs against a synthetic document built from the canvas tree, via the same CSS engine a browser uses."
    )]
    async fn node_find(
        &self,
        Parameters(params): Parameters<NodeFindParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let raw = self.read_raw()?;
        let canvas = Canvas::from_markdown(&raw).map_err(|e| invalid_params(e.to_string()))?;
        let ids = crate::find_node_ids(&canvas, &params.selector).map_err(invalid_params)?;
        let mut result = json!({ "ids": ids });
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
    let extra_parents: Vec<&str> = node
        .extra_parents
        .iter()
        .map(|e| e.from.as_str())
        .collect();
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
        "target": node.target,
        "display": node.display.map(|d| d.as_str()),
        "preview": node.preview,
        "lang": node.lang,
    });
    if include_body {
        result["body"] = json!(node.text);
    }
    result
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for MeshfoxMcp {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build()).with_instructions(
            "Tools for a meshfox canvas: a persistent debug shell (debug_start/debug_send/debug_stop) \
             running in a node/block's own resolved cwd and env, and thin structured wrappers around \
             the full `meshfox node <op>` surface (node_show/find/add/meta/body/block/rm/mv/rename/ \
             set_id/edges/move/reorder). Each edit call is its own immediate read-modify-write of the \
             file on disk — no batching, no conflict detection against a concurrent editor.",
        )
    }
}

// =======================================================================
// Root: what a host actually launches. Owns no canvas file itself — every
// canvas-scoped tool is a proxy to that canvas's own leaf child process.
// =======================================================================

struct CanvasHandle {
    client: RunningService<RoleClient, ()>,
    path: PathBuf,
    last_used: Instant,
}

#[derive(Clone)]
struct MeshfoxMcpRoot {
    root: PathBuf,
    canvases: Arc<Mutex<HashMap<String, Arc<Mutex<CanvasHandle>>>>>,
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

    fn spawn_idle_sweep(&self) {
        let canvases = Arc::clone(&self.canvases);
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(SWEEP_INTERVAL).await;
                let idle_ids: Vec<String> = {
                    let map = canvases.lock().await;
                    let mut idle = Vec::new();
                    for (id, handle) in map.iter() {
                        if handle.lock().await.last_used.elapsed() > IDLE_TIMEOUT {
                            idle.push(id.clone());
                        }
                    }
                    idle
                };
                for id in idle_ids {
                    let removed = canvases.lock().await.remove(&id);
                    if let Some(handle) = removed {
                        let mut guard = handle.lock().await;
                        let _ = guard.client.close_with_timeout(CANVAS_CLOSE_TIMEOUT).await;
                    }
                }
            }
        });
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
    fn resolve_under_root_for_create(&self, requested: &str) -> Result<(PathBuf, String), ErrorData> {
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

    async fn lookup(&self, canvas_id: &str) -> Result<Arc<Mutex<CanvasHandle>>, ErrorData> {
        self.canvases
            .lock()
            .await
            .get(canvas_id)
            .cloned()
            .ok_or_else(|| {
                invalid_params(format!(
                    "no open canvas {canvas_id:?} — call canvas_open first"
                ))
            })
    }

    /// Forwards `inner` to `tool_name` on `canvas_id`'s own child process,
    /// one-to-one — same tool name, same argument shape, minus the
    /// `canvas_id` wrapper this level adds. The child's own success/failure
    /// comes back exactly as it sent it.
    async fn forward(
        &self,
        canvas_id: &str,
        tool_name: &'static str,
        inner: impl Serialize,
    ) -> Result<CallToolResult, ErrorData> {
        let arguments = match serde_json::to_value(inner) {
            Ok(serde_json::Value::Object(map)) => Some(map),
            Ok(serde_json::Value::Null) => None,
            Ok(_) => {
                return Err(ErrorData::internal_error(
                    "internal: forwarded tool arguments must serialize to a JSON object",
                    None,
                ))
            }
            Err(e) => {
                return Err(ErrorData::internal_error(
                    format!("failed to serialize arguments for {tool_name}: {e}"),
                    None,
                ))
            }
        };

        let handle = self.lookup(canvas_id).await?;
        let mut guard = handle.lock().await;
        guard.last_used = Instant::now();
        let mut request = CallToolRequestParams::new(tool_name);
        if let Some(arguments) = arguments {
            request = request.with_arguments(arguments);
        }
        match guard.client.call_tool(request).await {
            Ok(result) => Ok(result),
            // The child's own tool returned `Err(ErrorData)` — propagate
            // its exact code/message rather than wrapping it, so calling a
            // proxied tool reads no differently than calling it directly
            // on a single-canvas leaf server would.
            Err(ServiceError::McpError(e)) => Err(e),
            Err(e) => Err(ErrorData::internal_error(
                format!("canvas {canvas_id:?} ({tool_name}): {e}"),
                None,
            )),
        }
    }
}

// ---------------------------------------------------------------------
// Root tool parameter types
// ---------------------------------------------------------------------

/// Wraps any leaf tool's own parameter type with the `canvas_id` every
/// proxied tool now requires — one generic wrapper instead of a bespoke
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
        description = "Opens a canvas file for editing/debugging, spawning its own isolated process if it isn't already open (a crash or hang on one canvas can't affect another). `path` must resolve under this server's own root directory. Returns a canvas_id — required by every other tool. Opening an already-open file just returns its existing id. Pass `create: true` to create the file first (an empty canvas) if it doesn't exist yet — a no-op if it already does."
    )]
    async fn canvas_open(
        &self,
        Parameters(params): Parameters<CanvasOpenParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let (resolved, canvas_id) = if params.create {
            let (resolved, canvas_id) = self.resolve_under_root_for_create(&params.path)?;
            if !resolved.exists() {
                let content = crate::canvas_template_content(&resolved);
                std::fs::write(&resolved, content).map_err(|e| {
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

        if let Some(handle) = self.canvases.lock().await.get(&canvas_id).cloned() {
            handle.lock().await.last_used = Instant::now();
            return Ok(CallToolResult::structured(
                json!({ "canvas_id": canvas_id, "path": canvas_id }),
            ));
        }

        let exe = std::env::current_exe().map_err(|e| {
            ErrorData::internal_error(format!("failed to locate own executable: {e}"), None)
        })?;
        let command = Command::new(exe).configure(|cmd| {
            cmd.arg("mcp")
                .env(LEAF_ENV_VAR, "1")
                .env(LEAF_PATH_ENV_VAR, &resolved)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped());
        });
        let transport = TokioChildProcess::new(command).map_err(|e| {
            ErrorData::internal_error(
                format!("failed to spawn a process for canvas {canvas_id:?}: {e}"),
                None,
            )
        })?;
        let client = ().serve(transport).await.map_err(|e| {
            ErrorData::internal_error(
                format!("failed to start MCP session for canvas {canvas_id:?}: {e}"),
                None,
            )
        })?;

        let mut canvases = self.canvases.lock().await;
        if let Some(handle) = canvases.get(&canvas_id).cloned() {
            // Lost a race with a concurrent canvas_open for the same file
            // — drop this call's own just-spawned duplicate, keep the
            // winner already in the registry.
            drop(canvases);
            let _ = client.cancel().await;
            handle.lock().await.last_used = Instant::now();
            return Ok(CallToolResult::structured(
                json!({ "canvas_id": canvas_id, "path": canvas_id }),
            ));
        }
        canvases.insert(
            canvas_id.clone(),
            Arc::new(Mutex::new(CanvasHandle {
                client,
                path: resolved,
                last_used: Instant::now(),
            })),
        );
        Ok(CallToolResult::structured(
            json!({ "canvas_id": canvas_id, "path": canvas_id }),
        ))
    }

    #[tool(
        description = "Closes an open canvas, gracefully shutting down its process (any live debug sessions on it end too)."
    )]
    async fn canvas_close(
        &self,
        Parameters(params): Parameters<CanvasIdOnlyParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let handle = self
            .canvases
            .lock()
            .await
            .remove(&params.canvas_id)
            .ok_or_else(|| invalid_params(format!("no open canvas {:?}", params.canvas_id)))?;
        let mut guard = handle.lock().await;
        let _ = guard.client.close_with_timeout(CANVAS_CLOSE_TIMEOUT).await;
        Ok(CallToolResult::structured(
            json!({ "closed": params.canvas_id }),
        ))
    }

    #[tool(description = "Lists every currently open canvas and its id.")]
    async fn canvas_list(
        &self,
        Parameters(_params): Parameters<EmptyParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let canvases = self.canvases.lock().await;
        let mut items = Vec::new();
        for (id, handle) in canvases.iter() {
            let guard = handle.lock().await;
            items.push(json!({
                "canvas_id": id,
                "path": id,
                "resolved_path": guard.path.display().to_string(),
            }));
        }
        Ok(CallToolResult::structured(json!({ "canvases": items })))
    }

    #[tool(
        description = "Same as debug_start, scoped to canvas_id (see canvas_open) — starts a persistent debug shell in that canvas."
    )]
    async fn debug_start(
        &self,
        Parameters(WithCanvas { canvas_id, inner }): Parameters<WithCanvas<DebugStartParams>>,
    ) -> Result<CallToolResult, ErrorData> {
        self.forward(&canvas_id, "debug_start", inner).await
    }

    #[tool(
        description = "Same as debug_send, scoped to canvas_id (see canvas_open) — runs code in that canvas's debug session."
    )]
    async fn debug_send(
        &self,
        Parameters(WithCanvas { canvas_id, inner }): Parameters<WithCanvas<DebugSendParams>>,
    ) -> Result<CallToolResult, ErrorData> {
        self.forward(&canvas_id, "debug_send", inner).await
    }

    #[tool(
        description = "Same as debug_stop, scoped to canvas_id (see canvas_open) — stops a debug session in that canvas."
    )]
    async fn debug_stop(
        &self,
        Parameters(WithCanvas { canvas_id, inner }): Parameters<WithCanvas<DebugStopParams>>,
    ) -> Result<CallToolResult, ErrorData> {
        self.forward(&canvas_id, "debug_stop", inner).await
    }

    #[tool(
        description = "Same as node_show, scoped to canvas_id (see canvas_open) — reads one node's structured metadata in that canvas."
    )]
    async fn node_show(
        &self,
        Parameters(WithCanvas { canvas_id, inner }): Parameters<WithCanvas<NodeIdParams>>,
    ) -> Result<CallToolResult, ErrorData> {
        self.forward(&canvas_id, "node_show", inner).await
    }

    #[tool(
        description = "Same as node_add, scoped to canvas_id (see canvas_open) — adds a new child node in that canvas."
    )]
    async fn node_add(
        &self,
        Parameters(WithCanvas { canvas_id, inner }): Parameters<WithCanvas<NodeAddParams>>,
    ) -> Result<CallToolResult, ErrorData> {
        self.forward(&canvas_id, "node_add", inner).await
    }

    #[tool(
        description = "Same as node_meta, scoped to canvas_id (see canvas_open) — updates a node's position/style/type fields in that canvas."
    )]
    async fn node_meta(
        &self,
        Parameters(WithCanvas { canvas_id, inner }): Parameters<WithCanvas<NodeMetaParams>>,
    ) -> Result<CallToolResult, ErrorData> {
        self.forward(&canvas_id, "node_meta", inner).await
    }

    #[tool(
        description = "Same as node_body, scoped to canvas_id (see canvas_open) — replaces a node's whole Markdown body in that canvas."
    )]
    async fn node_body(
        &self,
        Parameters(WithCanvas { canvas_id, inner }): Parameters<WithCanvas<NodeBodyParams>>,
    ) -> Result<CallToolResult, ErrorData> {
        self.forward(&canvas_id, "node_body", inner).await
    }

    #[tool(
        description = "Same as node_block, scoped to canvas_id (see canvas_open) — rewrites one runnable fence's attributes/code in that canvas."
    )]
    async fn node_block(
        &self,
        Parameters(WithCanvas { canvas_id, inner }): Parameters<WithCanvas<NodeBlockParams>>,
    ) -> Result<CallToolResult, ErrorData> {
        self.forward(&canvas_id, "node_block", inner).await
    }

    #[tool(
        description = "Same as node_rm, scoped to canvas_id (see canvas_open) — deletes a node in that canvas."
    )]
    async fn node_rm(
        &self,
        Parameters(WithCanvas { canvas_id, inner }): Parameters<WithCanvas<NodeRmParams>>,
    ) -> Result<CallToolResult, ErrorData> {
        self.forward(&canvas_id, "node_rm", inner).await
    }

    #[tool(
        description = "Same as node_mv, scoped to canvas_id (see canvas_open) — moves a node to a new structural parent in that canvas."
    )]
    async fn node_mv(
        &self,
        Parameters(WithCanvas { canvas_id, inner }): Parameters<WithCanvas<NodeMvParams>>,
    ) -> Result<CallToolResult, ErrorData> {
        self.forward(&canvas_id, "node_mv", inner).await
    }

    #[tool(
        description = "Same as node_rename, scoped to canvas_id (see canvas_open) — renames a node's heading text in that canvas."
    )]
    async fn node_rename(
        &self,
        Parameters(WithCanvas { canvas_id, inner }): Parameters<WithCanvas<NodeRenameParams>>,
    ) -> Result<CallToolResult, ErrorData> {
        self.forward(&canvas_id, "node_rename", inner).await
    }

    #[tool(
        description = "Same as node_set_id, scoped to canvas_id (see canvas_open) — changes a node's id in that canvas."
    )]
    async fn node_set_id(
        &self,
        Parameters(WithCanvas { canvas_id, inner }): Parameters<WithCanvas<NodeSetIdParams>>,
    ) -> Result<CallToolResult, ErrorData> {
        self.forward(&canvas_id, "node_set_id", inner).await
    }

    #[tool(
        description = "Same as node_edges, scoped to canvas_id (see canvas_open) — replaces a node's extra incoming edges in that canvas."
    )]
    async fn node_edges(
        &self,
        Parameters(WithCanvas { canvas_id, inner }): Parameters<WithCanvas<NodeEdgesParams>>,
    ) -> Result<CallToolResult, ErrorData> {
        self.forward(&canvas_id, "node_edges", inner).await
    }

    #[tool(
        description = "Same as node_move, scoped to canvas_id (see canvas_open) — reorders a node among its siblings in that canvas."
    )]
    async fn node_move(
        &self,
        Parameters(WithCanvas { canvas_id, inner }): Parameters<WithCanvas<NodeMoveParams>>,
    ) -> Result<CallToolResult, ErrorData> {
        self.forward(&canvas_id, "node_move", inner).await
    }

    #[tool(
        description = "Same as node_reorder, scoped to canvas_id (see canvas_open) — resyncs sibling order to canvas layout in that canvas."
    )]
    async fn node_reorder(
        &self,
        Parameters(WithCanvas { canvas_id, inner }): Parameters<WithCanvas<NodeReorderParams>>,
    ) -> Result<CallToolResult, ErrorData> {
        self.forward(&canvas_id, "node_reorder", inner).await
    }

    #[tool(
        description = "Same as node_find, scoped to canvas_id (see canvas_open) — finds nodes matching a CSS selector in that canvas."
    )]
    async fn node_find(
        &self,
        Parameters(WithCanvas { canvas_id, inner }): Parameters<WithCanvas<NodeFindParams>>,
    ) -> Result<CallToolResult, ErrorData> {
        self.forward(&canvas_id, "node_find", inner).await
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for MeshfoxMcpRoot {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build()).with_instructions(
            "Multi-canvas meshfox MCP server. Every canvas-scoped tool requires a canvas_id from \
             canvas_open first — there is no implicit 'current' canvas. canvas_open/canvas_close/ \
             canvas_list manage a registry of canvases, each backed by its own spawned, isolated \
             process (one file, one process — a crash or a hung debug session on one canvas can't \
             affect another). canvas_open only resolves paths under this server's own root directory \
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
    use super::{node_json, MeshfoxMcpRoot};
    use meshfox_core::Canvas;
    use std::sync::atomic::{AtomicU64, Ordering};

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
