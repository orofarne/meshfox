<!-- meshfox:canvas -->
# Architecture
<!-- meshfox:node id="architecture" -->

Part of the [meshfox documentation](./README.md).

Rust workspace + a small browser frontend:

```
crates/
  core/     canvas model, .canvas.md parsing/rendering (crate::mdcanvas),
            tree derivation, Markdown fence scanning, output-block
            rewriting, executors (bash, ...), auto-layout
  server/   library crate: axum HTTP backend (load/save a canvas file,
            execute a block) + the built web/ UI, embedded at compile time
            via rust-embed. No [[bin]] of its own — meshfox-cli links it.
  cli/      the only binary: `meshfox`. `run` uses crates/core
            directly; `view` starts crates/server's backend (with the UI
            baked in) on localhost, read-only until the browser's "Edit"
            button is clicked. One executable, no separate server process
            to install or start.
web/        React + React Flow editor: renders the node/edge graph,
            in-node Markdown editing, run buttons wired to the server API.
            Built once (`npm run build`) and embedded into the `meshfox`
            binary — not shipped or loaded separately at runtime.
```

`core` is the shared brain; `server` wraps it in an HTTP API (and owns the embedded UI); `cli` is the single front door a user actually runs. Every write to the source file — cached output, saved layout — goes through a surgical patch (`mdcanvas::set_node_body` / `set_node_meta`) that touches only the node(s) that actually changed, never a full-document reformat.

## Component diagram
<!-- meshfox:node id="component-diagram" type="group" -->

### Browser UI
<!-- meshfox:node id="browser-ui" x=0 y=0 w=380 h=210 -->

`web/`: React + React Flow SPA. Talks to the worker over `GET /api/canvas`, `PATCH /api/nodes/:id`, block-run/`tty` WebSockets. Built once (`npm run build`) into `web/dist`.

### TUI
<!-- meshfox:node id="tui" x=430 y=0 w=380 h=230 -->

`crates/cli/src/tui/`: `meshfox tui`, a real terminal frontend (ratatui). Never touches the canvas file itself — every read/mutation goes through `worker_client` to whichever worker owns the file.

### MCP root
<!-- meshfox:node id="mcp-root" x=860 y=0 w=380 h=210 -->

`meshfox mcp` — the one server a host (Claude Code, ...) launches, one process. It keeps a registry of open canvases; each entry owns only its debug shells and the port of the worker it last talked to. Every tool call is a thin client of the file's own worker (`worker_client`/`coordinator`) and has a deadline; a hung worker is reported by `canvas_list`, never killed. Every HTTP call `worker_client` makes (the CLI's `node …` commands, the TUI and this server alike) has a time limit — 30 s for ordinary calls, 60 s for whole-file writes and history jumps, 20 s for immediate kill operations, a service’s configured shutdown budget (up to one hour plus a margin) for Stop/Restart, a debug command's own timeout + 15 s, and 5 s to connect — so a hung worker ends a command or a TUI action with an error saying so instead of hanging it. WebSocket streams (runs, `tty`, watching) are not covered.

### CLI one-shot ops
<!-- meshfox:node id="cli-one-shot-ops" x=1290 y=0 w=380 h=210 -->

`meshfox node <op>` / `run` / `validate` / `check`: one-shot invocations from a shell. Node mutations always route through a worker too (no more direct-file-write fallback).

### coordinator::resolve
<!-- meshfox:node id="coordinator-resolve" x=430 y=585 w=380 h=280 -->
<!-- meshfox:edge from="tui" -->
<!-- meshfox:edge from="cli-one-shot-ops" -->
<!-- meshfox:edge from="mcp-root" -->

`crates/cli/src/coordinator.rs`: the one function every frontend calls to decide "who is *the* worker for this file" — tried in order, a configured `server_socket` daemon, then the per-file `worker_lock`. `get_or_spawn` embeds a new worker (in-process, `tokio::spawn`) when this call wins the lock.

### worker_lock
<!-- meshfox:node id="worker-lock" x=900 y=585 w=380 h=230 -->
<!-- meshfox:edge from="coordinator-resolve" -->

`meshfox-core::worker_lock`: one `flock`'d lock file per canvas path, holding the winning worker's bound port. A second `coordinator::resolve` for the same file reads that port instead of racing to also serve it.

### worker (axum HTTP server)
<!-- meshfox:node id="worker-axum-http-server" x=430 y=950 w=380 h=255 -->
<!-- meshfox:edge from="coordinator-resolve" -->
<!-- meshfox:edge from="browser-ui" -->
<!-- meshfox:edge from="tui" -->
<!-- meshfox:edge from="web-dist-bundle" -->
<!-- meshfox:edge from="mcp-root" -->

`meshfox_server::serve_as_worker`: the one process (embedded in `cli`, `tui`, or `view`) that actually owns one canvas file — every read/mutation for that file funnels through here, serialized, closing the read-modify-write race a direct file edit would have.

### web/dist bundle
<!-- meshfox:node id="web-dist-bundle" x=1290 y=950 w=380 h=210 -->

Built React app, embedded into the `meshfox` binary at compile time via `rust-embed` (`meshfox_server::WebAssets`). Not shipped or loaded separately at runtime.

### core (meshfox-core)
<!-- meshfox:node id="core-meshfox-core" x=215 y=1290 w=380 h=210 -->
<!-- meshfox:edge from="worker-axum-http-server" -->
<!-- meshfox:edge from="cli-one-shot-ops" -->

Canvas model: `mdcanvas` parse/surgical-patch, tree derivation, fence scanning, output-block rewriting, auto-layout. The shared brain — no I/O of its own, no `[[bin]]`.

### stream_exec / debug_session
<!-- meshfox:node id="stream-exec-debug-session" x=860 y=1290 w=380 h=210 -->
<!-- meshfox:edge from="worker-axum-http-server" -->

`meshfox_server::stream_exec`/`debug_session`/`pty_exec`: spawns the real bash/interpreter subprocess for a runnable block, `tty` block, or MCP `debug_*` shell.

### canvas file (*.canvas.md)
<!-- meshfox:node id="canvas-file-canvas-md" x=430 y=1580 w=380 h=210 -->
<!-- meshfox:edge from="worker-axum-http-server" -->

The actual file on disk — a Markdown outline with `meshfox:*` HTML-comment bookkeeping. Read/written only by the worker that holds its `worker_lock`.
