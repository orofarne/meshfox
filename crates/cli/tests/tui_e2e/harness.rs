//! Drives a real, spawned `meshfox tui <canvas>` inside a real pty — the
//! same `openpty`/`CommandBuilder`/`spawn_command` shape
//! `crates/server/src/pty_exec.rs::spawn` already uses for `tty` blocks,
//! just synchronous (no tokio needed for a test) and feeding a
//! `vt100::Parser` instead of a channel of raw chunks, so tests can assert
//! on rendered screen text instead of raw ANSI bytes.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};

pub struct TuiSession {
    parser: Arc<Mutex<vt100::Parser>>,
    writer: Box<dyn Write + Send>,
    child: Box<dyn Child + Send + Sync>,
    // Keeps the pty's master side alive for the session's whole lifetime —
    // dropping it would close the pty (and the reader thread's `read`
    // would just see EOF) well before the test is done with it.
    _master: Box<dyn MasterPty + Send>,
    fixture_dir: PathBuf,
}

impl TuiSession {
    /// Spawns `meshfox tui <canvas_path>` in a fresh `rows`x`cols` pty —
    /// fixed, explicit size (not "whatever the real terminal running this
    /// test process happens to be") so a screen-text assertion is
    /// deterministic regardless of what's driving `cargo test`.
    /// `fixture_dir` is removed on `Drop` — pass the directory `canvas_path`
    /// itself lives in (see `fixtures::write_fixture`).
    pub fn spawn(canvas_path: &Path, fixture_dir: PathBuf, rows: u16, cols: u16) -> Self {
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("openpty");

        let mut cmd = CommandBuilder::new(env!("CARGO_BIN_EXE_meshfox"));
        cmd.arg("tui");
        cmd.arg(canvas_path);

        let child = pair.slave.spawn_command(cmd).expect("spawn meshfox tui");
        // Only needed to spawn the child (which inherits it as its
        // controlling terminal) — dropping this copy is what lets the
        // master's own reader see EOF once the child actually exits, same
        // reasoning `pty_exec.rs::spawn` already documents.
        drop(pair.slave);

        let mut reader = pair.master.try_clone_reader().expect("clone pty reader");
        let writer = pair.master.take_writer().expect("pty writer");

        let parser = Arc::new(Mutex::new(vt100::Parser::new(rows, cols, 0)));
        let parser_for_thread = Arc::clone(&parser);
        std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => parser_for_thread.lock().unwrap().process(&buf[..n]),
                }
            }
        });

        TuiSession {
            parser,
            writer,
            child,
            _master: pair.master,
            fixture_dir,
        }
    }

    /// Writes `s` to the pty's stdin as-is — plain characters (`"j"`),
    /// control bytes (`"\r"` for Enter, `"\x1b"` for Esc), or a raw escape
    /// sequence (arrow keys, mouse events — see `send_mouse_click`).
    pub fn send_keys(&mut self, s: &str) {
        self.writer.write_all(s.as_bytes()).expect("write to pty");
        self.writer.flush().expect("flush pty");
    }

    /// A left-click at 0-indexed `(row, col)` — standard xterm SGR mouse
    /// protocol (`EnableMouseCapture`, already enabled by `tui/mod.rs`,
    /// puts crossterm's own parser in exactly this mode): press
    /// (`ESC [ < 0 ; Cx ; Cy M`) immediately followed by release (`...m`),
    /// both 1-indexed on the wire.
    pub fn send_mouse_click(&mut self, row: u16, col: u16) {
        self.send_keys(&format!("\x1b[<0;{};{}M", col + 1, row + 1));
        self.send_keys(&format!("\x1b[<0;{};{}m", col + 1, row + 1));
    }

    /// Two clicks at the same cell, close enough together that `on_mouse`'s
    /// own (not-yet-implemented — see `mouse_drag_dblclick.rs`) double-
    /// click detection should treat them as one gesture, not two separate
    /// clicks.
    pub fn send_mouse_double_click(&mut self, row: u16, col: u16) {
        self.send_mouse_click(row, col);
        std::thread::sleep(Duration::from_millis(60));
        self.send_mouse_click(row, col);
    }

    /// A wheel event at 0-indexed `(row, col)` — SGR button code `64` (up)
    /// or `65` (down), no matching release event (a real wheel doesn't
    /// send one either).
    pub fn send_mouse_scroll(&mut self, row: u16, col: u16, down: bool) {
        let button = if down { 65 } else { 64 };
        self.send_keys(&format!("\x1b[<{button};{};{}M", col + 1, row + 1));
    }

    /// A horizontal wheel event — SGR button code `67` (right) or `66`
    /// (left), the standard xterm extension alongside `64`/`65` for the
    /// vertical wheel (`crossterm::event::MouseEventKind::ScrollLeft`/
    /// `ScrollRight`).
    pub fn send_mouse_scroll_horizontal(&mut self, row: u16, col: u16, right: bool) {
        let button = if right { 67 } else { 66 };
        self.send_keys(&format!("\x1b[<{button};{};{}M", col + 1, row + 1));
    }

    /// The rendered screen as plain text (rows joined by `\n`, trailing
    /// blank cells on each row trimmed) — for `.contains()`-style
    /// assertions. Uses `vt100::Screen::contents`, which already does this
    /// exact trimming.
    pub fn screen_text(&self) -> String {
        self.parser.lock().unwrap().screen().contents()
    }

    /// Polls `screen_text()` every 30ms until it contains `needle`, up to
    /// `timeout` — never a raw `sleep` in a test, which would either race
    /// a slow CI machine or waste time on a fast one.
    pub fn wait_for(&self, needle: &str, timeout: Duration) -> Result<(), String> {
        let deadline = Instant::now() + timeout;
        loop {
            let screen = self.screen_text();
            if screen.contains(needle) {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(format!(
                    "timed out after {timeout:?} waiting for {needle:?} — last screen:\n{screen}"
                ));
            }
            std::thread::sleep(Duration::from_millis(30));
        }
    }

    /// 0-indexed `(row, col)` of the first cell where `needle` starts,
    /// scanning the rendered screen row-major — `None` if it isn't
    /// present. Lets a mouse test locate what it wants to click by the
    /// text actually on screen instead of a hand-guessed, layout-fragile
    /// coordinate.
    pub fn find(&self, needle: &str) -> Option<(u16, u16)> {
        for (row, line) in self.screen_text().split('\n').enumerate() {
            if let Some(byte_idx) = line.find(needle) {
                // Column count, not byte offset — a box-drawing glyph
                // (`├`, `▶`, ...) is one terminal column but several UTF-8
                // bytes.
                let col = line[..byte_idx].chars().count();
                return Some((row as u16, col as u16));
            }
        }
        None
    }

    /// The foreground color vt100 recorded for the cell at 0-indexed
    /// `(row, col)` — `None` if nothing's been drawn there yet. For
    /// asserting a focus/accent highlight (`crates/cli/src/tui/theme.rs`)
    /// actually landed on the pane a click targeted, where a text-only
    /// assertion can't tell (the text is the same either way — only its
    /// color changes).
    pub fn fgcolor_at(&self, row: u16, col: u16) -> Option<vt100::Color> {
        self.parser
            .lock()
            .unwrap()
            .screen()
            .cell(row, col)
            .map(|c| c.fgcolor())
    }

    /// Whether the cell at 0-indexed `(row, col)` is rendered in reverse
    /// video (`ratatui::style::Modifier::REVERSED`, SGR `7`) — the
    /// convention every `List`'s own selected row already uses in this
    /// TUI (`highlight_style`). For asserting a mouse click actually moved
    /// a list's selection, in a modal or the tree.
    pub fn inverse_at(&self, row: u16, col: u16) -> bool {
        self.parser
            .lock()
            .unwrap()
            .screen()
            .cell(row, col)
            .is_some_and(|c| c.inverse())
    }

    /// Blocks (briefly) until the child has actually exited, so a "press
    /// `q`, does it really quit" test isn't just asserting the app *would*
    /// exit eventually.
    pub fn wait_for_exit(&mut self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            if matches!(self.child.try_wait(), Ok(Some(_))) {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(30));
        }
    }
}

impl Drop for TuiSession {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = std::fs::remove_dir_all(&self.fixture_dir);
    }
}
