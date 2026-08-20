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
use edtui::{EditorEventHandler, EditorMode, EditorState, Highlight, Index2, Lines};
use meshfox_core::include::IncludeInfo;
use meshfox_core::mdcanvas::{EDGE_ATTRS, NODE_ATTRS};
use ratatui::style::{Color, Modifier, Style};

/// A runnable fence's own attribute vocabulary (`crates/core/src/fence.rs`)
/// — value-taking (`name="..."`) vs. bare presence flags (`cache`, no
/// `=value` at all) need different insertion shapes, see `AttrCandidate`.
const FENCE_VALUE_ATTRS: &[&str] = &["name", "deps", "env", "interpreter"];
const FENCE_FLAG_ATTRS: &[&str] = &["cache", "tty", "autoclose", "always", "default"];

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
    pub tag_suggest_open: bool,
    pub tag_suggest_candidates: Vec<String>,
    pub tag_suggest_selected: usize,
}

/// Which attribute vocabulary a suggestible line belongs to — see
/// `detect_attr_context`/`attr_candidates`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AttrKind {
    Node,
    Edge,
    Fence,
}

impl AttrKind {
    fn label(self) -> &'static str {
        match self {
            AttrKind::Node => "node",
            AttrKind::Edge => "edge",
            AttrKind::Fence => "fence",
        }
    }
}

/// One entry in the `Ctrl-p` popup. `is_flag` distinguishes a bare
/// presence flag (a runnable fence's `cache`/`tty`/`default` — inserted as
/// just the bare word) from a `key="value"`-shaped attribute (every
/// `meshfox:node`/`meshfox:edge` attribute, plus a fence's own
/// `name`/`deps`/`env` — inserted as `key=""` with the cursor landing
/// between the quotes, ready to type the value).
#[derive(Debug, Clone, Copy)]
pub struct AttrCandidate {
    pub name: &'static str,
    pub is_flag: bool,
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
        all_tags: Vec<String>,
    ) -> std::io::Result<Self> {
        let raw = std::fs::read_to_string(&path)?;
        let mut editor = EditorState::new(Lines::from(raw.as_str()));
        editor.cursor = cursor;
        editor.highlights = meshfox_highlights(&raw);
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
            attr_suggest_open: false,
            attr_suggest_kind: AttrKind::Node,
            attr_suggest_candidates: Vec::new(),
            attr_suggest_selected: 0,
            all_tags,
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
    pub fn mark_saved(&mut self) {
        self.original = self.editor.lines.to_string();
    }

    pub fn on_key(&mut self, key: KeyEvent) -> SourceEditorOutcome {
        if self.file_picker_open {
            self.on_file_picker_key(key);
            return SourceEditorOutcome::Stay;
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
        self.refresh_meshfox_highlights();
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
        if self.file_picker_open || self.attr_suggest_open || self.tag_suggest_open {
            return;
        }
        self.events.on_mouse_event(mouse, &mut self.editor);
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
        self.editor.highlights = meshfox_highlights(&raw);
        self.original = raw;
        self.path = path;
        self.is_canvas = is_canvas;
        self.error = None;
    }

    /// Recomputes `self.editor.highlights` from the buffer's current text
    /// — called after anything that could have changed it (a real
    /// keystroke, an attribute/node-comment insertion below), so the
    /// meshfox-specific coloring (see `meshfox_highlights`) never goes
    /// stale relative to what's actually on screen.
    fn refresh_meshfox_highlights(&mut self) {
        self.editor.highlights = meshfox_highlights(&self.editor.lines.to_string());
    }

    /// TODO.canvas.md: "Саджесты и подсветка синтаксиса в TUI", item 3 —
    /// `Ctrl-n`. Turns the heading the cursor's currently on into a node:
    /// appends a bare `<!-- meshfox:node -->` line right below it. Bare is
    /// deliberate, not a shortcut: no `id=` means the parser derives one
    /// from the heading's own title slug (see "Если не задан id в
    /// meshfox:node, использовать заоголовок"), so this one line is
    /// already a fully valid, addressable node — nothing further to fill
    /// in unless the user wants to override something.
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
        self.editor.execute(AppendNewline(1));
        type_str(&mut self.editor, "<!-- meshfox:node -->");
        self.refresh_meshfox_highlights();
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
        let Some(kind) = detect_attr_context(line) else {
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
        let value: String = line.chars().skip(value_start).take(value_end - value_start).collect();
        let present: std::collections::HashSet<&str> =
            value.split(',').map(str::trim).filter(|t| !t.is_empty()).collect();
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
        self.tag_suggest_candidates = candidates;
        self.tag_suggest_selected = 0;
        self.tag_suggest_open = true;
        self.error = None;
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
                if let Some(tag) = self.tag_suggest_candidates.get(self.tag_suggest_selected).cloned() {
                    self.insert_tag_candidate(&tag);
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
        self.refresh_meshfox_highlights();
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
        self.refresh_meshfox_highlights();
    }
}

/// Where `insert_attr_candidate` types a new attribute in `line` — right
/// before a node/edge comment's closing `-->` (so the new attribute lands
/// *inside* the comment, not after it), or the plain end of line for a
/// fence's own attribute line (nothing closes it).
fn insertion_col(line: &str, kind: AttrKind) -> usize {
    if kind != AttrKind::Fence {
        if let Some(close) = line.rfind("-->") {
            return line[..close].trim_end().chars().count();
        }
    }
    line.chars().count()
}

/// Locates a `tags="..."` attribute's value on `line`, as a char-column
/// range — `start` right after the opening quote, `end` at the closing
/// quote (so `end - start` chars sit strictly between them; `start ==
/// end` for an empty value, `tags=""`). `None` if the line has no `tags=`
/// at all, or the value's closing quote is missing. Char columns, not byte
/// offsets — matches `insertion_col`/`Index2` elsewhere in this file.
fn tags_value_range(line: &str) -> Option<(usize, usize)> {
    let key = "tags=\"";
    let start_byte = line.find(key)?;
    let value_start_byte = start_byte + key.len();
    let rest = &line[value_start_byte..];
    let close_byte_in_rest = rest.find('"')?;
    let start_col = line[..value_start_byte].chars().count();
    let end_col = start_col + rest[..close_byte_in_rest].chars().count();
    Some((start_col, end_col))
}

/// Which attribute vocabulary (if any) the cursor's current line belongs
/// to — a `meshfox:node`/`meshfox:edge` comment, or a fence's own opening
/// line (any line starting with a code-fence delimiter; a closing fence
/// has no attributes to suggest, but detecting the difference isn't worth
/// the complexity here — `attr_candidates` on one just offers every fence
/// attribute, harmlessly unused if the popup's dismissed).
fn detect_attr_context(line: &str) -> Option<AttrKind> {
    let trimmed = line.trim_start();
    if trimmed.starts_with("<!-- meshfox:node") {
        Some(AttrKind::Node)
    } else if trimmed.starts_with("<!-- meshfox:edge") {
        Some(AttrKind::Edge)
    } else if trimmed.starts_with("```") {
        Some(AttrKind::Fence)
    } else {
        None
    }
}

/// `kind`'s full attribute vocabulary, minus whatever `line` already has —
/// a value attribute counts as present if `"name="` appears anywhere in
/// the line (covers both `id="..."` and the bare-numeric `x=0` form); a
/// flag attribute counts as present if it appears as its own whitespace-
/// delimited word. Best-effort, same spirit as `meshfox_highlights` below
/// — a suggestion list, not the real parser.
fn attr_candidates(kind: AttrKind, line: &str) -> Vec<AttrCandidate> {
    let (value_attrs, flag_attrs): (&[&str], &[&str]) = match kind {
        AttrKind::Node => (NODE_ATTRS, &[]),
        AttrKind::Edge => (EDGE_ATTRS, &[]),
        AttrKind::Fence => (FENCE_VALUE_ATTRS, FENCE_FLAG_ATTRS),
    };
    let value_present = |name: &str| line.contains(&format!("{name}="));
    let flag_present = |name: &str| line.split_whitespace().any(|tok| tok == name);
    value_attrs
        .iter()
        .filter(|n| !value_present(n))
        .map(|&name| AttrCandidate { name, is_flag: false })
        .chain(
            flag_attrs
                .iter()
                .filter(|n| !flag_present(n))
                .map(|&name| AttrCandidate { name, is_flag: true }),
        )
        .collect()
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

/// TODO.canvas.md: "Саджесты и подсветка синтаксиса в TUI", item 1 —
/// highlights every `<!-- meshfox:... -->`/`<!-- /meshfox:... -->` marker
/// comment (the whole thing, one bold accent color) plus each `key=`
/// attribute name inside one (a second color), on top of whatever the
/// buffer's ordinary Markdown syntax highlighting already does (custom
/// `EditorState::highlights` are applied after the syntax highlighter's
/// own base styling — see `edtui`'s own render pipeline — so these always
/// win). Recomputed from scratch on every call rather than incrementally
/// — cheap enough (a handful of linear string scans) that there's no
/// reason to track what changed.
///
/// Scoped to `meshfox:` comment lines only, not a runnable fence's own
/// `name="..."`/`cache` attribute line — code-fence content already reads
/// visually distinct (its own syntax highlighting, different indentation
/// context), and parsing an attribute line's exact boundaries safely
/// alongside arbitrary fence-language syntax highlighting underneath adds
/// real risk for comparatively little extra clarity; the `Ctrl-p`
/// suggestion popup still covers fence attributes even though this
/// doesn't highlight them.
fn meshfox_highlights(text: &str) -> Vec<Highlight> {
    let marker_style = Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD);
    let key_style = Style::default().fg(Color::LightCyan);

    let mut highlights = Vec::new();
    let mut line_start = 0usize;
    for line in text.split_inclusive('\n') {
        let content = line.strip_suffix('\n').unwrap_or(line);
        if let (Some(open), Some(close)) = (content.find("<!--"), content.rfind("-->")) {
            let comment_end = close + "-->".len();
            if open < close && content[open..comment_end].contains("meshfox:") {
                highlights.push(Highlight::new(
                    byte_offset_to_cursor(text, line_start + open),
                    byte_offset_to_cursor(text, line_start + comment_end - 1),
                    marker_style,
                ));

                let inner_start = open + "<!--".len();
                let inner = &content[inner_start..close];
                for (rel, token) in whitespace_tokens_with_offsets(inner) {
                    let Some(eq) = token.find('=') else { continue };
                    let key = &token[..eq];
                    if key.is_empty() || !key.chars().all(|c| c.is_ascii_alphabetic()) {
                        continue;
                    }
                    let key_start = line_start + inner_start + rel;
                    let key_end = key_start + key.len();
                    highlights.push(Highlight::new(
                        byte_offset_to_cursor(text, key_start),
                        byte_offset_to_cursor(text, key_end - 1),
                        key_style,
                    ));
                }
            }
        }
        line_start += line.len();
    }
    highlights
}

/// Whitespace-delimited tokens in `s`, each paired with its own byte
/// offset from the start of `s` — `str::split_whitespace` alone doesn't
/// give offsets, and re-`find`ing each token back in `s` (the obvious
/// alternative) breaks the moment the same token appears twice. Safe to
/// slice with (never lands mid-character): every boundary here comes from
/// `char_indices`, which only ever yields real char-boundary offsets.
fn whitespace_tokens_with_offsets(s: &str) -> Vec<(usize, &str)> {
    let mut out = Vec::new();
    let mut start: Option<usize> = None;
    for (i, c) in s.char_indices() {
        if c.is_whitespace() {
            if let Some(st) = start.take() {
                out.push((st, &s[st..i]));
            }
        } else if start.is_none() {
            start = Some(i);
        }
    }
    if let Some(st) = start {
        out.push((st, &s[st..]));
    }
    out
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

    // TODO.canvas.md: "Поддержка мыши в редакторе TUI" — `on_mouse` is
    // mostly just wiring onto `edtui`'s own already-working mouse support
    // (see `on_mouse`'s own doc comment); what these two tests actually
    // cover is the wiring itself (crossterm's `MouseEvent` reaches
    // `EditorState` correctly through it) and the one guard this file adds
    // on top (no mouse handling while the file picker overlay is open).
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
        )
        .unwrap();
        // Same requirement `prime_viewport`'s own test documents: a real
        // render populates `screen_area`/`viewport`, which mouse
        // coordinate mapping needs.
        let area = Rect::new(0, 0, 80, 22);
        let mut buf = Buffer::empty(area);
        EditorView::new(&mut se.editor).wrap(true).render(area, &mut buf);
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

    // TODO.canvas.md: "Саджесты и подсветка синтаксиса в TUI", item 1.
    #[test]
    fn meshfox_highlights_covers_the_whole_marker_comment() {
        let text = "# Root\n<!-- meshfox:node id=\"root\" -->\n\nbody\n";
        let highlights = meshfox_highlights(text);
        let marker = highlights
            .iter()
            .find(|h| h.start == Index2::new(1, 0))
            .expect("a highlight starting at the comment's own line");
        // "<!-- meshfox:node id=\"root\" -->" is 31 chars long; `end` is
        // inclusive, so the last char (the closing `>`) is at col 30.
        assert_eq!(marker.end, Index2::new(1, 30));
    }

    #[test]
    fn meshfox_highlights_covers_each_attribute_key() {
        let text = "<!-- meshfox:node id=\"root\" color=\"1\" -->\n";
        let highlights = meshfox_highlights(text);
        // "id" starts right after "<!-- meshfox:node " (18 chars in).
        let id_key = highlights
            .iter()
            .find(|h| h.start == Index2::new(0, 18))
            .expect("a highlight for the `id` key");
        assert_eq!(id_key.end, Index2::new(0, 19)); // "id" is 2 chars, inclusive end
    }

    #[test]
    fn meshfox_highlights_ignores_a_plain_prose_line() {
        let text = "# Root\n\njust some text, no markers here\n";
        assert!(meshfox_highlights(text).is_empty());
    }

    #[test]
    fn meshfox_highlights_covers_a_bare_closing_marker_with_no_attributes() {
        let text = "<!-- /meshfox:output -->\n";
        let highlights = meshfox_highlights(text);
        assert_eq!(highlights.len(), 1, "just the marker span, no attribute keys");
        assert_eq!(highlights[0].start, Index2::new(0, 0));
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
        assert_eq!(lines[3], "<!-- meshfox:node -->");
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
    fn detect_attr_context_recognizes_node_edge_and_fence_lines() {
        assert_eq!(
            detect_attr_context("<!-- meshfox:node id=\"root\" -->"),
            Some(AttrKind::Node)
        );
        assert_eq!(
            detect_attr_context("<!-- meshfox:edge from=\"root\" -->"),
            Some(AttrKind::Edge)
        );
        assert_eq!(detect_attr_context("```bash name=\"build\""), Some(AttrKind::Fence));
        assert_eq!(detect_attr_context("just a body line"), None);
    }

    #[test]
    fn attr_candidates_excludes_attributes_already_on_the_line() {
        let names: Vec<&str> = attr_candidates(AttrKind::Node, "<!-- meshfox:node id=\"root\" x=0 -->")
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
        let name = candidates.iter().find(|c| c.name == "name").expect("name offered");
        assert!(!name.is_flag);
    }

    #[test]
    fn attr_candidates_for_a_fence_excludes_a_flag_already_present() {
        let candidates = attr_candidates(AttrKind::Fence, "```bash cache");
        assert!(!candidates.iter().any(|c| c.name == "cache"));
        assert!(candidates.iter().any(|c| c.name == "tty"));
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
        se.insert_attr_candidate(AttrCandidate { name: "x", is_flag: false });

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
            vec!["bag".to_string(), "improvement".to_string(), "docs".to_string()],
        );
        let (start, end) = tags_value_range(line.trim_end()).unwrap();
        se.editor.cursor = Index2::new(0, (start + end) / 2);

        se.open_attr_suggest();

        assert!(se.tag_suggest_open);
        assert!(!se.attr_suggest_open);
        assert!(!se.tag_suggest_candidates.contains(&"bag".to_string())); // already on this node
        assert!(se.tag_suggest_candidates.contains(&"improvement".to_string()));
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
