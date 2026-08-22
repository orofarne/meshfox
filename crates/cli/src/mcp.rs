//! `meshfox mcp <path>` — an MCP stdio server bound to one canvas file (see
//! `Command::Mcp`'s own doc comment). Two tool groups:
//!
//! - **Debug session** (`debug_start`/`debug_send`/`debug_stop`): a
//!   persistent `bash` kept alive in a node/block's own resolved cwd/env,
//!   so state between calls (exported vars, files a snippet wrote) survives
//!   the way a one-shot `meshfox run` never could. Always plain `bash`,
//!   never a block's own `interpreter=` — a session is inherently about
//!   running many different ad-hoc snippets over time, not repeatedly
//!   driving one fixed non-shell REPL.
//! - **Editing**: `node_show`/`node_find` as structured JSON, plus thin
//!   wrappers around the same pure `apply_node_*` functions `node <op>`
//!   already uses — the full `node <op>` surface (`add`/`rm`/`mv`/
//!   `rename`/`set_id`/`body`/`block`/`meta`/`edges`/`move`/`reorder`/
//!   `show`/`find`) is mirrored here one tool per subcommand, so an agent
//!   never has to fall back to hand-editing or shelling out to the CLI for
//!   something the MCP surface is missing. Each call is its own immediate
//!   read-modify-write, same as the CLI. Deliberately no batch/transactional
//!   multi-edit and no optimistic-concurrency write conflict detection (see
//!   TODO.canvas.md's own still-open design notes on both) — not attempted
//!   here.
//!
//! Concurrency: each debug session serializes its own `debug_send` calls
//! (a `Mutex` per session) but different sessions run independently. An
//! idle session (no `debug_send` for `IDLE_TIMEOUT`) is killed and dropped
//! by a periodic sweep, in case an agent forgets to call `debug_stop`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};

use meshfox_core::Canvas;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ServerCapabilities, ServerInfo};
use rmcp::{tool, tool_handler, tool_router, ErrorData, ServerHandler, ServiceExt};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{mpsc, Mutex};

const IDLE_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const SWEEP_INTERVAL: Duration = Duration::from_secs(5 * 60);
const DEFAULT_SEND_TIMEOUT_MS: u64 = 60_000;

pub async fn run(canvas_path: PathBuf) -> Result<(), String> {
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

fn invalid_params(msg: impl Into<String>) -> ErrorData {
    ErrorData::invalid_params(msg.into(), None)
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
// Tool parameter types
// ---------------------------------------------------------------------

#[derive(Deserialize, JsonSchema)]
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

#[derive(Deserialize, JsonSchema)]
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

#[derive(Deserialize, JsonSchema)]
struct DebugStopParams {
    session_id: String,
}

#[derive(Deserialize, JsonSchema)]
struct NodeIdParams {
    node_id: String,
}

#[derive(Deserialize, JsonSchema, Default)]
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

#[derive(Deserialize, JsonSchema)]
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

#[derive(Deserialize, JsonSchema)]
struct NodeMetaParams {
    node_id: String,
    /// Clears x/y/width/height back to unset. Mutually exclusive with
    /// passing any of them in `fields`.
    #[serde(default)]
    clear_position: bool,
    #[serde(flatten)]
    fields: McpNodeFields,
}

#[derive(Deserialize, JsonSchema)]
struct NodeBodyParams {
    node_id: String,
    body: String,
}

#[derive(Deserialize, JsonSchema)]
struct NodeRmParams {
    node_id: String,
    /// Promote direct children to this node's own parent instead of
    /// deleting them too.
    #[serde(default)]
    keep_children: bool,
}

#[derive(Deserialize, JsonSchema)]
struct NodeMvParams {
    node_id: String,
    new_parent_id: String,
}

#[derive(Deserialize, JsonSchema, Default)]
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

#[derive(Deserialize, JsonSchema)]
struct NodeRenameParams {
    node_id: String,
    /// New heading text. The node's id, heading level, and body are left
    /// untouched — an id is pinned the first time it's written and never
    /// follows later title edits.
    title: String,
}

#[derive(Deserialize, JsonSchema)]
struct NodeSetIdParams {
    node_id: String,
    /// The new stable id. Every `parent=`/`meshfox:edge from=` reference to
    /// the old id is rewritten exactly; `deps=` references are rewritten
    /// best-effort (plain text, not parser-validated).
    new_id: String,
}

#[derive(Deserialize, JsonSchema, Default)]
struct NodeEdgesParams {
    node_id: String,
    /// Full replacement list of extra-parent ids (`meshfox:edge from="..."`
    /// lines) — replaces whatever was already there, doesn't add to it.
    /// Pass an empty list to clear them all.
    #[serde(default)]
    from: Vec<String>,
}

#[derive(Deserialize, JsonSchema)]
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

#[derive(Deserialize, JsonSchema, Default)]
struct NodeReorderParams {}

#[derive(Deserialize, JsonSchema)]
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
}

fn bool_pair(v: Option<bool>) -> (bool, bool) {
    match v {
        Some(true) => (true, false),
        Some(false) => (false, true),
        None => (false, false),
    }
}

// ---------------------------------------------------------------------
// Tools
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
        description = "Reads one node's structured metadata — id, title, type, parent, children, position, color, tags, and, for a file/link node, target/display."
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
        Ok(CallToolResult::structured(node_json(&canvas, node)))
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
                .map(|n| node_json(&canvas, n))
                .collect();
            result["nodes"] = json!(nodes);
        }
        Ok(CallToolResult::structured(result))
    }
}

fn node_json(canvas: &Canvas, node: &meshfox_core::Node) -> serde_json::Value {
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
    json!({
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
    })
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
