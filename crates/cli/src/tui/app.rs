//! TUI application state and behavior: tree navigation, node-body
//! rendering, and running blocks (deps chain + live streamed output + kill
//! + cache write-back) — the same execution model `meshfox run`/`meshfox
//! view` use, adapted to drive a redraw loop instead of printing to stdout
//!   or a browser.

use std::collections::{HashMap, HashSet};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use meshfox_server::link_preview::{self, PreviewMeta};

use super::image_render::DocumentImage;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use meshfox_core::deps::BlockAddr;
use meshfox_core::fence::{self, scan_runnable_blocks};
use meshfox_core::image_attrs::Background;
use meshfox_core::mdcanvas;
use meshfox_core::vars::{declared_vars, VarDecl, VarType};
use meshfox_core::{Canvas, FileDisplay, Node, NodeType};
use meshfox_server::stream_exec::OutputStream;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::ListState;
use ratatui_image::picker::cap_parser::QueryStdioOptions;
use ratatui_image::picker::{Picker, ProtocolType};
use ratatui_image::protocol::Protocol;
use ratatui_image::sliced::SlicedProtocol;

use super::spatial;
use super::ui;

use super::markdown::{self, ClickRegion, ClickTarget, Highlighter, Segment};
use super::source_editor::{self, SourceEditorOutcome, SourceEditorState};
use super::table::{self, TableData, TableInput, TableMode};
use super::tree::{self, TreeRow};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Tree,
    Document,
    Output,
}

/// How close together two `MouseEventKind::Down`s at the same cell have to
/// land to count as a double-click (`App::on_mouse`'s own `last_click`) —
/// a real terminal never sends "double-click" as its own event. Roughly
/// the same ballpark most desktop OSes default their own double-click
/// interval to.
const DOUBLE_CLICK_WINDOW: std::time::Duration = std::time::Duration::from_millis(400);

/// Which border a `MouseEventKind::Down(Left)` landed on, if any — set on
/// `App::resize_drag` for the rest of the gesture (every `Drag(Left)` until
/// the matching `Up(Left)`), so `on_resize_drag` knows which of
/// `tree_width_pct`/`output_height` to update as the cursor moves. See
/// `App::resize_handle_at`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ResizeDrag {
    /// The seam between the tree and document panes — dragging it changes
    /// `tree_width_pct`.
    Vertical,
    /// The seam between the tree/document row and the Output pane below it
    /// — dragging it changes `output_height`.
    Horizontal,
}

/// `App::tree_width_pct`'s own allowed range — narrow enough on either end
/// that neither pane ever shrinks to something unreadable, wide enough that
/// the split still feels adjustable rather than barely-there.
const MIN_TREE_WIDTH_PCT: u16 = 15;
const MAX_TREE_WIDTH_PCT: u16 = 85;
/// `App::output_height`'s own lower bound — enough rows for its own border
/// plus a couple of lines of real output; the upper bound instead comes
/// from `ui::MIN_MAIN_HEIGHT` (leaving the tree/document row usable), since
/// that one depends on the terminal's own current height.
const MIN_OUTPUT_HEIGHT: u16 = 3;

/// A modal form for one or more declared variables at once — all of them
/// visible and editable together (arrow keys move the focused field,
/// Enter submits the whole form), same shape as the web UI's `VarsForm`,
/// rather than asking one at a time. Built either by `advance_run` (just
/// whatever a run still needs) or by `trigger_configure` (every declared
/// non-secret variable in the document).
pub struct VarFormState {
    pub arguments: bool,
    pub decls: Vec<VarDecl>,
    /// Parallel to `decls` — one editable buffer per field, pre-filled
    /// with `current_value` (so submitting untouched just confirms the
    /// suggestion).
    pub inputs: Vec<String>,
    /// Parallel to `decls`/`inputs` — `Some` when the field's `inputs`
    /// entry was pre-filled from shared/global config (see
    /// `meshfox_core::shared_env`) rather than an override/process env/the
    /// on-disk document cache, so `render_var_form` can badge it. Editing
    /// the field just overwrites `inputs` normally; submitting persists it
    /// to the document cache like any other answer, which is the override.
    pub origins: Vec<Option<meshfox_core::SharedOrigin>>,
    pub selected: usize,
    /// Parallel to `decls` — which `secret` fields have "save" ticked
    /// (`Ctrl-S`): the worker then persists that answer to the configured
    /// secret store (or, for `plaintext`, the on-disk cache) instead of
    /// asking again next time. Always `false` for a non-secret field.
    pub save: Vec<bool>,
    /// Parallel to `decls` — why the secret store couldn't be read for a
    /// field (locked keychain, access denied, ...), shown next to it.
    pub errors: Vec<Option<String>>,
    /// Parallel to `decls` — `configuring` only: whether the secret store
    /// already holds a value for a `secret` field (`None` for the rest).
    pub stored: Vec<Option<bool>>,
    /// Parallel to `decls` — `configuring` only (`Ctrl-D`): delete the
    /// stored value of a `secret` field on submit.
    pub clear: Vec<bool>,
    /// `"plaintext"` or `"keychain"` — what "save" means here; labels it.
    pub secret_store: String,
    /// `true` for a `c`-triggered walk of every declared (non-secret)
    /// variable (see `trigger_configure`), `false` for the ordinary
    /// "resolve whatever a run still needs" form (`advance_run`) — purely
    /// so `ui.rs` can title the modal accordingly and so submitting it
    /// doesn't try to resume a run that was never started.
    pub configuring: bool,
}

/// Set by `App::start_run_via_worker` when `GET /api/vars` finds something
/// still unresolved — consumed by `submit_var_form` to resume into
/// `begin_http_run`/`begin_http_tty_run` with the form's answers, instead
/// of `advance_run` (the local-mode resume path `configuring: false`
/// already takes).
pub struct PendingHttpRun {
    pub arguments: bool,
    pub node_id: String,
    pub block_name: String,
    pub with_deps: bool,
    pub port: u16,
    pub force: Option<(String, String)>,
    /// See `App::target_chain_has_tty` — which of `begin_http_run`/
    /// `begin_http_tty_run` to resume into.
    pub is_tty: bool,
}

/// Human-readable label for a dependency's own block — bare `block_name`
/// when it lives in the same node as the block depending on it, otherwise
/// `node_id/block_name` — mirrors the convention `core::fence::fingerprint`
/// already uses for the same "which block, from here" question. `pub(super)`
/// (not just local to this module) since `markdown::render` uses it too, to
/// label a block's own deps line the same way.
pub(super) fn dep_label(current_node_id: &str, node_id: &str, block_name: &str) -> String {
    if node_id == current_node_id {
        block_name.to_string()
    } else {
        format!("{node_id}/{block_name}")
    }
}

pub struct BlockChoice {
    pub name: String,
    pub cache: bool,
    pub tty: bool,
    pub is_default: bool,
    pub is_button: bool,
}

pub struct BlockPickerState {
    pub node_id: String,
    pub blocks: Vec<BlockChoice>,
    pub selected: usize,
    pub with_deps: bool,
    /// `true` when the choice opens the block's run history (`L`) instead of
    /// running it (`r`/`R`) — same "which block?" question, different answer.
    pub history: bool,
}

/// An *already-connected* `/api/run/tty` socket (see
/// `crate::worker_client::tty_connect`) waiting for `mod.rs`'s event loop to
/// hand it the terminal. Connecting happens in `App::begin_http_tty_run` (an
/// ordinary async call, no terminal access needed for that part — only the
/// actual byte relay needs `mod.rs`'s raw-mode/alternate-screen control),
/// so this just carries the live socket itself plus what the handoff needs
/// to print/decide with: `block_name` for the "==> {block}" banner the
/// handoff prints, `autoclose` mirroring the *last* tty block found in the
/// chain scan (see `App::target_chain_tty_autoclose`).
pub struct PendingHttpTty {
    pub socket: crate::worker_client::TtySocket,
    pub block_name: String,
    pub autoclose: bool,
}

/// An `/api/run/tty/attach` socket (see `crate::worker_client::tty_attach`)
/// waiting for `mod.rs`'s event loop to hand it the terminal — the "join a
/// session I didn't start" counterpart to `PendingHttpTty`, from the `t`
/// live-terminals view (`App::attach_selected_tty_session`). No
/// `autoclose` field: an attach-only viewer has no chain of its own to
/// skip back to (see `mod.rs`'s `run_http_tty_attach_handoff`).
pub struct PendingHttpTtyAttach {
    pub socket: crate::worker_client::TtySocket,
    pub block_name: String,
}

/// The `t` live-terminals view — see `App::live_tty_sessions`,
/// `App::on_tty_sessions_view_key`, `ui::render_tty_sessions_view`. Only
/// ever open in worker mode (fallback/no-worker TUI has no registry of
/// *other* connections' sessions to list — see `App::open_tty_sessions_view`).
pub struct TtySessionsViewState {
    pub selected: usize,
}

/// How many undo-log steps the `H` view asks the worker for.
const HISTORY_VIEW_LIMIT: usize = 200;

/// The `H` history view — see `App::open_history_view`,
/// `App::on_history_view_key`, `ui::render_history_view`. Worker-only: the
/// undo log lives in the worker (`/api/history`), same as the web UI's own
/// history menu.
#[derive(Default)]
pub struct HistoryViewState {
    /// Newest first — the order `GET /api/history` returns them in.
    pub entries: Vec<crate::worker_client::HistoryEntryDto>,
    pub selected: usize,
    /// Where the list was last drawn and how far it was scrolled
    /// (`ui::render_history_view` writes both every frame): a mouse click
    /// maps a screen row back to an entry from these, so the hit-test can't
    /// drift from what's actually on screen — the scroll offset in
    /// particular is ratatui's own decision, unknowable from here otherwise.
    pub list_rect: std::cell::Cell<Rect>,
    pub offset: std::cell::Cell<usize>,
}

/// The `L` run-history view — see `App::open_run_history`,
/// `App::on_run_history_key`, `ui::render_run_history_view`. Worker-only: the
/// history lives in the worker's session database. The TUI counterpart of
/// the web UI's `RunHistoryDialog`.
#[derive(Default)]
pub struct RunHistoryViewState {
    pub node_id: String,
    pub block: String,
    /// A `tty` block: its runs keep an outcome and timing but no output.
    pub tty: bool,
    /// Newest first — the order `GET /api/run/history` returns them in.
    pub entries: Vec<crate::worker_client::RunHistoryEntryDto>,
    pub selected: usize,
    /// The selected run's stored output, tagged with which run it belongs to
    /// so a slow load for an earlier selection never shows under a later one.
    pub output: Option<(i64, Vec<(OutputStream, String)>)>,
    /// First output line shown.
    pub output_scroll: usize,
    /// Where the run list was last drawn and how far it was scrolled
    /// (`ui::render_run_history_view` writes both every frame) — what a
    /// mouse click is hit-tested against.
    pub list_rect: std::cell::Cell<Rect>,
    pub list_offset: std::cell::Cell<usize>,
}

impl RunHistoryViewState {
    /// Scrolls the output down by `by` lines, never past its last line.
    pub fn scroll_output_down(&mut self, by: usize) {
        let len = self.output.as_ref().map_or(0, |(_, lines)| lines.len());
        self.output_scroll = (self.output_scroll + by).min(len.saturating_sub(1));
    }
}

/// A source edit waiting for the event loop to hand the terminal to the
/// external editor (`[tui] editor`).
pub struct PendingExternalEditor {
    pub request: super::external_editor::Request,
    /// The source editor was opened only to feed the external one (`e`), so
    /// backing out of it should leave no empty editor behind. `false` when
    /// it came from `Ctrl-o` inside an editor with its own unsaved state.
    pub close_on_cancel: bool,
}

/// API-resolved canvas navigation waiting for the event loop.
pub struct PendingChildCanvas {
    pub path: PathBuf,
    pub port: u16,
    pub node: Option<String>,
}

pub struct RunState {
    pub chain: Vec<BlockAddr>,
    pub idx: usize,
    /// `Some` while `begin_http_run`'s `POST /api/run`(`/force`) stream is
    /// still being forwarded (see `worker_client::run_stream`).
    /// `App::on_run_event` drains this — see `RunState::is_running`.
    pub http_rx: Option<tokio::sync::mpsc::UnboundedReceiver<crate::worker_client::RunEvent>>,
    pub lines: Vec<String>,
    pub full_output: String,
    /// Just this step's stdout lines, reset alongside `full_output` at the
    /// start of each step — see `stderr_only`/`ExecOutput::stdout` for why
    /// `full_output` alone isn't enough for `output="markdown"` mode.
    pub stdout_only: String,
    /// Just this step's stderr lines — see `stdout_only` above.
    pub stderr_only: String,
    /// Whether the step currently occupying `stdout_only`/`stderr_only`
    /// above declares the fence attribute `output="markdown"` — set
    /// alongside them, right before that step is actually spawned
    /// (`advance_run`), from the very `CodeBlock` about to run. Lets
    /// `render_output` (`ui.rs`) show that step's captured stdout as real,
    /// rendered Markdown once it's done, the same live-view treatment the
    /// web UI's `MeshNode.tsx` (`LiveRunOutput`) already gives it — see
    /// `crate::output::render_output_block_markdown`'s own doc comment for
    /// why this is a stdout-only, `cache`-independent live preview rather
    /// than reusing the on-disk cached-output splice.
    pub output_markdown: bool,
    /// Reset to `Instant::now()` right before each step is actually
    /// spawned (`advance_run`) — read back once its exit code is known
    /// (`on_output_line`) to time it into `ExecOutput::duration_ms`, the
    /// same figure the web UI/CLI already compute around their own spawn
    /// points.
    pub step_started: std::time::Instant,
    pub had_failure: bool,
    pub killed: bool,
    /// Set once the chain has run out (or been killed/cancelled) — `run`
    /// then stays around (rather than getting cleared) purely so its
    /// transcript keeps showing in the output pane until the *next* run
    /// starts. Only `http_rx.is_some()` (never this flag) gates whether the
    /// event loop still polls it for output.
    pub finished: bool,
}

/// Runtime addresses include bound arguments; source fences use the definition name.
fn definition_addr(addr: &BlockAddr) -> BlockAddr {
    let name = meshfox_core::args::Application::parse(&addr.block_name)
        .map(|app| app.definition)
        .unwrap_or_else(|_| addr.block_name.clone());
    BlockAddr::new(&addr.node_id, name)
}

impl RunState {
    /// Whether the event loop (`mod.rs::main_loop`) still has something to
    /// poll for this run.
    pub fn is_running(&self) -> bool {
        self.http_rx.is_some()
    }

    /// The chain address currently executing, if any — `ui::render_tree`'s
    /// own running-spinner badge reads this to know which row to animate.
    /// `chain` is built incrementally, one `RunEvent::StepStart` push at a
    /// time (see `App::on_run_event`'s own doc comment on why — "steps seen
    /// so far, most recent last") — so its own last entry, not `chain[idx]`,
    /// is the one currently running.
    pub fn current_addr(&self) -> Option<&BlockAddr> {
        if self.http_rx.is_some() {
            self.chain.last()
        } else {
            None
        }
    }
}

/// A snapshot of one step's own isolated output, taken the moment it
/// completes (`App::on_output_line`) — `RunState::stdout_only`/
/// `stderr_only` only ever hold the *current* step's own (reset at the start
/// of the next one, see their own doc comments), so without this, a step's
/// output would be visible in the Document pane only while it was the one
/// actually running, gone the instant the chain moved past it. Keyed by
/// address in `App::step_output`; a later run of the same block simply
/// overwrites its entry — only the most recent run's output matters here,
/// same "not persisted, just a live monitor" scope the console itself has.
/// Despite the doc comment above (written back when every constructor sat
/// behind a terminal `RunEvent`), an entry can now also represent a run
/// that's still *in flight* — see `running`.
#[derive(Clone)]
pub struct StepOutput {
    /// stdout and stderr in event arrival order, for plain text rendering.
    pub text: String,
    pub stdout: String,
    pub stderr: String,
    pub output_markdown: bool,
    pub exit_code: i32,
    pub duration_ms: u64,
    /// `true` only while `App::on_external_run_event` is still streaming
    /// this entry in (a run this session discovered passively, not yet at
    /// its own terminal event) — every other constructor sets this `false`,
    /// since a self-triggered run only ever inserts a `StepOutput` once the
    /// step has actually finished (`exit_code`/`duration_ms` are already
    /// real by then). `markdown::push_live_output` reads this before
    /// trusting `exit_code == 0` to mean "done" — an in-flight entry's own
    /// `exit_code` is just a `0` placeholder until the real outcome
    /// arrives, which would otherwise render as a false "done" the moment
    /// the very first line streams in.
    pub running: bool,
}

/// A runnable `file` node's own single execution — no `deps=` chain, no
/// `cache`, no `meshfox:var`, unlike a fenced block's `RunState`, since a
/// `file` node has none of those; see `App::start_file_run`. Kept as its
/// own separate state (rather than shoehorned into `RunState`, which is
/// built entirely around a `BlockAddr` chain) for the same reason the web
/// UI's `run_file_node` is its own endpoint, distinct from `run_block`.
pub struct FileRunState {
    /// Which row's running-spinner/failed-badge (`ui::render_tree`) this
    /// execution belongs to — a `file` node has no `BlockAddr` of its own
    /// to key off of (see this struct's own doc comment), so the node id
    /// is tracked directly instead.
    pub node_id: String,
    /// The worker's event stream for this run (`None` once it ended).
    pub events: Option<tokio::sync::mpsc::UnboundedReceiver<crate::worker_client::RunEvent>>,
    /// The worker's id for the run, once `Started` arrived — what a kill names.
    pub run_id: Option<String>,
    pub lines: Vec<String>,
    pub had_failure: bool,
    /// Set once the process has exited — `run` stays around afterward
    /// purely so its transcript keeps showing until the *next* run
    /// starts, same convention `RunState::finished` uses.
    pub finished: bool,
}

/// A `service` step's lock file (`.meshfox/services/...`) is already held
/// by another live-or-stale process — parked here (mirrors
/// `reset_session_confirm`'s "one thing at a time" precedence, see
/// `on_key`) until the user answers via `on_service_conflict_key`.
/// **Experimental**, see SPEC.md's "Service blocks (experimental)": per
/// the product decision, this always prompts, live owner or stale.
pub struct ServiceConflictState {
    pub block_name: String,
    pub owner_pid: u32,
    pub owner_desc: String,
    /// Always `Some` in practice — every conflict comes from a
    /// worker-routed run's `409` (`App::begin_http_run`/`begin_http_tty_run`)
    /// now; `on_service_conflict_key`'s `y` branch retries via `POST
    /// /api/run/force` (or, for a `tty` conflict, a direct kill-and-retry —
    /// see its own doc comment).
    pub http_retry: Option<HttpRunRetry>,
}

/// A source-editor save the worker refused because the file changed and
/// could not be combined with the buffer: the file as it is now, at `rev`,
/// and the lines that overlapped (none: no merge was possible at all).
struct RefusedSave {
    rev: String,
    text: String,
    conflicts: Vec<crate::worker_client::ConflictLines>,
}

/// What to re-issue, and how, after a worker-routed run's lock conflict is
/// confirmed away — see `ServiceConflictState::http_retry`.
pub struct HttpRunRetry {
    pub node_id: String,
    pub block_name: String,
    pub with_deps: bool,
    pub port: u16,
    pub force_node_id: String,
    pub force_block: String,
    /// See `App::target_chain_has_tty` — which of `begin_http_run`/
    /// `begin_http_tty_run` to retry into.
    pub is_tty: bool,
}

/// The `v` services list view — see `App::services_view`,
/// `App::on_services_view_key`, `ui::render_services_view`.
/// **Experimental**, see SPEC.md's "Service blocks (experimental)".
pub struct ServicesViewState {
    pub selected: usize,
}

pub struct RunConfirmation {
    pub blocks: Vec<String>,
    restart: bool,
    node_id: String,
    block_name: String,
    with_deps: bool,
    port: u16,
    vars: HashMap<String, String>,
    save_secrets: std::collections::HashSet<String>,
    force: Option<(String, String)>,
    tty: bool,
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
    pub(super) search: super::search::Search,
    pub decls: Vec<VarDecl>,
    /// Whether the worker stores `secret` answers in the OS keychain
    /// (`GET /api/info`) — decides if `c` has anything to offer for them.
    pub secret_keychain: bool,
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
    /// Visible rectangles of wrapped tree items, populated by the renderer.
    pub tree_click_rows: Vec<(Rect, usize)>,
    pub focus: Focus,
    pub doc_segments: Vec<Segment>,
    /// Document selection is independent of pane focus; actions target it
    /// only while Document is focused. Cleared when its node/block disappears.
    pub selected_block: Option<BlockAddr>,
    /// Authored `fold` defaults remain separate from manual session choices.
    pub block_views: HashMap<BlockAddr, markdown::BlockView>,
    pub reveal_selected_block: bool,
    /// Every clickable span in `doc_segments`, in the same segment-local
    /// coordinates `markdown::render` returned them in — recomputed
    /// alongside `doc_segments` (`render_current_document`), consulted by
    /// `ui::render_document` to populate `doc_click_targets` (actual
    /// on-screen `Rect`s) every frame, the same two-step split
    /// `ClickRegion`'s own doc comment describes.
    pub doc_click_regions: Vec<ClickRegion>,
    /// `doc_click_regions` translated to actual on-screen `Rect`s by the
    /// most recent `ui::render_document` call — consulted by
    /// `App::on_mouse`'s document-pane branch. Recomputed every frame (like
    /// `ui::compute_layout`'s own pane rects), so a click always hit-tests
    /// against exactly what's currently on screen, scroll/wrap included.
    pub doc_click_targets: Vec<(Rect, ClickTarget)>,
    pub(super) doc_images: HashMap<PathBuf, Option<Arc<DocumentImage>>>,
    /// Prepared images that survive `render_current_document`'s rebuild of
    /// `doc_images`; see `prepare_document_images`.
    doc_image_cache: HashMap<PathBuf, (DocImageKey, Option<Arc<DocumentImage>>)>,
    document_viewport: ratatui::layout::Size,
    /// Bytes of the document's image files, fetched from the worker
    /// (`GET /<relative path>`) — `doc_images` is rebuilt from these on
    /// every render. `image_requested` prevents duplicate fetches per path.
    image_bytes: HashMap<PathBuf, Vec<u8>>,
    pub(super) image_generation: u64,
    image_requested: std::collections::HashSet<PathBuf>,
    /// Requests that have not answered yet, distinct from completed failures.
    pub(super) image_pending: HashSet<PathBuf>,
    pub doc_scroll: u16,
    /// Lines scrolled back from the *bottom* of the Output pane — `0`
    /// (the default) pins it to the live tail, same as before this
    /// existed; scrolling up increases it, revealing earlier lines (see
    /// `scroll_output`). Reset to `0` whenever a fresh run starts, so a
    /// scrolled-up view doesn't silently miss new output.
    pub output_scroll: u16,
    /// Columns scrolled right, for the Output pane's own lines — unlike
    /// `output_scroll` there's no wrapping fallback for a line wider than
    /// the pane (see `ui::render_output`), so this is the only way to see
    /// a wide line's own tail. Reset to `0` whenever a fresh run starts,
    /// same reasoning as `output_scroll`.
    pub output_hscroll: u16,
    /// Which pane (if any) is currently filling the whole screen — set by
    /// pressing `f` (toggles whichever pane is `focus`ed), clicking a
    /// pane's own `[+]`/`[-]` title-row icon, or double-clicking its
    /// title row (`on_key`/`on_mouse`). Renders that one pane alone,
    /// full-terminal (`ui::render`/`ui::compute_layout`), same "one thing
    /// at a time" idea `source_editor` already has over the ordinary
    /// 3-pane layout, just toggle-able (any of the three routes above,
    /// again, on the now full-width pane) rather than a stack.
    pub fullscreen: Option<Focus>,
    /// `(row, col, Instant)` of the most recent left-click `on_mouse` saw
    /// — compared against the next one to detect a double-click. A real
    /// terminal never sends "double-click" as its own event; noticing two
    /// `Down`s in the same place within `DOUBLE_CLICK_WINDOW` is on this
    /// app.
    last_click: Option<(u16, u16, std::time::Instant)>,
    /// The tree pane's own share of the tree/document row, as a percentage
    /// (the document pane gets the rest) — `ui::compute_layout`'s own
    /// `Percentage` split. Adjustable by dragging the border between the
    /// two panes (`resize_drag`/`on_resize_drag`); clamped to
    /// `MIN_TREE_WIDTH_PCT..=MAX_TREE_WIDTH_PCT`.
    pub tree_width_pct: u16,
    /// Width chosen for positioned siblings, independent of the tree width.
    /// Until the first seam drag a map starts wider to fit several columns.
    pub spatial_width_pct: Option<u16>,
    /// Last visible origin for each positioned sibling set. Navigation only
    /// pans when the selected card would otherwise leave that window.
    pub(super) spatial_viewports: HashMap<String, spatial::Viewport>,
    /// Whether a positioned sibling set shows every extra edge or only edges of
    /// the selected node. The latter is the readable default.
    pub show_all_map_edges: bool,
    /// Whether the Tree pane is collapsed to a narrow, borderless handle
    /// (`ui::TREE_COLLAPSED_WIDTH` columns wide) instead of its usual
    /// `tree_width_pct` share of the tree/document row — the Tree pane's
    /// own counterpart to `console_collapsed`, along its width instead of
    /// Output's height. Unlike `console_collapsed`, nothing auto-collapses
    /// or auto-expands this over time; it only ever changes via a
    /// deliberate action (`z`, clicking the pane's own title bar/handle) or
    /// gaining keyboard focus while collapsed (see `on_key`'s `Tab`
    /// handler) — Tree has no "just finished, no longer needs the space"
    /// moment the way a run's own transcript does.
    pub tree_collapsed: bool,
    /// Which pane (if any) is currently expanded purely because it
    /// happened to gain focus (`Tab`/`BackTab`) while collapsed — not
    /// because anyone actually asked to see it (clicking its title bar/
    /// collapsed handle, `z`, `f`, or a chain run actually starting).
    /// Cleared, and that pane collapsed right back, the moment focus
    /// moves anywhere else (see `set_focus`) — the terminal equivalent of
    /// a sidebar that auto-hides again once you look away, rather than
    /// staying pinned open just because focus happened to pass through it
    /// once.
    auto_expanded_pane: Option<Focus>,
    /// The Output pane's own height, in rows — `ui::compute_layout`'s own
    /// `Constraint::Length`. Adjustable the same way as `tree_width_pct`,
    /// via the border between the tree/document row and Output; clamped to
    /// `MIN_OUTPUT_HEIGHT..=` whatever still leaves `ui::MIN_MAIN_HEIGHT`
    /// rows for the tree/document row above it.
    pub output_height: u16,
    /// Which border (if any) a `MouseEventKind::Down(Left)` most recently
    /// landed on and is still being dragged — `None` outside an active
    /// resize gesture. Set by `resize_handle_at`, consumed by
    /// `on_resize_drag`, cleared on the matching `Up(Left)`.
    resize_drag: Option<ResizeDrag>,
    pub highlighter: Highlighter,
    /// `crate::syntax_registry::resolve_editor_theme`'s result, resolved
    /// once at startup from `[tui] editor_theme` in `.meshfox/config.toml`
    /// (local or global) — the bundled `syntect` theme name `highlighter`
    /// above and `ui::render_source_editor`'s own fullscreen editor both
    /// use, so the two panes stay visually consistent. Falls back to
    /// `ui::SOURCE_EDITOR_THEME` when unset or not an actually-bundled
    /// theme name — see `resolve_editor_theme`'s own doc comment.
    pub editor_theme: String,
    pub picker: Picker,
    /// The canvas worker's port. Production startup requires `Some`;
    /// `None` is used only by local TUI unit tests.
    pub worker_port: Option<u16>,
    /// The worker serves this canvas read-only (it or its directory isn't
    /// writable): `e` and `H` — the two ways in that change the file — are
    /// unavailable. Blocks still run. Fixed for the session.
    pub read_only: bool,
    /// Set only while a `VarFormState` opened by `start_run_via_worker` is
    /// waiting on an answer — see `PendingHttpRun`'s own doc comment.
    pub pending_http_run: Option<PendingHttpRun>,
    pub run_confirmation: Option<RunConfirmation>,
    run_confirmation_approved: bool,
    pub run: Option<RunState>,
    pub file_run: Option<FileRunState>,
    /// Live per-step output from the most recent run, kept around after
    /// `RunState`/`FileRunState` moves past a step — see `StepOutput`'s own
    /// doc comment. Consulted by `render_current_document`/`markdown::render`
    /// to splice a block's own live output right under it, the TUI
    /// equivalent of the web UI's `LiveRunOutput`.
    pub step_output: HashMap<BlockAddr, StepOutput>,
    pub child_steps: HashMap<BlockAddr, String>,
    /// Latest invocation displayed beneath each source definition.
    displayed_invocations: HashMap<BlockAddr, BlockAddr>,
    /// Addresses currently running as part of a run this TUI session
    /// didn't itself start — another frontend's manual run, `force_run`,
    /// or a server-triggered `autorun`, discovered passively via
    /// `worker_client::WatchEvent::RunStarted` and streamed via
    /// `worker_client::subscribe_run` (`App::on_external_run_event`). Feeds
    /// `ui::render_tree`'s running-spinner badge the same way `self.run`'s
    /// own current step already does for a self-triggered run; removed
    /// once that run's own `Done` arrives (`step_output` keeps the settled
    /// result, same as any other finished step).
    pub external_running: HashMap<BlockAddr, std::time::Instant>,
    /// Transcript of active runs observed through passive subscriptions.
    pub external_console_lines: Vec<String>,
    /// Whether the Output pane (the "console" — a running transcript of the
    /// most recent chain/file run, not scoped to one block) is collapsed to
    /// its 1-line title strip — see `ui::compute_layout`. Starts collapsed;
    /// `start_run`/`start_file_run` expand it, `mod.rs`'s periodic tick
    /// re-collapses it once nothing is running and `console_last_activity`
    /// is stale (see `App::console_is_active`/`console_tick`).
    pub console_collapsed: bool,
    /// Touched on every streamed output line and on every run/step
    /// finishing (`on_output_line`/`on_file_output_line`/`resume_after_tty`)
    /// — `None` only before the very first run this session. The 10s
    /// auto-collapse window (`console_tick`) is measured from here, not
    /// from when the run *started*, so a long-running chain never gets
    /// collapsed out from under itself just because it's been a while since
    /// the initial expand.
    pub console_last_activity: Option<std::time::Instant>,
    /// Animation phase for `ui::render_tree`'s running-spinner badge —
    /// advanced by exactly one step (`advance_spinner`) each time `mod.rs`'s
    /// own dedicated spinner tick fires, rather than derived from wall-clock
    /// time. Wall-clock-based framing looked right in principle but wasn't:
    /// nothing actually redraws on a fixed schedule fast enough to sample
    /// every frame, so consecutive draws could land many frames apart and
    /// visibly jump/skip. Ticking a plain counter once per redraw instead
    /// means every draw this pane is running through *always* shows the
    /// very next frame, however far apart in real time two draws happen to
    /// land — see `mod.rs`'s own spinner-tick `select!` arm.
    pub spinner_tick: u32,
    pub pending_http_tty: Option<PendingHttpTty>,
    /// See `PendingHttpTtyAttach`'s own doc comment.
    pub pending_http_tty_attach: Option<PendingHttpTtyAttach>,
    /// The `t` live-terminals view's own state — `None` when closed. Its
    /// list is `live_tty_sessions`, refreshed on open and after every
    /// attach/kill (same "refetch, don't try to patch incrementally"
    /// posture `services_view`'s own worker-mode branch already has).
    pub tty_sessions_view: Option<TtySessionsViewState>,
    /// The `H` history view, `None` while closed.
    pub history_view: Option<HistoryViewState>,
    /// The `L` run-history view, when open.
    pub run_history_view: Option<RunHistoryViewState>,
    /// Last-fetched `GET /api/runs`, filtered to `kind == "tty" && status
    /// == "running"` — see `open_tty_sessions_view`. Empty (not `None`)
    /// when the view is closed; only meaningful while `tty_sessions_view`
    /// is `Some`.
    pub live_tty_sessions: Vec<crate::worker_client::ActiveRunDto>,
    /// Last-fetched `GET /api/runs` in full: for each plain or `tty` block,
    /// the latest run that still describes the document (running, or
    /// finished with its exit code) — the source of the tree's failed
    /// badge, so it reflects each block's *latest* run rather than any
    /// failure this session happened to see. `None` until the first
    /// successful fetch (or with no worker, where nothing can run anyway).
    pub block_runs: Option<Vec<crate::worker_client::ActiveRunDto>>,
    /// Set when a run just ended, so `mod.rs` refetches `block_runs` right
    /// away instead of waiting for the periodic tick.
    pub refresh_runs_soon: bool,
    /// A background `request_worker_refresh` that hasn't answered yet.
    worker_refresh_inflight: bool,
    pub pending_child_canvas: Option<PendingChildCanvas>,
    pub pending_external_editor: Option<PendingExternalEditor>,
    /// `[tui] editor` in `.meshfox/config.toml`: when set, `e` edits the
    /// source in that program instead of the built-in editor.
    pub external_editor: Option<String>,
    pub block_picker: Option<BlockPickerState>,
    pub var_form: Option<VarFormState>,
    /// The `dir`/`file` path picker (`Ctrl-O` on such a field of `var_form`
    /// or `active_inline_form`), drawn above whichever of them opened it.
    pub path_picker: Option<super::path_picker::PathPickerState>,
    pub status: String,
    pub show_help: bool,
    /// How far the `?` help modal's own content is scrolled down — counts
    /// wrapped display rows (post word-wrap), same convention
    /// `render_help`'s own `Paragraph::scroll` uses; reset to `0` every
    /// time help is (re-)opened. Unclamped here, clamped at render time
    /// against the modal's actual wrapped row count, same "state is
    /// unclamped, rendering clamps" convention `doc_scroll`/`output_scroll`
    /// already use.
    pub help_scroll: u16,
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
    /// An external change to `canvas_path` that arrived while
    /// `source_editor` was open — applying it immediately could yank the
    /// document out from under an in-progress edit (or, worse, get
    /// silently clobbered the moment the editor saves). Parked here
    /// instead and applied once the editor closes without saving; a save
    /// makes it stale (the editor's own write is now what's on disk), so
    /// it's just dropped in that case. Mirrors the web UI's
    /// `pendingExternalChange` (`web/src/App.tsx`).
    pub pending_external_change: Option<(String, Option<Canvas>)>,
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
    /// `on_background_msg`. Cloned into every spawned fetch task.
    pub background_tx: tokio::sync::mpsc::UnboundedSender<BackgroundMsg>,
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
    /// Shared sliced protocols avoid re-encoding preview images on redraw.
    pub(super) link_preview_image: HashMap<String, Arc<DocumentImage>>,
    /// Whether the `S` (reset session) confirm prompt is up — see `on_key`'s
    /// early dispatch to `on_reset_session_confirm_key` and
    /// `reset_session`'s own doc comment for why this asks at all despite
    /// being purely in-memory. Same "one thing at a time" precedence as
    /// `var_form`/`block_picker`/`source_editor`.
    pub reset_session_confirm: bool,
    /// `(running, crashed)` across `service_list` — recomputed by
    /// `refresh_services`, not on every render (unlike `constraint_stats`,
    /// which only changes on a document reload) since a service's own
    /// status changes independently of the document, on its own schedule.
    /// `None` when `service_list` is empty, so the footer renders nothing
    /// rather than a vacuous "0/0".
    pub service_stats: Option<(usize, usize)>,
    /// A service lock conflict awaiting the user's confirm/cancel — see
    /// `ServiceConflictState`'s own doc comment.
    pub service_conflict: Option<ServiceConflictState>,
    /// The `v` services list view (every service at once, not scoped to
    /// the currently selected node) — `Some` while open. See
    /// `ServicesViewState`'s own doc comment.
    pub services_view: Option<ServicesViewState>,
    /// `display="code"` nodes' content, fetched from the worker by node id —
    /// see `ensure_file_preview`.
    pub file_previews: HashMap<String, FilePreview>,
    /// `display="table"` nodes' schema, view and fetched rows, by node id —
    /// see `tui/table.rs`.
    pub tables: HashMap<String, TableData>,
    /// The full-screen table (`Enter` on a table node) — `Some` takes over
    /// the whole screen and the keymap, like `source_editor`.
    pub table_mode: Option<TableMode>,
    /// Every `GET /api/services` entry as of the last poll
    /// (`refresh_services`, on a periodic tick from `mod.rs` whenever
    /// `worker_port` is `Some`, the same ~3s cadence the web UI's own
    /// service panel already polls at). `sorted_service_keys`/
    /// `on_services_view_key`/`ui::render_services_view` all read from this.
    pub service_list: Vec<crate::worker_client::ServiceDto>,
    /// The currently-selected service's own retained log, as of the last
    /// `refresh_service_log` poll (`GET /api/services/log`, only worth
    /// fetching while the services view is actually open and pointed at
    /// this one entry).
    pub service_log: Vec<(meshfox_server::stream_exec::OutputStream, String)>,
    /// Values a `form` fence's own Send has committed this TUI process's
    /// lifetime (see `submit_inline_form`) — every variable a form field
    /// targets is implicitly `session`-scoped by its own `meshfox:var`
    /// declaration (node-scoped, per `meshfox_core::declared_vars`), so
    /// there's nothing for the on-disk `var_cache` to mean here. A form's
    /// submitted value has to outlive the one run it happened to trigger,
    /// for every later run (manual or `autorun`-triggered) within the same
    /// process to keep seeing it. Cleared by `reset_session`.
    pub session_vars: HashMap<String, String>,
    /// A `form`-lang fence currently open in the Document pane — `Some`
    /// the moment a field/Send is clicked or entered via `i`, whether or
    /// not it's actively claiming the keymap right now (see `editing`).
    /// Only one at a time, same "whichever was opened most recently wins"
    /// rule `var_form`/`block_picker` already have, but — unlike those —
    /// this does *not* sit in `on_key`'s "one thing at a time" precedence
    /// chain by itself: the rest of the document stays visible/scrollable
    /// around it. Only `editing` does.
    pub active_inline_form: Option<InlineFormState>,
    /// Addresses `meshfox_core::autorun_blocks_for_changed_vars` found for
    /// the most recent `submit_inline_form`, still waiting their turn —
    /// there is exactly one foreground run slot (`self.run`), so N
    /// triggered blocks can't just be N back-to-back `start_run` calls the
    /// way the web server's own (independently `tokio::spawn`ed) trigger
    /// can. Drained one at a time by `advance_run`, right where it would
    /// otherwise just mark the current chain finished and return — see
    /// that function's own doc comment. A chain that ends in failure still
    /// lets the next queued address start; one broken autorun shouldn't
    /// block an unrelated one.
    pub pending_autoruns: std::collections::VecDeque<BlockAddr>,
}

/// One field of a `form`-lang fence currently open in the Document pane,
/// paired with the `meshfox:var` it targets — parallel arrays with
/// `InlineFormState::inputs`, same shape `VarFormState` already uses for
/// the (unrelated) global "configure variables" modal.
pub struct InlineFormState {
    pub node_id: String,
    pub block_name: String,
    /// `field var=`/`label=` entries, in document order — resolved once
    /// when this form is first opened (click or `i`); re-opening after an
    /// edit re-derives this fresh, so a hand-edited `field` line is always
    /// reflected the next time the form is entered.
    pub fields: Vec<meshfox_core::FormField>,
    /// Parallel to `fields` — the declared `meshfox:var` each one targets,
    /// for type-aware editing/`validate_value` exactly the way `var_form`
    /// already does. A `field var=` naming something `meshfox validate`
    /// would have already rejected (out of scope, `from=`-computed, or
    /// just undeclared) is silently dropped from both `fields` and this —
    /// see `App::try_build_inline_form`.
    pub decls: Vec<VarDecl>,
    /// Parallel to `fields`/`decls` — one live editable buffer per field,
    /// pre-filled from `App::session_vars` (if already submitted this
    /// session) or else the same cache/shared/default fallback
    /// `initial_field_input` already gives `var_form`.
    pub inputs: Vec<String>,
    /// Which field is focused — `fields.len()` (one past the last field)
    /// means the virtual "Send" row is focused instead of any real field.
    pub selected: usize,
    /// Whether this form is actively claiming the keymap right now (see
    /// `on_key`'s precedence chain) — `false` the moment it's merely open
    /// (so `markdown::render` still draws its live `inputs`) but not
    /// focused; `true` from a field/Send click, or `i`, until `Esc` hands
    /// arrow keys back to ordinary document scrolling. Unlike
    /// `cancel_var_form`, there's no "cancel back to" — `Esc` only ever
    /// flips this to `false`, never clears `active_inline_form` itself or
    /// discards `inputs`.
    pub editing: bool,
}

/// A background fetch's result, reported back through
/// `App::background_tx` into `mod.rs`'s `main_loop` (same shape as the
/// existing `reload_rx`/output-line channels). A link-preview fetch never
/// sends anything on failure (see `App::link_preview_requested`); the
/// worker-backed ones below always report, an error included, so the
/// document can say why instead of staying on "loading…".
pub enum BackgroundMsg {
    Meta {
        url: String,
        meta: PreviewMeta,
    },
    Image {
        url: String,
        image: image::DynamicImage,
    },
    /// A `form` fence's fields with their resolved values, from the worker.
    FormFields {
        node_id: String,
        block: String,
        result: Result<Vec<crate::worker_client::FormFieldStatus>, String>,
    },
    /// A document image file's bytes, from the worker.
    ImageBytes {
        generation: u64,
        path: PathBuf,
        result: Result<Vec<u8>, String>,
    },
    /// A `display="code"` node's file content, from the worker.
    FileContent {
        node_id: String,
        result: Result<(String, bool), String>,
    },
    /// A `display="table"` node's schema/import state, from the worker.
    TableMeta {
        node_id: String,
        result: Result<crate::worker_client::TableMetaDto, String>,
    },
    /// The worker's service list and run list, from `request_worker_refresh`;
    /// `None` for a request that failed (the last list is kept).
    WorkerState {
        services: Option<Vec<crate::worker_client::ServiceDto>>,
        runs: Option<Vec<crate::worker_client::ActiveRunDto>>,
    },
    /// One block of a `display="table"` node's rows. `generation` is the
    /// `TableData` generation the request was made under — an answer for an
    /// older view is dropped.
    TableRows {
        node_id: String,
        generation: u64,
        block: usize,
        result: Result<crate::worker_client::TableRowsDto, String>,
    },
}

/// A `display="code"` node's fetched file content (see
/// `App::ensure_file_preview`).
pub enum FilePreviewState {
    Loading,
    Ready { content: String, truncated: bool },
    Failed(String),
}

pub struct FilePreview {
    pub state: FilePreviewState,
    inflight: bool,
    fetched: Option<std::time::Instant>,
}

/// How old a fetched preview may get before the next render refreshes it in
/// the background (the old content stays on screen meanwhile).
const FILE_PREVIEW_REFRESH: std::time::Duration = std::time::Duration::from_secs(2);
/// A table's `GET /table` is re-asked this often once it's ready, to notice
/// the file changing underneath.
const TABLE_META_REFRESH: std::time::Duration = std::time::Duration::from_secs(3);

/// A var form field's starting value, coerced to something its own
/// control can actually represent — a `bool` field is a toggle, so its
/// value is always canonically `"true"` or `"false"` (anything else,
/// including no suggestion at all, starts as `"false"`); a `select` field
/// is a chooser over `decl.choices`, so a suggestion that isn't actually
/// one of them falls back to the first choice rather than displaying
/// something the left/right cycle could never have produced itself.
/// `String`/`Int`/`Dir`/`File` fields are free text, so the suggestion (or an
/// empty string) passes through unchanged. The suggestion starts as the
/// declaration's `default`; the worker's resolved value (environment,
/// cache, shared env) replaces it once `GET /api/form/fields` answers.
fn coerce_field_input(decl: &VarDecl, suggestion: Option<&str>) -> String {
    match decl.var_type {
        VarType::Bool => if suggestion == Some("true") {
            "true"
        } else {
            "false"
        }
        .to_string(),
        VarType::Select => match suggestion {
            Some(v) if decl.choices.iter().any(|c| c == v) => v.to_string(),
            _ => decl.choices.first().cloned().unwrap_or_default(),
        },
        VarType::String | VarType::Int | VarType::Dir | VarType::File => {
            suggestion.map(str::to_string).unwrap_or_default()
        }
    }
}

/// A worker-reported `VarStatus` reshaped into the `VarDecl` shape
/// `VarFormState`/`render_var_form`/`validate_value` already work with —
/// see `var_form_from_statuses`. Fields the wire status doesn't carry
/// (`required`/`from`/`session`/`default_var`/`choices_var`) get harmless
/// defaults: the worker has already decided this needs asking (that's why
/// it came back `resolved: false`), and none of those fields change how a
/// single already-missing value is prompted for or validated.
fn var_decl_from_status(status: &crate::worker_client::VarStatus) -> VarDecl {
    let var_type = match status.var_type.as_str() {
        "int" => VarType::Int,
        "bool" => VarType::Bool,
        "select" => VarType::Select,
        "dir" => VarType::Dir,
        "file" => VarType::File,
        _ => VarType::String,
    };
    VarDecl {
        name: status.name.clone(),
        var_type,
        prompt: status.prompt.clone(),
        default: status.value.clone(),
        choices: status.choices.clone(),
        secret: status.secret,
        required: false,
        from: None,
        session: false,
        default_var: None,
        choices_var: None,
        file_mode: (var_type == VarType::File).then(|| match status.mode.as_deref() {
            Some("save") => meshfox_core::FileMode::Save,
            _ => meshfox_core::FileMode::Open,
        }),
        ext: status.ext.clone(),
    }
}

/// Builds the `VarFormState` `start_run_via_worker` opens when `GET
/// /api/vars` finds something unresolved — the worker-routed counterpart to
/// `park_on_unresolved`'s local-mode form, pre-filling each field from the
/// status's own `value` (the worker has already computed the same
/// default/cache/env suggestion `initial_field_input` would have) and
/// `inherited_from` rather than re-deriving them from a local `VarCache`.
fn var_form_from_statuses(missing: Vec<crate::worker_client::VarStatus>) -> VarFormState {
    let mut decls = Vec::with_capacity(missing.len());
    let mut inputs = Vec::with_capacity(missing.len());
    let mut origins = Vec::with_capacity(missing.len());
    let mut errors = Vec::with_capacity(missing.len());
    let stored = missing.iter().map(|s| s.stored).collect();
    let mut secret_store = "plaintext".to_string();
    for status in &missing {
        errors.push(status.secret_error.clone());
        if let Some(store) = &status.secret_store {
            secret_store = store.clone();
        }
        inputs.push(status.value.clone().unwrap_or_default());
        origins.push(status.inherited_from.clone().map(|o| match o {
            crate::worker_client::VarOrigin::Project => meshfox_core::SharedOrigin::Project,
            crate::worker_client::VarOrigin::Global { path } => {
                meshfox_core::SharedOrigin::Global { path }
            }
        }));
        decls.push(var_decl_from_status(status));
    }
    VarFormState {
        arguments: false,
        save: vec![false; decls.len()],
        decls,
        inputs,
        origins,
        errors,
        stored,
        clear: vec![false; missing.len()],
        secret_store,
        selected: 0,
        configuring: false,
    }
}

fn argument_form_from_statuses(mut fields: Vec<crate::worker_client::VarStatus>) -> VarFormState {
    for field in &mut fields {
        if field.value.is_none() {
            field.value = match field.var_type.as_str() {
                "bool" => Some("false".into()),
                "select" => field.choices.first().cloned(),
                _ => None,
            };
        }
    }
    let mut form = var_form_from_statuses(fields);
    form.arguments = true;
    form
}

#[cfg(test)]
pub(super) fn var_form_from_statuses_for_test(
    statuses: Vec<crate::worker_client::VarStatus>,
) -> VarFormState {
    var_form_from_statuses(statuses)
}

/// What `e`/`H` say on a canvas served read-only — see `App::read_only`.
const READ_ONLY_STATUS: &str =
    "read-only canvas (it or its directory isn't writable) — blocks run, nothing is saved";

impl App {
    /// Loads the primary canvas from its worker. A failed request is a
    /// startup error; unit tests may pass `None` to exercise local UI code.
    pub async fn new(
        canvas_path: PathBuf,
        background_tx: tokio::sync::mpsc::UnboundedSender<BackgroundMsg>,
        initial_node: Option<&str>,
        worker_port: Option<u16>,
    ) -> io::Result<App> {
        let configured = if std::env::var_os("MESHFOX_IMAGE_PROTOCOL").is_none() {
            match worker_port {
                Some(port) => crate::worker_client::tui_image_protocol(port)
                    .await
                    .map_err(io::Error::other)?,
                None => None,
            }
        } else {
            None
        };
        Self::new_with_picker(
            canvas_path,
            background_tx,
            initial_node,
            worker_port,
            build_picker(configured.as_deref())?,
        )
        .await
    }

    /// Nested canvases share the terminal and its already detected capabilities.
    /// Querying stdin again would race the running crossterm input reader.
    pub(super) async fn new_with_picker(
        canvas_path: PathBuf,
        background_tx: tokio::sync::mpsc::UnboundedSender<BackgroundMsg>,
        initial_node: Option<&str>,
        worker_port: Option<u16>,
        picker: Picker,
    ) -> io::Result<App> {
        let raw = match worker_port {
            Some(port) => match crate::worker_client::get_canvas_raw(port).await {
                Ok(raw) => raw,
                Err(e) => {
                    return Err(io::Error::other(format!(
                        "failed to load canvas from worker on port {port}: {e}"
                    )));
                }
            },
            #[cfg(test)]
            None => std::fs::read_to_string(&canvas_path)?,
            #[cfg(not(test))]
            None => return Err(io::Error::other("no worker for canvas")),
        };
        let canvas = Canvas::from_markdown(&raw).map_err(|e| io::Error::other(e.to_string()))?;
        let decls = declared_vars(&canvas).unwrap_or_default();
        let display_canvas = match worker_port {
            Some(port) => crate::worker_client::get_canvas(port)
                .await
                .unwrap_or_else(|_| canvas.clone()),
            None => local_display_canvas(&canvas, &canvas_path),
        };
        // Deep-link start (`meshfox tui <path> --node <id>` — see
        // `Command::Tui`): expand every ancestor of `initial_node` so its
        // row actually exists in `flatten`'s output below, same as the web
        // UI's own `deepLinkNodeId` handling has to unfold ancestors before
        // `flowInstance.getNode` can find anything.
        let mut expanded = HashSet::new();
        if let Some(target) = initial_node {
            let mut current = display_canvas.node(target).and_then(|n| n.parent.clone());
            while let Some(id) = current {
                current = display_canvas.node(&id).and_then(|n| n.parent.clone());
                expanded.insert(id);
            }
        }
        let constraint_stats = constraint_stats(&display_canvas);
        let rows = tree::flatten(&display_canvas, &expanded);
        // Computed before `canvas_path` is moved into the struct literal
        // below (its `canvas_path,` shorthand field).
        let syntax_root = crate::canvas_root_dir(&canvas_path).to_path_buf();
        let editor_theme = crate::syntax_registry::resolve_editor_theme(&syntax_root);
        let external_editor = super::external_editor::configured(&syntax_root);

        let info = match worker_port {
            Some(port) => crate::worker_client::info(port).await,
            None => Default::default(),
        };
        let read_only = info.read_only;
        let mut app = App {
            canvas_path,
            raw,
            canvas,
            display_canvas,
            search: super::search::Search::default(),
            decls,
            secret_keychain: info.secret_keychain,
            expanded,
            rows,
            selected: 0,
            list_state: ListState::default(),
            tree_click_rows: Vec::new(),
            focus: Focus::Tree,
            doc_segments: Vec::new(),
            selected_block: None,
            block_views: HashMap::new(),
            reveal_selected_block: false,
            doc_click_regions: Vec::new(),
            doc_click_targets: Vec::new(),
            doc_images: HashMap::new(),
            doc_image_cache: HashMap::new(),
            document_viewport: ratatui::layout::Size::new(56, 24),
            image_bytes: HashMap::new(),
            image_generation: 0,
            image_requested: std::collections::HashSet::new(),
            image_pending: HashSet::new(),
            doc_scroll: 0,
            output_scroll: 0,
            output_hscroll: 0,
            fullscreen: None,
            last_click: None,
            tree_width_pct: ui::DEFAULT_TREE_WIDTH_PCT,
            spatial_width_pct: None,
            spatial_viewports: HashMap::new(),
            show_all_map_edges: false,
            tree_collapsed: false,
            auto_expanded_pane: None,
            output_height: ui::DEFAULT_OUTPUT_HEIGHT,
            resize_drag: None,
            highlighter: Highlighter::with_extra_syntaxes(&syntax_root, &editor_theme),
            editor_theme,
            picker,
            worker_port,
            read_only,
            pending_http_run: None,
            run_confirmation: None,
            run_confirmation_approved: false,
            run: None,
            file_run: None,
            step_output: HashMap::new(),
            child_steps: HashMap::new(),
            displayed_invocations: HashMap::new(),
            external_running: HashMap::new(),
            external_console_lines: Vec::new(),
            console_collapsed: true,
            console_last_activity: None,
            spinner_tick: 0,
            pending_http_tty: None,
            pending_http_tty_attach: None,
            tty_sessions_view: None,
            history_view: None,
            run_history_view: None,
            live_tty_sessions: Vec::new(),
            block_runs: None,
            refresh_runs_soon: true,
            worker_refresh_inflight: false,
            pending_child_canvas: None,
            pending_external_editor: None,
            external_editor,
            block_picker: None,
            var_form: None,
            path_picker: None,
            status: if read_only {
                READ_ONLY_STATUS.into()
            } else {
                String::new()
            },
            show_help: false,
            help_scroll: 0,
            should_quit: false,
            source_editor: None,
            constraint_stats,
            pending_external_change: None,
            link_preview_cache: Arc::new(link_preview::PreviewCache::new()),
            background_tx,
            link_preview_requested: HashSet::new(),
            link_preview_meta: HashMap::new(),
            link_preview_image_requested: HashSet::new(),
            link_preview_image: HashMap::new(),
            reset_session_confirm: false,
            service_stats: None,
            service_conflict: None,
            services_view: None,
            file_previews: HashMap::new(),
            tables: HashMap::new(),
            table_mode: None,
            service_list: Vec::new(),
            service_log: Vec::new(),
            session_vars: HashMap::new(),
            active_inline_form: None,
            pending_autoruns: std::collections::VecDeque::new(),
        };
        if let Some(target) = initial_node {
            if let Some(idx) = app
                .rows
                .iter()
                .position(|r| r.reference.is_none() && r.node_id == target)
            {
                app.selected = idx;
            }
        }
        app.render_current_document();
        Ok(app)
    }

    pub async fn on_key(&mut self, key: KeyEvent) {
        if let Some(se) = &mut self.source_editor {
            match se.on_key(key) {
                SourceEditorOutcome::Stay => {}
                SourceEditorOutcome::Close => self.close_source_editor(),
                SourceEditorOutcome::Save => self.save_source_editor().await,
                SourceEditorOutcome::External => self.request_external_edit(false),
                SourceEditorOutcome::Switch { path, is_canvas } => {
                    self.switch_source_editor_file(path, is_canvas).await
                }
            }
            return;
        }
        if self.table_mode.is_some() {
            self.on_table_key(key);
            return;
        }
        if self.path_picker.is_some() {
            self.on_path_picker_key(key).await;
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
        if let Some(pending) = self.run_confirmation.take() {
            match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') => {
                    if pending.restart {
                        match crate::worker_client::restart_service(
                            pending.port,
                            &pending.node_id,
                            &pending.block_name,
                            true,
                        )
                        .await
                        {
                            Ok(pid) => {
                                self.status =
                                    format!("restarted {} (pid {pid})", pending.block_name)
                            }
                            Err(error) => self.status = format!("failed to restart: {error}"),
                        }
                        self.refresh_services().await;
                        return;
                    }
                    self.run_confirmation_approved = true;
                    if pending.tty {
                        Box::pin(self.begin_http_tty_run(
                            pending.node_id,
                            pending.block_name,
                            pending.with_deps,
                            pending.port,
                            pending.vars,
                            pending.save_secrets,
                            pending.force,
                        ))
                        .await;
                    } else {
                        Box::pin(self.begin_http_run(
                            pending.node_id,
                            pending.block_name,
                            pending.with_deps,
                            pending.port,
                            pending.vars,
                            pending.save_secrets,
                            pending.force,
                        ))
                        .await;
                    }
                }
                KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc | KeyCode::Enter => {
                    self.status = "run cancelled".into();
                }
                _ => self.run_confirmation = Some(pending),
            }
            return;
        }
        if self.services_view.is_some() {
            self.on_services_view_key(key).await;
            return;
        }
        if self.tty_sessions_view.is_some() {
            self.on_tty_sessions_view_key(key).await;
            return;
        }
        if self.history_view.is_some() {
            self.on_history_view_key(key).await;
            return;
        }
        if self.run_history_view.is_some() {
            self.on_run_history_key(key).await;
            return;
        }
        if self.reset_session_confirm {
            self.on_reset_session_confirm_key(key).await;
            return;
        }
        if self.service_conflict.is_some() {
            self.on_service_conflict_key(key).await;
            return;
        }
        // Same "claims the whole keymap" precedence as every modal above —
        // without this, `j`/`k`/PageUp/PageDown (meant to scroll *this*
        // popup) fell through to the main match below and scrolled/moved
        // selection in whichever pane was focused underneath instead, and
        // a letter this popup doesn't otherwise recognize (`r`, say) could
        // still trigger its ordinary action right through the open help.
        if self.show_help {
            self.on_help_key(key);
            return;
        }
        // Unlike the modals above, `active_inline_form` only claims the
        // keymap while it's actually `editing` — merely being open (so
        // `markdown::render` draws its live `inputs`) leaves ordinary
        // document scrolling untouched, since the form reads as part of
        // the document, not a screen-covering dialog. See
        // `InlineFormState::editing`'s own doc comment.
        if self.active_inline_form.as_ref().is_some_and(|f| f.editing) {
            self.on_inline_form_key(key).await;
            return;
        }
        if self.search.open {
            self.on_search_key(key);
            return;
        }
        if key.code == KeyCode::Char('/') {
            self.search.open = true;
            self.search.cursor = self.search.query.len();
            self.search.expanded_before = self.expanded.clone();
            self.search.revealed.clear();
            self.reveal_search_match();
            self.render_current_document();
            return;
        }
        if key.code == KeyCode::Char('i') && self.focus == Focus::Document {
            self.try_enter_inline_form();
            return;
        }

        if self.focus == Focus::Document {
            match key.code {
                KeyCode::Char('[') => {
                    self.select_adjacent_block(-1);
                    return;
                }
                KeyCode::Char(']') => {
                    self.select_adjacent_block(1);
                    return;
                }
                KeyCode::Esc if self.selected_block.is_some() => {
                    self.selected_block = None;
                    self.render_current_document();
                    return;
                }
                KeyCode::Enter | KeyCode::Char('c') if self.selected_block.is_some() => {
                    let addr = self.selected_block.clone().unwrap();
                    if self.is_button_block(&addr) {
                        if key.code == KeyCode::Enter {
                            self.activate_click_target(ClickTarget::RunBlock {
                                node_id: addr.node_id,
                                block_name: addr.block_name,
                            })
                            .await;
                        }
                        return;
                    }
                    // A selected `form` fence has no source or output to
                    // fold: `Enter` starts editing it, `c` does nothing.
                    if self.is_form_block(&addr) {
                        if key.code == KeyCode::Enter {
                            self.ensure_inline_form_open(&addr.node_id, &addr.block_name);
                            if let Some(form) = &mut self.active_inline_form {
                                form.editing = true;
                            }
                            self.render_current_document();
                        }
                        return;
                    }
                    let action = if key.code == KeyCode::Enter {
                        markdown::BlockAction::ToggleBlock
                    } else {
                        markdown::BlockAction::ToggleSource
                    };
                    self.activate_block_action(addr, action).await;
                    return;
                }
                _ => {}
            }
        }

        match key.code {
            KeyCode::Char('q') => self.quit(),
            KeyCode::Esc => {
                if self.fullscreen.is_some() {
                    self.fullscreen = None;
                } else if self.focus == Focus::Tree {
                    self.exit_spatial_parent();
                }
            }
            // `show_help` is never true here — while it is, the early
            // return above (`on_help_key`) already claimed this keypress.
            // Reset to the top every time it's (re-)opened, so scrolling
            // down, closing, and reopening doesn't land mid-scroll.
            KeyCode::Char('?') => {
                self.show_help = true;
                self.help_scroll = 0;
            }
            KeyCode::Tab => {
                let new_focus = match self.focus {
                    Focus::Tree => Focus::Document,
                    Focus::Document => Focus::Output,
                    Focus::Output => Focus::Tree,
                };
                self.set_focus(new_focus);
                // Gaining focus always means "I want to actually see this
                // pane" — a collapsed Tree/Output would otherwise leave
                // Tab cycling through a pane that's just a sliver on
                // screen, with no visible sign anything changed. Only a
                // *peek* though (see `peek_pane_on_focus`'s own doc
                // comment) — `set_focus` above already collapsed whichever
                // pane focus just left, if it was only open for the same
                // reason.
                self.peek_pane_on_focus(new_focus);
            }
            // Shift-Tab — the same cycle, backwards.
            KeyCode::BackTab => {
                let new_focus = match self.focus {
                    Focus::Tree => Focus::Output,
                    Focus::Output => Focus::Document,
                    Focus::Document => Focus::Tree,
                };
                self.set_focus(new_focus);
                self.peek_pane_on_focus(new_focus);
            }
            KeyCode::Up | KeyCode::Char('k') => match self.focus {
                Focus::Tree => self.move_selection(-1),
                Focus::Document => self.scroll_document(-1),
                Focus::Output => self.scroll_output(-1),
            },
            KeyCode::Down | KeyCode::Char('j') => match self.focus {
                Focus::Tree => self.move_selection(1),
                Focus::Document => self.scroll_document(1),
                Focus::Output => self.scroll_output(1),
            },
            KeyCode::Enter if self.selected_opens_table() => self.open_table_mode(),
            KeyCode::Enter if self.focus == Focus::Tree => self.toggle_expand(),
            KeyCode::Backspace if self.focus == Focus::Tree => {
                if !self.exit_spatial_parent() {
                    self.collapse_or_to_parent();
                }
            }
            KeyCode::Left | KeyCode::Char('h') if self.focus == Focus::Tree => {
                if !self.move_spatial_selection(spatial::Direction::Left) {
                    self.collapse_or_to_parent();
                }
            }
            KeyCode::Right | KeyCode::Char('l') if self.focus == Focus::Tree => {
                if !self.move_spatial_selection(spatial::Direction::Right) {
                    self.expand_selected();
                }
            }
            KeyCode::Char('a')
                if self.focus == Focus::Tree && spatial::active_spatial_parent(self).is_some() =>
            {
                self.show_all_map_edges = !self.show_all_map_edges;
            }
            KeyCode::Char('r') => self.trigger_run(true).await,
            KeyCode::Char('R') => self.trigger_run(false).await,
            KeyCode::Char('K') => self.kill_running().await,
            KeyCode::Char('S') => self.reset_session_confirm = true,
            // Opens the services list view — **experimental**, see
            // `open_services_view`'s own doc comment and SPEC.md's
            // "Service blocks (experimental)".
            KeyCode::Char('v') => self.open_services_view().await,
            // Opens the live-terminals view — every `tty` session the
            // shared worker currently knows about, whether started by
            // this TUI, another one, or a browser tab. See
            // `open_tty_sessions_view`'s own doc comment.
            KeyCode::Char('t') => self.open_tty_sessions_view().await,
            KeyCode::Char('H') => self.open_history_view().await,
            KeyCode::Char('L') => self.open_run_history().await,
            KeyCode::Char('o') => self.trigger_open_file().await,
            KeyCode::Char('c') => self.trigger_configure().await,
            KeyCode::Char('e') => self.open_source_editor().await,
            // The keyboard counterpart to clicking a pane's own `[+]`/`[-]`
            // title-row icon or double-clicking its title (`on_mouse`) —
            // those are mouse-only and, on a TUI, have no visual
            // affordance suggesting they're even possible (unlike a GUI
            // title bar's own double-click-to-maximize convention), so
            // this is the actually-discoverable way in (see the footer
            // hint/`?` help). Toggles fullscreen for whichever pane is
            // currently focused — leaves focus untouched either way, so
            // toggling back out lands right where toggling in did.
            KeyCode::Char('f') => {
                if self.fullscreen == Some(self.focus) {
                    self.fullscreen = None;
                } else {
                    self.fullscreen = Some(self.focus);
                    // A collapsed pane's own render function (`ui::
                    // render_output`/`render_tree`) checks its own
                    // collapsed flag *before* looking at `fullscreen` — so
                    // fullscreening a currently-collapsed pane would hide
                    // the other two but still only show that pane's own
                    // collapsed handle/strip, with the rest of the screen
                    // simply blank. Expanding here is what actually makes
                    // `f` work as "expand" from the collapsed state. This
                    // is an explicit action, not a focus-driven peek, so
                    // the pane stays expanded once `f` is pressed again —
                    // no `auto_expanded_pane` bookkeeping here.
                    self.expand_pane(self.focus);
                }
            }
            // The keyboard counterpart to clicking a pane's own title bar
            // (Tree/Output; a no-op on Document, which has no collapsed
            // state) or its collapsed handle — same "mouse-only has no
            // visible affordance on a TUI" reasoning `f` already has for
            // fullscreen (see its own doc comment above, and the footer
            // hint/`?` help).
            KeyCode::Char('z') => self.toggle_collapse_focused(),
            KeyCode::PageDown => match self.focus {
                Focus::Tree => self.page_selection(1),
                Focus::Output => self.scroll_output(10),
                Focus::Document => self.scroll_document(10),
            },
            KeyCode::PageUp => match self.focus {
                Focus::Tree => self.page_selection(-1),
                Focus::Output => self.scroll_output(-10),
                Focus::Document => self.scroll_document(-10),
            },
            KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => match self.focus
            {
                Focus::Output => self.scroll_output(10),
                _ => self.scroll_document(10),
            },
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => match self.focus
            {
                Focus::Output => self.scroll_output(-10),
                _ => self.scroll_document(-10),
            },
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
            // Ctrl-O: browse for the focused `dir`/`file` field.
            KeyCode::Char('o') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                let target = self.var_form.as_ref().and_then(|vf| {
                    matches!(vf.decls[vf.selected].var_type, VarType::Dir | VarType::File)
                        .then_some(super::path_picker::PickerTarget::VarForm(vf.selected))
                });
                if let Some(target) = target {
                    self.open_path_picker(target).await;
                }
            }
            // Ctrl-S: toggle "save" on the focused `secret` field.
            KeyCode::Char('s') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if let Some(vf) = &mut self.var_form {
                    let i = vf.selected;
                    if vf.decls[i].secret {
                        vf.save[i] = !vf.save[i];
                    }
                }
            }
            // Ctrl-D (configure only): delete the stored value of the
            // focused `secret` field on submit.
            KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if let Some(vf) = &mut self.var_form {
                    let i = vf.selected;
                    if vf.configuring && vf.decls[i].secret && vf.stored[i] == Some(true) {
                        vf.clear[i] = !vf.clear[i];
                    }
                }
            }
            KeyCode::Backspace => {
                if let Some(vf) = &mut self.var_form {
                    let i = vf.selected;
                    if matches!(
                        vf.decls[i].var_type,
                        VarType::String | VarType::Int | VarType::Dir | VarType::File
                    ) {
                        vf.inputs[i].pop();
                    }
                }
            }
            KeyCode::Char(c) => {
                if let Some(vf) = &mut self.var_form {
                    let i = vf.selected;
                    let allowed = match vf.decls[i].var_type {
                        VarType::String | VarType::Dir | VarType::File => true,
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
            VarType::String | VarType::Int | VarType::Dir | VarType::File => {}
        }
    }

    /// Resolves `node_id`/`block_name` into a fresh `InlineFormState`, if
    /// it's actually addressable as a `form`-lang fence — `None` for
    /// anything else (wrong lang, block not found), same lenient "just
    /// don't open" posture as a click on a non-existent `ClickTarget`
    /// would already have to tolerate. A `field var=` naming something not
    /// in `self.decls` at all is silently dropped (see
    /// `InlineFormState::decls`'s own doc comment) — `meshfox validate` is
    /// what would have caught that at authoring time, not this.
    fn try_build_inline_form(&self, node_id: &str, block_name: &str) -> Option<InlineFormState> {
        let node = self.display_canvas.node(node_id)?;
        let block = node
            .runnable_blocks()
            .into_iter()
            .find(|b| b.name.as_deref() == Some(block_name))?;
        if !meshfox_core::is_form(&block.lang) {
            return None;
        }
        let form = meshfox_core::form_block(&block).ok()?;
        let mut fields = Vec::new();
        let mut decls = Vec::new();
        let mut inputs = Vec::new();
        for field in form.fields {
            let Some(decl) = self.decls.iter().find(|d| d.name == field.var) else {
                continue;
            };
            let input = self
                .session_vars
                .get(&field.var)
                .cloned()
                .unwrap_or_else(|| coerce_field_input(decl, decl.default.as_deref()));
            fields.push(field);
            decls.push(decl.clone());
            inputs.push(input);
        }
        // The worker knows the resolved values (environment, cache, shared
        // env); they replace the defaults above once they arrive.
        if let Some(port) = self.worker_port {
            let (node, block, tx) = (
                node_id.to_string(),
                block_name.to_string(),
                self.background_tx.clone(),
            );
            tokio::spawn(async move {
                let result = crate::worker_client::get_form_fields(port, &node, &block).await;
                let _ = tx.send(BackgroundMsg::FormFields {
                    node_id: node,
                    block,
                    result,
                });
            });
        }
        Some(InlineFormState {
            node_id: node_id.to_string(),
            block_name: block_name.to_string(),
            fields,
            decls,
            inputs,
            selected: 0,
            editing: false,
        })
    }

    /// `i`, while `Focus::Document` — opens (and immediately starts
    /// editing) the selected node's own `form`-lang fence, if it has
    /// exactly one. With more than one, there's nothing unambiguous for a
    /// bare keypress to pick — same posture `trigger_run`'s own
    /// `BlockPickerState` fallback takes for "more than one runnable
    /// block" — so this just leaves a status hint; a mouse click on the
    /// specific form's own field/Send still works regardless.
    fn try_enter_inline_form(&mut self) {
        let Some(row) = self.rows.get(self.selected) else {
            return;
        };
        let node_id = row.node_id.clone();
        let Some(node) = self.display_canvas.node(&node_id) else {
            return;
        };
        let form_blocks: Vec<String> = node
            .runnable_blocks()
            .into_iter()
            .filter(|b| meshfox_core::is_form(&b.lang))
            .filter_map(|b| b.name)
            .collect();
        // A form selected with `[`/`]` wins over the "only one form" rule.
        if let Some(addr) = self.selected_block.clone() {
            if addr.node_id == node_id && form_blocks.contains(&addr.block_name) {
                if let Some(mut form) = self.try_build_inline_form(&node_id, &addr.block_name) {
                    form.editing = true;
                    self.active_inline_form = Some(form);
                    self.render_current_document();
                } else {
                    self.status = "meshfox: this form has no usable fields".into();
                }
                return;
            }
        }
        match form_blocks.as_slice() {
            [] => self.status = "no form in this node".into(),
            [name] => {
                let name = name.clone();
                if let Some(mut form) = self.try_build_inline_form(&node_id, &name) {
                    form.editing = true;
                    self.selected_block = Some(BlockAddr::new(&node_id, &name));
                    self.active_inline_form = Some(form);
                    self.render_current_document();
                } else {
                    self.status = "meshfox: this form has no usable fields".into();
                }
            }
            _ => {
                self.status = "more than one form in this node — click the one you want".into();
            }
        }
    }

    /// Keyboard handling while `active_inline_form` is actually `editing`
    /// — modeled directly on `on_var_form_key`, just scoped to one form's
    /// own `fields`/`decls`/`inputs` instead of every declared variable at
    /// once, and with an extra virtual "Send" position
    /// (`selected == fields.len()`) `Tab`/arrows can land on.
    async fn on_inline_form_key(&mut self, key: KeyEvent) {
        let Some(form) = &self.active_inline_form else {
            return;
        };
        let send_idx = form.fields.len();
        match key.code {
            KeyCode::Enter => self.submit_inline_form().await,
            KeyCode::Esc => {
                if let Some(form) = &mut self.active_inline_form {
                    form.editing = false;
                }
                self.render_current_document();
            }
            KeyCode::Up | KeyCode::BackTab => {
                if let Some(form) = &mut self.active_inline_form {
                    form.selected = form.selected.checked_sub(1).unwrap_or(send_idx);
                }
                self.render_current_document();
            }
            KeyCode::Down | KeyCode::Tab => {
                if let Some(form) = &mut self.active_inline_form {
                    form.selected = (form.selected + 1) % (send_idx + 1);
                }
                self.render_current_document();
            }
            KeyCode::Left => {
                self.cycle_inline_form_field(-1);
                self.render_current_document();
            }
            KeyCode::Right => {
                self.cycle_inline_form_field(1);
                self.render_current_document();
            }
            KeyCode::Char('o') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                let target = self.active_inline_form.as_ref().and_then(|form| {
                    let i = form.selected;
                    (i < form.decls.len()
                        && matches!(form.decls[i].var_type, VarType::Dir | VarType::File))
                    .then_some(super::path_picker::PickerTarget::InlineForm(i))
                });
                if let Some(target) = target {
                    self.open_path_picker(target).await;
                }
                self.render_current_document();
            }
            KeyCode::Backspace => {
                if let Some(form) = &mut self.active_inline_form {
                    let i = form.selected;
                    if i < form.decls.len()
                        && matches!(
                            form.decls[i].var_type,
                            VarType::String | VarType::Int | VarType::Dir | VarType::File
                        )
                    {
                        form.inputs[i].pop();
                    }
                }
                self.render_current_document();
            }
            KeyCode::Char(c) => {
                if let Some(form) = &mut self.active_inline_form {
                    let i = form.selected;
                    if i < form.decls.len() {
                        let allowed = match form.decls[i].var_type {
                            VarType::String | VarType::Dir | VarType::File => true,
                            VarType::Int => {
                                c.is_ascii_digit()
                                    || ((c == '-' || c == '+') && form.inputs[i].is_empty())
                            }
                            VarType::Bool | VarType::Select => false,
                        };
                        if allowed {
                            form.inputs[i].push(c);
                        }
                    }
                }
                self.render_current_document();
            }
            _ => {}
        }
    }

    /// Left/right on the focused field of `active_inline_form` — mirrors
    /// `cycle_var_form_field` exactly, just against `InlineFormState`.
    /// A no-op when the virtual "Send" row (`selected == fields.len()`) is
    /// focused — nothing to cycle there.
    fn cycle_inline_form_field(&mut self, dir: i32) {
        let Some(form) = &mut self.active_inline_form else {
            return;
        };
        let i = form.selected;
        if i >= form.decls.len() {
            return;
        }
        match form.decls[i].var_type {
            VarType::Bool => {
                form.inputs[i] = if form.inputs[i] == "true" {
                    "false"
                } else {
                    "true"
                }
                .to_string();
            }
            VarType::Select => {
                let choices = &form.decls[i].choices;
                if choices.is_empty() {
                    return;
                }
                let len = choices.len() as i32;
                let current = choices
                    .iter()
                    .position(|c| c == &form.inputs[i])
                    .map(|p| p as i32)
                    .unwrap_or(0);
                let next = (current + dir).rem_euclid(len) as usize;
                form.inputs[i] = choices[next].clone();
            }
            VarType::String | VarType::Int | VarType::Dir | VarType::File => {}
        }
    }

    /// Validates every field (same `validate_value` check `submit_var_form`
    /// already does, same "stay open, jump focus to the offender" recovery
    /// on failure), then commits them all into `self.session_vars` — never
    /// `run_overrides`/the on-disk `var_cache`, since every field here
    /// targets an implicitly-`session` var (see `InlineFormState`'s own
    /// doc comment). Unlike `submit_var_form`, the form stays open
    /// afterward (just `editing = false`) — it reads as part of the
    /// document, so there's nothing to close back out of. Finishes by
    /// queuing every `autorun` block the just-changed values reach
    /// (`meshfox_core::autorun_blocks_for_changed_vars`) and, if no run is
    /// currently using the one foreground slot, kicking off the first of
    /// them right away — `advance_run`'s own "chain exhausted" hook drains
    /// the rest.
    async fn submit_inline_form(&mut self) {
        // The worker refuses a Send while a foreground run is in progress
        // (it commits values *and* starts runs); say so here instead of
        // round-tripping, leaving the form open with what was typed.
        if self.foreground_run_in_progress() {
            self.status = "meshfox: a run is in progress — wait for it to finish (or stop it) before sending the form".into();
            return;
        }
        {
            let Some(form) = &self.active_inline_form else {
                return;
            };
            if let Some((i, e)) = form
                .decls
                .iter()
                .zip(form.inputs.iter())
                .enumerate()
                .find_map(|(i, (d, v))| meshfox_core::validate_value(d, v).err().map(|e| (i, e)))
            {
                let form = self.active_inline_form.as_mut().unwrap();
                form.selected = i;
                self.status = format!("meshfox: {e}");
                return;
            }
        }
        let Some(form) = &self.active_inline_form else {
            return;
        };
        let mut changed = HashSet::new();
        let mut values = HashMap::new();
        for (field, value) in form.fields.iter().zip(form.inputs.iter()) {
            self.session_vars.insert(field.var.clone(), value.clone());
            changed.insert(field.var.clone());
            values.insert(field.var.clone(), value.clone());
        }
        let (form_node_id, form_block_name) = {
            let form = self.active_inline_form.as_ref().unwrap();
            (form.node_id.clone(), form.block_name.clone())
        };
        if let Some(form) = &mut self.active_inline_form {
            form.editing = false;
        }
        self.render_current_document();

        // Worker mode: the run has to happen wherever `state.session_vars`
        // actually lives — this TUI's own `self.session_vars` above is
        // just a local pre-fill convenience for reopening the form (see
        // `InlineFormState::inputs`'s own doc comment), not what a
        // worker-side run would ever consult. `POST /api/form/submit`
        // saves the values server-side and runs every autorun block they
        // reach *there*, before returning — computing `changed`/
        // `autorun_blocks_for_changed_vars` and driving `start_run`
        // ourselves (the fallback branch below) would run the same blocks
        // a *second* time, against a canvas snapshot that never actually
        // saw the new value.
        if let Some(port) = self.worker_port {
            match crate::worker_client::submit_form(port, &form_node_id, &form_block_name, values)
                .await
            {
                Ok(triggered) => {
                    self.status = if triggered.is_empty() {
                        "meshfox: form submitted".into()
                    } else {
                        format!(
                            "meshfox: form submitted — {} autorun block(s) started",
                            triggered.len()
                        )
                    };
                    // Nothing further to do here: `spawn_worker_watcher`/
                    // `on_external_run_event` (already built to show any
                    // passively-discovered run's live output, not just an
                    // autorun's — see their own doc comments) pick up the
                    // `RunStarted` this submission just caused the same
                    // way a *different* tab/TUI submitting this exact form
                    // would rely on.
                }
                Err(e) => {
                    self.status = format!("meshfox: failed to submit form: {e}");
                }
            }
            return;
        }

        let triggered =
            meshfox_core::autorun_blocks_for_changed_vars(&self.display_canvas, &changed);
        let count = triggered.len();
        self.pending_autoruns.extend(triggered);
        self.status = if count == 0 {
            "meshfox: form submitted".into()
        } else {
            format!("meshfox: form submitted — {count} autorun block(s) queued")
        };
        let idle = self.run.as_ref().is_none_or(|r| r.finished)
            && self.file_run.as_ref().is_none_or(|r| r.finished);
        if idle {
            if let Some(addr) = self.pending_autoruns.pop_front() {
                self.start_run(addr.node_id, addr.block_name, true).await;
            }
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
                if bp.history {
                    self.open_run_history_for(bp.node_id, name).await;
                } else {
                    self.start_run(bp.node_id, name, bp.with_deps).await;
                }
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
    /// A `var_form`/`block_picker` modal, when up, claims every click of
    /// its own instead — its own list is the only clickable thing on
    /// screen while it's open (see `on_modal_mouse`).
    ///
    /// Otherwise: clicks select a tree row and focus that pane, same as
    /// before — except a click that lands specifically on a row's own
    /// disclosure marker (`▾`/`▸`, see `ui::render_tree`) toggles it
    /// expanded/collapsed instead, same as clicking it with the keyboard
    /// (`enter`) would; a double-click elsewhere on that same row runs its
    /// node's own default block, mirroring a file manager's
    /// double-click-to-open convention. A click anywhere on any of the
    /// three panes' own title row focuses it and — on its `[+]`/`[-]` icon
    /// specifically, or as a double-click anywhere else on that row (same
    /// "two `Down`s close together" detection `last_click` exists for,
    /// see its own doc comment) — toggles `fullscreen` for it (see
    /// `toggle_fullscreen_on_title_click`). A click on the document pane
    /// that lands on one of `doc_click_targets` (a button fence's own
    /// marker, or a deps line's block name — see `ui::render_document`)
    /// runs/jumps to it, same as `r` or a tree click would. The scroll
    /// wheel over any of the three panes moves/scrolls it — including,
    /// over the Output pane, sideways (`ScrollLeft`/`ScrollRight`), since
    /// its own lines never wrap (see `ui::render_output`). `layout` itself
    /// already reflects `fullscreen` (`ui::compute_layout`), so a collapsed
    /// pane's own `point_in` checks below simply never match.
    pub async fn on_mouse(&mut self, mouse: MouseEvent) {
        if self.run_confirmation.is_some() {
            return;
        }
        if let Some(se) = &mut self.source_editor {
            se.on_mouse(mouse);
            return;
        }
        if self.table_mode.is_some() {
            self.on_table_mouse(mouse);
            return;
        }
        if self.history_view.is_some() {
            self.on_history_view_mouse(mouse).await;
            return;
        }
        if self.run_history_view.is_some() {
            self.on_run_history_mouse(mouse).await;
            return;
        }
        let (cols, rows) = crossterm::terminal::size().unwrap_or((80, 24));
        let area = Rect::new(0, 0, cols, rows);
        if self.path_picker.is_some() {
            self.on_path_picker_mouse(mouse, area).await;
            return;
        }
        if self.var_form.is_some() || self.block_picker.is_some() {
            self.on_modal_mouse(mouse, area).await;
            return;
        }
        let layout = ui::compute_layout(
            area,
            self.fullscreen,
            self.navigation_width_pct(),
            self.output_height,
            self.console_collapsed,
            self.tree_collapsed,
        );

        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                let is_double_click = matches!(
                    self.last_click,
                    Some((r, c, t))
                        if r == mouse.row && c == mouse.column && t.elapsed() < DOUBLE_CLICK_WINDOW
                );
                self.last_click = Some((mouse.row, mouse.column, std::time::Instant::now()));

                if let Some(drag) = self.resize_handle_at(&layout, mouse.column, mouse.row) {
                    self.resize_drag = Some(drag);
                    return; // starting a resize claims the click — no pane underneath it
                }

                if point_in(layout.tree, mouse.column, mouse.row) {
                    if self.tree_collapsed {
                        // The collapsed handle is just a narrow column —
                        // clicking anywhere on it expands the tree back,
                        // same "click it to reopen" affordance Output's
                        // own collapsed strip already has. An explicit
                        // click, not a focus-driven peek, so it stays
                        // expanded even after focus later moves away.
                        self.set_focus(Focus::Tree);
                        self.expand_pane(Focus::Tree);
                    } else {
                        // A plain click on the title row used to also
                        // re-collapse the pane (mirroring Output's own
                        // affordance below) — dropped: it raced a genuine
                        // double-click-to-fullscreen gesture, since the
                        // first of the pair's two `Down`s always landed as
                        // a "plain click" on its own (nothing yet marks it
                        // as the start of a double-click), collapsing the
                        // pane before the second click could ever land on
                        // its now-relocated/shrunk title row to complete
                        // the fullscreen toggle. `z` (keyboard) is now the
                        // only way to collapse Tree by hand.
                        self.toggle_fullscreen_on_title_click(
                            Focus::Tree,
                            layout.tree,
                            &mouse,
                            is_double_click,
                        );
                        let inner_x = layout.tree.x + 1; // left border
                        let inner_y = layout.tree.y + 1; // top border
                        let map_area = Rect::new(
                            inner_x,
                            inner_y,
                            layout.tree.width.saturating_sub(2),
                            layout.tree.height.saturating_sub(4),
                        );
                        if spatial::active_spatial_parent(self).is_some() {
                            if let Some(id) =
                                spatial::hit_test(self, map_area, mouse.column, mouse.row)
                            {
                                self.jump_to_node(&id);
                                if is_double_click {
                                    self.trigger_run(true).await;
                                }
                            }
                        } else if let Some((rect, clicked)) = self
                            .tree_click_rows
                            .iter()
                            .find(|(rect, _)| point_in(*rect, mouse.column, mouse.row))
                            .copied()
                        {
                            if let Some(row) = self.rows.get(clicked) {
                                // "  " * depth (indent) then a 2-column-wide
                                // disclosure marker — see `ui::render_tree`'s
                                // own `indent`/`disclosure` spans, which this
                                // has to stay in step with.
                                let disclosure_col = inner_x + row.depth as u16 * 2;
                                let on_disclosure = row.has_children
                                    && mouse.row == rect.y
                                    && mouse.column >= disclosure_col
                                    && mouse.column < disclosure_col + 2;

                                if clicked != self.selected {
                                    self.selected = clicked;
                                    self.doc_scroll = 0;
                                    self.render_current_document();
                                }
                                if on_disclosure {
                                    self.toggle_expand();
                                } else if is_double_click {
                                    if self.rows[self.selected].reference.is_some() {
                                        let target = self.rows[self.selected].node_id.clone();
                                        self.jump_to_node(&target);
                                    } else {
                                        self.trigger_run(true).await;
                                    }
                                }
                            }
                        }
                    }
                } else if point_in(layout.document, mouse.column, mouse.row) {
                    self.toggle_fullscreen_on_title_click(
                        Focus::Document,
                        layout.document,
                        &mouse,
                        is_double_click,
                    );
                    let target = self
                        .doc_click_targets
                        .iter()
                        .find(|(r, _)| point_in(*r, mouse.column, mouse.row))
                        .map(|(_, t)| t.clone());
                    if let Some(target) = target {
                        self.activate_click_target(target).await;
                    }
                } else if point_in(layout.output, mouse.column, mouse.row) {
                    if self.console_collapsed {
                        // The collapsed strip is just one row — clicking
                        // anywhere on it expands the console, rather than
                        // trying to hit-test a real title row that barely
                        // exists at this height. An explicit click, not a
                        // focus-driven peek, so it stays expanded even
                        // after focus later moves away.
                        self.set_focus(Focus::Output);
                        self.expand_pane(Focus::Output);
                    } else {
                        // A plain click on the title row used to also
                        // re-collapse the console back down to its strip —
                        // dropped: it raced a genuine double-click-to-
                        // fullscreen gesture the same way Tree's own
                        // (now-removed) version did (see that branch's own
                        // comment above for the exact mechanism). `z`
                        // (keyboard) or just waiting out
                        // `CONSOLE_COLLAPSE_GRACE` after a run finishes are
                        // now the only ways back to the collapsed strip.
                        self.toggle_fullscreen_on_title_click(
                            Focus::Output,
                            layout.output,
                            &mouse,
                            is_double_click,
                        );
                    }
                }
            }
            MouseEventKind::ScrollDown => {
                if point_in(layout.tree, mouse.column, mouse.row) {
                    self.move_selection(1);
                } else if point_in(layout.document, mouse.column, mouse.row) {
                    self.scroll_document(3);
                } else if point_in(layout.output, mouse.column, mouse.row) {
                    self.scroll_output(3);
                }
            }
            MouseEventKind::ScrollUp => {
                if point_in(layout.tree, mouse.column, mouse.row) {
                    self.move_selection(-1);
                } else if point_in(layout.document, mouse.column, mouse.row) {
                    self.scroll_document(-3);
                } else if point_in(layout.output, mouse.column, mouse.row) {
                    self.scroll_output(-3);
                }
            }
            MouseEventKind::ScrollRight if point_in(layout.output, mouse.column, mouse.row) => {
                self.scroll_output_h(5);
            }
            MouseEventKind::ScrollLeft if point_in(layout.output, mouse.column, mouse.row) => {
                self.scroll_output_h(-5);
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                self.on_resize_drag(area, mouse.column, mouse.row);
            }
            MouseEventKind::Up(MouseButton::Left) => {
                self.resize_drag = None;
            }
            _ => {}
        }
    }

    /// Whether `addr` names a `form` fence (which `[`/`]` can select, but
    /// which has no run, source or output of its own).
    fn is_form_block(&self, addr: &BlockAddr) -> bool {
        self.display_canvas.node(&addr.node_id).is_some_and(|node| {
            node.runnable_blocks().iter().any(|b| {
                b.name.as_deref() == Some(addr.block_name.as_str())
                    && meshfox_core::is_form(&b.lang)
            })
        })
    }

    pub(super) fn is_button_block(&self, addr: &BlockAddr) -> bool {
        self.display_canvas.node(&addr.node_id).is_some_and(|node| {
            node.runnable_blocks().iter().any(|b| {
                b.name.as_deref() == Some(addr.block_name.as_str())
                    && meshfox_core::is_button(&b.lang)
            })
        })
    }

    pub(super) fn render_block_label(&self, addr: &BlockAddr) -> Option<String> {
        let index = addr
            .block_name
            .strip_prefix("@render:")?
            .parse::<usize>()
            .ok()?;
        let result = self
            .display_canvas
            .node(&addr.node_id)?
            .render_results
            .get(index)?;
        Some(
            result
                .label
                .strip_prefix(&format!("{}/", addr.node_id))
                .unwrap_or(&result.label)
                .to_owned(),
        )
    }

    fn select_adjacent_block(&mut self, direction: i32) {
        let Some(row) = self.rows.get(self.selected) else {
            return;
        };
        let Some(node) = self.display_canvas.node(&row.node_id) else {
            return;
        };
        let mut positions: Vec<_> = node
            .runnable_blocks()
            .into_iter()
            .filter_map(|b| b.name.map(|name| (b.span.start, name)))
            .collect();
        positions.extend(
            meshfox_core::fence::scan_render_blocks(&node.text)
                .into_iter()
                .take(node.render_results.len())
                .enumerate()
                .map(|(i, b)| (b.span.start, format!("@render:{i}"))),
        );
        positions.sort_by_key(|(start, _)| *start);
        let blocks: Vec<_> = positions.into_iter().map(|(_, name)| name).collect();
        if blocks.is_empty() {
            return;
        }
        let current = self
            .selected_block
            .as_ref()
            .and_then(|addr| blocks.iter().position(|name| name == &addr.block_name));
        let next = match current {
            Some(i) => (i as i32 + direction).rem_euclid(blocks.len() as i32) as usize,
            None if direction < 0 => blocks.len() - 1,
            None => 0,
        };
        self.selected_block = Some(BlockAddr::new(&node.id, &blocks[next]));
        self.reveal_selected_block = true;
        self.render_current_document();
    }

    async fn activate_block_action(&mut self, addr: BlockAddr, action: markdown::BlockAction) {
        use markdown::BlockAction;
        let Some(node) = self.display_canvas.node(&addr.node_id) else {
            return;
        };
        if let Some(index) = addr
            .block_name
            .strip_prefix("@render:")
            .and_then(|s| s.parse::<usize>().ok())
        {
            if index < node.render_results.len()
                && matches!(action, BlockAction::ToggleSource | BlockAction::ToggleBlock)
            {
                self.selected_block = Some(addr.clone());
                self.set_focus(Focus::Document);
                let view = self.block_views.entry(addr).or_default();
                view.source_expanded = Some(!view.source_expanded.unwrap_or(false));
                self.render_current_document();
            }
            return;
        }
        let Some(block) = node
            .runnable_blocks()
            .into_iter()
            .find(|b| b.name.as_deref() == Some(&addr.block_name))
        else {
            return;
        };
        if let Some(form) = &mut self.active_inline_form {
            form.editing = false;
        }
        self.selected_block = Some(addr.clone());
        self.set_focus(Focus::Document);
        match action {
            BlockAction::Select => {}
            BlockAction::ToggleBlock => {
                let view = self.block_views.entry(addr.clone()).or_default();
                view.collapsed = !view.collapsed;
                self.reveal_selected_block = true;
            }
            BlockAction::ToggleSource => {
                let view = self.block_views.entry(addr.clone()).or_default();
                view.source_expanded = Some(!view.source_expanded.unwrap_or(!block.fold));
                self.reveal_selected_block = true;
            }
            BlockAction::History => {
                if block.service
                    || meshfox_core::is_button(&block.lang)
                    || meshfox_core::is_form(&block.lang)
                {
                    self.status = "this block keeps no run history".into();
                } else {
                    self.open_run_history_for(addr.node_id, addr.block_name)
                        .await;
                }
            }
            BlockAction::Run | BlockAction::Chain => {
                self.trigger_run(action == BlockAction::Chain).await;
            }
        }
        self.render_current_document();
    }

    /// A `ClickTarget` (see its own doc comment) actually being clicked —
    /// split out of `on_mouse` so its own `match` doesn't have to live
    /// inside that already-long function. Respects the same "one run at a
    /// time" guard `trigger_run` applies to a keyboard `r`/`R`, since a
    /// button-fence click reaches `start_run` directly rather than through
    /// `trigger_run` itself (only `trigger_run` knows how to fall back to
    /// the block picker, which a click never needs — it always names its
    /// own exact block already).
    async fn activate_click_target(&mut self, target: ClickTarget) {
        match target {
            ClickTarget::Block {
                node_id,
                block_name,
                action,
            } => {
                self.activate_block_action(BlockAddr::new(&node_id, &block_name), action)
                    .await;
            }
            ClickTarget::RunBlock {
                node_id,
                block_name,
            } => {
                if self.run.as_ref().is_some_and(|r| !r.finished)
                    || self.file_run.as_ref().is_some_and(|r| !r.finished)
                {
                    self.status = "a run is already in progress — press K to kill it first".into();
                    return;
                }
                self.start_run(node_id, block_name, true).await;
            }
            ClickTarget::JumpToNode { node_id } => self.jump_to_node(&node_id),
            ClickTarget::FormField {
                node_id,
                block_name,
                field_index,
            } => {
                self.select_form_block(&node_id, &block_name);
                self.ensure_inline_form_open(&node_id, &block_name);
                if let Some(form) = &mut self.active_inline_form {
                    if field_index < form.fields.len() {
                        form.selected = field_index;
                        form.editing = true;
                    }
                }
                self.render_current_document();
            }
            ClickTarget::FormSend {
                node_id,
                block_name,
            } => {
                self.select_form_block(&node_id, &block_name);
                self.ensure_inline_form_open(&node_id, &block_name);
                self.submit_inline_form().await;
            }
            ClickTarget::FormBrowse {
                node_id,
                block_name,
                field_index,
            } => {
                self.select_form_block(&node_id, &block_name);
                self.ensure_inline_form_open(&node_id, &block_name);
                let usable = self.active_inline_form.as_mut().is_some_and(|form| {
                    let ok = field_index < form.decls.len()
                        && matches!(
                            form.decls[field_index].var_type,
                            VarType::Dir | VarType::File
                        );
                    if ok {
                        form.selected = field_index;
                        form.editing = true;
                    }
                    ok
                });
                if usable {
                    self.open_path_picker(super::path_picker::PickerTarget::InlineForm(
                        field_index,
                    ))
                    .await;
                }
                self.render_current_document();
            }
        }
    }

    /// Selects the form fence `(node_id, block_name)` the way `[`/`]` would
    /// (frame highlight, Document focus) — what clicking into a form does.
    fn select_form_block(&mut self, node_id: &str, block_name: &str) {
        self.selected_block = Some(BlockAddr::new(node_id, block_name));
        self.set_focus(Focus::Document);
    }

    /// Makes sure `active_inline_form` is open for exactly this
    /// `(node_id, block_name)` — rebuilding it fresh if it's currently
    /// open for a *different* form (only one at a time, same rule
    /// `var_form`/`block_picker` already have), or wasn't open at all.
    /// Already being open for the *same* form is left untouched, so an
    /// in-progress edit (`inputs`) survives clicking a different field of
    /// the same form.
    fn ensure_inline_form_open(&mut self, node_id: &str, block_name: &str) {
        let already_this_one = self
            .active_inline_form
            .as_ref()
            .is_some_and(|f| f.node_id == node_id && f.block_name == block_name);
        if !already_this_one {
            self.active_inline_form = self.try_build_inline_form(node_id, block_name);
        }
    }

    /// Lists `path` for the path picker — through the worker when there is
    /// one (the file system a run happens on), otherwise straight off disk.
    async fn picker_list(
        &self,
        path: &str,
        kind: meshfox_core::fsbrowse::PickKind,
        hidden: bool,
    ) -> Result<meshfox_core::fsbrowse::FsListing, String> {
        match self.worker_port {
            Some(port) => crate::worker_client::fs_list(port, path, kind, hidden).await,
            None => meshfox_core::fsbrowse::list(path, kind, hidden, &self.canvas_path)
                .map_err(|e| e.to_string()),
        }
    }

    /// Opens the path picker for the focused `dir`/`file` field, starting
    /// from the text it currently holds.
    async fn open_path_picker(&mut self, target: super::path_picker::PickerTarget) {
        use super::path_picker::{PathPickerState, PickerTarget};
        let (decl, current) = match target {
            PickerTarget::VarForm(i) => match &self.var_form {
                Some(vf) => (vf.decls[i].clone(), vf.inputs[i].clone()),
                None => return,
            },
            PickerTarget::InlineForm(i) => match &self.active_inline_form {
                Some(f) => (f.decls[i].clone(), f.inputs[i].clone()),
                None => return,
            },
        };
        let mut picker = PathPickerState::new(target, &decl);
        match self.picker_list(&current, picker.kind, false).await {
            Ok(listing) => picker.set_listing(listing),
            Err(e) => picker.error = Some(e),
        }
        self.path_picker = Some(picker);
    }

    async fn on_path_picker_key(&mut self, key: KeyEvent) {
        let Some(picker) = &mut self.path_picker else {
            return;
        };
        let action = picker.on_key(key);
        self.apply_picker_action(action).await;
    }

    async fn apply_picker_action(&mut self, action: super::path_picker::PickerAction) {
        use super::path_picker::{PickerAction, PickerTarget};
        match action {
            PickerAction::Stay => {}
            PickerAction::Cancel => self.path_picker = None,
            PickerAction::Navigate(path) => {
                let Some(picker) = &self.path_picker else {
                    return;
                };
                let (kind, hidden) = (picker.kind, picker.show_hidden);
                let result = self.picker_list(&path, kind, hidden).await;
                if let Some(picker) = &mut self.path_picker {
                    match result {
                        Ok(listing) => picker.set_listing(listing),
                        Err(e) => picker.error = Some(e),
                    }
                }
            }
            PickerAction::Choose(path) => {
                let Some(picker) = self.path_picker.take() else {
                    return;
                };
                match picker.target {
                    PickerTarget::VarForm(i) => {
                        if let Some(vf) = &mut self.var_form {
                            if let Some(slot) = vf.inputs.get_mut(i) {
                                *slot = path;
                            }
                        }
                    }
                    PickerTarget::InlineForm(i) => {
                        if let Some(form) = &mut self.active_inline_form {
                            if let Some(slot) = form.inputs.get_mut(i) {
                                *slot = path;
                            }
                        }
                        self.render_current_document();
                    }
                }
            }
        }
    }

    /// Mouse for the open path picker: a click on a row selects it (a second
    /// click on the selected row activates it, like `Enter`), the wheel
    /// moves the selection, and the geometry is `ui::path_picker_layout`'s
    /// own so a hit-test can't drift from what is drawn.
    async fn on_path_picker_mouse(&mut self, mouse: MouseEvent, area: Rect) {
        let Some(picker) = &mut self.path_picker else {
            return;
        };
        let layout = ui::path_picker_layout(area, picker.save);
        let action = match mouse.kind {
            MouseEventKind::ScrollUp => {
                picker.scroll(-3);
                None
            }
            MouseEventKind::ScrollDown => {
                picker.scroll(3);
                None
            }
            MouseEventKind::Down(MouseButton::Left) => {
                let inside = mouse.column >= layout.list.x
                    && mouse.column < layout.list.x + layout.list.width
                    && mouse.row >= layout.list.y
                    && mouse.row < layout.list.y + layout.list.height;
                if inside {
                    let start = super::path_picker::window_start(
                        picker.selected,
                        layout.list.height as usize,
                    );
                    Some(picker.click_row(start + (mouse.row - layout.list.y) as usize))
                } else {
                    None
                }
            }
            _ => None,
        };
        if let Some(action) = action {
            self.apply_picker_action(action).await;
        }
    }

    /// `on_mouse`'s own modal branch: while `var_form`/`block_picker` is up,
    /// a left-click on one of its own list rows selects it, same as `j`/`k`
    /// would — everything else (including a click that lands outside the
    /// modal's own rect, on whatever pane is still visible behind it) is a
    /// no-op, since there's no pane underneath a modal to click through to.
    /// `list_rect` mirrors exactly what `render_var_form`/
    /// `render_block_picker` themselves compute (`ui::var_form_list_rect`/
    /// `ui::block_picker_list_rect`), so a row's hit-test can never drift
    /// from where it actually renders.
    async fn on_modal_mouse(&mut self, mouse: MouseEvent, area: Rect) {
        if !matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
            return;
        }
        if let Some(vf) = &mut self.var_form {
            let list_rect = ui::var_form_list_rect(area, vf.decls.len());
            let mut browse = None;
            if mouse.row >= list_rect.y && mouse.row < list_rect.y + list_rect.height {
                let idx = (mouse.row - list_rect.y) as usize;
                if idx < vf.decls.len() {
                    vf.selected = idx;
                    // The right-aligned "[Browse… ^o]" strip of a
                    // `dir`/`file` row (see `render_var_form`).
                    let marker = super::path_picker::BROWSE_MARKER.chars().count() as u16;
                    let on_marker = mouse.column + marker >= list_rect.x + list_rect.width
                        && mouse.column < list_rect.x + list_rect.width;
                    if on_marker && matches!(vf.decls[idx].var_type, VarType::Dir | VarType::File) {
                        browse = Some(idx);
                    }
                }
            }
            if let Some(idx) = browse {
                self.open_path_picker(super::path_picker::PickerTarget::VarForm(idx))
                    .await;
            }
        } else if let Some(bp) = &mut self.block_picker {
            let list_rect = ui::block_picker_list_rect(area, bp.blocks.len());
            if mouse.row >= list_rect.y && mouse.row < list_rect.y + list_rect.height {
                let idx = (mouse.row - list_rect.y) as usize;
                if idx < bp.blocks.len() {
                    bp.selected = idx;
                }
            }
        }
    }

    /// Which border (if any) `(col, row)` lands on — the seam between the
    /// tree and document panes, or between the tree/document row and
    /// Output below it. `None` while a pane is fullscreen — there's nothing
    /// to drag apart then (`ui::compute_layout` itself ignores
    /// `tree_width_pct`/`output_height` in that case too).
    ///
    /// The vertical seam is genuinely two adjacent border columns (tree's
    /// own right border, then document's own left border right next to
    /// it) — either counts, since neither has any other click behavior of
    /// its own to conflict with. The horizontal seam is deliberately
    /// one-sided: only the tree/document row's own *bottom* border counts,
    /// never Output's own *top* border right below it — that row is
    /// Output's own title row, with its own click-to-focus/
    /// double-click-to-fullscreen behavior
    /// (`toggle_fullscreen_on_title_click`) that a resize-drag start must
    /// not shadow.
    fn resize_handle_at(&self, layout: &ui::PaneLayout, col: u16, row: u16) -> Option<ResizeDrag> {
        if self.fullscreen.is_some() {
            return None;
        }
        let in_tree_document_rows =
            row >= layout.tree.y && row < layout.tree.y + layout.tree.height;
        if in_tree_document_rows && (col == layout.document.x || col + 1 == layout.document.x) {
            return Some(ResizeDrag::Vertical);
        }
        if row + 1 == layout.tree.y + layout.tree.height {
            return Some(ResizeDrag::Horizontal);
        }
        None
    }

    /// The other half of a resize gesture `resize_handle_at` started —
    /// called on every `MouseEventKind::Drag(Left)` while `resize_drag` is
    /// `Some`, translating the cursor's current position back into a new
    /// `tree_width_pct`/`output_height`. `area` is the whole terminal, same
    /// as what `ui::compute_layout` itself sizes the panes against, so a
    /// drag to column/row `N` reproduces exactly the split that would put
    /// the border there.
    fn on_resize_drag(&mut self, area: Rect, col: u16, row: u16) {
        match self.resize_drag {
            None => {}
            Some(ResizeDrag::Vertical) => {
                if area.width == 0 {
                    return;
                }
                let pct = (col.saturating_sub(area.x) as u32 * 100 / area.width as u32) as u16;
                let pct = pct.clamp(MIN_TREE_WIDTH_PCT, MAX_TREE_WIDTH_PCT);
                if spatial::active_spatial_parent(self).is_some() {
                    self.spatial_width_pct = Some(pct);
                } else {
                    self.tree_width_pct = pct;
                }
            }
            Some(ResizeDrag::Horizontal) => {
                let content_height = area.height.saturating_sub(ui::FOOTER_HEIGHT);
                let max_output_height = content_height.saturating_sub(ui::MIN_MAIN_HEIGHT);
                if max_output_height < MIN_OUTPUT_HEIGHT {
                    return; // terminal too short to leave any room to negotiate
                }
                let target_row = row.saturating_sub(area.y);
                let new_height = content_height.saturating_sub(target_row);
                self.output_height = new_height.clamp(MIN_OUTPUT_HEIGHT, max_output_height);
            }
        }
    }

    // -- tree navigation --------------------------------------------------

    /// Give positioned siblings a wider default without changing the ordinary
    /// tree. Dragging the seam records a separate preferred map width.
    pub(super) fn navigation_width_pct(&self) -> u16 {
        if spatial::active_spatial_parent(self).is_some() {
            self.spatial_width_pct.unwrap_or(65)
        } else {
            self.tree_width_pct
        }
    }

    fn move_selection(&mut self, delta: i32) {
        if self.rows.is_empty() {
            return;
        }
        let direction = if delta < 0 {
            spatial::Direction::Up
        } else {
            spatial::Direction::Down
        };
        if self.move_spatial_selection(direction) {
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

    /// `PageUp`/`PageDown` in the tree pane: jump ten rows (clamped at
    /// either end). In a spatial projection "ten rows" has no flat meaning,
    /// so it steps ten cards along the compact grid instead.
    fn page_selection(&mut self, sign: i32) {
        if spatial::active_spatial_parent(self).is_some() {
            for _ in 0..10 {
                self.move_selection(sign);
            }
        } else {
            self.move_selection(sign * 10);
        }
    }

    /// Returns true whenever the spatial pane owns the key, including when
    /// there is no card farther in that direction.
    fn move_spatial_selection(&mut self, direction: spatial::Direction) -> bool {
        let Some(parent_id) = spatial::active_spatial_parent(self) else {
            return false;
        };
        let current = &self.rows[self.selected].node_id;
        if let Some(id) = spatial::neighbor(&self.display_canvas, &parent_id, current, direction) {
            if let Some(index) = self
                .rows
                .iter()
                .position(|row| row.reference.is_none() && row.node_id == id)
            {
                if index != self.selected {
                    self.selected = index;
                    self.doc_scroll = 0;
                    self.render_current_document();
                }
            }
        }
        true
    }

    /// Leave this positioned sibling set from any card or its overview by
    /// folding its parent. Keeping it expanded would expose the same cards
    /// again as ordinary tree rows and make moving past the group tedious.
    fn exit_spatial_parent(&mut self) -> bool {
        let Some(group_id) = spatial::active_spatial_parent(self) else {
            return false;
        };
        self.expanded.remove(&group_id);
        self.rebuild_rows();
        self.jump_to_node(&group_id);
        true
    }

    fn scroll_document(&mut self, delta: i32) {
        self.doc_scroll = (self.doc_scroll as i32 + delta).max(0) as u16;
    }

    /// `output_scroll` counts lines back from the *bottom* (the live
    /// tail), the opposite sense from `doc_scroll` (which counts forward
    /// from the top) — so a positive `delta` here (the same "down"/
    /// "forward" direction `scroll_document`'s own callers already use)
    /// *decreases* it, moving back toward the tail, while a negative one
    /// increases it, revealing earlier lines.
    fn scroll_output(&mut self, delta: i32) {
        self.output_scroll = (self.output_scroll as i32 - delta).max(0) as u16;
    }

    /// `output_hscroll`'s own counterpart to `scroll_output` — see that
    /// field's own doc comment. Unlike `output_scroll`, "forward"/"back"
    /// only ever means "right"/"left" here, so a positive `delta` increases
    /// it directly, no sign flip.
    fn scroll_output_h(&mut self, delta: i32) {
        self.output_hscroll = (self.output_hscroll as i32 + delta).max(0) as u16;
    }

    /// The TUI's counterpart to the web UI's `jumpTo` (`web/src/MeshNode.tsx`)
    /// — moves the tree's own selection to `node_id`'s row, expanding every
    /// collapsed ancestor first (mirrors `App::new`'s own deep-link
    /// expansion) so that row actually exists in `self.rows` to jump to.
    /// There's no canvas to pan/scroll to in the TUI the way there is on
    /// the web, so "jump" here is purely a tree-selection change — see
    /// `ClickTarget::JumpToNode`'s own doc comment.
    fn jump_to_node(&mut self, node_id: &str) {
        let mut expanded_any = false;
        let mut current = self
            .display_canvas
            .node(node_id)
            .and_then(|n| n.parent.clone());
        while let Some(id) = current {
            current = self.display_canvas.node(&id).and_then(|n| n.parent.clone());
            if self.expanded.insert(id) {
                expanded_any = true;
            }
        }
        if expanded_any {
            self.rebuild_rows();
        }
        if let Some(idx) = self
            .rows
            .iter()
            .position(|r| r.reference.is_none() && r.node_id == node_id)
        {
            if idx != self.selected {
                self.selected = idx;
                self.doc_scroll = 0;
                self.render_current_document();
            }
        }
    }

    fn on_search_key(&mut self, key: KeyEvent) {
        let previous_query = self.search.query.clone();
        match key.code {
            KeyCode::Esc => {
                self.search.open = false;
                self.search.revealed.clear();
                self.search.reveal_pending = false;
                self.render_current_document();
            }
            KeyCode::Enter | KeyCode::BackTab => {
                let matches = self.search.matches(&self.display_canvas);
                if matches.is_empty() {
                    return;
                }
                let index = self.search.index.min(matches.len() - 1);
                let previous =
                    key.code == KeyCode::BackTab || key.modifiers.contains(KeyModifiers::SHIFT);
                self.search.index = if previous {
                    (index + matches.len() - 1) % matches.len()
                } else {
                    (index + 1) % matches.len()
                };
                self.reveal_search_match();
            }
            KeyCode::Backspace => {
                let previous = self.search.previous_char();
                self.search.query.drain(previous..self.search.cursor);
                self.search.cursor = previous;
                self.search.index = 0;
            }
            KeyCode::Delete => {
                let next = self.search.next_char();
                self.search.query.drain(self.search.cursor..next);
                self.search.index = 0;
            }
            KeyCode::Left => self.search.cursor = self.search.previous_char(),
            KeyCode::Right => self.search.cursor = self.search.next_char(),
            KeyCode::Home => self.search.cursor = 0,
            KeyCode::End => self.search.cursor = self.search.query.len(),
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.search.query.clear();
                self.search.cursor = 0;
                self.search.index = 0;
            }
            KeyCode::Char(c)
                if !key.modifiers.intersects(
                    KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER,
                ) =>
            {
                self.search.query.insert(self.search.cursor, c);
                self.search.cursor += c.len_utf8();
                self.search.index = 0;
            }
            _ => {}
        }
        if self.search.open && self.search.query != previous_query {
            self.reveal_search_match();
        }
    }

    fn reveal_search_match(&mut self) {
        self.search.reveal_pending = false;
        let matches = self.search.matches(&self.display_canvas);
        let Some(current) = matches.get(self.search.index.min(matches.len().saturating_sub(1)))
        else {
            return;
        };
        self.search.index = self.search.index.min(matches.len() - 1);
        let id = &current.node_id;
        // Restore only branches this search opened, then reveal the
        // current candidate. Closing keeps the last visible branch.
        for revealed in self.search.revealed.drain() {
            if !self.search.expanded_before.contains(&revealed) {
                self.expanded.remove(&revealed);
            }
        }
        let mut current = Some(id.clone());
        while let Some(node_id) = current {
            current = self
                .display_canvas
                .node(&node_id)
                .and_then(|n| n.parent.clone());
            if self.expanded.insert(node_id.clone()) {
                self.search.revealed.insert(node_id);
            }
        }
        self.rebuild_rows();
        self.jump_to_node(id);
        if let Some(node) = self.display_canvas.node(id) {
            for block in node.runnable_blocks() {
                if !super::search::ranges(&block.code, self.search.query.trim()).is_empty() {
                    if let Some(name) = block.name {
                        self.block_views.insert(
                            BlockAddr {
                                node_id: id.clone(),
                                block_name: name,
                            },
                            markdown::BlockView {
                                collapsed: false,
                                source_expanded: Some(true),
                            },
                        );
                    }
                }
            }
        }
        self.selected_block = None;
        self.tree_collapsed = false;
        if self.fullscreen.is_some() {
            self.fullscreen = Some(Focus::Document);
        }
        self.render_current_document();
        self.search.reveal_pending = true;
    }

    /// Shared by `on_mouse`'s tree/document/output branches: a click
    /// anywhere in `pane`'s own `rect` focuses it, and — if it landed on
    /// `pane`'s own `[+]`/`[-]` icon (`ui::fullscreen_icon_title`, drawn
    /// flush against `rect`'s own top-right corner via
    /// `Line::right_aligned`) or was a double-click anywhere else on that
    /// same title row — toggles `fullscreen` for it, same as pressing `f`
    /// while it's focused would.
    fn toggle_fullscreen_on_title_click(
        &mut self,
        pane: Focus,
        rect: Rect,
        mouse: &MouseEvent,
        is_double_click: bool,
    ) {
        self.set_focus(pane);
        let icon_start = rect
            .x
            .saturating_add(rect.width)
            .saturating_sub(1 + ui::FULLSCREEN_ICON_WIDTH);
        let hit_title_row = mouse.row == rect.y;
        let on_icon = hit_title_row
            && mouse.column >= icon_start
            && mouse.column < icon_start + ui::FULLSCREEN_ICON_WIDTH;
        let toggled = on_icon || (is_double_click && hit_title_row);
        if toggled {
            self.fullscreen = if self.fullscreen == Some(pane) {
                None
            } else {
                Some(pane)
            };
        }
    }

    fn toggle_expand(&mut self) {
        let Some(row) = self.rows.get(self.selected) else {
            return;
        };
        if row.reference.is_some() {
            let target = row.node_id.clone();
            self.jump_to_node(&target);
            return;
        }
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
        if row.reference.is_some() {
            let target = row.node_id.clone();
            self.jump_to_node(&target);
            return;
        }
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
        let current = self.rows.get(self.selected).map(|row| {
            (
                row.node_id.clone(),
                row.reference
                    .as_ref()
                    .map(|reference| (reference.source_id.clone(), reference.edge_index)),
            )
        });
        self.rows = tree::flatten(&self.display_canvas, &self.expanded);
        match current.and_then(|(id, reference)| {
            self.rows.iter().position(|row| {
                row.node_id == id
                    && match (&row.reference, &reference) {
                        (None, None) => true,
                        (Some(found), Some((source, index))) => {
                            found.source_id == *source && found.edge_index == *index
                        }
                        _ => false,
                    }
            })
        }) {
            Some(pos) => self.selected = pos,
            None => self.selected = self.selected.min(self.rows.len().saturating_sub(1)),
        }
    }

    /// `display` is the worker's include-resolved document
    /// (`GET /api/canvas`, see `fetch_display_canvas`). `None` — the fetch
    /// failed, e.g. a broken `include` target — falls back to the
    /// unresolved canvas so the rest of the document stays browsable.
    fn rebuild_display_canvas(&mut self, display: Option<Canvas>) {
        self.display_canvas = match display {
            Some(display) => display,
            None => local_display_canvas(&self.canvas, &self.canvas_path),
        };
        self.constraint_stats = constraint_stats(&self.display_canvas);
    }

    /// The worker's include-resolved, constraint-annotated document
    /// (`GET /api/canvas`) — what the tree and Document pane render.
    /// `None` without a worker or when the worker refuses to resolve it.
    async fn fetch_display_canvas(&self) -> Option<Canvas> {
        crate::worker_client::get_canvas(self.worker_port?)
            .await
            .ok()
    }

    /// Applies an edit to `canvas_path` made by something other than this
    /// process — reported by the background file-watcher thread (see
    /// `mod.rs`'s `spawn_worker_watcher`). Deferred instead
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
    pub fn on_external_change(&mut self, content: String, display: Option<Canvas>) {
        if content == self.raw && display.as_ref().is_none_or(|d| *d == self.display_canvas) {
            return;
        }
        if self.source_editor.is_some() {
            self.pending_external_change = Some((content, display));
            self.status = "file changed on disk — will reload once the editor closes".into();
            return;
        }
        self.apply_external_reload(content, display);
    }

    fn apply_external_reload(&mut self, content: String, display: Option<Canvas>) {
        if content == self.raw && display.as_ref().is_none_or(|d| *d == self.display_canvas) {
            return;
        }
        self.raw = content;
        match Canvas::from_markdown(&self.raw) {
            Ok(parsed) => {
                self.canvas = parsed;
                self.decls = declared_vars(&self.canvas).unwrap_or_default();
                self.rebuild_display_canvas(display);
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

    /// Open via the worker: it owns target resolution, file access and
    /// coordinator requests; the TUI only handles the returned navigation.
    async fn trigger_open_file(&mut self) {
        let Some(row) = self.rows.get(self.selected) else {
            return;
        };
        let node_id = row.node_id.clone();
        let link = self
            .display_canvas
            .node(&node_id)
            .filter(|n| n.node_type == NodeType::Link)
            .map(|n| n.target.clone());
        if let Some(target) = link {
            let Some(url) = target.filter(|t| !t.is_empty()) else {
                self.status = "link has no target".into();
                return;
            };
            if !(url.starts_with("http://") || url.starts_with("https://")) {
                self.status = format!("not opening non-http link: {url}");
                return;
            }
            let shown = url.clone();
            self.status = match tokio::task::spawn_blocking(move || open::that(&url)).await {
                Ok(Ok(())) => format!("opened {shown}"),
                Ok(Err(e)) => format!("failed to open: {e}"),
                Err(e) => format!("failed to open: {e}"),
            };
            return;
        }
        let Some(port) = self.worker_port else {
            self.status = "no worker for canvas".into();
            return;
        };
        match crate::worker_client::open_node_file(port, &node_id).await {
            Ok(crate::worker_client::OpenAction::Done) => self.status = "opened".into(),
            Ok(crate::worker_client::OpenAction::OpenCanvas {
                path,
                fragment,
                port,
            }) => {
                self.pending_child_canvas = Some(PendingChildCanvas {
                    path,
                    port,
                    node: fragment,
                });
            }
            Err(e) => self.status = format!("failed to open: {e}"),
        }
    }

    // -- source editor --------------------------------------------------

    /// `e` — opens the fullscreen source editor (`source_editor.rs`) on
    /// whichever real file the selected node's content actually lives in,
    /// with the cursor at that node's own body. Mirrors the server's own
    /// `locate_node` (`crates/server/src/lib.rs`): a node found directly in
    /// `display_canvas` lives in the primary document. `plain_markdown_include`
    /// (see that field's own doc comment) is the one case with no per-node
    /// identity of its own at all — its target's own content was dumped
    /// straight into this node's body, so there's no node-specific cursor
    /// position to jump to; just opens that file at the top.
    async fn open_source_editor(&mut self) {
        if self.read_only {
            self.status = READ_ONLY_STATUS.into();
            return;
        }
        let Some(row) = self.rows.get(self.selected) else {
            return;
        };
        let node_id = row.node_id.clone();
        let Some(node) = self.display_canvas.node(&node_id) else {
            return;
        };

        let plain_include = node.plain_markdown_include;
        let Some(port) = self.worker_port else {
            self.status = "no worker for this canvas".into();
            return;
        };
        let files = match self.include_files(port).await {
            Ok(files) => files,
            Err(e) => {
                self.status = format!("couldn't list this canvas's includes: {e}");
                return;
            }
        };

        let (path, is_canvas, local_id, target_raw): (PathBuf, bool, Option<String>, String) =
            if plain_include {
                let Some(info) = files.iter().find(|i| i.node_id == node_id) else {
                    self.status = "couldn't resolve this include's target file".into();
                    return;
                };
                match crate::worker_client::get_include_raw(port, &node_id).await {
                    Ok(raw) => (info.path.clone(), false, None, raw),
                    Err(e) => {
                        self.status = format!("failed to open source editor: {e}");
                        return;
                    }
                }
            } else {
                (
                    self.canvas_path.clone(),
                    true,
                    Some(node_id),
                    self.raw.clone(),
                )
            };

        // Cursor placement needs the *target file's own raw text* (to
        // convert a byte offset into a row/col) — read once here and
        // again inside `SourceEditorState::open`, rather than plumbing it
        // through, since that constructor is also the file-switcher's
        // own reload path and shouldn't need a cursor argument for that
        // case.
        let cursor = local_id
            .as_deref()
            .and_then(|id| {
                mdcanvas::node_body_offset(&target_raw, id)
                    .map(|off| source_editor::byte_offset_to_cursor(&target_raw, off))
            })
            .unwrap_or_default();

        let mut all_tags: Vec<String> = self
            .display_canvas
            .nodes
            .iter()
            .flat_map(|n| {
                n.tags
                    .iter()
                    .chain(n.extra_parents.iter().flat_map(|e| e.tags.iter()))
            })
            .cloned()
            .collect();
        all_tags.sort();
        all_tags.dedup();

        match SourceEditorState::open(
            self.canvas_path.clone(),
            path,
            is_canvas,
            cursor,
            files,
            all_tags,
            self.raw.clone(),
            target_raw,
        ) {
            Ok(state) => {
                self.source_editor = Some(state);
                if self.external_editor.is_some() {
                    self.request_external_edit(true);
                }
            }
            Err(e) => self.status = format!("failed to open source editor: {e}"),
        }
    }

    fn close_source_editor(&mut self) {
        self.source_editor = None;
        if let Some((pending, display)) = self.pending_external_change.take() {
            self.apply_external_reload(pending, display);
        }
    }

    /// Queues the open source editor's buffer for the external editor; the
    /// event loop (`mod.rs`) picks it up and hands over the terminal.
    fn request_external_edit(&mut self, close_on_cancel: bool) {
        let Some(template) = self.external_editor.clone() else {
            if let Some(se) = &mut self.source_editor {
                se.error = Some("no external editor — set `editor` under [tui] in config".into());
            }
            return;
        };
        let Some(se) = self.source_editor.as_ref().filter(|se| !se.read_only) else {
            return;
        };
        let (line, col) = se.cursor_line_col();
        self.pending_external_editor = Some(PendingExternalEditor {
            request: super::external_editor::Request {
                template,
                text: se.editor.lines.to_string(),
                name: se.file_stem(),
                line,
                col,
            },
            close_on_cancel,
        });
    }

    /// The external editor returned. An edit goes straight through the
    /// ordinary save (validation, merge or comparison with what changed on
    /// disk meanwhile); a failed save leaves the edited text in the
    /// built-in editor with the reason shown.
    pub async fn finish_external_edit(
        &mut self,
        outcome: super::external_editor::Outcome,
        close_on_cancel: bool,
    ) {
        use super::external_editor::Outcome;
        let message = match outcome {
            Outcome::Edited(text) => {
                if let Some(se) = &mut self.source_editor {
                    se.replace_text(&text);
                    se.error = None;
                }
                self.save_source_editor().await;
                return;
            }
            Outcome::Unchanged => "no changes".to_string(),
            Outcome::Aborted(msg) | Outcome::Failed(msg) => msg,
        };
        if close_on_cancel {
            self.close_source_editor();
            self.status = message;
        } else if let Some(se) = &mut self.source_editor {
            se.error = Some(message);
        }
    }

    /// The document's `include` nodes as the worker lists them. `path` is
    /// only a label for the editor's title and file picker (the target as
    /// written, next to the canvas) — nothing reads it from disk.
    async fn include_files(
        &self,
        port: u16,
    ) -> Result<Vec<meshfox_core::include::IncludeInfo>, String> {
        let base_dir = self.canvas_path.parent().unwrap_or(Path::new("."));
        Ok(crate::worker_client::get_includes(port)
            .await?
            .into_iter()
            .map(|e| meshfox_core::include::IncludeInfo {
                path: base_dir.join(&e.target),
                node_id: e.node_id,
                title: e.title,
                target: e.target,
            })
            .collect())
    }

    /// The file picker chose an include target: fetch its text from the
    /// worker and load it into the editor.
    async fn switch_source_editor_file(&mut self, path: PathBuf, is_canvas: bool) {
        let (Some(port), Some(se)) = (self.worker_port, self.source_editor.as_ref()) else {
            return;
        };
        let Some(node_id) = se
            .files
            .iter()
            .find(|i| i.path == path)
            .map(|i| i.node_id.clone())
        else {
            return;
        };
        let result = crate::worker_client::get_include_raw(port, &node_id).await;
        let Some(se) = self.source_editor.as_mut() else {
            return;
        };
        match result {
            Ok(raw) => se.finish_switch(path, is_canvas, raw),
            Err(e) => se.error = Some(format!("failed to read {}: {e}", path.display())),
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
    /// Canvas-valued include targets are read-only. Plain Markdown includes
    /// save through the parent worker.
    async fn save_source_editor(&mut self) {
        let Some(se) = &self.source_editor else {
            return;
        };
        if se.read_only {
            self.status = "included canvas is read-only; open it separately to edit".into();
            return;
        }
        let mut text = se.editor.lines.to_string();
        let is_canvas = se.is_canvas
            || meshfox_core::mdcanvas::is_canvas_path(&se.path)
            || meshfox_core::mdcanvas::has_marker(&text);
        let path = se.path.clone();
        // The include node whose target is being edited (`None`: the
        // primary document). The worker owns the write either way.
        let include_id = se
            .files
            .iter()
            .find(|i| i.path == path)
            .map(|i| i.node_id.clone());
        // The version of the file this editor read: a save is refused if the
        // file has changed since.
        let base_rev = se.base_rev();
        // Set when the worker refused the write because the file changed
        // and could not be combined with the buffer: the file as it is, its
        // revision, and the lines that overlapped — shown side by side.
        let mut conflict: Option<RefusedSave> = None;
        // Set when the worker combined the buffer with a change made
        // elsewhere instead of refusing.
        let mut merged = false;

        if is_canvas {
            if let Err(e) = Canvas::from_markdown(&text) {
                self.source_editor.as_mut().unwrap().error = Some(e.to_string());
                return;
            }
        }

        let is_primary = path == self.canvas_path;
        let include_id = if is_primary { None } else { include_id };
        let write_result = match self.worker_port {
            None => Err("no worker for this canvas".to_string()),
            Some(port) => {
                match crate::worker_client::put_canvas_raw(
                    port,
                    &text,
                    &base_rev,
                    include_id.as_deref(),
                    true,
                )
                .await
                {
                    Ok(outcome) if is_primary => {
                        merged = outcome.merged;
                        match crate::worker_client::get_canvas_raw(port).await {
                            Ok(saved) => {
                                text = saved;
                                Ok(())
                            }
                            Err(e) => Err(format!("saved, but failed to reload: {e}")),
                        }
                    }
                    Ok(outcome) => {
                        merged = outcome.merged;
                        Ok(())
                    }
                    Err(crate::worker_client::PutRawError::Conflict {
                        current_rev,
                        current_text,
                        conflicts,
                    }) => {
                        conflict = Some(RefusedSave {
                            rev: current_rev,
                            text: current_text,
                            conflicts,
                        });
                        Err(String::new())
                    }
                    Err(crate::worker_client::PutRawError::Other(e)) => Err(e),
                }
            }
        };
        if let Err(e) = write_result {
            let se = self.source_editor.as_mut().unwrap();
            match conflict {
                // Not an error to read in a footer: put both versions side
                // by side and let the user decide (see `source_diff.rs`).
                Some(refused) => {
                    se.error = None;
                    se.open_diff(refused.text, refused.rev, &refused.conflicts);
                }
                None => se.error = Some(e),
            }
            return;
        }

        if let Some(se) = self.source_editor.as_mut() {
            se.mark_saved();
        }
        if is_primary {
            self.raw = text.clone();
            if let Ok(reparsed) = Canvas::from_markdown(&text) {
                self.canvas = reparsed;
            }
            // Whatever arrived externally while the editor was open is now
            // stale — this save just overwrote it on disk with the
            // editor's own buffer, same "last write wins" behavior a save
            // conflicting with a concurrent external edit already has.
            self.pending_external_change = None;
        }
        let display = self.fetch_display_canvas().await;
        self.rebuild_display_canvas(display);
        self.rebuild_rows();
        self.render_current_document();
        self.status = if merged {
            "saved — merged with changes made elsewhere".into()
        } else {
            "saved".into()
        };
        self.source_editor = None;
    }

    // -- document rendering -------------------------------------------------

    fn render_current_document(&mut self) {
        self.doc_segments.clear();
        self.doc_click_regions.clear();
        self.doc_images.clear();
        let Some(row) = self.rows.get(self.selected) else {
            return;
        };
        let Some(node) = self.display_canvas.node(&row.node_id) else {
            return;
        };
        if self.selected_block.as_ref().is_some_and(|addr| {
            addr.node_id != node.id
                || !(addr
                    .block_name
                    .strip_prefix("@render:")
                    .and_then(|s| s.parse::<usize>().ok())
                    .is_some_and(|i| i < node.render_results.len())
                    || node
                        .runnable_blocks()
                        .iter()
                        .any(|b| b.name.as_deref() == Some(&addr.block_name)))
        }) {
            self.selected_block = None;
        }
        let block_views: HashMap<String, markdown::BlockView> = self
            .block_views
            .iter()
            .filter(|(addr, _)| addr.node_id == node.id)
            .map(|(addr, view)| (addr.block_name.clone(), view.clone()))
            .collect();
        let selected_block = self
            .selected_block
            .as_ref()
            .map(|addr| addr.block_name.as_str());
        let mut heading = Vec::new();
        if let Some(color) = ui::tree_row_color(node.color.as_deref()) {
            heading.push(Span::styled("● ", Style::default().fg(color)));
        }
        heading.push(Span::styled(
            node.title.clone(),
            Style::default().add_modifier(Modifier::BOLD),
        ));
        for tag in &node.tags {
            heading.push(Span::styled(
                format!("  #{tag}"),
                Style::default().fg(Color::Cyan),
            ));
        }
        // Keep the heading in the document's segment stream so it wraps
        // and scrolls with the body, including file-content previews.
        self.doc_segments
            .push(Segment::Text(vec![Line::from(heading), Line::from("")]));
        let asset_base = node.asset_base.clone();
        let base_dir = asset_base
            .as_deref()
            .map(Path::new)
            .unwrap_or_else(|| self.canvas_path.parent().unwrap_or_else(|| Path::new(".")))
            .to_path_buf();
        // Document-wide declared vars, purely so `markdown::render` can
        // label each fenced block's own implicit `from=` deps (see its own
        // doc comment) — `self.display_canvas` is already include-resolved
        // (see its own field doc comment), so this sees a `meshfox:var`
        // spliced in from an `include` too, same as `trigger_run`'s own
        // `declared_vars` call. Best-effort: a `vars.rs` error (e.g. a
        // duplicate declaration) just means no implicit deps are shown.
        let decls = meshfox_core::declared_vars(&self.display_canvas).unwrap_or_default();

        // Current display value for every `form`-field-targeted variable —
        // what's already been submitted this session, with whichever
        // field is actively being typed into right now (if any) overlaid
        // on top so the form reads as live while editing. Only overlaid
        // while `editing` is actually true: that's also the one window
        // `on_key`'s precedence chain makes it impossible to navigate away
        // from this exact node (arrow keys move the focused field instead
        // of the tree selection), so the overlay can never land on a
        // different node's same-named variable by accident — see
        // `InlineFormState::editing`'s own doc comment.
        let mut form_values: HashMap<String, String> = self.session_vars.clone();
        // Which row of which form fence is actually focused right now, if
        // any — only while `active_inline_form` is both open *and*
        // `editing` (see that field's own doc comment for why arrow keys
        // can't have navigated to a different node in the meantime), so
        // `markdown::render` can draw a visible focus indicator on it (see
        // `render`'s own `form_focus` parameter doc comment).
        let mut form_focus: Option<(&str, usize)> = None;
        if let Some(form) = &self.active_inline_form {
            if form.editing {
                for (field, value) in form.fields.iter().zip(form.inputs.iter()) {
                    form_values.insert(field.var.clone(), value.clone());
                }
                form_focus = Some((form.block_name.as_str(), form.selected));
            }
        }

        // This node's own blocks' live output from the most recent run, if
        // any — see `StepOutput`'s own doc comment. Filtered down to just
        // this node up front so `markdown::render` (called from more than
        // one branch below) can do a plain by-name lookup instead of every
        // caller re-filtering the same global map.
        let live_output: HashMap<String, StepOutput> = self
            .step_output
            .iter()
            .filter(|(addr, _)| addr.node_id == node.id)
            .filter(|(addr, _)| {
                self.displayed_invocations
                    .get(&definition_addr(addr))
                    .is_none_or(|displayed| displayed == *addr)
            })
            .map(|(addr, so)| (definition_addr(addr).block_name, so.clone()))
            .collect();

        // `file` nodes with `display="code"` (see SPEC.md) show the
        // target's own file content — fetched from the worker, which reads
        // it fresh off disk confined to the canvas directory, same as the
        // browser's read-only preview — rather than the node's own body
        // (which for a `file` node is just the one Markdown link). The
        // fetch is in the background (`ensure_file_preview`); until it
        // lands the pane says so, and a refresh keeps the old content up.
        if node.node_type == NodeType::File
            && matches!(node.display, Some(FileDisplay::Code | FileDisplay::Table))
        {
            let is_table = node.display == Some(FileDisplay::Table);
            if let Some(target) = node.target.clone() {
                let path = base_dir.join(&target);
                let node_id = node.id.clone();
                let lang = node.lang.clone();
                let caption = node.caption.clone();
                let preview = if is_table {
                    self.ensure_table(&node_id);
                    self.tables
                        .get(&node_id)
                        .map(|data| vec![Segment::TablePreview(Box::new(data.clone()))])
                        .unwrap_or_default()
                } else {
                    self.ensure_file_preview(&node_id);
                    match self.file_previews.get(&node_id).map(|p| &p.state) {
                        Some(FilePreviewState::Ready { content, truncated }) => {
                            let mut lines =
                                self.highlighter
                                    .highlight_file(lang.as_deref(), &path, content);
                            if *truncated {
                                lines.push(Line::from(Span::styled(
                                    "preview truncated to the first part of the file",
                                    Style::default().fg(Color::DarkGray),
                                )));
                            }
                            vec![Segment::Text(lines)]
                        }
                        Some(FilePreviewState::Failed(e)) => {
                            vec![Segment::Text(vec![Line::from(Span::styled(
                                format!("failed to read {}: {e}", path.display()),
                                Style::default().fg(Color::Red),
                            ))])]
                        }
                        _ => vec![Segment::Text(vec![Line::from(Span::styled(
                            "loading preview…",
                            Style::default().fg(Color::DarkGray),
                        ))])],
                    }
                };
                // The preview replaces `node.text` entirely (it's the
                // target's own content, not the node's body) — but an
                // optional caption (see `Node::caption`) is still part of
                // that body and still worth showing. Above the preview, not
                // below (unlike the plain-link display mode, which gets its
                // caption for free below the link by rendering `node.text`
                // whole) — it reads as a heading/intro for the content, not
                // a footnote on it.
                if let Some(caption) = &caption {
                    let (segs, regions) = markdown::render(
                        caption,
                        &base_dir,
                        &self.highlighter,
                        &node_id,
                        &decls,
                        &form_values,
                        None,
                        &live_output,
                    );
                    // `regions` came back indexed into `segs` alone —
                    // offset by how many segments already precede it (none,
                    // here, but kept explicit rather than assumed) so they
                    // still point at the right entry once folded into the
                    // shared `doc_segments`/`doc_click_regions`.
                    let offset = self.doc_segments.len();
                    self.doc_click_regions
                        .extend(regions.into_iter().map(|mut r| {
                            r.segment_index += offset;
                            r
                        }));
                    self.doc_segments.extend(segs);
                    self.doc_segments.push(Segment::Text(vec![Line::from("")]));
                }
                self.doc_segments.extend(preview);
                self.prepare_document_images(&base_dir, asset_base);
                return;
            }
        }

        let (segs, regions) = markdown::render_with_computed(
            &node.text,
            &base_dir,
            &self.highlighter,
            &node.id,
            &decls,
            &form_values,
            form_focus,
            &live_output,
            &block_views,
            selected_block,
            self.focus == Focus::Document,
            true,
            &node.render_results,
        );
        let offset = self.doc_segments.len();
        self.doc_segments.extend(segs);
        self.doc_click_regions = regions
            .into_iter()
            .map(|mut r| {
                r.segment_index += offset;
                r
            })
            .collect();

        let preview_target = (node.node_type == NodeType::Link && node.preview)
            .then(|| node.target.clone())
            .flatten();
        self.prepare_document_images(&base_dir, asset_base);

        if let Some(target) = preview_target {
            self.append_link_preview(&target);
        }
    }

    fn prepare_document_images(&mut self, base_dir: &Path, asset_base: Option<String>) {
        let images: Vec<(PathBuf, Option<u32>, Option<u32>, Option<Background>)> = self
            .doc_segments
            .iter()
            .filter_map(|s| match s {
                Segment::Image {
                    path,
                    width_percent,
                    height_percent,
                    bg,
                    ..
                } => Some((path.clone(), *width_percent, *height_percent, *bg)),
                Segment::Text(_)
                | Segment::Button { .. }
                | Segment::Table { .. }
                | Segment::TablePreview(_) => None,
            })
            .collect();
        for (path, width_percent, height_percent, bg) in images {
            if path
                .to_str()
                .is_some_and(|p| p.starts_with("meshfox-link-preview:"))
            {
                continue;
            }
            let is_data_url = path.to_str().is_some_and(|s| s.starts_with("data:"));
            if !is_data_url && !self.image_bytes.contains_key(&path) {
                request_image(
                    self.worker_port,
                    self.image_generation,
                    asset_base.clone(),
                    &self.background_tx,
                    &mut self.image_requested,
                    &mut self.image_pending,
                    base_dir,
                    &path,
                );
                self.doc_images.insert(path, None);
                continue;
            }
            // The document title and its following blank row occupy two rows.
            let budget = super::document_layout::LayoutContext::new(self.document_viewport)
                .inset(0, 2)
                .image_budget(width_percent, height_percent);
            // Rasterizing (SVG parse + text layout, Lanczos resize, protocol
            // encoding) is far too slow to repeat on every selection change,
            // so reuse the result while nothing it depends on has changed.
            let bytes = self.image_bytes.get(&path).map(Vec::as_slice);
            let font = self.picker.font_size();
            let key = DocImageKey {
                content: bytes.map(|b| {
                    let mut hasher = std::collections::hash_map::DefaultHasher::new();
                    std::hash::Hash::hash(b, &mut hasher);
                    std::hash::Hasher::finish(&hasher)
                }),
                budget,
                bg,
                font: (font.width, font.height),
                protocol: self.picker.protocol_type(),
            };
            let protocol = match self.doc_image_cache.get(&path) {
                Some((cached, image)) if *cached == key => image.clone(),
                _ => {
                    let image = load_document_image(&self.picker, &path, bytes, bg, budget);
                    if self.doc_image_cache.len() >= DOC_IMAGE_CACHE_ENTRIES {
                        self.doc_image_cache.clear();
                    }
                    self.doc_image_cache
                        .insert(path.clone(), (key, image.clone()));
                    image
                }
            };
            self.doc_images.insert(path, protocol);
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
                    bg: None,
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
    /// (if any — a failure sends nothing) arrives via `background_tx`,
    /// handled by `on_background_msg`.
    fn maybe_fetch_link_preview(&mut self, url: String) {
        if !self.link_preview_requested.insert(url.clone()) {
            return;
        }
        let cache = Arc::clone(&self.link_preview_cache);
        let tx = self.background_tx.clone();
        tokio::spawn(async move {
            if let Some(meta) = cache.get_or_fetch(&url).await {
                let _ = tx.send(BackgroundMsg::Meta { url, meta });
            }
        });
    }

    /// Fixed image protocols refuse to draw into a smaller rectangle. Rebuild
    /// from cached bytes when the document pane changes size, without refetching.
    pub(super) fn set_document_viewport(&mut self, width: u16, height: u16) {
        let viewport = ratatui::layout::Size::new(width.max(1), height.max(1));
        if viewport != self.document_viewport {
            self.document_viewport = viewport;
            let node = self
                .rows
                .get(self.selected)
                .and_then(|r| self.display_canvas.node(&r.node_id));
            let asset_base = node.and_then(|n| n.asset_base.clone());
            let base_dir = asset_base
                .as_deref()
                .map(Path::new)
                .unwrap_or_else(|| self.canvas_path.parent().unwrap_or(Path::new(".")))
                .to_path_buf();
            self.prepare_document_images(&base_dir, asset_base);
        }
    }

    pub(super) fn invalidate_images(&mut self) {
        self.image_generation += 1;
        self.image_bytes.clear();
        self.image_requested.clear();
        self.image_pending.clear();
        self.render_current_document();
    }

    /// Same idea as `maybe_fetch_link_preview`, but for a loaded preview's
    /// own `og:image` bytes — a second, independent SSRF-safe fetch (see
    /// `link_preview::fetch_image_bytes`), decoded here in the background
    /// task (cheap enough for one small preview image) and handed back as
    /// a plain `image::DynamicImage`; building the actual `Protocol` still
    /// has to happen on the main thread (`on_background_msg`), since
    /// that needs `&mut self.picker`.
    fn maybe_fetch_link_preview_image(&mut self, url: String) {
        if !self.link_preview_image_requested.insert(url.clone()) {
            return;
        }
        let tx = self.background_tx.clone();
        tokio::spawn(async move {
            let Ok(bytes) = link_preview::fetch_image_bytes(&url).await else {
                return;
            };
            let Ok(image) = image::load_from_memory(&bytes) else {
                return;
            };
            let _ = tx.send(BackgroundMsg::Image { url, image });
        });
    }

    /// Applies a background link-preview fetch's result (see
    /// `BackgroundMsg`) — called from `mod.rs`'s `main_loop`. Just updates
    /// the cache and re-renders the currently selected node's document
    /// pane; if the fetch that just landed belongs to some other node than
    /// whatever's selected now, this is a harmless no-op-ish redraw rather
    /// than something that needs special-casing away.
    pub fn on_background_msg(&mut self, msg: BackgroundMsg) {
        match msg {
            BackgroundMsg::WorkerState { services, runs } => {
                self.worker_refresh_inflight = false;
                if let Some(list) = services {
                    self.apply_services(list);
                }
                if let Some(runs) = runs {
                    self.block_runs = Some(runs);
                    self.sync_tty_results();
                }
            }
            BackgroundMsg::FileContent { node_id, result } => {
                if let Some(entry) = self.file_previews.get_mut(&node_id) {
                    entry.inflight = false;
                    entry.fetched = Some(std::time::Instant::now());
                    entry.state = match result {
                        Ok((content, truncated)) => FilePreviewState::Ready { content, truncated },
                        Err(e) => FilePreviewState::Failed(e),
                    };
                }
            }
            BackgroundMsg::TableMeta { node_id, result } => {
                let data = self.tables.entry(node_id.clone()).or_default();
                data.meta_inflight = false;
                data.meta_fetched = Some(std::time::Instant::now());
                match result {
                    Ok(meta) => data.apply_meta(meta),
                    Err(e) => data.meta_error = Some(e),
                }
                self.schedule_table_rows(&node_id);
            }
            BackgroundMsg::TableRows {
                node_id,
                generation,
                block,
                result,
            } => {
                if let Some(data) = self.tables.get_mut(&node_id) {
                    match result {
                        Ok(page) => {
                            data.apply_rows(generation, block, page);
                        }
                        Err(e) => data.fail_rows(generation, block, e),
                    }
                }
                self.schedule_table_rows(&node_id);
            }
            BackgroundMsg::Meta { url, meta } => {
                self.link_preview_meta.insert(url, meta);
            }
            BackgroundMsg::FormFields {
                node_id,
                block,
                result,
            } => {
                if let (Ok(statuses), Some(form)) = (result, self.active_inline_form.as_mut()) {
                    if form.node_id == node_id && form.block_name == block {
                        for (decl, input) in form.decls.iter().zip(form.inputs.iter_mut()) {
                            // Only fields still showing the untouched default.
                            let untouched = !self.session_vars.contains_key(&decl.name)
                                && *input == coerce_field_input(decl, decl.default.as_deref());
                            let resolved = statuses.iter().find(|s| s.var.name == decl.name);
                            if let (true, Some(status)) = (untouched, resolved) {
                                *input = coerce_field_input(decl, status.var.value.as_deref());
                            }
                        }
                    }
                }
            }
            BackgroundMsg::ImageBytes {
                generation,
                path,
                result,
            } => {
                if generation != self.image_generation {
                    return;
                }
                self.image_pending.remove(&path);
                if let Ok(bytes) = result {
                    self.image_bytes.insert(path, bytes);
                }
            }
            BackgroundMsg::Image { url, image } => {
                let budget = ratatui::layout::Size::new(56, 24);
                if let Some(protocol) = prepared_image(&self.picker, image, budget) {
                    self.link_preview_image.insert(url, Arc::new(protocol));
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
        if self.focus == Focus::Document {
            if let Some(addr) = self.selected_block.clone() {
                if self.is_form_block(&addr) {
                    self.status =
                        "a form isn't run — Enter edits it, its Send button applies it".into();
                    return;
                }
                self.start_run(addr.node_id, addr.block_name, with_deps)
                    .await;
                return;
            }
        }

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

        // Include-resolved (not just `Canvas::from_markdown`) so a block
        // living inside an `include` node's own dumped body is found here
        // too — same `self.display_canvas` the `file`-node check above
        // already trusts.
        let Some(node_text) = self.display_canvas.node(&node_id).map(|n| n.text.clone()) else {
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
                    is_button: meshfox_core::is_button(&b.lang),
                }
            })
            .collect();
        self.block_picker = Some(BlockPickerState {
            node_id,
            blocks: choices,
            selected: 0,
            with_deps,
            history: false,
        });
    }

    /// Node-id path from the root's own children down to (and including)
    /// `node_id` — what `RunRequest`/`VarsQuery`/etc. address a block by
    /// (see their own field doc comments server-side), unlike the flat
    /// `node_id` the rest of this file already works with (`BlockAddr`,
    /// `deps::resolve_chain`, ...). Empty for a root-owned block, matching
    /// that same convention. Walks `self.display_canvas`'s own `parent`
    /// chain, same as `App::new`'s deep-link ancestor expansion already
    /// does.
    fn path_to(&self, node_id: &str) -> Vec<String> {
        if self
            .display_canvas
            .node(node_id)
            .and_then(|n| n.parent.clone())
            .is_none()
        {
            return Vec::new();
        }
        let mut chain = vec![node_id.to_string()];
        let mut current = node_id.to_string();
        while let Some(parent) = self
            .display_canvas
            .node(&current)
            .and_then(|n| n.parent.clone())
        {
            if self
                .display_canvas
                .node(&parent)
                .and_then(|n| n.parent.clone())
                .is_none()
            {
                break;
            }
            chain.push(parent.clone());
            current = parent;
        }
        chain.reverse();
        chain
    }

    /// Whether resolving `node_id`/`block_name`'s own chain (`deps=` if
    /// `with_deps`, `from=` only otherwise — same choice `start_run_local`
    /// makes) touches a `tty` block anywhere in it, not just at the target
    /// itself — a `tty` block may be an explicit or implicit dependency of
    /// any other block (`meshfox_core::deps`'s own module doc comment), so
    /// this can't just check the target block alone. Mirrors
    /// `crates/server/src/lib.rs`'s own `find_tty_block` exactly (same
    /// "resolved canvas, scan each address's own node text" approach), just
    /// against `display_canvas` locally instead of a request's own
    /// resolved snapshot — decides whether `start_run_via_worker` routes
    /// through `begin_http_run` (`POST /api/run`) or `begin_http_tty_run`
    /// (`GET /api/run/tty`, the only one of the two the server actually
    /// accepts a tty-touching chain on at all). Returns the *last* tty
    /// block's own `autoclose` flag found while walking the chain (`None`
    /// if none are `tty` at all) — a chain can touch more than one `tty`
    /// step, and the last one is the one whose own flag actually governs
    /// whether the handoff pauses at the end (see `PendingHttpTty::autoclose`).
    fn target_chain_tty_autoclose(
        &self,
        node_id: &str,
        block_name: &str,
        with_deps: bool,
    ) -> Option<bool> {
        let target = BlockAddr::new(node_id.to_string(), block_name.to_string());
        let chain_result = if with_deps {
            meshfox_core::deps::resolve_chain(&self.display_canvas, target)
        } else {
            meshfox_core::deps::resolve_from_chain(&self.display_canvas, target)
        };
        let Ok(chain) = chain_result else { return None };
        let mut autoclose = None;
        for addr in &chain {
            let Ok(block) = meshfox_core::deps::find_block(&self.display_canvas, addr) else {
                continue;
            };
            if !block.tty {
                continue;
            }
            autoclose = Some(block.autoclose);
        }
        autoclose
    }

    /// Length of `node_id`/`block_name`'s own resolved chain (`deps=` if
    /// `with_deps`, `from=` only otherwise) — same resolution
    /// `target_chain_tty_autoclose` does, just counting steps instead of
    /// scanning for `tty`. Used to decide whether starting this run should
    /// auto-open the console: a single-block run doesn't need it, only a
    /// multi-block chain does (see `begin_http_run`'s call site). Worker
    /// mode doesn't know its own chain length up front the way local mode's
    /// `chain: Vec<BlockAddr>` does (it's built incrementally from
    /// `StepStart` events as they arrive), so this resolves it locally
    /// against `display_canvas` purely to make that same decision early. `0`
    /// if resolution fails for any reason — treated as "don't auto-open"
    /// rather than guessing.
    fn resolved_chain_len(&self, node_id: &str, block_name: &str, with_deps: bool) -> usize {
        let target = BlockAddr::new(node_id.to_string(), block_name.to_string());
        let chain_result = if with_deps {
            meshfox_core::deps::resolve_chain(&self.display_canvas, target)
        } else {
            meshfox_core::deps::resolve_from_chain(&self.display_canvas, target)
        };
        chain_result.map(|chain| chain.len()).unwrap_or(0)
    }

    /// The worker-routed path `start_run` takes when a worker is reachable
    /// — pre-checks `GET /api/vars` (replacing `park_on_unresolved`'s lazy,
    /// per-step discovery with a single up-front check across the whole
    /// chain, the same gate the web UI's own `handleRun` already runs
    /// before `executeRun`), opening the same `VarFormState` modal on
    /// anything still unresolved, then starts the run for real — through
    /// `begin_http_tty_run` when `target_chain_tty_autoclose` finds a
    /// `tty` block anywhere in the chain (the server's plain `/api/run`
    /// rejects one outright, see its own `find_tty_block` check), through
    /// `begin_http_run` otherwise. `force`, when given, names the address a
    /// prior `LockConflict`/`TtyConnectError::Conflict` reported — see
    /// `on_service_conflict_key`'s worker-routed branch — and skips the
    /// vars check entirely (already done on the first attempt); passed
    /// straight through to whichever of the two this chain needs.
    async fn start_run_via_worker(
        &mut self,
        node_id: String,
        block_name: String,
        with_deps: bool,
        port: u16,
        force: Option<(String, String)>,
        extra_vars: HashMap<String, String>,
    ) {
        let mut node_id = node_id;
        let mut block_name = block_name;
        let mut path = self.path_to(&node_id);
        if force.is_none() {
            match crate::worker_client::prepare_arguments(
                port,
                &path,
                &block_name,
                &Default::default(),
                !with_deps,
            )
            .await
            {
                Ok(prepared) => {
                    node_id = prepared.node_id;
                    path = prepared.path;
                    if !block_name.contains('[') {
                        block_name = prepared.definition;
                    }
                    if !prepared.fields.is_empty()
                        && (!block_name.contains('[') || prepared.block.is_none())
                    {
                        self.pending_http_run = Some(PendingHttpRun {
                            arguments: true,
                            node_id,
                            block_name,
                            with_deps,
                            port,
                            force,
                            is_tty: false,
                        });
                        self.var_form = Some(argument_form_from_statuses(prepared.fields));
                        return;
                    }
                    if let Some(name) = prepared.block {
                        block_name = name;
                    }
                }
                Err(e) => {
                    self.status = format!("meshfox: failed to check arguments: {e}");
                    return;
                }
            }
        }
        let is_tty = self
            .target_chain_tty_autoclose(&node_id, &block_name, with_deps)
            .is_some();
        if force.is_none() {
            match crate::worker_client::get_vars(port, &path, &block_name, !with_deps).await {
                Ok(statuses) => {
                    let missing: Vec<_> = statuses.into_iter().filter(|v| !v.resolved).collect();
                    if !missing.is_empty() {
                        self.pending_http_run = Some(PendingHttpRun {
                            arguments: false,
                            node_id,
                            block_name,
                            with_deps,
                            port,
                            force,
                            is_tty,
                        });
                        self.var_form = Some(var_form_from_statuses(missing));
                        return;
                    }
                }
                Err(e) => {
                    self.status = format!("meshfox: failed to check variables: {e}");
                    return;
                }
            }
        }
        if is_tty {
            self.begin_http_tty_run(
                node_id,
                block_name,
                with_deps,
                port,
                extra_vars,
                std::collections::HashSet::new(),
                force,
            )
            .await;
        } else {
            self.begin_http_run(
                node_id,
                block_name,
                with_deps,
                port,
                extra_vars,
                std::collections::HashSet::new(),
                force,
            )
            .await;
        }
    }

    /// Actually starts a worker-routed interactive run (`GET
    /// /api/run/tty`) — connects the socket (an ordinary async call, no
    /// terminal access needed yet) and parks it as `pending_http_tty` for
    /// `mod.rs`'s event loop to actually hand the terminal to on its next
    /// iteration (see `PendingHttpTty`'s own doc comment) — the
    /// worker-routed counterpart to `advance_run`'s local `pending_tty`
    /// branch. A `409` reuses the same `ServiceConflictState` modal
    /// `begin_http_run`'s own conflict branch does, just with
    /// `HttpRunRetry::is_tty` set so `on_service_conflict_key` retries the
    /// right way (see `TtyConnectError::Conflict`'s own doc comment).
    /// `force`, when given, names the address a prior conflict on this same
    /// chain reported — mirrors `begin_http_run`'s own `force` parameter.
    #[allow(clippy::too_many_arguments)]
    async fn begin_http_tty_run(
        &mut self,
        node_id: String,
        block_name: String,
        with_deps: bool,
        port: u16,
        vars: HashMap<String, String>,
        save_secrets: std::collections::HashSet<String>,
        force: Option<(String, String)>,
    ) {
        let approved = std::mem::take(&mut self.run_confirmation_approved);
        if !approved {
            let path = self.path_to(&node_id);
            match crate::worker_client::confirmation_blocks(port, &path, &block_name, !with_deps)
                .await
            {
                Ok(blocks) if !blocks.is_empty() => {
                    self.run_confirmation = Some(RunConfirmation {
                        blocks,
                        restart: false,
                        node_id,
                        block_name,
                        with_deps,
                        port,
                        vars,
                        save_secrets,
                        force,
                        tty: true,
                    });
                    return;
                }
                Ok(_) => {}
                Err(error) => {
                    self.status = format!("confirmation check failed: {error}");
                    return;
                }
            }
        }
        let path = self.path_to(&node_id);
        let autoclose = self
            .target_chain_tty_autoclose(&node_id, &block_name, with_deps)
            .unwrap_or(false);
        let (cols, rows) = crossterm::terminal::size().unwrap_or((80, 24));
        match crate::worker_client::tty_connect(
            port,
            &path,
            &block_name,
            !with_deps,
            false,
            false,
            vars,
            save_secrets,
            cols,
            rows,
            force,
            approved,
        )
        .await
        {
            Ok(socket) => {
                self.status.clear();
                self.pending_http_tty = Some(PendingHttpTty {
                    socket,
                    block_name,
                    autoclose,
                });
            }
            Err(crate::worker_client::TtyConnectError::Conflict(conflict)) => {
                self.status = format!(
                    "meshfox: {:?}/{:?} is locked by pid {} ({}) — y to kill and retry, n to cancel",
                    conflict.node_id, conflict.block, conflict.owner_pid, conflict.owner_desc
                );
                self.service_conflict = Some(ServiceConflictState {
                    block_name: conflict.block.clone(),
                    owner_pid: conflict.owner_pid,
                    owner_desc: conflict.owner_desc.clone(),
                    http_retry: Some(HttpRunRetry {
                        node_id,
                        block_name,
                        with_deps,
                        port,
                        force_node_id: conflict.node_id.clone(),
                        force_block: conflict.block.clone(),
                        is_tty: true,
                    }),
                });
            }
            Err(e) => {
                self.status = format!("meshfox: failed to start interactive session: {e}");
            }
        }
    }

    /// Actually starts a worker-routed run (`POST /api/run`/`force`) and
    /// wires its streamed `RunEvent`s into a fresh `RunState` the same
    /// rendering code (`ui.rs`/`markdown.rs`'s live-output splice) already
    /// reads regardless of which mode populated it — see
    /// `App::on_run_event` for the folding itself.
    #[allow(clippy::too_many_arguments)]
    async fn begin_http_run(
        &mut self,
        node_id: String,
        block_name: String,
        with_deps: bool,
        port: u16,
        vars: HashMap<String, String>,
        save_secrets: std::collections::HashSet<String>,
        force: Option<(String, String)>,
    ) {
        let approved = std::mem::take(&mut self.run_confirmation_approved);
        if !approved {
            let path = self.path_to(&node_id);
            match crate::worker_client::confirmation_blocks(port, &path, &block_name, !with_deps)
                .await
            {
                Ok(blocks) if !blocks.is_empty() => {
                    self.run_confirmation = Some(RunConfirmation {
                        blocks,
                        restart: false,
                        node_id,
                        block_name,
                        with_deps,
                        port,
                        vars,
                        save_secrets,
                        force,
                        tty: false,
                    });
                    return;
                }
                Ok(_) => {}
                Err(error) => {
                    self.status = format!("confirmation check failed: {error}");
                    return;
                }
            }
        }
        let path = self.path_to(&node_id);
        self.output_scroll = 0;
        self.output_hscroll = 0;
        self.status.clear();
        // Only worth auto-opening the console for a real chain — a single
        // block's own output isn't worth losing screen space over. Worker
        // mode doesn't know its own chain length yet at this point (it's
        // built incrementally from `StepStart` events), so resolve it
        // locally just for this decision — see `resolved_chain_len`'s own
        // doc comment.
        if self.resolved_chain_len(&node_id, &block_name, with_deps) >= 2 {
            self.console_collapsed = false;
            self.console_last_activity = Some(std::time::Instant::now());
        }
        use crate::worker_client::RunEvent;
        match crate::worker_client::run_stream(
            port,
            &path,
            &block_name,
            !with_deps,
            vars,
            save_secrets,
            force,
            approved,
        )
        .await
        {
            Ok(mut http_rx) => {
                // A lock conflict is no longer a connect-time error (a
                // browser `WebSocket` can't read a rejected-handshake
                // status/body, so the server always completes the upgrade
                // and reports it as the very first streamed event instead
                // — see `worker_client::run_stream`'s own doc comment) —
                // peek at it here, before a `RunState` even exists, same
                // as the old connect-time `Err` branch used to.
                match http_rx.recv().await {
                    Some(RunEvent::LockConflict {
                        node_id: conflict_node,
                        block: conflict_block,
                        owner_pid,
                        owner_desc,
                    }) => {
                        self.status = format!(
                            "meshfox: {conflict_node:?}/{conflict_block:?} is locked by pid {owner_pid} ({owner_desc}) — y to kill and retry, n to cancel"
                        );
                        self.service_conflict = Some(ServiceConflictState {
                            block_name: conflict_block.clone(),
                            owner_pid,
                            owner_desc,
                            http_retry: Some(HttpRunRetry {
                                node_id,
                                block_name,
                                with_deps,
                                port,
                                force_node_id: conflict_node,
                                force_block: conflict_block,
                                is_tty: false,
                            }),
                        });
                    }
                    first => {
                        self.run = Some(RunState {
                            chain: Vec::new(),
                            idx: 0,
                            http_rx: Some(http_rx),
                            lines: Vec::new(),
                            full_output: String::new(),
                            stdout_only: String::new(),
                            stderr_only: String::new(),
                            output_markdown: false,
                            step_started: std::time::Instant::now(),
                            had_failure: false,
                            killed: false,
                            finished: false,
                        });
                        self.on_run_event(first).await;
                    }
                }
            }
            Err(e) => {
                self.status = format!("meshfox: failed to start run: {e}");
            }
        }
    }

    /// Folds one streamed `RunEvent` (`worker_client::run_stream`) into the
    /// current worker-routed `RunState` — the worker-mode counterpart to
    /// `on_output_line`/`advance_run`'s own local-mode step-by-step
    /// updates. Populates the exact same fields those do, so
    /// `ui.rs`/`markdown.rs`'s rendering never needs to know which mode
    /// produced them.
    pub async fn on_run_event(&mut self, event: Option<crate::worker_client::RunEvent>) {
        use crate::worker_client::RunEvent;
        self.console_last_activity = Some(std::time::Instant::now());
        if matches!(
            event,
            Some(RunEvent::Done { .. } | RunEvent::Killed { .. } | RunEvent::Error { .. })
        ) {
            self.refresh_runs_soon = true;
        }
        let Some(run) = &mut self.run else { return };
        let Some(event) = event else {
            // The channel closed without a terminal event (the worker died
            // mid-stream, say) — same "can't tell what happened, stop
            // waiting" posture a local `proc`'s output channel closing
            // unexpectedly would already need.
            run.http_rx = None;
            run.finished = true;
            self.status = "meshfox: lost the worker's run stream".into();
            return;
        };
        // Which address (if any) this event's own live-output splice
        // touched — `on_external_run_event`'s own passive-watch path
        // already refreshes `render_current_document`'s cached
        // `doc_segments` on every line so a run it's only watching shows
        // its output growing inline under its own block; a run *this*
        // session started itself used to never do that at all (only the
        // Output console ever saw it) — checked once, after the match
        // below, so every arm that touches `step_output` shares the same
        // "only redraw if this address's node is the one on screen"
        // gate instead of repeating it per arm.
        let mut touched: Option<BlockAddr> = None;
        match event {
            RunEvent::Started { .. } => {}
            RunEvent::StepStart { node_id, block } => {
                let addr = BlockAddr::new(node_id, block);
                self.child_steps.remove(&addr);
                // The passive watch stream can deliver a line before this
                // stream announces the step. Transfer ownership of its badge
                // along with its output to the foreground run.
                self.external_running.remove(&addr);
                self.displayed_invocations
                    .insert(definition_addr(&addr), addr.clone());
                run.chain.push(addr.clone());
                run.idx += 1;
                run.stdout_only.clear();
                run.stderr_only.clear();
                run.step_started = std::time::Instant::now();
                // Resolved fresh from `display_canvas` (never sent on the
                // wire) the same way `on_external_run_event` already
                // resolves it for a passively-watched run — previously
                // left at `RunState`'s own default (`false`) for every
                // worker-routed run, since nothing here ever set it.
                run.output_markdown = self
                    .display_canvas
                    .node(&addr.node_id)
                    .map(|n| n.runnable_blocks())
                    .unwrap_or_default()
                    .into_iter()
                    .find(|b| b.name.as_deref() == Some(definition_addr(&addr).block_name.as_str()))
                    .is_some_and(|b| b.attrs.get("output").map(String::as_str) == Some("markdown"));
                // A fresh, in-flight entry right away — mirrors
                // `on_external_run_event`'s own `Line` handling, so a
                // self-triggered run's live stdout shows up inline under
                // its own block in the Document pane the same way a
                // passively-watched one already does (and the same way
                // the web UI's own live view does), not just in the
                // Output console.
                self.step_output.insert(
                    addr.clone(),
                    StepOutput {
                        text: String::new(),
                        stdout: String::new(),
                        stderr: String::new(),
                        output_markdown: run.output_markdown,
                        exit_code: 0,
                        duration_ms: 0,
                        running: true,
                    },
                );
                touched = Some(addr);
            }
            RunEvent::StepSkipped { node_id, block, .. } => {
                run.lines
                    .push(format!("==> {block} (skipped, already fresh this session)"));
                let addr = BlockAddr::new(node_id, block);
                // A skipped step produced no live output for this run.
                self.step_output.remove(&addr);
                touched = Some(addr);
            }
            RunEvent::Output {
                node_id,
                block,
                stream,
                text,
                child,
            } => {
                if let Some(label) = child
                    .as_ref()
                    .and_then(crate::worker_client::child_step_label)
                {
                    self.child_steps
                        .insert(BlockAddr::new(&node_id, &block), label);
                }
                run.lines.push(text.clone());
                run.full_output.push_str(&text);
                run.full_output.push('\n');
                match stream {
                    meshfox_server::stream_exec::OutputStream::Stdout => {
                        run.stdout_only.push_str(&text);
                        run.stdout_only.push('\n');
                    }
                    meshfox_server::stream_exec::OutputStream::Stderr => {
                        run.stderr_only.push_str(&text);
                        run.stderr_only.push('\n');
                    }
                }
                let addr = BlockAddr::new(node_id, block);
                let entry = self
                    .step_output
                    .entry(addr.clone())
                    .or_insert_with(|| StepOutput {
                        text: String::new(),
                        stdout: String::new(),
                        stderr: String::new(),
                        output_markdown: run.output_markdown,
                        exit_code: 0,
                        duration_ms: 0,
                        running: true,
                    });
                let dest = match stream {
                    meshfox_server::stream_exec::OutputStream::Stdout => &mut entry.stdout,
                    meshfox_server::stream_exec::OutputStream::Stderr => &mut entry.stderr,
                };
                entry.text.push_str(&text);
                entry.text.push('\n');
                dest.push_str(&text);
                dest.push('\n');
                touched = Some(addr);
            }
            RunEvent::TtyStart { .. } => {}
            RunEvent::ServiceStarted {
                node_id: _,
                block,
                pid,
            } => {
                run.lines
                    .push(format!("==> {block} (service started, pid {pid})"));
            }
            RunEvent::StepEnd {
                node_id,
                block,
                exit_code,
                duration_ms,
            } => {
                run.lines.push(format!(
                    "(exit {exit_code} · {})",
                    meshfox_core::format_duration_ms(duration_ms)
                ));
                let addr = BlockAddr::new(node_id, block);
                self.step_output.insert(
                    addr.clone(),
                    StepOutput {
                        text: self
                            .step_output
                            .get(&addr)
                            .map(|entry| entry.text.clone())
                            .unwrap_or_default(),
                        stdout: run.stdout_only.clone(),
                        stderr: run.stderr_only.clone(),
                        output_markdown: run.output_markdown,
                        exit_code,
                        duration_ms,
                        running: false,
                    },
                );
                if exit_code != 0 {
                    run.had_failure = true;
                }
                touched = Some(addr);
            }
            RunEvent::LockConflict {
                node_id,
                block,
                owner_pid,
                owner_desc,
            } => {
                // Only ever arrives as the very first event, handled
                // directly by `begin_http_run` before a `RunState` even
                // exists — reaching here is defensive-only (shouldn't
                // happen). Same terminal-failure shape as `RunEvent::Error`.
                run.had_failure = true;
                run.finished = true;
                run.http_rx = None;
                self.status = format!(
                    "meshfox: {node_id:?}/{block:?} is locked by pid {owner_pid} ({owner_desc})"
                );
            }
            RunEvent::Killed { node_id, block } => {
                run.killed = true;
                run.finished = true;
                run.http_rx = None;
                self.status = "run killed".into();
                // Local mode gets a `step_output` entry for free once a
                // killed process's exit status actually resolves
                // (`on_output_line`'s own `None` branch always inserts
                // one, non-zero or not) — the server never sends a
                // `StepEnd` for a killed step (see `run_block_impl`'s own
                // "killed short-circuits before StepEnd" shape), so this is worker mode's own
                // equivalent, needed for `ui::render_tree`'s failed-badge
                // aggregation to see this address at all.
                let addr = BlockAddr::new(node_id, block);
                self.step_output.insert(
                    addr.clone(),
                    StepOutput {
                        text: self
                            .step_output
                            .get(&addr)
                            .map(|entry| entry.text.clone())
                            .unwrap_or_default(),
                        stdout: run.stdout_only.clone(),
                        stderr: run.stderr_only.clone(),
                        output_markdown: run.output_markdown,
                        exit_code: -1,
                        duration_ms: run.step_started.elapsed().as_millis() as u64,
                        running: false,
                    },
                );
                touched = Some(addr);
            }
            RunEvent::Error { message } => {
                run.had_failure = true;
                run.finished = true;
                run.http_rx = None;
                self.status = format!("meshfox: {message}");
            }
            RunEvent::Done { .. } => {
                run.finished = true;
                run.http_rx = None;
                self.status = if run.killed {
                    "run killed".into()
                } else if run.had_failure {
                    "run finished with a failure".into()
                } else {
                    "run finished".into()
                };
                if let Some(addr) = self.pending_autoruns.pop_front() {
                    if let Some(port) = self.worker_port {
                        Box::pin(self.start_run_via_worker(
                            addr.node_id,
                            addr.block_name,
                            true,
                            port,
                            None,
                            HashMap::new(),
                        ))
                        .await;
                    }
                }
            }
        }
        // Same trailing check `on_external_run_event` ends on — only
        // worth rebuilding `doc_segments` when the address this event
        // touched is actually the node on screen right now.
        if let Some(addr) = touched {
            if self
                .rows
                .get(self.selected)
                .is_some_and(|row| row.node_id == addr.node_id)
            {
                self.render_current_document();
            }
        }
    }

    /// Whether `addr`'s block declares `output="markdown"` — looked up fresh
    /// from `display_canvas`, best-effort `false` if the block can't be found
    /// (already gone from a since-edited canvas, say).
    fn block_output_markdown(&self, addr: &BlockAddr) -> bool {
        self.display_canvas
            .node(&addr.node_id)
            .map(|n| n.runnable_blocks())
            .unwrap_or_default()
            .into_iter()
            .find(|b| b.name.as_deref() == Some(definition_addr(&addr).block_name.as_str()))
            .is_some_and(|b| b.attrs.get("output").map(String::as_str) == Some("markdown"))
    }

    /// [`Self::on_external_run_event`] for a run found at startup
    /// (`mod.rs`'s `reconcile_runs`) rather than announced by
    /// `WatchEvent::RunStarted`: the worker already knows how long a finished
    /// run took, and replaying it takes no time at all, so its terminal event
    /// carries that known `duration_ms` instead of the near-zero replay time
    /// `on_external_run_event` would measure.
    pub fn on_reconciled_run_event(
        &mut self,
        addr: BlockAddr,
        event: crate::worker_client::SubscribeEvent,
        duration_ms: Option<u64>,
    ) {
        let done = matches!(event, crate::worker_client::SubscribeEvent::Done { .. });
        self.fold_external_run_event(addr.clone(), event, duration_ms.is_none());
        if let (true, Some(ms)) = (done, duration_ms) {
            if let Some(entry) = self.step_output.get_mut(&addr) {
                entry.duration_ms = ms;
            }
            if self
                .rows
                .get(self.selected)
                .is_some_and(|row| row.node_id == addr.node_id)
            {
                self.render_current_document();
            }
        }
    }

    /// Folds one `worker_client::SubscribeEvent` for `addr` into
    /// `step_output`/`external_running` — the passive-watch counterpart to
    /// `on_run_event`, for a run this TUI session never itself started
    /// (see `worker_client::WatchEvent::RunStarted`'s own doc comment and
    /// the web UI's `watchAutorunBlock`, which this mirrors). Skips
    /// addresses this session's own `self.run` already owns: that run
    /// already folds its output into the very same `step_output` map via
    /// `on_run_event`, so a passive subscription racing alongside it would
    /// only double up on writes that already agree.
    ///
    /// Unlike a self-triggered run, there's no live `RunState` to hold a
    /// growing `stdout_only`/`stderr_only` between lines — `step_output`'s
    /// own entry is grown in place instead, one `Line` at a time, so
    /// `render_current_document`'s live-splice shows this run's output
    /// growing the same way it would for a run this session started
    /// itself. `output_markdown` is looked up fresh from `display_canvas`
    /// (this TUI never resolved this block's own attributes for a run it
    /// didn't start) — best-effort `false` if the block can't be found
    /// (already gone from a since-edited canvas, say).
    #[cfg(test)]
    pub fn on_external_run_event(
        &mut self,
        addr: BlockAddr,
        event: crate::worker_client::SubscribeEvent,
    ) {
        self.fold_external_run_event(addr, event, true);
    }

    fn fold_external_run_event(
        &mut self,
        addr: BlockAddr,
        event: crate::worker_client::SubscribeEvent,
        console: bool,
    ) {
        use crate::worker_client::SubscribeEvent;
        if self
            .run
            .as_ref()
            .is_some_and(|r| r.is_running() && r.chain.contains(&addr))
        {
            // A passive Line may have arrived before our StepStart. Even
            // when ignoring duplicate output, never retain its running badge.
            self.external_running.remove(&addr);
            return;
        }
        if console {
            self.console_last_activity = Some(std::time::Instant::now());
            match &event {
                SubscribeEvent::Line { text, .. } => {
                    if !self.external_running.contains_key(&addr) {
                        self.external_console_lines
                            .push(format!("==> {}/{}", addr.node_id, addr.block_name));
                    }
                    self.external_console_lines.push(text.clone());
                }
                SubscribeEvent::Done { exit_code } => {
                    self.external_console_lines.push(format!(
                        "==> {}/{} (exit {})",
                        addr.node_id,
                        addr.block_name,
                        exit_code.unwrap_or(-1)
                    ));
                }
            }
        }
        match event {
            SubscribeEvent::Line { stream, text } => {
                // `external_running` not yet holding this address means
                // this is the first `Line` of a *fresh* subscription (a
                // new `WatchEvent::RunStarted`, in `spawn_run_subscriber`)
                // — `subscribe_run` always replays from the new run's own
                // `RunHandle` (a fresh ring buffer, `since_seq` unused
                // here), so without clearing the stale entry first, a
                // second trigger of the same form-`autorun` block would
                // just keep appending onto whatever the *previous* run
                // left behind here, showing every past run's output
                // stacked instead of only the current one.
                if !self.external_running.contains_key(&addr) {
                    self.step_output.remove(&addr);
                    self.displayed_invocations
                        .insert(definition_addr(&addr), addr.clone());
                }
                self.external_running
                    .entry(addr.clone())
                    .or_insert_with(std::time::Instant::now);
                let output_markdown = self.block_output_markdown(&addr);
                let entry = self
                    .step_output
                    .entry(addr.clone())
                    .or_insert_with(|| StepOutput {
                        text: String::new(),
                        stdout: String::new(),
                        stderr: String::new(),
                        output_markdown,
                        exit_code: 0,
                        duration_ms: 0,
                        running: true,
                    });
                entry.running = true;
                entry.text.push_str(&text);
                entry.text.push('\n');
                let line = match stream {
                    meshfox_server::stream_exec::OutputStream::Stdout => &mut entry.stdout,
                    meshfox_server::stream_exec::OutputStream::Stderr => &mut entry.stderr,
                };
                line.push_str(&text);
                line.push('\n');
            }
            SubscribeEvent::Done { exit_code } => {
                let started = self.external_running.remove(&addr);
                if !self.step_output.contains_key(&addr) {
                    self.displayed_invocations
                        .insert(definition_addr(&addr), addr.clone());
                }
                // A run that printed nothing never got an entry from a
                // `Line` — make one, so its exit code still shows.
                let output_markdown = self.block_output_markdown(&addr);
                let entry = self
                    .step_output
                    .entry(addr.clone())
                    .or_insert_with(|| StepOutput {
                        text: String::new(),
                        stdout: String::new(),
                        stderr: String::new(),
                        output_markdown,
                        exit_code: 0,
                        duration_ms: 0,
                        running: false,
                    });
                entry.exit_code = exit_code.unwrap_or(-1);
                entry.running = false;
                if let Some(started) = started {
                    entry.duration_ms = started.elapsed().as_millis() as u64;
                }
            }
        }
        if self
            .rows
            .get(self.selected)
            .is_some_and(|row| row.node_id == addr.node_id)
        {
            self.render_current_document();
        }
    }

    /// Dispatches to the worker-routed path (`start_run_via_worker`) when a
    /// worker is reachable, `start_run_local`'s original direct-file/
    /// local-process chain-driving otherwise. Every call site here calls
    /// this, never either half directly, so the branch lives in exactly
    /// one place.
    async fn start_run(&mut self, node_id: String, block_name: String, with_deps: bool) {
        let Some(port) = self.worker_port else {
            self.status = "no worker for canvas".into();
            return;
        };
        self.start_run_via_worker(node_id, block_name, with_deps, port, None, HashMap::new())
            .await;
    }

    /// Runs a runnable `file` node (`type="file"` with both `target` and
    /// `interpreter` set) as `interpreter target`, streaming output live —
    /// the TUI counterpart to the web UI's own "▷ run" button on a `file`
    /// node's title bar (`run_file_node` in `crates/server/src/lib.rs`),
    /// previously the only way to run one at all.
    async fn start_file_run(&mut self, node_id: String, _node: Node) {
        let Some(port) = self.worker_port else {
            self.status = "no worker for this canvas".into();
            return;
        };
        match crate::worker_client::run_file_node_stream(port, &node_id).await {
            Ok(events) => {
                self.status.clear();
                self.output_scroll = 0;
                self.output_hscroll = 0;
                self.file_run = Some(FileRunState {
                    node_id: node_id.clone(),
                    events: Some(events),
                    run_id: None,
                    lines: vec![format!("==> {node_id}")],
                    had_failure: false,
                    finished: false,
                });
                // A file run is always a single execution — never a
                // multi-block chain — so it never auto-opens the console
                // (see `resolved_chain_len`'s call sites for the chain-run
                // equivalent of this rule).
            }
            Err(e) => {
                self.status = format!("failed to run {node_id:?}: {e}");
            }
        }
    }

    /// One event from `file_run`'s worker stream (`None`: the stream ended).
    /// Mirrors `on_run_event`, minus everything that only applies to a
    /// fenced block (no `cache`, no `meshfox:var`, no chain to advance).
    pub fn on_file_run_event(&mut self, event: Option<crate::worker_client::RunEvent>) {
        use crate::worker_client::RunEvent;
        let Some(run) = &mut self.file_run else {
            return;
        };
        self.console_last_activity = Some(std::time::Instant::now());
        let exit_code = match event {
            Some(RunEvent::Started { run_id, .. }) => {
                run.run_id = Some(run_id);
                return;
            }
            Some(RunEvent::Output { text, .. }) => {
                run.lines.push(text);
                return;
            }
            Some(RunEvent::Done { exit_code }) => exit_code,
            Some(RunEvent::Error { message }) => {
                run.lines.push(message);
                -1
            }
            Some(RunEvent::Killed { .. }) => -1,
            Some(_) => return,
            None => -1,
        };
        run.events = None;
        run.lines.push(format!("(exit {exit_code})"));
        run.had_failure = exit_code != 0;
        run.finished = true;
    }

    /// Called by `mod.rs` once `bridge_http_tty`'s relay loop returns —
    /// there's no `RunState`/chain `idx` to advance here at all: the
    /// *entire* chain (deps and interactive step alike) already ran
    /// server-side over the one `/api/run/tty` connection
    /// `begin_http_tty_run` opened — see `PendingHttpTty`'s own doc
    /// comment for why this never populates `self.run` in the first
    /// place. Only draining `pending_autoruns` (same "one foreground run
    /// slot" reasoning a chain-exhausted run has) carries over.
    pub async fn resume_after_http_tty(&mut self, exit_code: i32) {
        self.refresh_block_runs().await;
        self.status = if exit_code == 0 {
            "run finished".into()
        } else {
            "run finished with a failure".into()
        };
        if let Some(addr) = self.pending_autoruns.pop_front() {
            Box::pin(self.start_run(addr.node_id, addr.block_name, true)).await;
        }
    }

    /// Whether the console (Output pane) still needs `mod.rs`'s periodic
    /// tick calling `console_tick` — i.e. it's expanded, and either
    /// something's running (so re-collapsing wouldn't be correct yet, but
    /// the tick still has to keep checking in case it stops) or the 10s
    /// grace window since the last activity hasn't been confirmed stale
    /// yet. `false` once it's already collapsed, so the tick isn't
    /// scheduled at all when there's nothing left for it to do.
    pub fn console_pending_collapse(&self) -> bool {
        !self.console_collapsed
    }

    /// Whether `pane` is currently collapsed — always `false` for
    /// `Focus::Document`, which has no collapsed state of its own.
    fn is_pane_collapsed(&self, pane: Focus) -> bool {
        match pane {
            Focus::Tree => self.tree_collapsed,
            Focus::Output => self.console_collapsed,
            Focus::Document => false,
        }
    }

    /// Collapses `pane` outright — a no-op for `Focus::Document`.
    fn collapse_pane(&mut self, pane: Focus) {
        match pane {
            Focus::Tree => self.tree_collapsed = true,
            Focus::Output => self.console_collapsed = true,
            Focus::Document => {}
        }
    }

    /// Un-collapses `pane` outright, as a deliberate action (clicking its
    /// title bar/collapsed handle, `z`, `f`) — unlike `peek_pane_on_focus`,
    /// this always clears `auto_expanded_pane` for it (if set), so it
    /// stays open on its own terms regardless of where focus goes next,
    /// rather than snapping back closed the instant focus happens to move
    /// elsewhere.
    fn expand_pane(&mut self, pane: Focus) {
        match pane {
            Focus::Tree => self.tree_collapsed = false,
            Focus::Output => {
                self.console_collapsed = false;
                // `console_last_activity` has to move too: it's `None`
                // until the very first run this session (see its own doc
                // comment), and `console_tick`'s own `is_none_or(..)`
                // treats `None` as "already stale" — without this, the
                // very next tick (`mod.rs`'s 500ms poll while anything is
                // expanded) would immediately re-collapse it right back
                // before anyone could see the difference.
                self.console_last_activity = Some(std::time::Instant::now());
            }
            Focus::Document => {}
        }
        if self.auto_expanded_pane == Some(pane) {
            self.auto_expanded_pane = None;
        }
    }

    /// Changes `self.focus`, first collapsing whatever pane focus is
    /// *leaving* if `auto_expanded_pane` says it was only expanded as a
    /// side effect of gaining that focus a moment ago (see that field's
    /// own doc comment) — shared by every focus-changing action (`Tab`/
    /// `BackTab`, clicking any pane, via `toggle_fullscreen_on_title_click`
    /// and the Tree/Output collapsed-handle click handlers).
    fn set_focus(&mut self, new_focus: Focus) {
        if self.focus != new_focus && self.auto_expanded_pane == Some(self.focus) {
            self.collapse_pane(self.focus);
            self.auto_expanded_pane = None;
        }
        let changed = self.focus != new_focus;
        self.focus = new_focus;
        if changed && self.selected_block.is_some() {
            self.render_current_document();
        }
    }

    /// `Tab`/`BackTab` landing on a collapsed Tree/Output — expands it for
    /// as long as focus actually stays there (see `set_focus`'s own
    /// collapse-on-blur logic), rather than leaving focus on an invisible
    /// sliver with no visible sign anything changed.
    fn peek_pane_on_focus(&mut self, pane: Focus) {
        if self.is_pane_collapsed(pane) {
            self.expand_pane(pane);
            self.auto_expanded_pane = Some(pane);
        }
    }

    /// `z` — toggles collapse for whichever pane is focused; a no-op on
    /// Document (no collapsed state of its own) or on a pane that's
    /// currently fullscreen (collapsing it there would just blank the
    /// fullscreen area — see `expand_pane`'s own doc comment for the
    /// mirror-image reasoning on entering fullscreen instead).
    fn toggle_collapse_focused(&mut self) {
        if self.fullscreen == Some(self.focus) {
            return;
        }
        if self.is_pane_collapsed(self.focus) {
            self.expand_pane(self.focus);
        } else {
            self.collapse_pane(self.focus);
            if self.auto_expanded_pane == Some(self.focus) {
                self.auto_expanded_pane = None;
            }
        }
    }

    /// Whether `ui::render_tree`'s running-spinner badge is showing on any
    /// row at all right now — gates `mod.rs`'s own spinner tick, the same
    /// "no wakeups scheduled once there's nothing to animate" reasoning
    /// `console_pending_collapse`/`has_services` already follow for their
    /// own ticks.
    pub fn spinner_active(&self) -> bool {
        self.run.as_ref().is_some_and(RunState::is_running)
            || self.file_run.as_ref().is_some_and(|f| f.events.is_some())
            || !self.external_running.is_empty()
    }

    /// Advances `spinner_tick` by one frame — see that field's own doc
    /// comment for why a plain counter, not wall-clock time. Wrapping is
    /// harmless: only ever read modulo the frame count.
    pub fn advance_spinner(&mut self) {
        self.spinner_tick = self.spinner_tick.wrapping_add(1);
    }

    /// How long the console stays expanded after the last run/step
    /// activity (`console_last_activity`) before `console_tick` collapses
    /// it again — only actually applies once nothing is running, since
    /// every streamed line/step completion refreshes `console_last_activity`
    /// on its own.
    const CONSOLE_COLLAPSE_GRACE: std::time::Duration = std::time::Duration::from_secs(10);

    /// Re-collapses the console once nothing is running and it's been at
    /// least [`Self::CONSOLE_COLLAPSE_GRACE`] since the last activity —
    /// called from `mod.rs`'s periodic tick, guarded by
    /// `console_pending_collapse` so this is only ever polled while the
    /// console is actually expanded.
    pub fn console_tick(&mut self) {
        // Never auto-collapse out from under the user while they're
        // actually looking at it — whether that's keyboard focus or having
        // it fullscreened (fullscreen implies focus too, but is checked
        // explicitly in case that ever changes independently).
        if self.focus == Focus::Output || self.fullscreen == Some(Focus::Output) {
            return;
        }
        let running = self.run.as_ref().is_some_and(RunState::is_running)
            || self.file_run.as_ref().is_some_and(|r| r.events.is_some())
            || !self.external_running.is_empty();
        if running {
            return;
        }
        let stale = self
            .console_last_activity
            .is_none_or(|t| t.elapsed() >= Self::CONSOLE_COLLAPSE_GRACE);
        if stale {
            self.console_collapsed = true;
        }
    }

    /// The worker-routed equivalent of `tick_services` — refetches `GET
    /// /api/services` into `service_list` and recomputes `service_stats`
    /// from its own `status` strings (`"running"`/`"crashed"`/anything
    /// else counts as neither, matching `service_dto`'s own three-way
    /// split server-side). Called from `mod.rs`'s periodic tick whenever
    /// `worker_port` is `Some`, same ~3s cadence the web UI's own service
    /// panel already polls at. A failed request (worker gone, say) just
    /// leaves the last-known list/stats in place rather than clearing
    /// them — a transient hiccup shouldn't flash the footer badge away.
    pub async fn refresh_services(&mut self) {
        let Some(port) = self.worker_port else { return };
        let Ok(list) = crate::worker_client::list_services(port).await else {
            return;
        };
        self.apply_services(list);
    }

    fn apply_services(&mut self, list: Vec<crate::worker_client::ServiceDto>) {
        let (mut running, mut crashed) = (0usize, 0usize);
        for dto in &list {
            match dto.status.as_str() {
                "running" | "starting" | "stopping" | "restarting" => running += 1,
                "crashed" | "stop_failed" => crashed += 1,
                _ => {}
            }
        }
        self.service_stats = if list.is_empty() {
            None
        } else {
            Some((running, crashed))
        };
        self.service_list = list;
    }

    /// Refetches `block_runs` (`GET /api/runs`). A failed request keeps the
    /// last list, like `refresh_services` does.
    pub async fn refresh_block_runs(&mut self) {
        self.refresh_runs_soon = false;
        let Some(port) = self.worker_port else { return };
        if let Ok(runs) = crate::worker_client::list_active_runs(port).await {
            self.block_runs = Some(runs);
            self.sync_tty_results();
        }
    }

    /// `refresh_services` + `refresh_block_runs` without waiting for the
    /// worker: the periodic refresh runs in the event loop, and awaiting
    /// HTTP there would hold up every keystroke (the source editor's
    /// included) for as long as the worker takes to answer. The result
    /// arrives as `BackgroundMsg::WorkerState`. Does nothing while a
    /// previous request is outstanding; `refresh_runs_soon` then stays set so
    /// the next loop iteration retries.
    pub fn request_worker_refresh(&mut self) {
        let Some(port) = self.worker_port else {
            self.refresh_runs_soon = false;
            return;
        };
        if self.worker_refresh_inflight {
            return;
        }
        self.worker_refresh_inflight = true;
        self.refresh_runs_soon = false;
        let tx = self.background_tx.clone();
        tokio::spawn(async move {
            let (services, runs) = tokio::join!(
                crate::worker_client::list_services(port),
                crate::worker_client::list_active_runs(port),
            );
            let _ = tx.send(BackgroundMsg::WorkerState {
                services: services.ok(),
                runs: runs.ok(),
            });
        });
    }

    /// Shows each `tty` block's latest finished run under it in the Document
    /// pane — the same `output: <block> · live · done|failed · <time>` frame a
    /// plain block gets, just with no output (a terminal session's isn't
    /// stored). Plain blocks get theirs by replaying their stored run
    /// (`mod.rs`'s `reconcile_runs` / `on_external_run_event`); a `tty` block
    /// has nothing to replay, so its outcome comes straight from
    /// `block_runs`. An entry whose run is gone from the list (the block was
    /// edited, or the session reset) is dropped again. Only touches `tty`
    /// blocks' entries.
    fn sync_tty_results(&mut self) {
        let Some(runs) = &self.block_runs else { return };
        let finished: HashMap<BlockAddr, (i32, u64)> = runs
            .iter()
            .filter(|r| r.kind == "tty" && r.status != "running")
            .map(|r| {
                let code = if r.status == "killed" {
                    -1
                } else {
                    r.exit_code.unwrap_or(-1)
                };
                (BlockAddr::new(&r.node_id, &r.block), (code, r.uptime_ms))
            })
            .collect();
        let mut changed_nodes: Vec<String> = Vec::new();
        for (addr, (code, duration_ms)) in &finished {
            let same = self.step_output.get(addr).is_some_and(|so| {
                !so.running && so.exit_code == *code && so.duration_ms == *duration_ms
            });
            if same {
                continue;
            }
            self.displayed_invocations
                .insert(definition_addr(addr), addr.clone());
            self.step_output.insert(
                addr.clone(),
                StepOutput {
                    text: String::new(),
                    stdout: String::new(),
                    stderr: String::new(),
                    output_markdown: false,
                    exit_code: *code,
                    duration_ms: *duration_ms,
                    running: false,
                },
            );
            changed_nodes.push(addr.node_id.clone());
        }
        let gone: Vec<BlockAddr> = self
            .step_output
            .keys()
            .filter(|addr| !finished.contains_key(*addr) && self.is_tty_block(addr))
            .cloned()
            .collect();
        for addr in gone {
            self.step_output.remove(&addr);
            changed_nodes.push(addr.node_id);
        }
        if self
            .rows
            .get(self.selected)
            .is_some_and(|row| changed_nodes.contains(&row.node_id))
        {
            self.render_current_document();
        }
    }

    fn is_tty_block(&self, addr: &BlockAddr) -> bool {
        self.display_canvas
            .node(&addr.node_id)
            .map(|n| n.runnable_blocks())
            .unwrap_or_default()
            .into_iter()
            .any(|b| b.tty && b.name.as_deref() == Some(definition_addr(&addr).block_name.as_str()))
    }

    /// Nodes whose blocks' latest current run failed or was killed, per the
    /// worker's own record (`block_runs`) — so a block that failed and later
    /// succeeded, or whose run no longer describes the document after an
    /// edit or a session reset, isn't flagged. Empty until the first fetch;
    /// with no worker there is nothing to show either (the TUI can't start
    /// a run without one), and if the worker goes away the last list stays.
    pub fn failed_node_ids(&self) -> std::collections::HashSet<&str> {
        self.block_runs
            .iter()
            .flatten()
            .filter(|r| r.status == "killed" || (r.status == "exited" && r.exit_code != Some(0)))
            .map(|r| r.node_id.as_str())
            .collect()
    }

    /// Refreshes `service_log` for one service — the worker-routed
    /// equivalent of calling `ServiceHandle::log_snapshot()` directly at
    /// render time (impossible here since fetching it is an async HTTP
    /// call, not a synchronous read — see `service_log`'s own doc
    /// comment). Called right after the services view's selection changes
    /// and on `mod.rs`'s periodic tick while the view is open, so the log
    /// panel stays live without `render_services_view` itself ever
    /// touching the network.
    pub async fn refresh_service_log(&mut self, node_id: &str, block: &str) {
        let Some(port) = self.worker_port else { return };
        if let Ok(log) = crate::worker_client::get_service_log(port, node_id, block).await {
            self.service_log = log;
        }
    }

    /// `y`/Enter: retries the run with `force` set to the conflicting
    /// address — the worker kills whatever it finds still holding it
    /// (`run_ledger::kill_running`/`force_take_over`) before its own
    /// locking pass. `n`/Esc: cancels — the chain ends here, same "block
    /// errors out" outcome `meshfox run`'s own declined prompt has. See
    /// `ServiceConflictState`'s own doc comment.
    async fn on_service_conflict_key(&mut self, key: KeyEvent) {
        let Some(conflict) = self.service_conflict.take() else {
            return;
        };
        match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                // `http_retry` is always `Some` in practice now — a
                // worker-routed run is the only way this conflict is ever
                // raised (see `ServiceConflictState::http_retry`'s own doc
                // comment); a bare `None` here would mean the conflict got
                // parked some other way, which nothing does.
                let Some(retry) = conflict.http_retry else {
                    return;
                };
                self.status = format!(
                    "killed pid {} — restarting {:?}",
                    conflict.owner_pid, conflict.block_name
                );
                if retry.is_tty {
                    Box::pin(self.begin_http_tty_run(
                        retry.node_id,
                        retry.block_name,
                        retry.with_deps,
                        retry.port,
                        HashMap::new(),
                        std::collections::HashSet::new(),
                        Some((retry.force_node_id, retry.force_block)),
                    ))
                    .await;
                } else {
                    Box::pin(self.begin_http_run(
                        retry.node_id,
                        retry.block_name,
                        retry.with_deps,
                        retry.port,
                        HashMap::new(),
                        std::collections::HashSet::new(),
                        Some((retry.force_node_id, retry.force_block)),
                    ))
                    .await;
                }
            }
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                self.status = format!(
                    "cancelled — {:?} is already running elsewhere",
                    conflict.block_name
                );
                if let Some(run) = &mut self.run {
                    run.finished = true;
                    run.had_failure = true;
                }
            }
            _ => {
                // Any other key: leave it parked, waiting for a real answer.
                self.service_conflict = Some(conflict);
            }
        }
    }

    /// `v` on a row with a `service` block: stops it if running, restarts
    /// it otherwise (crashed or stopped) — the TUI's lightweight
    /// stand-in for the webui's dedicated service panel (no separate
    /// browsing view here; the tree itself, via `TreeRow::has_service` and
    /// the row glyph, is already the list). Acts on every service
    /// belonging to the selected node — same row-level (not per-block)
    /// granularity `has_service`/the glyph already use. No-op for a row
    /// with no active service to act on.
    /// `v` — opens the services list view (every service this process
    /// knows about at once, not scoped to the selected node), or reports
    /// there's nothing to show yet. See `ServicesViewState`.
    async fn open_services_view(&mut self) {
        if self.worker_port.is_some() {
            self.refresh_services().await;
            if self.service_list.is_empty() {
                self.status = "no services running yet".into();
                return;
            }
            self.services_view = Some(ServicesViewState { selected: 0 });
            let key = self.sorted_service_keys()[0].clone();
            self.refresh_service_log(&key.0, &key.1).await;
        }
    }

    /// A stable, sorted key order for `service_list` — the services view
    /// (and its own key handler) index into this rather than trusting
    /// `service_list`'s own last-poll response order, to stay put between
    /// frames/keypresses.
    pub(super) fn sorted_service_keys(&self) -> Vec<(String, String)> {
        let mut keys: Vec<(String, String)> = self
            .service_list
            .iter()
            .map(|d| (d.node_id.clone(), d.block.clone()))
            .collect();
        keys.sort();
        keys
    }

    /// `j`/`k`/arrows navigate, `s` stops the selected service (no-op if
    /// it isn't running), `r` restarts it (no-op if it's already
    /// running — use `s` first), `q`/Esc closes the view. **Experimental**,
    /// see SPEC.md's "Service blocks (experimental)".
    async fn on_services_view_key(&mut self, key: KeyEvent) {
        let keys = self.sorted_service_keys();
        if keys.is_empty() {
            self.services_view = None;
            return;
        }
        let selected = self
            .services_view
            .as_ref()
            .map(|v| v.selected)
            .unwrap_or(0)
            .min(keys.len() - 1);
        let Some(port) = self.worker_port else { return };
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                self.services_view = None;
            }
            KeyCode::Up | KeyCode::Char('k') => {
                let new_selected = selected.saturating_sub(1);
                if let Some(view) = &mut self.services_view {
                    view.selected = new_selected;
                }
                let key = keys[new_selected].clone();
                self.refresh_service_log(&key.0, &key.1).await;
            }
            KeyCode::Down | KeyCode::Char('j') => {
                let new_selected = (selected + 1).min(keys.len() - 1);
                if let Some(view) = &mut self.services_view {
                    view.selected = new_selected;
                }
                let key = keys[new_selected].clone();
                self.refresh_service_log(&key.0, &key.1).await;
            }
            KeyCode::Char('s') => {
                let (node_id, block) = keys[selected].clone();
                match crate::worker_client::stop_service(port, &node_id, &block).await {
                    Ok(()) => self.status = format!("stopped {block}"),
                    Err(e) => self.status = format!("failed to stop {block}: {e}"),
                }
                self.refresh_services().await;
                self.refresh_service_log(&node_id, &block).await;
            }
            KeyCode::Char('r') => {
                let (node_id, block) = keys[selected].clone();
                match crate::worker_client::confirmation_blocks(
                    port,
                    &self.path_to(&node_id),
                    &block,
                    true,
                )
                .await
                {
                    Ok(blocks) if !blocks.is_empty() => {
                        self.run_confirmation = Some(RunConfirmation {
                            blocks,
                            restart: true,
                            node_id,
                            block_name: block,
                            with_deps: false,
                            port,
                            vars: HashMap::new(),
                            save_secrets: Default::default(),
                            force: None,
                            tty: false,
                        });
                        return;
                    }
                    Ok(_) => {}
                    Err(error) => {
                        self.status = format!("confirmation check failed: {error}");
                        return;
                    }
                }
                match crate::worker_client::restart_service(port, &node_id, &block, false).await {
                    Ok(pid) => self.status = format!("restarted {block} (pid {pid})"),
                    Err(e) => self.status = format!("failed to restart {block}: {e}"),
                }
                self.refresh_services().await;
                self.refresh_service_log(&node_id, &block).await;
            }
            _ => {}
        }
    }

    /// Opens the `t` live-terminals view — every `tty` session this
    /// worker process currently knows about (started by this TUI, another
    /// TUI, or a browser tab, see `crates/server/src/lib.rs`'s own
    /// `get_active_runs`), whether or not anything is still attached to
    /// it. Worker-only: fallback (no-worker) mode has no shared registry
    /// of *other* connections' sessions to list at all — the only `tty`
    /// session it could ever know about is one this very process is
    /// already in the middle of running, which already has the terminal.
    async fn open_tty_sessions_view(&mut self) {
        let Some(port) = self.worker_port else {
            self.status = "live terminal sessions need a worker — none reachable".into();
            return;
        };
        match crate::worker_client::list_active_runs(port).await {
            Ok(runs) => {
                self.live_tty_sessions = runs
                    .into_iter()
                    .filter(|r| r.kind == "tty" && r.status == "running")
                    .collect();
            }
            Err(e) => {
                self.status = format!("failed to list live terminals: {e}");
                return;
            }
        }
        if self.live_tty_sessions.is_empty() {
            self.status = "no live terminal sessions".into();
            return;
        }
        self.tty_sessions_view = Some(TtySessionsViewState { selected: 0 });
    }

    /// `j`/`k`/arrows navigate, `enter` attaches to the selected session
    /// (`attach_selected_tty_session`), `K` kills it (same `POST /api/kill`
    /// any other run/session address goes through), `q`/Esc closes the
    /// view.
    async fn on_tty_sessions_view_key(&mut self, key: KeyEvent) {
        let Some(port) = self.worker_port else {
            self.tty_sessions_view = None;
            return;
        };
        if self.live_tty_sessions.is_empty() {
            self.tty_sessions_view = None;
            return;
        }
        let selected = self
            .tty_sessions_view
            .as_ref()
            .map(|v| v.selected)
            .unwrap_or(0)
            .min(self.live_tty_sessions.len() - 1);
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                self.tty_sessions_view = None;
            }
            KeyCode::Up | KeyCode::Char('k') => {
                if let Some(view) = &mut self.tty_sessions_view {
                    view.selected = selected.saturating_sub(1);
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if let Some(view) = &mut self.tty_sessions_view {
                    view.selected = (selected + 1).min(self.live_tty_sessions.len() - 1);
                }
            }
            KeyCode::Enter => {
                let session = self.live_tty_sessions[selected].clone();
                self.tty_sessions_view = None;
                self.attach_tty_session(port, session.node_id, session.block)
                    .await;
            }
            KeyCode::Char('K') => {
                let session = self.live_tty_sessions[selected].clone();
                match crate::worker_client::kill_run(port, &session.node_id, &session.block).await {
                    Ok(()) => self.status = format!("killed {}", session.block),
                    Err(e) => self.status = format!("failed to kill {}: {e}", session.block),
                }
                match crate::worker_client::list_active_runs(port).await {
                    Ok(runs) => {
                        self.live_tty_sessions = runs
                            .into_iter()
                            .filter(|r| r.kind == "tty" && r.status == "running")
                            .collect();
                    }
                    Err(_) => self.live_tty_sessions.clear(),
                }
                if self.live_tty_sessions.is_empty() {
                    self.tty_sessions_view = None;
                } else if let Some(view) = &mut self.tty_sessions_view {
                    view.selected = view.selected.min(self.live_tty_sessions.len() - 1);
                }
            }
            _ => {}
        }
    }

    /// Opens the `H` history view — every undo-log step the worker knows
    /// about (`GET /api/history`), newest first, with the current cursor
    /// preselected. Worker-only, like `t`: the log lives in the worker.
    async fn open_history_view(&mut self) {
        if self.read_only {
            self.status = READ_ONLY_STATUS.into();
            return;
        }
        let Some(port) = self.worker_port else {
            self.status = "history needs a worker — none reachable".into();
            return;
        };
        match crate::worker_client::history(port, Some(HISTORY_VIEW_LIMIT)).await {
            Ok(history) => {
                if history.entries.is_empty() {
                    self.status = "no history yet".into();
                    return;
                }
                // Already newest first (`UndoLog::history_around`), which is
                // the order the view shows.
                let mut entries = history.entries;
                // "Start of history": `goto` seq 0 undoes everything. No real
                // row can undo the oldest step itself (each jumps to the state
                // right after its own step). Only offered when the list holds
                // the whole log — past the fetch limit, seq 0 would undo far
                // more than the rows shown. Not `oldest.seq - 1`: seqs have
                // gaps once a redo tail is dropped, so that may never be a
                // cursor value.
                let applied = entries.iter().filter(|e| e.applied).count();
                if !entries.is_empty() && applied < HISTORY_VIEW_LIMIT {
                    entries.push(crate::worker_client::HistoryEntryDto {
                        seq: 0,
                        created_at: String::new(),
                        op_kind: String::new(),
                        applied: true,
                        summary: "Start of history".to_string(),
                    });
                }
                // Preselect the newest *applied* step — the current state.
                let selected = entries.iter().position(|e| e.applied).unwrap_or(0);
                self.history_view = Some(HistoryViewState {
                    entries,
                    selected,
                    ..Default::default()
                });
            }
            Err(e) => self.status = format!("failed to load history: {e}"),
        }
    }

    /// `j`/`k`/arrows/PageUp/PageDown navigate, `enter` jumps the canvas to
    /// the state right after the selected step (`POST /api/history/goto` —
    /// undoing or redoing as many steps as that takes), `q`/Esc closes.
    async fn on_history_view_key(&mut self, key: KeyEvent) {
        let Some(view) = &mut self.history_view else {
            return;
        };
        let last = view.entries.len().saturating_sub(1);
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => self.history_view = None,
            KeyCode::Up | KeyCode::Char('k') => view.selected = view.selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => view.selected = (view.selected + 1).min(last),
            KeyCode::PageUp => view.selected = view.selected.saturating_sub(10),
            KeyCode::PageDown => view.selected = (view.selected + 10).min(last),
            KeyCode::Home | KeyCode::Char('g') => view.selected = 0,
            KeyCode::End | KeyCode::Char('G') => view.selected = last,
            KeyCode::Enter => {
                let Some(entry) = view.entries.get(view.selected).cloned() else {
                    return;
                };
                self.history_view = None;
                self.goto_history_step(entry).await;
            }
            _ => {}
        }
    }

    /// The history view is modal: the wheel moves the selection, a click
    /// selects the row under it, a double-click jumps there (same as
    /// `Enter`), and everything else is swallowed so a click never reaches
    /// the panes underneath.
    async fn on_history_view_mouse(&mut self, mouse: MouseEvent) {
        let Some(view) = &mut self.history_view else {
            return;
        };
        let last = view.entries.len().saturating_sub(1);
        match mouse.kind {
            MouseEventKind::ScrollUp => view.selected = view.selected.saturating_sub(1),
            MouseEventKind::ScrollDown => view.selected = (view.selected + 1).min(last),
            MouseEventKind::Down(MouseButton::Left) => {
                let rect = view.list_rect.get();
                let inside = mouse.column >= rect.x
                    && mouse.column < rect.x + rect.width
                    && mouse.row >= rect.y
                    && mouse.row < rect.y + rect.height;
                let idx = view.offset.get() + (mouse.row.saturating_sub(rect.y)) as usize;
                if !inside || idx >= view.entries.len() {
                    return;
                }
                let is_double_click = matches!(
                    self.last_click,
                    Some((r, c, t))
                        if r == mouse.row && c == mouse.column && t.elapsed() < DOUBLE_CLICK_WINDOW
                );
                view.selected = idx;
                if is_double_click {
                    self.last_click = None;
                    let entry = view.entries[idx].clone();
                    self.history_view = None;
                    self.goto_history_step(entry).await;
                } else {
                    self.last_click = Some((mouse.row, mouse.column, std::time::Instant::now()));
                }
            }
            _ => {}
        }
    }

    /// `L` — opens the run history of the selected node's block, with the
    /// same "which block?" logic `r`/`R` use: a node with one block that has a
    /// history goes straight to it, one with several opens the picker first.
    /// Plain and `tty` blocks have a history (a `tty` run keeps its outcome
    /// and timing, not its output); a `service`, `button` or `form` block
    /// never leaves a run record behind, so those aren't offered (and a node
    /// with none says so).
    async fn open_run_history(&mut self) {
        if self.focus == Focus::Document {
            if let Some(addr) = self.selected_block.clone() {
                self.activate_block_action(addr, markdown::BlockAction::History)
                    .await;
                return;
            }
        }
        if self.worker_port.is_none() {
            self.status = "run history needs a worker — none reachable".into();
            return;
        }
        let Some(row) = self.rows.get(self.selected) else {
            return;
        };
        let node_id = row.node_id.clone();
        let Some(node) = self.display_canvas.node(&node_id) else {
            self.status = format!("node {node_id:?} not found");
            return;
        };
        if node.is_runnable_file() {
            self.status = "a runnable file node keeps no run history".into();
            return;
        }
        let blocks: Vec<_> = node
            .runnable_blocks()
            .into_iter()
            .filter(|b| {
                !b.service && !meshfox_core::is_button(&b.lang) && !meshfox_core::is_form(&b.lang)
            })
            .collect();
        match blocks.len() {
            0 => self.status = "no block in this node keeps a run history".into(),
            1 => {
                let name = blocks[0]
                    .name
                    .clone()
                    .expect("scan_runnable_blocks always names its blocks");
                self.open_run_history_for(node_id, name).await;
            }
            _ => {
                let default_name = fence::default_block(&node_id, &blocks)
                    .ok()
                    .flatten()
                    .and_then(|b| b.name.clone());
                let choices: Vec<BlockChoice> = blocks
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
                            tty: false,
                            is_button: false,
                        }
                    })
                    .collect();
                let selected = choices.iter().position(|c| c.is_default).unwrap_or(0);
                self.block_picker = Some(BlockPickerState {
                    node_id,
                    blocks: choices,
                    selected,
                    with_deps: false,
                    history: true,
                });
            }
        }
    }

    /// Fetches `block`'s run history and opens the view on its newest run.
    async fn open_run_history_for(&mut self, node_id: String, block: String) {
        let Some(port) = self.worker_port else {
            self.status = "run history needs a worker — none reachable".into();
            return;
        };
        match crate::worker_client::run_history(port, &node_id, &block).await {
            Ok(entries) if entries.is_empty() => {
                self.status = format!("no earlier runs of {block}")
            }
            Ok(entries) => {
                let tty = self.display_canvas.node(&node_id).is_some_and(|n| {
                    n.runnable_blocks()
                        .iter()
                        .any(|b| b.tty && b.name.as_deref() == Some(block.as_str()))
                });
                self.run_history_view = Some(RunHistoryViewState {
                    node_id,
                    block,
                    tty,
                    entries,
                    ..Default::default()
                });
                self.load_run_history_output().await;
            }
            Err(e) => self.status = format!("failed to load run history: {e}"),
        }
    }

    /// Loads the selected run's stored output into the view.
    async fn load_run_history_output(&mut self) {
        let Some(port) = self.worker_port else { return };
        let Some(view) = &self.run_history_view else {
            return;
        };
        let Some(run) = view.entries.get(view.selected) else {
            return;
        };
        let (id, node_id, block) = (run.id, view.node_id.clone(), view.block.clone());
        // A load that fails shows why in place of the output, rather than
        // passing for "this run printed nothing".
        let lines = crate::worker_client::run_output(port, &node_id, &block, id)
            .await
            .unwrap_or_else(|e| {
                vec![(
                    OutputStream::Stderr,
                    format!("couldn't load this run's output: {e}"),
                )]
            });
        if let Some(view) = &mut self.run_history_view {
            // Only if the selection hasn't moved on while this was loading.
            if view.entries.get(view.selected).is_some_and(|r| r.id == id) {
                view.output = Some((id, lines));
                view.output_scroll = 0;
            }
        }
    }

    /// `j`/`k`/arrows/`g`/`G` pick a run, `PageUp`/`PageDown` (or `J`/`K`)
    /// scroll its output, `q`/Esc closes.
    async fn on_run_history_key(&mut self, key: KeyEvent) {
        let Some(view) = &mut self.run_history_view else {
            return;
        };
        let last = view.entries.len().saturating_sub(1);
        let before = view.selected;
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                self.run_history_view = None;
                return;
            }
            KeyCode::Up | KeyCode::Char('k') => view.selected = view.selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => view.selected = (view.selected + 1).min(last),
            KeyCode::Home | KeyCode::Char('g') => view.selected = 0,
            KeyCode::End | KeyCode::Char('G') => view.selected = last,
            KeyCode::PageUp | KeyCode::Char('K') => {
                view.output_scroll = view.output_scroll.saturating_sub(10)
            }
            KeyCode::PageDown | KeyCode::Char('J') => view.scroll_output_down(10),
            _ => {}
        }
        if view.selected != before {
            view.output = None;
            self.load_run_history_output().await;
        }
    }

    /// The run-history view is modal: the wheel scrolls the output, a click
    /// on a run selects it, everything else is swallowed.
    async fn on_run_history_mouse(&mut self, mouse: MouseEvent) {
        let Some(view) = &mut self.run_history_view else {
            return;
        };
        match mouse.kind {
            MouseEventKind::ScrollUp => view.output_scroll = view.output_scroll.saturating_sub(3),
            MouseEventKind::ScrollDown => view.scroll_output_down(3),
            MouseEventKind::Down(MouseButton::Left) => {
                let rect = view.list_rect.get();
                let inside = mouse.column >= rect.x
                    && mouse.column < rect.x + rect.width
                    && mouse.row >= rect.y
                    && mouse.row < rect.y + rect.height;
                let idx = view.list_offset.get() + (mouse.row.saturating_sub(rect.y)) as usize;
                if inside && idx < view.entries.len() && idx != view.selected {
                    view.selected = idx;
                    view.output = None;
                    self.load_run_history_output().await;
                }
            }
            _ => {}
        }
    }

    async fn goto_history_step(&mut self, entry: crate::worker_client::HistoryEntryDto) {
        let Some(port) = self.worker_port else { return };
        match crate::worker_client::history_goto(port, entry.seq).await {
            Ok(result) if !result.changed => self.status = "already at that step".into(),
            Ok(_) => {
                // Pull the rewritten canvas right away instead of waiting for
                // the `/api/watch` push, so the tree is already current.
                if let Ok(content) = crate::worker_client::get_canvas_raw(port).await {
                    let display = self.fetch_display_canvas().await;
                    self.on_external_change(content, display);
                }
                self.status = format!("jumped to #{} · {}", entry.seq, entry.summary);
            }
            Err(e) => self.status = format!("history jump failed: {e}"),
        }
    }

    /// Whether a foreground (non-`service`) run is in progress as far as
    /// this TUI knows: its own run, a passively watched one, a terminal
    /// being handed over, or any running entry in the worker's last
    /// `GET /api/runs`. The worker's `409` on a form's Send is the backstop.
    fn foreground_run_in_progress(&self) -> bool {
        self.spinner_active()
            || self.pending_http_tty.is_some()
            || self.pending_http_tty_attach.is_some()
            || self
                .block_runs
                .as_ref()
                .is_some_and(|runs| runs.iter().any(|r| r.status == "running"))
    }

    /// `WatchEvent::TtyStarted`: a terminal session started in the worker
    /// that this TUI didn't ask for (an `autorun` `tty` block after a
    /// form's Send, or a manual run in another frontend) — hand the terminal
    /// to it, like picking it in the `t` live-terminals view, unless one is
    /// already being handed over. Skipped for a session that is no longer
    /// running by the time this is looked at: this TUI's own run delivers
    /// its own announcement too, queued behind the handoff that just ended,
    /// and a finished session is still attachable (its byte history) — it
    /// would flash a dead screen.
    pub async fn on_tty_started(&mut self, node_id: String, block: String) {
        let Some(port) = self.worker_port else { return };
        if self.pending_http_tty.is_some() || self.pending_http_tty_attach.is_some() {
            return;
        }
        let running = crate::worker_client::list_active_runs(port)
            .await
            .map(|runs| {
                runs.iter().any(|r| {
                    r.kind == "tty"
                        && r.status == "running"
                        && r.node_id == node_id
                        && r.block == block
                })
            })
            .unwrap_or(false);
        if running {
            self.attach_tty_session(port, node_id, block).await;
        }
    }

    /// Connects `worker_client::tty_attach` and parks the result as
    /// `pending_http_tty_attach` for `mod.rs`'s event loop to hand the
    /// terminal to on its next iteration — the attach counterpart to
    /// `begin_http_tty_run`. A session can finish (or simply stop existing
    /// — killed by someone else, say) between this view listing it and the
    /// keypress that picks it, so a `404` here is a normal, unsurprising
    /// outcome, not a bug to alarm about.
    async fn attach_tty_session(&mut self, port: u16, node_id: String, block: String) {
        let (cols, rows) = crossterm::terminal::size().unwrap_or((80, 24));
        match crate::worker_client::tty_attach(port, &node_id, &block, cols, rows).await {
            Ok(socket) => {
                self.status.clear();
                self.pending_http_tty_attach = Some(PendingHttpTtyAttach {
                    socket,
                    block_name: block,
                });
            }
            Err(e) => {
                self.status = format!("failed to attach to {block:?}: {e}");
            }
        }
    }

    /// A `service` block now always runs inside the shared worker, not this
    /// TUI process itself — stopping it on quit (or on any other TUI exit
    /// path) is the worker's own job, not this process's; see
    /// `crates/server/src/lib.rs`'s `spawn_shutdown_signal_handler`. An
    /// *external* kill takes a different path: see `mod.rs`'s
    /// `spawn_signal_terminal_restore`.
    fn quit(&mut self) {
        self.should_quit = true;
    }

    async fn kill_running(&mut self) {
        if let Some(run) = &self.run {
            if run.http_rx.is_some() {
                if let (Some(port), Some(addr)) = (self.worker_port, run.chain.last()) {
                    let (node_id, block) = (addr.node_id.clone(), addr.block_name.clone());
                    self.status = "killing...".into();
                    let _ = crate::worker_client::kill_run(port, &node_id, &block).await;
                }
            }
        }
        if let (Some(run), Some(port)) = (&self.file_run, self.worker_port) {
            if let (true, Some(run_id)) = (run.events.is_some(), run.run_id.clone()) {
                let _ = crate::worker_client::kill_run_id(port, &run_id).await;
                self.status = "killing...".into();
            }
        }
    }

    /// Forgets every `form` field this process has submitted
    /// (`session_vars`) and every autorun this process has already fired
    /// (`pending_autoruns`), and — when this TUI runs blocks through a worker
    /// (`worker_port`) — has the worker forget *its* session
    /// (`POST /api/session/reset`: every block's "already ran, still fresh"
    /// record, `AppState::session_runs`, the thing that lets a chain run skip
    /// re-running an unchanged dependency, plus its own copy of submitted
    /// form values). Without that second half `S` looked like it reset the
    /// session but the next `r` still skipped every dependency the worker
    /// thought fresh. Never touches the canvas file itself or any persisted
    /// `<!-- meshfox:output ... -->` cache. See TODO.canvas.md: "Сброс
    /// сессии".
    async fn reset_session(&mut self) {
        self.session_vars.clear();
        self.pending_autoruns.clear();
        self.status = "session reset".into();
        if let Some(port) = self.worker_port {
            if let Err(e) = crate::worker_client::reset_session(port).await {
                self.status = format!("session reset here, but the worker didn't reset: {e}");
            }
        }
    }

    /// While `reset_session_confirm` is up (see `on_key`'s early dispatch) —
    /// `y`/Enter confirms, `n`/Esc backs out untouched. Same
    /// confirm-before-acting gate the web UI's own `ResetSessionConfirmDialog`
    /// puts in front of its "↺ reset session" button, for the same reason
    /// (see that component's doc comment): the reset is harmless to the
    /// file/cache but still easy to trigger by accident and mildly costly to
    /// shrug off, so `S` alone shouldn't fire it immediately.
    /// Keyboard handling while the `?` help modal is open — claims the
    /// whole keymap (see `on_key`'s own precedence chain), same posture
    /// every other modal already takes. `j`/`k`/arrows/`PageUp`/`PageDown`/
    /// `Ctrl-u`/`Ctrl-d` scroll the modal's own content instead of falling
    /// through to whatever pane is focused underneath — `render_help`
    /// clamps `help_scroll` against the actual wrapped row count, so
    /// there's no need to bound it here (same "state is unclamped,
    /// rendering clamps" convention `doc_scroll`/`output_scroll` use).
    fn on_help_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('?') => self.show_help = false,
            KeyCode::Up | KeyCode::Char('k') => {
                self.help_scroll = self.help_scroll.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.help_scroll = self.help_scroll.saturating_add(1);
            }
            KeyCode::PageUp => self.help_scroll = self.help_scroll.saturating_sub(10),
            KeyCode::PageDown => self.help_scroll = self.help_scroll.saturating_add(10),
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.help_scroll = self.help_scroll.saturating_sub(10);
            }
            KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.help_scroll = self.help_scroll.saturating_add(10);
            }
            _ => {}
        }
    }

    async fn on_reset_session_confirm_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('y') | KeyCode::Enter => {
                self.reset_session_confirm = false;
                self.reset_session().await;
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
    /// never written to the cache, matching `vars::resolve`'s own secret
    /// handling.
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
                // An empty secret in `configure` means "leave it alone".
                .filter(|(_, (d, v))| !(vf.configuring && d.secret && v.is_empty()))
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
        if let Some(pending) = self.pending_http_run.take() {
            // Worker-routed resume: the worker itself persists each
            // non-secret answer to its own cache once the run actually
            // starts (`run_block_impl`'s own doc comment) — this process's
            // local `var_cache`/`run_overrides` play no part in HTTP mode,
            // so just collect the form's answers and hand them straight to
            // `begin_http_run`.
            let vars: HashMap<String, String> = vf
                .decls
                .iter()
                .zip(vf.inputs.iter())
                .map(|(d, v)| (d.name.clone(), v.clone()))
                .collect();
            if pending.arguments {
                let answers = vars.into_iter().collect();
                match crate::worker_client::prepare_arguments(
                    pending.port,
                    &self.path_to(&pending.node_id),
                    &pending.block_name,
                    &answers,
                    !pending.with_deps,
                )
                .await
                {
                    Ok(prepared) => {
                        if let Some(block) = prepared.block {
                            self.start_run_via_worker(
                                prepared.node_id,
                                block,
                                pending.with_deps,
                                pending.port,
                                pending.force,
                                HashMap::new(),
                            )
                            .await;
                        }
                    }
                    Err(e) => {
                        self.status = format!("meshfox: invalid arguments: {e}");
                        self.var_form = Some(vf);
                        self.pending_http_run = Some(pending);
                    }
                }
                return;
            }
            let save_secrets: std::collections::HashSet<String> = vf
                .decls
                .iter()
                .zip(vf.save.iter())
                .filter(|(d, save)| d.secret && **save)
                .map(|(d, _)| d.name.clone())
                .collect();
            if pending.is_tty {
                self.begin_http_tty_run(
                    pending.node_id,
                    pending.block_name,
                    pending.with_deps,
                    pending.port,
                    vars,
                    save_secrets,
                    pending.force,
                )
                .await;
            } else {
                self.begin_http_run(
                    pending.node_id,
                    pending.block_name,
                    pending.with_deps,
                    pending.port,
                    vars,
                    save_secrets,
                    pending.force,
                )
                .await;
            }
            return;
        }
        // Worker mode's own `c` flow — this only reaches here via
        // `trigger_configure`, the only remaining way a var_form opens
        // without going through the `pending_http_run` branch above.
        // `self.var_cache` is this *process's* own on-disk cache handle,
        // not the worker's, so this always resolves against the worker's
        // own `state.vars_cache` (`GET /api/vars`) instead.
        let Some(port) = self.worker_port else { return };
        let (secret_pairs, plain_pairs): (Vec<_>, Vec<_>) = vf
            .decls
            .iter()
            .zip(vf.inputs.iter())
            .partition(|(d, _)| d.secret);
        let vars: HashMap<String, String> = plain_pairs
            .into_iter()
            .map(|(d, v)| (d.name.clone(), v.clone()))
            .collect();
        let secrets: HashMap<String, String> = secret_pairs
            .into_iter()
            .filter(|(_, v)| !v.is_empty())
            .map(|(d, v)| (d.name.clone(), v.clone()))
            .collect();
        let clear: Vec<String> = vf
            .decls
            .iter()
            .zip(vf.clear.iter())
            .filter(|(d, c)| **c && d.secret && !secrets.contains_key(&d.name))
            .map(|(d, _)| d.name.clone())
            .collect();
        match crate::worker_client::post_configure_vars(port, vars, secrets, clear).await {
            Ok(saved) => {
                self.status = format!("meshfox: saved {saved} declared variable(s) to the cache");
            }
            Err(e) => {
                self.status = format!("meshfox: failed to save configured variable(s): {e}");
            }
        }
    }

    /// `c` — walks every declared non-secret, non-session, non-`from=`
    /// variable in the whole document (regardless of which, if any, block
    /// currently references it via `env=`), same scope `meshfox configure`
    /// covers, all shown at once with each one's currently-resolved value
    /// as the pre-filled suggestion. Confirming (even unchanged) writes it
    /// to the cache — the browser counterpart is `VarsForm` opened from the
    /// toolbar's "configure" button; see `crates/server/src/lib.rs`'s
    /// `/api/vars/configure`/`get_configure_vars`, whose filter this
    /// mirrors: a `from`-declared variable is computed by running its own
    /// block, never something to configure by hand. A no-op (past a status
    /// message) when there's nothing configurable, or while a run/another
    /// form/the block picker is already active.
    async fn trigger_configure(&mut self) {
        if self.var_form.is_some() || self.block_picker.is_some() {
            return;
        }
        if self.run.as_ref().is_some_and(|r| !r.finished) {
            self.status = "a run is already in progress — press K to kill it first".into();
            return;
        }
        // Worker mode: `self.var_cache` is this *process's* own on-disk
        // cache handle — reading it directly here would show (and, on
        // submit, write to) a copy the worker's own `state.vars_cache`
        // never sees, which is what silently made a value "configured" in
        // the TUI have no effect on a worker-routed run right after (the
        // run resolves against the *worker's* cache, via `GET /api/vars`,
        // never this process's own). `GET /api/vars/configure` is the
        // worker's own already-materialized answer (choices/defaults from
        // a `from=` source it may have had to run to compute) to exactly
        // this same question.
        if let Some(port) = self.worker_port {
            match crate::worker_client::get_configure_vars(port).await {
                Ok(statuses) => {
                    if statuses.is_empty() {
                        self.status =
                            "meshfox: this canvas declares no configurable (non-session, non-from=) variable(s)"
                                .into();
                        return;
                    }
                    let mut form = var_form_from_statuses(statuses);
                    form.configuring = true;
                    self.var_form = Some(form);
                }
                Err(e) => {
                    self.status = format!("meshfox: failed to list configurable variables: {e}");
                }
            }
            return;
        }
        self.status = "no worker for this canvas".into();
    }

    /// Whether the footer/help hint for `c` (configure) should be shown at
    /// all — same "configurable" definition `trigger_configure` itself
    /// uses (declared, non-secret, non-session, non-`from=`; a document
    /// that declares only secret/session/computed variables has nothing
    /// `c` could usefully do, same as the CLI's own `configure` skipping
    /// them).
    pub fn has_configurable_vars(&self) -> bool {
        let keychain = self.secret_keychain;
        self.decls
            .iter()
            .any(|d| (keychain || !d.secret) && !d.session && d.from.is_none())
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
#[cfg(not(test))]
fn local_display_canvas(canvas: &Canvas, _canvas_path: &Path) -> Canvas {
    canvas.clone()
}

/// Unit tests run `App` without a worker, so they resolve locally.
#[cfg(test)]
fn local_display_canvas(canvas: &Canvas, canvas_path: &Path) -> Canvas {
    resolve_includes(canvas, canvas_path)
}

#[cfg(test)]
fn resolve_includes(canvas: &Canvas, canvas_path: &Path) -> Canvas {
    let mut resolved =
        meshfox_core::include::resolve(canvas, canvas_path).unwrap_or_else(|_| canvas.clone());
    // Populates every node's `constraint_results` (see
    // `meshfox_core::constraint::annotate_status`) so `tree::flatten` and
    // `constraint_stats` below can read pass/fail straight off the tree
    // without re-evaluating it themselves.
    let canvas_dir = canvas_path.parent().filter(|p| !p.as_os_str().is_empty());
    let canvas_dir = Some(canvas_dir.unwrap_or(Path::new(".")));
    let access = meshfox_core::FileAccess::for_canvas_path(&resolved, canvas_path);
    meshfox_core::constraint::annotate_status_with_access(&mut resolved, canvas_dir, &access);
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
fn decode_data_url_image(url: &str, bg: Option<Background>) -> Option<image::DynamicImage> {
    let rest = url.strip_prefix("data:")?;
    let (meta, payload) = rest.split_once(',')?;
    if !meta.ends_with(";base64") {
        return None;
    }
    let bytes = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, payload).ok()?;
    // The `image` crate has no SVG decoder: an SVG goes through the
    // rasterizer (themed, `bg` painted behind it) instead.
    if meta.eq_ignore_ascii_case("image/svg+xml;base64") {
        return super::svg_raster::rasterize(std::str::from_utf8(&bytes).ok()?, bg);
    }
    image::load_from_memory(&bytes).ok()
}

/// A `.svg` file path (case-insensitive) — rasterized instead of decoded
/// by `image`, same as an `image/svg+xml` data URL.
fn is_svg_path(path: &Path) -> bool {
    path.extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("svg"))
}

/// `Picker::from_query_stdio()` does a real capability round-trip (writes
/// an escape sequence, reads the terminal's actual reply — see `mod.rs`'s
/// raw-mode-first ordering, which exists for exactly this call) rather
/// than guessing from env vars, and it already falls back to
/// `ProtocolType::Halfblocks` on its own whenever detection is
/// inconclusive.
///
/// iTerm2 (and terminals in its family — see ratatui-image's own
/// compatibility matrix entry for Warp: "Kitty unicode-placeholders part
/// not implemented") answers that same round-trip's Kitty-transmission
/// probe with "OK", but never implemented the Unicode-placeholder
/// placement extension `ratatui_image::protocol::kitty::Kitty` actually
/// renders through — so trusting that probe there doesn't silently fall
/// back to halfblocks, it draws a screenful of visible placeholder/
/// diacritic glyphs where the image should be. `from_query_stdio` already
/// special-cases WezTerm/Konsole the same way (see its own
/// `blacklist_protocols` handling) for an equivalent false-positive; this
/// adds iTerm2 to that list from our side so its own env-based
/// `iterm2_from_env()` hint wins instead. Exclude Sixel too: otherwise its
/// positive capability reply takes precedence over the iTerm2 hint, bypassing
/// our cell-based image sizing (Sixel pixels can differ from reported cell
/// metrics on Retina displays).
fn build_picker(configured: Option<&str>) -> io::Result<Picker> {
    let override_value = match std::env::var("MESHFOX_IMAGE_PROTOCOL") {
        Ok(value) => Some(value),
        Err(std::env::VarError::NotPresent) => None,
        Err(std::env::VarError::NotUnicode(_)) => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "MESHFOX_IMAGE_PROTOCOL must be valid Unicode",
            ))
        }
    };
    let forced = select_image_protocol(override_value.as_deref(), configured)?;
    if forced == Some(ProtocolType::Halfblocks) {
        return Ok(Picker::halfblocks());
    }
    let options = image_query_options(std::env::var("TERM_PROGRAM").ok().as_deref());
    let picker =
        Picker::from_query_stdio_with_options(options).unwrap_or_else(|_| Picker::halfblocks());
    let mut picker = apply_image_protocol(picker, forced);
    if picker.protocol_type() == ProtocolType::Sixel
        && std::env::var("TERM_PROGRAM").is_ok_and(|t| t.contains("iTerm"))
    {
        if let Some(metrics) = super::image_metrics::query_iterm_cell_pixels() {
            // The generic CSI query reports points in iTerm2, whereas Sixel
            // uses physical pixels. Preserve the queried display scale.
            #[allow(deprecated)]
            let mut physical_picker = Picker::from_fontsize(metrics);
            physical_picker.set_protocol_type(ProtocolType::Sixel);
            picker = physical_picker;
        }
    }
    Ok(picker)
}

fn select_image_protocol(
    override_value: Option<&str>,
    configured: Option<&str>,
) -> io::Result<Option<ProtocolType>> {
    parse_image_protocol(override_value.or(configured))
}

fn parse_image_protocol(value: Option<&str>) -> io::Result<Option<ProtocolType>> {
    match value {
        None | Some("" | "auto") => Ok(None),
        Some("sixel") => Ok(Some(ProtocolType::Sixel)),
        Some("iterm2") => Ok(Some(ProtocolType::Iterm2)),
        Some("kitty") => Ok(Some(ProtocolType::Kitty)),
        Some("halfblocks") => Ok(Some(ProtocolType::Halfblocks)),
        Some(value) => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("invalid MESHFOX_IMAGE_PROTOCOL={value:?}; expected auto, sixel, iterm2, kitty, or halfblocks"),
        )),
    }
}

fn apply_image_protocol(mut picker: Picker, forced: Option<ProtocolType>) -> Picker {
    if let Some(protocol) = forced {
        picker.set_protocol_type(protocol);
    }
    picker
}

fn image_query_options(term_program: Option<&str>) -> QueryStdioOptions {
    let mut options = QueryStdioOptions::default();
    if term_program.is_some_and(|t| t.contains("iTerm")) {
        options.blacklist_protocols = vec![ProtocolType::Kitty, ProtocolType::Sixel];
    }
    options
}

/// Fetches an image file of the document from the worker (once per path);
/// the result arrives as `BackgroundMsg::ImageBytes`.
fn request_image(
    port: Option<u16>,
    generation: u64,
    asset_base: Option<String>,
    tx: &tokio::sync::mpsc::UnboundedSender<BackgroundMsg>,
    requested: &mut std::collections::HashSet<PathBuf>,
    pending: &mut HashSet<PathBuf>,
    base_dir: &Path,
    path: &Path,
) {
    let Some(port) = port else { return };
    if !requested.insert(path.to_path_buf()) {
        return;
    }
    let Ok(relative) = path.strip_prefix(base_dir) else {
        return;
    };
    let relative = relative
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/");
    let (path, tx) = (path.to_path_buf(), tx.clone());
    pending.insert(path.clone());
    tokio::spawn(async move {
        let clean = relative.split(['?', '#']).next().unwrap_or(&relative);
        let relative = meshfox_core::image_attrs::decode_image_path(clean);
        let result = if let Some(base) = asset_base {
            crate::worker_client::get_include_asset(port, &base, &relative).await
        } else {
            crate::worker_client::get_relative_file(port, &relative).await
        };
        let _ = tx.send(BackgroundMsg::ImageBytes {
            generation,
            path,
            result,
        });
    });
}

/// Everything `load_document_image`'s output depends on besides the path.
#[derive(PartialEq)]
struct DocImageKey {
    content: Option<u64>,
    budget: ratatui::layout::Size,
    bg: Option<Background>,
    font: (u16, u16),
    protocol: ProtocolType,
}

const DOC_IMAGE_CACHE_ENTRIES: usize = 32;

/// `path` is a real file for every ordinary `Segment::Image`, but a
/// `data:` URL string doubling as its own synthetic cache key for one
/// pasted/embedded directly in the document (see `markdown::Renderer`'s
/// own `Tag::Image` handling) — same "no file on disk to key it by"
/// situation `link_preview_image_path` already has, just without a
/// prefix: a `data:` URL can't collide with a real relative path.
/// Encode document images once into protocols that can reveal individual rows.
fn load_document_image(
    picker: &Picker,
    path: &Path,
    bytes: Option<&[u8]>,
    bg: Option<Background>,
    budget: ratatui::layout::Size,
) -> Option<Arc<DocumentImage>> {
    let embedded_svg = path.to_str().and_then(|url| {
        let (meta, payload) = url.strip_prefix("data:")?.split_once(',')?;
        meta.eq_ignore_ascii_case("image/svg+xml;base64")
            .then(|| {
                base64::Engine::decode(&base64::engine::general_purpose::STANDARD, payload).ok()
            })
            .flatten()
    });
    let svg = embedded_svg
        .as_deref()
        .or_else(|| is_svg_path(path).then_some(bytes).flatten());
    let image = if let Some(svg) = svg {
        let density = if picker.protocol_type() == ProtocolType::Iterm2 {
            2
        } else {
            1
        };
        super::svg_raster::rasterize_to_fit(
            std::str::from_utf8(svg).ok()?,
            bg,
            u32::from(budget.width) * u32::from(picker.font_size().width) * density,
            u32::from(budget.height) * u32::from(picker.font_size().height) * density,
        )?
    } else {
        match path.to_str().filter(|s| s.starts_with("data:")) {
            Some(url) => decode_data_url_image(url, bg)?,
            None => image::load_from_memory(bytes?).ok()?,
        }
    };
    prepared_image(picker, image, budget).map(Arc::new)
}

pub(super) fn prepared_image(
    picker: &Picker,
    image: image::DynamicImage,
    budget: ratatui::layout::Size,
) -> Option<DocumentImage> {
    let density = if picker.protocol_type() == ProtocolType::Iterm2 {
        2
    } else {
        1
    };
    let font = picker.font_size();
    let cell_width = (u32::from(font.width) * density).max(1);
    let cell_height = (u32::from(font.height) * density).max(1);
    let pixels = image.resize(
        u32::from(budget.width) * cell_width,
        u32::from(budget.height) * cell_height,
        image::imageops::FilterType::Lanczos3,
    );
    let size = ratatui::layout::Size::new(
        pixels.width().div_ceil(cell_width) as u16,
        pixels.height().div_ceil(cell_height) as u16,
    );
    match picker.protocol_type() {
        ProtocolType::Iterm2 | ProtocolType::Sixel => {
            // Obtain tmux framing without encoding the full image or individual rows.
            let template = picker
                .new_protocol(
                    image::DynamicImage::new_rgba8(1, 1),
                    ratatui::layout::Size::new(1, 1),
                    ratatui_image::Resize::Fit(None),
                )
                .ok()?;
            let (tmux, sixel) = match template {
                Protocol::ITerm2(image) => (image.is_tmux, false),
                Protocol::Sixel(image) => (image.is_tmux, true),
                _ => return None,
            };
            Some(DocumentImage::fragment(
                size,
                pixels,
                cell_height,
                tmux,
                sixel,
            ))
        }
        _ => {
            SlicedProtocol::new_with_resize(picker, pixels, size, ratatui_image::Resize::Fit(None))
                .ok()
                .map(DocumentImage::sliced)
        }
    }
}

#[cfg(test)]
fn load_image_protocol(
    picker: &mut Picker,
    path: &Path,
    bytes: Option<&[u8]>,
    width_percent: Option<u32>,
    height_percent: Option<u32>,
    bg: Option<Background>,
) -> Option<Protocol> {
    load_image_protocol_with_budget(
        picker,
        path,
        bytes,
        bg,
        image_size_budget(width_percent, height_percent),
    )
}

#[cfg(test)]
fn load_image_protocol_with_budget(
    picker: &mut Picker,
    path: &Path,
    bytes: Option<&[u8]>,
    bg: Option<Background>,
    budget: ratatui::layout::Size,
) -> Option<Protocol> {
    // SVG data URLs and files share the same direct vector-to-display path.
    let embedded_svg = path.to_str().and_then(|url| {
        let (meta, payload) = url.strip_prefix("data:")?.split_once(',')?;
        meta.eq_ignore_ascii_case("image/svg+xml;base64")
            .then(|| {
                base64::Engine::decode(&base64::engine::general_purpose::STANDARD, payload).ok()
            })
            .flatten()
    });
    if let Some(svg) = embedded_svg
        .as_deref()
        .or_else(|| is_svg_path(path).then_some(bytes).flatten())
    {
        return load_svg_protocol(picker, std::str::from_utf8(svg).ok()?, bg, budget);
    }
    let dyn_img = match path.to_str().filter(|s| s.starts_with("data:")) {
        Some(data_url) => decode_data_url_image(data_url, bg)?,
        None => image::load_from_memory(bytes?).ok()?,
    };
    picker
        .new_protocol(
            dyn_img,
            budget,
            ratatui_image::Resize::Fit(Some(image::imageops::FilterType::Lanczos3)),
        )
        .ok()
        .map(image_cell_dimensions)
}

#[cfg(test)]
fn load_svg_protocol(
    picker: &Picker,
    svg: &str,
    bg: Option<Background>,
    budget: ratatui::layout::Size,
) -> Option<Protocol> {
    let font = picker.font_size();
    // iTerm2 accepts a dense PNG independently of its cell-based placement.
    // Keep twice the pixel density for Retina instead of shrinking to points
    // and asking the terminal to enlarge a low-resolution bitmap again.
    let density = if picker.protocol_type() == ProtocolType::Iterm2 {
        2
    } else {
        1
    };
    let pixels_per_col = u32::from(font.width).checked_mul(density)?;
    let pixels_per_row = u32::from(font.height).checked_mul(density)?;
    let image = super::svg_raster::rasterize_to_fit(
        svg,
        bg,
        u32::from(budget.width) * pixels_per_col,
        u32::from(budget.height) * pixels_per_row,
    )?;
    if picker.protocol_type() == ProtocolType::Iterm2 {
        let size = ratatui::layout::Size::new(
            image.width().div_ceil(pixels_per_col) as u16,
            image.height().div_ceil(pixels_per_row) as u16,
        );
        // Use the picker's own tmux detection/passthrough configuration.
        let Protocol::ITerm2(template) = picker
            .new_protocol(
                image::DynamicImage::new_rgba8(1, 1),
                ratatui::layout::Size::new(1, 1),
                ratatui_image::Resize::Fit(None),
            )
            .ok()?
        else {
            return None;
        };
        return ratatui_image::protocol::iterm2::Iterm2::new(image, size, template.is_tmux)
            .ok()
            .map(Protocol::ITerm2)
            .map(image_cell_dimensions);
    }
    picker
        .new_protocol(
            image,
            budget,
            ratatui_image::Resize::Fit(Some(image::imageops::FilterType::Lanczos3)),
        )
        .ok()
}

/// iTerm2 pixel dimensions can use a different scale than the reported font
/// metrics on Retina displays. Use the protocol's cell footprint for both
/// layout and display, so the image fills exactly the space we reserve.
pub(super) fn image_cell_dimensions(mut protocol: Protocol) -> Protocol {
    if let Protocol::ITerm2(image) = &mut protocol {
        if let Some(start) = image.data.find(";width=") {
            if let Some(end) = image.data[start..].find(";doNotMoveCursor=") {
                image.data.replace_range(
                    start..start + end,
                    &format!(";width={};height={}", image.size.width, image.size.height),
                );
            }
        }
    }
    protocol
}

/// Percentage attributes use the document pane, rather than the default
/// thumbnail budget. The unconstrained axis can use the whole viewport so
/// a width hint is not silently defeated by the old 24-row height limit.
#[cfg(test)]
fn image_size_budget_for_viewport(
    viewport: ratatui::layout::Size,
    width_percent: Option<u32>,
    height_percent: Option<u32>,
) -> ratatui::layout::Size {
    super::document_layout::LayoutContext::new(viewport).image_budget(width_percent, height_percent)
}

#[cfg(test)]
fn image_size_budget(
    width_percent: Option<u32>,
    height_percent: Option<u32>,
) -> ratatui::layout::Size {
    image_size_budget_for_viewport(
        ratatui::layout::Size::new(56, 24),
        width_percent,
        height_percent,
    )
}

fn point_in(rect: Rect, x: u16, y: u16) -> bool {
    x >= rect.x && x < rect.x + rect.width && y >= rect.y && y < rect.y + rect.height
}

// ---------------------------------------------------------------------------
// Worker-backed node data: `display="code"` content and `display="table"`.
// All of it goes through the worker's API (never straight off disk), so the
// confinement to the canvas directory and — for tables — the `duckdb` process
// live in one place, shared with the web UI.
// ---------------------------------------------------------------------------

impl App {
    /// Starts (or refreshes) the background fetch of a `display="code"`
    /// node's content. The first call leaves a "loading" entry; once content
    /// has landed, an entry older than [`FILE_PREVIEW_REFRESH`] is refetched
    /// while the old content stays on screen — so an edit to the file shows
    /// up the next time the pane is drawn, as it did when this read the disk
    /// itself.
    fn ensure_file_preview(&mut self, node_id: &str) {
        let Some(port) = self.worker_port else {
            self.file_previews.insert(
                node_id.to_string(),
                FilePreview {
                    state: FilePreviewState::Failed("no worker to read it through".into()),
                    inflight: false,
                    fetched: None,
                },
            );
            return;
        };
        let entry = self
            .file_previews
            .entry(node_id.to_string())
            .or_insert(FilePreview {
                state: FilePreviewState::Loading,
                inflight: false,
                fetched: None,
            });
        let due = entry
            .fetched
            .is_none_or(|t| t.elapsed() >= FILE_PREVIEW_REFRESH);
        if entry.inflight || !due {
            return;
        }
        entry.inflight = true;
        let tx = self.background_tx.clone();
        let node_id = node_id.to_string();
        tokio::spawn(async move {
            let result = crate::worker_client::get_node_file_content(port, &node_id).await;
            let _ = tx.send(BackgroundMsg::FileContent { node_id, result });
        });
    }

    /// The selected node's id when it is a `display="table"` file node.
    fn selected_table_node_id(&self) -> Option<String> {
        let row = self.rows.get(self.selected)?;
        let node = self.display_canvas.node(&row.node_id)?;
        (node.node_type == NodeType::File && node.display == Some(FileDisplay::Table))
            .then(|| node.id.clone())
    }

    /// Whether `Enter` should open the selected node's table: it is one, there
    /// is a worker to serve it, and (from the tree) it has no children whose
    /// expansion `Enter` would otherwise toggle.
    pub fn selected_opens_table(&self) -> bool {
        if self.worker_port.is_none() || self.selected_table_node_id().is_none() {
            return false;
        }
        self.focus == Focus::Document
            || self
                .rows
                .get(self.selected)
                .is_some_and(|r| !r.has_children)
    }

    /// For the footer: the selected node is a table node.
    pub fn selected_is_table(&self) -> bool {
        self.selected_table_node_id().is_some()
    }

    fn spawn_table_meta(&mut self, port: u16, node_id: &str) {
        let data = self.tables.entry(node_id.to_string()).or_default();
        if data.meta_inflight {
            return;
        }
        data.meta_inflight = true;
        let tx = self.background_tx.clone();
        let node_id = node_id.to_string();
        tokio::spawn(async move {
            let result = crate::worker_client::get_table_meta(port, &node_id).await;
            let _ = tx.send(BackgroundMsg::TableMeta { node_id, result });
        });
    }

    fn spawn_table_blocks(&mut self, port: u16, node_id: &str, blocks: Vec<usize>) {
        let Some(data) = self.tables.get(node_id) else {
            return;
        };
        let generation = data.generation;
        let view = data.view.clone();
        for block in blocks {
            let tx = self.background_tx.clone();
            let node_id = node_id.to_string();
            let view = view.clone();
            tokio::spawn(async move {
                let result = crate::worker_client::get_table_rows(
                    port,
                    &node_id,
                    block * table::BLOCK_ROWS,
                    table::BLOCK_ROWS,
                    &view,
                )
                .await;
                let _ = tx.send(BackgroundMsg::TableRows {
                    node_id,
                    generation,
                    block,
                    result,
                });
            });
        }
    }

    /// Makes sure the selected table node's schema is being fetched, then
    /// asks for the rows its inline window needs.
    fn ensure_table(&mut self, node_id: &str) {
        let Some(port) = self.worker_port else {
            self.tables
                .entry(node_id.to_string())
                .or_default()
                .meta_error = Some("no worker to read it through".into());
            return;
        };
        let data = self.tables.entry(node_id.to_string()).or_default();
        if data.meta.is_none() && !data.meta_inflight {
            self.spawn_table_meta(port, node_id);
        }
        self.schedule_table_rows(node_id);
    }

    /// Fetches whichever blocks the window being shown for `node_id` still
    /// lacks: the full-screen mode's rows (plus a block of margin either
    /// side) when it's open on that node, else the inline window's first rows
    /// when it's the selected node.
    fn schedule_table_rows(&mut self, node_id: &str) {
        let Some(port) = self.worker_port else { return };
        let window = match &self.table_mode {
            Some(tm) if tm.node_id == node_id => {
                Some((tm.top_row, usize::from(tm.body_height.get()), 1))
            }
            _ if self.selected_table_node_id().as_deref() == Some(node_id) => {
                Some((0, table::INLINE_ROWS, 0))
            }
            _ => None,
        };
        let Some((first, count, margin)) = window else {
            return;
        };
        let Some(data) = self.tables.get_mut(node_id) else {
            return;
        };
        if data.meta.as_ref().is_none_or(|m| m.state == "failed") {
            return;
        }
        let blocks = data.blocks_to_fetch(first, count, margin);
        if !blocks.is_empty() {
            self.spawn_table_blocks(port, node_id, blocks);
        }
    }

    /// The table nodes this client is currently looking at.
    fn watched_tables(&self) -> Vec<String> {
        let mut ids: Vec<String> = self.selected_table_node_id().into_iter().collect();
        if let Some(tm) = &self.table_mode {
            if !ids.contains(&tm.node_id) {
                ids.push(tm.node_id.clone());
            }
        }
        ids
    }

    /// While a watched table is still importing (or hasn't answered once),
    /// `mod.rs` polls fast instead of waiting for its slow tick.
    pub fn tables_want_fast_poll(&self) -> bool {
        self.worker_port.is_some()
            && self.watched_tables().iter().any(|id| {
                self.tables
                    .get(id)
                    .is_none_or(|d| d.meta.is_none() || d.is_importing())
            })
    }

    /// Re-asks the worker about each watched table: continually while it
    /// imports, every [`TABLE_META_REFRESH`] otherwise, so a changed file (a
    /// new `version`) is noticed and the rows reloaded.
    pub fn poll_tables(&mut self) {
        let Some(port) = self.worker_port else { return };
        for id in self.watched_tables() {
            let due = match self.tables.get(&id) {
                None => true,
                Some(d) if d.meta_inflight => false,
                Some(d) => match &d.meta {
                    None => true,
                    Some(m) if m.state == "importing" => true,
                    Some(m) => {
                        let wait = if m.state == "failed" {
                            TABLE_META_REFRESH * 2
                        } else {
                            TABLE_META_REFRESH
                        };
                        d.meta_fetched.is_none_or(|t| t.elapsed() >= wait)
                    }
                },
            };
            if due {
                self.spawn_table_meta(port, &id);
            }
        }
    }

    /// `Enter` on a table node: opens it full screen.
    fn open_table_mode(&mut self) {
        let Some(id) = self.selected_table_node_id() else {
            return;
        };
        let title = self
            .display_canvas
            .node(&id)
            .map(|n| n.target.clone().unwrap_or_else(|| n.title.clone()))
            .unwrap_or_else(|| id.clone());
        self.table_mode = Some(TableMode::new(id.clone(), title));
        self.ensure_table(&id);
    }

    fn close_table_mode(&mut self) {
        self.table_mode = None;
        self.render_current_document();
    }

    /// Moves the cursor by `drow`/`dcol` (clamped), scrolling to keep it in
    /// view and fetching what's newly visible.
    fn table_move(&mut self, drow: isize, dcol: isize) {
        let Some(tm) = &self.table_mode else { return };
        let row = tm.cursor_row.saturating_add_signed(drow);
        let col = tm.cursor_col.saturating_add_signed(dcol);
        self.table_goto(row, col);
    }

    fn table_goto(&mut self, row: usize, col: usize) {
        let Some(tm) = self.table_mode.as_mut() else {
            return;
        };
        let Some(data) = self.tables.get(&tm.node_id) else {
            return;
        };
        let max_row = data.row_count().unwrap_or(0).saturating_sub(1);
        let max_col = data.columns().len().saturating_sub(1);
        tm.cursor_row = row.min(max_row);
        tm.cursor_col = col.min(max_col);
        let height = usize::from(tm.body_height.get()).max(1);
        if tm.cursor_row < tm.top_row {
            tm.top_row = tm.cursor_row;
        } else if tm.cursor_row >= tm.top_row + height {
            tm.top_row = tm.cursor_row + 1 - height;
        }
        let id = tm.node_id.clone();
        self.schedule_table_rows(&id);
    }

    /// Applies a new sort/filter/search: refetches from the top.
    fn apply_table_view(&mut self, view: meshfox_server::tables::ViewSpec) {
        let Some(tm) = self.table_mode.as_mut() else {
            return;
        };
        let id = tm.node_id.clone();
        let Some(data) = self.tables.get_mut(&id) else {
            return;
        };
        if !data.is_ready() {
            tm.notice = Some("sorting, filtering and search wait for the import to finish".into());
            return;
        }
        data.set_view(view);
        tm.cursor_row = 0;
        tm.top_row = 0;
        self.schedule_table_rows(&id);
    }

    fn current_table_view(&self) -> Option<meshfox_server::tables::ViewSpec> {
        let tm = self.table_mode.as_ref()?;
        Some(self.tables.get(&tm.node_id)?.view.clone())
    }

    fn sort_table_by_cursor(&mut self, additive: bool) {
        let Some(mut view) = self.current_table_view() else {
            return;
        };
        let Some(tm) = &self.table_mode else { return };
        view.sort = table::toggle_sort(&view.sort, tm.cursor_col, additive);
        self.apply_table_view(view);
    }

    /// Rebuilds the view from what's typed in the prompts and applies it.
    fn apply_table_filters(&mut self) {
        let Some(mut view) = self.current_table_view() else {
            return;
        };
        let Some(tm) = &self.table_mode else { return };
        let Some(data) = self.tables.get(&tm.node_id) else {
            return;
        };
        view.filters = table::build_filters(&tm.filter_texts, data.columns());
        let search = tm.search_text.trim();
        view.search = (!search.is_empty()).then(|| search.to_string());
        self.apply_table_view(view);
    }

    fn copy_table_cell(&mut self) {
        let Some(tm) = self.table_mode.as_mut() else {
            return;
        };
        let value = self.tables.get(&tm.node_id).and_then(|d| {
            let col = d.columns().get(tm.cursor_col)?;
            d.row(tm.cursor_row)?.get(col.index).cloned()
        });
        tm.notice = Some(match value {
            None => "nothing to copy yet".to_string(),
            Some(v) => match table::copy_to_clipboard(v.as_deref().unwrap_or("")) {
                Ok(()) => "copied the cell to the clipboard".to_string(),
                Err(e) => format!("copy failed: {e}"),
            },
        });
    }

    /// Keys in the full-screen table. A prompt (`/`, `f`) claims every key
    /// until Enter applies it or Esc drops it.
    fn on_table_key(&mut self, key: KeyEvent) {
        let Some(tm) = self.table_mode.as_mut() else {
            return;
        };
        tm.notice = None;

        if tm.input.is_some() {
            match key.code {
                KeyCode::Esc => tm.input = None,
                KeyCode::Enter => {
                    match tm.input.take() {
                        Some(TableInput::Search(text)) => tm.search_text = text,
                        Some(TableInput::Filter { column, text }) => {
                            if text.trim().is_empty() {
                                tm.filter_texts.remove(&column);
                            } else {
                                tm.filter_texts.insert(column, text);
                            }
                        }
                        None => {}
                    }
                    self.apply_table_filters();
                }
                KeyCode::Backspace => {
                    if let Some(TableInput::Search(t) | TableInput::Filter { text: t, .. }) =
                        tm.input.as_mut()
                    {
                        t.pop();
                    }
                }
                KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    if let Some(TableInput::Search(t) | TableInput::Filter { text: t, .. }) =
                        tm.input.as_mut()
                    {
                        t.push(c);
                    }
                }
                _ => {}
            }
            return;
        }
        if tm.show_cell {
            if matches!(key.code, KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q')) {
                tm.show_cell = false;
            }
            return;
        }

        let page = tm.page() as isize;
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => self.close_table_mode(),
            KeyCode::Down | KeyCode::Char('j') => self.table_move(1, 0),
            KeyCode::Up | KeyCode::Char('k') => self.table_move(-1, 0),
            KeyCode::Right | KeyCode::Char('l') => self.table_move(0, 1),
            KeyCode::Left | KeyCode::Char('h') => self.table_move(0, -1),
            KeyCode::PageDown => self.table_move(page, 0),
            KeyCode::PageUp => self.table_move(-page, 0),
            KeyCode::Char('d') if ctrl => self.table_move(page / 2, 0),
            KeyCode::Char('u') if ctrl => self.table_move(-page / 2, 0),
            KeyCode::Char('g') | KeyCode::Home => {
                let col = tm.cursor_col;
                self.table_goto(0, col);
            }
            KeyCode::Char('G') | KeyCode::End => {
                let col = tm.cursor_col;
                self.table_goto(usize::MAX, col);
            }
            KeyCode::Char('0') => {
                let row = tm.cursor_row;
                self.table_goto(row, 0);
            }
            KeyCode::Char('$') => {
                let row = tm.cursor_row;
                self.table_goto(row, usize::MAX);
            }
            KeyCode::Char('s') => self.sort_table_by_cursor(false),
            KeyCode::Char('S') => self.sort_table_by_cursor(true),
            KeyCode::Char('/') => {
                tm.input = Some(TableInput::Search(tm.search_text.clone()));
            }
            KeyCode::Char('f') => {
                let column = tm.cursor_col;
                let text = tm.filter_texts.get(&column).cloned().unwrap_or_default();
                tm.input = Some(TableInput::Filter { column, text });
            }
            KeyCode::Char('F') => {
                let column = tm.cursor_col;
                tm.filter_texts.remove(&column);
                self.apply_table_filters();
            }
            KeyCode::Char('x') => {
                tm.filter_texts.clear();
                tm.search_text.clear();
                if let Some(mut view) = self.current_table_view() {
                    view.sort.clear();
                    view.filters.clear();
                    view.search = None;
                    self.apply_table_view(view);
                }
            }
            KeyCode::Enter => tm.show_cell = true,
            KeyCode::Char('y') => self.copy_table_cell(),
            _ => {}
        }
    }

    /// Mouse in the full-screen table: the wheel scrolls (horizontal wheel
    /// moves by column), a click on a header sorts by that column (Shift adds
    /// a key), a click on a cell moves the cursor there.
    fn on_table_mouse(&mut self, mouse: MouseEvent) {
        match mouse.kind {
            MouseEventKind::ScrollDown => self.table_move(3, 0),
            MouseEventKind::ScrollUp => self.table_move(-3, 0),
            MouseEventKind::ScrollRight => self.table_move(0, 1),
            MouseEventKind::ScrollLeft => self.table_move(0, -1),
            MouseEventKind::Down(MouseButton::Left) => {
                let Some(tm) = &self.table_mode else { return };
                let geometry = tm.geometry.borrow().clone();
                let column = geometry
                    .columns
                    .iter()
                    .find(|(x0, x1, _)| mouse.column >= *x0 && mouse.column < *x1)
                    .map(|(_, _, c)| *c);
                let in_header =
                    mouse.row == geometry.header_y || mouse.row == geometry.header_y + 1;
                let body = geometry.body;
                let in_body = mouse.row >= body.y
                    && mouse.row < body.y + body.height
                    && mouse.column >= body.x
                    && mouse.column < body.x + body.width;
                if in_header {
                    if let Some(c) = column {
                        let row = tm.cursor_row;
                        self.table_goto(row, c);
                        self.sort_table_by_cursor(mouse.modifiers.contains(KeyModifiers::SHIFT));
                    }
                } else if in_body {
                    let row = tm.top_row + usize::from(mouse.row - body.y);
                    let col = column.unwrap_or(tm.cursor_col);
                    self.table_goto(row, col);
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn canvas_search_wraps_occurrences_and_restores_only_its_own_branches() {
        let mut app = app_with_blocks("canvas-search", "## Alpha\n<!-- meshfox:node id=\"alpha\" -->\n\n### First\n<!-- meshfox:node id=\"first\" -->\n\nneedle NEEDLE\n\n## Beta\n<!-- meshfox:node id=\"beta\" -->\n\n### Second needle\n<!-- meshfox:node id=\"second\" -->\n\nbody\n").await;
        app.expanded.clear();
        app.rebuild_rows();
        app.on_key(key(KeyCode::Char('/'))).await;
        for c in "needle".chars() {
            app.on_key(key(KeyCode::Char(c))).await;
        }
        assert_eq!(app.search.matches(&app.display_canvas).len(), 3);
        assert_eq!(app.search.index, 0);
        assert_eq!(app.rows[app.selected].node_id, "first");
        assert!(app.expanded.contains("alpha"));
        assert!(app.search.reveal_pending);
        // Typing reveals 1/N immediately; Enter advances to 2/N.
        app.on_key(key(KeyCode::Enter)).await;
        assert_eq!(app.search.index, 1);
        assert_eq!(app.rows[app.selected].node_id, "first");
        assert!(app.expanded.contains("alpha"));
        app.on_key(key(KeyCode::Enter)).await;
        assert_eq!(app.rows[app.selected].node_id, "second");
        assert!(!app.expanded.contains("alpha"));
        assert!(app.expanded.contains("beta"));
        app.on_key(key(KeyCode::Enter)).await;
        assert_eq!(app.search.index, 0);
        assert_eq!(app.rows[app.selected].node_id, "first");
        app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT))
            .await;
        assert_eq!(app.search.index, 2);
        app.on_key(key(KeyCode::Esc)).await;
        assert!(!app.search.open);
        assert!(app.expanded.contains("beta"));
        assert_eq!(app.rows[app.selected].node_id, "second");
        app.on_key(key(KeyCode::Char('/'))).await;
        app.on_key(key(KeyCode::Enter)).await;
        assert!(
            app.expanded.contains("beta"),
            "branches open before this session stay open"
        );
        std::fs::remove_dir_all(app.canvas_path.parent().unwrap()).unwrap();
    }

    #[tokio::test]
    async fn canvas_search_owns_keys_and_handles_empty_results_and_live_reload() {
        let mut app = app_with_blocks("search-input", "needle\n").await;
        app.on_key(key(KeyCode::Char('/'))).await;
        for c in "qПривет".chars() {
            app.on_key(key(KeyCode::Char(c))).await;
        }
        assert_eq!(app.search.query, "qПривет");
        app.on_key(key(KeyCode::Backspace)).await;
        assert_eq!(app.search.query, "qПриве");
        assert!(
            !app.should_quit,
            "q belongs to the query while search is open"
        );
        app.on_key(key(KeyCode::Left)).await;
        app.on_key(key(KeyCode::Delete)).await;
        assert_eq!(app.search.query, "qПрив");
        app.on_key(key(KeyCode::Home)).await;
        app.on_key(key(KeyCode::Char('Ж'))).await;
        assert_eq!(app.search.query, "ЖqПрив");
        app.on_key(key(KeyCode::Right)).await;
        app.on_key(key(KeyCode::Backspace)).await;
        assert_eq!(app.search.query, "ЖПрив");
        assert!(app.search.matches(&app.display_canvas).is_empty());
        app.on_key(key(KeyCode::Enter)).await;
        app.on_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL))
            .await;
        assert!(app.search.query.is_empty());
        app.search.query = " needle ".into();
        app.search.index = 99;
        app.on_key(key(KeyCode::Enter)).await;
        assert_eq!(app.search.index, 0);
        app.display_canvas
            .nodes
            .iter_mut()
            .for_each(|n| n.text.clear());
        assert!(app.search.matches(&app.display_canvas).is_empty());
        app.on_key(key(KeyCode::Enter)).await;
        app.on_key(key(KeyCode::Esc)).await;
        assert_eq!(app.search.query, " needle ");
        std::fs::remove_dir_all(app.canvas_path.parent().unwrap()).unwrap();
    }

    #[tokio::test]
    async fn canvas_search_reveals_matching_folded_code_without_changing_source() {
        let mut app =
            app_with_blocks("search-fold", "```sh name=build fold\necho needle\n```\n").await;
        let raw = app.raw.clone();
        app.on_key(key(KeyCode::Char('/'))).await;
        for c in "needle".chars() {
            app.on_key(key(KeyCode::Char(c))).await;
        }
        assert!(document_text(&app).contains("echo needle"));
        assert_eq!(app.raw, raw);
        std::fs::remove_dir_all(app.canvas_path.parent().unwrap()).unwrap();
    }

    #[tokio::test]
    async fn opening_missing_file_reports_worker_error_without_handoff() {
        for target in ["missing.txt", "missing.canvas.md#root"] {
            let fixture = app_with_blocks(
                "missing-open",
                &format!(
                    "## Missing\n<!-- meshfox:node id=\"missing\" type=\"file\" -->\n\n[Missing]({target})\n"
                ),
            )
            .await;
            let mut app = app_with_real_worker(fixture.canvas_path.clone()).await;
            app.selected = app
                .rows
                .iter()
                .position(|r| r.node_id == "missing")
                .unwrap();
            app.on_key(key(KeyCode::Char('o'))).await;
            assert!(app.status.starts_with("failed to open:"), "{}", app.status);
            assert!(app.status.contains("missing."), "{}", app.status);
            assert!(app.pending_child_canvas.is_none());
            let _ = std::fs::remove_dir_all(app.canvas_path.parent().unwrap());
        }
    }

    #[tokio::test]
    async fn open_uses_worker_action_without_reading_the_returned_path() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let _ = rustls::crypto::ring::default_provider().install_default();
        for body in [
            r#"{"error":"missing.txt: No such file or directory"}"#,
            r#"{"action":"done"}"#,
            r#"{"action":"open_canvas","path":"/api-only/other.canvas.md","fragment":"child","port":4242}"#,
        ] {
            let mut app = app_with_blocks("api-open", "").await;
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            app.worker_port = Some(listener.local_addr().unwrap().port());
            let request = tokio::spawn(async move {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut bytes = vec![0; 4096];
                let n = socket.read(&mut bytes).await.unwrap();
                socket.write_all(format!(
                    "HTTP/1.1 {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", if body.contains("error") { "404 Not Found" } else { "200 OK" }, body.len()
                ).as_bytes()).await.unwrap();
                String::from_utf8_lossy(&bytes[..n]).into_owned()
            });
            app.on_key(key(KeyCode::Char('o'))).await;
            assert!(request
                .await
                .unwrap()
                .starts_with("POST /api/nodes/root/open?context=tui "));
            if body.contains("error") {
                assert!(app
                    .status
                    .contains("missing.txt: No such file or directory"));
                assert!(app.pending_child_canvas.is_none());
            } else if body.contains("open_canvas") {
                let pending = app.pending_child_canvas.as_ref().unwrap();
                assert_eq!(pending.path, Path::new("/api-only/other.canvas.md"));
                assert_eq!(pending.port, 4242);
                assert_eq!(pending.node.as_deref(), Some("child"));
            } else {
                assert_eq!(app.status, "opened");
                assert!(app.pending_child_canvas.is_none());
            }
            let _ = std::fs::remove_dir_all(app.canvas_path.parent().unwrap());
        }
    }

    #[tokio::test]
    async fn parameterized_invocations_show_live_output_under_the_definition() {
        use crate::worker_client::{RunEvent, SubscribeEvent};
        let mut app = app_with_blocks(
            "parameterized-output",
            "<!-- meshfox:arg name=output_dir default=../../target/screencast -->\n```sh name=record output=markdown\necho recording\n```\n",
        ).await;
        let definition = BlockAddr::new("root", "record");
        let first = BlockAddr::new("root", "record[output_dir=../../target/screencast]");
        let second = BlockAddr::new("root", "record[output_dir=/tmp/other-recording]");
        app.on_external_run_event(
            first.clone(),
            SubscribeEvent::Line {
                stream: meshfox_server::stream_exec::OutputStream::Stdout,
                text: "first recording".into(),
            },
        );
        assert!(document_text(&app).contains("first recording"));
        assert!(document_text(&app).contains("running"));
        assert!(app.step_output[&first].output_markdown);
        app.on_external_run_event(first.clone(), SubscribeEvent::Done { exit_code: Some(0) });
        app.run = Some(RunState {
            chain: Vec::new(),
            idx: 0,
            http_rx: None,
            lines: Vec::new(),
            full_output: String::new(),
            stdout_only: String::new(),
            stderr_only: String::new(),
            output_markdown: false,
            step_started: std::time::Instant::now(),
            had_failure: false,
            killed: false,
            finished: false,
        });
        app.on_run_event(Some(RunEvent::StepStart {
            node_id: "root".into(),
            block: second.block_name.clone(),
        }))
        .await;
        app.on_run_event(Some(RunEvent::Output {
            node_id: "root".into(),
            block: second.block_name.clone(),
            stream: meshfox_server::stream_exec::OutputStream::Stdout,
            text: "second recording".into(),
            child: None,
        }))
        .await;
        let text = document_text(&app);
        assert!(text.contains("second recording"), "{text}");
        assert!(!text.contains("first recording"), "{text}");
        assert!(text.contains("running"), "{text}");
        assert!(app.step_output[&second].output_markdown);
        assert_eq!(app.displayed_invocations[&definition], second);
        assert!(app.step_output.contains_key(&first));
        app.on_run_event(Some(RunEvent::StepEnd {
            node_id: "root".into(),
            block: second.block_name.clone(),
            exit_code: 0,
            duration_ms: 100,
        }))
        .await;
        assert!(document_text(&app).contains("second recording"));
        assert!(!app.step_output[&second].running);
    }

    #[tokio::test]
    async fn external_console_replays_active_but_not_finished_runs() {
        use crate::worker_client::SubscribeEvent;
        let mut app =
            app_with_blocks("external-console", "```sh name=build\necho build\n```\n").await;
        let addr = BlockAddr::new("root", "build");
        let line = |text: &str| SubscribeEvent::Line {
            stream: meshfox_server::stream_exec::OutputStream::Stdout,
            text: text.into(),
        };
        app.on_reconciled_run_event(addr.clone(), line("old output"), Some(123));
        app.on_reconciled_run_event(
            addr.clone(),
            SubscribeEvent::Done { exit_code: Some(0) },
            Some(123),
        );
        assert!(app.external_console_lines.is_empty());
        assert_eq!(app.step_output[&addr].duration_ms, 123);
        app.on_reconciled_run_event(addr.clone(), line("live backlog"), None);
        app.on_external_run_event(addr.clone(), line("next line"));
        app.on_external_run_event(addr.clone(), SubscribeEvent::Done { exit_code: Some(0) });
        assert_eq!(
            app.external_console_lines,
            vec![
                "==> root/build",
                "live backlog",
                "next line",
                "==> root/build (exit 0)"
            ]
        );
        assert!(app.step_output[&addr].text.contains("live backlog"));
        assert!(!app.step_output[&addr].text.contains("old output"));
    }

    #[tokio::test]
    async fn passive_output_before_foreground_step_does_not_leave_a_spinner() {
        use crate::worker_client::{RunEvent, SubscribeEvent};
        let mut app = app_with_blocks("spinner-race", "```sh name=build\necho build\n```\n").await;
        let addr = BlockAddr::new("root", "build");
        let (_tx, rx) = tokio::sync::mpsc::unbounded_channel();
        app.run = Some(RunState {
            chain: Vec::new(),
            idx: 0,
            http_rx: Some(rx),
            lines: Vec::new(),
            full_output: String::new(),
            stdout_only: String::new(),
            stderr_only: String::new(),
            output_markdown: false,
            step_started: std::time::Instant::now(),
            had_failure: false,
            killed: false,
            finished: false,
        });
        app.on_external_run_event(
            addr.clone(),
            SubscribeEvent::Line {
                stream: meshfox_server::stream_exec::OutputStream::Stdout,
                text: "build".into(),
            },
        );
        assert!(app.external_running.contains_key(&addr));
        app.on_run_event(Some(RunEvent::StepStart {
            node_id: "root".into(),
            block: "build".into(),
        }))
        .await;
        assert!(!app.external_running.contains_key(&addr));
        assert!(app.spinner_active());
        for (stream, text) in [
            (meshfox_server::stream_exec::OutputStream::Stderr, "waiting"),
            (
                meshfox_server::stream_exec::OutputStream::Stdout,
                "wrote files",
            ),
            (meshfox_server::stream_exec::OutputStream::Stderr, "warning"),
        ] {
            app.on_run_event(Some(RunEvent::Output {
                node_id: "root".into(),
                block: "build".into(),
                stream,
                text: text.into(),
                child: None,
            }))
            .await;
        }
        assert_eq!(
            app.step_output[&addr].text,
            "waiting\nwrote files\nwarning\n"
        );
        app.on_run_event(Some(RunEvent::StepEnd {
            node_id: "root".into(),
            block: "build".into(),
            exit_code: 0,
            duration_ms: 10,
        }))
        .await;
        app.on_external_run_event(addr.clone(), SubscribeEvent::Done { exit_code: Some(0) });
        app.on_run_event(Some(RunEvent::Done { exit_code: 0 }))
            .await;
        assert!(!app.step_output[&addr].running);
        assert_eq!(
            app.step_output[&addr].text,
            "waiting\nwrote files\nwarning\n"
        );
        assert!(!app.spinner_active());
    }

    #[tokio::test]
    async fn skipped_steps_do_not_replay_old_logs_in_the_tui() {
        let dir = std::env::temp_dir().join(format!("meshfox-tui-skipped-{}", uuid_like()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("canvas.canvas.md");
        std::fs::write(&path, "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n```sh name=build\necho build\n```\n").unwrap();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(path, tx, None, None).await.unwrap();
        app.run = Some(RunState {
            chain: Vec::new(),
            idx: 0,
            http_rx: None,
            lines: Vec::new(),
            full_output: String::new(),
            stdout_only: String::new(),
            stderr_only: String::new(),
            output_markdown: false,
            step_started: std::time::Instant::now(),
            had_failure: false,
            killed: false,
            finished: false,
        });
        app.on_run_event(Some(crate::worker_client::RunEvent::StepStart {
            node_id: "root".into(),
            block: "build".into(),
        }))
        .await;
        app.on_run_event(Some(crate::worker_client::RunEvent::StepSkipped {
            node_id: "root".into(),
            block: "build".into(),
            output: "old npm/vite log".into(),
            duration_ms: 50_800,
        }))
        .await;
        let transcript = app.run.as_ref().unwrap().lines.join("\n");
        assert!(transcript.contains("skipped"));
        assert!(!transcript.contains("old npm/vite log"));
        assert!(!transcript.contains("50.8"));
        assert!(!app
            .step_output
            .contains_key(&BlockAddr::new("root", "build")));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn extra_edge_row_follows_the_real_target_on_enter() {
        let dir = std::env::temp_dir().join(format!("meshfox-tui-link-row-{}", uuid_like()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("canvas.canvas.md");
        std::fs::write(
            &path,
            concat!(
                "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n",
                "## Source\n<!-- meshfox:node id=\"source\" -->\n",
                "## Target\n<!-- meshfox:node id=\"target\" -->\n",
                "<!-- meshfox:edge from=\"source\" label=\"uses worker\" -->\n",
                "Target body.\n",
            ),
        )
        .unwrap();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(path, tx, None, None).await.unwrap();
        app.jump_to_node("source");
        app.toggle_expand();
        let reference_index = app
            .rows
            .iter()
            .position(|row| row.reference.is_some())
            .unwrap();
        app.selected = reference_index;
        app.rebuild_rows();
        assert_eq!(app.selected, reference_index); // reload preserves the link row
        app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .await;
        assert_eq!(app.rows[app.selected].node_id, "target");
        assert!(app.rows[app.selected].reference.is_none());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn spatial_selection_uses_all_four_directions_instead_of_file_order() {
        let dir = std::env::temp_dir().join(format!("meshfox-spatial-nav-{}", uuid_like()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("canvas.canvas.md");
        std::fs::write(
            &path,
            concat!(
                "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n",
                "## Diagram\n<!-- meshfox:node id=\"group\" -->\n",
                "### Bottom\n<!-- meshfox:node id=\"bottom\" x=0 y=200 w=100 h=80 -->\n",
                "### Top right\n<!-- meshfox:node id=\"right\" x=200 y=0 w=100 h=80 -->\n",
                "### Top left\n<!-- meshfox:node id=\"left\" x=0 y=0 w=100 h=80 -->\n",
                "## After diagram\n<!-- meshfox:node id=\"after\" -->\n",
            ),
        )
        .unwrap();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(path, tx, None, None).await.unwrap();
        app.jump_to_node("group");
        app.expanded.insert("group".to_string());
        app.rebuild_rows();
        app.on_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))
            .await;
        assert_eq!(app.rows[app.selected].node_id, "left");
        app.on_key(KeyEvent::new(KeyCode::Char('l'), KeyModifiers::NONE))
            .await;
        assert_eq!(app.rows[app.selected].node_id, "right");
        app.on_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE))
            .await;
        assert_eq!(app.rows[app.selected].node_id, "bottom");
        app.on_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE))
            .await;
        assert_eq!(app.rows[app.selected].node_id, "left");
        app.on_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE))
            .await;
        assert_eq!(app.rows[app.selected].node_id, "right");
        app.on_key(KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE))
            .await;
        assert_eq!(app.rows[app.selected].node_id, "left");
        app.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
            .await;
        assert_eq!(app.rows[app.selected].node_id, "group");
        assert!(!app.expanded.contains("group"));
        assert!(spatial::active_spatial_parent(&app).is_none());
        assert!(!app.rows.iter().any(|row| row.node_id == "left"));
        app.move_selection(1);
        assert_eq!(app.rows[app.selected].node_id, "after");
        assert!(!app.should_quit);
        app.jump_to_node("right");
        app.on_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE))
            .await;
        assert_eq!(app.rows[app.selected].node_id, "group");
        assert!(!app.expanded.contains("group"));
        assert!(spatial::active_spatial_parent(&app).is_none());
        assert!(!app.rows.iter().any(|row| row.node_id == "right"));
        app.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
            .await;
        assert!(!app.should_quit, "Esc in the ordinary tree must not quit");
        app.focus = Focus::Document;
        app.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
            .await;
        assert!(!app.should_quit, "Esc in the document must not quit");
        app.fullscreen = Some(Focus::Document);
        app.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
            .await;
        assert!(app.fullscreen.is_none());
        assert!(!app.should_quit, "Esc leaving fullscreen must not quit");
        app.on_key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE))
            .await;
        assert!(app.should_quit, "q remains the explicit quit key");
        let _ = std::fs::remove_dir_all(dir);
    }

    // TODO.canvas.md: "Base64 image" — `decode_data_url_image`/
    // `load_image_protocol`'s own `data:` branch. `Picker::halfblocks()`
    // needs no real terminal (see its own construction above in
    // `App::new`), so this runs fine in CI, same as everywhere else in
    // this file that already relies on it.
    const ONE_PIXEL_PNG_DATA_URL: &str = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=";

    #[test]
    fn svg_iterm_output_keeps_retina_detail_without_enlarging_cell_placement() {
        let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="200" height="100"><rect width="200" height="100" fill="white"/><path d="M0 0 L200 100" stroke="black" stroke-width="0.5"/><text x="10" y="50" font-size="10">GPIO22 SCL</text></svg>"##;
        let mut picker = Picker::halfblocks();
        picker.set_protocol_type(ProtocolType::Iterm2);
        let data_url = format!(
            "data:image/svg+xml;base64,{}",
            base64::Engine::encode(&base64::engine::general_purpose::STANDARD, svg.as_bytes(),)
        );
        for (path, bytes) in [
            (Path::new("diagram.svg"), Some(svg.as_bytes())),
            (Path::new(&data_url), None),
        ] {
            let protocol = load_image_protocol(&mut picker, path, bytes, None, None, None).unwrap();
            assert_eq!((protocol.size().width, protocol.size().height), (56, 14));
            let Protocol::ITerm2(image) = protocol else {
                panic!("iTerm2 expected")
            };
            assert!(image
                .data
                .contains(";width=56;height=14;doNotMoveCursor=1:"));
            let png = image.data.split_once("doNotMoveCursor=1:").unwrap().1;
            let png = png.split('\x07').next().unwrap();
            let bytes =
                base64::Engine::decode(&base64::engine::general_purpose::STANDARD, png).unwrap();
            let decoded = image::load_from_memory(&bytes).unwrap();
            assert_eq!((decoded.width(), decoded.height()), (1120, 560));
        }
    }

    #[test]
    fn image_protocol_environment_overrides_config_including_auto() {
        assert_eq!(
            select_image_protocol(None, Some("sixel")).unwrap(),
            Some(ProtocolType::Sixel)
        );
        assert_eq!(
            select_image_protocol(Some("iterm2"), Some("sixel")).unwrap(),
            Some(ProtocolType::Iterm2)
        );
        assert_eq!(
            select_image_protocol(Some("auto"), Some("sixel")).unwrap(),
            None
        );
        assert_eq!(
            select_image_protocol(Some("halfblocks"), Some("invalid")).unwrap(),
            Some(ProtocolType::Halfblocks)
        );
        assert!(select_image_protocol(Some("invalid"), Some("iterm2")).is_err());
        assert_eq!(select_image_protocol(None, None).unwrap(), None);
    }

    #[test]
    fn image_protocol_override_parses_modes_and_preserves_font_metrics() {
        for value in [None, Some(""), Some("auto")] {
            assert_eq!(parse_image_protocol(value).unwrap(), None);
        }
        for (value, protocol) in [
            ("sixel", ProtocolType::Sixel),
            ("iterm2", ProtocolType::Iterm2),
            ("kitty", ProtocolType::Kitty),
            ("halfblocks", ProtocolType::Halfblocks),
        ] {
            let forced = parse_image_protocol(Some(value)).unwrap();
            assert_eq!(forced, Some(protocol));
            let picker = Picker::halfblocks();
            let metrics = picker.font_size();
            let picker = apply_image_protocol(picker, forced);
            assert_eq!(picker.protocol_type(), protocol);
            assert_eq!(picker.font_size().width, metrics.width);
            assert_eq!(picker.font_size().height, metrics.height);
            let image = load_image_protocol(
                &mut picker.clone(),
                Path::new(ONE_PIXEL_PNG_DATA_URL),
                None,
                None,
                None,
                None,
            )
            .unwrap();
            assert!(matches!(
                (protocol, image),
                (ProtocolType::Sixel, Protocol::Sixel(_))
                    | (ProtocolType::Iterm2, Protocol::ITerm2(_))
                    | (ProtocolType::Kitty, Protocol::Kitty(_))
                    | (ProtocolType::Halfblocks, Protocol::Halfblocks(_))
            ));
        }
        let error = parse_image_protocol(Some("sixle")).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert!(error.to_string().contains("MESHFOX_IMAGE_PROTOCOL"));
        let mut picker = Picker::halfblocks();
        picker.set_protocol_type(ProtocolType::Sixel);
        assert_eq!(
            apply_image_protocol(picker, None).protocol_type(),
            ProtocolType::Sixel
        );
    }

    #[test]
    fn iterm_image_query_does_not_allow_sixel_to_override_native_protocol() {
        use ratatui_image::picker::cap_parser::Parser;
        let query = Parser::query(false, image_query_options(Some("iTerm.app")));
        assert!(
            !query.contains("\x1b[c"),
            "Sixel detection overrides the iTerm2 hint"
        );
        assert!(
            !query.contains("\x1b_G"),
            "Kitty placeholders are unsupported"
        );
        assert!(query.contains("\x1b[16t"), "still query font metrics");
        for program in [None, Some("kitty"), Some("WezTerm")] {
            let query = Parser::query(false, image_query_options(program));
            assert!(query.contains("\x1b[c"));
            assert!(query.contains("\x1b_G"));
        }
    }

    #[test]
    #[allow(deprecated)] // Explicit metrics exercise Retina scaling without querying a terminal.
    fn iterm_image_uses_reserved_cells_instead_of_retina_pixels() {
        let mut picker = Picker::from_fontsize(ratatui_image::FontSize::new(20, 40));
        picker.set_protocol_type(ProtocolType::Iterm2);
        let original = picker
            .new_protocol(
                image::DynamicImage::new_rgb8(560, 520),
                image_size_budget(None, None),
                ratatui_image::Resize::Fit(None),
            )
            .unwrap();
        let size = original.size();
        let Protocol::ITerm2(original_image) = &original else {
            panic!("iTerm2")
        };
        let payload = original_image
            .data
            .split_once("doNotMoveCursor=1:")
            .unwrap()
            .1
            .to_owned();
        let Protocol::ITerm2(fixed) = image_cell_dimensions(original) else {
            panic!("iTerm2")
        };
        assert!(fixed.data.contains(&format!(
            ";width={};height={};doNotMoveCursor=1:",
            size.width, size.height
        )));
        assert_eq!(
            fixed.data.split_once("doNotMoveCursor=1:").unwrap().1,
            payload
        );
        assert_eq!(fixed.size, size);
    }

    #[tokio::test]
    #[allow(deprecated)] // Explicit metrics must survive the nested constructor unchanged.
    async fn nested_canvas_inherits_image_protocol_and_font_metrics() {
        let dir = std::env::temp_dir().join(format!("meshfox-picker-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test.canvas.md");
        std::fs::write(&path, "# Root\n<!-- meshfox:node id=\"root\" -->\n").unwrap();
        let mut picker = Picker::from_fontsize(ratatui_image::FontSize::new(20, 40));
        picker.set_protocol_type(ProtocolType::Iterm2);
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let child = App::new_with_picker(path, tx, None, None, picker.clone())
            .await
            .unwrap();
        assert_eq!(child.picker.protocol_type(), picker.protocol_type());
        assert_eq!(child.picker.font_size().width, picker.font_size().width);
        assert_eq!(child.picker.font_size().height, picker.font_size().height);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn decode_data_url_image_decodes_a_valid_base64_png() {
        let img = decode_data_url_image(ONE_PIXEL_PNG_DATA_URL, None).expect("valid data: URL");
        assert_eq!((img.width(), img.height()), (1, 1));
    }

    #[test]
    fn decode_data_url_image_rejects_a_non_base64_data_url() {
        assert!(decode_data_url_image("data:image/png,not-base64-payload", None).is_none());
    }

    #[test]
    fn decode_data_url_image_rejects_a_garbage_payload() {
        assert!(decode_data_url_image("data:image/png;base64,not valid base64!!!", None).is_none());
    }

    #[test]
    fn load_image_protocol_loads_a_data_url_without_touching_disk() {
        let mut picker = Picker::halfblocks();
        let path = PathBuf::from(ONE_PIXEL_PNG_DATA_URL);
        assert!(load_image_protocol(&mut picker, &path, None, None, None, None).is_some());
    }

    const SVG_BASE64_DATA_URL: &str = "data:image/svg+xml;base64,PHN2ZyB4bWxucz0iaHR0cDovL3d3dy53My5vcmcvMjAwMC9zdmciIHdpZHRoPSI0IiBoZWlnaHQ9IjQiPjxyZWN0IHdpZHRoPSI0IiBoZWlnaHQ9IjQiLz48L3N2Zz4=";

    #[test]
    fn decode_data_url_image_rasterizes_an_svg() {
        let img = decode_data_url_image(SVG_BASE64_DATA_URL, None).expect("valid svg data: URL");
        // A 4x4 SVG, scaled up by the rasterizer's own 8x cap.
        assert_eq!((img.width(), img.height()), (32, 32));
    }

    #[test]
    fn decode_data_url_image_rejects_an_svg_data_url_with_a_garbage_payload() {
        // Valid base64, not an SVG.
        assert!(decode_data_url_image("data:image/svg+xml;base64,aGVsbG8=", None).is_none());
    }

    #[test]
    fn load_image_protocol_loads_an_svg_data_url_and_an_svg_file() {
        let mut picker = Picker::halfblocks();
        let path = PathBuf::from(SVG_BASE64_DATA_URL);
        assert!(load_image_protocol(
            &mut picker,
            &path,
            None,
            None,
            None,
            Background::parse("#fff")
        )
        .is_some());

        let dir = std::env::temp_dir().join(format!("meshfox-svg-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("pic.SVG");
        std::fs::write(
            &file,
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="4" height="4"><rect width="4" height="4"/></svg>"#,
        )
        .unwrap();
        let bytes = std::fs::read(&file).unwrap();
        assert!(load_image_protocol(&mut picker, &file, Some(&bytes), None, None, None).is_some());
        let _ = std::fs::remove_dir_all(&dir);
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
    fn image_percentages_use_the_pane_and_release_the_other_axis() {
        use ratatui::layout::Size;
        assert_eq!(
            image_size_budget_for_viewport(Size::new(120, 60), Some(100), None),
            Size::new(120, 60)
        );
        assert_eq!(
            image_size_budget_for_viewport(Size::new(120, 60), Some(50), None),
            Size::new(60, 60)
        );
        assert_eq!(
            image_size_budget_for_viewport(Size::new(120, 60), None, Some(50)),
            Size::new(120, 30)
        );
        assert_eq!(
            image_size_budget_for_viewport(Size::new(120, 60), None, None),
            Size::new(56, 24)
        );
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

    /// End-to-end: `start_run`/`advance_run`/`on_output_line` finding and
    /// running a block that lives inside an `include` node's own dumped-in
    /// body — same limitation `crates/server/src/lib.rs`'s own chain-
    /// execution loops and `crates/cli/tests/run_cmd.rs` used to have (a
    /// block only reachable through an `include` was simply unaddressable
    /// — `self.canvas` is deliberately never `include`-resolved, see its
    /// own doc comment) before `meshfox_core::include::resolve` was wired
    /// into `advance_run`. Never cached, even with `cache` set on the
    /// fence — the include node's real on-disk body is just the bare link,
    /// nowhere to write a `meshfox:output` comment without clobbering it
    /// (same reasoning `crates/server/src/lib.rs`'s own chain loops guard
    /// against — see their `plain_markdown_include` check).
    // `runs_a_block_inside_an_included_node_without_caching` and
    // `trigger_run_reaches_a_block_inside_an_included_node` used to live
    // here, driving a run through the local (no-worker) execution path
    // directly — removed along with that path (see TODO.canvas.md's
    // "Category C" note on `advance_run`/`start_run_local` being
    // unreachable in a real build). The first scenario ("run a block
    // inside an included node without caching") is still covered,
    // worker-routed, by `crates/cli/tests/run_cmd.rs`'s
    // `run_finds_and_reports_the_cwd_of_a_block_inside_an_included_node_
    // without_caching`. The second (`trigger_run`'s own `display_canvas`
    // vs `canvas` node-lookup guard for an include-dumped row) has no
    // worker-routed equivalent yet — restoring it would need a real
    // worker in this test (or a pty e2e test under `tui_e2e/`), left for
    // the separate TUI-focused pass already tracked in TODO.canvas.md.

    #[tokio::test]
    async fn reset_session_clears_every_submitted_form_value() {
        let dir =
            std::env::temp_dir().join(format!("meshfox-tui-reset-session-test-{}", uuid_like()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("canvas.md");
        std::fs::write(
            &path,
            "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n",
        )
        .unwrap();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(path, tx, None, None).await.unwrap();

        app.session_vars
            .insert("greeting".to_string(), "hi".to_string());
        assert!(!app.session_vars.is_empty());

        app.reset_session().await;

        assert!(app.session_vars.is_empty());
        assert_eq!(app.status, "session reset");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A one-shot HTTP server that answers 204 to whatever it is sent and
    /// reports the request line of the first request — enough of a "worker"
    /// to see what the TUI asks it.
    /// A real embedded worker for `path` (never the user's daemon: the lock
    /// is taken directly, no `server_socket` lookup) and an `App` talking to it.
    async fn app_with_real_worker(path: PathBuf) -> App {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let canonical = path.canonicalize().unwrap();
        let guard = match meshfox_core::worker_lock::try_acquire(&canonical).unwrap() {
            meshfox_core::worker_lock::Acquired::Us(guard) => guard,
            meshfox_core::worker_lock::Acquired::Other { .. } => {
                panic!("test canvas already served")
            }
        };
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
        tokio::spawn(meshfox_server::serve_as_worker(
            canonical,
            0,
            false,
            None,
            true,
            guard,
            Some(ready_tx),
        ));
        let port = ready_rx.await.unwrap();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        App::new(path, tx, None, Some(port)).await.unwrap()
    }

    async fn stub_worker() -> (u16, tokio::sync::oneshot::Receiver<String>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let (tx, rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 4096];
            let n = socket.read(&mut buf).await.unwrap();
            let request = String::from_utf8_lossy(&buf[..n]).to_string();
            let _ = socket
                .write_all(b"HTTP/1.1 204 No Content\r\nconnection: close\r\n\r\n")
                .await;
            let _ = tx.send(request.lines().next().unwrap_or_default().to_string());
        });
        (port, rx)
    }

    // On a canvas served read-only, `e` (the source editor) and `H` (history
    // jumps) — the two ways into the file from the TUI — do nothing but say
    // why, so a write that can only be refused is never started.
    #[tokio::test]
    async fn a_read_only_canvas_opens_neither_the_source_editor_nor_the_history_view() {
        let dir = std::env::temp_dir().join(format!("meshfox-tui-read-only-test-{}", uuid_like()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("canvas.md");
        std::fs::write(
            &path,
            "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n",
        )
        .unwrap();
        let mut app = app_with_real_worker(path).await;
        app.read_only = true;

        app.on_key(key(KeyCode::Char('e'))).await;
        assert!(app.source_editor.is_none());
        assert!(app.status.contains("read-only"), "status: {}", app.status);

        app.status.clear();
        app.on_key(key(KeyCode::Char('H'))).await;
        assert!(app.history_view.is_none());
        assert!(app.status.contains("read-only"), "status: {}", app.status);

        // Writable, `e` opens the editor — the guard is the read-only flag, not the key.
        app.read_only = false;
        app.on_key(key(KeyCode::Char('e'))).await;
        assert!(app.source_editor.is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }

    // The bug this guards: `S` in a worker-routed TUI only cleared the TUI's
    // own state, so the worker still considered every dependency fresh and
    // the next `r` ran only the block itself.
    #[tokio::test]
    async fn reset_session_also_resets_the_workers_session_when_there_is_one() {
        let dir =
            std::env::temp_dir().join(format!("meshfox-tui-reset-worker-test-{}", uuid_like()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("canvas.md");
        std::fs::write(
            &path,
            "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n",
        )
        .unwrap();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(path, tx, None, None).await.unwrap();
        // `main` installs the rustls provider reqwest needs; a test has to.
        let _ = rustls::crypto::ring::default_provider().install_default();
        let (port, request) = stub_worker().await;
        app.worker_port = Some(port);

        app.on_key(key(KeyCode::Char('S'))).await;
        app.on_key(key(KeyCode::Char('y'))).await;

        assert_eq!(request.await.unwrap(), "POST /api/session/reset HTTP/1.1");
        assert_eq!(app.status, "session reset");

        // A worker that can't be reached is reported, not silently ignored.
        let dead = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap().port()
        };
        app.worker_port = Some(dead);
        app.reset_session().await;
        assert!(
            app.status.contains("worker didn't reset"),
            "status was {:?}",
            app.status
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn secret_form_statuses() -> Vec<crate::worker_client::VarStatus> {
        serde_json::from_value(serde_json::json!([
            {"name": "USER", "type": "string", "prompt": "USER", "secret": false, "resolved": false},
            {"name": "TOKEN", "type": "string", "prompt": "TOKEN", "secret": true, "resolved": false,
             "secretStore": "keychain", "secretError": "keychain: locked"}
        ]))
        .unwrap()
    }

    #[test]
    fn var_form_from_statuses_carries_the_store_label_and_read_errors() {
        let vf = var_form_from_statuses(secret_form_statuses());
        assert_eq!(vf.secret_store, "keychain");
        assert_eq!(vf.errors, vec![None, Some("keychain: locked".to_string())]);
        assert_eq!(vf.save, vec![false, false]);
    }

    #[tokio::test]
    async fn ctrl_d_marks_a_stored_secret_for_clearing_in_configure_only() {
        let dir = std::env::temp_dir().join(format!("meshfox-tui-clear-secret-{}", uuid_like()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("canvas.md");
        std::fs::write(
            &path,
            "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n",
        )
        .unwrap();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(path, tx, None, None).await.unwrap();
        let statuses: Vec<crate::worker_client::VarStatus> = serde_json::from_value(serde_json::json!([
            {"name": "TOKEN", "type": "string", "prompt": "TOKEN", "secret": true, "resolved": true,
             "secretStore": "keychain", "stored": true},
            {"name": "OTHER", "type": "string", "prompt": "OTHER", "secret": true, "resolved": false,
             "secretStore": "keychain", "stored": false}
        ]))
        .unwrap();
        let mut form = var_form_from_statuses(statuses);
        assert_eq!(form.stored, vec![Some(true), Some(false)]);
        let ctrl_d = KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL);

        // Outside configure the key does nothing (and isn't typed).
        app.var_form = Some(form);
        app.on_var_form_key(ctrl_d).await;
        assert_eq!(app.var_form.as_ref().unwrap().clear, vec![false, false]);

        form = app.var_form.take().unwrap();
        form.configuring = true;
        app.var_form = Some(form);
        app.on_var_form_key(ctrl_d).await;
        assert_eq!(app.var_form.as_ref().unwrap().clear, vec![true, false]);
        // Nothing stored under OTHER, so nothing to clear.
        app.on_var_form_key(key(KeyCode::Down)).await;
        app.on_var_form_key(ctrl_d).await;
        assert_eq!(app.var_form.as_ref().unwrap().clear, vec![true, false]);
    }

    #[tokio::test]
    async fn ctrl_s_toggles_save_on_a_secret_field_only() {
        let dir =
            std::env::temp_dir().join(format!("meshfox-tui-save-secret-test-{}", uuid_like()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("canvas.md");
        std::fs::write(
            &path,
            "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n",
        )
        .unwrap();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(path, tx, None, None).await.unwrap();
        app.var_form = Some(var_form_from_statuses(secret_form_statuses()));
        let ctrl_s = KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL);

        // Field 0 (USER) isn't a secret: nothing to save, and the keypress
        // isn't typed into it either.
        app.on_var_form_key(ctrl_s).await;
        let vf = app.var_form.as_ref().unwrap();
        assert_eq!(vf.save, vec![false, false]);
        assert_eq!(vf.inputs[0], "");

        app.on_var_form_key(key(KeyCode::Down)).await;
        app.on_var_form_key(ctrl_s).await;
        assert_eq!(app.var_form.as_ref().unwrap().save, vec![false, true]);
        app.on_var_form_key(ctrl_s).await;
        assert_eq!(app.var_form.as_ref().unwrap().save, vec![false, false]);
    }

    #[tokio::test]
    async fn a_finished_tty_run_is_shown_under_its_block_and_dropped_when_it_goes_stale() {
        let mut app = app_with_blocks(
            "root",
            concat!(
                "```bash name=\"term\" tty\nsh\n```\n\n",
                "```bash name=\"plain\"\necho hi\n```\n",
            ),
        )
        .await;
        let run = |block: &str, kind: &str, status: &str, code: Option<i32>| {
            crate::worker_client::ActiveRunDto {
                history_id: None,
                node_id: "root".into(),
                block: block.into(),
                kind: kind.into(),
                status: status.into(),
                exit_code: code,
                uptime_ms: 2800,
            }
        };
        let addr = BlockAddr::new("root", "term");

        app.block_runs = Some(vec![run("term", "tty", "exited", Some(0))]);
        app.sync_tty_results();
        let entry = &app.step_output[&addr];
        assert_eq!(
            (entry.exit_code, entry.duration_ms, entry.running),
            (0, 2800, false)
        );
        assert!(entry.stdout.is_empty() && entry.stderr.is_empty());
        // ...and the Document pane actually draws it under the block.
        app.selected = app.rows.iter().position(|r| r.node_id == "root").unwrap();
        app.render_current_document();
        let drawn = |app: &App| -> String {
            app.doc_segments
                .iter()
                .filter_map(|seg| match seg {
                    Segment::Text(lines) => Some(lines),
                    _ => None,
                })
                .flatten()
                .map(|l| {
                    l.spans
                        .iter()
                        .map(|s| s.content.as_ref())
                        .collect::<String>()
                })
                .collect::<Vec<_>>()
                .join("\n")
        };
        let text = drawn(&app);
        assert!(text.contains("output: term · live · done · 2.8s"), "{text}");

        // A newer failing run replaces it.
        app.block_runs = Some(vec![run("term", "tty", "exited", Some(3))]);
        app.sync_tty_results();
        assert_eq!(app.step_output[&addr].exit_code, 3);
        assert!(
            drawn(&app).contains("output: term · live · failed"),
            "{}",
            drawn(&app)
        );

        // A plain block's entry is never touched, and a tty one whose run is
        // no longer current (edited / session reset) disappears.
        app.step_output.insert(
            BlockAddr::new("root", "plain"),
            StepOutput {
                text: "hi\n".into(),
                stdout: "hi\n".into(),
                stderr: String::new(),
                output_markdown: false,
                exit_code: 0,
                duration_ms: 1,
                running: false,
            },
        );
        app.block_runs = Some(Vec::new());
        app.sync_tty_results();
        assert!(!app.step_output.contains_key(&addr));
        assert!(app
            .step_output
            .contains_key(&BlockAddr::new("root", "plain")));
    }

    #[tokio::test]
    async fn the_failed_badge_follows_the_workers_latest_current_run_per_block() {
        let dir = std::env::temp_dir().join(format!("meshfox-tui-failed-badge-{}", uuid_like()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("canvas.md");
        std::fs::write(
            &path,
            "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n",
        )
        .unwrap();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(path, tx, None, None).await.unwrap();
        let run = |node: &str, kind: &str, status: &str, exit_code: Option<i32>| {
            crate::worker_client::ActiveRunDto {
                history_id: None,
                node_id: node.into(),
                block: "b".into(),
                kind: kind.into(),
                status: status.into(),
                exit_code,
                uptime_ms: 1,
            }
        };

        // Nothing is flagged before the first fetch, whatever this session saw.
        app.step_output.insert(
            BlockAddr::new("session-only", "b"),
            StepOutput {
                text: String::new(),
                stdout: String::new(),
                stderr: String::new(),
                output_markdown: false,
                exit_code: 1,
                duration_ms: 0,
                running: false,
            },
        );
        assert!(app.failed_node_ids().is_empty());

        // After it, only the worker's latest current runs count: an old
        // failure that isn't listed any more (fixed, edited or reset) is gone,
        // and this session's own `step_output` no longer matters.
        app.block_runs = Some(vec![
            run("plain-failed", "plain", "exited", Some(2)),
            run("plain-ok", "plain", "exited", Some(0)),
            run("tty-failed", "tty", "exited", Some(1)),
            run("tty-killed", "tty", "killed", None),
            run("running", "plain", "running", None),
        ]);
        let failed = app.failed_node_ids();
        assert_eq!(
            failed,
            ["plain-failed", "tty-failed", "tty-killed"]
                .into_iter()
                .collect()
        );
    }

    #[tokio::test]
    async fn history_view_navigates_clamps_and_closes_without_a_worker() {
        let dir =
            std::env::temp_dir().join(format!("meshfox-tui-history-view-test-{}", uuid_like()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("canvas.md");
        std::fs::write(
            &path,
            "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n",
        )
        .unwrap();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(path, tx, None, None).await.unwrap();

        // No worker: `H` reports it instead of opening anything.
        app.worker_port = None;
        app.on_key(key(KeyCode::Char('H'))).await;
        assert!(app.history_view.is_none());
        assert!(app.status.contains("worker"));

        let entry = |seq| crate::worker_client::HistoryEntryDto {
            seq,
            created_at: "2026-09-29T12:00:00Z".into(),
            op_kind: "node_body".into(),
            applied: true,
            summary: String::new(),
        };
        app.history_view = Some(HistoryViewState {
            entries: vec![entry(3), entry(2), entry(1)],
            selected: 0,
            ..Default::default()
        });
        app.on_key(key(KeyCode::Up)).await;
        assert_eq!(app.history_view.as_ref().unwrap().selected, 0);
        app.on_key(key(KeyCode::Char('j'))).await;
        app.on_key(key(KeyCode::PageDown)).await;
        assert_eq!(app.history_view.as_ref().unwrap().selected, 2);
        // The modal claims the keymap: `q` closes it rather than quitting.
        app.on_key(key(KeyCode::Char('q'))).await;
        assert!(app.history_view.is_none());
    }

    fn run_entry(id: i64, exit_code: i32, stale: bool) -> crate::worker_client::RunHistoryEntryDto {
        crate::worker_client::RunHistoryEntryDto {
            id,
            outcome: "exited".into(),
            exit_code: Some(exit_code),
            started_at: "2026-09-30T12:00:00Z".into(),
            duration_ms: Some(120),
            stale,
        }
    }

    #[test]
    fn argument_form_offers_valid_bool_and_select_values_without_persisting_them() {
        let fields = serde_json::from_value(serde_json::json!([
            {"name":"flag", "type":"bool", "prompt":"flag", "resolved":false},
            {"name":"lang", "type":"select", "prompt":"lang", "choices":["en", "hy"], "resolved":false},
        ])).unwrap();
        let form = argument_form_from_statuses(fields);
        assert!(form.arguments);
        assert_eq!(form.inputs, ["false", "en"]);
        assert!(form.save.iter().all(|save| !save));
    }

    #[tokio::test]
    async fn parameterized_tty_targets_and_forwarded_dependencies_select_the_terminal() {
        let app = app_with_blocks(
            "arg-tty",
            concat!(
                "<!-- meshfox:arg name=\"lang\" type=\"select\" choices=\"en,hy\" -->\n",
                "```bash name=\"extract\" tty autoclose\necho ok\n```\n",
                "```button name=\"launch\" deps=\"extract[lang=hy]\"\nRun\n```\n",
            ),
        )
        .await;
        assert_eq!(
            app.target_chain_tty_autoclose("root", "extract[lang=hy]", true),
            Some(true)
        );
        assert_eq!(
            app.target_chain_tty_autoclose("root", "launch", true),
            Some(true)
        );
    }

    fn document_text(app: &App) -> String {
        app.doc_segments
            .iter()
            .filter_map(|seg| match seg {
                Segment::Text(lines) => Some(
                    lines
                        .iter()
                        .map(Line::to_string)
                        .collect::<Vec<_>>()
                        .join("\n"),
                ),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[tokio::test]
    async fn a_form_is_framed_selectable_with_brackets_and_enter_edits_it() {
        let mut app = app_with_blocks(
            "form-select",
            concat!(
                "<!-- meshfox:var name=\"IN\" type=\"file\" session -->\n\n",
                "```sh name=first\necho one\n```\n\n",
                "```form name=pick\nfield var=\"IN\" label=\"Input\"\n```\n\n",
                "```sh name=last\necho two\n```\n",
            ),
        )
        .await;
        app.set_focus(Focus::Document);
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        let text = |app: &App| document_text(app);
        // Framed like a code block, unselected: plain corner, gutter, foot,
        // and the Browse marker after the file field.
        assert!(text(&app).contains("┌─ form · pick"), "{}", text(&app));
        assert!(text(&app).contains("│ Input: "));
        assert!(text(&app).contains("[Browse… ^o]"));
        assert!(!text(&app).contains("›─ form"));

        // `]` walks document order: first -> pick -> last -> (wraps) first.
        app.on_key(key(KeyCode::Char(']'))).await;
        assert_eq!(app.selected_block, Some(BlockAddr::new("root", "first")));
        app.on_key(key(KeyCode::Char(']'))).await;
        assert_eq!(app.selected_block, Some(BlockAddr::new("root", "pick")));
        // Selected + Document focused: `›` replaces the corner.
        assert!(text(&app).contains("›─ form · pick"), "{}", text(&app));
        assert!(!text(&app).contains("┌─ form · pick"));
        // The other blocks' frames are untouched by the selection.
        assert_eq!(
            text(&app).matches("┌─[").count(),
            2,
            "the two code blocks keep their plain corners: {}",
            text(&app)
        );

        // Selection is dimmed (no `›`) while another pane has focus, but kept.
        app.set_focus(Focus::Tree);
        app.render_current_document();
        assert_eq!(app.selected_block, Some(BlockAddr::new("root", "pick")));
        assert!(text(&app).contains("┌─ form · pick"), "{}", text(&app));
        app.set_focus(Focus::Document);
        app.render_current_document();

        // `r` on a form says so instead of trying to run it.
        app.on_key(key(KeyCode::Char('r'))).await;
        assert!(app.status.contains("isn't run"), "{}", app.status);

        // Enter edits it: the header says so and the foot lists the keys.
        app.on_key(key(KeyCode::Enter)).await;
        let form = app
            .active_inline_form
            .as_ref()
            .expect("Enter opens the form");
        assert!(form.editing);
        assert_eq!(form.block_name, "pick");
        assert!(text(&app).contains("· editing"), "{}", text(&app));
        assert!(text(&app).contains("└─ ↑/↓ field · enter apply · esc done"));
        app.on_key(key(KeyCode::Esc)).await;
        assert!(!app.active_inline_form.as_ref().unwrap().editing);
        assert!(!text(&app).contains("· editing"));
        // Esc only stopped editing; the form is still the selected block.
        assert_eq!(app.selected_block, Some(BlockAddr::new("root", "pick")));

        app.on_key(key(KeyCode::Char(']'))).await;
        assert_eq!(app.selected_block, Some(BlockAddr::new("root", "last")));
        assert!(text(&app).contains("┌─ form · pick"), "{}", text(&app));
        app.on_key(key(KeyCode::Char(']'))).await;
        assert_eq!(app.selected_block, Some(BlockAddr::new("root", "first")));
        // And `[` goes back the other way, through the form.
        app.on_key(key(KeyCode::Char('['))).await;
        assert_eq!(app.selected_block, Some(BlockAddr::new("root", "last")));
        app.on_key(key(KeyCode::Char('['))).await;
        assert_eq!(app.selected_block, Some(BlockAddr::new("root", "pick")));
    }

    #[tokio::test]
    async fn clicking_into_a_form_selects_it() {
        let mut app = app_with_blocks(
            "form-click-select",
            concat!(
                "<!-- meshfox:var name=\"IN\" session -->\n\n",
                "```form name=pick\nfield var=\"IN\" label=\"Input\"\n```\n",
            ),
        )
        .await;
        app.set_focus(Focus::Tree);
        assert!(app.selected_block.is_none());
        app.activate_click_target(ClickTarget::FormField {
            node_id: "root".into(),
            block_name: "pick".into(),
            field_index: 0,
        })
        .await;
        assert_eq!(app.selected_block, Some(BlockAddr::new("root", "pick")));
        assert!(app.focus == Focus::Document);
        assert!(document_text(&app).contains("›─ form · pick"));
    }

    #[tokio::test]
    async fn block_focus_keyboard_and_mouse_share_independent_fold_state() {
        let mut app = app_with_blocks("block-focus", concat!(
            "```sh name=a fold\necho SOURCE_A\n```\n",
            "<!-- meshfox:output name=\"a\" exit=\"0\" -->\n```text\nCACHED_A\n```\n<!-- /meshfox:output -->\n",
            "```sh name=b fold=false\necho SOURCE_B\n```\n",
        )).await;
        app.set_focus(Focus::Document);
        assert!(!document_text(&app).contains("SOURCE_A"));
        assert!(document_text(&app).contains("SOURCE_B"));
        assert!(document_text(&app).contains("CACHED_A"));
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        app.on_key(key(KeyCode::Char(']'))).await;
        assert_eq!(app.selected_block, Some(BlockAddr::new("root", "a")));
        assert!(!document_text(&app).contains("› ┌─"));
        app.on_key(key(KeyCode::Char('c'))).await;
        assert!(document_text(&app).contains("SOURCE_A"));
        app.on_key(key(KeyCode::Enter)).await;
        assert!(!document_text(&app).contains("SOURCE_A"));
        assert!(!document_text(&app).contains("CACHED_A"));
        assert!(document_text(&app).contains("[run]"));
        app.activate_click_target(ClickTarget::Block {
            node_id: "root".into(),
            block_name: "a".into(),
            action: markdown::BlockAction::ToggleBlock,
        })
        .await;
        assert!(document_text(&app).contains("SOURCE_A"));
        assert!(document_text(&app).contains("CACHED_A"));
        app.activate_click_target(ClickTarget::Block {
            node_id: "root".into(),
            block_name: "a".into(),
            action: markdown::BlockAction::ToggleSource,
        })
        .await;
        assert!(!document_text(&app).contains("SOURCE_A"));
        assert!(document_text(&app).contains("CACHED_A"));
        app.on_key(key(KeyCode::Char(']'))).await;
        assert_eq!(app.selected_block, Some(BlockAddr::new("root", "b")));
        app.on_key(key(KeyCode::Char(']'))).await;
        assert_eq!(app.selected_block, Some(BlockAddr::new("root", "a")));
        assert!(!document_text(&app).contains("SOURCE_A"));
        app.on_key(key(KeyCode::Esc)).await;
        assert!(app.selected_block.is_none());
        app.on_key(key(KeyCode::Char('['))).await;
        assert_eq!(app.selected_block, Some(BlockAddr::new("root", "b")));
        let _ = std::fs::remove_dir_all(app.canvas_path.parent().unwrap());
    }

    #[tokio::test]
    async fn block_selection_preserves_geometry_and_highlights_cached_and_live_output() {
        let mut app = app_with_blocks("block-highlight", concat!(
            "```sh name=a\necho CODE\n```\n",
            "<!-- meshfox:output name=\"a\" exit=\"0\" -->\n```text\nCACHED\n```\n<!-- /meshfox:output -->\n",
            "```sh name=b\ntrue\n```\n",
        )).await;
        let before = document_text(&app);
        app.activate_block_action(BlockAddr::new("root", "a"), markdown::BlockAction::Select)
            .await;
        assert_eq!(
            document_text(&app).replace("›─", "┌─"),
            before,
            "selection must not add columns or rows"
        );
        let check_output = |app: &App, color: Color| {
            let line = app
                .doc_segments
                .iter()
                .find_map(|seg| match seg {
                    Segment::Text(lines) => lines
                        .iter()
                        .find(|line| line.to_string().starts_with("┌─ output: a")),
                    _ => None,
                })
                .expect("output header");
            assert_eq!(line.spans[0].style.fg, Some(color));
            let other = app
                .doc_segments
                .iter()
                .find_map(|seg| match seg {
                    Segment::Text(lines) => lines.iter().find(|line| {
                        (line.to_string().starts_with("┌─") || line.to_string().starts_with("›─"))
                            && line.to_string().contains("sh · b")
                    }),
                    _ => None,
                })
                .unwrap();
            assert_eq!(other.spans[0].style.fg, Some(super::super::theme::DEP));
        };
        check_output(&app, super::super::theme::ACCENT);
        app.on_reconciled_run_event(
            BlockAddr::new("root", "a"),
            crate::worker_client::SubscribeEvent::Line {
                stream: meshfox_server::stream_exec::OutputStream::Stdout,
                text: "LIVE".into(),
            },
            None,
        );
        check_output(&app, super::super::theme::OK);
        app.set_focus(Focus::Tree);
        check_output(&app, super::super::theme::OK);
        app.set_focus(Focus::Document);
        check_output(&app, super::super::theme::OK);
        app.selected_block = None;
        app.render_current_document();
        check_output(&app, super::super::theme::OK);
        let _ = std::fs::remove_dir_all(app.canvas_path.parent().unwrap());
    }

    #[tokio::test]
    async fn block_fold_choices_survive_node_navigation_and_reload() {
        let mut app = app_with_blocks(
            "block-navigation",
            concat!(
                "```sh name=a fold\necho SOURCE\n```\n",
                "## Child\n<!-- meshfox:node id=\"child\" -->\nChild body\n",
            ),
        )
        .await;
        app.activate_block_action(
            BlockAddr::new("root", "a"),
            markdown::BlockAction::ToggleSource,
        )
        .await;
        assert!(document_text(&app).contains("echo SOURCE"));
        app.jump_to_node("child");
        assert!(app.selected_block.is_none());
        app.jump_to_node("root");
        assert!(document_text(&app).contains("echo SOURCE"));
        let updated = app.raw.replace("echo SOURCE", "echo UPDATED_SOURCE");
        app.apply_external_reload(updated, None);
        assert!(document_text(&app).contains("echo UPDATED_SOURCE"));
        app.activate_block_action(BlockAddr::new("root", "a"), markdown::BlockAction::Select)
            .await;
        let removed = app.raw.replace(
            "```sh name=a fold\necho UPDATED_SOURCE\n```",
            "Removed block",
        );
        app.apply_external_reload(removed, None);
        assert!(app.selected_block.is_none());
        let _ = std::fs::remove_dir_all(app.canvas_path.parent().unwrap());
    }

    #[tokio::test]
    async fn collapsed_block_stays_collapsed_when_live_output_arrives() {
        let mut app = app_with_blocks("block-live-fold", "```sh name=a\necho SOURCE\n```\n").await;
        app.activate_block_action(
            BlockAddr::new("root", "a"),
            markdown::BlockAction::ToggleBlock,
        )
        .await;
        app.on_reconciled_run_event(
            BlockAddr::new("root", "a"),
            crate::worker_client::SubscribeEvent::Line {
                stream: meshfox_server::stream_exec::OutputStream::Stdout,
                text: "LIVE_OUTPUT".into(),
            },
            None,
        );
        assert!(!document_text(&app).contains("LIVE_OUTPUT"));
        app.activate_block_action(
            BlockAddr::new("root", "a"),
            markdown::BlockAction::ToggleBlock,
        )
        .await;
        assert!(document_text(&app).contains("LIVE_OUTPUT"));
        app.activate_block_action(
            BlockAddr::new("root", "a"),
            markdown::BlockAction::ToggleSource,
        )
        .await;
        assert!(!document_text(&app).contains("echo SOURCE"));
        assert!(document_text(&app).contains("LIVE_OUTPUT"));
        let _ = std::fs::remove_dir_all(app.canvas_path.parent().unwrap());
    }

    #[tokio::test]
    async fn selected_block_highlight_dims_on_blur_and_history_addresses_it_directly() {
        let mut app = app_with_blocks(
            "block-blur",
            "```sh name=a\necho a\n```\n```sh name=b\necho b\n```\n",
        )
        .await;
        app.activate_block_action(BlockAddr::new("root", "b"), markdown::BlockAction::Select)
            .await;
        let header_color = |app: &App| {
            app.doc_segments.iter().find_map(|seg| match seg {
                Segment::Text(lines) => lines
                    .iter()
                    .find(|line| {
                        (line.to_string().starts_with("┌─") || line.to_string().starts_with("›─"))
                            && line.to_string().contains("sh · b")
                    })
                    .map(|line| line.spans[0].style.fg),
                _ => None,
            })
        };
        assert_eq!(header_color(&app), Some(Some(super::super::theme::ACCENT)));
        assert!(document_text(&app).contains("›─"));
        app.set_focus(Focus::Tree);
        assert_eq!(header_color(&app), Some(Some(Color::DarkGray)));
        assert!(!document_text(&app).contains("›─"));
        app.set_focus(Focus::Document);
        assert!(document_text(&app).contains("›─"));
        app.open_run_history().await;
        assert!(app.block_picker.is_none());
        assert!(app.status.contains("none reachable"));
        app.selected_block = None;
        app.worker_port = Some(1);
        app.open_run_history().await;
        assert!(app.block_picker.is_some());
        let _ = std::fs::remove_dir_all(app.canvas_path.parent().unwrap());
    }

    #[tokio::test]
    async fn image_invalidation_discards_old_responses_and_retries_failed_paths() {
        let mut app = app_with_blocks("image-refresh", "![plot](plot.png)").await;
        let path = app.canvas_path.parent().unwrap().join("plot.png");
        app.image_bytes.insert(path.clone(), vec![1]);
        app.image_requested.insert(path.clone());
        app.invalidate_images();
        assert!(!app.image_bytes.contains_key(&path));
        assert!(!app.image_requested.contains(&path));
        app.on_background_msg(BackgroundMsg::ImageBytes {
            generation: 0,
            path: path.clone(),
            result: Ok(vec![2]),
        });
        assert!(!app.image_bytes.contains_key(&path));
        app.on_background_msg(BackgroundMsg::ImageBytes {
            generation: app.image_generation,
            path: path.clone(),
            result: Ok(vec![3]),
        });
        assert_eq!(app.image_bytes[&path], vec![3]);
        let _ = std::fs::remove_dir_all(app.canvas_path.parent().unwrap());
    }

    async fn app_with_blocks(name: &str, node_body: &str) -> App {
        let dir =
            std::env::temp_dir().join(format!("meshfox-tui-run-history-{name}-{}", uuid_like()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("canvas.md");
        std::fs::write(
            &path,
            format!(
                "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n\n{node_body}"
            ),
        )
        .unwrap();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        App::new(path, tx, None, None).await.unwrap()
    }

    fn active_run(block: &str, kind: &str, status: &str) -> crate::worker_client::ActiveRunDto {
        crate::worker_client::ActiveRunDto {
            history_id: None,
            node_id: "root".into(),
            block: block.into(),
            kind: kind.into(),
            status: status.into(),
            exit_code: None,
            uptime_ms: 0,
        }
    }

    #[tokio::test]
    async fn a_form_send_is_refused_while_a_foreground_run_is_in_progress() {
        let mut app = app_with_blocks("send-busy", "```sh name=build\necho build\n```\n").await;
        assert!(!app.foreground_run_in_progress());

        // A finished run, and a service-less idle worker, don't count...
        app.block_runs = Some(vec![active_run("build", "plain", "exited")]);
        assert!(!app.foreground_run_in_progress());
        // ...a running one does, plain or `tty` (an unattended `autorun`
        // terminal can sit there for as long as it likes).
        for kind in ["plain", "tty"] {
            app.block_runs = Some(vec![active_run("build", kind, "running")]);
            assert!(app.foreground_run_in_progress(), "{kind}");
        }

        // The Send itself: refused with a reason, nothing committed.
        app.active_inline_form = Some(InlineFormState {
            node_id: "root".into(),
            block_name: "pick".into(),
            fields: Vec::new(),
            decls: Vec::new(),
            inputs: Vec::new(),
            selected: 0,
            editing: true,
        });
        app.submit_inline_form().await;
        assert!(app.status.contains("run is in progress"), "{}", app.status);
        assert!(app.session_vars.is_empty());
        assert!(app.active_inline_form.as_ref().unwrap().editing);
    }

    #[tokio::test]
    async fn a_run_found_at_startup_shows_its_output_exit_code_and_known_duration() {
        use crate::worker_client::SubscribeEvent;
        let mut app = app_with_blocks("reconcile", "```bash name=\"a\"\necho a\n```\n").await;
        let addr = BlockAddr::new("root", "a");
        let out = meshfox_server::stream_exec::OutputStream::Stdout;
        let err = meshfox_server::stream_exec::OutputStream::Stderr;
        app.on_reconciled_run_event(
            addr.clone(),
            SubscribeEvent::Line {
                stream: out,
                text: "hello".into(),
            },
            Some(1234),
        );
        assert!(app.step_output[&addr].running, "still replaying");
        app.on_reconciled_run_event(
            addr.clone(),
            SubscribeEvent::Line {
                stream: err,
                text: "oops".into(),
            },
            Some(1234),
        );
        app.on_reconciled_run_event(
            addr.clone(),
            SubscribeEvent::Done { exit_code: Some(3) },
            Some(1234),
        );
        let entry = &app.step_output[&addr];
        assert_eq!(
            (entry.stdout.as_str(), entry.stderr.as_str()),
            ("hello\n", "oops\n")
        );
        assert_eq!(entry.text, "hello\noops\n");
        assert_eq!(
            (entry.exit_code, entry.duration_ms, entry.running),
            (3, 1234, false)
        );
        assert!(
            !app.external_running.contains_key(&addr),
            "no longer counted as running"
        );
    }

    #[tokio::test]
    async fn a_finished_run_that_printed_nothing_still_gets_an_entry_with_its_exit_code() {
        use crate::worker_client::SubscribeEvent;
        let mut app = app_with_blocks("silent", "```bash name=\"a\"\ntrue\n```\n").await;
        let addr = BlockAddr::new("root", "a");
        app.on_reconciled_run_event(
            addr.clone(),
            SubscribeEvent::Done { exit_code: Some(0) },
            Some(40),
        );
        let entry = &app.step_output[&addr];
        assert_eq!(
            (entry.exit_code, entry.duration_ms, entry.running),
            (0, 40, false)
        );
        assert!(entry.stdout.is_empty() && entry.stderr.is_empty());
    }

    #[tokio::test]
    async fn l_without_a_worker_reports_it_instead_of_opening_anything() {
        let mut app = app_with_blocks("noworker", "```bash name=\"a\"\necho a\n```\n").await;
        app.worker_port = None;
        app.on_key(key(KeyCode::Char('L'))).await;
        assert!(app.run_history_view.is_none() && app.block_picker.is_none());
        assert!(app.status.contains("worker"), "status: {}", app.status);
    }

    #[tokio::test]
    async fn l_on_a_node_with_several_blocks_opens_a_picker_of_only_the_ones_with_a_history() {
        let mut app = app_with_blocks(
            "picker",
            concat!(
                "```bash name=\"a\"\necho a\n```\n\n",
                "```bash name=\"b\" default\necho b\n```\n\n",
                "```bash name=\"term\" tty\nsh\n```\n\n",
                "```bash name=\"srv\" service\nsleep 1\n```\n",
            ),
        )
        .await;
        // An unreachable port: choosing a block is what would hit the
        // network, and this only gets as far as the picker.
        app.worker_port = Some(1);
        app.on_key(key(KeyCode::Char('L'))).await;
        let bp = app
            .block_picker
            .as_ref()
            .expect("several blocks open the picker first");
        assert!(bp.history, "the picker is for the history, not a run");
        let names: Vec<_> = bp.blocks.iter().map(|b| b.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["a", "b", "term"],
            "a tty block keeps a history (outcome and timing); a service block doesn't"
        );
        assert_eq!(bp.selected, 1, "the default block is preselected");
        assert!(app.run_history_view.is_none());
    }

    #[tokio::test]
    async fn l_on_a_node_without_any_history_keeping_block_says_so() {
        // (A single history-keeping block skips the picker and fetches
        // straight away — that path needs a worker, so it's covered by the
        // pty suite's `run_history.rs`.)
        let mut app = app_with_blocks(
            "service-only",
            "```bash name=\"srv\" service\nsleep 1\n```\n",
        )
        .await;
        app.worker_port = Some(1);
        app.on_key(key(KeyCode::Char('L'))).await;
        assert!(app.block_picker.is_none() && app.run_history_view.is_none());
        assert!(app.status.contains("no block"), "status: {}", app.status);
    }

    #[tokio::test]
    async fn run_history_view_navigates_scrolls_within_the_output_and_closes() {
        let mut app = app_with_blocks("nav", "```bash name=\"a\"\necho a\n```\n").await;
        app.worker_port = Some(1);
        app.run_history_view = Some(RunHistoryViewState {
            node_id: "root".into(),
            block: "a".into(),
            entries: vec![
                run_entry(3, 0, false),
                run_entry(2, 1, false),
                run_entry(1, 0, true),
            ],
            output: Some((
                3,
                (0..30)
                    .map(|i| (OutputStream::Stdout, format!("line {i}")))
                    .collect(),
            )),
            ..Default::default()
        });
        app.on_key(key(KeyCode::Up)).await;
        assert_eq!(app.run_history_view.as_ref().unwrap().selected, 0);
        app.on_key(key(KeyCode::Char('G'))).await;
        assert_eq!(app.run_history_view.as_ref().unwrap().selected, 2);
        // Moving to another run replaces the previous run's output rather
        // than leaving it under the wrong heading — here the reload fails
        // (no worker on port 1), and says so instead of showing nothing.
        let (shown_for, lines) = app
            .run_history_view
            .as_ref()
            .unwrap()
            .output
            .clone()
            .expect("reloaded");
        assert_eq!(shown_for, 1);
        assert!(lines[0].1.contains("couldn't load"), "{lines:?}");

        let view = app.run_history_view.as_mut().unwrap();
        view.output = Some((
            1,
            (0..30)
                .map(|i| (OutputStream::Stdout, format!("line {i}")))
                .collect(),
        ));
        app.on_key(key(KeyCode::PageDown)).await;
        assert_eq!(app.run_history_view.as_ref().unwrap().output_scroll, 10);
        for _ in 0..10 {
            app.on_key(key(KeyCode::PageDown)).await;
        }
        assert_eq!(
            app.run_history_view.as_ref().unwrap().output_scroll,
            29,
            "never past the last line"
        );

        // The modal claims the keymap: `q` closes it rather than quitting.
        app.on_key(key(KeyCode::Char('q'))).await;
        assert!(app.run_history_view.is_none());
    }

    #[tokio::test]
    async fn history_view_click_selects_the_row_and_double_click_jumps() {
        let dir =
            std::env::temp_dir().join(format!("meshfox-tui-history-mouse-test-{}", uuid_like()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("canvas.md");
        std::fs::write(
            &path,
            "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n",
        )
        .unwrap();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(path, tx, None, None).await.unwrap();
        let entry = |seq| crate::worker_client::HistoryEntryDto {
            seq,
            created_at: String::new(),
            op_kind: String::new(),
            applied: true,
            summary: String::new(),
        };
        let view = HistoryViewState {
            entries: (0..30).rev().map(entry).collect(),
            ..Default::default()
        };
        // As if drawn at (10, 5), 40x10, scrolled down by 7 rows.
        view.list_rect.set(Rect::new(10, 5, 40, 10));
        view.offset.set(7);
        app.history_view = Some(view);
        let click = |row: u16, col: u16| MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: col,
            row,
            modifiers: KeyModifiers::NONE,
        };

        // Third visible row -> entry 7 + 2.
        app.on_mouse(click(7, 20)).await;
        assert_eq!(app.history_view.as_ref().unwrap().selected, 9);
        // Outside the list: ignored, and never reaches the panes below.
        app.on_mouse(click(3, 20)).await;
        app.on_mouse(click(7, 5)).await;
        assert_eq!(app.history_view.as_ref().unwrap().selected, 9);
        // A single click elsewhere is just a selection...
        app.on_mouse(click(9, 20)).await;
        assert_eq!(app.history_view.as_ref().unwrap().selected, 11);
        // ...the same spot again, right away, is a double-click: it jumps
        // (no worker here, so nothing to move) and closes the view.
        app.on_mouse(click(9, 20)).await;
        assert!(app.history_view.is_none());
    }

    #[tokio::test]
    async fn page_keys_move_tree_selection_by_ten_rows_and_clamp() {
        let dir =
            std::env::temp_dir().join(format!("meshfox-tui-tree-paging-test-{}", uuid_like()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("canvas.md");
        let mut src =
            String::from("<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n");
        for i in 0..25 {
            src.push_str(&format!(
                "## Item {i}\n<!-- meshfox:node id=\"item-{i}\" -->\n"
            ));
        }
        std::fs::write(&path, src).unwrap();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(path, tx, None, None).await.unwrap();
        app.focus = Focus::Tree;
        app.selected = 0;

        app.on_key(key(KeyCode::PageDown)).await;
        assert_eq!(app.selected, 10);
        app.on_key(key(KeyCode::PageDown)).await;
        app.on_key(key(KeyCode::PageDown)).await;
        assert_eq!(app.selected, app.rows.len() - 1);
        app.on_key(key(KeyCode::PageUp)).await;
        assert_eq!(app.selected, app.rows.len() - 11);
    }

    #[tokio::test]
    async fn s_key_only_resets_the_session_after_confirming() {
        let dir = std::env::temp_dir().join(format!(
            "meshfox-tui-reset-session-confirm-test-{}",
            uuid_like()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("canvas.md");
        std::fs::write(
            &path,
            "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n",
        )
        .unwrap();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(path, tx, None, None).await.unwrap();
        app.session_vars
            .insert("greeting".to_string(), "hi".to_string());

        // `S` alone opens the prompt — doesn't clear anything yet.
        app.on_key(key(KeyCode::Char('S'))).await;
        assert!(app.reset_session_confirm);
        assert!(!app.session_vars.is_empty());

        // `n` backs out untouched.
        app.on_key(key(KeyCode::Char('n'))).await;
        assert!(!app.reset_session_confirm);
        assert!(!app.session_vars.is_empty());

        // `S` then `y` actually resets.
        app.on_key(key(KeyCode::Char('S'))).await;
        app.on_key(key(KeyCode::Char('y'))).await;
        assert!(!app.reset_session_confirm);
        assert!(app.session_vars.is_empty());
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

    /// A `from=`-declared variable is computed by running its own block,
    /// never something a human edits directly — `trigger_configure`/
    /// `has_configurable_vars` must exclude it, same as the web UI's
    /// `get_configure_vars` (`crates/server/src/lib.rs`) already does.
    #[tokio::test]
    async fn configure_excludes_from_declared_variables() {
        let dir = std::env::temp_dir().join(format!(
            "meshfox-tui-configure-from-var-test-{}",
            uuid_like()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("canvas.md");
        std::fs::write(
            &path,
            concat!(
                "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
                "<!-- meshfox:var name=\"COMPUTED\" from=\"root/produce\" -->\n",
                "<!-- meshfox:var name=\"NORMAL\" prompt=\"A normal var\" default=\"x\" -->\n\n",
                "```bash name=\"produce\"\necho hi\n```\n",
            ),
        )
        .unwrap();
        let mut app = app_with_real_worker(path).await;

        assert!(app.has_configurable_vars());

        app.trigger_configure().await;
        let form = app.var_form.as_ref().expect("configure should open a form");
        assert_eq!(
            form.decls
                .iter()
                .map(|d| d.name.as_str())
                .collect::<Vec<_>>(),
            vec!["NORMAL"],
            "COMPUTED (from=) must not show up as a configurable field"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Same exclusion, but for a document where `from=` is the *only*
    /// declared variable — `has_configurable_vars` must say there's
    /// nothing to configure, and `c` must show its "nothing configurable"
    /// status instead of opening an empty form.
    #[tokio::test]
    async fn configure_reports_nothing_to_configure_when_only_from_declared() {
        let dir = std::env::temp_dir().join(format!(
            "meshfox-tui-configure-only-from-var-test-{}",
            uuid_like()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("canvas.md");
        std::fs::write(
            &path,
            concat!(
                "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
                "<!-- meshfox:var name=\"COMPUTED\" from=\"root/produce\" -->\n\n",
                "```bash name=\"produce\"\necho hi\n```\n",
            ),
        )
        .unwrap();
        let mut app = app_with_real_worker(path).await;

        assert!(!app.has_configurable_vars());

        app.trigger_configure().await;
        assert!(app.var_form.is_none());
        assert!(app.status.contains("no configurable"));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
