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
mod tree;
mod ui;

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

pub async fn run(canvas_path: PathBuf) -> io::Result<()> {
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

    let result = match App::new(canvas_path, link_preview_tx) {
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
            spawn_file_watcher(app.canvas_path.clone(), Arc::clone(&app.known_raw), reload_tx);

            main_loop(
                &mut terminal,
                &mut app,
                &mut input_rx,
                &paused,
                &mut reload_rx,
                &mut link_preview_rx,
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

async fn main_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut App,
    input_rx: &mut tokio::sync::mpsc::UnboundedReceiver<Event>,
    input_paused: &Arc<AtomicBool>,
    reload_rx: &mut tokio::sync::mpsc::UnboundedReceiver<String>,
    link_preview_rx: &mut tokio::sync::mpsc::UnboundedReceiver<LinkPreviewMsg>,
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
                &pending.block_name,
                &pending.code,
                pending.interpreter.as_deref(),
                &pending.env,
                crate::canvas_root_dir(&app.canvas_path),
            )
            .await?;
            app.resume_after_tty(exit_code).await;
            continue;
        }

        let has_proc = app.run.as_ref().is_some_and(|r| r.proc.is_some());
        tokio::select! {
            maybe_ev = input_rx.recv() => {
                match maybe_ev {
                    Some(Event::Key(key)) if key.kind == KeyEventKind::Press => app.on_key(key).await,
                    Some(Event::Mouse(mouse)) => app.on_mouse(mouse),
                    Some(_) => {}
                    None => return Ok(()),
                }
            }
            line = async {
                app.run.as_mut().unwrap().proc.as_mut().unwrap().output_rx.recv().await
            }, if has_proc => {
                app.on_output_line(line).await;
            }
            Some(content) = reload_rx.recv() => {
                app.on_external_change(content);
            }
            Some(msg) = link_preview_rx.recv() => {
                app.on_link_preview_msg(msg);
            }
        }
    }
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
    block_name: &str,
    code: &str,
    interpreter: Option<&str>,
    env: &HashMap<String, String>,
    cwd: &std::path::Path,
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

    let exit_code = run_tty_block(code, interpreter, env, cwd).await;

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
) -> i32 {
    let Ok(resolved) = meshfox_core::resolve_command(code, interpreter) else {
        return -1;
    };
    let spawned = tokio::process::Command::new(&resolved.program)
        .args(&resolved.args)
        .envs(envs)
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
