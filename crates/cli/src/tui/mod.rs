//! `meshfox tui`: an ncurses-style terminal viewer for a canvas — browse
//! the node tree, read a node's rendered Markdown body (syntax-highlighted
//! code, local images), and run blocks with live streamed output, same
//! deps-chain/cache/`meshfox:var` handling as `meshfox run`/`meshfox view`.
//! A `tty` block hands the real terminal over to its process, same as
//! `meshfox run` does (see `run_tty_handoff` below) — no in-app terminal
//! emulator. `e` opens a fullscreen raw-source editor on the selected
//! node's own file (`source_editor`) — the terminal counterpart to the
//! web UI's Source mode; still no *structural* editing (that's `meshfox
//! node ...`/the browser UI's Edit mode's dedicated node operations).

mod app;
mod markdown;
mod source_editor;
mod theme;
mod tree;
// `pub(crate)` (not the default private) so `syntax_registry.rs`'s own
// tests can reference `ui::SOURCE_EDITOR_THEME` directly rather than
// hardcoding a second copy of it that could silently drift out of sync.
pub(crate) mod ui;

use std::collections::HashMap;
use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crossterm::event::{DisableMouseCapture, EnableMouseCapture, Event, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

use app::{App, LinkPreviewMsg};
use meshfox_core::deps::BlockAddr;

pub async fn run(canvas_path: PathBuf, initial_node: Option<String>) -> io::Result<()> {
    // Raw mode + the alternate screen go up *before* `App::new` — it calls
    // `Picker::from_query_stdio()` (see `app::App::new`), which queries the
    // terminal for its graphics-protocol support by writing an escape
    // sequence and reading the reply directly off stdin. That only works
    // reliably once the terminal is already in raw mode (no line buffering,
    // no local echo racing the reply) — querying first and only enabling
    // raw mode afterward leaves the query's own answer sitting in a
    // line-buffered read that nothing then consumes correctly, and (worse)
    // leaves the terminal in whatever half-toggled state the query left it,
    // which is what silently ate every keypress before this fix.
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let (link_preview_tx, mut link_preview_rx) =
        tokio::sync::mpsc::unbounded_channel::<LinkPreviewMsg>();

    // TUI is always a client of *some* worker for this file from here on —
    // its own embedded one if nobody else's is running, someone else's
    // otherwise (a local sibling `view`/`tui` session, or an externally-
    // configured coordinator — see `crate::coordinator`'s own doc comment
    // for why both feed the same decision) — never both parsing/writing the
    // file directly the way it used to. `coordinator::resolve` is called
    // directly here (rather than letting `meshfox_server::run` do its own
    // `worker_lock` internally, as `view_worker` does) specifically so this
    // can learn the resolved port *synchronously*, needed before any of
    // `App`'s own HTTP calls can work — see `meshfox_server::serve_as_worker`'s
    // own doc comment for why that's a separate, lower-level entry point
    // from `run` for exactly this reason. A resolve failure (a configured
    // `server_socket` that's unreachable, or the local lock file itself
    // couldn't be read) is a real, printed error, not a silent degrade to
    // today's direct-file/local-process behavior — matching every other
    // core-launch operation's own "no silent fallback" posture.
    let worker_port = match crate::coordinator::resolve(&canvas_path).await {
        Ok(crate::coordinator::Resolved::Us(guard)) => {
            let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
            // `auto_exit: false` matters here specifically — with `true`, a
            // browser tab that peeked at this worker and then closed would
            // eventually call `std::process::exit(0)` (`TabGuard`), killing
            // this whole TUI session, not just the embedded worker.
            // `quiet: true` — a stray write to the shared stdout this
            // process's own alt-screen rendering owns would corrupt it.
            tokio::spawn(meshfox_server::serve_as_worker(
                canvas_path.clone(),
                0,
                false,
                None,
                true,
                Some(guard),
                Some(ready_tx),
            ));
            // The embedded bind itself failing (distinct from `resolve`'s
            // own error above) still degrades gracefully — `ready_rx`
            // simply never fires, and `App::new` already has its own
            // direct-file/local-process fallback for `worker_port: None`.
            ready_rx.await.ok()
        }
        Ok(crate::coordinator::Resolved::Other(port)) => Some(port),
        Err(e) => {
            disable_raw_mode()?;
            execute!(terminal.backend_mut(), LeaveAlternateScreen, DisableMouseCapture)?;
            eprintln!("meshfox tui: couldn't reach a worker for this canvas: {e}");
            std::process::exit(1);
        }
    };

    let result = match App::new(canvas_path, link_preview_tx, initial_node.as_deref(), worker_port).await {
        Ok(mut app) => {
            // `crossterm::event::read()` is blocking, so reading happens on
            // its own OS thread — the main loop stays async and can
            // select! between keyboard/mouse input and a running block's
            // streamed output (see `app::App::on_output_line`). `paused`
            // is what makes a `tty` handoff (see `run_tty_handoff`) safe:
            // while it's set, this thread only ever calls the
            // non-consuming `poll()`, never `read()`, so it can't steal a
            // byte of input out from under the child process that's about
            // to inherit the real terminal — a blocked `read()` call can't
            // be cancelled once started, so staying out of it entirely
            // during a handoff is the only reliable way to avoid the race.
            let (input_tx, mut input_rx) = tokio::sync::mpsc::unbounded_channel::<Event>();
            let paused = Arc::new(AtomicBool::new(false));
            let reader_paused = Arc::clone(&paused);
            std::thread::spawn(move || loop {
                if reader_paused.load(Ordering::Acquire) {
                    std::thread::sleep(Duration::from_millis(50));
                    continue;
                }
                match crossterm::event::poll(Duration::from_millis(50)) {
                    Ok(true) => match crossterm::event::read() {
                        Ok(ev) => {
                            if input_tx.send(ev).is_err() {
                                break;
                            }
                        }
                        Err(_) => break,
                    },
                    Ok(false) => continue,
                    Err(_) => break,
                }
            });

            let (reload_tx, mut reload_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
            let (external_run_tx, mut external_run_rx) =
                tokio::sync::mpsc::unbounded_channel::<ExternalRunUpdate>();
            // `app.worker_port` (not the outer `worker_port` this function
            // resolved before constructing `App`) is the authoritative
            // answer — `App::new`'s own canvas-load fallback can still turn
            // a resolved port back into `None` if the very first HTTP call
            // against it failed (see its own doc comment), and that's
            // exactly the case this should also fall back to mtime-polling
            // for. No worker-reachable case for `external_run_tx` at all —
            // a passive "watch a run I didn't start" only makes sense
            // against a shared worker; fallback (no-worker) mode has no
            // such thing to discover, so `external_run_rx` just never
            // receives anything then.
            match app.worker_port {
                Some(port) => spawn_worker_watcher(port, Arc::clone(&app.known_raw), reload_tx, external_run_tx),
                None => spawn_file_watcher(app.canvas_path.clone(), Arc::clone(&app.known_raw), reload_tx),
            }

            main_loop(
                &mut terminal,
                &mut app,
                &mut input_rx,
                &paused,
                &mut reload_rx,
                &mut link_preview_rx,
                &mut external_run_rx,
            )
            .await
        }
        Err(e) => Err(e),
    };

    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    result
}

/// Polls `canvas_path`'s mtime every 500ms on its own OS thread — same
/// cadence and same "diff the actual content, not just the mtime" trick as
/// the web server's own `spawn_file_watcher`
/// (`crates/server/src/lib.rs`) — and pushes the new content through
/// `reload_tx` whenever it differs from `known_raw`, which it also
/// updates so it doesn't re-report a change this process just wrote
/// itself (see `App::known_raw`'s doc comment for who else writes to it).
fn spawn_file_watcher(
    canvas_path: PathBuf,
    known_raw: Arc<std::sync::Mutex<String>>,
    reload_tx: tokio::sync::mpsc::UnboundedSender<String>,
) {
    std::thread::spawn(move || {
        let mut last_mtime = std::fs::metadata(&canvas_path)
            .and_then(|m| m.modified())
            .ok();
        loop {
            std::thread::sleep(Duration::from_millis(500));
            let Ok(meta) = std::fs::metadata(&canvas_path) else {
                continue;
            };
            let Ok(mtime) = meta.modified() else { continue };
            if Some(mtime) == last_mtime {
                continue;
            }
            last_mtime = Some(mtime);
            let Ok(contents) = std::fs::read_to_string(&canvas_path) else {
                continue;
            };
            let mut raw = known_raw.lock().unwrap();
            if *raw != contents {
                *raw = contents.clone();
                drop(raw);
                if reload_tx.send(contents).is_err() {
                    return;
                }
            }
        }
    });
}

/// The worker-routed equivalent of `spawn_file_watcher` — consumes
/// `worker_client::watch`'s change notifications instead of polling
/// `canvas_path`'s own mtime, re-fetching `GET /api/canvas/raw` on each one
/// and pushing it through `reload_tx` the same "diff against `known_raw`
/// first" way `spawn_file_watcher` already does (so this process's own
/// writes, echoed back as a notification, don't re-trigger a reload of
/// what's already on screen). Also reacts to `WatchEvent::RunStarted` —
/// see `spawn_run_subscriber`.
fn spawn_worker_watcher(
    port: u16,
    known_raw: Arc<std::sync::Mutex<String>>,
    reload_tx: tokio::sync::mpsc::UnboundedSender<String>,
    external_run_tx: tokio::sync::mpsc::UnboundedSender<ExternalRunUpdate>,
) {
    tokio::spawn(async move {
        use crate::worker_client::WatchEvent;
        let mut changes = crate::worker_client::watch(port);
        while let Some(event) = changes.recv().await {
            match event {
                WatchEvent::Changed => {
                    let Ok(contents) = crate::worker_client::get_canvas_raw(port).await else {
                        continue;
                    };
                    let mut raw = known_raw.lock().unwrap();
                    if *raw != contents {
                        *raw = contents.clone();
                        drop(raw);
                        if reload_tx.send(contents).is_err() {
                            return;
                        }
                    }
                }
                WatchEvent::RunStarted { node_id, block } => {
                    spawn_run_subscriber(port, node_id, block, external_run_tx.clone());
                }
            }
        }
    });
}

/// One incremental update for a run this TUI session never itself started
/// — see `App::on_external_run_event`, which this feeds.
struct ExternalRunUpdate {
    node_id: String,
    block: String,
    event: crate::worker_client::SubscribeEvent,
}

/// Reacts to one `WatchEvent::RunStarted` by opening a passive
/// `worker_client::subscribe_run` connection for that exact address and
/// forwarding every event it yields through `tx` — the TUI counterpart to
/// the web UI's own `watchAutorunBlock`. A short-lived task per run
/// (`subscribe_run`'s own channel closes once the run's terminal event
/// arrives, or immediately if the address turns out not to exist — a
/// harmless no-op either way, same as the web UI's own best-effort
/// `.catch()` on this call), not a long-lived one — nothing here needs
/// deduping against an already-in-flight subscription for the same address
/// (an unlikely double `RunStarted` just means two connections briefly
/// agreeing on the same data).
fn spawn_run_subscriber(
    port: u16,
    node_id: String,
    block: String,
    tx: tokio::sync::mpsc::UnboundedSender<ExternalRunUpdate>,
) {
    tokio::spawn(async move {
        let mut events = crate::worker_client::subscribe_run(port, node_id.clone(), block.clone());
        while let Some(event) = events.recv().await {
            if tx
                .send(ExternalRunUpdate { node_id: node_id.clone(), block: block.clone(), event })
                .is_err()
            {
                return;
            }
        }
    });
}

/// What `main_loop`'s combined `RunState`-polling `select!` arm actually
/// got — `RunState::proc`'s local-mode output line, or
/// `RunState::http_rx`'s worker-mode `RunEvent`. See that arm's own
/// comment for why the two are folded into one future rather than two.
enum RunPollOutcome {
    Local(Option<(meshfox_server::stream_exec::OutputStream, String)>),
    Http(Option<crate::worker_client::RunEvent>),
}

async fn main_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut App,
    input_rx: &mut tokio::sync::mpsc::UnboundedReceiver<Event>,
    input_paused: &Arc<AtomicBool>,
    reload_rx: &mut tokio::sync::mpsc::UnboundedReceiver<String>,
    link_preview_rx: &mut tokio::sync::mpsc::UnboundedReceiver<LinkPreviewMsg>,
    external_run_rx: &mut tokio::sync::mpsc::UnboundedReceiver<ExternalRunUpdate>,
) -> io::Result<()> {
    loop {
        terminal.draw(|f| ui::render(f, app))?;
        if app.should_quit {
            return Ok(());
        }

        if let Some(pending) = app.pending_tty.take() {
            let exit_code = run_tty_handoff(
                terminal,
                input_paused,
                input_rx,
                &pending.block_name,
                &pending.code,
                pending.interpreter.as_deref(),
                &pending.env,
                &pending.cwd,
                &pending.canvas_path,
                pending.autoclose,
            )
            .await?;
            app.resume_after_tty(exit_code).await;
            continue;
        }

        if let Some(pending) = app.pending_http_tty.take() {
            let exit_code = run_http_tty_handoff(
                terminal,
                input_paused,
                input_rx,
                pending.socket,
                &pending.block_name,
                pending.autoclose,
            )
            .await?;
            app.resume_after_http_tty(exit_code).await;
            continue;
        }

        if let Some(pending) = app.pending_http_tty_attach.take() {
            run_http_tty_attach_handoff(
                terminal,
                input_paused,
                input_rx,
                pending.socket,
                &pending.block_name,
            )
            .await?;
            continue;
        }

        if let Some(pending) = app.pending_child_canvas.take() {
            run_child_canvas_handoff(
                terminal,
                input_paused,
                input_rx,
                &pending.path,
                pending.node.as_deref(),
            )
            .await?;
            continue;
        }

        let has_proc = app.run.as_ref().is_some_and(|r| r.proc.is_some());
        let has_http_run = app.run.as_ref().is_some_and(|r| r.http_rx.is_some());
        let has_file_proc = app.file_run.as_ref().is_some_and(|r| r.proc.is_some());
        // Not draining any output here — each `ServiceHandle`'s own
        // background task (`meshfox_server::services`) already keeps its
        // `status()`/`log_snapshot()` live on its own; this just redraws
        // the tree glyph/footer aggregate periodically while at least one
        // service exists, gated the same way `has_proc`/`has_file_proc`
        // are so it's a true no-op (no wakeups at all) once `services` is
        // empty. **Experimental**, see SPEC.md's "Service blocks
        // (experimental)".
        let has_services = !app.services.is_empty();
        // The worker-routed equivalent of `has_services`'s tick — polls
        // `GET /api/services` (`App::refresh_services`) on the same ~3s
        // cadence the web UI's own service panel already uses, unconditionally
        // whenever a worker is reachable (unlike `has_services`, there's no
        // cheap local check to gate this on — the whole point is finding out
        // about services this process never itself spawned).
        let worker_reachable = app.worker_port.is_some();
        // While the services view is actually open in worker mode, also
        // keep the selected entry's own log fresh — see `service_log`'s own
        // doc comment for why this can't just be read at render time.
        let services_view_open_on_worker = worker_reachable && app.services_view.is_some();
        // Only scheduled while the console is actually expanded — once
        // `console_tick` re-collapses it (or it was never expanded this
        // session), this branch simply isn't in the `select!` at all, same
        // "true no-op, no wakeups" reasoning `has_services` above already
        // has for its own tick.
        let console_pending_collapse = app.console_pending_collapse();
        // Keeps `ui::render_tree`'s running-spinner badge animating at a
        // steady rate regardless of whatever else is (or isn't) causing a
        // redraw — see `App::spinner_tick`'s own doc comment for why this
        // exists at all (a redraw's own real-world timing is too irregular
        // to derive a smooth frame from directly). 120ms keeps the
        // 10-frame cycle a little over a second per rotation, and is cheap
        // enough to run for however long a run takes.
        let spinner_active = app.spinner_active();
        tokio::select! {
            maybe_ev = input_rx.recv() => {
                match maybe_ev {
                    Some(Event::Key(key)) if key.kind == KeyEventKind::Press => app.on_key(key).await,
                    Some(Event::Mouse(mouse)) => app.on_mouse(mouse).await,
                    Some(_) => {}
                    None => return Ok(()),
                }
            }
            outcome = async {
                // `proc`/`http_rx` are mutually exclusive on a given
                // `RunState` (see its own doc comment) — both arms borrow
                // `app.run` mutably, so they're combined into one future
                // rather than two separate `select!` branches (which
                // `tokio::select!` would otherwise construct at once, each
                // borrowing `app.run` for itself, even though only one is
                // ever actually polled).
                let run = app.run.as_mut().unwrap();
                if let Some(proc) = run.proc.as_mut() {
                    RunPollOutcome::Local(proc.output_rx.recv().await)
                } else {
                    RunPollOutcome::Http(run.http_rx.as_mut().unwrap().recv().await)
                }
            }, if has_proc || has_http_run => {
                match outcome {
                    RunPollOutcome::Local(line) => app.on_output_line(line).await,
                    RunPollOutcome::Http(event) => app.on_run_event(event).await,
                }
            }
            line = async {
                app.file_run.as_mut().unwrap().proc.as_mut().unwrap().output_rx.recv().await
            }, if has_file_proc => {
                app.on_file_output_line(line).await;
            }
            Some(content) = reload_rx.recv() => {
                app.on_external_change(content);
            }
            Some(msg) = link_preview_rx.recv() => {
                app.on_link_preview_msg(msg);
            }
            Some(update) = external_run_rx.recv() => {
                app.on_external_run_event(
                    BlockAddr::new(update.node_id, update.block),
                    update.event,
                );
            }
            _ = tokio::time::sleep(std::time::Duration::from_millis(300)), if has_services => {
                app.tick_services();
            }
            _ = tokio::time::sleep(std::time::Duration::from_secs(3)), if worker_reachable => {
                app.refresh_services().await;
            }
            _ = tokio::time::sleep(std::time::Duration::from_secs(1)), if services_view_open_on_worker => {
                if let Some(view) = &app.services_view {
                    let keys = app.sorted_service_keys();
                    if let Some((node_id, block)) = keys.get(view.selected.min(keys.len().saturating_sub(1))).cloned() {
                        app.refresh_service_log(&node_id, &block).await;
                    }
                }
            }
            _ = tokio::time::sleep(std::time::Duration::from_millis(500)), if console_pending_collapse => {
                app.console_tick();
            }
            _ = tokio::time::sleep(std::time::Duration::from_millis(120)), if spinner_active => {
                app.advance_spinner();
            }
        }
    }
}

/// Leaves the TUI's screen entirely, runs a synchronous child `meshfox tui
/// <path>` (`--node <id>` too, for a deep link — see `Command::Tui`'s own
/// `node` field) with its stdin/stdout/stderr inherited from the real
/// terminal, and comes back once it exits — the terminal counterpart to
/// the web UI's "↗ open" button navigating to another canvas (see
/// `crates/server/src/lib.rs`'s `open_node_file`), but without that one's
/// registry/daemon machinery: a nested TUI is a plain foreground child,
/// same as a `tty` block, so there's no port to hand back and nothing to
/// keep alive after it exits. Same leave/restore-screen shape as
/// `run_tty_handoff` right below, for the same reason (`input_paused`
/// keeps the background reader thread off the fd the child now owns).
async fn run_child_canvas_handoff(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    input_paused: &Arc<AtomicBool>,
    input_rx: &mut tokio::sync::mpsc::UnboundedReceiver<Event>,
    path: &std::path::Path,
    node: Option<&str>,
) -> io::Result<()> {
    input_paused.store(true, Ordering::Release);
    tokio::time::sleep(Duration::from_millis(80)).await;

    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;

    let outcome = async {
        let exe = std::env::current_exe()?;
        let mut cmd = tokio::process::Command::new(&exe);
        cmd.arg("tui").arg(path);
        if let Some(node) = node {
            cmd.arg("--node").arg(node);
        }
        cmd.stdin(std::process::Stdio::inherit())
            .stdout(std::process::Stdio::inherit())
            .stderr(std::process::Stdio::inherit())
            .status()
            .await
    }
    .await;

    if let Err(e) = outcome {
        println!(
            "\r\nfailed to open {}: {e}\r\n(press any key to return to the canvas)",
            path.display()
        );
        input_paused.store(false, Ordering::Release);
        let _ = input_rx.recv().await;
        input_paused.store(true, Ordering::Release);
    }

    enable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        EnterAlternateScreen,
        EnableMouseCapture
    )?;
    terminal.clear()?;

    input_paused.store(false, Ordering::Release);
    Ok(())
}

/// Leaves the TUI's screen entirely, runs `code` with its stdin/stdout/
/// stderr connected directly to the real terminal (`Stdio::inherit()`, no
/// pty of our own — same as `meshfox run`'s own `tty` handling in
/// `crates/cli/src/main.rs`), and comes back once it exits. `input_paused`
/// is set for the duration so the background input-reader thread (see
/// `run` above) isn't calling `read()` on the same fd the child now owns.
#[allow(clippy::too_many_arguments)]
async fn run_tty_handoff(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    input_paused: &Arc<AtomicBool>,
    input_rx: &mut tokio::sync::mpsc::UnboundedReceiver<Event>,
    block_name: &str,
    code: &str,
    interpreter: Option<&str>,
    env: &HashMap<String, String>,
    cwd: &std::path::Path,
    canvas_path: &std::path::Path,
    autoclose: bool,
) -> io::Result<i32> {
    input_paused.store(true, Ordering::Release);
    // Comfortably longer than the reader thread's own 50ms poll timeout,
    // so it's guaranteed to have observed the flag and gone quiet before
    // the terminal is actually handed to the child below.
    tokio::time::sleep(Duration::from_millis(80)).await;

    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    println!("==> {block_name}");

    let exit_code = run_tty_block(code, interpreter, env, cwd, canvas_path).await;

    // Without `autoclose`, the canvas doesn't come back on its own — the
    // exit code (and whatever the process last printed, still on screen
    // right above this) stays visible until a deliberate keypress, same
    // as leaving a real shell open after a command finishes. `autoclose`
    // skips straight to restoring the canvas, the only behavior this block
    // had before the flag existed.
    if !autoclose {
        println!("\r\n(exited {exit_code} — press any key to return to the canvas)");
        // Briefly un-paused so the background reader thread (paused above,
        // for the child's own exclusive use of the terminal) forwards the
        // next keypress here instead of it being silently dropped.
        input_paused.store(false, Ordering::Release);
        let _ = input_rx.recv().await;
        input_paused.store(true, Ordering::Release);
    }

    enable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        EnterAlternateScreen,
        EnableMouseCapture
    )?;
    // The alternate screen's own saved contents are stale after leaving
    // and re-entering it — force a full repaint on the next `draw` rather
    // than a diff against what's actually on screen now (the child's own
    // last frame).
    terminal.clear()?;

    input_paused.store(false, Ordering::Release);
    Ok(exit_code)
}

/// Mirrors `crates/cli/src/main.rs`'s own `run_tty_block`: `Ctrl+C` is
/// swallowed and just keeps waiting — the child, as its own independent
/// foreground process, decides for itself whether that signal ends it.
async fn run_tty_block(
    code: &str,
    interpreter: Option<&str>,
    envs: &HashMap<String, String>,
    cwd: &std::path::Path,
    canvas_path: &std::path::Path,
) -> i32 {
    let env_names: Vec<String> = envs.keys().cloned().collect();
    let Ok(resolved) =
        meshfox_core::resolve_command(code, interpreter, Some(cwd), Some(canvas_path), &env_names)
    else {
        return -1;
    };
    let spawned = tokio::process::Command::new(&resolved.program)
        .args(&resolved.args)
        .envs(envs)
        .envs(resolved.extra_envs.iter().map(|(k, v)| (k.as_str(), v.as_str())))
        .current_dir(cwd)
        .stdin(std::process::Stdio::inherit())
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
        .spawn();

    let mut child = match spawned {
        Ok(child) => child,
        Err(_) => {
            if let Some(path) = &resolved.cleanup {
                let _ = std::fs::remove_file(path);
            }
            return -1;
        }
    };

    let exit_code = loop {
        tokio::select! {
            status = child.wait() => break status.ok().and_then(|s| s.code()).unwrap_or(-1),
            _ = tokio::signal::ctrl_c() => continue,
        }
    };
    if let Some(path) = &resolved.cleanup {
        let _ = std::fs::remove_file(path);
    }
    exit_code
}

/// Leaves the alternate screen and relays an already-connected
/// `/api/run/tty` socket (see `crate::worker_client::tty_connect`,
/// `app::PendingHttpTty`) — the worker-routed counterpart to
/// `run_tty_handoff` above. Unlike that one, raw mode is never disabled
/// here: `run_tty_handoff` can safely leave it (the *child process* it
/// spawns owns the real fds directly and manages its own terminal
/// discipline once it does), but here *this* process is the one relaying
/// bytes itself, so local echo/line-buffering/signal-generation have to
/// stay off the whole time — same posture the rest of this TUI already
/// runs under. `input_paused` still matters: it keeps the ordinary
/// crossterm background reader (`run`, above) from racing
/// `spawn_raw_stdin_reader`'s own direct reads of the same fd.
async fn run_http_tty_handoff(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    input_paused: &Arc<AtomicBool>,
    input_rx: &mut tokio::sync::mpsc::UnboundedReceiver<Event>,
    mut socket: crate::worker_client::TtySocket,
    block_name: &str,
    autoclose: bool,
) -> io::Result<i32> {
    use std::io::Write;

    input_paused.store(true, Ordering::Release);
    tokio::time::sleep(Duration::from_millis(80)).await;

    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    print!("==> {block_name}\r\n");
    io::stdout().flush()?;

    let exit_code = bridge_http_tty(&mut socket).await;
    let _ = futures_util::SinkExt::close(&mut socket).await;

    if !autoclose {
        print!("\r\n(exited {exit_code} — press any key to return to the canvas)\r\n");
        io::stdout().flush()?;
        input_paused.store(false, Ordering::Release);
        let _ = input_rx.recv().await;
        input_paused.store(true, Ordering::Release);
    }

    execute!(
        terminal.backend_mut(),
        EnterAlternateScreen,
        EnableMouseCapture
    )?;
    terminal.clear()?;

    input_paused.store(false, Ordering::Release);
    Ok(exit_code)
}

/// The attach-mode counterpart to `run_http_tty_handoff` — joins a `tty`
/// session this TUI didn't itself start (`app::PendingHttpTtyAttach`, from
/// the `t` live-terminals view) instead of one it just spun up. No
/// `autoclose` concept: there's no chain waiting on this step to finish
/// the way a self-started run has, so there's nothing to skip straight
/// back to — always pauses on a plain "detached" message instead of
/// `run_http_tty_handoff`'s own "(exited N — ...)" (an attach-only viewer
/// is never told a real exit code either way — see `bridge_tty_pty_phase`'s
/// own doc comment on why its `RunEvent`-reading branch is dead code on
/// this path — so showing one here would just be a plausible-looking
/// guess).
async fn run_http_tty_attach_handoff(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    input_paused: &Arc<AtomicBool>,
    input_rx: &mut tokio::sync::mpsc::UnboundedReceiver<Event>,
    mut socket: crate::worker_client::TtySocket,
    block_name: &str,
) -> io::Result<()> {
    use std::io::Write;

    input_paused.store(true, Ordering::Release);
    tokio::time::sleep(Duration::from_millis(80)).await;

    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    print!("==> attached to {block_name}\r\n");
    io::stdout().flush()?;

    bridge_http_tty_attach(&mut socket).await;
    let _ = futures_util::SinkExt::close(&mut socket).await;

    print!("\r\n(detached — the session may still be running elsewhere; press any key to return to the canvas)\r\n");
    io::stdout().flush()?;
    input_paused.store(false, Ordering::Release);
    let _ = input_rx.recv().await;
    input_paused.store(true, Ordering::Release);

    execute!(
        terminal.backend_mut(),
        EnterAlternateScreen,
        EnableMouseCapture
    )?;
    terminal.clear()?;

    input_paused.store(false, Ordering::Release);
    Ok(())
}

/// Discards whatever's currently sitting unread in the real terminal's own
/// input buffer — `POSIX`'s standard `tcflush(fd, TCIFLUSH)`, the same
/// mechanism a shell or `ssh` already uses before handing a fd to a new
/// interactive program, for the same reason: without it, keystrokes typed
/// while nothing was reading stdin land on whatever starts reading it next
/// as an unexpected burst, not as if freshly typed. See `bridge_http_tty`'s
/// own call site for exactly which window this closes.
#[cfg(unix)]
fn flush_pending_stdin() {
    use std::os::unix::io::AsRawFd;
    let fd = std::io::stdin().as_raw_fd();
    unsafe {
        libc::tcflush(fd, libc::TCIFLUSH);
    }
}

/// `Write::write_all` to real stdout, but treating `WouldBlock` as
/// "retry shortly" rather than a fatal error — belt-and-braces alongside
/// `stdin_has_input_within` actually fixing the root cause (see its own
/// doc comment): stdin's read-side is no longer put in non-blocking mode
/// at all, so stdout sharing that same open file description should never
/// see `EWOULDBLOCK` from *this* process's own doing any more, but nothing
/// stops some other program sharing this controlling terminal from having
/// left it that way, or a future change here from reintroducing the same
/// mistake — treating a transient full pty buffer as fatal cost real users
/// a crashed interactive session for no good reason.
fn write_all_retrying(stdout: &mut std::io::Stdout, mut bytes: &[u8]) -> io::Result<()> {
    use std::io::Write;
    while !bytes.is_empty() {
        match stdout.write(bytes) {
            Ok(0) => return Err(io::Error::new(io::ErrorKind::WriteZero, "stdout wrote 0 bytes")),
            Ok(n) => bytes = &bytes[n..],
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(e) => return Err(e),
        }
    }
    stdout.flush()
}

/// Blocks for up to 20ms waiting for real stdin to have a byte ready,
/// via `poll(2)` — `spawn_raw_stdin_reader`'s way of checking `stop`
/// periodically without ever touching the fd's own blocking mode. An
/// earlier version instead flipped stdin non-blocking (`fcntl`,
/// `O_NONBLOCK`) so a plain `read()` would return promptly either way —
/// which turned out to be a real bug, not just an implementation detail:
/// `O_NONBLOCK` is a property of the underlying *open file description*,
/// not the file descriptor, and a terminal's stdin/stdout/stderr are
/// ordinarily all `dup()`ed from that same one description. Flipping
/// stdin non-blocking silently made stdout non-blocking too — so a large
/// screen redraw (a real interactive program's own, e.g. a table-viewer
/// repainting many rows at once) could hit a momentarily-full pty output
/// buffer and get back `EWOULDBLOCK` on an ordinary write, which
/// `bridge_http_tty` had no reason to treat as anything but fatal. `poll`
/// only checks readiness; it never mutates any flag any other fd could be
/// sharing.
#[cfg(unix)]
fn stdin_has_input_within(timeout: Duration) -> bool {
    use std::os::unix::io::AsRawFd;
    let fd = std::io::stdin().as_raw_fd();
    let mut pfd = libc::pollfd { fd, events: libc::POLLIN, revents: 0 };
    let ready = unsafe { libc::poll(&mut pfd, 1, timeout.as_millis() as libc::c_int) };
    ready > 0 && pfd.revents & libc::POLLIN != 0
}

/// Reads raw bytes straight off real stdin onto its own OS thread, forwarded
/// on the returned channel, until `stop` is set — the worker-routed tty
/// bridge's counterpart to `Stdio::inherit()`'s zero-copy fd handoff (see
/// `bridge_http_tty`). Deliberately *not* the ordinary crossterm event
/// reader (`run`'s own background thread): a real interactive session (a
/// shell, `vim`, ...) needs the exact bytes typed — arrow-key escape
/// sequences, a literal Ctrl-C byte, everything — relayed to the remote
/// pty verbatim, not parsed into `crossterm::event::Event`s and lost.
/// Checks readiness with `stdin_has_input_within` before each `read()`
/// call (which stdin's own normal blocking mode, left untouched, then
/// guarantees won't actually block) purely so this thread can notice
/// `stop` roughly every 20ms and exit instead of being leaked. Returns the
/// `JoinHandle` alongside the channel — `bridge_http_tty` joins it before
/// returning (see its own call site's doc comment for why that matters,
/// not just tidiness).
fn spawn_raw_stdin_reader(
    stop: Arc<AtomicBool>,
) -> (tokio::sync::mpsc::UnboundedReceiver<Vec<u8>>, std::thread::JoinHandle<()>) {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let handle = std::thread::spawn(move || {
        use std::io::Read;
        let mut stdin = std::io::stdin();
        let mut buf = [0u8; 4096];
        loop {
            if stop.load(Ordering::Acquire) {
                break;
            }
            #[cfg(unix)]
            if !stdin_has_input_within(Duration::from_millis(20)) {
                continue;
            }
            match stdin.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    if tx.send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(_) => break,
            }
        }
    });
    (rx, handle)
}

/// One `RunEvent` text frame arriving over the tty socket, printed as a
/// plain scrolling transcript line (mirrors `run_stream`'s own
/// `RunEvent`-to-`RunState` folding, just printed directly instead of fed
/// into a `RunState` — see `app::PendingHttpTty`'s own doc comment for why
/// a worker-routed `tty` chain's pre-interactive steps aren't shown in the
/// ordinary Output pane the way local mode's are). `bridge_http_tty`'s
/// pre-tty loop keeps going on `Continue`, hands off to the actual byte
/// relay on `EnterPty` (the interactive step itself is about to start),
/// and returns the run's own exit code on `Done` (a terminal event arrived
/// before any `tty` step ever did — a failed dep, say).
enum TtyPreludeOutcome {
    Continue,
    EnterPty,
    Done(i32),
}

fn print_tty_transcript_event(event: crate::worker_client::RunEvent) -> TtyPreludeOutcome {
    use crate::worker_client::RunEvent;
    match event {
        RunEvent::TtyStart { .. } => TtyPreludeOutcome::EnterPty,
        RunEvent::Started { .. } | RunEvent::ServiceStarted { .. } => TtyPreludeOutcome::Continue,
        RunEvent::StepStart { block, .. } => {
            print!("==> {block}\r\n");
            TtyPreludeOutcome::Continue
        }
        RunEvent::StepSkipped { block, output, duration_ms, .. } => {
            print!(
                "==> {block} (skipped, already fresh this session)\r\n{output}\r\n(skipped · {})\r\n",
                meshfox_core::format_duration_ms(duration_ms)
            );
            TtyPreludeOutcome::Continue
        }
        RunEvent::Output { text, .. } => {
            print!("{text}\r\n");
            TtyPreludeOutcome::Continue
        }
        RunEvent::StepEnd { exit_code, duration_ms, .. } => {
            print!("(exit {exit_code} · {})\r\n", meshfox_core::format_duration_ms(duration_ms));
            TtyPreludeOutcome::Continue
        }
        RunEvent::Killed { .. } => TtyPreludeOutcome::Done(-1),
        RunEvent::Error { message } => {
            print!("{message}\r\n");
            TtyPreludeOutcome::Continue
        }
        // `/api/run/tty` still reports a conflict as a pre-upgrade `409`
        // (`TtyConnectError::Conflict`), not a streamed event — this arm
        // exists only for exhaustiveness against the shared `RunEvent`
        // enum and should never actually be reached here.
        RunEvent::LockConflict { node_id, block, owner_pid, owner_desc } => {
            print!("{node_id:?}/{block:?} is locked by pid {owner_pid} ({owner_desc})\r\n");
            TtyPreludeOutcome::Done(-1)
        }
        RunEvent::Done { exit_code } => TtyPreludeOutcome::Done(exit_code),
    }
}

/// The actual byte relay for a connected `/api/run/tty` socket — binary
/// frames each direction are raw pty bytes (real stdin -> socket, socket ->
/// real stdout); text frames from the server are `RunEvent`s, printed as a
/// plain transcript (`print_tty_transcript_event`) until one reports either
/// the run's own end or that the interactive step itself is starting. Also
/// watches the real terminal's own size every 300ms (simpler and portable
/// than a `SIGWINCH` handler, and plenty responsive for a size that only
/// ever changes on an explicit user resize) and sends a
/// `{"cols":..,"rows":..}` text frame whenever it changes — the only thing
/// a text frame ever means client-to-server once `TtyStart` has arrived
/// (see `TtySocket`'s own doc comment).
///
/// `pub(crate)`, not private — `crate::main`'s own `run_via_worker` reuses
/// this verbatim for `meshfox run`'s worker-routed `tty` handling. Nothing
/// in here or in what it calls touches this TUI's own `Terminal`/alt-screen
/// state at all (that's all in this module's *caller*,
/// `run_http_tty_handoff`, which a plain `run` invocation has no
/// equivalent of — it's never in an alt screen to begin with, just needs
/// its own raw-mode enable/disable around this call).
pub(crate) async fn bridge_http_tty(socket: &mut crate::worker_client::TtySocket) -> i32 {
    use futures_util::StreamExt;
    use tokio_tungstenite::tungstenite::Message;

    // Pre-tty phase: a `tty`-touching chain can (and typically does) run
    // ordinary deps first (see `App::target_chain_tty_autoclose`'s own doc
    // comment on a chain's `tty` step never having to be the last one) —
    // this reads and prints only that transcript, deliberately *not yet*
    // touching real stdin at all. Starting the raw stdin reader here too
    // (as an earlier version of this function did) meant every keystroke
    // typed during this window — or, worse, whatever was already sitting
    // unconsumed in the terminal's own input queue the instant this
    // connection opened — got forwarded as a binary WS frame immediately,
    // with nothing server-side reading it yet (a plain step's own loop
    // never polls the socket for input). Those bytes don't vanish: they
    // sit in the OS's TCP receive buffer until the pty step's own
    // `socket.recv()` loop starts polling, then land on it all at once, as
    // if just typed — which is exactly the "opens fine most of the time,
    // but sometimes the interactive program gets flooded with garbage the
    // instant it starts and crashes" bug this restructuring fixes.
    loop {
        match socket.next().await {
            Some(Ok(Message::Text(text))) => {
                let Ok(event) = serde_json::from_str::<crate::worker_client::RunEvent>(&text) else {
                    continue;
                };
                match print_tty_transcript_event(event) {
                    TtyPreludeOutcome::Continue => {}
                    TtyPreludeOutcome::EnterPty => break,
                    TtyPreludeOutcome::Done(exit_code) => return exit_code,
                }
            }
            // A binary frame has no meaning before `TtyStart` — nothing
            // should send one this early, but ignoring rather than
            // erroring costs nothing.
            Some(Ok(Message::Binary(_))) => {}
            Some(Ok(Message::Close(_))) | None | Some(Err(_)) => return -1,
            Some(Ok(_)) => {}
        }
    }

    bridge_tty_pty_phase(socket).await
}

/// The actual interactive byte relay, shared by `bridge_http_tty` (once its
/// own pre-tty transcript phase reaches `TtyStart`) and
/// `bridge_http_tty_attach` (which has no pre-tty phase to begin with — an
/// attach connection is already mid-session the instant it opens, see that
/// function's own doc comment): binary frames each direction are raw pty
/// bytes (real stdin -> socket, socket -> real stdout); a text frame from
/// the server is a `RunEvent`, printed as a plain transcript
/// (`print_tty_transcript_event`) — meaningful for a `tty_connect`ed
/// socket (a trailing `Done` after the interactive step, say), never sent
/// at all by an attach connection (`relay_tty_viewer`, server-side, has no
/// `RunEvent` vocabulary), so this branch is simply dead code on that path.
/// Also watches the real terminal's own size every 300ms (simpler and
/// portable than a `SIGWINCH` handler, and plenty responsive for a size
/// that only ever changes on an explicit user resize) and sends a
/// `{"cols":..,"rows":..}` text frame whenever it changes — the only thing
/// a text frame ever means client-to-server on either kind of connection.
async fn bridge_tty_pty_phase(socket: &mut crate::worker_client::TtySocket) -> i32 {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;

    // Pty phase: only from here does real stdin get read/forwarded at all.
    // The kernel's own tty input buffer, though, has been silently piling
    // up this whole time regardless — `input_paused` (set before this
    // function is ever reached, see `run_http_tty_handoff`) stopped the
    // ordinary crossterm reader thread from draining it, and (for
    // `bridge_http_tty`'s own caller) the pre-tty phase just above
    // deliberately never reads real stdin either (see its own doc comment
    // on why not). Left alone, whatever a user typed while still-running
    // deps ate anywhere from milliseconds to many seconds would all land
    // on the interactive program in one garbled burst the instant it
    // actually starts — flushing it away here, right before
    // `spawn_raw_stdin_reader` starts reading for real, is what keeps a
    // slow (or merely observably non-instant) dep from ever being able to
    // do that. Harmless on an attach connection too (nothing meaningful
    // could be sitting in the terminal's own input queue yet — the whole
    // point of attaching is that this process only just started viewing).
    #[cfg(unix)]
    flush_pending_stdin();
    let stop = Arc::new(AtomicBool::new(false));
    let (mut stdin_rx, stdin_thread) = spawn_raw_stdin_reader(Arc::clone(&stop));
    let mut stdout = std::io::stdout();
    let mut last_size = crossterm::terminal::size().ok();

    let exit_code = loop {
        tokio::select! {
            input = stdin_rx.recv() => {
                let Some(bytes) = input else { break -1 };
                if socket.send(Message::Binary(bytes.into())).await.is_err() {
                    break -1;
                }
            }
            msg = socket.next() => {
                match msg {
                    Some(Ok(Message::Binary(bytes))) => {
                        if write_all_retrying(&mut stdout, &bytes).is_err() {
                            break -1;
                        }
                    }
                    Some(Ok(Message::Text(text))) => {
                        let Ok(event) = serde_json::from_str::<crate::worker_client::RunEvent>(&text) else {
                            continue;
                        };
                        match print_tty_transcript_event(event) {
                            TtyPreludeOutcome::Continue | TtyPreludeOutcome::EnterPty => {}
                            TtyPreludeOutcome::Done(exit_code) => break exit_code,
                        }
                    }
                    Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break -1,
                    Some(Ok(_)) => {}
                }
            }
            _ = tokio::time::sleep(Duration::from_millis(300)) => {
                if let Ok(size) = crossterm::terminal::size() {
                    if Some(size) != last_size {
                        last_size = Some(size);
                        let resize = serde_json::json!({"cols": size.0, "rows": size.1}).to_string();
                        if socket.send(Message::Text(resize.into())).await.is_err() {
                            break -1;
                        }
                    }
                }
            }
        }
    };
    stop.store(true, Ordering::Release);
    // Waits for the thread to actually notice `stop` and revert stdin's
    // fd mode (`set_stdin_nonblocking(false)`) before this function
    // returns — skipping this (as an earlier version did) left a real
    // race: the caller (`run_http_tty_handoff`) goes on to resume the
    // ordinary crossterm reader thread almost immediately after this
    // returns, and if *this* thread hadn't actually finished exiting yet,
    // both threads could read real stdin for a brief window. Worse, on a
    // *second* `tty` block run in the same session, this thread's own
    // now-delayed `set_stdin_nonblocking(false)` cleanup could land after
    // the next invocation's own `set_stdin_nonblocking(true)`, silently
    // flipping stdin back to blocking mode out from under a thread that's
    // relying on it staying non-blocking to ever notice its own `stop`
    // flag — exactly the kind of "works most of the time, but a session
    // that runs more than one interactive block eventually wedges" bug
    // this specific ordering exists to rule out. `spawn_blocking` rather
    // than a bare `.join()` so this wait (at most ~20ms, this thread's own
    // poll interval) doesn't block the async runtime thread it's
    // otherwise running on.
    let _ = tokio::task::spawn_blocking(move || stdin_thread.join()).await;
    exit_code
}

/// The attach-mode counterpart to `bridge_http_tty` — no pre-tty transcript
/// phase at all: a `worker_client::tty_attach` socket is already mid-
/// session the instant it opens (see that function's own doc comment), so
/// this goes straight to the same interactive byte relay `bridge_http_tty`
/// itself hands off to once its own prelude reaches `TtyStart`.
async fn bridge_http_tty_attach(socket: &mut crate::worker_client::TtySocket) -> i32 {
    bridge_tty_pty_phase(socket).await
}
