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

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent};
use edtui::actions::{AppendNewline, InsertChar};
use edtui::{EditorEventHandler, EditorMode, EditorState, Index2, Lines};
use meshfox_core::include::IncludeInfo;
use meshfox_core::suggest::{
    attr_candidates, attribute_value_at, document_attr_context, finite_values, insertion_col,
    inside_attribute_value, tags_value_range, AttrCandidate, AttrKind,
};

use super::source_diff::{DiffOutcome, SourceDiff};
use crate::worker_client::ConflictLines;

/// What the caller (`App::on_key`) should do after handing a keypress to
/// the editor.
pub enum SourceEditorOutcome {
    /// Stay open — nothing for the caller to do.
    Stay,
    /// The buffer's own Esc handling wants to close (see `SourceEditorState::on_key`)
    /// — the caller drops `App::source_editor`.
    Close,
    /// The file picker chose an include target: the caller fetches its
    /// text from the worker and calls `SourceEditorState::finish_switch`.
    Switch { path: PathBuf, is_canvas: bool },
    /// `Ctrl-s` was pressed — the caller calls `App::save_source_editor`,
    /// which needs `App`'s own fields (`raw`/`canvas`) that this type
    /// deliberately doesn't hold, to keep file I/O and validation in one
    /// place (mirrors the server's `put_canvas_raw`).
    Save,
    /// `Ctrl-o` — the caller hands the buffer to the external editor
    /// (`[tui] editor`), or says there is none configured.
    External,
}

pub struct SourceEditorState {
    /// The primary document's own path — never changes for the lifetime
    /// of one editor session, used to tell "editing the document itself"
    /// apart from "editing an include target" (`self.path == primary_path`)
    /// without a separate `Option` to keep in sync.
    primary_path: PathBuf,
    primary_raw: String,
    pub path: PathBuf,
    /// Whether `path` needs to parse as a canvas before a save is allowed
    /// — always `true` for the primary document; for an include target,
    /// only when it's a canvas one (a plain-Markdown target has no such
    /// structure to hold it to — see `meshfox_core::include`'s own module
    /// docs).
    pub is_canvas: bool,
    pub read_only: bool,
    pub editor: EditorState,
    events: EditorEventHandler,
    /// Last-saved (or last-loaded) snapshot — the dirty check, and the
    /// version of the file every save claims to be based on (`base_rev`).
    original: String,
    /// The revision a save found the file at when it had changed on disk
    /// since `original` was read: nothing was written, and the *next*
    /// save is written against this one instead — an explicit second
    /// `Ctrl-s` to overwrite what is there now. Cleared whenever the
    /// editor is saved or switches file.
    conflict_rev: Option<String>,
    /// The side-by-side comparison shown after a save that found the file
    /// changed and could not combine it with the buffer — while it is
    /// `Some`, it owns the keymap and the screen (see `source_diff.rs`).
    pub diff: Option<SourceDiff>,
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
    /// `Ctrl-p` popup — TODO.canvas.md: "Саджесты... в TUI". `open_attr_suggest`
    /// fills `attr_suggest_kind`/`attr_suggest_candidates` from whatever
    /// `meshfox:node`/`meshfox:edge`/fence-attribute line the cursor is
    /// currently on; `on_attr_suggest_key` drives selection while this is
    /// `true`.
    pub attr_suggest_open: bool,
    attr_suggest_kind: AttrKind,
    pub attr_suggest_candidates: Vec<AttrCandidate>,
    pub attr_suggest_selected: usize,
    /// Every distinct tag used anywhere in the currently loaded canvas
    /// (nodes and extra edges alike), computed once by the caller
    /// (`App::open_source_editor`) when the editor opens — the candidate
    /// source for the `tags="..."`-value flavor of `Ctrl-p` below.
    /// TODO.canvas.md: "Саджест по тегам в TUI".
    all_tags: Vec<String>,
    /// Same `Ctrl-p` popup mechanism as `attr_suggest_*`, but triggered
    /// instead when the cursor sits inside an already-typed `tags="..."`
    /// value (`open_attr_suggest` picks between the two) — candidates are
    /// tag names already used elsewhere in the document, not attribute
    /// names, and picking one inserts the tag name plus a trailing comma
    /// rather than a `key=""` template.
    value_suggest: Option<(String, usize, usize)>,
    pub tag_suggest_open: bool,
    pub tag_suggest_candidates: Vec<String>,
    pub tag_suggest_selected: usize,
    /// Ctrl-g captures input until a symbol is inserted or the palette is cancelled.
    pub unicode_picker: Option<super::unicode_picker::UnicodePicker>,
}

impl SourceEditorState {
    /// Opens `path` (the primary document if `path == primary_path`,
    /// otherwise some include target already resolved by the caller —
    /// see `App::open_source_editor`) with `cursor` as the starting
    /// position. `raw` is `path`'s own text (the caller reads it from the
    /// worker; for the primary document it equals `primary_raw`). `files` is computed once here via `list_includes` and
    /// reused for every subsequent file switch this session.
    pub fn open(
        primary_path: PathBuf,
        path: PathBuf,
        is_canvas: bool,
        cursor: Index2,
        files: Vec<IncludeInfo>,
        all_tags: Vec<String>,
        primary_raw: String,
        raw: String,
    ) -> std::io::Result<Self> {
        let mut editor = EditorState::new(Lines::from(raw.as_str()));
        editor.cursor = cursor;
        prime_viewport(&mut editor);
        let read_only = path != primary_path
            && (meshfox_core::mdcanvas::is_canvas_path(&path)
                || meshfox_core::mdcanvas::has_marker(&raw));
        Ok(SourceEditorState {
            read_only,
            primary_path,
            primary_raw,
            path,
            is_canvas,
            editor,
            events: EditorEventHandler::default(),
            original: raw,
            conflict_rev: None,
            diff: None,
            files,
            file_picker_open: false,
            file_picker_selected: 0,
            error: None,
            pending_discard: false,
            attr_suggest_open: false,
            attr_suggest_kind: AttrKind::Node,
            attr_suggest_candidates: Vec::new(),
            attr_suggest_selected: 0,
            all_tags,
            unicode_picker: None,
            value_suggest: None,
            tag_suggest_open: false,
            tag_suggest_candidates: Vec::new(),
            tag_suggest_selected: 0,
        })
    }

    pub fn dirty(&self) -> bool {
        self.editor.lines.to_string() != self.original
    }

    /// Marks the buffer as saved — called by `App::save_source_editor`
    /// right after it successfully writes `self.path` to disk, so the
    /// dirty check (and thus `Ctrl-f`'s switch-file guard) reflects it
    /// immediately rather than comparing against stale content.
    /// The revision (`meshfox_core::body_rev`) of the file as this editor
    /// last read it — what a save must name as the version it replaces. A
    /// save that already found the file changed (`conflict_rev`) names the
    /// revision it found instead, so pressing `Ctrl-s` again overwrites it.
    pub fn base_rev(&self) -> String {
        self.conflict_rev
            .clone()
            .unwrap_or_else(|| meshfox_core::body_rev(&self.original))
    }

    pub fn mark_saved(&mut self) {
        self.conflict_rev = None;
        self.original = self.editor.lines.to_string();
        if self.path == self.primary_path {
            self.primary_raw = self.original.clone();
        }
    }

    /// Shows the comparison of the file as it is on disk now (`theirs_text`,
    /// at `theirs_rev`) against the buffer, after a save that could not
    /// combine them. `conflicts` are the line ranges that overlapped; none
    /// means no merge was possible at all.
    pub fn open_diff(
        &mut self,
        theirs_text: String,
        theirs_rev: String,
        conflicts: &[ConflictLines],
    ) {
        let mine = self.editor.lines.to_string();
        self.diff = Some(SourceDiff::new(theirs_text, theirs_rev, &mine, conflicts));
    }

    /// Replaces the buffer with the file as it is on disk (the comparison's
    /// "take theirs"): nothing of the buffer's own edits survives, and
    /// there is nothing left to save.
    /// Loads text edited elsewhere (the external editor) into the buffer,
    /// leaving the saved snapshot and `base_rev` alone so the save that
    /// follows is checked against the version this editor started from.
    pub fn replace_text(&mut self, text: &str) {
        let row = self.editor.cursor.row;
        self.editor = EditorState::new(Lines::from(text));
        let last = self.editor.lines.len().saturating_sub(1);
        self.editor.cursor = Index2::new(row.min(last), 0);
        prime_viewport(&mut self.editor);
        self.pending_discard = false;
    }

    /// The cursor as a 1-based (line, column), for an external editor.
    pub fn cursor_line_col(&self) -> (usize, usize) {
        (self.editor.cursor.row + 1, self.editor.cursor.col + 1)
    }

    /// The edited file's name without its `.canvas.md`/`.md` suffix.
    pub fn file_stem(&self) -> String {
        let name = self
            .path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let stem = name
            .strip_suffix(".canvas.md")
            .or_else(|| name.strip_suffix(".md"))
            .unwrap_or(&name);
        if stem.is_empty() { "canvas" } else { stem }.to_string()
    }

    fn take_theirs(&mut self, theirs: String) {
        let row = self.editor.cursor.row;
        self.editor = EditorState::new(Lines::from(theirs.as_str()));
        let last = self.editor.lines.len().saturating_sub(1);
        self.editor.cursor = Index2::new(row.min(last), 0);
        prime_viewport(&mut self.editor);
        self.original = theirs;
        if self.path == self.primary_path {
            self.primary_raw = self.original.clone();
        }
        self.conflict_rev = None;
        self.error = None;
        self.pending_discard = false;
    }

    fn on_diff_key(&mut self, key: KeyEvent) -> SourceEditorOutcome {
        let Some(diff) = self.diff.as_mut() else {
            return SourceEditorOutcome::Stay;
        };
        match diff.on_key(key) {
            DiffOutcome::Stay => SourceEditorOutcome::Stay,
            DiffOutcome::Back => {
                self.diff = None;
                self.error = Some(
                    "not saved — the file changed elsewhere; Ctrl-s compares again".to_string(),
                );
                SourceEditorOutcome::Stay
            }
            DiffOutcome::TakeTheirs => {
                let theirs = self.diff.take().map(|d| d.theirs_text).unwrap_or_default();
                self.take_theirs(theirs);
                SourceEditorOutcome::Stay
            }
            DiffOutcome::KeepMine => {
                // The next save is written against the file as it is now —
                // it still fails if the file moves again before it lands.
                if let Some(diff) = self.diff.take() {
                    self.conflict_rev = Some(diff.theirs_rev);
                }
                self.error = None;
                SourceEditorOutcome::Save
            }
        }
    }

    pub fn on_key(&mut self, key: KeyEvent) -> SourceEditorOutcome {
        if self.read_only && !self.file_picker_open {
            if key.code == KeyCode::Esc {
                return SourceEditorOutcome::Close;
            }
            if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('f') {
                self.file_picker_open = true;
                self.file_picker_selected = self.current_file_index().map(|i| i + 1).unwrap_or(0);
            } else if matches!(
                key.code,
                KeyCode::Up
                    | KeyCode::Down
                    | KeyCode::Left
                    | KeyCode::Right
                    | KeyCode::PageUp
                    | KeyCode::PageDown
                    | KeyCode::Home
                    | KeyCode::End
                    | KeyCode::Char('h' | 'j' | 'k' | 'l' | 'w' | 'b' | '0' | '$' | 'G')
            ) {
                self.events.on_key_event(key, &mut self.editor);
            }
            return SourceEditorOutcome::Stay;
        }
        if self.diff.is_some() {
            return self.on_diff_key(key);
        }
        if let Some(picker) = &mut self.unicode_picker {
            match key.code {
                KeyCode::Esc => self.unicode_picker = None,
                KeyCode::Enter => {
                    if let Some(c) = picker.chosen() {
                        let mode = self.editor.mode;
                        if mode != EditorMode::Insert {
                            self.editor
                                .execute(edtui::actions::SwitchMode(EditorMode::Insert));
                        }
                        type_str(&mut self.editor, &c.to_string());
                        if mode != EditorMode::Insert {
                            self.editor.execute(edtui::actions::SwitchMode(mode));
                        }
                        self.unicode_picker = None;
                    }
                }
                _ => picker.on_key(key),
            }
            return SourceEditorOutcome::Stay;
        }
        if self.file_picker_open {
            return match self.on_file_picker_key(key) {
                Some((path, is_canvas)) => SourceEditorOutcome::Switch { path, is_canvas },
                None => SourceEditorOutcome::Stay,
            };
        }
        if self.tag_suggest_open {
            self.on_tag_suggest_key(key);
            return SourceEditorOutcome::Stay;
        }
        if self.attr_suggest_open {
            self.on_attr_suggest_key(key);
            return SourceEditorOutcome::Stay;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Char('g') => {
                    self.pending_discard = false;
                    self.unicode_picker = Some(super::unicode_picker::UnicodePicker::new());
                    return SourceEditorOutcome::Stay;
                }
                KeyCode::Char('s') => return SourceEditorOutcome::Save,
                KeyCode::Char('o') => return SourceEditorOutcome::External,
                KeyCode::Char('f') => {
                    self.file_picker_open = true;
                    self.file_picker_selected = if self.path == self.primary_path {
                        0
                    } else {
                        self.current_file_index().map(|i| i + 1).unwrap_or(0)
                    };
                    return SourceEditorOutcome::Stay;
                }
                // TODO.canvas.md: "Саджесты и подсветка синтаксиса в TUI",
                // item 3 — turns the heading the cursor's on into a node in
                // one keypress (a bare `<!-- meshfox:node -->` is already
                // enough: no id= means the parser derives one from the
                // title's own slug, see "Если не задан id...").
                KeyCode::Char('n') => {
                    match self.insert_node_below_heading() {
                        Ok(()) => self.error = None,
                        Err(msg) => self.error = Some(msg.to_string()),
                    }
                    return SourceEditorOutcome::Stay;
                }
                // Item 2 — opens the attribute-suggestion popup for
                // whatever meshfox:node/meshfox:edge/fence-attribute line
                // the cursor's currently on.
                KeyCode::Char('p') => {
                    self.open_attr_suggest();
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

    /// `vim mouse=a`-style mouse support (TODO.canvas.md: "Поддержка мыши в
    /// редакторе TUI") — click to position the cursor, drag to select
    /// (auto-switches to Visual mode, same as `on_key`'s own `v` would),
    /// scroll to move the viewport. Entirely `edtui`'s own `mouse-support`
    /// feature (already enabled by default — see `EditorEventHandler::
    /// on_mouse_event`); this is just the wiring `App::on_mouse` needed to
    /// reach it instead of falling through to the tree/document pane
    /// hit-testing underneath. No-op while the file picker overlay is open
    /// — same as `on_key`'s own picker branch, it has no mouse handling of
    /// its own yet.
    pub fn on_mouse(&mut self, mouse: MouseEvent) {
        if let Some(diff) = self.diff.as_mut() {
            diff.on_mouse(mouse);
            return;
        }
        if self.file_picker_open
            || self.attr_suggest_open
            || self.tag_suggest_open
            || self.unicode_picker.is_some()
        {
            return;
        }
        self.events.on_mouse_event(mouse, &mut self.editor);
    }

    fn current_file_index(&self) -> Option<usize> {
        self.files.iter().position(|i| i.path == self.path)
    }

    /// `Some((path, is_canvas))` when the user picked an *include target*
    /// (needs a worker fetch); the primary document is switched to here,
    /// from the copy already held.
    fn on_file_picker_key(&mut self, key: KeyEvent) -> Option<(PathBuf, bool)> {
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
                    return None;
                }
                let target = if self.file_picker_selected == 0 {
                    Some((self.primary_path.clone(), true))
                } else {
                    self.files
                        .get(self.file_picker_selected - 1)
                        .map(|i| (i.path.clone(), false))
                };
                if let Some((path, is_canvas)) = target {
                    if path == self.primary_path {
                        let raw = self.primary_raw.clone();
                        self.finish_switch(path, is_canvas, raw);
                    } else {
                        return Some((path, is_canvas));
                    }
                }
            }
            _ => {}
        }
        None
    }

    /// Loads `raw` (`path`'s text) into the editor.
    pub fn finish_switch(&mut self, path: PathBuf, is_canvas: bool, raw: String) {
        self.editor = EditorState::new(Lines::from(raw.as_str()));
        self.read_only = path != self.primary_path
            && (meshfox_core::mdcanvas::is_canvas_path(&path)
                || meshfox_core::mdcanvas::has_marker(&raw));
        self.original = raw;
        self.conflict_rev = None;
        self.path = path;
        self.is_canvas = is_canvas;
        self.error = None;
    }

    /// TODO.canvas.md: "Саджесты и подсветка синтаксиса в TUI", item 3 —
    /// `Ctrl-n`. Turns the heading the cursor's currently on into a node:
    /// appends a `<!-- meshfox:node id="..." -->` line right below it, with
    /// the same random base36 id the web UI assigns to a new node
    /// (TODO.canvas.md: "Авто Id в TUI").
    fn insert_node_below_heading(&mut self) -> Result<(), &'static str> {
        let text = self.editor.lines.to_string();
        let row = self.editor.cursor.row;
        let line = text.lines().nth(row).ok_or("no line here")?;
        if !line.trim_start().starts_with('#') {
            return Err("place the cursor on a heading line to turn it into a node");
        }
        if text
            .lines()
            .nth(row + 1)
            .is_some_and(|l| l.trim_start().starts_with("<!-- meshfox:node"))
        {
            return Err("this heading is already a node");
        }
        // `AppendNewline` inserts right below `cursor.row` regardless of
        // `cursor.col` (it resets that to 0 itself) — no repositioning
        // needed first.
        // Same random base36 id the web UI's "add child" button assigns,
        // deduped against every id the document already has; an
        // unparsable mid-edit document falls back to "no known ids".
        let used: std::collections::HashSet<String> = meshfox_core::mdcanvas::parse(&text)
            .map(|c| c.nodes.iter().map(|n| n.id.clone()).collect())
            .unwrap_or_default();
        let id = meshfox_core::mdcanvas::random_node_id(&used);
        self.editor.execute(AppendNewline(1));
        type_str(
            &mut self.editor,
            &format!("<!-- meshfox:node id=\"{id}\" -->"),
        );
        Ok(())
    }

    /// `Ctrl-p` — opens the attribute-suggestion popup for whatever
    /// `meshfox:node`/`meshfox:edge`/fence-attribute line the cursor is
    /// currently on (`detect_attr_context`), pre-filtered to only the
    /// attributes that line doesn't already have (`attr_candidates`).
    ///
    /// TODO.canvas.md: "Саджест по тегам в TUI" — but first, on a node/edge
    /// line, checks whether the cursor sits inside an already-typed
    /// `tags="..."` value (`tags_value_range`); if so this delegates to
    /// `open_tag_suggest` instead, which offers tag *values* rather than
    /// attribute names — the natural reading of "cursor is inside the tags
    /// value" as *intent* to add another tag, not to add a whole new
    /// attribute.
    fn open_attr_suggest(&mut self) {
        let text = self.editor.lines.to_string();
        let row = self.editor.cursor.row;
        let Some(line) = text.lines().nth(row) else {
            self.error = Some("no line here".to_string());
            return;
        };
        let Some(kind) = document_attr_context(&text, row) else {
            self.error = Some(
                "place the cursor on a meshfox:node/meshfox:edge comment or a fence's opening line"
                    .to_string(),
            );
            return;
        };
        if kind != AttrKind::Fence {
            if let Some((start, end)) = tags_value_range(line) {
                let col = self.editor.cursor.col;
                if col >= start && col <= end {
                    self.open_tag_suggest(line, start, end);
                    return;
                }
            }
        }
        if let Some((name, start, end)) = attribute_value_at(line, self.editor.cursor.col) {
            let values = finite_values(kind, &name);
            if !values.is_empty() {
                self.value_suggest = Some((name, start, end));
                self.tag_suggest_candidates = values.iter().map(|s| s.to_string()).collect();
                self.tag_suggest_selected = 0;
                self.tag_suggest_open = true;
                self.error = None;
                return;
            }
        }
        if inside_attribute_value(line, self.editor.cursor.col) {
            self.error = Some("no suggestions for this attribute value".to_string());
            return;
        }
        let candidates = attr_candidates(kind, line);
        if candidates.is_empty() {
            self.error = Some("every known attribute is already on this line".to_string());
            return;
        }
        self.attr_suggest_kind = kind;
        self.attr_suggest_candidates = candidates;
        self.attr_suggest_selected = 0;
        self.attr_suggest_open = true;
        self.error = None;
    }

    /// Offers every tag in `self.all_tags` not already present in the
    /// `tags="..."` value spanning `[value_start, value_end)` (char
    /// columns) on `line`.
    fn open_tag_suggest(&mut self, line: &str, value_start: usize, value_end: usize) {
        let value: String = line
            .chars()
            .skip(value_start)
            .take(value_end - value_start)
            .collect();
        let present: std::collections::HashSet<&str> = value
            .split(',')
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .collect();
        let candidates: Vec<String> = self
            .all_tags
            .iter()
            .filter(|t| !present.contains(t.as_str()))
            .cloned()
            .collect();
        if candidates.is_empty() {
            self.error = Some("no other tags used elsewhere in the document".to_string());
            return;
        }
        self.value_suggest = None;
        self.tag_suggest_candidates = candidates;
        self.tag_suggest_selected = 0;
        self.tag_suggest_open = true;
        self.error = None;
    }

    pub fn value_suggest_label(&self) -> Option<&str> {
        self.value_suggest
            .as_ref()
            .map(|(name, _, _)| name.as_str())
    }

    fn on_tag_suggest_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.tag_suggest_selected = self.tag_suggest_selected.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.tag_suggest_selected = (self.tag_suggest_selected + 1)
                    .min(self.tag_suggest_candidates.len().saturating_sub(1));
            }
            KeyCode::Esc => self.tag_suggest_open = false,
            KeyCode::Enter => {
                self.tag_suggest_open = false;
                if let Some(tag) = self
                    .tag_suggest_candidates
                    .get(self.tag_suggest_selected)
                    .cloned()
                {
                    if let Some((_, start, end)) = self.value_suggest.take() {
                        self.editor.cursor.col = start;
                        if end > start {
                            self.editor
                                .execute(edtui::actions::DeleteCharForward(end - start));
                        }
                        type_str(&mut self.editor, &tag);
                    } else {
                        self.insert_tag_candidate(&tag);
                    }
                }
            }
            _ => {}
        }
    }

    /// Inserts `tag` into the current line's `tags="..."` value, right
    /// before the closing quote — independent of the cursor's own exact
    /// sub-position within the value, same "the line matters, not the
    /// column" choice `insert_attr_candidate` already makes. A leading
    /// comma is added only if the value isn't already empty or
    /// comma-terminated; a trailing comma always follows, so the cursor
    /// lands ready to pick (or type) the next tag — mirroring what
    /// clicking a suggestion in the web UI's tag editor does. The trailing
    /// comma is harmless if left dangling (`parse_tags` filters out empty
    /// entries), so there's no need to clean it up on save.
    fn insert_tag_candidate(&mut self, tag: &str) {
        let text = self.editor.lines.to_string();
        let row = self.editor.cursor.row;
        let Some(line) = text.lines().nth(row) else {
            return;
        };
        let Some((start, end)) = tags_value_range(line) else {
            return;
        };
        let value: String = line.chars().skip(start).take(end - start).collect();
        let needs_leading_comma = !value.trim_end().is_empty() && !value.trim_end().ends_with(',');
        self.editor.cursor = Index2::new(row, end);
        let insertion = if needs_leading_comma {
            format!(",{tag},")
        } else {
            format!("{tag},")
        };
        type_str(&mut self.editor, &insertion);
    }

    /// Which vocabulary the currently-open popup is suggesting from — for
    /// `ui::render_attr_suggest_popup`'s own title.
    pub fn attr_suggest_label(&self) -> &'static str {
        self.attr_suggest_kind.label()
    }

    fn on_attr_suggest_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.attr_suggest_selected = self.attr_suggest_selected.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.attr_suggest_selected = (self.attr_suggest_selected + 1)
                    .min(self.attr_suggest_candidates.len().saturating_sub(1));
            }
            KeyCode::Esc => self.attr_suggest_open = false,
            KeyCode::Enter => {
                self.attr_suggest_open = false;
                if let Some(candidate) = self
                    .attr_suggest_candidates
                    .get(self.attr_suggest_selected)
                    .copied()
                {
                    self.insert_attr_candidate(candidate);
                }
            }
            _ => {}
        }
    }

    /// Types `candidate` in at the right spot for `self.attr_suggest_kind`
    /// — just before a `meshfox:node`/`meshfox:edge` comment's closing
    /// `-->` (typing it at the raw end of line would land *after* the
    /// comment closes, breaking it), or at the true end of a fence's own
    /// attribute line. `x`/`y`/`w`/`h` get a bare `=0` (the on-disk
    /// convention every existing coordinate already uses — see `DOC` in
    /// `mdcanvas.rs`'s own tests — not a quoted string); every other
    /// value-taking attribute gets `=""` with the cursor left between the
    /// quotes, ready to type; a bare flag (fence `cache`/`tty`/`default`)
    /// gets just its own word, nothing to fill in.
    fn insert_attr_candidate(&mut self, candidate: AttrCandidate) {
        let text = self.editor.lines.to_string();
        let row = self.editor.cursor.row;
        let Some(line) = text.lines().nth(row) else {
            return;
        };
        let col = insertion_col(line, self.attr_suggest_kind);
        self.editor.cursor = Index2::new(row, col);
        if candidate.is_flag {
            type_str(&mut self.editor, &format!(" {}", candidate.name));
        } else if matches!(candidate.name, "x" | "y" | "w" | "h") {
            type_str(&mut self.editor, &format!(" {}=0", candidate.name));
        } else {
            type_str(&mut self.editor, &format!(" {}=\"\"", candidate.name));
            self.editor.cursor.col -= 1; // land between the quotes
        }
    }
}

/// Types `s` in at the cursor, one character at a time, via `edtui`'s own
/// public `InsertChar` action — the same path a real keystroke takes (see
/// `EditorEventHandler::on_key_event`), so it lands in the same undo/
/// dot-repeat machinery, rather than reconstructing and replacing the
/// whole buffer.
fn type_str(editor: &mut EditorState, s: &str) {
    for c in s.chars() {
        editor.execute(InsertChar(c));
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
    use meshfox_core::suggest::detect_attr_context;

    fn open_editor(raw: &str) -> SourceEditorState {
        let path = PathBuf::from("/tmp/meshfox-source-editor-test.canvas.md");
        SourceEditorState::open(
            path.clone(),
            path,
            true,
            Index2::new(0, 0),
            Vec::new(),
            Vec::new(),
            raw.to_string(),
            raw.to_string(),
        )
        .unwrap()
    }

    #[test]
    fn included_canvas_blocks_edit_keys_and_save_but_keeps_file_switching() {
        let mut editor = open_editor("# Primary\n");
        for (name, raw) in [
            ("child.canvas.md", "# Child\n"),
            ("child.md", "<!-- meshfox:canvas -->\n# Child\n"),
        ] {
            editor.finish_switch(PathBuf::from(name), false, raw.into());
            assert!(editor.read_only);
            for key in [
                KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE),
                KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE),
                KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL),
                KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL),
            ] {
                assert!(matches!(editor.on_key(key), SourceEditorOutcome::Stay));
                assert_eq!(editor.editor.lines.to_string(), raw);
            }
            editor.on_key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL));
            assert!(editor.file_picker_open);
            editor.file_picker_open = false;
        }
        editor.finish_switch(PathBuf::from("notes.md"), false, "plain prose".into());
        assert!(!editor.read_only);
        editor.finish_switch(editor.primary_path.clone(), true, "# Primary\n".into());
        assert!(!editor.read_only);
    }

    #[test]
    fn a_save_is_based_on_the_text_the_editor_opened_with() {
        let editor = open_editor("# Root\n");
        assert_eq!(editor.base_rev(), meshfox_core::body_rev("# Root\n"));
    }

    #[test]
    fn keeping_mine_after_a_conflict_saves_against_what_the_file_is_now() {
        let mut editor = open_editor("# Root\n");
        editor.open_diff("# Root, theirs\n".to_string(), "abc123".to_string(), &[]);
        assert!(editor.diff.is_some());
        let outcome = editor.on_key(KeyEvent::new(KeyCode::Char('m'), KeyModifiers::NONE));
        assert!(matches!(outcome, SourceEditorOutcome::Save));
        assert!(editor.diff.is_none());
        assert_eq!(editor.base_rev(), "abc123");
        // Saving (or switching file) starts over from the saved text.
        editor.mark_saved();
        assert_eq!(editor.base_rev(), meshfox_core::body_rev("# Root\n"));
    }

    #[test]
    fn taking_theirs_replaces_the_buffer_and_leaves_nothing_to_save() {
        let mut editor = open_editor("# Root\n");
        editor.open_diff("# Root, theirs\n".to_string(), "abc123".to_string(), &[]);
        editor.on_key(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::NONE));
        assert!(editor.diff.is_none());
        assert_eq!(editor.editor.lines.to_string(), "# Root, theirs\n");
        assert!(!editor.dirty());
        assert_eq!(
            editor.base_rev(),
            meshfox_core::body_rev("# Root, theirs\n")
        );
    }

    #[test]
    fn going_back_from_the_comparison_keeps_the_buffer_and_a_note() {
        let mut editor = open_editor("# Root\n");
        editor.open_diff("# Root, theirs\n".to_string(), "abc123".to_string(), &[]);
        let outcome = editor.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(matches!(outcome, SourceEditorOutcome::Stay));
        assert!(editor.diff.is_none());
        assert_eq!(editor.editor.lines.to_string(), "# Root\n");
        assert!(editor
            .error
            .as_deref()
            .is_some_and(|e| e.contains("not saved")));
        // Nothing was decided: the next save is still against what was read.
        assert_eq!(editor.base_rev(), meshfox_core::body_rev("# Root\n"));
    }

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
        EditorView::new(&mut editor)
            .wrap(true)
            .render(area, &mut buf);

        assert!(
            editor.cursor_screen_position().is_some(),
            "cursor should already be on screen on the first real render"
        );
        assert!(
            editor.viewport_offset().1 > 0,
            "viewport should already have scrolled down to the cursor, not stayed at the top"
        );
    }

    // TODO.canvas.md: "Поддержка мыши в редакторе TUI" — `on_mouse` is
    // mostly just wiring onto `edtui`'s own already-working mouse support
    // (see `on_mouse`'s own doc comment); what these two tests actually
    // cover is the wiring itself (crossterm's `MouseEvent` reaches
    // `EditorState` correctly through it) and the one guard this file adds
    // on top (no mouse handling while the file picker overlay is open).
    #[test]
    fn unicode_picker_inserts_at_cursor_and_undoes_in_normal_and_insert_modes() {
        for insert_mode in [false, true] {
            let mut se = open_test_editor("ab");
            se.editor.cursor = Index2::new(0, 1);
            if insert_mode {
                se.on_key(KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE));
            }
            se.on_key(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL));
            for c in "U+221E".chars() {
                se.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
            }
            se.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
            assert!(se.unicode_picker.is_none());
            assert_eq!(se.editor.lines.to_string(), "a∞b");
            if insert_mode {
                se.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
            }
            se.on_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::NONE));
            assert_eq!(se.editor.lines.to_string(), "ab");
        }
    }

    #[test]
    fn unicode_picker_cancel_and_empty_enter_leave_editor_untouched() {
        let mut se = open_test_editor("ab");
        se.on_key(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL));
        for c in "U+D800".chars() {
            se.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        se.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(se.unicode_picker.is_some());
        se.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(se.unicode_picker.is_none());
        assert_eq!(se.editor.lines.to_string(), "ab");
        assert!(!se.dirty());
    }

    fn open_test_editor(text: &str) -> SourceEditorState {
        open_test_editor_with_tags(text, Vec::new())
    }

    fn open_test_editor_with_tags(text: &str, all_tags: Vec<String>) -> SourceEditorState {
        use edtui::EditorView;
        use ratatui::{buffer::Buffer, layout::Rect, widgets::Widget};

        let path = std::env::temp_dir().join(format!(
            "meshfox-source-editor-mouse-test-{}.md",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(&path, text).unwrap();
        let mut se = SourceEditorState::open(
            path.clone(),
            path,
            false,
            Index2::new(0, 0),
            Vec::new(),
            all_tags,
            text.to_string(),
            text.to_string(),
        )
        .unwrap();
        // Same requirement `prime_viewport`'s own test documents: a real
        // render populates `screen_area`/`viewport`, which mouse
        // coordinate mapping needs.
        let area = Rect::new(0, 0, 80, 22);
        let mut buf = Buffer::empty(area);
        EditorView::new(&mut se.editor)
            .wrap(true)
            .render(area, &mut buf);
        se
    }

    fn left_click_at(row: u16, column: u16) -> MouseEvent {
        use crossterm::event::MouseEventKind;
        MouseEvent {
            kind: MouseEventKind::Down(crossterm::event::MouseButton::Left),
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    #[test]
    fn on_mouse_positions_the_cursor_on_a_left_click() {
        let mut se = open_test_editor("abc\ndef\nghi");
        assert_eq!(se.editor.cursor, Index2::new(0, 0));

        se.on_mouse(left_click_at(1, 1));

        assert_eq!(se.editor.cursor, Index2::new(1, 1));
        let _ = std::fs::remove_file(&se.path);
    }

    #[test]
    fn on_mouse_does_nothing_while_the_file_picker_is_open() {
        let mut se = open_test_editor("abc\ndef\nghi");
        se.file_picker_open = true;

        se.on_mouse(left_click_at(1, 1));

        assert_eq!(se.editor.cursor, Index2::new(0, 0));
        let _ = std::fs::remove_file(&se.path);
    }

    // TODO.canvas.md: "Саджесты и подсветка синтаксиса в TUI", item 3.
    #[test]
    fn ctrl_n_turns_a_heading_into_a_node() {
        let mut se = open_test_editor("# Root\n\n## Section\n\nbody\n");
        se.editor.cursor = Index2::new(2, 0); // "## Section"

        se.insert_node_below_heading().unwrap();

        let text = se.editor.lines.to_string();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[2], "## Section");
        let id = lines[3]
            .strip_prefix("<!-- meshfox:node id=\"")
            .and_then(|l| l.strip_suffix("\" -->"))
            .expect("an id-bearing node comment");
        assert!(
            !id.is_empty()
                && id
                    .chars()
                    .all(|c| c.is_ascii_digit() || c.is_ascii_lowercase())
        );
        let _ = std::fs::remove_file(&se.path);
    }

    #[test]
    fn ctrl_n_refuses_off_a_non_heading_line() {
        let mut se = open_test_editor("# Root\n\nplain body text\n");
        se.editor.cursor = Index2::new(2, 0);

        let err = se.insert_node_below_heading().unwrap_err();

        assert!(err.contains("heading"));
        assert_eq!(se.editor.lines.to_string(), "# Root\n\nplain body text\n");
        let _ = std::fs::remove_file(&se.path);
    }

    #[test]
    fn ctrl_n_refuses_a_heading_thats_already_a_node() {
        let mut se = open_test_editor("# Root\n<!-- meshfox:node id=\"root\" -->\n\nbody\n");
        se.editor.cursor = Index2::new(0, 0);

        let err = se.insert_node_below_heading().unwrap_err();

        assert!(err.contains("already a node"));
        let _ = std::fs::remove_file(&se.path);
    }

    // TODO.canvas.md: "Саджесты и подсветка синтаксиса в TUI", item 2.
    #[test]
    fn finite_value_suggestions_replace_values_and_preserve_surroundings() {
        for (line, value, expected) in [
            (
                "<!-- meshfox:node type=\"\" color=\"red\" -->",
                "file",
                "<!-- meshfox:node type=\"file\" color=\"red\" -->",
            ),
            (
                "<!-- meshfox:node type='текст' -->",
                "group",
                "<!-- meshfox:node type='group' -->",
            ),
            (
                "<!-- meshfox:edge style=dashed -->",
                "solid",
                "<!-- meshfox:edge style=solid -->",
            ),
            (
                "```sh output=\"text\" cache",
                "image",
                "```sh output=\"image\" cache",
            ),
            ("```sh render=\"\"", "form", "```sh render=\"form\""),
            (
                "<!-- meshfox:node preview=\"\" -->",
                "false",
                "<!-- meshfox:node preview=\"false\" -->",
            ),
        ] {
            for mode in [EditorMode::Normal, EditorMode::Insert] {
                let mut se = open_editor(line);
                se.editor.mode = mode;
                let eq = line.find('=').unwrap();
                se.editor.cursor.col = line[..eq + 1].chars().count()
                    + usize::from(matches!(line.as_bytes().get(eq + 1), Some(b'\'' | b'"')));
                se.open_attr_suggest();
                assert!(se.tag_suggest_open, "{line}");
                se.tag_suggest_selected = se
                    .tag_suggest_candidates
                    .iter()
                    .position(|v| v == value)
                    .unwrap();
                se.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
                assert_eq!(se.editor.lines.to_string(), expected);
            }
        }
    }

    #[test]
    fn finite_value_popup_cancels_without_editing_and_respects_context() {
        let line = "<!-- meshfox:node type=\"\" -->";
        let mut se = open_editor(line);
        se.editor.cursor.col = line.find("\"\"").unwrap() + 1;
        se.open_attr_suggest();
        assert_eq!(
            se.tag_suggest_candidates,
            ["text", "file", "link", "group", "include"]
        );
        se.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(!se.tag_suggest_open);
        assert_eq!(se.editor.lines.to_string(), line);
        assert!(finite_values(AttrKind::Edge, "type").is_empty());
        assert!(finite_values(AttrKind::Node, "id").is_empty());
        assert_eq!(
            attribute_value_at("type=\"file\"", 8),
            Some(("type".into(), 6, 10))
        );
    }

    #[test]
    fn suggestions_ignore_fence_bodies_closers_and_non_runnable_languages() {
        let text = "```bash\n<!-- meshfox:node -->\n```\n```json\n{}\n```\n~~~sh\necho ok\n~~~";
        assert_eq!(document_attr_context(text, 0), Some(AttrKind::Fence));
        for row in [1, 2, 3, 4, 5, 7, 8] {
            assert_eq!(document_attr_context(text, row), None, "row {row}");
        }
        assert_eq!(document_attr_context(text, 6), Some(AttrKind::Fence));
    }

    #[test]
    fn suggestions_do_not_offer_attribute_names_inside_values() {
        for line in [
            "<!-- meshfox:node color=\"red\" -->",
            "```sh env='FOO'",
            "```sh env=FOO",
        ] {
            let col = line.find("red").or_else(|| line.find("FOO")).unwrap() + 1;
            assert!(inside_attribute_value(line, col));
        }
        let mut se = open_editor("<!-- meshfox:node color=\"red\" -->");
        se.editor.cursor.col = 26;
        se.open_attr_suggest();
        assert!(!se.attr_suggest_open);
    }

    #[test]
    fn fence_suggestions_respect_flag_conflicts_and_exact_attribute_names() {
        let names = |line| {
            attr_candidates(AttrKind::Fence, line)
                .into_iter()
                .map(|c| c.name)
                .collect::<Vec<_>>()
        };
        let tty = names("```sh tty=\"true\"");
        assert!(!tty.contains(&"cache"));
        assert!(!tty.contains(&"service"));
        assert!(!tty.contains(&"tty"));
        assert!(tty.contains(&"autoclose"));
        assert!(!names("```sh").contains(&"autoclose"));
        assert!(names("```sh tty=false").contains(&"cache"));
        // `tangle` only means something with an `interpreter=` to run the file.
        assert!(!names("```python name=\"x\"").contains(&"tangle"));
        assert!(names("```python name=\"x\" interpreter=\"python3\"").contains(&"tangle"));
        assert!(
            !names("```python interpreter=\"python3\" tangle").contains(&"tangle"),
            "an attribute already present is not offered again"
        );
        let candidates = attr_candidates(AttrKind::Node, "<!-- meshfox:node tags=\"id=fake\" -->");
        assert!(candidates.iter().any(|c| c.name == "id"));
    }

    #[test]
    fn detect_attr_context_recognizes_node_edge_and_fence_lines() {
        assert_eq!(
            detect_attr_context("<!-- meshfox:node id=\"root\" -->"),
            Some(AttrKind::Node)
        );
        assert_eq!(
            detect_attr_context("<!-- meshfox:edge from=\"root\" -->"),
            Some(AttrKind::Edge)
        );
        assert_eq!(
            detect_attr_context("```bash name=\"build\""),
            Some(AttrKind::Fence)
        );
        assert_eq!(detect_attr_context("just a body line"), None);
    }

    #[test]
    fn attr_candidates_excludes_attributes_already_on_the_line() {
        let names: Vec<&str> =
            attr_candidates(AttrKind::Node, "<!-- meshfox:node id=\"root\" x=0 -->")
                .iter()
                .map(|c| c.name)
                .collect();
        assert!(!names.contains(&"id"));
        assert!(!names.contains(&"x")); // bare-numeric form is still detected
        assert!(names.contains(&"color"));
        assert!(names.contains(&"y"));
    }

    #[test]
    fn attr_candidates_for_a_fence_separates_flags_from_value_attrs() {
        let candidates = attr_candidates(AttrKind::Fence, "```bash");
        let cache = candidates
            .iter()
            .find(|c| c.name == "cache")
            .expect("cache offered");
        assert!(cache.is_flag);
        let name = candidates
            .iter()
            .find(|c| c.name == "name")
            .expect("name offered");
        assert!(!name.is_flag);
    }

    #[test]
    fn attr_candidates_for_a_fence_excludes_a_flag_already_present() {
        let candidates = attr_candidates(AttrKind::Fence, "```bash cache");
        assert!(!candidates.iter().any(|c| c.name == "cache"));
        assert!(!candidates.iter().any(|c| c.name == "tty"));
    }

    #[test]
    fn insertion_col_lands_right_before_the_closing_arrow() {
        assert_eq!(
            insertion_col("<!-- meshfox:node id=\"root\" -->", AttrKind::Node),
            27 // right after the closing quote of id="root", before " -->"
        );
        assert_eq!(insertion_col("```bash name=\"build\"", AttrKind::Fence), 20);
    }

    #[test]
    fn open_attr_suggest_and_insert_attr_candidate_end_to_end() {
        let mut se = open_test_editor("<!-- meshfox:node id=\"root\" -->\n");
        se.editor.cursor = Index2::new(0, 0);

        se.open_attr_suggest();
        assert!(se.attr_suggest_open);
        assert!(!se.attr_suggest_candidates.iter().any(|c| c.name == "id"));

        let color_idx = se
            .attr_suggest_candidates
            .iter()
            .position(|c| c.name == "color")
            .expect("color offered");
        se.attr_suggest_selected = color_idx;
        se.on_attr_suggest_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        assert!(!se.attr_suggest_open);
        assert_eq!(
            se.editor.lines.to_string(),
            "<!-- meshfox:node id=\"root\" color=\"\" -->\n"
        );
        // Cursor landed between the new attribute's quotes.
        assert_eq!(se.editor.cursor, Index2::new(0, 35));
        let _ = std::fs::remove_file(&se.path);
    }

    #[test]
    fn insert_attr_candidate_gives_x_y_w_h_a_bare_numeric_default() {
        let mut se = open_test_editor("<!-- meshfox:node id=\"root\" -->\n");
        se.insert_attr_candidate(AttrCandidate {
            name: "x",
            is_flag: false,
        });

        assert_eq!(
            se.editor.lines.to_string(),
            "<!-- meshfox:node id=\"root\" x=0 -->\n"
        );
        let _ = std::fs::remove_file(&se.path);
    }

    // TODO.canvas.md: "Саджест по тегам в TUI" — `Ctrl-p`'s tag-value
    // flavor, taken when the cursor sits inside an already-typed
    // `tags="..."` value rather than on the rest of the line.
    #[test]
    fn tags_value_range_finds_the_value_between_the_quotes() {
        let line = "<!-- meshfox:node id=\"root\" tags=\"bag,improvement\" -->";
        let (start, end) = tags_value_range(line).expect("tags= present");
        let value: String = line.chars().skip(start).take(end - start).collect();
        assert_eq!(value, "bag,improvement");
    }

    #[test]
    fn tags_value_range_is_none_without_a_tags_attribute() {
        assert_eq!(tags_value_range("<!-- meshfox:node id=\"root\" -->"), None);
    }

    #[test]
    fn open_attr_suggest_offers_tag_values_when_the_cursor_sits_inside_the_tags_value() {
        let line = "<!-- meshfox:node id=\"root\" tags=\"bag\" -->\n";
        let mut se = open_test_editor_with_tags(
            line,
            vec![
                "bag".to_string(),
                "improvement".to_string(),
                "docs".to_string(),
            ],
        );
        let (start, end) = tags_value_range(line.trim_end()).unwrap();
        se.editor.cursor = Index2::new(0, (start + end) / 2);

        se.open_attr_suggest();

        assert!(se.tag_suggest_open);
        assert!(!se.attr_suggest_open);
        assert!(!se.tag_suggest_candidates.contains(&"bag".to_string())); // already on this node
        assert!(se
            .tag_suggest_candidates
            .contains(&"improvement".to_string()));
        assert!(se.tag_suggest_candidates.contains(&"docs".to_string()));
        let _ = std::fs::remove_file(&se.path);
    }

    #[test]
    fn open_attr_suggest_still_offers_attribute_names_when_the_cursor_is_outside_the_tags_value() {
        let mut se = open_test_editor_with_tags(
            "<!-- meshfox:node id=\"root\" tags=\"bag\" -->\n",
            vec!["bag".to_string(), "improvement".to_string()],
        );
        se.editor.cursor = Index2::new(0, 0); // well before `tags=`

        se.open_attr_suggest();

        assert!(se.attr_suggest_open);
        assert!(!se.tag_suggest_open);
        let _ = std::fs::remove_file(&se.path);
    }

    #[test]
    fn insert_tag_candidate_appends_with_a_leading_comma_when_the_value_is_non_empty() {
        let mut se = open_test_editor("<!-- meshfox:node id=\"root\" tags=\"bag\" -->\n");
        se.insert_tag_candidate("improvement");

        assert_eq!(
            se.editor.lines.to_string(),
            "<!-- meshfox:node id=\"root\" tags=\"bag,improvement,\" -->\n"
        );
        let _ = std::fs::remove_file(&se.path);
    }

    #[test]
    fn insert_tag_candidate_omits_the_leading_comma_for_an_empty_value() {
        let mut se = open_test_editor("<!-- meshfox:node id=\"root\" tags=\"\" -->\n");
        se.insert_tag_candidate("bag");

        assert_eq!(
            se.editor.lines.to_string(),
            "<!-- meshfox:node id=\"root\" tags=\"bag,\" -->\n"
        );
        let _ = std::fs::remove_file(&se.path);
    }

    #[test]
    fn open_tag_suggest_and_insert_tag_candidate_end_to_end() {
        let line = "<!-- meshfox:node id=\"root\" tags=\"bag\" -->\n";
        let mut se =
            open_test_editor_with_tags(line, vec!["bag".to_string(), "improvement".to_string()]);
        let (start, _end) = tags_value_range(line.trim_end()).unwrap();
        se.editor.cursor = Index2::new(0, start);

        se.open_attr_suggest();
        assert!(se.tag_suggest_open);
        assert_eq!(se.tag_suggest_candidates, vec!["improvement".to_string()]);

        se.on_tag_suggest_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        assert!(!se.tag_suggest_open);
        assert_eq!(
            se.editor.lines.to_string(),
            "<!-- meshfox:node id=\"root\" tags=\"bag,improvement,\" -->\n"
        );
        let _ = std::fs::remove_file(&se.path);
    }
}
