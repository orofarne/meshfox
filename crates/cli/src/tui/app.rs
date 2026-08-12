//! TUI application state and behavior: tree navigation, node-body
//! rendering, and running blocks (deps chain + live streamed output + kill
//! + cache write-back) — the same execution model `meshfox run`/`meshfox
//! view` use, adapted to drive a redraw loop instead of printing to stdout
//! or a browser.

use std::collections::{HashMap, HashSet};
use std::io;
use std::path::{Path, PathBuf};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use meshfox_core::deps::BlockAddr;
use meshfox_core::fence::{self, scan_runnable_blocks};
use meshfox_core::mdcanvas;
use meshfox_core::output::{write_output, ExecOutput};
use meshfox_core::vars::{declared_vars, resolve_block_env, VarDecl, VarType};
use meshfox_core::{Canvas, FileDisplay, NodeType, VarCache};
use meshfox_server::stream_exec::SpawnedProcess;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::ListState;
use ratatui_image::picker::Picker;
use ratatui_image::protocol::Protocol;

use super::ui;

use super::markdown::{self, Highlighter, Segment};
use super::tree::{self, TreeRow};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Tree,
    Document,
}

/// A modal form for one or more declared variables at once — all of them
/// visible and editable together (arrow keys move the focused field,
/// Enter submits the whole form), same shape as the web UI's `VarsForm`,
/// rather than asking one at a time. Built either by `advance_run` (just
/// whatever a run still needs) or by `trigger_configure` (every declared
/// non-secret variable in the document).
pub struct VarFormState {
    pub decls: Vec<VarDecl>,
    /// Parallel to `decls` — one editable buffer per field, pre-filled
    /// with `current_value` (so submitting untouched just confirms the
    /// suggestion).
    pub inputs: Vec<String>,
    pub selected: usize,
    /// `true` for a `c`-triggered walk of every declared (non-secret)
    /// variable (see `trigger_configure`), `false` for the ordinary
    /// "resolve whatever a run still needs" form (`advance_run`) — purely
    /// so `ui.rs` can title the modal accordingly and so submitting it
    /// doesn't try to resume a run that was never started.
    pub configuring: bool,
}

pub struct BlockChoice {
    pub name: String,
    pub cache: bool,
    pub tty: bool,
    pub is_default: bool,
}

pub struct BlockPickerState {
    pub node_id: String,
    pub blocks: Vec<BlockChoice>,
    pub selected: usize,
    pub with_deps: bool,
}

/// A `tty` step waiting for the event loop (`mod.rs`) to actually hand the
/// terminal over — `App` itself never touches raw mode/the alternate
/// screen, that's `mod.rs`'s job, so it just parks the request here and
/// `advance_run` returns; the main loop checks this before its next
/// `select!` and does the handoff, then calls `resume_after_tty`.
pub struct PendingTty {
    pub block_name: String,
    pub code: String,
    pub env: HashMap<String, String>,
}

pub struct RunState {
    pub chain: Vec<BlockAddr>,
    pub idx: usize,
    pub proc: Option<SpawnedProcess>,
    pub lines: Vec<String>,
    pub full_output: String,
    pub current_node_text: String,
    pub had_failure: bool,
    pub killed: bool,
    /// Set once the chain has run out (or been killed/cancelled) — `run`
    /// then stays around (rather than getting cleared) purely so its
    /// transcript keeps showing in the output pane until the *next* run
    /// starts. Only `proc.is_some()` (never this flag) gates whether the
    /// event loop still polls it for output.
    pub finished: bool,
}

pub struct App {
    pub canvas_path: PathBuf,
    pub raw: String,
    /// Parsed straight from `raw` — the source of truth for running blocks
    /// and writing cache back (same raw-file-only scope `meshfox run` has;
    /// `include` nodes are never resolved here, so a block that only
    /// exists inside an included file simply isn't addressable through the
    /// including document, same as the CLI).
    pub canvas: Canvas,
    /// `canvas` with every `include` node spliced in (`meshfox_core::include::resolve`),
    /// same as what `meshfox view` sends the browser — this is what the
    /// tree and document pane actually render, so browsing a doc in the
    /// TUI reads the same way it does in the browser.
    pub display_canvas: Canvas,
    pub decls: Vec<VarDecl>,
    pub var_cache: VarCache,
    pub run_overrides: HashMap<String, String>,
    pub expanded: HashSet<String>,
    pub rows: Vec<TreeRow>,
    pub selected: usize,
    /// Persisted across frames (rather than a fresh `ListState::default()`
    /// per render) purely so `.offset()` — which `List` updates as it
    /// renders, to reflect wherever it actually auto-scrolled to keep the
    /// selection visible — is available for mouse hit-testing: a click's
    /// row has to be mapped through the *real* scroll offset, not
    /// recomputed by hand.
    pub list_state: ListState,
    pub focus: Focus,
    pub doc_segments: Vec<Segment>,
    pub doc_images: HashMap<PathBuf, Option<Protocol>>,
    pub doc_scroll: u16,
    pub highlighter: Highlighter,
    pub picker: Picker,
    pub run: Option<RunState>,
    pub pending_tty: Option<PendingTty>,
    pub block_picker: Option<BlockPickerState>,
    pub var_form: Option<VarFormState>,
    pub status: String,
    pub show_help: bool,
    pub should_quit: bool,
    /// Aggregate `(total, failed)` across every embedded constraint fence
    /// in `display_canvas`, computed alongside it (see
    /// `resolve_includes`/`rebuild_display_canvas`) — `None` when the
    /// document has no constraint fences at all, so the footer can render
    /// nothing rather than a vacuous "0/0" (same convention the web UI's
    /// toolbar badge uses).
    pub constraint_stats: Option<(usize, usize)>,
}

/// A non-secret declaration's currently-resolved value with no overrides
/// in play — the process environment, then the on-disk cache, then its
/// own `default` — same precedence `vars::resolve` uses minus the
/// `run_overrides`/form-override step, and the same idea as the CLI's own
/// `current_value` in `main.rs`. Shown as a var form field's pre-filled
/// suggestion, both for `advance_run`'s "still missing" form (where, by
/// construction, this can only ever equal `decl.default` — env/cache
/// already failed, or it wouldn't be missing) and for `trigger_configure`'s
/// "every declared variable" form (where it's the actual point: show
/// what's already resolved, not just the bare `default`).
fn current_value(decl: &VarDecl, cache: &VarCache) -> Option<String> {
    std::env::var(&decl.name).ok().or_else(|| cache.get(&decl.name).map(str::to_string)).or_else(|| decl.default.clone())
}

/// A var form field's starting value, coerced to something its own
/// control can actually represent — a `bool` field is a toggle, so its
/// value is always canonically `"true"` or `"false"` (anything else,
/// including no suggestion at all, starts as `"false"`); a `select` field
/// is a chooser over `decl.choices`, so a suggestion that isn't actually
/// one of them (a stale cache entry from before `choices=` changed, say)
/// falls back to the first choice rather than displaying something the
/// left/right cycle could never have produced itself. `String`/`Int`
/// fields are free text, so whatever `current_value` found (or an empty
/// string) passes through unchanged.
fn initial_field_input(decl: &VarDecl, cache: &VarCache) -> String {
    let suggestion = current_value(decl, cache);
    match decl.var_type {
        VarType::Bool => if suggestion.as_deref() == Some("true") { "true" } else { "false" }.to_string(),
        VarType::Select => match suggestion {
            Some(v) if decl.choices.iter().any(|c| c == &v) => v,
            _ => decl.choices.first().cloned().unwrap_or_default(),
        },
        VarType::String | VarType::Int => suggestion.unwrap_or_default(),
    }
}

impl App {
    pub fn new(canvas_path: PathBuf) -> io::Result<App> {
        let raw = std::fs::read_to_string(&canvas_path)?;
        let canvas = Canvas::from_markdown(&raw).map_err(|e| io::Error::other(e.to_string()))?;
        let decls = declared_vars(&canvas).unwrap_or_default();
        let var_cache = VarCache::load(&canvas_path).unwrap_or_else(|_| VarCache::in_memory());
        let expanded = HashSet::new();
        let display_canvas = resolve_includes(&canvas, &canvas_path);
        let constraint_stats = constraint_stats(&display_canvas);
        let rows = tree::flatten(&display_canvas, &expanded);
        // `Picker::from_query_stdio()` (protocol auto-detection) turned out
        // to be unreliable across real terminals in practice — a terminal
        // that doesn't actually support the protocol it gets detected as
        // just renders nothing, silently. Half-blocks are pure Unicode +
        // color, so they render everywhere, tmux included.
        let picker = Picker::halfblocks();

        let mut app = App {
            canvas_path,
            raw,
            canvas,
            display_canvas,
            decls,
            var_cache,
            run_overrides: HashMap::new(),
            expanded,
            rows,
            selected: 0,
            list_state: ListState::default(),
            focus: Focus::Tree,
            doc_segments: Vec::new(),
            doc_images: HashMap::new(),
            doc_scroll: 0,
            highlighter: Highlighter::new(),
            picker,
            run: None,
            pending_tty: None,
            block_picker: None,
            var_form: None,
            status: String::new(),
            show_help: false,
            should_quit: false,
            constraint_stats,
        };
        app.render_current_document();
        Ok(app)
    }

    pub async fn on_key(&mut self, key: KeyEvent) {
        if self.var_form.is_some() {
            self.on_var_form_key(key).await;
            return;
        }
        if self.block_picker.is_some() {
            self.on_block_picker_key(key).await;
            return;
        }

        match key.code {
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Esc => {
                if self.show_help {
                    self.show_help = false;
                } else {
                    self.should_quit = true;
                }
            }
            KeyCode::Char('?') => self.show_help = !self.show_help,
            KeyCode::Tab => {
                self.focus = match self.focus {
                    Focus::Tree => Focus::Document,
                    Focus::Document => Focus::Tree,
                };
            }
            KeyCode::Up | KeyCode::Char('k') => match self.focus {
                Focus::Tree => self.move_selection(-1),
                Focus::Document => self.scroll_document(-1),
            },
            KeyCode::Down | KeyCode::Char('j') => match self.focus {
                Focus::Tree => self.move_selection(1),
                Focus::Document => self.scroll_document(1),
            },
            KeyCode::Enter if self.focus == Focus::Tree => self.toggle_expand(),
            KeyCode::Left | KeyCode::Char('h') if self.focus == Focus::Tree => self.collapse_or_to_parent(),
            KeyCode::Right | KeyCode::Char('l') if self.focus == Focus::Tree => self.expand_selected(),
            KeyCode::Char('r') => self.trigger_run(true).await,
            KeyCode::Char('R') => self.trigger_run(false).await,
            KeyCode::Char('K') => self.kill_running(),
            KeyCode::Char('o') => self.trigger_open_file(),
            KeyCode::Char('c') => self.trigger_configure(),
            KeyCode::PageDown => self.scroll_document(10),
            KeyCode::PageUp => self.scroll_document(-10),
            KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => self.scroll_document(10),
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => self.scroll_document(-10),
            _ => {}
        }
    }

    /// All of `var_form`'s fields at once — arrow keys/Tab move which one's
    /// focused, and Enter submits the *whole* form regardless of which
    /// field is focused (same as pressing Enter in an HTML text input
    /// submits its enclosing `<form>`, which is what the web UI's own
    /// `VarsForm` is). Editing a field is type-aware, same control each
    /// type gets in `VarsForm`/the CLI's own line prompt: `String` takes
    /// any text; `Int` only accepts digits (and a leading `-`) as they're
    /// typed, so the field can't even hold something
    /// `meshfox_core::validate_value` would reject by the time Enter is
    /// pressed; `Bool`/`Select` are a left/right toggle/cycle instead —
    /// typing a character or backspace does nothing to those, since
    /// there's no text to edit.
    async fn on_var_form_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Enter => self.submit_var_form().await,
            KeyCode::Esc => self.cancel_var_form(),
            KeyCode::Up | KeyCode::BackTab => {
                if let Some(vf) = &mut self.var_form {
                    vf.selected = vf.selected.checked_sub(1).unwrap_or(vf.decls.len() - 1);
                }
            }
            KeyCode::Down | KeyCode::Tab => {
                if let Some(vf) = &mut self.var_form {
                    vf.selected = (vf.selected + 1) % vf.decls.len();
                }
            }
            KeyCode::Left => self.cycle_var_form_field(-1),
            KeyCode::Right => self.cycle_var_form_field(1),
            KeyCode::Backspace => {
                if let Some(vf) = &mut self.var_form {
                    let i = vf.selected;
                    if matches!(vf.decls[i].var_type, VarType::String | VarType::Int) {
                        vf.inputs[i].pop();
                    }
                }
            }
            KeyCode::Char(c) => {
                if let Some(vf) = &mut self.var_form {
                    let i = vf.selected;
                    let allowed = match vf.decls[i].var_type {
                        VarType::String => true,
                        // A leading `+`/`-` (once, only as the very first
                        // character — same grammar `i64::from_str` itself
                        // accepts, see `meshfox_core::validate_value`)
                        // plus digits — anything else typed just doesn't
                        // land, rather than landing and then failing
                        // `validate_value` at submit time.
                        VarType::Int => {
                            c.is_ascii_digit() || ((c == '-' || c == '+') && vf.inputs[i].is_empty())
                        }
                        VarType::Bool | VarType::Select => false,
                    };
                    if allowed {
                        vf.inputs[i].push(c);
                    }
                }
            }
            _ => {}
        }
    }

    /// Left/right on the focused field of `var_form` — a no-op for
    /// `String`/`Int` (nothing to cycle through), flips `Bool` between
    /// `"true"`/`"false"`, and steps `Select` to the next/previous
    /// `choices` entry (wrapping both ways), starting from wherever the
    /// current value sits (or the first choice, if it somehow isn't one —
    /// same fallback `initial_field_input` already applies when a field is
    /// first built).
    fn cycle_var_form_field(&mut self, dir: i32) {
        let Some(vf) = &mut self.var_form else { return };
        let i = vf.selected;
        match vf.decls[i].var_type {
            VarType::Bool => {
                vf.inputs[i] = if vf.inputs[i] == "true" { "false" } else { "true" }.to_string();
            }
            VarType::Select => {
                let choices = &vf.decls[i].choices;
                if choices.is_empty() {
                    return;
                }
                let len = choices.len() as i32;
                let current = choices.iter().position(|c| c == &vf.inputs[i]).map(|p| p as i32).unwrap_or(0);
                let next = (current + dir).rem_euclid(len) as usize;
                vf.inputs[i] = choices[next].clone();
            }
            VarType::String | VarType::Int => {}
        }
    }

    async fn on_block_picker_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                if let Some(bp) = &mut self.block_picker {
                    bp.selected = bp.selected.saturating_sub(1);
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if let Some(bp) = &mut self.block_picker {
                    bp.selected = (bp.selected + 1).min(bp.blocks.len().saturating_sub(1));
                }
            }
            KeyCode::Enter => {
                let Some(bp) = self.block_picker.take() else { return };
                let name = bp.blocks[bp.selected].name.clone();
                self.start_run(bp.node_id, name, bp.with_deps).await;
            }
            KeyCode::Esc => self.block_picker = None,
            _ => {}
        }
    }

    /// Clicks select a tree row and focus that pane, same as before — except
    /// a click that lands specifically on a row's own disclosure marker
    /// (`▾`/`▸`, see `ui::render_tree`) toggles it expanded/collapsed
    /// instead, same as clicking it with the keyboard (`enter`) would. The
    /// scroll wheel over either the tree or the document pane moves/scrolls
    /// it. Nothing else (run/kill buttons, clicking inside a `tty`
    /// handoff) is wired up yet.
    pub fn on_mouse(&mut self, mouse: MouseEvent) {
        if self.var_form.is_some() || self.block_picker.is_some() {
            return; // modal is up — no pane underneath it to click through to
        }
        let (cols, rows) = crossterm::terminal::size().unwrap_or((80, 24));
        let layout = ui::compute_layout(Rect::new(0, 0, cols, rows));

        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if point_in(layout.tree, mouse.column, mouse.row) {
                    self.focus = Focus::Tree;
                    let inner_x = layout.tree.x + 1; // left border
                    let inner_y = layout.tree.y + 1; // top border
                    if mouse.row >= inner_y {
                        let clicked = self.list_state.offset() + (mouse.row - inner_y) as usize;
                        if let Some(row) = self.rows.get(clicked) {
                            // "  " * depth (indent) then a 2-column-wide
                            // disclosure marker — see `ui::render_tree`'s
                            // own `indent`/`disclosure` spans, which this
                            // has to stay in step with.
                            let disclosure_col = inner_x + row.depth as u16 * 2;
                            let on_disclosure = row.has_children
                                && mouse.column >= disclosure_col
                                && mouse.column < disclosure_col + 2;

                            if clicked != self.selected {
                                self.selected = clicked;
                                self.doc_scroll = 0;
                                self.render_current_document();
                            }
                            if on_disclosure {
                                self.toggle_expand();
                            }
                        }
                    }
                } else if point_in(layout.document, mouse.column, mouse.row) {
                    self.focus = Focus::Document;
                }
            }
            MouseEventKind::ScrollDown => {
                if point_in(layout.tree, mouse.column, mouse.row) {
                    self.move_selection(1);
                } else if point_in(layout.document, mouse.column, mouse.row) {
                    self.scroll_document(3);
                }
            }
            MouseEventKind::ScrollUp => {
                if point_in(layout.tree, mouse.column, mouse.row) {
                    self.move_selection(-1);
                } else if point_in(layout.document, mouse.column, mouse.row) {
                    self.scroll_document(-3);
                }
            }
            _ => {}
        }
    }

    // -- tree navigation --------------------------------------------------

    fn move_selection(&mut self, delta: i32) {
        if self.rows.is_empty() {
            return;
        }
        let len = self.rows.len() as i32;
        let idx = (self.selected as i32 + delta).clamp(0, len - 1) as usize;
        if idx != self.selected {
            self.selected = idx;
            self.doc_scroll = 0;
            self.render_current_document();
        }
    }

    fn scroll_document(&mut self, delta: i32) {
        self.doc_scroll = (self.doc_scroll as i32 + delta).max(0) as u16;
    }

    fn toggle_expand(&mut self) {
        let Some(row) = self.rows.get(self.selected) else { return };
        if !row.has_children {
            return;
        }
        if row.expanded {
            self.expanded.remove(&row.node_id);
        } else {
            self.expanded.insert(row.node_id.clone());
        }
        self.rebuild_rows();
    }

    fn expand_selected(&mut self) {
        let Some(row) = self.rows.get(self.selected) else { return };
        if row.has_children && !row.expanded {
            self.expanded.insert(row.node_id.clone());
            self.rebuild_rows();
        }
    }

    fn collapse_or_to_parent(&mut self) {
        let Some(row) = self.rows.get(self.selected) else { return };
        if row.has_children && row.expanded {
            self.expanded.remove(&row.node_id);
            self.rebuild_rows();
            return;
        }
        if row.depth > 0 {
            let target_depth = row.depth - 1;
            if let Some(pos) = self.rows[..self.selected].iter().rposition(|r| r.depth == target_depth) {
                self.selected = pos;
                self.doc_scroll = 0;
                self.render_current_document();
            }
        }
    }

    fn rebuild_rows(&mut self) {
        let current_id = self.rows.get(self.selected).map(|r| r.node_id.clone());
        self.rows = tree::flatten(&self.display_canvas, &self.expanded);
        match current_id.and_then(|id| self.rows.iter().position(|r| r.node_id == id)) {
            Some(pos) => self.selected = pos,
            None => self.selected = self.selected.min(self.rows.len().saturating_sub(1)),
        }
    }

    fn rebuild_display_canvas(&mut self) {
        self.display_canvas = resolve_includes(&self.canvas, &self.canvas_path);
        self.constraint_stats = constraint_stats(&self.display_canvas);
    }

    /// Whether the currently selected row is a `file` node with a
    /// `target` — same gate the web UI's `MeshNode` uses to decide
    /// whether to render its "↗ open" button at all, used here so the
    /// footer/help hint for `o` only shows up when it'd actually do
    /// something (unlike `r`/`R`/`c`, which stay in the footer
    /// unconditionally and just report "nothing runnable"/etc. if
    /// pressed somewhere they don't apply — a *contextual* hint that
    /// changed with every arrow-key move would just be noise for those).
    pub fn selected_is_open_target(&self) -> bool {
        self.rows
            .get(self.selected)
            .and_then(|row| self.display_canvas.node(&row.node_id))
            .is_some_and(|node| node.node_type == NodeType::File && node.target.is_some())
    }

    /// `o` — opens the selected `file` node's `target` in the OS's own
    /// default application for it (`open` on macOS, `xdg-open` on Linux,
    /// `start` on Windows, via the `open` crate), the terminal
    /// counterpart to the web UI's "↗ open" button
    /// (`crates/server/src/lib.rs`'s `open_node_file` handler does the
    /// same thing over HTTP, for the browser case). Resolved relative to
    /// the canvas file's own directory, same `base_dir` join
    /// `render_current_document`'s `display="code"` preview already
    /// uses — no separate "must stay inside the canvas directory"
    /// confinement check like the server's own `resolve_confined_target`,
    /// since there's no network boundary to defend here: this is the
    /// user's own local process, opening a file they can already reach
    /// directly. Best-effort — spawns the opener and returns as soon as
    /// it has, without waiting for it to exit.
    fn trigger_open_file(&mut self) {
        let Some(row) = self.rows.get(self.selected) else { return };
        let Some(node) = self.display_canvas.node(&row.node_id) else { return };
        if node.node_type != NodeType::File {
            self.status = "not a file node".into();
            return;
        }
        let Some(target) = &node.target else {
            self.status = "file node has no target".into();
            return;
        };
        let base_dir = self.canvas_path.parent().unwrap_or_else(|| Path::new(".")).to_path_buf();
        let path = base_dir.join(target);
        match open::that(&path) {
            Ok(()) => self.status = format!("opened {}", path.display()),
            Err(e) => self.status = format!("failed to open {}: {e}", path.display()),
        }
    }

    // -- document rendering -------------------------------------------------

    fn render_current_document(&mut self) {
        self.doc_segments.clear();
        self.doc_images.clear();
        let Some(row) = self.rows.get(self.selected) else { return };
        let Some(node) = self.display_canvas.node(&row.node_id) else { return };
        let base_dir = self.canvas_path.parent().unwrap_or_else(|| Path::new(".")).to_path_buf();

        // `file` nodes with `display="code"` (see SPEC.md) show the
        // target's own file content, read fresh off disk — same as the
        // browser's read-only preview — rather than the node's own body
        // (which for a `file` node is just the one Markdown link).
        if node.node_type == NodeType::File && node.display == Some(FileDisplay::Code) {
            if let Some(target) = &node.target {
                let path = base_dir.join(target);
                self.doc_segments = match std::fs::read_to_string(&path) {
                    Ok(content) => {
                        vec![Segment::Text(self.highlighter.highlight_file(node.lang.as_deref(), &path, &content))]
                    }
                    Err(e) => vec![Segment::Text(vec![Line::from(Span::styled(
                        format!("failed to read {}: {e}", path.display()),
                        Style::default().fg(Color::Red),
                    ))])],
                };
                return;
            }
        }

        self.doc_segments = markdown::render(&node.text, &base_dir, &self.highlighter);

        let paths: Vec<PathBuf> = self
            .doc_segments
            .iter()
            .filter_map(|s| match s {
                Segment::Image { path, .. } => Some(path.clone()),
                Segment::Text(_) => None,
            })
            .collect();
        for path in paths {
            let protocol = load_image_protocol(&mut self.picker, &path);
            self.doc_images.insert(path, protocol);
        }
    }

    // -- running blocks -----------------------------------------------------

    /// `r` (`with_deps = true`, runs the block's full `deps=` chain first —
    /// the usual choice) / `R` (`with_deps = false`, this block alone,
    /// same as the browser's plain "run" vs "⛓ run chain" pair). Opens a
    /// picker first when the node has more than one runnable block — there
    /// isn't a single obvious one to default to.
    async fn trigger_run(&mut self, with_deps: bool) {
        if self.run.as_ref().is_some_and(|r| !r.finished) {
            self.status = "a run is already in progress — press K to kill it first".into();
            return;
        }
        let Some(row) = self.rows.get(self.selected) else { return };
        let node_id = row.node_id.clone();
        let Some(node) = self.canvas.node(&node_id) else {
            self.status = "this comes from an `include` — open its own file to run its blocks".into();
            return;
        };
        let node_text = node.text.clone();
        let blocks = scan_runnable_blocks(&node_id, &node_text);
        if blocks.is_empty() {
            self.status = "nothing runnable in this node".into();
            return;
        }

        if blocks.len() == 1 {
            let name = blocks[0].name.clone().expect("scan_runnable_blocks always names its blocks");
            self.start_run(node_id, name, with_deps).await;
            return;
        }

        let default_name = fence::default_block(&node_id, &blocks).ok().flatten().and_then(|b| b.name.clone());
        let choices = blocks
            .iter()
            .map(|b| {
                let name = b.name.clone().expect("scan_runnable_blocks always names its blocks");
                BlockChoice { is_default: Some(&name) == default_name.as_ref(), name, cache: b.cache, tty: b.tty }
            })
            .collect();
        self.block_picker = Some(BlockPickerState { node_id, blocks: choices, selected: 0, with_deps });
    }

    async fn start_run(&mut self, node_id: String, block_name: String, with_deps: bool) {
        let target = BlockAddr::new(node_id, block_name);
        let chain = if with_deps {
            match meshfox_core::deps::resolve_chain(&self.canvas, target) {
                Ok(c) => c,
                Err(e) => {
                    self.status = format!("dependency error: {e}");
                    return;
                }
            }
        } else {
            vec![target]
        };

        self.run = Some(RunState {
            chain,
            idx: 0,
            proc: None,
            lines: Vec::new(),
            full_output: String::new(),
            current_node_text: String::new(),
            had_failure: false,
            killed: false,
            finished: false,
        });
        self.run_overrides.clear();
        self.status.clear();
        self.advance_run().await;
    }

    /// Drives the current run forward until it either starts a process
    /// (returns, so the event loop can start polling its output), pauses
    /// for a missing `meshfox:var` (returns, waiting on the prompt), or the
    /// chain runs out (marks `self.run` finished — see `RunState::finished`).
    async fn advance_run(&mut self) {
        loop {
            let Some((idx, len)) = self.run.as_ref().map(|r| (r.idx, r.chain.len())) else { return };
            if idx >= len {
                let (killed, had_failure) =
                    self.run.as_ref().map(|r| (r.killed, r.had_failure)).unwrap_or_default();
                self.status = if killed {
                    "run killed".into()
                } else if had_failure {
                    "run finished with a failure".into()
                } else {
                    "run finished".into()
                };
                if let Some(run) = &mut self.run {
                    run.finished = true;
                }
                return;
            }

            let addr = self.run.as_ref().unwrap().chain[idx].clone();
            let Some(node) = self.canvas.node(&addr.node_id) else {
                self.status = format!("node {:?} not found", addr.node_id);
                if let Some(run) = &mut self.run {
                    run.finished = true;
                    run.had_failure = true;
                }
                return;
            };
            let node_text = node.text.clone();
            let Some(block) = scan_runnable_blocks(&addr.node_id, &node_text)
                .into_iter()
                .find(|b| b.name.as_deref() == Some(addr.block_name.as_str()))
            else {
                self.status = format!("block {:?} not found in {:?}", addr.block_name, addr.node_id);
                if let Some(run) = &mut self.run {
                    run.finished = true;
                    run.had_failure = true;
                }
                return;
            };

            let resolution = resolve_block_env(&block.env, &self.decls, &self.run_overrides, &self.var_cache);
            if !resolution.missing.is_empty() {
                let inputs = resolution.missing.iter().map(|d| initial_field_input(d, &self.var_cache)).collect();
                self.var_form = Some(VarFormState { decls: resolution.missing, inputs, selected: 0, configuring: false });
                return;
            }

            // `tty` hands the *real* terminal over to the child, same as
            // `meshfox run` does — see `mod.rs::run_tty_handoff`, which is
            // what actually leaves the alternate screen/raw mode, runs it,
            // and comes back. `App` never touches the terminal itself, so
            // it just parks the request and returns; `mod.rs`'s loop picks
            // `pending_tty` up before its next `select!` and calls
            // `resume_after_tty` once the child exits.
            if block.tty {
                if let Some(run) = &mut self.run {
                    run.lines.push(format!("==> {} (interactive — handing over the terminal)", addr.block_name));
                }
                self.pending_tty =
                    Some(PendingTty { block_name: addr.block_name.clone(), code: block.code.clone(), env: resolution.env });
                return;
            }

            match meshfox_server::stream_exec::spawn_bash(&block.code, &resolution.env) {
                Ok(proc) => {
                    let run = self.run.as_mut().unwrap();
                    run.proc = Some(proc);
                    run.current_node_text = node_text;
                    run.full_output.clear();
                    run.lines.push(format!("==> {}", addr.block_name));
                    return;
                }
                Err(e) => {
                    self.status = format!("failed to run {:?}: {e}", addr.block_name);
                    if let Some(run) = &mut self.run {
                        run.finished = true;
                        run.had_failure = true;
                    }
                    return;
                }
            }
        }
    }

    /// Called by `mod.rs` once a `tty` handoff's child has exited —
    /// `tty`/`cache` are mutually exclusive (a `meshfox validate` error),
    /// so there's never cached output to write back for this step, unlike
    /// `on_output_line`'s non-tty completion path.
    pub async fn resume_after_tty(&mut self, exit_code: i32) {
        if let Some(run) = &mut self.run {
            run.lines.push(format!("(exited {exit_code})"));
            if exit_code != 0 {
                run.had_failure = true;
                run.idx = run.chain.len();
            } else {
                run.idx += 1;
            }
        }
        self.advance_run().await;
    }

    pub async fn on_output_line(&mut self, line: Option<String>) {
        if self.run.is_none() {
            return;
        }
        match line {
            Some(text) => {
                let run = self.run.as_mut().unwrap();
                run.lines.push(text.clone());
                run.full_output.push_str(&text);
                run.full_output.push('\n');
            }
            None => {
                let mut proc = self.run.as_mut().unwrap().proc.take().expect("output channel closed without a process");
                let status = proc.child.wait().await;
                let exit_code = status.ok().and_then(|s| s.code()).unwrap_or(-1);

                let (addr, node_text, full_output) = {
                    let run = self.run.as_ref().unwrap();
                    (run.chain[run.idx].clone(), run.current_node_text.clone(), run.full_output.clone())
                };
                self.run.as_mut().unwrap().lines.push(format!("(exit {exit_code})"));

                if let Some(block) = scan_runnable_blocks(&addr.node_id, &node_text)
                    .into_iter()
                    .find(|b| b.name.as_deref() == Some(addr.block_name.as_str()))
                {
                    if block.cache {
                        let result = ExecOutput { exit_code, output: full_output };
                        if let Some(updated) = write_output(&node_text, &addr.block_name, &result) {
                            if let Some(patched) = mdcanvas::set_node_body(&self.raw, &addr.node_id, &updated) {
                                self.raw = patched;
                                let _ = std::fs::write(&self.canvas_path, &self.raw);
                                if let Ok(reparsed) = Canvas::from_markdown(&self.raw) {
                                    self.canvas = reparsed;
                                    self.rebuild_display_canvas();
                                    self.rebuild_rows();
                                    self.render_current_document();
                                }
                            }
                        }
                    }
                }

                let run = self.run.as_mut().unwrap();
                if exit_code != 0 {
                    run.had_failure = true;
                    run.idx = run.chain.len();
                } else {
                    run.idx += 1;
                }
                self.advance_run().await;
            }
        }
    }

    fn kill_running(&mut self) {
        if let Some(run) = &mut self.run {
            if let Some(proc) = &run.proc {
                let _ = proc.kill();
            }
            run.killed = true;
            self.status = "killing...".into();
        }
    }

    /// Saves every field in `var_form` at once — whatever's currently
    /// typed in each, not just the focused one — same "submit the whole
    /// form" semantics as the web UI's `VarsForm`. A `secret` field is
    /// never written to the cache (still resolves for just this run via
    /// `run_overrides`, same as before), matching `vars::resolve`'s own
    /// secret handling.
    ///
    /// Validates every field first (`meshfox_core::validate_value`) —
    /// `Bool`/`Select`'s own left/right controls already can't produce an
    /// invalid value, and `Int`'s typed-character restriction
    /// (`on_var_form_key`) already rules most of this out too, but an
    /// empty or bare-`-` `Int` field can still slip through both of those
    /// (there's nothing to *type* that's invalid about an empty field).
    /// The whole form stays open — nothing is saved, nothing resumes — and
    /// focus jumps to the first offending field, so it's obvious what to
    /// fix.
    async fn submit_var_form(&mut self) {
        {
            let vf = self.var_form.as_ref().expect("guarded by on_key's is_some() check");
            if let Some((i, e)) = vf
                .decls
                .iter()
                .zip(vf.inputs.iter())
                .enumerate()
                .find_map(|(i, (d, v))| meshfox_core::validate_value(d, v).err().map(|e| (i, e)))
            {
                let vf = self.var_form.as_mut().unwrap();
                vf.selected = i;
                self.status = format!("meshfox: {e}");
                return;
            }
        }
        let Some(vf) = self.var_form.take() else { return };
        for (decl, value) in vf.decls.iter().zip(vf.inputs.iter()) {
            if !decl.secret {
                let _ = self.var_cache.set(&decl.name, value);
            }
            self.run_overrides.insert(decl.name.clone(), value.clone());
        }
        if vf.configuring {
            self.status = "meshfox: saved declared variable(s) to the cache".into();
        } else {
            self.advance_run().await;
        }
    }

    /// `c` — walks every declared non-secret variable in the whole
    /// document (regardless of which, if any, block currently references
    /// it via `env=`), same scope `meshfox configure` covers, all shown at
    /// once with each one's currently-resolved value as the pre-filled
    /// suggestion. Confirming (even unchanged) writes it to the cache —
    /// the browser counterpart is `VarsForm` opened from the toolbar's
    /// "configure" button; see `crates/server/src/lib.rs`'s
    /// `/api/vars/configure`. A no-op (past a status message) when
    /// there's nothing configurable, or while a run/another form/the
    /// block picker is already active.
    fn trigger_configure(&mut self) {
        if self.var_form.is_some() || self.block_picker.is_some() {
            return;
        }
        if self.run.as_ref().is_some_and(|r| !r.finished) {
            self.status = "a run is already in progress — press K to kill it first".into();
            return;
        }
        let decls: Vec<VarDecl> = self.decls.iter().filter(|d| !d.secret).cloned().collect();
        if decls.is_empty() {
            self.status = "meshfox: this canvas declares no configurable (non-secret) variable(s)".into();
            return;
        }
        let inputs = decls.iter().map(|d| initial_field_input(d, &self.var_cache)).collect();
        self.var_form = Some(VarFormState { decls, inputs, selected: 0, configuring: true });
    }

    /// Whether the footer/help hint for `c` (configure) should be shown at
    /// all — same "configurable" definition `trigger_configure` itself
    /// uses (declared, non-secret; a document that declares only secret
    /// variables has nothing `c` could usefully do, same as the CLI's own
    /// `configure` skipping them).
    pub fn has_configurable_vars(&self) -> bool {
        self.decls.iter().any(|d| !d.secret)
    }

    fn cancel_var_form(&mut self) {
        let Some(vf) = self.var_form.take() else { return };
        if vf.configuring {
            self.status = "configure cancelled".into();
            return;
        }
        if let Some(run) = &mut self.run {
            run.finished = true;
            run.had_failure = true;
        }
        self.status = "run cancelled".into();
    }
}

/// `include::resolve` errors (a broken target, a cycle) fall back to the
/// unresolved canvas rather than refusing to show anything — the rest of
/// the document is still worth browsing even if one `include` is broken.
fn resolve_includes(canvas: &Canvas, canvas_path: &Path) -> Canvas {
    let mut resolved = meshfox_core::include::resolve(canvas, canvas_path).unwrap_or_else(|_| canvas.clone());
    // Populates every node's `constraint_results` (see
    // `meshfox_core::constraint::annotate_status`) so `tree::flatten` and
    // `constraint_stats` below can read pass/fail straight off the tree
    // without re-evaluating it themselves.
    meshfox_core::constraint::annotate_status(&mut resolved);
    resolved
}

/// `(total, failed)` across every embedded constraint fence in `canvas`
/// (already `annotate_status`-ed by `resolve_includes`) — `None` when there
/// are none at all. See `App::constraint_stats`.
fn constraint_stats(canvas: &Canvas) -> Option<(usize, usize)> {
    let results: Vec<_> = canvas.nodes.iter().flat_map(|n| n.constraint_results.iter()).collect();
    if results.is_empty() {
        return None;
    }
    let failed = results.iter().filter(|r| !r.ok).count();
    Some((results.len(), failed))
}

fn load_image_protocol(picker: &mut Picker, path: &Path) -> Option<Protocol> {
    let dyn_img = image::ImageReader::open(path).ok()?.with_guessed_format().ok()?.decode().ok()?;
    let budget = ratatui::layout::Size::new(56, 24);
    picker.new_protocol(dyn_img, budget, ratatui_image::Resize::Fit(None)).ok()
}

fn point_in(rect: Rect, x: u16, y: u16) -> bool {
    x >= rect.x && x < rect.x + rect.width && y >= rect.y && y < rect.y + rect.height
}
