//! TUI application state and behavior: tree navigation, node-body
//! rendering, and running blocks (deps chain + live streamed output + kill
//! + cache write-back) — the same execution model `meshfox run`/`meshfox
//! view` use, adapted to drive a redraw loop instead of printing to stdout
//!   or a browser.

use std::collections::{HashMap, HashSet};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use meshfox_server::link_preview::{self, PreviewMeta};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use meshfox_core::deps::BlockAddr;
use meshfox_core::fence::{self, scan_runnable_blocks};
use meshfox_core::mdcanvas;
use meshfox_core::output::{write_output, ExecOutput};
use meshfox_core::vars::{declared_vars, resolve_block_env, BlockEnvResolution, VarDecl, VarType};
use meshfox_core::{Canvas, FileDisplay, Node, NodeType, VarCache};
use meshfox_server::stream_exec::SpawnedProcess;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::ListState;
use ratatui_image::picker::Picker;
use ratatui_image::protocol::Protocol;

use super::ui;

use super::markdown::{self, Highlighter, Segment};
use super::source_editor::{self, SourceEditorOutcome, SourceEditorState};
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
    pub interpreter: Option<String>,
    pub env: HashMap<String, String>,
    /// This step's own real `PWD` (see `advance_run`) — the primary
    /// document's own directory, unless the step's node was spliced in
    /// from an `include` elsewhere on disk, in which case that target's
    /// own directory instead.
    pub cwd: PathBuf,
    /// Mirrors `CodeBlock::autoclose` — `mod.rs::run_tty_handoff` skips its
    /// own "press any key to return" pause when this is set, going
    /// straight back to the canvas the instant the process exits.
    pub autoclose: bool,
}

pub struct RunState {
    pub chain: Vec<BlockAddr>,
    pub idx: usize,
    pub proc: Option<SpawnedProcess>,
    pub lines: Vec<String>,
    pub full_output: String,
    /// Reset to `Instant::now()` right before each step is actually
    /// spawned (`advance_run`) — read back once its exit code is known
    /// (`on_output_line`) to time it into `ExecOutput::duration_ms`, the
    /// same figure the web UI/CLI already compute around their own spawn
    /// points.
    pub step_started: std::time::Instant,
    pub current_node_text: String,
    pub had_failure: bool,
    pub killed: bool,
    /// Set once the chain has run out (or been killed/cancelled) — `run`
    /// then stays around (rather than getting cleared) purely so its
    /// transcript keeps showing in the output pane until the *next* run
    /// starts. Only `proc.is_some()` (never this flag) gates whether the
    /// event loop still polls it for output.
    pub finished: bool,
    /// Set by `advance_run` right before spawning the *current* step,
    /// only when that step is a `from=` target for some declared
    /// variable — the vars-out file path it was handed, plus which
    /// declarations to extract from it. Consumed (and cleared) once that
    /// step's exit code is known, by `on_output_line`/`resume_after_tty` —
    /// see `meshfox_core::varout`.
    pub pending_vars_out: Option<(PathBuf, Vec<VarDecl>)>,
    /// Which of `chain`'s entries must run for real regardless of
    /// `session_runs` — computed once, up front, by
    /// `meshfox_core::compute_forced_reruns` in `start_run` (see its own
    /// doc comment) and consulted by `advance_run`'s skip check alongside
    /// `is_requested_target`/`block.always`.
    pub forced_reruns: HashSet<BlockAddr>,
}

/// A runnable `file` node's own single execution — no `deps=` chain, no
/// `cache`, no `meshfox:var`, unlike a fenced block's `RunState`, since a
/// `file` node has none of those; see `App::start_file_run`. Kept as its
/// own separate state (rather than shoehorned into `RunState`, which is
/// built entirely around a `BlockAddr` chain) for the same reason the web
/// UI's `run_file_node` is its own endpoint, distinct from `run_block`.
pub struct FileRunState {
    pub proc: Option<SpawnedProcess>,
    pub lines: Vec<String>,
    pub had_failure: bool,
    /// Set once the process has exited — `run` stays around afterward
    /// purely so its transcript keeps showing until the *next* run
    /// starts, same convention `RunState::finished` uses.
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
    /// Values produced by `from=` source blocks already run earlier in the
    /// *current* run — kept separate from `run_overrides` so a computed
    /// variable can never be impersonated by a form answer; see
    /// `vars::resolve`'s doc comment. Cleared at the start of every new run
    /// (`start_run`), same as `run_overrides`.
    pub run_computed: HashMap<String, String>,
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
    pub file_run: Option<FileRunState>,
    pub pending_tty: Option<PendingTty>,
    pub block_picker: Option<BlockPickerState>,
    pub var_form: Option<VarFormState>,
    pub status: String,
    pub show_help: bool,
    pub should_quit: bool,
    /// The fullscreen raw-source editor (`e`) — `Some` takes over
    /// rendering entirely (see `ui::render`) instead of the usual 3-pane
    /// layout, same "one thing at a time" precedence `var_form`/
    /// `block_picker` already have over the base keymap (see `on_key`).
    pub source_editor: Option<SourceEditorState>,
    /// Aggregate `(total, failed)` across every embedded constraint fence
    /// in `display_canvas`, computed alongside it (see
    /// `resolve_includes`/`rebuild_display_canvas`) — `None` when the
    /// document has no constraint fences at all, so the footer can render
    /// nothing rather than a vacuous "0/0" (same convention the web UI's
    /// toolbar badge uses).
    pub constraint_stats: Option<(usize, usize)>,
    /// The last content of `canvas_path` either loaded from or written to
    /// disk by *this* process — shared with the background file-watcher
    /// thread (`mod.rs`'s `spawn_file_watcher`) purely so it can tell an
    /// external edit apart from its own write-back landing on disk, same
    /// "compare against what we last wrote" trick the web server's own
    /// watcher uses (`crates/server/src/lib.rs`'s `spawn_file_watcher`).
    /// Every place that writes `self.raw` to `canvas_path` must update
    /// this right after, or the watcher will (harmlessly, but
    /// distractingly) mistake that write for an external change.
    pub known_raw: Arc<Mutex<String>>,
    /// An external change to `canvas_path` that arrived while
    /// `source_editor` was open — applying it immediately could yank the
    /// document out from under an in-progress edit (or, worse, get
    /// silently clobbered the moment the editor saves). Parked here
    /// instead and applied once the editor closes without saving; a save
    /// makes it stale (the editor's own write is now what's on disk), so
    /// it's just dropped in that case. Mirrors the web UI's
    /// `pendingExternalChange` (`web/src/App.tsx`).
    pub pending_external_change: Option<String>,
    /// `link`+`preview` social-preview fetch — SSRF-safe, in-process (see
    /// `meshfox_server::link_preview`), shared with whatever background
    /// task is currently fetching via `Arc`. Same "alive for exactly this
    /// process's lifetime" cache contract as the web server's own copy
    /// (`crates/server/src/lib.rs`'s `AppState::link_preview_cache`) — this
    /// is a separate instance since the TUI doesn't run through `AppState`
    /// at all, never shared across processes either way.
    pub link_preview_cache: Arc<link_preview::PreviewCache>,
    /// Where a background fetch (see `maybe_fetch_link_preview`/
    /// `maybe_fetch_link_preview_image`) reports back once it's done —
    /// consumed by `mod.rs`'s `main_loop`, which forwards each message to
    /// `on_link_preview_msg`. Cloned into every spawned fetch task.
    pub link_preview_tx: tokio::sync::mpsc::UnboundedSender<LinkPreviewMsg>,
    /// URLs a metadata fetch has already been kicked off for — gates
    /// `maybe_fetch_link_preview` against re-spawning one on every
    /// selection change while the first is still in flight (or already
    /// failed — a failure is never retried within this session, same as
    /// the web server's own cache). Never cleared; outlives any single
    /// `render_current_document` call, unlike `doc_segments`/`doc_images`.
    pub link_preview_requested: HashSet<String>,
    /// Loaded OpenGraph metadata, keyed by page URL — only ever gains
    /// entries (a failed fetch just never appears here; see
    /// `link_preview_requested`), so "no entry" means "nothing to show
    /// yet, whether still loading or already failed" — deliberately not
    /// distinguished any further in the UI (see `render_current_document`).
    pub link_preview_meta: HashMap<String, PreviewMeta>,
    /// Same "requested" gate as `link_preview_requested`, but for a
    /// preview's own `og:image` bytes (a second, independent fetch —  see
    /// `maybe_fetch_link_preview_image`), keyed by the image URL.
    pub link_preview_image_requested: HashSet<String>,
    /// Decoded preview images, keyed by image URL rather than a local path
    /// (unlike `doc_images` — there's no file on disk here). Re-cloned
    /// into `doc_images` under a synthetic path on every
    /// `render_current_document` call so the existing `Segment::Image`
    /// render path (`ui.rs`) can show it with no changes of its own —
    /// `Protocol` is cheap to clone (see `ratatui_image::protocol`).
    pub link_preview_image: HashMap<String, Protocol>,
    /// Every block that has completed successfully at least once during
    /// this TUI process's own lifetime, keyed by its address — consulted
    /// (and updated) by `advance_run` so a chain run can skip re-running a
    /// dependency that's already run this session *and* hasn't changed
    /// since (see `SessionRun`; mirrors the web server's own
    /// `AppState::session_runs`). Never persisted anywhere; restarting the
    /// TUI starts fresh.
    pub session_runs: HashMap<(String, String), SessionRun>,
    /// Whether the `S` (reset session) confirm prompt is up — see `on_key`'s
    /// early dispatch to `on_reset_session_confirm_key` and
    /// `reset_session`'s own doc comment for why this asks at all despite
    /// being purely in-memory. Same "one thing at a time" precedence as
    /// `var_form`/`block_picker`/`source_editor`.
    pub reset_session_confirm: bool,
}

/// One block's most recent successful run this session — see
/// `App::session_runs`.
#[derive(Clone)]
pub struct SessionRun {
    /// `meshfox_core::session_fingerprint` of the block *as it stood*
    /// (code/lang/interpreter/env=/deps=, same as `crate::output`'s
    /// cached-output staleness mechanism) *and* the resolved values of
    /// whatever variables it referenced, on that successful run — a later
    /// `advance_run` only treats this as "already fresh" (skippable) if
    /// both still match.
    pub fingerprint: String,
    /// Whatever this block wrote to its own vars-out file last time it
    /// actually ran (only ever non-empty for a block that's a `from=`
    /// source for something) — folded into `run_computed` in place of
    /// re-running it when this run is skipped, so a later step that
    /// declared `from=` this block still gets a value.
    pub produced_vars: HashMap<String, String>,
    /// Whatever this block printed the last time it actually ran — there's
    /// no fresh output from a skipped step (it didn't run), so this is
    /// printed into the transcript instead, right after the skip line (see
    /// `advance_run`). Empty for a `tty` step, which never populates
    /// `RunState::full_output` to begin with.
    pub output: String,
    /// That same earlier run's own duration, in milliseconds.
    pub duration_ms: u64,
}

/// A background link-preview fetch's result, reported back through
/// `App::link_preview_tx` into `mod.rs`'s `main_loop` (same shape as the
/// existing `reload_rx`/output-line channels) — never sent at all on
/// failure, see `App::link_preview_requested`'s own doc comment.
pub enum LinkPreviewMsg {
    Meta { url: String, meta: PreviewMeta },
    Image { url: String, image: image::DynamicImage },
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
    std::env::var(&decl.name)
        .ok()
        .or_else(|| cache.get(&decl.name).map(str::to_string))
        .or_else(|| decl.default.clone())
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
        VarType::Bool => if suggestion.as_deref() == Some("true") {
            "true"
        } else {
            "false"
        }
        .to_string(),
        VarType::Select => match suggestion {
            Some(v) if decl.choices.iter().any(|c| c == &v) => v,
            _ => decl.choices.first().cloned().unwrap_or_default(),
        },
        VarType::String | VarType::Int => suggestion.unwrap_or_default(),
    }
}

impl App {
    pub fn new(
        canvas_path: PathBuf,
        link_preview_tx: tokio::sync::mpsc::UnboundedSender<LinkPreviewMsg>,
    ) -> io::Result<App> {
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
        let known_raw = Arc::new(Mutex::new(raw.clone()));
        // Computed before `canvas_path` is moved into the struct literal
        // below (its `canvas_path,` shorthand field).
        let syntax_root = crate::canvas_root_dir(&canvas_path).to_path_buf();

        let mut app = App {
            canvas_path,
            raw,
            canvas,
            display_canvas,
            decls,
            var_cache,
            run_overrides: HashMap::new(),
            run_computed: HashMap::new(),
            expanded,
            rows,
            selected: 0,
            list_state: ListState::default(),
            focus: Focus::Tree,
            doc_segments: Vec::new(),
            doc_images: HashMap::new(),
            doc_scroll: 0,
            highlighter: Highlighter::with_extra_syntaxes(&syntax_root),
            picker,
            run: None,
            file_run: None,
            pending_tty: None,
            block_picker: None,
            var_form: None,
            status: String::new(),
            show_help: false,
            should_quit: false,
            source_editor: None,
            constraint_stats,
            known_raw,
            pending_external_change: None,
            link_preview_cache: Arc::new(link_preview::PreviewCache::new()),
            link_preview_tx,
            link_preview_requested: HashSet::new(),
            link_preview_meta: HashMap::new(),
            link_preview_image_requested: HashSet::new(),
            link_preview_image: HashMap::new(),
            session_runs: HashMap::new(),
            reset_session_confirm: false,
        };
        app.render_current_document();
        Ok(app)
    }

    pub async fn on_key(&mut self, key: KeyEvent) {
        if let Some(se) = &mut self.source_editor {
            match se.on_key(key) {
                SourceEditorOutcome::Stay => {}
                SourceEditorOutcome::Close => {
                    self.source_editor = None;
                    if let Some(pending) = self.pending_external_change.take() {
                        self.apply_external_reload(pending);
                    }
                }
                SourceEditorOutcome::Save => self.save_source_editor(),
            }
            return;
        }
        if self.var_form.is_some() {
            self.on_var_form_key(key).await;
            return;
        }
        if self.block_picker.is_some() {
            self.on_block_picker_key(key).await;
            return;
        }
        if self.reset_session_confirm {
            self.on_reset_session_confirm_key(key);
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
            KeyCode::Left | KeyCode::Char('h') if self.focus == Focus::Tree => {
                self.collapse_or_to_parent()
            }
            KeyCode::Right | KeyCode::Char('l') if self.focus == Focus::Tree => {
                self.expand_selected()
            }
            KeyCode::Char('r') => self.trigger_run(true).await,
            KeyCode::Char('R') => self.trigger_run(false).await,
            KeyCode::Char('K') => self.kill_running(),
            KeyCode::Char('S') => self.reset_session_confirm = true,
            KeyCode::Char('o') => self.trigger_open_file(),
            KeyCode::Char('c') => self.trigger_configure(),
            KeyCode::Char('e') => self.open_source_editor(),
            KeyCode::PageDown => self.scroll_document(10),
            KeyCode::PageUp => self.scroll_document(-10),
            KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.scroll_document(10)
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.scroll_document(-10)
            }
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
                            c.is_ascii_digit()
                                || ((c == '-' || c == '+') && vf.inputs[i].is_empty())
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
                vf.inputs[i] = if vf.inputs[i] == "true" {
                    "false"
                } else {
                    "true"
                }
                .to_string();
            }
            VarType::Select => {
                let choices = &vf.decls[i].choices;
                if choices.is_empty() {
                    return;
                }
                let len = choices.len() as i32;
                let current = choices
                    .iter()
                    .position(|c| c == &vf.inputs[i])
                    .map(|p| p as i32)
                    .unwrap_or(0);
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
                let Some(bp) = self.block_picker.take() else {
                    return;
                };
                let name = bp.blocks[bp.selected].name.clone();
                self.start_run(bp.node_id, name, bp.with_deps).await;
            }
            KeyCode::Esc => self.block_picker = None,
            _ => {}
        }
    }

    /// While the fullscreen source editor is open, every mouse event is
    /// entirely its own — `SourceEditorState::on_mouse` (`edtui`'s own
    /// `mouse-support` feature, already enabled) handles click-to-position-
    /// cursor, drag-to-select, and scroll, same "vim `mouse=a`" shape this
    /// TODO item asked for — same early-return split `on_key` already has
    /// for it, so the tree/document hit-testing below never runs against
    /// coordinates that actually landed on the editor's own overlay.
    ///
    /// Otherwise: clicks select a tree row and focus that pane, same as
    /// before — except a click that lands specifically on a row's own
    /// disclosure marker (`▾`/`▸`, see `ui::render_tree`) toggles it
    /// expanded/collapsed instead, same as clicking it with the keyboard
    /// (`enter`) would. The scroll wheel over either the tree or the
    /// document pane moves/scrolls it. Nothing else (run/kill buttons,
    /// clicking inside a `tty` handoff) is wired up yet.
    pub fn on_mouse(&mut self, mouse: MouseEvent) {
        if let Some(se) = &mut self.source_editor {
            se.on_mouse(mouse);
            return;
        }
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
        let Some(row) = self.rows.get(self.selected) else {
            return;
        };
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
        let Some(row) = self.rows.get(self.selected) else {
            return;
        };
        if row.has_children && !row.expanded {
            self.expanded.insert(row.node_id.clone());
            self.rebuild_rows();
        }
    }

    fn collapse_or_to_parent(&mut self) {
        let Some(row) = self.rows.get(self.selected) else {
            return;
        };
        if row.has_children && row.expanded {
            self.expanded.remove(&row.node_id);
            self.rebuild_rows();
            return;
        }
        if row.depth > 0 {
            let target_depth = row.depth - 1;
            if let Some(pos) = self.rows[..self.selected]
                .iter()
                .rposition(|r| r.depth == target_depth)
            {
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

    /// Applies an edit to `canvas_path` made by something other than this
    /// process — reported by the background file-watcher thread (see
    /// `mod.rs`'s `spawn_file_watcher`) via `known_raw`. Deferred instead
    /// (into `pending_external_change`) rather than applied here if the
    /// source editor is open on this same file; see that field's own doc
    /// comment. Mirrors the web UI's own reload-on-change
    /// (`web/src/App.tsx`'s `watchChanges` callback), including leaving
    /// the currently selected node/scroll position alone where possible
    /// (`rebuild_rows` re-finds the same node id rather than resetting to
    /// the top).
    /// Entry point for the background file-watcher's notifications (see
    /// `mod.rs`'s `main_loop`) — defers to `pending_external_change`
    /// instead of applying immediately while the source editor is open on
    /// this file (see that field's doc comment), applies right away
    /// otherwise.
    pub fn on_external_change(&mut self, content: String) {
        if self.source_editor.is_some() {
            self.pending_external_change = Some(content);
            self.status = "file changed on disk — will reload once the editor closes".into();
            return;
        }
        self.apply_external_reload(content);
    }

    fn apply_external_reload(&mut self, content: String) {
        if content == self.raw {
            return;
        }
        self.raw = content;
        match Canvas::from_markdown(&self.raw) {
            Ok(parsed) => {
                self.canvas = parsed;
                self.decls = declared_vars(&self.canvas).unwrap_or_default();
                self.rebuild_display_canvas();
                self.rebuild_rows();
                self.render_current_document();
                self.status = "reloaded — file changed on disk".into();
            }
            Err(e) => {
                self.status = format!("file changed on disk but failed to parse: {e}");
            }
        }
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
        let Some(row) = self.rows.get(self.selected) else {
            return;
        };
        let Some(node) = self.display_canvas.node(&row.node_id) else {
            return;
        };
        if node.node_type != NodeType::File {
            self.status = "not a file node".into();
            return;
        }
        let Some(target) = &node.target else {
            self.status = "file node has no target".into();
            return;
        };
        let base_dir = self
            .canvas_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf();
        let path = base_dir.join(target);
        match open::that(&path) {
            Ok(()) => self.status = format!("opened {}", path.display()),
            Err(e) => self.status = format!("failed to open {}: {e}", path.display()),
        }
    }

    // -- source editor --------------------------------------------------

    /// `e` — opens the fullscreen source editor (`source_editor.rs`) on
    /// whichever real file the selected node's content actually lives in,
    /// with the cursor at that node's own body. Mirrors the server's own
    /// `locate_node` (`crates/server/src/lib.rs`): a node found directly
    /// in `display_canvas` with no `origin_path`/`origin_id` of its own
    /// lives in the primary document (including a canvas-`include` node
    /// itself, which by then is a `group` with an empty body — nothing
    /// node-specific to jump to beyond its own heading, same as any other
    /// node); one that does carry an origin is a canvas-`include`
    /// descendant, with a real separate on-disk identity; and
    /// `plain_markdown_include` (see that field's own doc comment) is the
    /// one case with no per-node identity inside its target at all — just
    /// opens that file at the top.
    fn open_source_editor(&mut self) {
        let Some(row) = self.rows.get(self.selected) else {
            return;
        };
        let node_id = row.node_id.clone();
        let Some(node) = self.display_canvas.node(&node_id) else {
            return;
        };

        let (path, is_canvas, local_id): (PathBuf, bool, Option<String>) =
            if let (Some(p), Some(local)) = (&node.origin_path, &node.origin_id) {
                (PathBuf::from(p), true, Some(local.clone()))
            } else if node.plain_markdown_include {
                match meshfox_core::include::list_includes(&self.canvas, &self.canvas_path)
                    .into_iter()
                    .find(|i| i.node_id == node_id)
                {
                    Some(info) => (info.path, false, None),
                    None => {
                        self.status = "couldn't resolve this include's target file".into();
                        return;
                    }
                }
            } else {
                (self.canvas_path.clone(), true, Some(node_id))
            };

        let files = meshfox_core::include::list_includes(&self.canvas, &self.canvas_path);

        // Cursor placement needs the *target file's own raw text* (to
        // convert a byte offset into a row/col) — read once here and
        // again inside `SourceEditorState::open`, rather than plumbing it
        // through, since that constructor is also the file-switcher's
        // own reload path and shouldn't need a cursor argument for that
        // case.
        let cursor = local_id
            .as_deref()
            .zip(std::fs::read_to_string(&path).ok())
            .and_then(|(id, raw)| {
                mdcanvas::node_body_offset(&raw, id)
                    .map(|off| source_editor::byte_offset_to_cursor(&raw, off))
            })
            .unwrap_or_default();

        let mut all_tags: Vec<String> = self
            .display_canvas
            .nodes
            .iter()
            .flat_map(|n| n.tags.iter().chain(n.extra_parents.iter().flat_map(|e| e.tags.iter())))
            .cloned()
            .collect();
        all_tags.sort();
        all_tags.dedup();

        match SourceEditorState::open(self.canvas_path.clone(), path, is_canvas, cursor, files, all_tags) {
            Ok(state) => self.source_editor = Some(state),
            Err(e) => self.status = format!("failed to open source editor: {e}"),
        }
    }

    /// `Ctrl-s` inside the source editor — validates (as a canvas, only
    /// when `is_canvas`; a plain-Markdown include target has no such
    /// requirement — see `SourceEditorState::is_canvas`'s own doc
    /// comment) and writes to disk, exactly mirroring the server's
    /// `put_canvas_raw`/`SourceFile` split. Refreshes `display_canvas`
    /// (and `raw`/`canvas`, if the primary document was what got saved)
    /// afterward, same as any other on-disk change here, so the tree/
    /// document panes reflect the edit the moment the editor closes.
    fn save_source_editor(&mut self) {
        let Some(se) = &mut self.source_editor else {
            return;
        };
        let text = se.editor.lines.to_string();
        if se.is_canvas {
            if let Err(e) = Canvas::from_markdown(&text) {
                se.error = Some(e.to_string());
                return;
            }
        }
        if let Err(e) = std::fs::write(&se.path, &text) {
            se.error = Some(format!("failed to write {}: {e}", se.path.display()));
            return;
        }
        let is_primary = se.path == self.canvas_path;
        se.mark_saved();
        if is_primary {
            self.raw = text.clone();
            if let Ok(reparsed) = Canvas::from_markdown(&text) {
                self.canvas = reparsed;
            }
            *self.known_raw.lock().unwrap() = self.raw.clone();
            // Whatever arrived externally while the editor was open is now
            // stale — this save just overwrote it on disk with the
            // editor's own buffer, same "last write wins" behavior a save
            // conflicting with a concurrent external edit already has.
            self.pending_external_change = None;
        }
        self.rebuild_display_canvas();
        self.rebuild_rows();
        self.render_current_document();
        self.status = "saved".into();
        self.source_editor = None;
    }

    // -- document rendering -------------------------------------------------

    fn render_current_document(&mut self) {
        self.doc_segments.clear();
        self.doc_images.clear();
        let Some(row) = self.rows.get(self.selected) else {
            return;
        };
        let Some(node) = self.display_canvas.node(&row.node_id) else {
            return;
        };
        let base_dir = self
            .canvas_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf();

        // `file` nodes with `display="code"` (see SPEC.md) show the
        // target's own file content, read fresh off disk — same as the
        // browser's read-only preview — rather than the node's own body
        // (which for a `file` node is just the one Markdown link).
        if node.node_type == NodeType::File && node.display == Some(FileDisplay::Code) {
            if let Some(target) = &node.target {
                let path = base_dir.join(target);
                self.doc_segments = match std::fs::read_to_string(&path) {
                    Ok(content) => {
                        vec![Segment::Text(self.highlighter.highlight_file(
                            node.lang.as_deref(),
                            &path,
                            &content,
                        ))]
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

        let images: Vec<(PathBuf, Option<u32>, Option<u32>)> = self
            .doc_segments
            .iter()
            .filter_map(|s| match s {
                Segment::Image {
                    path,
                    width_percent,
                    height_percent,
                    ..
                } => Some((path.clone(), *width_percent, *height_percent)),
                Segment::Text(_) => None,
            })
            .collect();
        for (path, width_percent, height_percent) in images {
            let protocol =
                load_image_protocol(&mut self.picker, &path, width_percent, height_percent);
            self.doc_images.insert(path, protocol);
        }

        if node.node_type == NodeType::Link && node.preview {
            if let Some(target) = node.target.clone() {
                self.append_link_preview(&target);
            }
        }
    }

    /// Appends a `link`+`preview` node's OpenGraph preview (title/
    /// description/image) after its plain link body — kicks off the
    /// metadata fetch (and, once that lands, the image fetch) if not
    /// already in flight (see `maybe_fetch_link_preview`/
    /// `maybe_fetch_link_preview_image`), but renders nothing extra until
    /// (and unless) a result actually lands — same "just show the plain
    /// link" fallback this pane already has for a missing/broken target.
    /// Reuses the ordinary `Segment::Image`/`doc_images` render path
    /// (`ui.rs`) for the image itself, keyed by a synthetic path (there's
    /// no file on disk here) rather than a real one.
    fn append_link_preview(&mut self, target: &str) {
        self.maybe_fetch_link_preview(target.to_string());
        let Some(meta) = self.link_preview_meta.get(target).cloned() else {
            return;
        };

        let mut lines = vec![Line::from("")];
        if let Some(title) = &meta.title {
            lines.push(Line::from(Span::styled(
                title.clone(),
                Style::default().add_modifier(Modifier::BOLD),
            )));
        }
        if let Some(description) = &meta.description {
            lines.push(Line::from(Span::styled(
                description.clone(),
                Style::default().fg(Color::DarkGray),
            )));
        }
        if lines.len() > 1 {
            self.doc_segments.push(Segment::Text(lines));
        }

        if let Some(image_url) = &meta.image {
            self.maybe_fetch_link_preview_image(image_url.clone());
            if let Some(protocol) = self.link_preview_image.get(image_url).cloned() {
                let path = link_preview_image_path(image_url);
                self.doc_images.insert(path.clone(), Some(protocol));
                self.doc_segments.push(Segment::Image {
                    path,
                    alt: "preview image".to_string(),
                    width_percent: None,
                    height_percent: None,
                });
            }
        }
    }

    /// Kicks off `url`'s OpenGraph metadata fetch in the background (see
    /// `link_preview` module doc for the SSRF hardening) unless one's
    /// already in flight or already landed — `link_preview_requested`
    /// gates against re-spawning on every selection change, and is never
    /// cleared, so a prior failure isn't retried within this session
    /// either (same cache contract the web server's own copy has). Result
    /// (if any — a failure sends nothing) arrives via `link_preview_tx`,
    /// handled by `on_link_preview_msg`.
    fn maybe_fetch_link_preview(&mut self, url: String) {
        if !self.link_preview_requested.insert(url.clone()) {
            return;
        }
        let cache = Arc::clone(&self.link_preview_cache);
        let tx = self.link_preview_tx.clone();
        tokio::spawn(async move {
            if let Some(meta) = cache.get_or_fetch(&url).await {
                let _ = tx.send(LinkPreviewMsg::Meta { url, meta });
            }
        });
    }

    /// Same idea as `maybe_fetch_link_preview`, but for a loaded preview's
    /// own `og:image` bytes — a second, independent SSRF-safe fetch (see
    /// `link_preview::fetch_image_bytes`), decoded here in the background
    /// task (cheap enough for one small preview image) and handed back as
    /// a plain `image::DynamicImage`; building the actual `Protocol` still
    /// has to happen on the main thread (`on_link_preview_msg`), since
    /// that needs `&mut self.picker`.
    fn maybe_fetch_link_preview_image(&mut self, url: String) {
        if !self.link_preview_image_requested.insert(url.clone()) {
            return;
        }
        let tx = self.link_preview_tx.clone();
        tokio::spawn(async move {
            let Ok(bytes) = link_preview::fetch_image_bytes(&url).await else {
                return;
            };
            let Ok(image) = image::load_from_memory(&bytes) else {
                return;
            };
            let _ = tx.send(LinkPreviewMsg::Image { url, image });
        });
    }

    /// Applies a background link-preview fetch's result (see
    /// `LinkPreviewMsg`) — called from `mod.rs`'s `main_loop`. Just updates
    /// the cache and re-renders the currently selected node's document
    /// pane; if the fetch that just landed belongs to some other node than
    /// whatever's selected now, this is a harmless no-op-ish redraw rather
    /// than something that needs special-casing away.
    pub fn on_link_preview_msg(&mut self, msg: LinkPreviewMsg) {
        match msg {
            LinkPreviewMsg::Meta { url, meta } => {
                self.link_preview_meta.insert(url, meta);
            }
            LinkPreviewMsg::Image { url, image } => {
                let budget = ratatui::layout::Size::new(56, 24);
                if let Ok(protocol) =
                    self.picker
                        .new_protocol(image, budget, ratatui_image::Resize::Fit(None))
                {
                    self.link_preview_image.insert(url, protocol);
                }
            }
        }
        self.render_current_document();
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
        if self.file_run.as_ref().is_some_and(|r| !r.finished) {
            self.status = "a run is already in progress — press K to kill it first".into();
            return;
        }
        let Some(row) = self.rows.get(self.selected) else {
            return;
        };
        let node_id = row.node_id.clone();

        // A runnable `file` node (`type="file" interpreter="..."`) has no
        // fenced blocks at all — it's its own single, uncached, unchained
        // action, same as the web UI's own "▷ run" on its title bar.
        // Checked against `self.display_canvas` (already include-resolved,
        // the same tree `row`/`node_id` came from) since a `file` node's
        // own runnability has nothing to do with `deps=`/chain machinery.
        if let Some(node) = self.display_canvas.node(&node_id) {
            if node.is_runnable_file() {
                self.start_file_run(node_id, node.clone()).await;
                return;
            }
        }

        // Finds which real file `node_id` actually lives in — itself or
        // an `include` target elsewhere on disk (see `advance_run`'s own
        // doc comment) — a fenced block spliced in from an `include` used
        // to be entirely unreachable from here.
        let located = match meshfox_core::locate_node(&self.raw, &self.canvas_path, &node_id) {
            Ok(l) => l,
            Err(e) => {
                self.status = e.to_string();
                return;
            }
        };
        let Some(node_text) = Canvas::from_markdown(&located.raw)
            .ok()
            .and_then(|c| c.node(&located.local_id).map(|n| n.text.clone()))
        else {
            self.status = format!("node {node_id:?} not found");
            return;
        };
        let blocks = scan_runnable_blocks(&node_id, &node_text);
        if blocks.is_empty() {
            self.status = "nothing runnable in this node".into();
            return;
        }

        if blocks.len() == 1 {
            let name = blocks[0]
                .name
                .clone()
                .expect("scan_runnable_blocks always names its blocks");
            self.start_run(node_id, name, with_deps).await;
            return;
        }

        let default_name = fence::default_block(&node_id, &blocks)
            .ok()
            .flatten()
            .and_then(|b| b.name.clone());
        let choices = blocks
            .iter()
            .map(|b| {
                let name = b
                    .name
                    .clone()
                    .expect("scan_runnable_blocks always names its blocks");
                BlockChoice {
                    is_default: Some(&name) == default_name.as_ref(),
                    name,
                    cache: b.cache,
                    tty: b.tty,
                }
            })
            .collect();
        self.block_picker = Some(BlockPickerState {
            node_id,
            blocks: choices,
            selected: 0,
            with_deps,
        });
    }

    async fn start_run(&mut self, node_id: String, block_name: String, with_deps: bool) {
        let target = BlockAddr::new(node_id, block_name);
        // Include-resolved (not just `self.canvas`) so `target` can name a
        // node spliced in from an `include` — same namespaced id
        // `self.display_canvas`/the tree the caller picked `node_id` from
        // already uses — and so a `deps=`/`from=` chain can cross from one
        // file into another. `advance_run` (below) is what actually finds
        // each step's *own* file to run/cache in via `locate_node`; this
        // resolved canvas only has to be complete enough to trace the
        // dependency graph, not track which file owns which node.
        let resolved = meshfox_core::include::resolve(&self.canvas, &self.canvas_path)
            .unwrap_or_else(|_| self.canvas.clone());
        // Even with `with_deps` false (the plain "run", skipping `deps=`),
        // the target's own `from=` sources still have to run first — a
        // computed variable has no value at all otherwise, unlike a
        // `deps=` dependency that might already have fresh cached output.
        // See `meshfox_core::resolve_run_chain`'s own doc comment.
        let chain_result = if with_deps {
            meshfox_core::deps::resolve_chain(&resolved, target)
        } else {
            meshfox_core::deps::resolve_from_chain(&resolved, target)
        };
        let chain = match chain_result {
            Ok(c) => c,
            Err(e) => {
                self.status = format!("dependency error: {e}");
                return;
            }
        };

        // Dry-run pass over the whole chain, up front — mirrors the web
        // server's own `run_block`/`run_tty_chain` (see their shared doc
        // comment on the equivalent call) — so a `!` (sync) `deps=` edge
        // can force its dependency to run for real when the block that
        // declared the edge is about to, even though that block comes
        // *later* in `chain`'s dependency order than its dependency does.
        let forced_reruns = match meshfox_core::compute_forced_reruns(
            &resolved,
            &chain,
            |block, computed| self.fingerprint_vars_for(block, computed),
            |addr| {
                self.session_runs
                    .get(&(addr.node_id.clone(), addr.block_name.clone()))
                    .map(|r| (r.fingerprint.clone(), r.produced_vars.clone()))
            },
        ) {
            Ok(f) => f,
            Err(e) => {
                self.status = format!("dependency error: {e}");
                return;
            }
        };

        self.run = Some(RunState {
            chain,
            idx: 0,
            proc: None,
            lines: Vec::new(),
            full_output: String::new(),
            step_started: std::time::Instant::now(),
            current_node_text: String::new(),
            had_failure: false,
            killed: false,
            finished: false,
            pending_vars_out: None,
            forced_reruns,
        });
        self.run_overrides.clear();
        self.run_computed.clear();
        self.status.clear();
        self.advance_run().await;
    }

    /// Shared handling for a `resolve_block_env` result, used for both a
    /// block's `env=` refs and (separately) any `$NAME` refs inside its
    /// `interpreter=`: a hard failure on an unresolved `from=` source parks
    /// the run as failed, a `missing` var opens the prompt form, and
    /// otherwise the resolved values are returned so the caller can carry
    /// on. Either parking path already did all the necessary `self.*`
    /// mutation, so the caller just needs to `return` when this is `None`.
    fn park_on_unresolved(
        &mut self,
        resolution: BlockEnvResolution,
    ) -> Option<std::collections::HashMap<String, String>> {
        if !resolution.unresolved_from.is_empty() {
            let names: Vec<&str> = resolution
                .unresolved_from
                .iter()
                .map(|d| d.name.as_str())
                .collect();
            self.status = format!(
                "computed variable(s) {} have no value — their from= source block either \
                 didn't run, failed, or didn't produce them",
                names.join(", ")
            );
            if let Some(run) = &mut self.run {
                run.finished = true;
                run.had_failure = true;
            }
            return None;
        }
        if !resolution.missing.is_empty() {
            let inputs = resolution
                .missing
                .iter()
                .map(|d| initial_field_input(d, &self.var_cache))
                .collect();
            self.var_form = Some(VarFormState {
                decls: resolution.missing,
                inputs,
                selected: 0,
                configuring: false,
            });
            return None;
        }
        Some(resolution.env)
    }

    /// `interpreter=`'s own `$NAME` refs, as `EnvRef`s (local name == var
    /// name — an interpreter substitution has no `local=name` renaming of
    /// its own, unlike `env=`) — `None` when `block.interpreter` has no
    /// such reference at all, not merely that resolution hasn't happened
    /// yet. Shared by `advance_run` and `fingerprint_vars_for`.
    fn interp_env_refs(block: &meshfox_core::CodeBlock) -> Vec<meshfox_core::EnvRef> {
        block
            .interpreter
            .as_deref()
            .map(meshfox_core::interpreter_var_refs)
            .unwrap_or_default()
            .into_iter()
            .map(|n| meshfox_core::EnvRef { local_name: n.clone(), var_name: n })
            .collect()
    }

    /// `session_fingerprint` wants var-name-keyed values; `env_resolution`/
    /// `interp_resolution` (`BlockEnvResolution::env`) are local-name-keyed
    /// (relabeled per `env=`), so project back through `block.env`'s own
    /// pairs — for `interp_resolution`, local name == var name already (see
    /// `interp_env_refs`), so its `env` needs no projection. Shared by
    /// `advance_run` (which already has both resolutions in hand) and
    /// `fingerprint_vars_for` (which computes its own, purely to feed this).
    fn project_fingerprint_vars(
        block: &meshfox_core::CodeBlock,
        env_resolution: &BlockEnvResolution,
        interp_resolution: Option<&BlockEnvResolution>,
    ) -> HashMap<String, String> {
        let mut fingerprint_vars: HashMap<String, String> = HashMap::new();
        for env_ref in &block.env {
            if let Some(v) = env_resolution.env.get(&env_ref.local_name) {
                fingerprint_vars.insert(env_ref.var_name.clone(), v.clone());
            }
        }
        if let Some(ir) = interp_resolution {
            fingerprint_vars.extend(ir.env.clone());
        }
        fingerprint_vars
    }

    /// The var-name-keyed value map `meshfox_core::session_fingerprint`
    /// needs for `block`, resolved against `self.run_overrides`/
    /// `self.var_cache`/`default` the same way `advance_run` resolves them
    /// before actually running a step, with `computed` standing in for
    /// `self.run_computed` — a caller simulating a chain that hasn't
    /// actually run yet (`start_run`'s own `compute_forced_reruns` call)
    /// passes its own simulated copy instead. Never parks on anything
    /// missing: a best-effort lookup purely to know what a fingerprint
    /// comparison would see, same as `advance_run`'s own equivalent
    /// resolve-then-project pair — kept separate from it (rather than
    /// having `advance_run` call this too) so a step that isn't skippable
    /// doesn't resolve `env=`/`interpreter=` twice.
    fn fingerprint_vars_for(
        &self,
        block: &meshfox_core::CodeBlock,
        computed: &HashMap<String, String>,
    ) -> HashMap<String, String> {
        let env_resolution =
            resolve_block_env(&block.env, &self.decls, &self.run_overrides, &self.var_cache, computed);
        let interp_refs = Self::interp_env_refs(block);
        let interp_resolution = if interp_refs.is_empty() {
            None
        } else {
            Some(resolve_block_env(
                &interp_refs,
                &self.decls,
                &self.run_overrides,
                &self.var_cache,
                computed,
            ))
        };
        Self::project_fingerprint_vars(block, &env_resolution, interp_resolution.as_ref())
    }

    /// Drives the current run forward until it either starts a process
    /// (returns, so the event loop can start polling its output), pauses
    /// for a missing `meshfox:var` (returns, waiting on the prompt), or the
    /// chain runs out (marks `self.run` finished — see `RunState::finished`).
    async fn advance_run(&mut self) {
        // Loops rather than a single lookup: a step found to be "already
        // fresh this session" (see `session_runs`) is skipped in place —
        // folding whatever it produced last time straight into
        // `run_computed` and moving on to the next `idx` — without ever
        // reaching the spawn logic below. The loop only ever exits via an
        // early `return` (chain exhausted, an error, or a step that
        // genuinely needs to run) or falls through to spawn a step once
        // one is found that isn't skippable.
        let (addr, located, node_text, block, env_resolution, interp_resolution) = loop {
            let Some((idx, len)) = self.run.as_ref().map(|r| (r.idx, r.chain.len())) else {
                return;
            };
            if idx >= len {
                let (killed, had_failure) = self
                    .run
                    .as_ref()
                    .map(|r| (r.killed, r.had_failure))
                    .unwrap_or_default();
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
            // Finds which real file `addr.node_id` actually lives in —
            // itself (the primary document) or an `include` target
            // elsewhere on disk, however deeply nested — so a block
            // spliced in from an `include` is addressable here at all, and
            // its own `cache`/`PWD` land in the right file (see
            // `Node::cwd`'s own reasoning; mirrors the web UI's
            // `run_block`/`run_tty_chain`).
            let located =
                match meshfox_core::locate_node(&self.raw, &self.canvas_path, &addr.node_id) {
                    Ok(l) => l,
                    Err(e) => {
                        self.status = e.to_string();
                        if let Some(run) = &mut self.run {
                            run.finished = true;
                            run.had_failure = true;
                        }
                        return;
                    }
                };
            let Some(node_text) = Canvas::from_markdown(&located.raw)
                .ok()
                .and_then(|c| c.node(&located.local_id).map(|n| n.text.clone()))
            else {
                self.status = format!("node {:?} not found", addr.node_id);
                if let Some(run) = &mut self.run {
                    run.finished = true;
                    run.had_failure = true;
                }
                return;
            };
            let Some(block) = scan_runnable_blocks(&addr.node_id, &node_text)
                .into_iter()
                .find(|b| b.name.as_deref() == Some(addr.block_name.as_str()))
            else {
                self.status = format!(
                    "block {:?} not found in {:?}",
                    addr.block_name, addr.node_id
                );
                if let Some(run) = &mut self.run {
                    run.finished = true;
                    run.had_failure = true;
                }
                return;
            };

            // Resolved *without* parking on anything still missing — a
            // best-effort lookup against whatever's already available
            // (overrides/cache/default/computed), purely to know the
            // values feeding `session_fingerprint` below. A block that
            // ends up skippable never needs to actually park (which could
            // otherwise force an interactive prompt just to decide whether
            // to skip); a block that isn't skippable reuses this exact
            // same resolution afterward instead of resolving twice.
            let env_resolution = resolve_block_env(
                &block.env,
                &self.decls,
                &self.run_overrides,
                &self.var_cache,
                &self.run_computed,
            );
            let interp_refs = Self::interp_env_refs(&block);
            let interp_resolution = if interp_refs.is_empty() {
                None
            } else {
                Some(resolve_block_env(
                    &interp_refs,
                    &self.decls,
                    &self.run_overrides,
                    &self.var_cache,
                    &self.run_computed,
                ))
            };
            let fingerprint_vars =
                Self::project_fingerprint_vars(&block, &env_resolution, interp_resolution.as_ref());

            // The block actually requested (always the chain's own last
            // entry) always runs for real; only a pulled-in dependency is
            // ever eligible to be skipped as "already fresh this session"
            // — see `App::session_runs`. A block's own `always` flag opts
            // it out of the skip entirely, even as a pulled-in dependency;
            // so does `RunState::forced_reruns` (a `!` `deps=` edge whose
            // declaring block is itself running for real this pass — see
            // `meshfox_core::compute_forced_reruns`), computed once up
            // front in `start_run` since it needs to look *ahead* in the
            // chain, past this block's own position.
            let is_requested_target = idx + 1 == len;
            let live_fingerprint = meshfox_core::session_fingerprint(&block, &fingerprint_vars);
            let forced = self.run.as_ref().is_some_and(|r| r.forced_reruns.contains(&addr));
            if !is_requested_target && !block.always && !forced {
                let key = (addr.node_id.clone(), addr.block_name.clone());
                if let Some(session_run) =
                    self.session_runs.get(&key).filter(|r| r.fingerprint == live_fingerprint)
                {
                    self.run_computed.extend(session_run.produced_vars.clone());
                    if let Some(run) = &mut self.run {
                        run.lines.push(format!(
                            "==> {} (skipped — already ran this session, unchanged · {})",
                            addr.block_name,
                            meshfox_core::format_duration_ms(session_run.duration_ms)
                        ));
                        // No per-line fold in this transcript (unlike the
                        // web UI's collapsible section — see
                        // `web/src/MeshNode.tsx`'s `LiveRunOutput`), so the
                        // last real run's output is just printed straight
                        // through, same as if it had run again.
                        run.lines.extend(session_run.output.lines().map(str::to_string));
                        run.idx += 1;
                    }
                    continue;
                }
            }

            break (addr, located, node_text, block, env_resolution, interp_resolution);
        };

        let Some(mut env) = self.park_on_unresolved(env_resolution) else {
            return;
        };

        // A `$NAME` reference inside `interpreter=` needs exactly the same
        // resolution `env=` just got — a second, independent pass (rather
        // than folding these names into `block.env` itself) so a
        // referenced variable never silently ends up in the spawned
        // process's own environment just because `interpreter=` happened
        // to need it too; only a real `env=` entry ever does that.
        let effective_interpreter = match &block.interpreter {
            None => None,
            Some(spec) => match interp_resolution {
                None => Some(spec.clone()),
                Some(interp_resolution) => {
                    let Some(values) = self.park_on_unresolved(interp_resolution) else {
                        return;
                    };
                    Some(meshfox_core::resolve_interpreter(spec, &values))
                }
            },
        };

        // If some declared variable is `from=`-sourced from *this* block,
        // give it a fresh output file to write `NAME=value` lines to —
        // see `meshfox_core::varout`. Ordinary blocks never see this env
        // var at all.
        let from_decls: Vec<VarDecl> = meshfox_core::from_targets(&self.decls, &addr)
            .into_iter()
            .cloned()
            .collect();
        let vars_out_path = if from_decls.is_empty() {
            None
        } else {
            let path = meshfox_core::allocate_vars_out_path();
            env.insert(
                meshfox_core::VARS_OUT_ENV.to_string(),
                path.display().to_string(),
            );
            Some(path)
        };
        if let Some(run) = &mut self.run {
            run.pending_vars_out = vars_out_path.map(|p| (p, from_decls));
        }

        // `tty` hands the *real* terminal over to the child, same as
        // `meshfox run` does — see `mod.rs::run_tty_handoff`, which is
        // what actually leaves the alternate screen/raw mode, runs it,
        // and comes back. `App` never touches the terminal itself, so
        // it just parks the request and returns; `mod.rs`'s loop picks
        // `pending_tty` up before its next `select!` and calls
        // `resume_after_tty` once the child exits.
        let cwd = crate::canvas_root_dir(located.origin.as_deref().unwrap_or(&self.canvas_path))
            .to_path_buf();

        if block.tty {
            if let Some(run) = &mut self.run {
                run.lines.push(format!(
                    "==> {} (interactive — handing over the terminal)",
                    addr.block_name
                ));
            }
            self.pending_tty = Some(PendingTty {
                block_name: addr.block_name.clone(),
                code: block.code.clone(),
                interpreter: effective_interpreter,
                env,
                cwd,
                autoclose: block.autoclose,
            });
            return;
        }

        let mut resolved_block = block.clone();
        resolved_block.interpreter = effective_interpreter;
        match meshfox_server::stream_exec::spawn_block(&resolved_block, &env, Some(&cwd)) {
            Ok(proc) => {
                let run = self.run.as_mut().unwrap();
                run.proc = Some(proc);
                run.current_node_text = node_text;
                run.full_output.clear();
                run.step_started = std::time::Instant::now();
                run.lines.push(format!("==> {}", addr.block_name));
            }
            Err(e) => {
                self.status = format!("failed to run {:?}: {e}", addr.block_name);
                if let Some(run) = &mut self.run {
                    run.finished = true;
                    run.had_failure = true;
                }
            }
        }
    }

    /// Reads back whatever the step that just finished wrote to its own
    /// vars-out file (if `advance_run` gave it one — i.e. it was a `from=`
    /// target for something), type-validates each declared value, and
    /// folds it into `self.run_computed` for whatever later step in this
    /// run declared `from=` it. Only trusted on a `0` exit. Always clears
    /// `pending_vars_out` (there's nothing left to consume it once this
    /// step is done, tty or not). Returns whether anything about this went
    /// wrong — the caller treats that the same as a nonzero exit.
    fn apply_pending_vars_out(&mut self, exit_code: i32) -> bool {
        let Some((path, from_decls)) = self
            .run
            .as_mut()
            .and_then(|r| r.pending_vars_out.take())
        else {
            return false;
        };
        let produced = match meshfox_core::read_and_cleanup_vars_out(&path) {
            Ok(produced) => produced,
            Err(e) => {
                self.status = format!("failed to read computed variables: {e}");
                return true;
            }
        };
        if exit_code != 0 {
            return false; // handled by the caller's own exit-code check
        }
        let mut had_error = false;
        for decl in &from_decls {
            match produced.get(&decl.name) {
                Some(value) => match meshfox_core::validate_value(decl, value) {
                    Ok(()) => {
                        self.run_computed.insert(decl.name.clone(), value.clone());
                    }
                    Err(e) => {
                        self.status = format!("computed variable {:?} is invalid: {e}", decl.name);
                        had_error = true;
                    }
                },
                None => {
                    self.status = format!(
                        "block produced no value for {:?} (declared from=\"{}\")",
                        decl.name,
                        decl.from
                            .as_ref()
                            .map(|f| format!(
                                "{}/{}",
                                f.node_id.as_deref().unwrap_or(""),
                                f.block_name
                            ))
                            .unwrap_or_default()
                    );
                    had_error = true;
                }
            }
        }
        had_error
    }

    /// Runs a runnable `file` node (`type="file"` with both `target` and
    /// `interpreter` set) as `interpreter target`, streaming output live —
    /// the TUI counterpart to the web UI's own "▷ run" button on a `file`
    /// node's title bar (`run_file_node` in `crates/server/src/lib.rs`),
    /// previously the only way to run one at all. `node`'s own
    /// `origin_path`, when set (spliced in from an `include`), names the
    /// *real* file `target`/`PWD` resolve relative to, confined to it —
    /// same boundary the web UI's `resolve_confined_target` enforces.
    async fn start_file_run(&mut self, node_id: String, node: Node) {
        let interpreter = node
            .interpreter
            .as_deref()
            .expect("checked by is_runnable_file");
        let (program, args) = match meshfox_core::split_interpreter(interpreter) {
            Some(pair) => pair,
            None => {
                self.status =
                    format!("interpreter={interpreter:?} isn't a valid shell-word command");
                return;
            }
        };
        let target = node.target.as_deref().expect("checked by is_runnable_file");
        let origin_path = node
            .origin_path
            .as_deref()
            .map(Path::new)
            .unwrap_or(&self.canvas_path);
        let origin_dir = crate::canvas_root_dir(origin_path);
        let resolved_target = match meshfox_core::confine(origin_dir, target) {
            Ok(p) => p,
            Err(e) => {
                self.status = e.to_string();
                return;
            }
        };

        match meshfox_server::stream_exec::spawn_process(
            &program,
            args.iter()
                .map(std::ffi::OsStr::new)
                .chain([resolved_target.as_os_str()]),
            Some(origin_dir),
        ) {
            Ok(proc) => {
                self.status.clear();
                self.file_run = Some(FileRunState {
                    proc: Some(proc),
                    lines: vec![format!("==> {node_id}")],
                    had_failure: false,
                    finished: false,
                });
            }
            Err(e) => {
                self.status = format!("failed to run {node_id:?}: {e}");
            }
        }
    }

    /// Called by `mod.rs` once `file_run`'s own output channel closes —
    /// mirrors `on_output_line`, minus everything that only applies to a
    /// fenced block (no `cache`, no `meshfox:var`, no chain to advance).
    pub async fn on_file_output_line(&mut self, line: Option<String>) {
        let Some(run) = &mut self.file_run else { return };
        match line {
            Some(text) => run.lines.push(text),
            None => {
                let mut proc = run.proc.take().expect("output channel closed without a process");
                let status = proc.child.wait().await;
                let exit_code = status.ok().and_then(|s| s.code()).unwrap_or(-1);
                let run = self.file_run.as_mut().unwrap();
                run.lines.push(format!("(exit {exit_code})"));
                run.had_failure = exit_code != 0;
                run.finished = true;
            }
        }
    }

    /// Called by `mod.rs` once a `tty` handoff's child has exited —
    /// `tty`/`cache` are mutually exclusive (a `meshfox validate` error),
    /// so there's never cached output to write back for this step, unlike
    /// `on_output_line`'s non-tty completion path.
    pub async fn resume_after_tty(&mut self, exit_code: i32) {
        let from_value_error = self.apply_pending_vars_out(exit_code);
        if let Some(run) = &mut self.run {
            run.lines.push(format!("(exited {exit_code})"));
            if exit_code != 0 || from_value_error {
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
                let mut proc = self
                    .run
                    .as_mut()
                    .unwrap()
                    .proc
                    .take()
                    .expect("output channel closed without a process");
                let status = proc.child.wait().await;
                let exit_code = status.ok().and_then(|s| s.code()).unwrap_or(-1);

                let (addr, node_text, full_output, duration_ms) = {
                    let run = self.run.as_ref().unwrap();
                    (
                        run.chain[run.idx].clone(),
                        run.current_node_text.clone(),
                        run.full_output.clone(),
                        run.step_started.elapsed().as_millis() as u64,
                    )
                };
                self.run.as_mut().unwrap().lines.push(format!(
                    "(exit {exit_code} · {})",
                    meshfox_core::format_duration_ms(duration_ms)
                ));

                let from_value_error = self.apply_pending_vars_out(exit_code);

                if let Some(block) = scan_runnable_blocks(&addr.node_id, &node_text)
                    .into_iter()
                    .find(|b| b.name.as_deref() == Some(addr.block_name.as_str()))
                {
                    if block.cache {
                        let result = ExecOutput {
                            exit_code,
                            output: full_output.clone(),
                            duration_ms,
                        };
                        // Re-located (rather than stashed from `advance_run`)
                        // since it's cheap and this is the only place that
                        // needs it again — an earlier step in this same
                        // chain may have already patched this exact file
                        // (primary or `include` target), so re-reading it
                        // fresh here (via `locate_node`) picks that up
                        // rather than risking a stale in-memory copy.
                        if let Ok(located) = meshfox_core::locate_node(
                            &self.raw,
                            &self.canvas_path,
                            &addr.node_id,
                        ) {
                            if let Some(updated) =
                                write_output(&node_text, &addr.block_name, &result)
                            {
                                if let Some(patched) = mdcanvas::set_node_body(
                                    &located.raw,
                                    &located.local_id,
                                    &updated,
                                ) {
                                    match &located.origin {
                                        None => {
                                            self.raw = patched;
                                            let _ =
                                                std::fs::write(&self.canvas_path, &self.raw);
                                            *self.known_raw.lock().unwrap() = self.raw.clone();
                                            if let Ok(reparsed) = Canvas::from_markdown(&self.raw)
                                            {
                                                self.canvas = reparsed;
                                            }
                                        }
                                        Some(path) => {
                                            let _ = std::fs::write(path, &patched);
                                        }
                                    }
                                    self.rebuild_display_canvas();
                                    self.rebuild_rows();
                                    self.render_current_document();
                                }
                            }
                        }
                    }

                    if exit_code == 0 && !from_value_error {
                        let produced_vars: HashMap<String, String> =
                            meshfox_core::from_targets(&self.decls, &addr)
                                .into_iter()
                                .filter_map(|decl| {
                                    self.run_computed
                                        .get(&decl.name)
                                        .map(|v| (decl.name.clone(), v.clone()))
                                })
                                .collect();
                        self.session_runs.insert(
                            (addr.node_id.clone(), addr.block_name.clone()),
                            SessionRun {
                                fingerprint: meshfox_core::fingerprint(&block),
                                produced_vars,
                                output: full_output,
                                duration_ms,
                            },
                        );
                    }
                }

                let run = self.run.as_mut().unwrap();
                if exit_code != 0 || from_value_error {
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
        if let Some(run) = &mut self.file_run {
            if let Some(proc) = &run.proc {
                let _ = proc.kill();
            }
            self.status = "killing...".into();
        }
    }

    /// Forgets every block's session-freshness record (`session_runs`) —
    /// the next chain run re-runs every pulled-in dependency for real
    /// instead of skipping whichever ones still look unchanged since their
    /// last run this session. Mirrors the web server's own `POST
    /// /api/session/reset`. Purely in-memory, so this never touches the
    /// canvas file itself or any persisted `<!-- meshfox:output ... -->`
    /// cache. See TODO.canvas.md: "Сброс сессии".
    fn reset_session(&mut self) {
        self.session_runs.clear();
        self.status = "session reset".into();
    }

    /// While `reset_session_confirm` is up (see `on_key`'s early dispatch) —
    /// `y`/Enter confirms, `n`/Esc backs out untouched. Same
    /// confirm-before-acting gate the web UI's own `ResetSessionConfirmDialog`
    /// puts in front of its "↺ reset session" button, for the same reason
    /// (see that component's doc comment): the reset is harmless to the
    /// file/cache but still easy to trigger by accident and mildly costly to
    /// shrug off, so `S` alone shouldn't fire it immediately.
    fn on_reset_session_confirm_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('y') | KeyCode::Enter => {
                self.reset_session_confirm = false;
                self.reset_session();
            }
            KeyCode::Char('n') | KeyCode::Esc => {
                self.reset_session_confirm = false;
                self.status = "session reset cancelled".into();
            }
            _ => {}
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
            let vf = self
                .var_form
                .as_ref()
                .expect("guarded by on_key's is_some() check");
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
        let Some(vf) = self.var_form.take() else {
            return;
        };
        for (decl, value) in vf.decls.iter().zip(vf.inputs.iter()) {
            if !decl.secret && !decl.session {
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

    /// `c` — walks every declared non-secret, non-session variable in the
    /// whole document (regardless of which, if any, block currently
    /// references it via `env=`), same scope `meshfox configure` covers,
    /// all shown at once with each one's currently-resolved value as the
    /// pre-filled suggestion. Confirming (even unchanged) writes it to the
    /// cache — the browser counterpart is `VarsForm` opened from the
    /// toolbar's "configure" button; see `crates/server/src/lib.rs`'s
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
        let decls: Vec<VarDecl> = self
            .decls
            .iter()
            .filter(|d| !d.secret && !d.session)
            .cloned()
            .collect();
        if decls.is_empty() {
            self.status =
                "meshfox: this canvas declares no configurable (non-secret, non-session) variable(s)"
                    .into();
            return;
        }
        let inputs = decls
            .iter()
            .map(|d| initial_field_input(d, &self.var_cache))
            .collect();
        self.var_form = Some(VarFormState {
            decls,
            inputs,
            selected: 0,
            configuring: true,
        });
    }

    /// Whether the footer/help hint for `c` (configure) should be shown at
    /// all — same "configurable" definition `trigger_configure` itself
    /// uses (declared, non-secret, non-session; a document that declares
    /// only secret/session variables has nothing `c` could usefully do,
    /// same as the CLI's own `configure` skipping them).
    pub fn has_configurable_vars(&self) -> bool {
        self.decls.iter().any(|d| !d.secret && !d.session)
    }

    fn cancel_var_form(&mut self) {
        let Some(vf) = self.var_form.take() else {
            return;
        };
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
    let mut resolved =
        meshfox_core::include::resolve(canvas, canvas_path).unwrap_or_else(|_| canvas.clone());
    // Populates every node's `constraint_results` (see
    // `meshfox_core::constraint::annotate_status`) so `tree::flatten` and
    // `constraint_stats` below can read pass/fail straight off the tree
    // without re-evaluating it themselves.
    let canvas_dir = canvas_path.parent().filter(|p| !p.as_os_str().is_empty());
    let canvas_dir = Some(canvas_dir.unwrap_or(Path::new(".")));
    meshfox_core::constraint::annotate_status(&mut resolved, canvas_dir);
    // Same "populate before flatten reads it" idiom as `constraint_results`
    // right above, for `meshfox:tag-color` (TODO.canvas.md: "Node colour by
    // tag") — best-effort, a malformed declaration just means no node
    // falls back to a tag-derived color, same split `meshfox validate`
    // otherwise catches loudly.
    meshfox_core::annotate_effective_colors(&mut resolved);
    resolved
}

/// `(total, failed)` across every embedded constraint fence in `canvas`
/// (already `annotate_status`-ed by `resolve_includes`) — `None` when there
/// are none at all. See `App::constraint_stats`.
fn constraint_stats(canvas: &Canvas) -> Option<(usize, usize)> {
    let results: Vec<_> = canvas
        .nodes
        .iter()
        .flat_map(|n| n.constraint_results.iter())
        .collect();
    if results.is_empty() {
        return None;
    }
    let failed = results.iter().filter(|r| !r.ok).count();
    Some((results.len(), failed))
}

/// A synthetic `doc_images` key for a `link` node's preview image — there's
/// no real file on disk to key it by (unlike every other `Segment::Image`),
/// so this just needs to be stable and collision-free per image URL, not an
/// actual path anything ever opens.
fn link_preview_image_path(image_url: &str) -> PathBuf {
    PathBuf::from(format!("meshfox-link-preview:{image_url}"))
}

/// TODO.canvas.md: "Base64 image" — a `data:image/...;base64,...` URL,
/// consistent with `crate::pdf`/`staticgen`'s own already-working pass-
/// through (a browser/headless-Chrome decodes it natively there) and with
/// the web UI's `img` `urlTransform`. Decoded synchronously and entirely
/// in memory — unlike `App::maybe_fetch_link_preview_image`'s async fetch,
/// there's no network round-trip to wait on, the bytes are already right
/// there in the document.
fn decode_data_url_image(url: &str) -> Option<image::DynamicImage> {
    let rest = url.strip_prefix("data:")?;
    let (meta, payload) = rest.split_once(',')?;
    if !meta.ends_with(";base64") {
        return None;
    }
    let bytes = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, payload).ok()?;
    image::load_from_memory(&bytes).ok()
}

/// `path` is a real file for every ordinary `Segment::Image`, but a
/// `data:` URL string doubling as its own synthetic cache key for one
/// pasted/embedded directly in the document (see `markdown::Renderer`'s
/// own `Tag::Image` handling) — same "no file on disk to key it by"
/// situation `link_preview_image_path` already has, just without a
/// prefix: a `data:` URL can't collide with a real relative path.
fn load_image_protocol(
    picker: &mut Picker,
    path: &Path,
    width_percent: Option<u32>,
    height_percent: Option<u32>,
) -> Option<Protocol> {
    let dyn_img = match path.to_str().filter(|s| s.starts_with("data:")) {
        Some(data_url) => decode_data_url_image(data_url)?,
        None => image::ImageReader::open(path)
            .ok()?
            .with_guessed_format()
            .ok()?
            .decode()
            .ok()?,
    };
    let budget = image_size_budget(width_percent, height_percent);
    picker
        .new_protocol(dyn_img, budget, ratatui_image::Resize::Fit(None))
        .ok()
}

/// TODO.canvas.md: "Формальные граматики для meshfox:*" subtree ->
/// "Атрибуты картинок в markdown" — this terminal's own fixed 56x24
/// "how big can an image get" budget, scaled by a `{width=NN%}`/
/// `{height=NN%}` hint (see `markdown::Segment::Image`). There's no
/// pixel grid a literal `width=300` (no `%`) could map onto without
/// knowing the terminal's own font metrics, so only the percent form has
/// any effect — an absolute value is silently ignored, same fallback
/// every other unsupported bit of this narrow syntax gets rather than
/// guessing. Clamped well away from zero/overflow at either end.
fn image_size_budget(
    width_percent: Option<u32>,
    height_percent: Option<u32>,
) -> ratatui::layout::Size {
    let mut budget = ratatui::layout::Size::new(56, 24);
    if let Some(pct) = width_percent {
        budget.width = ((budget.width as u32 * pct) / 100).clamp(1, 500) as u16;
    }
    if let Some(pct) = height_percent {
        budget.height = ((budget.height as u32 * pct) / 100).clamp(1, 500) as u16;
    }
    budget
}

fn point_in(rect: Rect, x: u16, y: u16) -> bool {
    x >= rect.x && x < rect.x + rect.width && y >= rect.y && y < rect.y + rect.height
}

#[cfg(test)]
mod tests {
    use super::*;

    // TODO.canvas.md: "Base64 image" — `decode_data_url_image`/
    // `load_image_protocol`'s own `data:` branch. `Picker::halfblocks()`
    // needs no real terminal (see its own construction above in
    // `App::new`), so this runs fine in CI, same as everywhere else in
    // this file that already relies on it.
    const ONE_PIXEL_PNG_DATA_URL: &str = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=";

    #[test]
    fn decode_data_url_image_decodes_a_valid_base64_png() {
        let img = decode_data_url_image(ONE_PIXEL_PNG_DATA_URL).expect("valid data: URL");
        assert_eq!((img.width(), img.height()), (1, 1));
    }

    #[test]
    fn decode_data_url_image_rejects_a_non_base64_data_url() {
        assert!(decode_data_url_image("data:image/png,not-base64-payload").is_none());
    }

    #[test]
    fn decode_data_url_image_rejects_a_garbage_payload() {
        assert!(decode_data_url_image("data:image/png;base64,not valid base64!!!").is_none());
    }

    #[test]
    fn load_image_protocol_loads_a_data_url_without_touching_disk() {
        let mut picker = Picker::halfblocks();
        let path = PathBuf::from(ONE_PIXEL_PNG_DATA_URL);
        assert!(load_image_protocol(&mut picker, &path, None, None).is_some());
    }

    // TODO.canvas.md: "Формальные граматики для meshfox:*" subtree ->
    // "Атрибуты картинок в markdown" — `image_size_budget`'s own scaling
    // of the fixed 56x24 budget by a `{width=NN%}`/`{height=NN%}` hint.
    #[test]
    fn image_size_budget_defaults_to_the_fixed_budget() {
        let budget = image_size_budget(None, None);
        assert_eq!((budget.width, budget.height), (56, 24));
    }

    #[test]
    fn image_size_budget_scales_by_percent() {
        let budget = image_size_budget(Some(50), Some(50));
        assert_eq!((budget.width, budget.height), (28, 12));
    }

    #[test]
    fn image_size_budget_clamps_away_from_zero_and_overflow() {
        let tiny = image_size_budget(Some(0), Some(0));
        assert!(tiny.width >= 1 && tiny.height >= 1);
        let huge = image_size_budget(Some(10_000), Some(10_000));
        assert!(huge.width <= 500 && huge.height <= 500);
    }

    /// End-to-end: `start_run`/`advance_run`/`on_output_line` finding,
    /// running, and caching a block that lives inside an `include` target
    /// — same limitation `crates/server/src/lib.rs`'s own
    /// `run_block_include_tests` and `crates/cli/tests/run_cmd.rs` used to
    /// have (a block only reachable through an `include` was simply
    /// unaddressable — `self.canvas` is deliberately never
    /// `include`-resolved, see its own doc comment) before
    /// `meshfox_core::locate_node` was wired into `start_run`/
    /// `advance_run`/`on_output_line`.
    #[tokio::test]
    async fn runs_finds_and_caches_a_block_inside_an_included_canvas() {
        let dir = std::env::temp_dir().join(format!(
            "meshfox-tui-run-include-test-{}",
            uuid_like()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("child.canvas.md"),
            concat!(
                "<!-- meshfox:canvas -->\n# Child\n<!-- meshfox:node id=\"root\" -->\n\n",
                "## Leaf\n<!-- meshfox:node id=\"leaf\" -->\n\n",
                "```bash name=\"report\" cache\npwd -P\n```\n",
            ),
        )
        .unwrap();
        let base_path = dir.join("base.canvas.md");
        std::fs::write(
            &base_path,
            concat!(
                "<!-- meshfox:canvas -->\n# Base\n<!-- meshfox:node id=\"base\" -->\n\n",
                "## Child\n<!-- meshfox:node id=\"child\" type=\"include\" -->\n\n[child](./child.canvas.md)\n",
            ),
        )
        .unwrap();
        let child_path = dir.join("child.canvas.md");

        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(base_path.clone(), tx).unwrap();

        app.start_run("child/leaf".to_string(), "report".to_string(), true)
            .await;

        // Mirrors `mod.rs`'s own main loop: drain the spawned process's
        // output, then signal EOF (`None`) so `on_output_line` reaps it,
        // writes the cache, and advances the chain.
        loop {
            let has_proc = app.run.as_ref().is_some_and(|r| r.proc.is_some());
            if !has_proc {
                break;
            }
            let line = app
                .run
                .as_mut()
                .unwrap()
                .proc
                .as_mut()
                .unwrap()
                .output_rx
                .recv()
                .await;
            app.on_output_line(line).await;
        }

        let run = app.run.as_ref().expect("a run was started");
        assert!(!run.had_failure, "lines: {:?}", run.lines);

        let want_cwd = dir.canonicalize().unwrap().to_string_lossy().into_owned();
        assert!(
            run.lines.iter().any(|l| l == &want_cwd),
            "expected {want_cwd:?} among the run's own output lines, got: {:?}",
            run.lines
        );

        let base_after = std::fs::read_to_string(&base_path).unwrap();
        assert!(!base_after.contains("meshfox:output"));
        let child_after = std::fs::read_to_string(&child_path).unwrap();
        assert!(child_after.contains("meshfox:output name=\"report\""));

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `trigger_run` (the actual `r`/`R` keybinding handler) used to guard
    /// on `self.canvas.node(&node_id)` directly — which, unlike
    /// `self.display_canvas`, is deliberately never `include`-resolved —
    /// so selecting a row spliced in from an `include` and pressing `r`
    /// always bailed with "this comes from an `include`...", regardless of
    /// what `start_run`/`advance_run` themselves could already handle.
    /// Covers both a fenced block and a runnable `file` node, since they
    /// take different branches inside `trigger_run`.
    #[tokio::test]
    async fn trigger_run_reaches_both_a_block_and_a_file_node_inside_an_included_canvas() {
        let dir = std::env::temp_dir().join(format!(
            "meshfox-tui-trigger-run-include-test-{}",
            uuid_like()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("seed.sh"), "#!/bin/sh\necho hi from seed\n").unwrap();
        std::fs::write(
            dir.join("child.canvas.md"),
            concat!(
                "<!-- meshfox:canvas -->\n# Child\n<!-- meshfox:node id=\"root\" -->\n\n",
                "## Leaf\n<!-- meshfox:node id=\"leaf\" -->\n\n",
                "```bash name=\"report\" cache\necho hi from leaf\n```\n\n",
                "## Seed\n<!-- meshfox:node id=\"seed\" type=\"file\" interpreter=\"bash\" -->\n\n",
                "[seed](./seed.sh)\n",
            ),
        )
        .unwrap();
        let base_path = dir.join("base.canvas.md");
        std::fs::write(
            &base_path,
            concat!(
                "<!-- meshfox:canvas -->\n# Base\n<!-- meshfox:node id=\"base\" -->\n\n",
                "## Child\n<!-- meshfox:node id=\"child\" type=\"include\" -->\n\n[child](./child.canvas.md)\n",
            ),
        )
        .unwrap();

        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(base_path.clone(), tx).unwrap();
        // Expand ancestors so the rows under test are actually visible —
        // `tree::flatten` only ever auto-expands depth 0 (the root).
        app.expanded.insert("child".to_string());
        app.expanded.insert("child/root".to_string());
        app.rebuild_rows();

        let leaf_idx = app
            .rows
            .iter()
            .position(|r| r.node_id == "child/leaf")
            .expect("child/leaf row visible");
        app.selected = leaf_idx;
        app.trigger_run(true).await;
        assert!(
            app.status.is_empty(),
            "expected trigger_run to actually start a run, got status: {:?}",
            app.status
        );
        loop {
            if !app.run.as_ref().is_some_and(|r| r.proc.is_some()) {
                break;
            }
            let line = app.run.as_mut().unwrap().proc.as_mut().unwrap().output_rx.recv().await;
            app.on_output_line(line).await;
        }
        let run = app.run.as_ref().expect("a run was started");
        assert!(!run.had_failure, "lines: {:?}", run.lines);
        let child_after = std::fs::read_to_string(dir.join("child.canvas.md")).unwrap();
        assert!(child_after.contains("meshfox:output name=\"report\""));

        app.run = None;
        let seed_idx = app
            .rows
            .iter()
            .position(|r| r.node_id == "child/seed")
            .expect("child/seed row visible");
        app.selected = seed_idx;
        app.trigger_run(true).await;
        loop {
            if !app.file_run.as_ref().is_some_and(|r| r.proc.is_some()) {
                break;
            }
            let line = app
                .file_run
                .as_mut()
                .unwrap()
                .proc
                .as_mut()
                .unwrap()
                .output_rx
                .recv()
                .await;
            app.on_file_output_line(line).await;
        }
        let file_run = app.file_run.as_ref().expect("a file run was started");
        assert!(!file_run.had_failure, "lines: {:?}", file_run.lines);
        assert!(file_run.lines.iter().any(|l| l.contains("hi from seed")));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn reset_session_clears_every_recorded_session_run() {
        let dir = std::env::temp_dir().join(format!("meshfox-tui-reset-session-test-{}", uuid_like()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("canvas.md");
        std::fs::write(
            &path,
            "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n",
        )
        .unwrap();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(path, tx).unwrap();

        app.session_runs.insert(
            ("root".to_string(), "dep".to_string()),
            SessionRun {
                fingerprint: "deadbeef".to_string(),
                produced_vars: HashMap::new(),
                output: "dep-ran\n".to_string(),
                duration_ms: 1,
            },
        );
        assert!(!app.session_runs.is_empty());

        app.reset_session();

        assert!(app.session_runs.is_empty());
        assert_eq!(app.status, "session reset");

        let _ = std::fs::remove_dir_all(&dir);
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[tokio::test]
    async fn s_key_only_resets_the_session_after_confirming() {
        let dir = std::env::temp_dir().join(format!("meshfox-tui-reset-session-confirm-test-{}", uuid_like()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("canvas.md");
        std::fs::write(
            &path,
            "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n",
        )
        .unwrap();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(path, tx).unwrap();
        app.session_runs.insert(
            ("root".to_string(), "dep".to_string()),
            SessionRun {
                fingerprint: "deadbeef".to_string(),
                produced_vars: HashMap::new(),
                output: "dep-ran\n".to_string(),
                duration_ms: 1,
            },
        );

        // `S` alone opens the prompt — doesn't clear anything yet.
        app.on_key(key(KeyCode::Char('S'))).await;
        assert!(app.reset_session_confirm);
        assert!(!app.session_runs.is_empty());

        // `n` backs out untouched.
        app.on_key(key(KeyCode::Char('n'))).await;
        assert!(!app.reset_session_confirm);
        assert!(!app.session_runs.is_empty());

        // `S` then `y` actually resets.
        app.on_key(key(KeyCode::Char('S'))).await;
        app.on_key(key(KeyCode::Char('y'))).await;
        assert!(!app.reset_session_confirm);
        assert!(app.session_runs.is_empty());
        assert_eq!(app.status, "session reset");

        let _ = std::fs::remove_dir_all(&dir);
    }

    fn uuid_like() -> String {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        format!(
            "{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        )
    }
}
