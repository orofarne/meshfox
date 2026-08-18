//! Fullscreen raw-Markdown source editor — the terminal counterpart to the
//! web UI's Source mode (`web/src/CanvasSourceEditor.tsx`), opened with
//! `e` on the selected tree node. Unlike the rest of the tui (read + run
//! only — see `mod.rs`'s own module doc comment), this actually writes to
//! disk: `Ctrl-s` saves, `Esc` leaves (confirming first if there are
//! unsaved edits).
//!
//! A document that pulls in other files via `include` (see SPEC.md) is
//! still just one file at a time here, same as the web editor: `files`
//! (via `meshfox_core::include::list_includes`, computed once when the
//! editor opens) lists every include reachable from the primary document,
//! however deeply nested, and `Ctrl-f` opens a picker over "this document"
//! plus that list to switch which file's raw text is being edited.

use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use edtui::{EditorEventHandler, EditorMode, EditorState, Index2, Lines};
use meshfox_core::include::IncludeInfo;

/// What the caller (`App::on_key`) should do after handing a keypress to
/// the editor.
pub enum SourceEditorOutcome {
    /// Stay open — nothing for the caller to do.
    Stay,
    /// The buffer's own Esc handling wants to close (see `SourceEditorState::on_key`)
    /// — the caller drops `App::source_editor`.
    Close,
    /// `Ctrl-s` was pressed — the caller calls `App::save_source_editor`,
    /// which needs `App`'s own fields (`raw`/`canvas`) that this type
    /// deliberately doesn't hold, to keep file I/O and validation in one
    /// place (mirrors the server's `put_canvas_raw`).
    Save,
}

pub struct SourceEditorState {
    /// The primary document's own path — never changes for the lifetime
    /// of one editor session, used to tell "editing the document itself"
    /// apart from "editing an include target" (`self.path == primary_path`)
    /// without a separate `Option` to keep in sync.
    primary_path: PathBuf,
    pub path: PathBuf,
    /// Whether `path` needs to parse as a canvas before a save is allowed
    /// — always `true` for the primary document; for an include target,
    /// only when it's a canvas one (a plain-Markdown target has no such
    /// structure to hold it to — see `meshfox_core::include`'s own module
    /// docs).
    pub is_canvas: bool,
    pub editor: EditorState,
    events: EditorEventHandler,
    /// Last-saved (or last-loaded) snapshot — the dirty check.
    original: String,
    /// Every include reachable from the primary document (however deeply
    /// nested), computed once at open time — switching files (`Ctrl-f`)
    /// reads straight out of this rather than re-walking the include
    /// graph, same as the web picker only calls `fetchIncludes` once.
    pub files: Vec<IncludeInfo>,
    pub file_picker_open: bool,
    /// Index into `files`, offset by one — `0` means "this document"
    /// (`primary_path`), `n` means `files[n - 1]`.
    pub file_picker_selected: usize,
    pub error: Option<String>,
    /// Set by a first Esc on a dirty buffer (see `on_key`) — a second Esc
    /// with this still set actually closes, discarding. Cleared by any
    /// other keypress, so it only ever fires as an immediate follow-up,
    /// never a "leftover" confirmation from several edits ago.
    pending_discard: bool,
}

impl SourceEditorState {
    /// Opens `path` (the primary document if `path == primary_path`,
    /// otherwise some include target already resolved by the caller —
    /// see `App::open_source_editor`) with `cursor` as the starting
    /// position. `files` is computed once here via `list_includes` and
    /// reused for every subsequent file switch this session.
    pub fn open(
        primary_path: PathBuf,
        path: PathBuf,
        is_canvas: bool,
        cursor: Index2,
        files: Vec<IncludeInfo>,
    ) -> std::io::Result<Self> {
        let raw = std::fs::read_to_string(&path)?;
        let mut editor = EditorState::new(Lines::from(raw.as_str()));
        editor.cursor = cursor;
        prime_viewport(&mut editor);
        Ok(SourceEditorState {
            primary_path,
            path,
            is_canvas,
            editor,
            events: EditorEventHandler::default(),
            original: raw,
            files,
            file_picker_open: false,
            file_picker_selected: 0,
            error: None,
            pending_discard: false,
        })
    }

    pub fn dirty(&self) -> bool {
        self.editor.lines.to_string() != self.original
    }

    /// Marks the buffer as saved — called by `App::save_source_editor`
    /// right after it successfully writes `self.path` to disk, so the
    /// dirty check (and thus `Ctrl-f`'s switch-file guard) reflects it
    /// immediately rather than comparing against stale content.
    pub fn mark_saved(&mut self) {
        self.original = self.editor.lines.to_string();
    }

    pub fn on_key(&mut self, key: KeyEvent) -> SourceEditorOutcome {
        if self.file_picker_open {
            self.on_file_picker_key(key);
            return SourceEditorOutcome::Stay;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Char('s') => return SourceEditorOutcome::Save,
                KeyCode::Char('f') => {
                    self.file_picker_open = true;
                    self.file_picker_selected = if self.path == self.primary_path {
                        0
                    } else {
                        self.current_file_index().map(|i| i + 1).unwrap_or(0)
                    };
                    return SourceEditorOutcome::Stay;
                }
                _ => {}
            }
        }
        // Only Normal mode's own Esc is ours to take — Insert mode's Esc
        // has to reach `events` below first (it's what steps back to
        // Normal in the first place; vim's own Normal-mode Esc is already
        // a no-op, so intercepting it here instead never loses anything).
        if key.code == KeyCode::Esc && self.editor.mode == EditorMode::Normal {
            if self.dirty() && !self.pending_discard {
                self.pending_discard = true;
                self.error =
                    Some("unsaved changes — Ctrl-s to save, or Esc again to discard".to_string());
                return SourceEditorOutcome::Stay;
            }
            return SourceEditorOutcome::Close;
        }
        self.pending_discard = false;
        self.error = None;
        self.events.on_key_event(key, &mut self.editor);
        SourceEditorOutcome::Stay
    }

    fn current_file_index(&self) -> Option<usize> {
        self.files.iter().position(|i| i.path == self.path)
    }

    fn on_file_picker_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.file_picker_selected = self.file_picker_selected.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.file_picker_selected = (self.file_picker_selected + 1).min(self.files.len());
            }
            KeyCode::Esc => self.file_picker_open = false,
            KeyCode::Enter => {
                self.file_picker_open = false;
                if self.dirty() {
                    self.error = Some(
                        "save or discard changes before switching files (Ctrl-s, or Esc twice)"
                            .to_string(),
                    );
                    return;
                }
                let target = if self.file_picker_selected == 0 {
                    Some((self.primary_path.clone(), true))
                } else {
                    self.files
                        .get(self.file_picker_selected - 1)
                        .map(|i| (i.path.clone(), i.is_canvas))
                };
                if let Some((path, is_canvas)) = target {
                    self.switch_to(path, is_canvas);
                }
            }
            _ => {}
        }
    }

    fn switch_to(&mut self, path: PathBuf, is_canvas: bool) {
        let raw = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(e) => {
                self.error = Some(format!("failed to read {}: {e}", path.display()));
                return;
            }
        };
        self.editor = EditorState::new(Lines::from(raw.as_str()));
        self.original = raw;
        self.path = path;
        self.is_canvas = is_canvas;
        self.error = None;
    }
}

/// Works around an `edtui` quirk that otherwise left the source editor
/// showing the top of the file even though the cursor opened correctly
/// deep inside it (TODO.canvas.md: "Скролл в TUI"): `EditorView::render`
/// only scrolls the viewport to follow the cursor once it already knows
/// how many rows fit on screen (its own internal `num_rows`), and that
/// count is normally only ever *learned* by actually rendering once — it
/// starts at `0`, and `edtui`'s own scroll-into-view logic explicitly
/// no-ops whenever it's still `0`. So the very first real frame after
/// opening on a cursor far down the file rendered with the viewport still
/// stuck at the top, and nothing ever forced a second frame to fix it —
/// this app only redraws in response to an event (see `mod.rs`'s main
/// loop), so a user who just opened the editor and looked, without
/// pressing another key, saw that stuck first frame indefinitely, cursor
/// position correct internally the whole time.
///
/// Fixes it via `EditorState::set_viewport_height` — a public escape
/// hatch `edtui` itself documents as normally unnecessary ("set
/// automatically during render") for exactly this kind of exception: tell
/// it how many rows are available up front, matching
/// `ui::render_source_editor`'s own header(1)/body/footer(1) split, so
/// the *first* real render already has a real row count to scroll
/// against instead of `0`.
fn prime_viewport(editor: &mut EditorState) {
    let (_cols, rows) = crossterm::terminal::size().unwrap_or((80, 24));
    editor.set_viewport_height(rows.saturating_sub(2) as usize);
}

/// Converts a byte offset in `text` (e.g. from
/// `meshfox_core::mdcanvas::node_body_offset`) to an edtui cursor position
/// — `Index2`'s `row`/`col` count *lines*/*chars*, not bytes. Skips a
/// leading newline first (`node_body_offset` lands right at the blank
/// line separating a node's `meshfox:node` comment from its own body —
/// see that function's own doc comment), so the cursor opens on the
/// body's first real line instead of the blank line above it.
pub fn byte_offset_to_cursor(text: &str, mut byte_offset: usize) -> Index2 {
    if text[byte_offset..].starts_with('\n') {
        byte_offset += 1;
    }
    let mut row = 0;
    let mut col = 0;
    for (i, ch) in text.char_indices() {
        if i >= byte_offset {
            break;
        }
        if ch == '\n' {
            row += 1;
            col = 0;
        } else {
            col += 1;
        }
    }
    Index2::new(row, col)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn byte_offset_to_cursor_skips_a_leading_newline() {
        // Matches `node_body_offset`'s own real shape (see its doc
        // comment/tests in `meshfox_core::mdcanvas`): it lands right on
        // the single `\n` separating a node's `meshfox:node` comment from
        // its own body, one character above the body's real first line —
        // skipping it is what lands the cursor on "line1" instead.
        let text = "line0\nline1\nline2";
        let offset = text.find("line1").unwrap() - 1; // the `\n` right before it
        assert_eq!(byte_offset_to_cursor(text, offset), Index2::new(1, 0));
    }

    #[test]
    fn byte_offset_to_cursor_finds_a_mid_line_column() {
        let text = "line0\nline1\nline2";
        let offset = text.find("ne2").unwrap();
        assert_eq!(byte_offset_to_cursor(text, offset), Index2::new(2, 2));
    }

    // TODO.canvas.md: "Скролл в TUI" — opening the source editor with the
    // cursor already deep in a long file left the *very first* rendered
    // frame scrolled to the top regardless (see `prime_viewport`'s own doc
    // comment for the underlying `edtui` mechanics). Without the fix, a
    // cursor row past the visible height renders with no on-screen cursor
    // position at all on that first frame (`cursor_screen_position()` is
    // `None` — the row isn't among the ones actually drawn) and the
    // viewport offset stays `(0, 0)`; with it, that very first frame
    // already scrolls to keep the cursor visible.
    #[test]
    fn prime_viewport_makes_the_first_real_render_already_scrolled_to_the_cursor() {
        use edtui::EditorView;
        use ratatui::{buffer::Buffer, layout::Rect, widgets::Widget};

        let text = (0..200)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let mut editor = EditorState::new(Lines::from(text.as_str()));
        editor.cursor = Index2::new(150, 0);
        prime_viewport(&mut editor);

        // Same shape `crossterm::terminal::size()`'s test-environment
        // fallback implies (see `prime_viewport`): an 80x24 terminal, minus
        // the source editor's own header/footer rows.
        let area = Rect::new(0, 0, 80, 22);
        let mut buf = Buffer::empty(area);
        EditorView::new(&mut editor).wrap(true).render(area, &mut buf);

        assert!(
            editor.cursor_screen_position().is_some(),
            "cursor should already be on screen on the first real render"
        );
        assert!(
            editor.viewport_offset().1 > 0,
            "viewport should already have scrolled down to the cursor, not stayed at the top"
        );
    }
}
