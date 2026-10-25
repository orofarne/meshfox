//! Rendering the TUI's three panes (tree / document / output) plus its
//! modal overlays (block picker, variable form, help).

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::Frame;
use ratatui_image::sliced::{SignedPosition, SlicedImage};
use ratatui_image::Image;
use std::collections::HashMap;
use std::sync::Arc;
use syntect::parsing::SyntaxSet;

use edtui::{EditorView, LineNumbers, SyntaxHighlighter};

use super::app::{App, Focus, ServiceConflictState, ServicesViewState};
use super::markdown::Segment;
use super::source_editor::SourceEditorState;
use super::spatial;
use super::table;
use super::theme::{ACCENT, BORDER, DEP, FAIL, MAP_NODE_BG, MAP_NODE_FG, MAP_SELECTED_BG, OK};
use super::tree::TreeRow;
use crate::pdf::render::resolve_color_hex;
use meshfox_core::{NodeType, VarType};

/// Resolves a `meshfox:node`/`meshfox:edge` `color` attribute (a JSON-Canvas
/// preset `"1"`-`"6"` or a literal `#rrggbb` hex string — `resolve_color_hex`
/// handles the preset lookup, always returning a `#rrggbb` string either
/// way) to an actual terminal color for the tree pane. `None` for anything
/// that isn't a valid color — same "malformed input just renders with no
/// explicit color" fallback `resolve_color_hex`'s own callers already rely
/// on, rather than erroring.
pub(super) fn tree_row_color(color: Option<&str>) -> Option<Color> {
    let hex = resolve_color_hex(color?)?;
    let hex = hex.strip_prefix('#')?;
    let r = u8::from_str_radix(hex.get(0..2)?, 16).ok()?;
    let g = u8::from_str_radix(hex.get(2..4)?, 16).ok()?;
    let b = u8::from_str_radix(hex.get(4..6)?, 16).ok()?;
    Some(Color::Rgb(r, g, b))
}

/// This node's own live run status, as far as row rendering cares —
/// aggregated across every block belonging to it, mirroring the web UI's
/// own `nodeRunning`/`nodeFailed` (`MeshNode.tsx`): `Running` wins over
/// `Failed` (a node re-running a block that failed last time shows the
/// spinner, not the X), and `Failed` means the node's own last-run block
/// exited non-zero or was killed. Computed once per `render_tree` call
/// (not per row) from `App.run`/`App.file_run`/`App.step_output` — see
/// that function's own comment.
#[derive(Clone, Copy, PartialEq, Eq)]
enum RunRowState {
    Running,
    Failed,
}

/// A Braille "dots" spinner frame, indexed by `App::spinner_tick` — see
/// that field's own doc comment for why a plain per-redraw counter, not
/// wall-clock time: this pane doesn't redraw on any fixed schedule fast
/// enough to sample every 100ms-ish slice of real time, so deriving the
/// frame from elapsed time meant consecutive draws routinely landed many
/// frames apart and visibly jumped. Indexing by tick count instead means
/// every draw shows exactly the next frame after the last one drawn,
/// however much real time actually passed in between.
fn spinner_frame(tick: u32) -> char {
    const FRAMES: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
    FRAMES[tick as usize % FRAMES.len()]
}

/// Everything a tree row's title line is made of, flattened into
/// individually-wrappable words, each carrying its own style — the title's
/// own words, then (if present) the run-status badge, the constraint mark,
/// the service glyph, each tag, and the runnable/cache/tty badge, in that
/// order (same relative placement as the web UI's own title bar — run
/// status right after the title, ahead of everything else). Keeping this
/// as a flat word list (rather than a handful of pre-joined strings) is
/// what lets `wrap_word_indices` below wrap the *whole* row — title, tags,
/// and badges alike — instead of only the title while silently clipping
/// the rest.
/// This node's own live service state, as far as row rendering cares —
/// aggregated across every service belonging to it (same row-level, not
/// per-block, granularity `TreeRow::has_service` already uses). Computed
/// once per `render_tree` call (not per row) from `App.service_list` — see
/// that function's own comment. **Experimental**, see SPEC.md's "Service
/// blocks (experimental)".
#[derive(Clone, Copy, PartialEq, Eq)]
enum ServiceRowState {
    Running,
    Crashed,
}

fn tree_row_words(
    row: &TreeRow,
    title_style: Style,
    service: Option<ServiceRowState>,
    run_status: Option<RunRowState>,
    spinner_tick: u32,
) -> Vec<(String, Style)> {
    if let Some(reference) = &row.reference {
        let mut words = Vec::new();
        if let Some(label) = reference.label.as_deref().filter(|label| !label.is_empty()) {
            words.push((format!("[{label}]"), Style::default().fg(ACCENT)));
        }
        words.extend(row.title.split_whitespace().map(|word| {
            (
                word.to_string(),
                Style::default().fg(DEP).add_modifier(Modifier::ITALIC),
            )
        }));
        return words;
    }
    let mut words: Vec<(String, Style)> = row
        .title
        .split_whitespace()
        .map(|w| (w.to_string(), title_style))
        .collect();
    match run_status {
        Some(RunRowState::Running) => {
            words.push((
                spinner_frame(spinner_tick).to_string(),
                Style::default().fg(ACCENT),
            ));
        }
        Some(RunRowState::Failed) => {
            words.push(("✗".to_string(), Style::default().fg(FAIL)));
        }
        None => {}
    }
    match row.constraint_ok {
        Some(true) => words.push(("✓".to_string(), Style::default().fg(OK))),
        Some(false) => words.push(("✗".to_string(), Style::default().fg(FAIL))),
        None => {}
    }
    if row.has_service {
        let (glyph, color) = match service {
            Some(ServiceRowState::Running) => ("●service", OK),
            Some(ServiceRowState::Crashed) => ("⚠service", FAIL),
            None => ("○service", Color::DarkGray),
        };
        words.push((glyph.to_string(), Style::default().fg(color)));
    }
    for tag in &row.tags {
        words.push((format!("#{tag}"), Style::default().fg(Color::Cyan)));
    }
    let mut flags = Vec::new();
    if row.runnable_count > 0 {
        flags.push("run");
    }
    if row.has_cache {
        flags.push("cache");
    }
    if row.has_tty {
        flags.push("tty");
    }
    if !flags.is_empty() {
        words.push((format!("[{}]", flags.join(",")), Style::default().fg(OK)));
    }
    words
}

/// Greedy word-wrap over a row's own words (by index, so callers keep
/// ownership of the words themselves): packs as many as fit within
/// `first_width` on the first output row, `cont_width` on every row after
/// (the tree pane's continuation rows lose the disclosure/type-marker
/// prefix the first row has, so they get more room). A single word wider
/// than its own row's budget still gets a whole row to itself rather than
/// being split mid-word — same "don't break tokens" choice `ratatui`'s own
/// `WordWrapper` defaults to, and what keeps e.g. a lone long tag legible.
fn wrap_word_indices(
    word_widths: &[usize],
    first_width: usize,
    cont_width: usize,
) -> Vec<Vec<usize>> {
    if word_widths.is_empty() {
        return vec![Vec::new()];
    }
    let mut result: Vec<Vec<usize>> = Vec::new();
    let mut current: Vec<usize> = Vec::new();
    let mut current_width = 0usize;
    let mut budget = first_width.max(1);
    for (i, &w) in word_widths.iter().enumerate() {
        let needed = if current.is_empty() {
            w
        } else {
            current_width + 1 + w
        };
        if !current.is_empty() && needed > budget {
            result.push(current);
            current = Vec::new();
            current_width = 0;
            budget = cont_width.max(1);
        }
        current_width = if current.is_empty() {
            w
        } else {
            current_width + 1 + w
        };
        current.push(i);
    }
    result.push(current);
    result
}

/// Theme/language edtui's own bundled `syntect` highlighting uses for the
/// source editor — always Markdown, since that's what every file this
/// editor can open (a canvas or a plain-Markdown include target) actually
/// is. Same `"base16-ocean.dark"` the read-only preview pane's own
/// highlighter already uses (`markdown.rs`), so both panes read as the
/// same product. This used to be `"dracula"` — edtui's own docs/examples
/// default to that name, but it's *not* one of the themes `syntect::
/// highlighting::ThemeSet::load_defaults()` actually bundles (confirmed
/// directly: `["InspiredGitHub", "Solarized (dark)", "Solarized (light)",
/// "base16-eighties.dark", "base16-mocha.dark", "base16-ocean.dark",
/// "base16-ocean.light"]`, no "dracula" anywhere in it) — the lookup
/// always silently missed and fell through to `crate::syntax_registry::
/// with_meshfox_scope_colors`'s caller's own `.or_else(|| ...values()
/// .next()...)` fallback, landing on *whichever* bundled theme happened to
/// be first in the `HashMap`'s own (arbitrary, unspecified) iteration
/// order — never a deliberate choice at all. See that same module's
/// `with_meshfox_scope_colors` for the other half of this fix (forcing a
/// legible base/default foreground regardless of which theme ends up
/// loaded).
///
/// This is only the *default* now — `App::editor_theme`
/// (`crate::syntax_registry::resolve_editor_theme`) picks a
/// user-configured `[tui] editor_theme` override when one is set and
/// actually valid, falling back to this constant otherwise; `render`
/// threads `App::editor_theme` into `render_source_editor` rather than
/// this constant being read directly there anymore.
pub(crate) const SOURCE_EDITOR_THEME: &str = "base16-ocean.dark";
/// Falls back to plain `"md"` (`find_syntax_by_token`, extension-based) only
/// if `crate::syntax_registry::MESHFOX_MARKDOWN_SYNTAX_NAME` somehow isn't
/// registered — never actually expected (it's bundled into the binary, see
/// `syntax_registry::build_syntax_set`), but resolving a language is one
/// `Option` chain either way, so there's no cost to not panicking on it.
const SOURCE_EDITOR_LANG: &str = "md";

/// Default `App::output_height_pct` — see `App::resize_handle_at`/
/// `on_resize_drag` for how a mouse drag on the border can change it.
pub(super) const DEFAULT_OUTPUT_HEIGHT_PCT: u16 = 20;
/// The fewest rows the Output pane keeps (its border plus one content row).
pub(super) const MIN_OUTPUT_HEIGHT: u16 = 3;
/// Default `App::tree_width_pct` — same reasoning as `DEFAULT_OUTPUT_HEIGHT_PCT`.
pub(super) const DEFAULT_TREE_WIDTH_PCT: u16 = 30;
pub(super) const FOOTER_HEIGHT: u16 = 1;

/// Any pane's own top-right corner toggle (`fullscreen_icon_title`) — same
/// bracket-badge look `[cache]`/`[run,cache]` already use elsewhere in
/// this TUI. Both exactly `FULLSCREEN_ICON_WIDTH` columns wide, so
/// `App::on_mouse`'s own click hit-test (it only needs the width, not
/// which glyph is currently showing) stays in step with wherever this
/// actually renders (always flush against the border's own right corner,
/// via `Line::right_aligned`).
pub const FULLSCREEN_ICON_EXPAND: &str = "[+]";
pub const FULLSCREEN_ICON_SHRINK: &str = "[-]";
pub const FULLSCREEN_ICON_WIDTH: u16 = 3;

/// The right-aligned title every pane's own `Block` carries (`render_tree`/
/// `render_document`/`render_output`) — `[+]` normally, `[-]` when `pane`
/// itself is the one currently filling the screen (`App::fullscreen`).
/// Click it, double-click the title row, or press `f` while `pane` is
/// focused (`App::on_key`/`on_mouse`) to toggle it.
fn fullscreen_icon_title(app: &App, pane: Focus) -> Line<'static> {
    let icon = if app.fullscreen == Some(pane) {
        FULLSCREEN_ICON_SHRINK
    } else {
        FULLSCREEN_ICON_EXPAND
    };
    Line::from(Span::styled(icon, Style::default().fg(ACCENT))).right_aligned()
}

/// The three panes' rects for a given terminal size — computed once here
/// and shared by both rendering (`render`) and mouse hit-testing
/// (`app::App::on_mouse`), so the two can never drift apart.
pub struct PaneLayout {
    pub tree: Rect,
    pub document: Rect,
    pub output: Rect,
    pub footer: Rect,
}

/// The minimum height (rows) `compute_layout` ever leaves the tree/document
/// row for — matches the `Constraint::Min(6)` below. `App::on_resize_drag`
/// clamps `output_height` against this so a drag can never starve it past
/// what `compute_layout` itself would already refuse to shrink further.
pub(super) const MIN_MAIN_HEIGHT: u16 = 6;

/// The Tree pane's own width, in columns, while `App::tree_collapsed` is
/// set — the Tree/Document split's counterpart to `console_collapsed`'s
/// `Constraint::Length(1)`. Unlike Output's collapsed strip (a full-width
/// row, plenty of room for its own title text), Tree collapses along its
/// *width*, so there's no meaningful room left for a label — just enough
/// for `render_tree`'s own borderless "▸" handle, still wide enough to
/// register a click.
pub(super) const TREE_COLLAPSED_WIDTH: u16 = 2;

/// `fullscreen` (see `App::fullscreen`), when set, collapses the other two
/// panes to empty rects and gives whichever one it names the whole area
/// above the footer — a click/scroll's own `point_in(layout.tree, ...)`/
/// `point_in(layout.document, ...)`/`point_in(layout.output, ...)` checks
/// in `App::on_mouse` then simply never match a collapsed pane, with no
/// separate "are we fullscreen, and which pane" branch needed there.
///
/// `tree_width_pct`/`output_height_pct` (`App`'s own fields, adjustable by
/// dragging the border between panes — see `App::on_resize_drag`) size the
/// tree/document horizontal split and the Output pane's own height; both
/// are ignored while `fullscreen` is set, same as before either field
/// existed.
/// `console_collapsed`/`tree_collapsed` (see `App`'s own fields) shrink the
/// Output row to a 1-line strip / the Tree column to `TREE_COLLAPSED_WIDTH`
/// instead of their usual `output_height_pct`/`tree_width_pct` — collapsing
/// only ever shrinks a pane along its own axis, it never disappears
/// entirely, so it stays visible/clickable to expand manually at any time
/// (see `App::on_mouse`'s own collapsed-strip click handling for each).
pub fn compute_layout(
    area: Rect,
    fullscreen: Option<Focus>,
    tree_width_pct: u16,
    output_height_pct: u16,
    console_collapsed: bool,
    tree_collapsed: bool,
) -> PaneLayout {
    if let Some(pane) = fullscreen {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(1), Constraint::Length(FOOTER_HEIGHT)])
            .split(area);
        let full = chunks[0];
        let empty = Rect::default();
        return PaneLayout {
            tree: if pane == Focus::Tree { full } else { empty },
            document: if pane == Focus::Document { full } else { empty },
            output: if pane == Focus::Output { full } else { empty },
            footer: chunks[1],
        };
    }

    let content_height = area.height.saturating_sub(FOOTER_HEIGHT);
    let max_output = content_height.saturating_sub(MIN_MAIN_HEIGHT);
    let output_row_height = if console_collapsed {
        1
    } else {
        let wanted = (content_height as u32 * output_height_pct as u32 / 100) as u16;
        wanted.max(MIN_OUTPUT_HEIGHT).min(max_output.max(1))
    };
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(MIN_MAIN_HEIGHT),
            Constraint::Length(output_row_height),
            Constraint::Length(FOOTER_HEIGHT),
        ])
        .split(area);

    let main = if tree_collapsed {
        Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(TREE_COLLAPSED_WIDTH), Constraint::Min(0)])
            .split(chunks[0])
    } else {
        Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Percentage(tree_width_pct),
                Constraint::Percentage(100 - tree_width_pct),
            ])
            .split(chunks[0])
    };

    PaneLayout {
        tree: main[0],
        document: main[1],
        output: chunks[1],
        footer: chunks[2],
    }
}

pub fn render(f: &mut Frame, app: &mut App) {
    let area = f.area();

    // The source editor is a genuine full-terminal takeover, not an
    // overlay on top of the usual 3-pane layout — see `source_editor.rs`'s
    // own module docs.
    if let Some(se) = &mut app.source_editor {
        let syntax_set = Arc::clone(app.highlighter.syntax_set());
        render_source_editor(f, area, se, &syntax_set, &app.editor_theme);
        if let Some(palette) = &mut app.palette {
            palette.render(f, area);
        }
        return;
    }

    // So is the full-screen table (`Enter` on a `display="table"` node): it
    // owns the keymap too (`App::on_table_key`), so nothing else is drawn.
    if let Some(tm) = &app.table_mode {
        let empty = table::TableData::default();
        let data = app.tables.get(&tm.node_id).unwrap_or(&empty);
        table::render_overlay(f, area, data, tm);
        return;
    }

    let layout = compute_layout(
        area,
        app.fullscreen,
        app.navigation_width_pct(),
        app.output_height_pct,
        app.console_collapsed,
        app.tree_collapsed,
    );

    match app.fullscreen {
        None => {
            render_tree(f, layout.tree, app);
            render_document(f, layout.document, app);
            render_output(f, layout.output, &*app);
        }
        Some(Focus::Tree) => render_tree(f, layout.tree, app),
        Some(Focus::Document) => render_document(f, layout.document, app),
        Some(Focus::Output) => render_output(f, layout.output, &*app),
    }
    render_footer(f, layout.footer, &*app);
    if app.search.open {
        render_search(f, layout.footer, app);
    }

    if let Some(pp) = &app.path_picker {
        render_path_picker(f, area, pp);
    } else if let Some(bp) = &app.block_picker {
        render_block_picker(f, area, bp);
    } else if let Some(vf) = &app.var_form {
        render_var_form(f, area, vf);
    } else if let Some(pending) = &app.run_confirmation {
        let rect = centered_rect(70, 12, area);
        f.render_widget(Clear, rect);
        f.render_widget(Paragraph::new(format!(
            "Potentially destructive operation(s):\n{}\n\nRun these blocks?\n\ny confirm · n/esc/enter cancel",
            pending.blocks.join("\n")))
            .wrap(Wrap { trim: true })
            .block(Block::default().borders(Borders::ALL).title(" confirm run ")), rect);
    } else if app.reset_session_confirm {
        render_reset_session_confirm(f, area);
    } else if let Some(conflict) = &app.service_conflict {
        render_service_conflict(f, area, conflict);
    } else if let Some(sv) = &app.services_view {
        render_services_view(f, area, &*app, sv);
    } else if let Some(tv) = &app.tty_sessions_view {
        render_tty_sessions_view(f, area, &*app, tv);
    } else if let Some(sv) = &app.stats_view {
        render_stats_view(f, area, sv);
    } else if let Some(hv) = &app.history_view {
        render_history_view(f, area, hv);
    } else if let Some(rv) = &app.run_history_view {
        render_run_history_view(f, area, rv);
    } else if app.show_help {
        render_help(f, area, &*app);
    }
    if let Some(palette) = &mut app.palette {
        palette.render(f, area);
    }
}

fn pane_border(focused: bool) -> Style {
    Style::default().fg(if focused { ACCENT } else { BORDER })
}

fn render_search(f: &mut Frame, area: Rect, app: &App) {
    let matches = app.search.matches(&app.display_canvas);
    let count = if app.search.query.trim().is_empty() {
        String::new()
    } else if matches.is_empty() {
        "0/0".into()
    } else {
        format!(
            "{}/{}",
            app.search.index.min(matches.len() - 1) + 1,
            matches.len()
        )
    };
    let prefix = format!("/ {count} ");
    let budget = usize::from(area.width).saturating_sub(Span::raw(&prefix).width());
    let cursor = app.search.cursor.min(app.search.query.len());
    let before = &app.search.query[..cursor];
    let mut start = 0;
    let mut width = Span::raw(before).width();
    for (i, ch) in before.char_indices() {
        if width < budget {
            break;
        }
        width = width.saturating_sub(Span::raw(ch.to_string()).width());
        start = i + ch.len_utf8();
    }
    let before = &before[start..];
    let input = format!("{before}▏{}", &app.search.query[cursor..]);
    f.render_widget(Clear, area);
    f.render_widget(
        Paragraph::new(format!(
            "{prefix}{input} · Enter next · Shift+Enter/Tab prev · Esc close"
        ))
        .style(Style::default().fg(ACCENT)),
        area,
    );
}

fn type_marker(t: NodeType) -> &'static str {
    match t {
        NodeType::Text => "",
        NodeType::File => "[file] ",
        NodeType::Link => "[link] ",
        NodeType::Group => "[group] ",
        NodeType::Include => "[include] ",
    }
}

fn map_title_lines(title: &str, width: usize) -> Text<'static> {
    let width = width.max(1);
    let mut lines = Vec::new();
    let mut current = String::new();
    for word in title.split_whitespace() {
        if !current.is_empty() && current.chars().count() + 1 + word.chars().count() > width {
            lines.push(Line::raw(std::mem::take(&mut current)));
        }
        if !current.is_empty() {
            current.push(' ');
        }
        if word.chars().count() <= width {
            current.push_str(word);
        } else {
            for ch in word.chars() {
                if current.chars().count() >= width {
                    lines.push(Line::raw(std::mem::take(&mut current)));
                }
                current.push(ch);
            }
        }
    }
    if !current.is_empty() {
        lines.push(Line::raw(current));
    }
    Text::from(lines.into_iter().take(2).collect::<Vec<_>>())
}

fn draw_map_arrow(
    grid: &mut [Vec<char>],
    from: Rect,
    to: Rect,
    area: Rect,
    obstacles: &[Rect],
) -> Vec<(usize, usize)> {
    use std::cmp::Reverse;
    use std::collections::BinaryHeap;

    let width = area.width as i32;
    let height = area.height as i32;
    if width == 0 || height == 0 {
        return Vec::new();
    }
    // Ports sit just outside the cards. An edge can bend around any other
    // card, but it must never paint across one, even when it spans rows.
    let vertical = from.y != to.y;
    let (start, end, head, entrance) = if vertical {
        let down = from.y < to.y;
        let sx = from.x + from.width / 2;
        let tx = to.x + to.width / 2;
        let sy = if down {
            from.y + from.height
        } else {
            from.y.saturating_sub(1)
        };
        let ty = if down {
            to.y.saturating_sub(1)
        } else {
            to.y + to.height
        };
        (
            (sx, sy),
            (tx, ty),
            if down { '↓' } else { '↑' },
            if down { 0 } else { 1 },
        )
    } else {
        let right = from.x < to.x;
        let sy = from.y + from.height / 2;
        let ty = to.y + to.height / 2;
        let sx = if right {
            from.x + from.width
        } else {
            from.x.saturating_sub(1)
        };
        let tx = if right {
            to.x.saturating_sub(1)
        } else {
            to.x + to.width
        };
        (
            (sx, sy),
            (tx, ty),
            if right { '→' } else { '←' },
            if right { 2 } else { 3 },
        )
    };
    let start = (
        start.0 as i32 - area.x as i32,
        start.1 as i32 - area.y as i32,
    );
    let end = (end.0 as i32 - area.x as i32, end.1 as i32 - area.y as i32);
    let inside = |(x, y): (i32, i32)| x >= 0 && y >= 0 && x < width && y < height;
    if !inside(start) || !inside(end) {
        return Vec::new();
    }
    let blocked = |(x, y): (i32, i32)| {
        let x = x + area.x as i32;
        let y = y + area.y as i32;
        obstacles.iter().any(|r| {
            x >= r.x as i32
                && x < (r.x + r.width) as i32
                && y >= r.y as i32
                && y < (r.y + r.height) as i32
        })
    };
    let dirs = [(0, 1), (0, -1), (1, 0), (-1, 0)];
    let index = |(x, y): (i32, i32), d: usize| ((y * width + x) as usize) * 4 + d;
    let mut cost = vec![u32::MAX; (width * height * 4) as usize];
    let mut previous = vec![None; cost.len()];
    let mut queue = BinaryHeap::new();
    let first = index(start, entrance);
    cost[first] = 0;
    queue.push(Reverse((0u32, start.0, start.1, entrance)));
    let mut goal = None;
    while let Some(Reverse((current, x, y, dir))) = queue.pop() {
        let at = index((x, y), dir);
        if current != cost[at] {
            continue;
        }
        if (x, y) == end {
            goal = Some(at);
            break;
        }
        for (next_dir, (dx, dy)) in dirs.iter().enumerate() {
            let next = (x + dx, y + dy);
            if !inside(next) || blocked(next) {
                continue;
            }
            let crossing = grid[next.1 as usize][next.0 as usize] != ' ';
            let next_cost =
                current + 10 + u32::from(next_dir != dir) * 18 + u32::from(crossing) * 80;
            let at_next = index(next, next_dir);
            if next_cost < cost[at_next] {
                cost[at_next] = next_cost;
                previous[at_next] = Some(at);
                queue.push(Reverse((next_cost, next.0, next.1, next_dir)));
            }
        }
    }
    let Some(mut at) = goal else {
        return Vec::new();
    };
    let mut path = Vec::new();
    loop {
        let cell = at / 4;
        path.push(((cell as i32 % width), (cell as i32 / width)));
        if at == first {
            break;
        }
        at = previous[at].expect("routed cell has a predecessor");
    }
    path.reverse();
    for (i, &(x, y)) in path.iter().enumerate() {
        let glyph = if i + 1 == path.len() {
            head
        } else {
            let before = if i == 0 {
                match entrance {
                    0 => (x, y - 1),
                    1 => (x, y + 1),
                    2 => (x - 1, y),
                    _ => (x + 1, y),
                }
            } else {
                path[i - 1]
            };
            let after = path[i + 1];
            let left = before.0 < x || after.0 < x;
            let right = before.0 > x || after.0 > x;
            let up = before.1 < y || after.1 < y;
            let down = before.1 > y || after.1 > y;
            match (left, right, up, down) {
                (true, true, _, _) => '─',
                (_, _, true, true) => '│',
                (false, true, false, true) => '┌',
                (true, false, false, true) => '┐',
                (false, true, true, false) => '└',
                (true, false, true, false) => '┘',
                (true, false, false, false) | (false, true, false, false) => '─',
                _ => '│',
            }
        };
        grid[y as usize][x as usize] = glyph;
    }
    path.into_iter()
        .map(|(x, y)| (x as usize, y as usize))
        .collect()
}

fn render_spatial_parent(f: &mut Frame, area: Rect, app: &mut App, group_id: &str) {
    let title = app
        .display_canvas
        .node(group_id)
        .map(|n| n.title.as_str())
        .unwrap_or("Group");
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(pane_border(app.focus == Focus::Tree))
        .title(format!(" {} ", title))
        .title(fullscreen_icon_title(app, Focus::Tree));
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.height < 3 || inner.width < 8 {
        return;
    }
    let map_area = Rect::new(inner.x, inner.y, inner.width, inner.height - 2);
    let Some(selected) = app.rows.get(app.selected) else {
        return;
    };
    let selected_id = selected.node_id.clone();
    let overview = selected_id == group_id;
    let prior = app
        .spatial_viewports
        .get(group_id)
        .copied()
        .unwrap_or_default();
    let projection = spatial::project(&app.display_canvas, group_id, map_area, &selected_id, prior);
    app.spatial_viewports
        .insert(group_id.to_string(), projection.viewport);
    let nodes = projection.visible;
    let mut grid = vec![
        vec![' '; projection.virtual_area.width as usize];
        projection.virtual_area.height as usize
    ];
    let mut selected_grid = grid.clone();
    let rects: HashMap<&str, Rect> = projection
        .all
        .iter()
        .map(|n| (n.id.as_str(), n.rect))
        .collect();
    let obstacles: Vec<Rect> = projection.all.iter().map(|n| n.rect).collect();
    let visible_ids: std::collections::HashSet<&str> =
        nodes.iter().map(|n| n.id.as_str()).collect();
    let mut incoming = 0;
    let mut outgoing = 0;
    let mut offscreen = Vec::new();
    let mut edge_details = Vec::new();
    let mut visible_edges = Vec::new();
    for child in app.display_canvas.children(group_id) {
        for edge in &child.extra_parents {
            let connected = edge.from == selected_id || child.id == selected_id;
            if edge.from == selected_id {
                outgoing += 1;
            }
            if child.id == selected_id {
                incoming += 1;
            }
            if overview || connected {
                if let Some(label) = &edge.label {
                    let direction = if child.id == selected_id {
                        "←"
                    } else {
                        "→"
                    };
                    let other = if child.id == selected_id {
                        app.display_canvas
                            .node(&edge.from)
                            .map(|n| n.title.as_str())
                            .unwrap_or(&edge.from)
                    } else {
                        &child.title
                    };
                    let mut detail = format!("{direction} {other}: {label}");
                    if edge.source_side.is_some()
                        || edge.target_side.is_some()
                        || !edge.via.is_empty()
                    {
                        let from = edge.source_side.map(|s| s.as_str()).unwrap_or("auto");
                        let to = edge.target_side.map(|s| s.as_str()).unwrap_or("auto");
                        detail.push_str(&format!(" [{from}→{to}, {} via]", edge.via.len()));
                    }
                    edge_details.push(detail);
                }
            }
            if let (Some(&from), Some(&to)) =
                (rects.get(edge.from.as_str()), rects.get(child.id.as_str()))
            {
                visible_edges.push((from, to, connected));
            }
            if child.id == selected_id && !visible_ids.contains(edge.from.as_str()) {
                let title = app
                    .display_canvas
                    .node(&edge.from)
                    .map(|n| n.title.as_str())
                    .unwrap_or(&edge.from);
                offscreen.push(format!("← {title}"));
            } else if edge.from == selected_id && !visible_ids.contains(child.id.as_str()) {
                offscreen.push(format!("→ {}", child.title));
            }
        }
    }
    for &(from, to, connected) in &visible_edges {
        let path = draw_map_arrow(&mut grid, from, to, projection.virtual_area, &obstacles);
        if connected {
            for (x, y) in path {
                selected_grid[y][x] = grid[y][x];
            }
        }
    }
    let lines: Vec<Line> = (0..map_area.height as usize)
        .map(|screen_y| {
            let mut spans = Vec::new();
            let mut run = String::new();
            let mut run_selected = false;
            for screen_x in 0..map_area.width as usize {
                let virtual_x = projection.offset_x + screen_x;
                let virtual_y = projection.offset_y + screen_y;
                let plain = if overview || app.show_all_map_edges {
                    grid.get(virtual_y)
                        .and_then(|row| row.get(virtual_x))
                        .copied()
                        .unwrap_or(' ')
                } else {
                    ' '
                };
                let marked = selected_grid
                    .get(virtual_y)
                    .and_then(|row| row.get(virtual_x))
                    .copied()
                    .unwrap_or(' ');
                let highlighted = marked != ' ';
                if highlighted != run_selected && !run.is_empty() {
                    let color = if run_selected {
                        ACCENT
                    } else if app.show_all_map_edges && !overview {
                        Color::DarkGray
                    } else {
                        BORDER
                    };
                    spans.push(Span::styled(
                        std::mem::take(&mut run),
                        Style::default().fg(color),
                    ));
                }
                run_selected = highlighted;
                run.push(if highlighted { marked } else { plain });
            }
            if !run.is_empty() {
                let color = if run_selected {
                    ACCENT
                } else if app.show_all_map_edges && !overview {
                    Color::DarkGray
                } else {
                    BORDER
                };
                spans.push(Span::styled(run, Style::default().fg(color)));
            }
            Line::from(spans)
        })
        .collect();
    f.render_widget(Paragraph::new(Text::from(lines)), map_area);
    let visible_count = nodes.len();
    for node in nodes {
        let selected_node = node.id == selected_id;
        let color = app
            .display_canvas
            .node(&node.id)
            .and_then(|n| tree_row_color(n.effective_color.as_deref()))
            .unwrap_or(MAP_NODE_FG);
        let bg = if selected_node {
            MAP_SELECTED_BG
        } else {
            MAP_NODE_BG
        };
        let border = if selected_node { ACCENT } else { BORDER };
        let text = map_title_lines(&node.title, node.rect.width.saturating_sub(2) as usize);
        let card = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(border).bg(bg));
        f.render_widget(
            Paragraph::new(text)
                .style(Style::default().fg(color).bg(bg))
                .block(card),
            node.rect,
        );
    }
    let total = app.display_canvas.children(group_id).len();
    let hidden = if offscreen.is_empty() {
        String::new()
    } else {
        format!(" · beyond view: {}", offscreen.join(", "))
    };
    let mode = if overview {
        "arrows/hjkl: move by position".to_string()
    } else {
        format!(
            "a: {} arrows",
            if app.show_all_map_edges {
                "all"
            } else {
                "selected"
            }
        )
    };
    let status =
        format!(" {visible_count}/{total} nodes · ←{incoming} →{outgoing}{hidden} · {mode} ");
    f.render_widget(
        Paragraph::new(status).style(Style::default().fg(Color::DarkGray)),
        Rect::new(inner.x, inner.y + inner.height - 2, inner.width, 1),
    );
    let details = if edge_details.is_empty() {
        " arrows/hjkl move · Esc/⌫ fold group · Enter fold ".to_string()
    } else {
        format!(" labels: {} ", edge_details.join(" · "))
    };
    f.render_widget(
        Paragraph::new(details).style(Style::default().fg(Color::DarkGray)),
        Rect::new(inner.x, inner.y + inner.height - 1, inner.width, 1),
    );
}

/// `nodes` plus every non-root ancestor of each, by document nesting.
fn with_ancestors<'a>(
    canvas: &'a meshfox_core::Canvas,
    nodes: &std::collections::HashSet<&'a str>,
) -> std::collections::HashSet<&'a str> {
    let mut all = nodes.clone();
    for id in nodes {
        let mut cur = canvas.node(id).and_then(|n| n.parent.as_deref());
        while let Some(pid) = cur {
            let parent = canvas.node(pid);
            if parent.is_none_or(|p| p.parent.is_none()) || !all.insert(pid) {
                break;
            }
            cur = parent.and_then(|p| p.parent.as_deref());
        }
    }
    all
}

fn render_tree(f: &mut Frame, area: Rect, app: &mut App) {
    app.tree_click_rows.clear();
    if app.tree_collapsed {
        // A narrow, borderless handle — no room for a title the way
        // Output's own collapsed strip shows one (that one collapses along
        // its *height*, where a whole row is free for text; this collapses
        // along *width* instead, down to `ui::TREE_COLLAPSED_WIDTH`
        // columns). Just a repeating "▸" column instead, running the full
        // height so the same "click anywhere on it to reopen" affordance
        // `App::on_mouse` already gives Output's own strip works here too,
        // regardless of which row a click actually lands on.
        let style = pane_border(app.focus == Focus::Tree);
        let line = format!("▸{}", " ".repeat(area.width.saturating_sub(1) as usize));
        let lines: Vec<Line> = (0..area.height)
            .map(|_| Line::styled(line.clone(), style))
            .collect();
        f.render_widget(Paragraph::new(Text::from(lines)), area);
        return;
    }

    if let Some(group_id) = spatial::active_spatial_parent(app) {
        render_spatial_parent(f, area, app, &group_id);
        return;
    }

    // Borders on both sides of the list eat 2 columns of `area.width`.
    let content_width = area.width.saturating_sub(2) as usize;

    // Computed once per call (owned, no borrow conflict with `app.rows`
    // below) rather than threading `&App.service_list` all the way into
    // `tree_row_words` — a node's live status, aggregated across every
    // service belonging to it (crashed wins over running, same "worst
    // status shown" convention `ServiceBadge` uses in the webui).
    // **Experimental**, see SPEC.md's "Service blocks (experimental)".
    let mut service_by_node: HashMap<String, ServiceRowState> = HashMap::new();
    for dto in &app.service_list {
        let state = match dto.status.as_str() {
            "running" | "starting" | "stopping" | "restarting" => ServiceRowState::Running,
            "crashed" | "stop_failed" => ServiceRowState::Crashed,
            _ => continue,
        };
        let entry = service_by_node.entry(dto.node_id.clone()).or_insert(state);
        if state == ServiceRowState::Crashed {
            *entry = ServiceRowState::Crashed;
        }
    }

    // Same "aggregate per node, running wins over failed" model as the web
    // UI's own `nodeRunning`/`nodeFailed` (`MeshNode.tsx`) — see
    // `RunRowState`'s own doc comment. `running_node` is whichever single
    // node currently owns the in-flight block/file run (there's only ever
    // one foreground run at a time, see `App::advance_run`'s own doc
    // comment); `failed_nodes` is every node with a block whose latest
    // current run the worker recorded as failed or killed (`App::
    // failed_node_ids`), plus a `file` node's own last failed run.
    let mut running_nodes: std::collections::HashSet<&str> = app
        .external_running
        .keys()
        .map(|addr| addr.node_id.as_str())
        .collect();
    running_nodes.extend(
        app.run
            .as_ref()
            .and_then(super::app::RunState::current_addr)
            .map(|addr| addr.node_id.as_str()),
    );
    let failed_nodes: std::collections::HashSet<&str> = app.failed_node_ids();

    // A running/failed block also marks every non-root ancestor of its node
    // (same rule as the web UI's `runningAncestors`/`failedAncestors`), so a
    // collapsed parent still shows what's going on underneath it.
    let running_nodes = with_ancestors(&app.display_canvas, &running_nodes);
    let failed_nodes = with_ancestors(&app.display_canvas, &failed_nodes);

    let items: Vec<ListItem> = app
        .rows
        .iter()
        .map(|row| {
            let indent = "  ".repeat(row.depth);
            let disclosure = if !row.has_children {
                "  "
            } else if row.expanded {
                "▾ "
            } else {
                "▸ "
            };
            let type_mark = if row.reference.is_some() {
                "→ "
            } else {
                type_marker(row.node_type)
            };
            let title_style = match tree_row_color(row.color.as_deref()) {
                Some(c) => Style::default().fg(c),
                None => Style::default(),
            };

            let run_status = if row.reference.is_some() {
                None
            } else if running_nodes.contains(row.node_id.as_str()) {
                Some(RunRowState::Running)
            } else if failed_nodes.contains(row.node_id.as_str()) {
                Some(RunRowState::Failed)
            } else {
                None
            };
            let words = tree_row_words(
                row,
                title_style,
                service_by_node.get(&row.node_id).copied(),
                run_status,
                app.spinner_tick,
            );
            let word_widths: Vec<usize> = words.iter().map(|(t, _)| t.chars().count()).collect();
            // Row 0 also carries the indent + disclosure marker + type
            // marker; continuation rows only re-indent by the disclosure
            // marker's own width, so wrapped text still lines up under it.
            let first_width = content_width.saturating_sub(
                indent.chars().count() + disclosure.chars().count() + type_mark.chars().count(),
            );
            let cont_width =
                content_width.saturating_sub(indent.chars().count() + disclosure.chars().count());
            let wrapped = wrap_word_indices(&word_widths, first_width, cont_width);

            let lines: Vec<Line> = wrapped
                .iter()
                .enumerate()
                .map(|(row_i, word_indices)| {
                    let mut spans = Vec::new();
                    if row_i == 0 {
                        spans.push(Span::raw(indent.clone()));
                        spans.push(Span::styled(
                            disclosure,
                            Style::default().fg(Color::DarkGray),
                        ));
                        spans.push(Span::styled(
                            type_mark,
                            Style::default().fg(if row.reference.is_some() {
                                DEP
                            } else {
                                Color::DarkGray
                            }),
                        ));
                    } else {
                        spans.push(Span::raw(format!(
                            "{indent}{}",
                            " ".repeat(disclosure.chars().count())
                        )));
                    }
                    for (i, &wi) in word_indices.iter().enumerate() {
                        if i > 0 {
                            spans.push(Span::raw(" "));
                        }
                        let (text, style) = &words[wi];
                        spans.push(Span::styled(text.clone(), *style));
                    }
                    Line::from(spans)
                })
                .collect();

            ListItem::new(Text::from(lines))
        })
        .collect();

    let heights: Vec<_> = items.iter().map(ListItem::height).collect();
    app.list_state.select(Some(app.selected));

    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(pane_border(app.focus == Focus::Tree))
                .title(format!(
                    " {} ",
                    app.canvas_path
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("canvas")
                ))
                .title(fullscreen_icon_title(&*app, Focus::Tree)),
        )
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED));

    f.render_stateful_widget(list, area, &mut app.list_state);
    // ListState's offset counts items, while mouse coordinates count terminal
    // rows. Use the rendered item heights, including wrapped title lines.
    let mut y = area.y.saturating_add(1);
    let bottom = area.bottom().saturating_sub(1);
    for (index, height) in heights.iter().enumerate().skip(app.list_state.offset()) {
        if y >= bottom {
            break;
        }
        let height = (*height).min(usize::from(bottom - y)) as u16;
        app.tree_click_rows.push((
            Rect::new(area.x + 1, y, area.width.saturating_sub(2), height),
            index,
        ));
        y += height;
    }
}

#[cfg(test)]
use super::document_layout::wrapped_click_rects;
use super::document_layout::wrapped_text_layout;

/// Reveal an off-screen header with the smallest scroll adjustment.
fn reveal_header_scroll(scroll: u16, height: u16, header: u16) -> u16 {
    if header < scroll {
        header
    } else if u32::from(header) >= u32::from(scroll) + u32::from(height) {
        header.saturating_sub(height.saturating_sub(1))
    } else {
        scroll
    }
}

/// Bitmap protocols render one cached crop; Kitty/Halfblocks use native slicing.
fn render_document_image(
    image: &super::image_render::DocumentImage,
    skip: u16,
    area: Rect,
    buffer: &mut ratatui::buffer::Buffer,
) {
    use ratatui::widgets::Widget;
    if let Some(fragment) = image.visible_fragment(skip, area.height) {
        Image::new(fragment.as_ref()).render(area, buffer);
    } else if let Some(sliced) = &image.sliced {
        SlicedImage::new(sliced, SignedPosition::from((0, -(skip as i16)))).render(area, buffer);
    }
}

fn render_document(f: &mut Frame, area: Rect, app: &mut App) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(pane_border(app.focus == Focus::Document))
        .title(" Document ")
        .title(fullscreen_icon_title(app, Focus::Document));
    let inner = block.inner(area);
    f.render_widget(block, area);

    app.doc_click_targets.clear();
    if inner.height == 0 || inner.width == 0 {
        return;
    }
    app.set_document_viewport(inner.width, inner.height);
    let context = super::document_layout::LayoutContext::new(inner.as_size());
    let query = if app.search.open {
        app.search.query.trim()
    } else {
        ""
    };
    let matches = if app.search.open {
        app.search.matches(&app.display_canvas)
    } else {
        Vec::new()
    };
    let current = if app.search.open {
        matches
            .get(app.search.index.min(matches.len().saturating_sub(1)))
            .filter(|m| {
                app.rows
                    .get(app.selected)
                    .is_some_and(|r| r.node_id == m.node_id)
            })
            .map(|m| m.occurrence)
    } else {
        None
    };
    let mut layouts: Vec<_> = app
        .doc_segments
        .iter()
        .map(|seg| {
            if query.is_empty() {
                None
            } else {
                context.text(seg)
            }
        })
        .collect();
    // Source can include markup with no rendered text (e.g. a URL).
    // Like webui's DOM highlight, clamp to the last painted occurrence.
    let visible_count: usize = if query.is_empty() {
        0
    } else {
        layouts
            .iter()
            .flatten()
            .flat_map(|t| &t.lines)
            .map(|line| super::search::ranges(&line.to_string(), query).len())
            .sum()
    };
    let current = current
        .filter(|_| visible_count > 0)
        .map(|i| i.min(visible_count - 1));
    let mut occurrence = 0;
    let mut target_row = None;
    let mut preceding_rows = 0usize;
    if !query.is_empty() {
        for (segment, layout) in app.doc_segments.iter().zip(layouts.iter_mut()) {
            if let Some(text) = layout {
                for i in 0..text.lines.len() {
                    if let Some((start, end)) = super::search::highlight(
                        &mut text.lines[i],
                        query,
                        &mut occurrence,
                        current,
                    ) {
                        if let Some(rect) = text.click_rects(i, start, end).first() {
                            target_row = Some(preceding_rows + usize::from(rect.y));
                        }
                    }
                }
                preceding_rows += text.rows();
            } else if let Segment::Image { path, .. } = segment {
                preceding_rows += app
                    .doc_images
                    .get(path)
                    .and_then(|p| p.as_ref())
                    .map(|p| usize::from(p.size().height))
                    .unwrap_or(1);
            }
        }
    }
    if app.search.reveal_pending {
        if let Some(row) = target_row {
            app.doc_scroll = reveal_header_scroll(
                app.doc_scroll,
                inner.height,
                row.min(u16::MAX as usize) as u16,
            );
        }
        app.search.reveal_pending = false;
    }

    if app.reveal_selected_block {
        if let Some(addr) = &app.selected_block {
            let header = app
                .doc_click_regions
                .iter()
                .find(|r| matches!(&r.target,
                    super::markdown::ClickTarget::Block { node_id, block_name, action: super::markdown::BlockAction::ToggleBlock }
                    if node_id == &addr.node_id && block_name == &addr.block_name))
                // A `form` fence's header carries a `Select` region instead.
                .or_else(|| {
                    app.doc_click_regions.iter().find(|r| matches!(&r.target,
                        super::markdown::ClickTarget::Block { node_id, block_name, action: super::markdown::BlockAction::Select }
                        if node_id == &addr.node_id && block_name == &addr.block_name))
                })
                .or_else(|| {
                    app.doc_click_regions.iter().find(|r| matches!(&r.target,
                        super::markdown::ClickTarget::RunBlock { node_id, block_name }
                        if node_id == &addr.node_id && block_name == &addr.block_name))
                });
            if let Some(header) = header {
                let rows: usize = app
                    .doc_segments
                    .iter()
                    .take(header.segment_index)
                    .map(|seg| match seg {
                        Segment::Text(_)
                        | Segment::Button { .. }
                        | Segment::Table { .. }
                        | Segment::TablePreview(_) => context.text(seg).unwrap().rows(),
                        Segment::Image { path, .. } => app
                            .doc_images
                            .get(path)
                            .and_then(|p| p.as_ref())
                            .map(|p| p.size().height as usize)
                            .unwrap_or(1),
                    })
                    .sum();
                app.doc_scroll = reveal_header_scroll(
                    app.doc_scroll,
                    inner.height,
                    rows.min(u16::MAX as usize) as u16,
                );
            }
        }
        app.reveal_selected_block = false;
    }
    let mut y = inner.y;
    let bottom = inner.y + inner.height;
    let mut skip = app.doc_scroll;

    for (seg_idx, seg) in app.doc_segments.iter().enumerate() {
        if y >= bottom {
            break;
        }
        match seg {
            Segment::Text(_)
            | Segment::Button { .. }
            | Segment::Table { .. }
            | Segment::TablePreview(_) => {
                let text_layout = layouts[seg_idx]
                    .take()
                    .unwrap_or_else(|| context.text(seg).unwrap());
                // How far into this segment's own wrapped rows `skip`
                // reaches, *before* it gets zeroed out below — needed to
                // translate a click region's (pre-scroll) row back to
                // where it actually lands once scrolled.
                let seg_skip = skip;
                // Measurement and hit testing come from the same container layout.
                let mut text_layout = text_layout;
                // Document segments are cached between output events. Animate
                // only the live header without re-parsing Markdown or images.
                for line in &mut text_layout.lines {
                    if let Some(header) = line.spans.first_mut() {
                        if header.content.starts_with("┌─ output: ")
                            && header.content.ends_with(" · running ──")
                        {
                            header.content = header
                                .content
                                .replacen(
                                    " · ⠋ live",
                                    &format!(" · {} live", spinner_frame(app.spinner_tick)),
                                    1,
                                )
                                .into();
                        }
                    }
                }
                let paragraph = text_layout.paragraph();
                let total = text_layout.rows().min(u16::MAX as usize) as u16;
                let layout = match wrapped_text_layout(total, skip, bottom - y) {
                    None => {
                        skip -= total;
                        continue;
                    }
                    Some(l) => l,
                };
                skip = 0;
                let rect = Rect {
                    x: inner.x,
                    y,
                    width: inner.width,
                    height: layout.height,
                };
                f.render_widget(paragraph.scroll((layout.row_offset, 0)), rect);

                // Any `ClickRegion`s targeting this segment, translated
                // into real on-screen cells — `App::on_mouse` hit-tests
                // against these, not `doc_click_regions` directly, so it
                // never has to redo this segment's own wrap/scroll math
                // itself.
                if app
                    .doc_click_regions
                    .iter()
                    .any(|r| r.segment_index == seg_idx)
                {
                    for region in app
                        .doc_click_regions
                        .iter()
                        .filter(|r| r.segment_index == seg_idx)
                    {
                        for local in text_layout.click_rects(
                            region.line_index,
                            region.col_start,
                            region.col_end,
                        ) {
                            let row = local.y;
                            if row < seg_skip || row - seg_skip >= layout.height {
                                continue;
                            }
                            app.doc_click_targets.push((
                                Rect::new(inner.x + local.x, y + row - seg_skip, local.width, 1),
                                region.target.clone(),
                            ));
                        }
                    }
                }

                y += layout.height;
            }
            Segment::Image { path, alt, .. } => {
                let protocol = app.doc_images.get(path).and_then(|o| o.as_ref());
                let rows = protocol.map(|p| p.size().height).unwrap_or(1);
                let Some(layout) = wrapped_text_layout(rows, skip, bottom - y) else {
                    skip -= rows;
                    continue;
                };
                let height = layout.height;
                let rect = Rect {
                    x: inner.x,
                    y,
                    width: inner
                        .width
                        .min(protocol.map(|p| p.size().width).unwrap_or(inner.width)),
                    height,
                };
                match protocol {
                    Some(p) => render_document_image(p, skip, rect, f.buffer_mut()),
                    None if app.image_pending.contains(path) => f.render_widget(
                        Paragraph::new(format!("[image loading: {alt}]"))
                            .style(Style::default().fg(Color::DarkGray)),
                        rect,
                    ),
                    None => f.render_widget(
                        Paragraph::new(format!("[image failed to load: {alt}]"))
                            .style(Style::default().fg(FAIL)),
                        rect,
                    ),
                }
                skip = 0;
                y += height;
            }
        }
    }
}

fn render_output(f: &mut Frame, area: Rect, app: &App) {
    let title = if !app.external_running.is_empty() {
        " Output (running) ".to_string()
    } else if let Some(run) = &app.run {
        if run.is_running() {
            " Output (running — K to kill) ".to_string()
        } else if run.killed {
            " Output (killed) ".to_string()
        } else if run.had_failure {
            " Output (failed) ".to_string()
        } else {
            " Output (done) ".to_string()
        }
    } else {
        " Output ".to_string()
    };

    if app.console_collapsed {
        // A 1-row strip (see `compute_layout`) — no border (there's no room
        // for one plus content), just the same title text a click here
        // expands back into the full pane.
        // A status message ("saved — merged with changes made elsewhere",
        // "reloaded — file changed on disk") would otherwise be invisible
        // here: it is drawn in the pane body, which this strip hides.
        let text = if app.status.is_empty() {
            format!("▸{}", title.trim())
        } else {
            format!("▸{} — {}", title.trim(), app.status)
        };
        let line = Line::styled(text, pane_border(app.focus == Focus::Output));
        f.render_widget(Paragraph::new(line), area);
        return;
    }

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(pane_border(app.focus == Focus::Output))
        .title(title)
        .title(fullscreen_icon_title(app, Focus::Output));
    let inner = block.inner(area);
    f.render_widget(block, area);

    // The plain `==>`/exit-code transcript, always shown as-is — plus, once
    // a step whose own block declared `output="markdown"` is done, that
    // step's captured stdout rendered as real Markdown right below it (see
    // `RunState::output_markdown`'s own doc comment). Mirrors the "raw
    // while running, rendered once done" treatment the web UI's own live
    // view (`MeshNode.tsx`'s `LiveRunOutput`) already gives it — until now
    // this pane never looked at `output=` at all, so a `cache`-less
    // `output="markdown"` block (nothing for `write_output` to ever splice
    // into the Document pane) had nowhere it would *ever* render as
    // anything but raw text.
    let content: Option<Vec<Line<'static>>> = if let Some(run) = &app.run {
        let mut lines: Vec<Line<'static>> = super::ansi::lines(&run.lines.join("\n"));
        if run.finished && run.output_markdown && !run.stdout_only.trim().is_empty() {
            lines.push(Line::from(""));
            lines.push(Line::styled(
                "── rendered markdown ──",
                Style::default().fg(super::theme::DEP),
            ));
            let base_dir = app
                .canvas_path
                .parent()
                .unwrap_or_else(|| std::path::Path::new("."))
                .to_path_buf();
            let node_id = run.chain.last().map(|a| a.node_id.as_str()).unwrap_or("");
            let (segs, _clicks) = super::markdown::render(
                &run.stdout_only,
                &base_dir,
                &app.highlighter,
                node_id,
                &[],
                &HashMap::new(),
                None,
                &HashMap::new(),
            );
            for seg in segs {
                let context = super::document_layout::LayoutContext::new(inner.as_size());
                if let Some(text) = context.text(&seg) {
                    lines.extend(text.into_rows());
                }
            }
        }
        Some(lines)
    } else {
        None
    };
    let content = if app.external_console_lines.is_empty() {
        content
    } else {
        let mut lines = content.unwrap_or_default();
        lines.extend(super::ansi::lines(&app.external_console_lines.join("\n")));
        Some(lines)
    };
    let text: Text = if let Some(lines) = content {
        // `output_scroll` counts lines back from the bottom (see its own
        // doc comment) — clamped here (not in `App::scroll_output`, same
        // "state is unclamped, rendering clamps" convention `doc_scroll`/
        // `wrapped_text_layout` already use) so scrolling can't go past
        // the very first line.
        let take = inner.height as usize;
        let max_scroll = lines.len().saturating_sub(take);
        let scroll = (app.output_scroll as usize).min(max_scroll);
        let end = lines.len() - scroll;
        let start = end.saturating_sub(take);
        Text::from(lines[start..end].to_vec())
    } else if !app.status.is_empty() {
        Text::from(Line::from(Span::styled(
            app.status.as_str(),
            Style::default().fg(ACCENT),
        )))
    } else {
        Text::from(Line::from(Span::styled(
            "select a node and press r to run its block (R to run without deps)",
            Style::default().fg(Color::DarkGray),
        )))
    };
    // `output_hscroll` counts columns scrolled right (see its own doc
    // comment) — clamped here against the *currently visible* lines' own
    // longest width, same "state is unclamped, rendering clamps"
    // convention `output_scroll` above already uses, so scrolling right
    // can't go past a line's own last column.
    let max_hscroll = (text.width() as u16).saturating_sub(inner.width);
    let hscroll = app.output_hscroll.min(max_hscroll);
    f.render_widget(Paragraph::new(text).scroll((0, hscroll)), inner);
}

fn render_footer(f: &mut Frame, area: Rect, app: &App) {
    let mut hint = String::from(
        "tab focus · f fullscreen focused pane · z collapse focused pane · j/k move/scroll · enter expand/follow link · h/l collapse/expand · r run · R run (no deps) · K kill · e edit",
    );
    if app.focus == Focus::Document {
        hint = if let Some(addr) = &app.selected_block {
            if let Some(label) = app.render_block_label(addr) {
                format!("render: {label} · [/] select · Enter/c source · Esc clear · j/k scroll · tab focus")
            } else if app.is_button_block(addr) {
                format!("button: {} · Enter/r run chain · R run without deps · [/] select · Esc clear · tab focus", addr.block_name)
            } else {
                format!("block: {} · [/] select · Enter fold · c code · r chain · R run · L history · Esc clear · j/k scroll · tab focus", addr.block_name)
            }
        } else {
            "[/] select block · r run · R run (no deps) · L history · j/k scroll · tab focus".into()
        };
    }
    if spatial::active_spatial_parent(app).is_some() {
        hint.push_str(" · a all/selected arrows");
    }
    if app.selected_is_open_target() {
        hint.push_str(" · o open");
    }
    if app.selected_is_table() && app.worker_port.is_some() {
        hint.push_str(" · enter open table");
    }
    if app.has_configurable_vars()
        && !(app.focus == Focus::Document && app.selected_block.is_some())
    {
        hint.push_str(" · c configure");
    }
    if app.service_stats.is_some() {
        hint.push_str(" · v services");
    }
    if app.worker_port.is_some() {
        hint.push_str(" · t live terminals · H history · L run history");
    }
    hint.push_str(" · / search · ? help · q quit");

    let mut spans = Vec::new();
    if let Some((total, failed)) = app.constraint_stats {
        let (text, color) = if failed > 0 {
            (format!("{failed}/{total} constraints failing"), FAIL)
        } else {
            (
                format!(
                    "all {total} constraint{} pass",
                    if total == 1 { "" } else { "s" }
                ),
                OK,
            )
        };
        spans.push(Span::styled(text, Style::default().fg(color)));
        spans.push(Span::styled("  ·  ", Style::default().fg(Color::DarkGray)));
    }
    // **Experimental**, see SPEC.md's "Service blocks (experimental)" —
    // recomputed by `App::tick_services`, not derived here (unlike
    // `constraint_stats`, service status changes on its own schedule, not
    // just on a document reload).
    if let Some((running, crashed)) = app.service_stats {
        let (text, color) = if crashed > 0 {
            (format!("{crashed} service(s) crashed"), FAIL)
        } else {
            (format!("{running} service(s) running"), OK)
        };
        spans.push(Span::styled(text, Style::default().fg(color)));
        spans.push(Span::styled("  ·  ", Style::default().fg(Color::DarkGray)));
    }
    spans.push(Span::styled(hint, Style::default().fg(Color::DarkGray)));

    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    let x = area.x + (area.width.saturating_sub(width)) / 2;
    let y = area.y + (area.height.saturating_sub(height)) / 2;
    Rect {
        x,
        y,
        width,
        height,
    }
}

/// Every field in `vf` at once, one per row — arrow keys/Tab move which
/// row is focused (highlighted), typing edits only that row's value, and
/// Enter submits every row's current value together, same "whole form at
/// once" shape as the web UI's `VarsForm` rather than one field at a time.
/// Keeps a growing text field's own cursor on screen — `List`/`Line`
/// (unlike edtui's own source editor, see `source_editor.rs`'s
/// `prime_viewport`) never wrap or scroll on their own, so a value once it
/// outgrew the row's width used to just push its trailing cursor marker
/// off the right edge of the terminal, with no way to bring it back into
/// view (TODO.canvas.md: "Горизонтальная промотка в TUI-редакторе").
/// Every field this renders only ever grows/shrinks from its own right end
/// (`push`/`pop` — see `App::on_key`), so the tail is always where the
/// action (and the cursor right after it) is — showing the last `max`
/// characters keeps that in view, at the cost of scrolling the *front* of
/// a long value out of sight instead; a leading "…" marks when that's
/// happened. `max` is a character count, not a byte count — matches every
/// other width calculation in this module (see e.g. `masked` above).
fn tail_fit(s: &str, max: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= max {
        return s.to_string();
    }
    if max <= 1 {
        return chars[chars.len() - max..].iter().collect();
    }
    let start = chars.len() - (max - 1);
    format!("…{}", chars[start..].iter().collect::<String>())
}

/// The var form modal's own outer rect for `decl_count` fields in `area` —
/// factored out of `render_var_form` so `var_form_list_rect` (below) can
/// derive the same inner list rows rect from it without duplicating this
/// sizing math.
fn var_form_rect(area: Rect, decl_count: usize) -> Rect {
    let height = (decl_count.saturating_mul(2).saturating_add(4)).min(area.height as usize) as u16;
    centered_rect(64, height, area)
}

/// The var form's own list rows rect for `decl_count` fields in `area` —
/// shared by `render_var_form` and `App::on_mouse`'s modal hit-testing, so
/// the two can never drift apart (same idea as `PaneLayout`/
/// `compute_layout`).
pub(super) fn var_form_list_rect(area: Rect, decl_count: usize) -> Rect {
    let rect = var_form_rect(area, decl_count);
    let inner = Block::default().borders(Borders::ALL).inner(rect);
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(inner);
    rows[0]
}

/// Keep the selected two-line field visible and share the scroll offset with mouse input.
pub(super) fn var_form_offset(rows: Rect, selected: usize) -> usize {
    selected.saturating_sub((rows.height as usize / 2).max(1) - 1)
}

fn render_var_form(f: &mut Frame, area: Rect, vf: &super::app::VarFormState) {
    let rect = var_form_rect(area, vf.decls.len());
    f.render_widget(Clear, rect);
    let title = if vf.arguments {
        " arguments for this application "
    } else if vf.configuring {
        " configure variables "
    } else {
        " variables needed "
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(ACCENT))
        .title(title);
    let inner = block.inner(rect);
    f.render_widget(block, rect);

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(inner);

    let items: Vec<ListItem> = vf
        .decls
        .iter()
        .zip(vf.inputs.iter())
        .zip(vf.origins.iter())
        .enumerate()
        .map(|(i, ((decl, input), origin))| {
            let masked;
            let shown: &str = if decl.secret {
                masked = "*".repeat(input.chars().count());
                &masked
            } else {
                input.as_str()
            };
            // `Bool`/`Select` are a left/right toggle/cycle, not free text
            // (see `App::cycle_var_form_field`) — the `‹ ›` framing marks
            // that visually, on every such row, not just the focused one,
            // same way `VarsForm` renders them as a checkbox/dropdown
            // rather than a text input. `String`/`Int` keep the plain
            // text-cursor look, shown only on the focused row.
            let value = match decl.var_type {
                VarType::Bool | VarType::Select => format!("‹ {shown} ›"),
                VarType::String | VarType::Int | VarType::Dir | VarType::File => {
                    format!("{shown}{}", if i == vf.selected { "_" } else { "" })
                }
            };
            // `[shared]`/`[global]` when this field's value was inherited
            // from a project-/global-config `[[env]]` section (see
            // `meshfox_core::shared_env`) — the terminal counterpart to
            // the web `VarsForm`'s "inherited" badge. Typing over the
            // field just overrides it, same as any other answer.
            let origin_tag = match origin {
                Some(meshfox_core::SharedOrigin::Project) => " [shared]",
                Some(meshfox_core::SharedOrigin::Global { .. }) => " [global]",
                None => "",
            };
            // A `secret` field's "save" toggle (Ctrl-S), labelled by where
            // it would go (`secret_store`), and — if the store couldn't be
            // read — why the field is being asked for again.
            let save_tag = if decl.secret && vf.configuring {
                // Configure: typing a value saves it (empty leaves it
                // alone), so there's no save toggle — just what's there.
                let state = if vf.clear[i] {
                    "[x] clear stored value"
                } else if vf.stored[i] == Some(true) {
                    "stored in keychain · ^d clear"
                } else if origin.is_some() {
                    "type to override"
                } else {
                    "not set"
                };
                format!(" ({state})")
            } else if decl.secret {
                let label = if vf.secret_store == "keychain" {
                    "keychain"
                } else {
                    "plaintext"
                };
                format!(" [{}] save→{label}", if vf.save[i] { "x" } else { " " })
            } else {
                String::new()
            };
            let error_tag = vf.errors[i]
                .as_ref()
                .map(|e| format!(" ⚠ {e}"))
                .unwrap_or_default();
            let prefix = "  ";
            // A `dir`/`file` field ends in a right-aligned, clickable
            // "[Browse… ^o]" (`App::on_modal_mouse` hit-tests the same
            // `BROWSE_MARKER`-wide strip at the row's right edge).
            let browse_width = if matches!(decl.var_type, VarType::Dir | VarType::File) {
                super::path_picker::BROWSE_MARKER.chars().count() + 2
            } else {
                0
            };
            let value_budget = (inner.width as usize).saturating_sub(
                prefix.chars().count()
                    + save_tag.chars().count()
                    + error_tag.chars().count()
                    + browse_width,
            );
            let value = tail_fit(&value, value_budget.max(8));
            let mut spans = vec![
                Span::raw(prefix),
                Span::styled(value.clone(), Style::default().fg(Color::LightGreen)),
                Span::styled(save_tag.clone(), Style::default().fg(Color::DarkGray)),
                Span::styled(error_tag.clone(), Style::default().fg(Color::Red)),
            ];
            if browse_width > 0 {
                let used = prefix.chars().count()
                    + value.chars().count()
                    + save_tag.chars().count()
                    + error_tag.chars().count();
                let pad = (inner.width as usize)
                    .saturating_sub(used + super::path_picker::BROWSE_MARKER.chars().count());
                spans.push(Span::raw(" ".repeat(pad.max(2))));
                spans.push(Span::styled(
                    super::path_picker::BROWSE_MARKER,
                    Style::default()
                        .fg(ACCENT)
                        .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
                ));
            }
            ListItem::new(vec![
                Line::from(vec![
                    Span::raw(decl.prompt.clone()),
                    Span::styled(origin_tag, Style::default().fg(Color::DarkGray)),
                ]),
                Line::from(spans),
            ])
        })
        .collect();

    let mut state = ListState::default();
    state.select(Some(vf.selected));
    *state.offset_mut() = var_form_offset(rows[0], vf.selected);
    let list = List::new(items).highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    f.render_stateful_widget(list, rows[0], &mut state);

    let esc_hint = if vf.configuring {
        "↑/↓/tab field · ←/→ toggle/cycle · ^d clear secret · enter save all · esc cancel configure"
    } else {
        "↑/↓/tab field · ←/→ toggle/cycle · ^s save secret · enter confirm all · esc cancel run"
    };
    let browse = vf
        .decls
        .get(vf.selected)
        .is_some_and(|d| matches!(d.var_type, VarType::Dir | VarType::File));
    let esc_hint = if browse {
        format!("^o browse · {esc_hint}")
    } else {
        esc_hint.to_string()
    };
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            esc_hint,
            Style::default().fg(Color::DarkGray),
        ))),
        rows[1],
    );
}

/// Every rect of the path picker modal, shared by `render_path_picker` and
/// `App::on_path_picker_mouse` so a click can't drift from what is drawn.
pub(super) struct PathPickerLayout {
    pub outer: Rect,
    pub location: Rect,
    pub flags: Rect,
    pub list: Rect,
    /// The file-name line — `mode="save"` pickers only.
    pub name: Option<Rect>,
    pub footer: Rect,
}

pub(super) fn path_picker_layout(area: Rect, save: bool) -> PathPickerLayout {
    let outer = centered_rect(100, 26, area);
    let inner = Block::default().borders(Borders::ALL).inner(outer);
    let mut constraints = vec![
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(1),
    ];
    if save {
        constraints.push(Constraint::Length(1));
    }
    constraints.push(Constraint::Length(1));
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(inner);
    PathPickerLayout {
        outer,
        location: rows[0],
        flags: rows[1],
        list: rows[2],
        name: save.then(|| rows[3]),
        footer: rows[rows.len() - 1],
    }
}

fn human_size(n: u64) -> String {
    if n < 1024 {
        return format!("{n} B");
    }
    let mut v = n as f64 / 1024.0;
    for unit in ["KB", "MB", "GB"] {
        if v < 1024.0 {
            return if v < 10.0 {
                format!("{v:.1} {unit}")
            } else {
                format!("{:.0} {unit}", v)
            };
        }
        v /= 1024.0;
    }
    format!("{:.0} TB", v)
}

/// The `Ctrl-O` file/directory chooser — see `super::path_picker`.
fn render_path_picker(f: &mut Frame, area: Rect, pp: &super::path_picker::PathPickerState) {
    use super::path_picker::{window_start, PickerFocus};
    let layout = path_picker_layout(area, pp.save);
    f.render_widget(Clear, layout.outer);
    let what = match (pp.kind, pp.save) {
        (meshfox_core::fsbrowse::PickKind::Dir, _) => "directory",
        (_, true) => "output file",
        (_, false) => "file",
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(ACCENT))
        .title(format!(" {} — choose {what} ", pp.title));
    f.render_widget(block, layout.outer);

    let location = pp.listing.as_ref().map(|l| l.path.as_str()).unwrap_or("…");
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("in ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                tail_fit(
                    location,
                    (layout.location.width as usize).saturating_sub(3).max(8),
                ),
                Style::default().add_modifier(Modifier::BOLD),
            ),
        ])),
        layout.location,
    );

    let on_off = |b: bool| if b { "on" } else { "off" };
    let mut flags = vec![
        Span::styled("/ filter: ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            if pp.focus == PickerFocus::Filter {
                format!("{}_", pp.filter)
            } else if pp.filter.is_empty() {
                "—".to_string()
            } else {
                pp.filter.clone()
            },
            Style::default().fg(Color::LightGreen),
        ),
        Span::styled(
            format!("   . hidden: {}", on_off(pp.show_hidden)),
            Style::default().fg(Color::DarkGray),
        ),
    ];
    if pp.kind == meshfox_core::fsbrowse::PickKind::File && !pp.ext.is_empty() {
        flags.push(Span::styled(
            format!("   e .{} only: {}", pp.ext.join(" ."), on_off(pp.only_ext)),
            Style::default().fg(Color::DarkGray),
        ));
    }
    f.render_widget(Paragraph::new(Line::from(flags)), layout.flags);

    let rows = pp.rows();
    let height = layout.list.height as usize;
    let start = window_start(pp.selected, height);
    let items: Vec<ListItem> = rows
        .iter()
        .skip(start)
        .take(height)
        .map(|r| {
            let (marker, style) = if r.is_dir {
                ("▸ ", Style::default().fg(ACCENT))
            } else {
                ("  ", Style::default())
            };
            let name = if r.is_dir && !r.is_parent {
                format!("{}/", r.name)
            } else if r.is_parent {
                "../".to_string()
            } else {
                r.name.clone()
            };
            let mut spans = vec![Span::raw(marker), Span::styled(name, style)];
            if let Some(n) = r.size {
                spans.push(Span::styled(
                    format!("  {}", human_size(n)),
                    Style::default().fg(Color::DarkGray),
                ));
            }
            ListItem::new(Line::from(spans))
        })
        .collect();
    let mut state = ListState::default();
    if !rows.is_empty() {
        state.select(Some(pp.selected - start));
    }
    let list = List::new(items).highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    f.render_stateful_widget(list, layout.list, &mut state);
    if rows.is_empty() {
        let msg = match (&pp.error, &pp.listing) {
            (Some(e), _) => format!("⚠ {e}"),
            (None, Some(_)) => "(nothing here)".to_string(),
            (None, None) => "loading…".to_string(),
        };
        f.render_widget(
            Paragraph::new(Span::styled(msg, Style::default().fg(Color::DarkGray))),
            layout.list,
        );
    }

    if let Some(name_rect) = layout.name {
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("name: ", Style::default().fg(Color::DarkGray)),
                Span::styled(
                    if pp.focus == PickerFocus::Name {
                        format!("{}_", pp.name)
                    } else {
                        pp.name.clone()
                    },
                    Style::default().fg(Color::LightGreen),
                ),
            ])),
            name_rect,
        );
    }

    let hint = match pp.focus {
        PickerFocus::Filter => "type to filter · enter/tab list · esc clear".to_string(),
        PickerFocus::Name => "type a file name · enter choose · tab/esc list".to_string(),
        PickerFocus::List => {
            let choose = match (pp.kind, pp.save) {
                (meshfox_core::fsbrowse::PickKind::Dir, _) => "s choose this dir",
                (_, true) => "tab name",
                (_, false) => "enter choose",
            };
            format!("↑/↓ move · enter open · ⌫ up · ~ home · c canvas · {choose} · esc cancel")
        }
    };
    let footer = match (&pp.error, rows.is_empty()) {
        (Some(e), false) => format!("⚠ {e}"),
        _ => hint,
    };
    f.render_widget(
        Paragraph::new(Span::styled(footer, Style::default().fg(Color::DarkGray))),
        layout.footer,
    );
}

/// The block picker modal's own outer rect for `block_count` blocks in
/// `area` — see `var_form_rect`'s own doc comment for why this is factored
/// out the same way.
fn block_picker_rect(area: Rect, block_count: usize) -> Rect {
    let height = (block_count as u16 + 4).min(area.height);
    centered_rect(56, height, area)
}

/// The block picker's own list rect for `block_count` blocks in `area` —
/// see `var_form_list_rect`'s own doc comment; same sharing reasoning.
pub(super) fn block_picker_list_rect(area: Rect, block_count: usize) -> Rect {
    let rect = block_picker_rect(area, block_count);
    Block::default().borders(Borders::ALL).inner(rect)
}

fn render_block_picker(f: &mut Frame, area: Rect, bp: &super::app::BlockPickerState) {
    let rect = block_picker_rect(area, bp.blocks.len());
    f.render_widget(Clear, rect);
    let mode = if bp.stats {
        "statistics"
    } else if bp.history {
        "run history"
    } else if bp.with_deps {
        "run (with deps)"
    } else {
        "run (no deps)"
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(ACCENT))
        .title(format!(" {} — which block? ", mode));
    let inner = block.inner(rect);
    f.render_widget(block, rect);

    let items: Vec<ListItem> = bp
        .blocks
        .iter()
        .map(|b| {
            let mut flags = Vec::new();
            if b.is_default {
                flags.push("default");
            }
            if b.is_button {
                flags.push("button");
            }
            if b.cache {
                flags.push("cache");
            }
            if b.tty {
                flags.push("tty");
            }
            let badge = if flags.is_empty() {
                String::new()
            } else {
                format!("  [{}]", flags.join(","))
            };
            ListItem::new(Line::from(vec![
                Span::raw(b.name.clone()),
                Span::styled(badge, Style::default().fg(OK)),
            ]))
        })
        .collect();

    let mut state = ListState::default();
    state.select(Some(bp.selected));
    let list = List::new(items).highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    f.render_stateful_widget(list, inner, &mut state);
}

/// The `S` (reset session) confirm prompt — see `App::reset_session_confirm`
/// and `App::on_reset_session_confirm_key`. Fixed size rather than
/// `render_block_picker`/`render_var_form`'s content-driven height (`bp
/// .blocks.len()`/`vf.decls.len()`) since this has no list to size around,
/// just the one static message.
fn render_reset_session_confirm(f: &mut Frame, area: Rect) {
    let rect = centered_rect(60, 8, area);
    f.render_widget(Clear, rect);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(ACCENT))
        .title(" reset session? ");
    let inner = block.inner(rect);
    f.render_widget(block, rect);

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(inner);

    let message = Paragraph::new(
        "Forgets which blocks already ran successfully this session, so the next chain run \
         re-runs every dependency instead of skipping unchanged ones. Doesn't touch the canvas \
         file or any saved output.",
    )
    .wrap(Wrap { trim: true });
    f.render_widget(message, rows[0]);

    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "y/enter confirm · n/esc cancel",
            Style::default().fg(Color::DarkGray),
        ))),
        rows[1],
    );
}

/// A `service` step's lock-conflict confirm — see `App::service_conflict`
/// and `App::on_service_conflict_key`. Same fixed-size shape as
/// `render_reset_session_confirm`. **Experimental**, see SPEC.md's
/// "Service blocks (experimental)".
fn render_service_conflict(f: &mut Frame, area: Rect, conflict: &ServiceConflictState) {
    let rect = centered_rect(60, 8, area);
    f.render_widget(Clear, rect);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(FAIL))
        .title(" service already running ");
    let inner = block.inner(rect);
    f.render_widget(block, rect);

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(inner);

    let message = Paragraph::new(format!(
        "{:?} is already running elsewhere — pid {}, started via {}. Kill that process and start a fresh one here, or cancel and leave it running.",
        conflict.block_name, conflict.owner_pid, conflict.owner_desc
    ))
    .wrap(Wrap { trim: true });
    f.render_widget(message, rows[0]);

    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "y/enter kill & start · n/esc cancel",
            Style::default().fg(Color::DarkGray),
        ))),
        rows[1],
    );
}

/// The `v` services list view (`App::services_view`) — every `service`
/// this process knows about at once, sorted the same stable
/// `(node_id, block_name)` order `App::sorted_service_keys` uses so the
/// selected row stays put between frames. **Experimental**, see SPEC.md's
/// "Service blocks (experimental)".
/// One row's worth of pre-rendered display for `render_services_view` —
/// built once, up front, from `app.service_list` (the worker-routed poll,
/// see that field's own doc comment).
struct ServiceRow {
    block_name: String,
    line: Line<'static>,
}

fn render_services_view(f: &mut Frame, area: Rect, app: &App, sv: &ServicesViewState) {
    let mut list: Vec<&crate::worker_client::ServiceDto> = app.service_list.iter().collect();
    list.sort_by(|a, b| (&a.node_id, &a.block).cmp(&(&b.node_id, &b.block)));
    let rows: Vec<ServiceRow> = list
        .into_iter()
        .map(|dto| {
            let line = match dto.status.as_str() {
                "running" | "starting" | "stopping" | "restarting" | "stop_failed" => {
                    Line::from(vec![
                        Span::raw(format!("{}  ", dto.block)),
                        Span::styled(
                            format!("{} · pid {}", dto.status.replace('_', " "), dto.pid),
                            Style::default().fg(if dto.status == "stop_failed" {
                                FAIL
                            } else {
                                OK
                            }),
                        ),
                        Span::styled(
                            format!("  {}", dto.node_id),
                            Style::default().fg(Color::DarkGray),
                        ),
                    ])
                }
                "crashed" => Line::from(vec![
                    Span::raw(format!("{}  ", dto.block)),
                    Span::styled(
                        format!("crashed (exit {})", dto.exit_code.unwrap_or(-1)),
                        Style::default().fg(FAIL),
                    ),
                    Span::styled(
                        format!("  {}", dto.node_id),
                        Style::default().fg(Color::DarkGray),
                    ),
                ]),
                _ => Line::from(vec![
                    Span::raw(format!("{}  ", dto.block)),
                    Span::styled("stopped", Style::default().fg(Color::DarkGray)),
                    Span::styled(
                        format!("  {}", dto.node_id),
                        Style::default().fg(Color::DarkGray),
                    ),
                ]),
            };
            ServiceRow {
                block_name: dto.block.clone(),
                line,
            }
        })
        .collect();
    let selected = sv.selected.min(rows.len().saturating_sub(1));

    // Large, near-fullscreen (not `block_picker_rect`'s content-sized
    // shape) — unlike a plain picker, this also needs room to actually
    // show a service's own retained log underneath the list.
    let width = (area.width * 9 / 10).max(30).min(area.width);
    let height = (area.height * 4 / 5).max(10).min(area.height);
    let rect = centered_rect(width, height, area);
    f.render_widget(Clear, rect);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(ACCENT))
        .title(" services ");
    let inner = block.inner(rect);
    f.render_widget(block, rect);

    let list_height = (rows.len() as u16 + 2).clamp(3, 8);
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(list_height),
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(inner);
    let (list_area, log_title_area, log_area, hint_area) =
        (layout[0], layout[1], layout[2], layout[3]);

    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "j/k select · s stop · r restart · q/esc close",
            Style::default().fg(Color::DarkGray),
        ))),
        hint_area,
    );

    let items: Vec<ListItem> = rows.iter().map(|r| ListItem::new(r.line.clone())).collect();

    let mut state = ListState::default();
    state.select(Some(selected));
    let list = List::new(items).highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    f.render_stateful_widget(list, list_area, &mut state);

    // The selected service's own retained output — `app.service_log`'s
    // last poll (see that field's own doc comment — an async fetch has no
    // place in a synchronous render function). Always the *tail* (whatever
    // fits `log_area`'s own height), auto-following as more arrives — no
    // manual scrollback yet, same "good enough for now" scope as everything
    // else marked **experimental** here.
    if let Some(row) = rows.get(selected) {
        f.render_widget(
            Line::from(Span::styled(
                format!("── {} log ──", row.block_name),
                Style::default().fg(Color::DarkGray),
            )),
            log_title_area,
        );
        let log: Vec<(meshfox_server::stream_exec::OutputStream, String)> = app.service_log.clone();
        {
            let take = log_area.height as usize;
            let start = log.len().saturating_sub(take);
            let mut ansi = super::ansi::Ansi::default();
            let lines: Vec<Line> = log
                .iter()
                .map(|(stream, text)| {
                    let style = match stream {
                        meshfox_server::stream_exec::OutputStream::Stderr => {
                            Style::default().fg(FAIL)
                        }
                        meshfox_server::stream_exec::OutputStream::Stdout => Style::default(),
                    };
                    ansi.line(text, style)
                })
                .collect();
            f.render_widget(
                Paragraph::new(Text::from(lines[start..].to_vec())),
                log_area,
            );
        }
    }
}

/// The `t` live-terminals view — every `tty` session the shared worker
/// currently knows about (`App::live_tty_sessions`, from `GET /api/runs`),
/// whoever started it. A flat list, no per-item detail body the way
/// `render_services_view` has (there's no retained log to show — the
/// session's own live bytes are the point, only visible once actually
/// attached) — same "closer to a plain picker than a full panel" shape
/// the web UI's own `TtySessionsPanel` has.
fn render_tty_sessions_view(
    f: &mut Frame,
    area: Rect,
    app: &App,
    tv: &super::app::TtySessionsViewState,
) {
    let sessions = &app.live_tty_sessions;
    let selected = tv.selected.min(sessions.len().saturating_sub(1));

    let rect = block_picker_rect(area, sessions.len());
    f.render_widget(Clear, rect);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(ACCENT))
        .title(" live terminals ");
    let inner = block.inner(rect);
    f.render_widget(block, rect);

    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(inner);
    let (list_area, hint_area) = (layout[0], layout[1]);

    let items: Vec<ListItem> = sessions
        .iter()
        .map(|s| {
            ListItem::new(Line::from(vec![
                Span::raw(format!("{}  ", s.block)),
                Span::styled(
                    format!(
                        "running · {}",
                        meshfox_core::format_duration_ms(s.uptime_ms)
                    ),
                    Style::default().fg(OK),
                ),
                Span::styled(
                    format!("  {}", s.node_id),
                    Style::default().fg(Color::DarkGray),
                ),
            ]))
        })
        .collect();
    let mut state = ListState::default();
    state.select(Some(selected));
    let list = List::new(items).highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    f.render_stateful_widget(list, list_area, &mut state);

    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "j/k select · enter attach · K kill · q/esc close",
            Style::default().fg(Color::DarkGray),
        ))),
        hint_area,
    );
}

/// `created_at` as a short relative label ("just now", "5m ago", "3h ago",
/// "2d ago") — same as the web UI's history panel, and it sidesteps the
/// log's UTC timestamps vs. the viewer's timezone altogether. Empty for the
/// synthetic "before the first step" row (no timestamp) or an unparsable one.
fn history_time(created_at: &str) -> String {
    let Some(ts) = meshfox_core::timestamp::unix_timestamp(created_at) else {
        return String::new();
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(ts);
    let sec = (now - ts).max(0);
    match sec {
        0..=4 => "just now".to_string(),
        5..=59 => format!("{sec}s ago"),
        60..=3599 => format!("{}m ago", (sec + 30) / 60),
        3600..=86399 => format!("{}h ago", (sec + 1800) / 3600),
        _ => format!("{}d ago", (sec + 43200) / 86400),
    }
}

/// The `H` history view: one row per undo-log step, newest first. Steps
/// still applied read normally; the redo tail (undone, `applied == false`)
/// is dimmed, and the newest applied step — the canvas's current state — is
/// marked. `enter` jumps to the highlighted row.
/// The palette's statistics view: `meshfox stats`' text, scrollable.
fn render_stats_view(f: &mut Frame, area: Rect, sv: &super::app::StatsViewState) {
    let rect = centered_rect(area.width * 9 / 10, area.height * 8 / 10, area);
    f.render_widget(Clear, rect);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(ACCENT))
        .title(" statistics ");
    let inner = block.inner(rect);
    f.render_widget(block, rect);
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(inner);
    let mut ansi = super::ansi::Ansi::default();
    let lines: Vec<Line> = sv
        .lines
        .iter()
        .skip(sv.scroll)
        .take(usize::from(layout[0].height))
        .map(|l| ansi.line(l, Style::default()))
        .collect();
    f.render_widget(Paragraph::new(lines), layout[0]);
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "j/k scroll · PgUp/PgDn · g/G top/bottom · q/esc close",
            Style::default().fg(Color::DarkGray),
        ))),
        layout[1],
    );
}

fn render_history_view(f: &mut Frame, area: Rect, hv: &super::app::HistoryViewState) {
    let current = hv.entries.iter().position(|e| e.applied);
    let rect = centered_rect(72, (hv.entries.len() as u16 + 4).min(area.height), area);
    f.render_widget(Clear, rect);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(ACCENT))
        .title(" history ");
    let inner = block.inner(rect);
    f.render_widget(block, rect);

    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(inner);
    let (list_area, hint_area) = (layout[0], layout[1]);

    let items: Vec<ListItem> = hv
        .entries
        .iter()
        .enumerate()
        .map(|(i, e)| {
            let marker = if Some(i) == current { "▶ " } else { "  " };
            let style = if e.applied {
                Style::default()
            } else {
                Style::default().fg(Color::DarkGray)
            };
            let time = history_time(&e.created_at);
            ListItem::new(Line::from(vec![
                Span::styled(format!("{marker}#{:<4} {time:>8}  ", e.seq), style),
                Span::styled(format!("{:<12} ", e.op_kind), Style::default().fg(OK)),
                Span::styled(e.summary.clone(), style),
            ]))
        })
        .collect();
    let mut state = ListState::default().with_offset(hv.offset.get());
    state.select(Some(hv.selected.min(hv.entries.len().saturating_sub(1))));
    let list = List::new(items).highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    f.render_stateful_widget(list, list_area, &mut state);
    // What `App::on_history_view_mouse` hit-tests clicks against.
    hv.list_rect.set(list_area);
    hv.offset.set(state.offset());

    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "j/k select · enter/double-click jump here · dim = undone · q/esc close",
            Style::default().fg(Color::DarkGray),
        ))),
        hint_area,
    );
}

/// The `L` run-history view: the block's kept runs on the left (newest
/// first, stale ones dimmed), the selected run's stored output on the right.
/// The terminal counterpart of the web UI's `RunHistoryDialog`.
fn render_run_history_view(f: &mut Frame, area: Rect, rv: &super::app::RunHistoryViewState) {
    let rect = centered_rect(area.width * 9 / 10, area.height * 8 / 10, area);
    f.render_widget(Clear, rect);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(ACCENT))
        .title(format!(" run history — {}/{} ", rv.node_id, rv.block));
    let inner = block.inner(rect);
    f.render_widget(block, rect);

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(inner);
    let (main, hint_area) = (rows[0], rows[1]);
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length((main.width / 2).min(38)),
            Constraint::Min(1),
        ])
        .split(main);
    let (list_area, detail_area) = (cols[0], cols[1]);

    let run_label = |e: &crate::worker_client::RunHistoryEntryDto| {
        if e.outcome == "killed" {
            "killed".to_string()
        } else {
            format!(
                "exit {}",
                e.exit_code.map_or("?".to_string(), |c| c.to_string())
            )
        }
    };
    let items: Vec<ListItem> = rv
        .entries
        .iter()
        .map(|e| {
            let dim = if e.stale {
                Style::default().fg(Color::DarkGray)
            } else {
                Style::default()
            };
            let exit_style = if e.stale {
                dim
            } else if e.outcome == "killed" || e.exit_code != Some(0) {
                Style::default().fg(Color::Red)
            } else {
                Style::default().fg(OK)
            };
            let mut spans = vec![
                Span::styled(format!(" {:<9}", history_time(&e.started_at)), dim),
                Span::styled(format!("{:<8}", run_label(e)), exit_style),
            ];
            if let Some(ms) = e.duration_ms {
                spans.push(Span::styled(
                    format!("{:>7}", meshfox_core::format_duration_ms(ms)),
                    dim,
                ));
            }
            if e.stale {
                spans.push(Span::styled(
                    " stale",
                    Style::default()
                        .fg(Color::DarkGray)
                        .add_modifier(Modifier::ITALIC),
                ));
            }
            ListItem::new(Line::from(spans))
        })
        .collect();
    let mut state = ListState::default().with_offset(rv.list_offset.get());
    state.select(Some(rv.selected.min(rv.entries.len().saturating_sub(1))));
    let list = List::new(items)
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
        .block(Block::default().borders(Borders::RIGHT));
    f.render_stateful_widget(list, list_area, &mut state);
    // The list's own rows sit inside its right border, one column narrower.
    rv.list_rect.set(Rect {
        width: list_area.width.saturating_sub(1),
        ..list_area
    });
    rv.list_offset.set(state.offset());

    let detail = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(2), Constraint::Min(1)])
        .split(detail_area);
    if let Some(run) = rv.entries.get(rv.selected) {
        let mut head = format!(" {} · {}", history_time(&run.started_at), run_label(run));
        if let Some(ms) = run.duration_ms {
            head.push_str(&format!(" · {}", meshfox_core::format_duration_ms(ms)));
        }
        let mut lines = vec![Line::from(Span::raw(head))];
        if run.stale {
            lines.push(Line::from(Span::styled(
                " stale — the block, a dependency or a variable changed since (or the session was reset)",
                Style::default().fg(Color::DarkGray).add_modifier(Modifier::ITALIC),
            )));
        }
        f.render_widget(Paragraph::new(lines), detail[0]);
    }
    let output_lines: Vec<Line> = match &rv.output {
        None => vec![Line::from(Span::styled(
            " loading…",
            Style::default().fg(Color::DarkGray),
        ))],
        Some((_, lines)) if lines.is_empty() => {
            vec![Line::from(Span::styled(
                if rv.tty {
                    " a tty session's output isn't stored"
                } else {
                    " no output stored for this run"
                },
                Style::default().fg(Color::DarkGray),
            ))]
        }
        Some((_, lines)) => {
            let mut ansi = super::ansi::Ansi::default();
            lines
                .iter()
                .map(|(stream, text)| {
                    let style = match stream {
                        meshfox_server::stream_exec::OutputStream::Stdout => Style::default(),
                        meshfox_server::stream_exec::OutputStream::Stderr => {
                            Style::default().fg(Color::Yellow)
                        }
                    };
                    let line = ansi.line(text, style);
                    let mut spans = vec![Span::raw(" ")];
                    spans.extend(line.spans);
                    Line::from(spans)
                })
                .collect()
        }
    };
    f.render_widget(
        Paragraph::new(output_lines).scroll((rv.output_scroll.min(u16::MAX as usize) as u16, 0)),
        detail[1],
    );

    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "j/k select run · PgUp/PgDn (or K/J) scroll output · stderr in yellow · q/esc close",
            Style::default().fg(Color::DarkGray),
        ))),
        hint_area,
    );
}

fn render_help(f: &mut Frame, area: Rect, app: &App) {
    let mut items = vec![
        "tab             cycle focus: tree -> document -> output -> tree",
        "                (shift-tab: reverse)",
        "arrows / hjkl   move in spatial view",
        "esc / backspace fold spatial group",
        "j / k           select tree / scroll panes",
        "/               search all node titles/bodies (case-insensitive)",
        "                Enter next, Shift+Enter/Shift+Tab previous, Esc close",
        "h / l           collapse / expand tree",
        "enter           expand/collapse node or follow → link",
        "a               toggle all/selected arrows among positioned nodes",
        "r               run this node's block, with its deps chain",
        "R               run this node's block only (skip deps)",
        "  (a node with more than one block opens a picker first)",
        "L               run history of selected block, otherwise node/picker",
        "[ / ]           select previous/next block or button in Document",
        "enter / c       fold selected block / its code (output stays visible)",
        "enter           run the selected button and its dependencies",
        "esc             clear Document block selection; r/R/L target node again",
        "K               kill the running block",
        "f               expand the focused pane to fill the screen, or",
        "                shrink it back (esc also shrinks it back)",
        "z               collapse/expand the focused Tree/Output pane — same",
        "                as clicking its title bar or collapsed handle",
        "S               reset session — forget which blocks already ran,",
        "                so the next chain run re-runs every dependency",
        "e               edit this node's own file, full-screen (Ctrl-s save,",
        "                Ctrl-f switch file, Ctrl-n heading->node, Ctrl-Space",
        "                suggest attributes/tags, Ctrl-g Unicode, esc close)",
        "ctrl-p          command palette (also inside the editor): node",
        "                edits, run statistics, and every action above",
        "                mouse click/drag/scroll inside the editor",
        "                (unavailable on a read-only canvas, like H below)",
        "i               (Document focus) open/focus this node's own form —",
        "                tab/down next field, shift-tab/up previous, left/right",
        "                toggle a bool or cycle a select, enter submits the whole",
        "                form, esc stops editing (leaves it open, still visible)",
    ];
    if app.selected_is_open_target() {
        items.push("o               open this file node's target in the OS's default application");
    }
    if app.selected_is_table() && app.worker_port.is_some() {
        items.push("enter           (a display=\"table\" node) open the table full screen —");
        items.push("                j/k h/l move, PgUp/PgDn, g/G top/bottom, s sort by the");
        items.push("                column (S adds a key), / search, f filter the column");
        items.push("                (text, >10, =x, ^starts, $ends, ~has, null), F clear it,");
        items.push("                x reset, enter shows the whole cell, y copies it, q closes;");
        items.push("                click a header to sort, wheel scrolls (needs the duckdb CLI)");
    }
    if app.has_configurable_vars() {
        items.push(
            "c               configure every declared variable (see SPEC.md's \"Variables\")",
        );
    }
    if app.service_stats.is_some() {
        items.push("v               open the services list (stop/restart any of them) —");
        items.push("                experimental, see SPEC.md's \"Service blocks (experimental)\"");
    }
    if app.worker_port.is_some() {
        items.push("t               live terminals — every tty session the shared worker");
        items.push("                knows about, started here, another TUI, or a browser tab;");
        items.push("                enter attaches (joins as a viewer), K kills it");
        items.push("H               history — every undo step, newest first; enter jumps");
        items.push("                the canvas back (or forward) to the selected step");
    }
    items.extend([
        "PageUp/Down     scroll the focused pane (document or output)",
        "Ctrl-u / Ctrl-d scroll the focused pane (document or output)",
        "?               toggle this help (j/k/PageUp/Down scroll it while open)",
        "q               quit (esc never quits)",
        "",
        "mouse: click a tree row to select it, or its ▾/▸ marker to",
        "expand/collapse; double-click a node to run its default block;",
        "double-click a → link to jump to its target;",
        "click a button fence's own ▶ marker, or a block name in a",
        "deps line, to run/jump to it; click a var-form/block-picker",
        "row to select it; scroll wheel over any of the three panes",
        "scrolls/moves it (also sideways, over Output); click a pane's",
        "own [+]/[-] (top-right of its title), or double-click its",
        "title, to expand/shrink it (same as f); drag the border between",
        "the tree/document panes, or between them and Output, to resize",
        "",
        "running a `tty` block hands the real terminal over to it,",
        "same as `meshfox run` — this UI reappears once it exits",
    ]);

    // A handful of these lines run past 60 columns (the old fixed-62-wide
    // box's own usable width, after its 2-column border) — with no
    // `.wrap()`, `Paragraph` just clips a line at the pane's right edge
    // instead of wrapping it, which is what actually cut text off (not a
    // vertical scrolling problem, though a short terminal still needs one
    // too — see below). 78 comfortably fits every line here as of this
    // writing; still `.wrap()`ped regardless, so a future longer line (or a
    // narrower real terminal, via `centered_rect`'s own `.min(area.width)`)
    // degrades to wrapping instead of silently truncating again.
    let outer_width = 78u16.min(area.width);
    let inner_width = outer_width.saturating_sub(2);
    let lines = items.into_iter().map(Line::from).collect::<Vec<_>>();
    let measured = Paragraph::new(lines.clone()).wrap(Wrap { trim: false });
    let total_rows = measured.line_count(inner_width) as u16;

    // Sized to fit every (wrapped) row when the terminal is tall enough,
    // same as before — only a short terminal now caps the box height and
    // relies on `help_scroll` (below) to reach the rest, instead of
    // silently dropping whatever didn't fit.
    let outer_height = (total_rows + 2).min(area.height);
    let rect = centered_rect(outer_width, outer_height, area);
    f.render_widget(Clear, rect);
    let inner_height = rect.height.saturating_sub(2);
    let max_scroll = total_rows.saturating_sub(inner_height);
    let scroll = app.help_scroll.min(max_scroll);
    let title = if max_scroll > 0 {
        format!(" keybindings (↑/↓ scroll — {scroll}/{max_scroll}) ")
    } else {
        " keybindings ".to_string()
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(ACCENT))
        .title(title);
    let inner = block.inner(rect);
    f.render_widget(block, rect);

    f.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((scroll, 0)),
        inner,
    );
}

/// The editor's syntect theme and the theme set it came from. Unpacking the
/// bundled `ThemeSet` (zlib + bincode) and recolouring the theme for
/// meshfox's scopes is far too slow to redo on every frame, so it is built
/// once per theme name.
fn editor_syntect_theme(
    theme_name: &str,
) -> (
    Arc<syntect::highlighting::ThemeSet>,
    syntect::highlighting::Theme,
) {
    type Cached = (
        String,
        Arc<syntect::highlighting::ThemeSet>,
        syntect::highlighting::Theme,
    );
    static CACHE: std::sync::Mutex<Option<Cached>> = std::sync::Mutex::new(None);
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((name, theme_set, theme)) = cache.as_ref() {
        if name == theme_name {
            return (Arc::clone(theme_set), theme.clone());
        }
    }
    let theme_set = syntect::highlighting::ThemeSet::load_defaults();
    let theme = theme_set.themes.get(theme_name).cloned();
    let theme = theme.or_else(|| theme_set.themes.values().next().cloned());
    let theme = theme.expect("syntect ships at least one theme");
    // Bundled `syntect` themes have no rules at all for meshfox's own scope
    // names — without this, a `<!-- meshfox:... -->` marker's keyword/
    // attribute-name/attribute-value would all render in one plain color,
    // same as any other comment text.
    let theme = crate::syntax_registry::with_meshfox_scope_colors(theme);
    let theme_set = Arc::new(theme_set);
    *cache = Some((
        theme_name.to_string(),
        Arc::clone(&theme_set),
        theme.clone(),
    ));
    (theme_set, theme)
}

/// The fullscreen source editor (`e`) — header (which file, dirty state),
/// the `edtui` buffer itself, and a footer (error, or the keybinding
/// hint) — plus the file-switcher (`Ctrl-f`) as a `render_block_picker`-
/// style overlay on top when open.
fn render_source_editor(
    f: &mut Frame,
    area: Rect,
    se: &mut SourceEditorState,
    syntax_set: &Arc<SyntaxSet>,
    theme_name: &str,
) {
    // A save that found the file changed and couldn't combine it with the
    // buffer: the comparison owns the whole screen until it is decided.
    if se.diff.is_some() {
        render_source_diff(f, area, se);
        return;
    }
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(area);

    let kind = if se.is_canvas {
        "canvas"
    } else {
        "plain markdown"
    };
    let dirty = if se.dirty() { " [modified]" } else { "" };
    let header = Line::from(vec![
        Span::styled(
            format!(" {} ", se.path.display()),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("({kind}){dirty}"),
            Style::default().fg(Color::DarkGray),
        ),
    ]);
    f.render_widget(Paragraph::new(header), chunks[0]);

    // `with_sets` (not `new`, which would load its own separate, defaults-
    // only SyntaxSet) so this editor sees the exact same custom grammars as
    // the read-only preview pane — see `crate::syntax_registry`.
    let syntax_highlighter = syntax_set
        .find_syntax_by_name(crate::syntax_registry::MESHFOX_MARKDOWN_SYNTAX_NAME)
        .or_else(|| syntax_set.find_syntax_by_token(SOURCE_EDITOR_LANG))
        .cloned()
        .map(|syntax_ref| {
            let (theme_set, theme) = editor_syntect_theme(theme_name);
            SyntaxHighlighter::with_sets(theme, theme_set, syntax_ref, Arc::clone(syntax_set))
        });
    let view = EditorView::new(&mut se.editor)
        .line_numbers(LineNumbers::Absolute)
        .wrap(true);
    let view = match syntax_highlighter {
        Some(h) => view.syntax_highlighter(Some(h)),
        None => view,
    };
    f.render_widget(view, chunks[1]);

    let footer = match &se.error {
        _ if se.read_only => Line::from("Read-only canvas include · Ctrl-f switch file · Esc close · open separately to edit"),
        Some(msg) => Line::from(Span::styled(msg.as_str(), Style::default().fg(FAIL))),
        None => Line::from(Span::styled(
            "Ctrl-s save · Ctrl-f switch file · Ctrl-n heading→node · Ctrl-p suggest params · Ctrl-g Unicode · \
             esc close (vim keys inside the buffer)",
            Style::default().fg(Color::DarkGray),
        )),
    };
    f.render_widget(Paragraph::new(footer), chunks[2]);

    if se.file_picker_open {
        render_source_file_picker(f, area, se);
    }
    if se.attr_suggest_open {
        render_attr_suggest_popup(f, area, se);
    }
    if se.tag_suggest_open {
        render_tag_suggest_popup(f, area, se);
    }
    if let Some(picker) = &mut se.unicode_picker {
        picker.render(f, area);
    }
}

/// Columns `skip..skip+width` of `text` (display columns, so a wide
/// character counts as two), tabs shown as four spaces — one line of the
/// side-by-side comparison's column, never wider than the column.
fn visible_columns(text: &str, skip: usize, width: usize) -> String {
    let mut out = String::new();
    let mut col = 0usize;
    for c in text.replace('\t', "    ").chars() {
        let w = Span::raw(c.to_string()).width();
        if col + w <= skip {
            col += w;
            continue;
        }
        if col < skip {
            // A wide character cut by the left edge: a blank in its place.
            out.push(' ');
            col += w;
            continue;
        }
        if col - skip + w > width {
            break;
        }
        out.push(c);
        col += w;
    }
    out
}

/// The source editor's comparison after a refused save (`source_diff.rs`):
/// the file as it is on disk now on the left, the buffer on the right, the
/// lines both sides changed marked with `!`.
fn render_source_diff(f: &mut Frame, area: Rect, se: &mut SourceEditorState) {
    let path = se.path.display().to_string();
    let Some(diff) = se.diff.as_mut() else {
        return;
    };
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(area);
    diff.view_height = chunks[2].height as usize;

    let summary = if diff.overlapped {
        format!(
            "your edits overlap changes made elsewhere ({} region{}) — nothing was saved",
            diff.conflict_starts.len(),
            if diff.conflict_starts.len() == 1 {
                ""
            } else {
                "s"
            },
        )
    } else {
        "the file changed elsewhere and couldn't be merged automatically — nothing was saved"
            .to_string()
    };
    f.render_widget(
        Paragraph::new(Line::from(vec![
            // The verdict first — a long path must not push it off-screen.
            Span::styled(format!(" {summary} "), Style::default().fg(FAIL)),
            Span::styled(format!("· {path}"), Style::default().fg(Color::DarkGray)),
        ])),
        chunks[0],
    );

    // Two equal columns with a one-cell marker gutter between them.
    let total = chunks[2].width as usize;
    let left_w = total.saturating_sub(1) / 2;
    let right_w = total.saturating_sub(1) - left_w;
    let digits = |n: usize| n.max(1).to_string().len();
    let left_lines = diff.rows.iter().filter(|r| r.left.is_some()).count();
    let right_lines = diff.rows.iter().filter(|r| r.right.is_some()).count();
    let (left_nw, right_nw) = (digits(left_lines), digits(right_lines));

    let head = |label: &str, width: usize| {
        let mut text = format!(" {label}");
        text.truncate(width);
        format!("{text:<width$}")
    };
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                head("theirs — the file on disk now", left_w),
                Style::default().fg(FAIL).add_modifier(Modifier::BOLD),
            ),
            Span::raw(" "),
            Span::styled(
                head("yours — the editor buffer", right_w),
                Style::default().fg(OK).add_modifier(Modifier::BOLD),
            ),
        ])),
        chunks[1],
    );

    let cell = |side: &Option<(usize, String)>, nw: usize, width: usize, hscroll: usize| match side
    {
        Some((idx, text)) => {
            let prefix = format!("{:>nw$} ", idx + 1);
            let room = width.saturating_sub(prefix.len());
            let body = visible_columns(text, hscroll, room);
            (prefix, body)
        }
        None => (String::new(), String::new()),
    };
    let mut lines: Vec<Line> = Vec::new();
    for i in diff.scroll..(diff.scroll + diff.view_height).min(diff.rows.len()) {
        let row = &diff.rows[i];
        let conflict = diff.conflict_rows[i];
        let (mark, mark_style) = if conflict {
            (
                '!',
                Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
            )
        } else {
            match row.kind {
                meshfox_core::DiffKind::Same => (' ', Style::default()),
                meshfox_core::DiffKind::Changed => ('~', Style::default().fg(Color::Yellow)),
                meshfox_core::DiffKind::Removed => ('-', Style::default().fg(FAIL)),
                meshfox_core::DiffKind::Added => ('+', Style::default().fg(OK)),
            }
        };
        let bold = if conflict {
            Modifier::BOLD
        } else {
            Modifier::empty()
        };
        let changed = row.kind != meshfox_core::DiffKind::Same;
        let side_style = |color: Color| {
            if changed {
                Style::default().fg(color).add_modifier(bold)
            } else {
                Style::default()
            }
        };
        let (lp, lt) = cell(&row.left, left_nw, left_w, diff.hscroll);
        let (rp, rt) = cell(&row.right, right_nw, right_w, diff.hscroll);
        let pad = |used: usize, width: usize| " ".repeat(width.saturating_sub(used));
        let l_used = lp.len() + Span::raw(lt.clone()).width();
        lines.push(Line::from(vec![
            Span::styled(lp, Style::default().fg(Color::DarkGray)),
            Span::styled(lt, side_style(FAIL)),
            Span::raw(pad(l_used, left_w)),
            Span::styled(mark.to_string(), mark_style),
            Span::styled(rp, Style::default().fg(Color::DarkGray)),
            Span::styled(rt, side_style(OK)),
        ]));
    }
    f.render_widget(Paragraph::new(lines), chunks[2]);

    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "t take theirs · m keep mine (overwrite) · n/N conflicts · j/k scroll · h/l pan · \
             esc back",
            Style::default().fg(Color::DarkGray),
        ))),
        chunks[3],
    );
}

/// TODO.canvas.md: "Саджесты и подсветка синтаксиса в TUI", item 2 —
/// `Ctrl-p`'s popup, same floating-`List`-over-`Clear` shape as
/// `render_source_file_picker` right above. Each row shows the attribute
/// name with a trailing `=` for a value-taking one (nothing for a bare
/// fence flag — see `AttrCandidate::is_flag`), so the list itself hints at
/// what selecting it will actually type.
fn render_attr_suggest_popup(f: &mut Frame, area: Rect, se: &SourceEditorState) {
    let height = (se.attr_suggest_candidates.len() as u16 + 2).min(area.height);
    let rect = centered_rect(36, height, area);
    f.render_widget(Clear, rect);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(ACCENT))
        .title(format!(" {} attribute ", se.attr_suggest_label()));
    let inner = block.inner(rect);
    f.render_widget(block, rect);

    let items: Vec<ListItem> = se
        .attr_suggest_candidates
        .iter()
        .map(|c| {
            let suffix = if c.is_flag { "" } else { "=" };
            ListItem::new(Line::from(format!("{}{suffix}", c.name)))
        })
        .collect();

    let mut state = ListState::default();
    state.select(Some(se.attr_suggest_selected));
    let list = List::new(items).highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    f.render_stateful_widget(list, inner, &mut state);
}

/// TODO.canvas.md: "Саджест по тегам в TUI" — `Ctrl-p`'s other popup
/// shape, shown instead of `render_attr_suggest_popup` when the cursor was
/// inside a `tags="..."` value (see `SourceEditorState::open_attr_suggest`).
/// Same floating-`List`-over-`Clear` layout, candidates are already plain
/// tag strings so there's no per-item suffix logic to branch on.
fn render_tag_suggest_popup(f: &mut Frame, area: Rect, se: &SourceEditorState) {
    let height = (se.tag_suggest_candidates.len() as u16 + 2).min(area.height);
    let rect = centered_rect(36, height, area);
    f.render_widget(Clear, rect);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(ACCENT))
        .title(format!(" {} ", se.value_suggest_label().unwrap_or("tag")));
    let inner = block.inner(rect);
    f.render_widget(block, rect);

    let items: Vec<ListItem> = se
        .tag_suggest_candidates
        .iter()
        .map(|t| {
            let text = if se.value_suggest_label().is_some() {
                t.clone()
            } else {
                format!("#{t}")
            };
            ListItem::new(Line::from(text))
        })
        .collect();

    let mut state = ListState::default();
    state.select(Some(se.tag_suggest_selected));
    let list = List::new(items).highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    f.render_stateful_widget(list, inner, &mut state);
}

fn render_source_file_picker(f: &mut Frame, area: Rect, se: &SourceEditorState) {
    let count = se.files.len() + 1; // +1 for "this document"
    let height = (count as u16 + 2).min(area.height);
    let rect = centered_rect(64, height, area);
    f.render_widget(Clear, rect);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(ACCENT))
        .title(" switch file ");
    let inner = block.inner(rect);
    f.render_widget(block, rect);

    let mut items = vec![ListItem::new(Line::from("this document"))];
    items.extend(
        se.files
            .iter()
            .map(|inc| ListItem::new(Line::from(format!("↳ {} ({})", inc.title, inc.target)))),
    );

    let mut state = ListState::default();
    state.select(Some(se.file_picker_selected));
    let list = List::new(items).highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    f.render_stateful_widget(list, inner, &mut state);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn extra_edge_row_shows_bracketed_label_before_italic_target() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let dir =
            std::env::temp_dir().join(format!("meshfox-tui-link-render-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("canvas.canvas.md");
        std::fs::write(
            &path,
            concat!(
                "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n",
                "## Source\n<!-- meshfox:node id=\"source\" -->\n",
                "## Target\n<!-- meshfox:node id=\"target\" -->\n",
                "<!-- meshfox:edge from=\"source\" label=\"uses worker\" -->\n",
                "<!-- meshfox:edge from=\"source\" -->\n",
            ),
        )
        .unwrap();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(path, tx, None, None).await.unwrap();
        app.expanded.insert("source".to_string());
        app.rows = crate::tui::tree::flatten(&app.display_canvas, &app.expanded);
        let mut terminal = Terminal::new(TestBackend::new(70, 12)).unwrap();
        terminal
            .draw(|f| render_tree(f, Rect::new(0, 0, 70, 12), &mut app))
            .unwrap();
        let buf = terminal.backend().buffer();
        let row: String = (0..70)
            .map(|x| buf.cell((x, 3)).unwrap().symbol())
            .collect();
        assert!(row.contains("→ [uses worker] Target"), "{row}");
        let label_x = row.find('[').unwrap() as u16;
        let target_x = row.find("Target").unwrap() as u16;
        assert_eq!(buf.cell((label_x, 3)).unwrap().fg, ACCENT);
        assert!(buf
            .cell((target_x, 3))
            .unwrap()
            .modifier
            .contains(Modifier::ITALIC));
        let plain_row: String = (0..70)
            .map(|x| buf.cell((x, 4)).unwrap().symbol())
            .collect();
        assert!(plain_row.contains("→ Target"), "{plain_row}");
        assert!(!plain_row.contains("[]"), "{plain_row}");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn arrows_have_source_stem_corners_and_target_stem_between_adjacent_cards() {
        let mut grid = vec![vec![' '; 20]; 14];
        draw_map_arrow(
            &mut grid,
            Rect::new(0, 8, 5, 4),
            Rect::new(10, 0, 5, 4),
            Rect::new(0, 0, 20, 14),
            &[],
        );
        assert_eq!(grid[7][2], '│');
        assert_eq!(grid[4][12], '↑');
        assert!(grid.iter().any(|row| row.contains(&'─')));

        let mut grid = vec![vec![' '; 20]; 14];
        draw_map_arrow(
            &mut grid,
            Rect::new(0, 0, 5, 4),
            Rect::new(10, 8, 5, 4),
            Rect::new(0, 0, 20, 14),
            &[],
        );
        assert_eq!(grid[4][2], '│');
        assert_eq!(grid[7][12], '↓');
    }

    #[test]
    fn aligned_nodes_keep_a_straight_connection() {
        let mut grid = vec![vec![' '; 30]; 16];
        let from = Rect::new(5, 0, 12, 4);
        let to = Rect::new(5, 8, 12, 4);
        draw_map_arrow(&mut grid, from, to, Rect::new(0, 0, 30, 16), &[from, to]);
        assert_eq!(grid[4][11], '│');
        assert_eq!(grid[5][11], '│');
        assert_eq!(grid[6][11], '│');
        assert_eq!(grid[7][11], '↓');
    }

    #[test]
    fn long_connection_goes_around_intermediate_card() {
        let mut grid = vec![vec![' '; 60]; 24];
        let from = Rect::new(40, 0, 12, 4);
        let middle = Rect::new(20, 8, 12, 4);
        let to = Rect::new(4, 16, 12, 4);
        let obstacles = [from, middle, to];
        draw_map_arrow(&mut grid, from, to, Rect::new(0, 0, 60, 24), &obstacles);
        assert_eq!(grid[15][10], '↓');
        assert!(grid.iter().flatten().any(|&c| c == '─'));
        for y in middle.y..middle.y + middle.height {
            for x in middle.x..middle.x + middle.width {
                assert_eq!(grid[y as usize][x as usize], ' ');
            }
        }
    }

    #[test]
    fn readme_component_edges_do_not_cross_cards() {
        let canvas =
            meshfox_core::Canvas::from_markdown(include_str!("../../../../README.md")).unwrap();
        let area = Rect::new(0, 0, 128, 50);
        let nodes = spatial::layout(&canvas, "component-diagram", area, "mcp-root");
        let rect = |id: &str| nodes.iter().find(|node| node.id == id).unwrap().rect;
        let obstacles: Vec<_> = nodes.iter().map(|node| node.rect).collect();
        for (source, target) in [
            ("mcp-root", "coordinator-resolve"),
            ("mcp-root", "worker-axum-http-server"),
            ("cli-one-shot-ops", "coordinator-resolve"),
        ] {
            let mut grid = vec![vec![' '; area.width as usize]; area.height as usize];
            draw_map_arrow(&mut grid, rect(source), rect(target), area, &obstacles);
            assert!(grid
                .iter()
                .flatten()
                .any(|&ch| matches!(ch, '↓' | '↑' | '←' | '→')));
            for card in &obstacles {
                for y in card.y..card.y + card.height {
                    for x in card.x..card.x + card.width {
                        assert_eq!(
                            grid[y as usize][x as usize], ' ',
                            "{source} → {target} crossed a card"
                        );
                    }
                }
            }
        }
    }

    #[tokio::test]
    async fn all_edges_highlights_only_links_of_selected_node() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let dir =
            std::env::temp_dir().join(format!("meshfox-spatial-highlight-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("canvas.canvas.md");
        std::fs::write(&path, concat!(
            "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n",
            "## Diagram\n<!-- meshfox:node id=\"group\" -->\n",
            "### A\n<!-- meshfox:node id=\"a\" x=0 y=0 w=100 h=80 -->\n",
            "### B\n<!-- meshfox:node id=\"b\" x=200 y=0 w=100 h=80 -->\n<!-- meshfox:edge from=\"a\" -->\n",
            "### C\n<!-- meshfox:node id=\"c\" x=400 y=0 w=100 h=80 -->\n",
            "### D\n<!-- meshfox:node id=\"d\" x=600 y=0 w=100 h=80 -->\n<!-- meshfox:edge from=\"c\" -->\n",
        )).unwrap();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(path, tx, None, None).await.unwrap();
        app.expanded.insert("group".to_string());
        app.rows = crate::tui::tree::flatten(&app.display_canvas, &app.expanded);
        app.selected = app.rows.iter().position(|row| row.node_id == "a").unwrap();
        app.show_all_map_edges = true;
        let mut terminal = Terminal::new(TestBackend::new(100, 15)).unwrap();
        terminal
            .draw(|f| render_tree(f, Rect::new(0, 0, 100, 15), &mut app))
            .unwrap();
        let buf = terminal.backend().buffer();
        let arrow_colors: Vec<_> = (0..100)
            .filter_map(|x| {
                let cell = buf.cell((x, 3))?;
                (cell.symbol() == "→").then_some(cell.fg)
            })
            .collect();
        assert!(
            arrow_colors.contains(&ACCENT),
            "selected edge should be highlighted"
        );
        assert!(
            arrow_colors.contains(&Color::DarkGray),
            "other edge should be subdued"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn positioned_regular_parent_renders_nodes_arrow_and_label() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let dir =
            std::env::temp_dir().join(format!("meshfox-spatial-render-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("canvas.canvas.md");
        std::fs::write(&path, concat!(
            "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n",
            "## Section\n<!-- meshfox:node id=\"group\" -->\n",
            "### Long source title\n<!-- meshfox:node id=\"a\" x=0 y=0 w=240 h=112 -->\n",
            "### Destination\n<!-- meshfox:node id=\"b\" x=280 y=0 w=240 h=112 -->\n",
            "<!-- meshfox:edge from=\"a\" label=\"Test\" sourceSide=\"top\" targetSide=\"top\" -->\n",
        )).unwrap();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(path, tx, None, None).await.unwrap();
        app.expanded.insert("group".to_string());
        app.rows = crate::tui::tree::flatten(&app.display_canvas, &app.expanded);
        app.selected = app.rows.iter().position(|r| r.node_id == "a").unwrap();

        let mut terminal = Terminal::new(TestBackend::new(62, 15)).unwrap();
        terminal
            .draw(|f| render_tree(f, Rect::new(0, 0, 62, 15), &mut app))
            .unwrap();
        let buf = terminal.backend().buffer();
        let screen: String = (0..15)
            .map(|y| {
                let row: String = (0..62)
                    .filter_map(|x| buf.cell((x, y)))
                    .map(|c| c.symbol())
                    .collect();
                format!("{row}\n")
            })
            .collect();
        assert!(screen.contains("Long source"), "{screen}");
        assert!(screen.contains("Destination"), "{screen}");
        assert!(
            screen.lines().take(4).any(|line| line.contains('→')),
            "{screen}"
        );
        assert!(screen.contains("Test"), "{screen}");
        assert!(screen.contains("top→top"), "{screen}");
        let map_area = Rect::new(1, 1, 60, 11);
        let source = spatial::layout(&app.display_canvas, "group", map_area, "a")
            .into_iter()
            .find(|n| n.id == "a")
            .unwrap();
        let destination = spatial::layout(&app.display_canvas, "group", map_area, "a")
            .into_iter()
            .find(|n| n.id == "b")
            .unwrap();
        assert_eq!(
            buf.cell((source.rect.x + 1, source.rect.y + 1)).unwrap().bg,
            MAP_SELECTED_BG
        );
        assert_eq!(
            buf.cell((destination.rect.x + 1, destination.rect.y + 1))
                .unwrap()
                .bg,
            MAP_NODE_BG
        );
        assert_eq!(
            spatial::hit_test(&app, map_area, destination.rect.x, destination.rect.y),
            Some("b".to_string())
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    // A real bug, found live: the document pane used to size a
    // `Segment::Text`'s rect (and advance `y`) by its raw `lines.len()`
    // rather than how many rows it actually needs once word-wrapped to
    // the pane's width — a single long bullet point (common in prose,
    // rare in this codebase's own short example bodies, which is why this
    // went unnoticed) wraps into several screen rows but was only ever
    // given one, so `Paragraph`'s own wrapping silently clipped the rest,
    // and every segment after it drew overlapping the tail of this one
    // instead of below it (reported against README.md's own new "Source
    // editor keybindings" node, whose long bullets tripped this exactly).
    #[test]
    fn wrapped_text_layout_renders_a_whole_segment_when_it_fits() {
        let layout = wrapped_text_layout(3, 0, 10).expect("segment is below the scroll offset");
        assert_eq!(layout.row_offset, 0);
        assert_eq!(layout.height, 3);
    }

    #[test]
    fn wrapped_text_layout_skips_a_whole_segment_entirely_scrolled_past() {
        assert!(wrapped_text_layout(3, 5, 10).is_none());
    }

    #[test]
    fn wrapped_text_layout_can_land_partway_through_a_long_wrapped_line() {
        // `skip` (2) is less than `total` (5) — this used to only be
        // reachable at all between whole *logical* lines; now that a
        // segment's "rows" are wrapped rows, a scroll offset can land
        // inside what's still just one long `Line` underneath.
        let layout = wrapped_text_layout(5, 2, 10).unwrap();
        assert_eq!(layout.row_offset, 2);
        assert_eq!(layout.height, 3);
    }

    #[test]
    fn wrapped_text_layout_clamps_height_to_the_room_left_in_the_pane() {
        let layout = wrapped_text_layout(5, 0, 2).unwrap();
        assert_eq!(layout.height, 2);
    }

    #[test]
    fn a_long_wrapped_line_is_not_clipped_when_measured_with_line_count() {
        use ratatui::buffer::Buffer;
        use ratatui::widgets::Widget;

        let long_line = "word ".repeat(40); // wraps to several rows at width 20
        let paragraph = Paragraph::new(Text::from(vec![Line::from(long_line.trim().to_string())]))
            .wrap(Wrap { trim: false });
        let width = 20u16;
        let total = paragraph.line_count(width) as u16;
        assert!(total > 1, "this line should need more than one wrapped row");

        let area = Rect::new(0, 0, width, total);
        let mut buf = Buffer::empty(area);
        paragraph.render(area, &mut buf);

        // The very last wrapped row (what a too-short rect, sized by
        // `lines.len()` instead of `line_count`, used to cut off) must
        // still have real content in it, not be left blank.
        let last_row: String = (0..width)
            .map(|x| {
                buf.cell((x, total - 1))
                    .and_then(|c| c.symbol().chars().next())
                    .unwrap_or(' ')
            })
            .collect();
        assert!(
            !last_row.trim().is_empty(),
            "the last wrapped row should still have visible text, got {last_row:?}"
        );
    }

    // Same bug, end to end through `render_document` itself this time
    // (the tests above cover the two pieces it's built from in isolation)
    // — a real `App` over a real canvas file whose root body has one long
    // bullet plus a short line right after it, rendered through a real
    // `Terminal`/`TestBackend`. Both the long line's own tail and the
    // short line after it must actually appear somewhere on screen — the
    // pre-fix version clipped the former and drew the latter overlapping
    // the former's own last (visible) row instead of below it.
    #[tokio::test]
    async fn render_document_does_not_clip_a_long_wrapped_line_or_the_segment_after_it() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let dir = std::env::temp_dir().join(format!(
            "meshfox-render-document-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("canvas.canvas.md");
        // Distinct, numbered tokens rather than one repeated word — a
        // repeated word's own tail ("...docious") would still show up even
        // if every repetition past the first got clipped, since it's a
        // substring of the *first* one too; numbered tokens can't lie
        // about which repetitions actually survived.
        let long_line: String = (1..=8)
            .map(|n| format!("TOKEN{n:02}"))
            .collect::<Vec<_>>()
            .join(" ");
        std::fs::write(
            &path,
            format!(
                "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n\n{long_line}\n\nTAIL-MARKER-LINE\n"
            ),
        )
        .unwrap();

        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(path, tx, None, None)
            .await
            .expect("valid test canvas");

        let backend = TestBackend::new(20, 20);
        let mut terminal = Terminal::new(backend).unwrap();
        let area = Rect::new(0, 0, 20, 20);
        terminal
            .draw(|f| render_document(f, area, &mut app))
            .unwrap();

        let buf = terminal.backend().buffer();
        let mut screen = String::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                if let Some(cell) = buf.cell((x, y)) {
                    screen.push_str(cell.symbol());
                }
            }
            screen.push('\n');
        }
        assert!(
            screen.contains("TOKEN08"),
            "the long line's own last wrapped row should still be on screen:\n{screen}"
        );
        assert!(
            screen.contains("TAIL-MARKER-LINE"),
            "the short segment after the long line should be on screen, not hidden behind it:\n{screen}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    // A finished step whose own block declares `output="markdown"` gets its
    // captured stdout rendered as real Markdown in the Output pane, not
    // left as literal `| a | b |` pipe-table text — see
    // `RunState::output_markdown`'s own doc comment for why this pane
    // needed to start looking at `output=` at all. `render_output` never
    // executes anything itself — `run` is built by hand, exactly the shape
    // `advance_run`/`on_output_line` would have left it in once a
    // `output="markdown"` step finishes.
    #[tokio::test]
    async fn render_output_shows_a_finished_output_markdown_steps_stdout_as_a_real_table() {
        use meshfox_core::deps::BlockAddr;
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let dir = std::env::temp_dir().join(format!(
            "meshfox-render-output-markdown-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("canvas.canvas.md");
        std::fs::write(
            &path,
            "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n\n\
             ```bash name=\"table\" cache output=\"markdown\"\necho hi\n```\n",
        )
        .unwrap();

        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(path.clone(), tx, None, None)
            .await
            .expect("valid test canvas");
        app.run = Some(crate::tui::app::RunState {
            chain: vec![BlockAddr::new("root", "table")],
            idx: 1,
            http_rx: None,
            lines: vec!["==> table".to_string(), "(exit 0 · 5ms)".to_string()],
            full_output: String::new(),
            stdout_only: "| score | name |\n|---|---|\n| 1.0 | ZMARKERZ |\n".to_string(),
            stderr_only: String::new(),
            output_markdown: true,
            step_started: std::time::Instant::now(),
            had_failure: false,
            killed: false,
            finished: true,
        });
        // Bypasses `start_run` (which is what flips this in real use) since
        // this test builds `RunState` by hand — without it the console
        // defaults to collapsed and `render_output` would only draw the
        // 1-line strip this test isn't checking for.
        app.console_collapsed = false;

        let backend = TestBackend::new(40, 20);
        let mut terminal = Terminal::new(backend).unwrap();
        let area = Rect::new(0, 0, 40, 20);
        terminal.draw(|f| render_output(f, area, &app)).unwrap();

        let buf = terminal.backend().buffer();
        let mut screen = String::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                if let Some(cell) = buf.cell((x, y)) {
                    screen.push_str(cell.symbol());
                }
            }
            screen.push('\n');
        }
        assert!(
            screen.contains("ZMARKERZ"),
            "the table's own cell text should be on screen:\n{screen}"
        );
        assert!(
            !screen.contains("|---|---|"),
            "the raw pipe-table syntax should have been rendered, not left as literal text:\n{screen}"
        );
        assert!(
            screen.contains("───"),
            "a real rendered table draws its own header rule (render_table's `─` line):\n{screen}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn document_resize_reflows_cached_tables_and_keeps_following_click_targets() {
        let mut app = test_app().await;
        let md = "| Mode | Command | Notes |\n|---|---|---|\n| Single shot + power-down | `0x36E0` | long description of sensor power mode |\n\n```bash name=\"go\"\necho AFTER\n```\n";
        let (segments, clicks) = super::super::markdown::render(
            md,
            std::path::Path::new("/nonexistent"),
            &app.highlighter,
            "root",
            &[],
            &HashMap::new(),
            None,
            &HashMap::new(),
        );
        app.doc_segments = segments;
        app.doc_click_regions = clicks;
        let mut header_rows = Vec::new();
        for width in [72, 32, 72] {
            let area = Rect::new(0, 0, width, 40);
            let screen = render_to_screen(area, |f| render_document(f, area, &mut app));
            assert!(screen.contains("AFTER"), "{screen}");
            assert!(
                app.doc_segments
                    .iter()
                    .any(|s| matches!(s, Segment::Table { .. })),
                "resize must retain parsed content"
            );
            let target = app
                .doc_click_targets
                .iter()
                .find(|(_, target)| {
                    matches!(
                        target,
                        super::super::markdown::ClickTarget::Block {
                            action: super::super::markdown::BlockAction::ToggleBlock,
                            ..
                        }
                    )
                })
                .unwrap();
            header_rows.push(target.0.y);
        }
        assert!(header_rows[1] > header_rows[0]);
        assert_eq!(header_rows[0], header_rows[2]);
        std::fs::remove_dir_all(app.canvas_path.parent().unwrap()).unwrap();
    }

    async fn test_app() -> App {
        let dir = std::env::temp_dir().join(format!(
            "meshfox-render-help-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("canvas.canvas.md");
        std::fs::write(
            &path,
            "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n",
        )
        .unwrap();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        App::new(path, tx, None, None)
            .await
            .expect("valid test canvas")
    }

    #[tokio::test]
    async fn canvas_search_scrolls_to_each_wrapped_occurrence_and_clears_highlights() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        use ratatui::{backend::TestBackend, Terminal};
        let mut app = test_app().await;
        let node = app
            .display_canvas
            .nodes
            .iter_mut()
            .find(|n| n.id == "root")
            .unwrap();
        node.title = "needle title".into();
        node.text = format!(
            "{} **needle** {} needle",
            "длинное слово ".repeat(70),
            "tail ".repeat(120)
        );
        app.on_key(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE))
            .await;
        for c in "needle".chars() {
            app.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE))
                .await;
        }
        let mut terminal = Terminal::new(TestBackend::new(80, 20)).unwrap();
        for expected in [1, 2, 0] {
            app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
                .await;
            terminal.draw(|f| render(f, &mut app)).unwrap();
            assert_eq!(app.search.index, expected);
            let buffer = terminal.backend().buffer();
            let selected: String = buffer
                .content
                .iter()
                .filter(|cell| cell.bg == Color::Rgb(0, 215, 255))
                .map(|cell| cell.symbol())
                .collect();
            assert_eq!(
                selected, "needle",
                "the current occurrence must actually be painted inside the viewport"
            );
            if expected != 0 {
                assert!(app.doc_scroll > 0);
            } else {
                assert_eq!(app.doc_scroll, 0);
            }
        }
        app.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
            .await;
        terminal.draw(|f| render(f, &mut app)).unwrap();
        assert!(!terminal
            .backend()
            .buffer()
            .content
            .iter()
            .any(|c| c.bg == Color::Rgb(0, 215, 255) || c.bg == Color::Yellow));
        std::fs::remove_dir_all(app.canvas_path.parent().unwrap()).unwrap();
    }

    #[tokio::test]
    async fn canvas_search_counts_zero_and_all_occurrences_in_footer() {
        let mut app = test_app().await;
        app.search.open = true;
        app.search.query = "missing".into();
        let area = Rect::new(0, 0, 90, 2);
        assert!(render_to_screen(area, |f| render_search(f, area, &app)).contains("0/0"));
        app.search.query = "Root".into();
        app.search.index = usize::MAX;
        assert!(render_to_screen(area, |f| render_search(f, area, &app)).contains("1/1"));
        app.search.query.clear();
        assert!(!render_to_screen(area, |f| render_search(f, area, &app)).contains("0/0"));
        app.search.query = "界".repeat(40);
        app.search.cursor = app.search.query.len();
        let narrow = Rect::new(0, 0, 12, 1);
        let screen = render_to_screen(narrow, |f| render_search(f, narrow, &app));
        assert!(screen.contains("0/0"));
        assert!(
            screen.contains('▏'),
            "wide query characters must not push the cursor off-screen"
        );
        std::fs::remove_dir_all(app.canvas_path.parent().unwrap()).unwrap();
    }

    fn render_to_screen(area: Rect, mut draw: impl FnMut(&mut ratatui::Frame)) -> String {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        let backend = TestBackend::new(area.width, area.height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| draw(f)).unwrap();
        let buf = terminal.backend().buffer();
        let mut screen = String::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                if let Some(cell) = buf.cell((x, y)) {
                    screen.push_str(cell.symbol());
                }
            }
            screen.push('\n');
        }
        screen
    }

    #[tokio::test]
    #[allow(deprecated)]
    async fn scrolling_sixel_uses_cropped_raster_dimensions() {
        use super::super::app::BackgroundMsg;
        use ratatui_image::{
            picker::{Picker, ProtocolType},
            FontSize,
        };
        let mut app = test_app().await;
        app.picker = Picker::from_fontsize(FontSize::new(12, 7));
        app.picker.set_protocol_type(ProtocolType::Sixel);
        app.display_canvas = meshfox_core::Canvas::from_markdown(
            "# Root\n<!-- meshfox:node id=\"root\" -->\n\n![plot](plot.png)\n\ntext below\n",
        )
        .unwrap();
        let path = app.canvas_path.parent().unwrap().join("plot.png");
        let mut bytes = std::io::Cursor::new(Vec::new());
        image::DynamicImage::new_rgb8(48, 28)
            .write_to(&mut bytes, image::ImageFormat::Png)
            .unwrap();
        app.on_background_msg(BackgroundMsg::ImageBytes {
            generation: app.image_generation,
            path,
            result: Ok(bytes.into_inner()),
        });
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(12, 8)).unwrap();
        terminal
            .draw(|f| render_document(f, Rect::new(0, 0, 12, 8), &mut app))
            .unwrap();
        app.doc_scroll = 3;
        let frame = terminal
            .draw(|f| render_document(f, Rect::new(0, 0, 12, 8), &mut app))
            .unwrap();
        let sequence = frame
            .buffer
            .content
            .iter()
            .map(|cell| cell.symbol())
            .find(|s| s.contains("q\"1;1;"))
            .unwrap();
        assert!(sequence.contains("q\"1;1;48;21"), "{sequence:?}");
        assert!(!sequence.contains("q\"1;1;48;28"));
        let _ = std::fs::remove_dir_all(app.canvas_path.parent().unwrap());
    }

    #[test]
    fn partial_images_keep_visible_rows_inside_the_viewport_for_all_protocols() {
        use super::super::app::prepared_image;
        use ratatui::buffer::Buffer;
        use ratatui_image::picker::{Picker, ProtocolType};
        let pixels = image::RgbaImage::from_fn(40, 80, |_, y| {
            if y < 40 {
                image::Rgba([255, 0, 0, 255])
            } else {
                image::Rgba([0, 0, 255, 255])
            }
        });
        for kind in [
            ProtocolType::Halfblocks,
            ProtocolType::Iterm2,
            ProtocolType::Sixel,
            ProtocolType::Kitty,
        ] {
            let mut picker = Picker::halfblocks();
            picker.set_protocol_type(kind);
            let protocol = prepared_image(
                &picker,
                image::DynamicImage::ImageRgba8(pixels.clone()),
                ratatui::layout::Size::new(4, 4),
            )
            .unwrap();
            assert_eq!(protocol.size(), ratatui::layout::Size::new(4, 4));
            for (skip, height) in [(0, 2), (1, 2), (3, 1)] {
                let mut buffer = Buffer::empty(Rect::new(0, 0, 6, 6));
                for cell in &mut buffer.content {
                    cell.set_symbol(".");
                }
                let area = Rect::new(1, 1, 4, height);
                render_document_image(&protocol, skip, area, &mut buffer);
                assert_ne!(buffer[(1, 1)].symbol(), ".", "{kind:?} skip={skip}");
                for y in 0..6 {
                    for x in 0..6 {
                        if !area.contains((x, y).into()) {
                            assert_eq!(buffer[(x, y)].symbol(), ".", "{kind:?} escaped viewport");
                        }
                    }
                }
                if kind == ProtocolType::Halfblocks && skip == 1 {
                    assert!(matches!(buffer[(1, 1)].bg, Color::Rgb(r, _, b) if r > b));
                    assert!(matches!(buffer[(1, 2)].bg, Color::Rgb(r, _, b) if b > r));
                }
                if kind == ProtocolType::Iterm2 {
                    use base64::Engine;
                    let sequence = buffer[(1, 1)].symbol();
                    assert!(
                        sequence.contains(&format!("width=4;height={height};")),
                        "{sequence}"
                    );
                    let encoded = sequence
                        .split("doNotMoveCursor=1:")
                        .nth(1)
                        .unwrap()
                        .split('\u{7}')
                        .next()
                        .unwrap();
                    let bytes = base64::engine::general_purpose::STANDARD
                        .decode(encoded)
                        .unwrap();
                    let image = image::load_from_memory(&bytes).unwrap().to_rgba8();
                    assert_eq!(image.dimensions(), (80, u32::from(height) * 40));
                    for row in 0..height {
                        let expected = if skip + row < 2 {
                            [255, 0, 0, 255]
                        } else {
                            [0, 0, 255, 255]
                        };
                        assert_eq!(image.get_pixel(20, u32::from(row) * 40 + 20).0, expected);
                    }
                }
            }
        }
    }

    #[tokio::test]
    async fn scrolling_document_keeps_the_correct_image_rows() {
        use super::super::app::BackgroundMsg;
        let mut app = test_app().await;
        app.picker = ratatui_image::picker::Picker::halfblocks();
        app.display_canvas = meshfox_core::Canvas::from_markdown(
            "# Root\n<!-- meshfox:node id=\"root\" -->\n\nbefore\n\n![plot](plot.png)\n",
        )
        .unwrap();
        let path = app.canvas_path.parent().unwrap().join("plot.png");
        let pixels = image::RgbaImage::from_fn(40, 80, |_, y| {
            if y < 40 {
                image::Rgba([255, 0, 0, 255])
            } else {
                image::Rgba([0, 0, 255, 255])
            }
        });
        let mut bytes = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(pixels)
            .write_to(&mut bytes, image::ImageFormat::Png)
            .unwrap();
        app.on_background_msg(BackgroundMsg::ImageBytes {
            generation: app.image_generation,
            path,
            result: Ok(bytes.into_inner()),
        });
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(12, 8)).unwrap();
        for (scroll, color) in [(0, Color::Rgb(255, 0, 0)), (5, Color::Rgb(0, 0, 255))] {
            app.doc_scroll = scroll;
            terminal
                .draw(|f| render_document(f, Rect::new(0, 0, 12, 8), &mut app))
                .unwrap();
            assert!(
                terminal
                    .backend()
                    .buffer()
                    .content
                    .iter()
                    .any(|cell| cell.bg == color),
                "image disappeared at scroll={scroll}"
            );
        }
        let _ = std::fs::remove_dir_all(app.canvas_path.parent().unwrap());
    }

    #[tokio::test]
    async fn svg_images_render_in_narrow_and_resized_document_panes() {
        use super::super::app::BackgroundMsg;
        let mut app = test_app().await;
        app.picker = ratatui_image::picker::Picker::halfblocks();
        app.display_canvas = meshfox_core::Canvas::from_markdown(
            "# Root\n<!-- meshfox:node id=\"root\" -->\n\n![power](docs/power.svg)\n",
        )
        .unwrap();
        let path = app.canvas_path.parent().unwrap().join("docs/power.svg");
        let svg = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 960 600"><style>:root { --bg:#ff0000 } .bg { fill:var(--bg) }</style><rect class="bg" width="960" height="600"/></svg>"##;
        app.on_background_msg(BackgroundMsg::ImageBytes {
            generation: app.image_generation,
            path: path.clone(),
            result: Ok(svg.to_vec()),
        });
        for (width, height) in [(54, 22), (30, 12), (100, 40)] {
            let area = Rect::new(0, 0, width, height);
            let mut terminal =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
            terminal
                .draw(|f| render_document(f, area, &mut app))
                .unwrap();
            let image = app.doc_images[&path].as_ref().expect("decoded SVG");
            assert!(image.size().width <= width - 2);
            assert!(image.size().height <= height - 4);
            let buffer = terminal.backend().buffer();
            assert!(
                buffer
                    .content
                    .iter()
                    .any(|cell| cell.bg == Color::Rgb(255, 0, 0)),
                "SVG pixels disappeared in {width}x{height}"
            );
        }
        let _ = std::fs::remove_dir_all(app.canvas_path.parent().unwrap());
    }

    #[tokio::test]
    async fn full_width_svg_grows_with_the_document_pane() {
        use super::super::app::BackgroundMsg;
        let mut app = test_app().await;
        app.picker = ratatui_image::picker::Picker::halfblocks();
        app.display_canvas = meshfox_core::Canvas::from_markdown(
            "# Root\n<!-- meshfox:node id=\"root\" -->\n\n![power](power.svg){width=100%}\n",
        )
        .unwrap();
        let path = app.canvas_path.parent().unwrap().join("power.svg");
        let svg = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 960 600"><rect width="960" height="600" fill="#ff0000"/></svg>"##;
        app.on_background_msg(BackgroundMsg::ImageBytes {
            generation: app.image_generation,
            path: path.clone(),
            result: Ok(svg.to_vec()),
        });
        for width in [82, 122, 62] {
            let mut terminal =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, 60)).unwrap();
            terminal
                .draw(|f| render_document(f, Rect::new(0, 0, width, 60), &mut app))
                .unwrap();
            assert_eq!(
                app.doc_images[&path].as_ref().unwrap().size().width,
                width - 2
            );
            let buffer = terminal.backend().buffer();
            assert_eq!(buffer[(width - 2, 3)].bg, Color::Rgb(255, 0, 0));
        }
        let _ = std::fs::remove_dir_all(app.canvas_path.parent().unwrap());
    }

    #[tokio::test]
    async fn document_image_loading_is_not_reported_as_failure() {
        use super::super::app::BackgroundMsg;
        for result in [Ok(true), Ok(false), Err("not found")] {
            let mut app = test_app().await;
            app.picker = ratatui_image::picker::Picker::halfblocks();
            app.display_canvas = meshfox_core::Canvas::from_markdown(
                "# Root\n<!-- meshfox:node id=\"root\" -->\n\n![sensor](sensor.png)\n",
            )
            .unwrap();
            let path = app.canvas_path.parent().unwrap().join("sensor.png");
            app.doc_segments = vec![Segment::Image {
                path: path.clone(),
                alt: "sensor".into(),
                width_percent: None,
                height_percent: None,
                bg: None,
            }];
            app.image_pending.insert(path.clone());
            let area = Rect::new(0, 0, 80, 15);
            let screen = render_to_screen(area, |f| render_document(f, area, &mut app));
            assert!(screen.contains("[image loading: sensor]"));
            assert!(!screen.contains("failed to load"));

            let bytes = match result {
                Ok(true) => {
                    let mut bytes = std::io::Cursor::new(Vec::new());
                    image::DynamicImage::new_rgb8(10, 10)
                        .write_to(&mut bytes, image::ImageFormat::Png)
                        .unwrap();
                    Ok(bytes.into_inner())
                }
                Ok(false) => Ok(b"invalid image".to_vec()),
                Err(error) => Err(error.to_string()),
            };
            app.on_background_msg(BackgroundMsg::ImageBytes {
                generation: app.image_generation,
                path: path.clone(),
                result: bytes,
            });
            assert!(!app.image_pending.contains(&path));
            let screen = render_to_screen(area, |f| render_document(f, area, &mut app));
            assert!(!screen.contains("image loading"));
            assert_eq!(
                screen.contains("[image failed to load: sensor]"),
                result != Ok(true)
            );
            assert_eq!(
                app.doc_images.get(&path).is_some_and(Option::is_some),
                result == Ok(true)
            );
            std::fs::remove_dir_all(app.canvas_path.parent().unwrap()).unwrap();
        }
    }

    // A long keybindings-help line (over the box's old fixed 60-usable-
    // column width) used to just get cut off at the right border —
    // `Paragraph` clips instead of wrapping without an explicit `.wrap()`.
    // On a narrow terminal, `render_help` now wraps it onto another row
    // instead — its own tail text should still be on screen somewhere,
    // not silently dropped. The last word is checked on its own: where the
    // wrap falls depends on the help text, and `esc close)` can end up split
    // across two rows (`esc` at the end of one, `close)` starting the next).
    #[tokio::test]
    async fn render_help_wraps_a_long_line_instead_of_clipping_it() {
        let mut app = test_app().await;
        app.show_help = true;
        let area = Rect::new(0, 0, 40, 60);
        let screen = render_to_screen(area, |f| render_help(f, area, &app));
        assert!(
            screen.contains("close)"),
            "a long help line's own tail should still appear (wrapped), not be clipped off:\n{screen}"
        );
    }

    #[test]
    fn history_time_is_relative_like_the_web_panel() {
        let now = meshfox_core::timestamp::now_utc_rfc3339();
        assert_eq!(history_time(&now), "just now");
        assert_eq!(
            history_time("2020-01-15T12:00:00Z").chars().last(),
            Some('o'),
            "days/years ago"
        );
        assert!(history_time("2020-01-15T12:00:00Z").ends_with("d ago"));
        assert_eq!(history_time(""), "");
    }

    #[test]
    fn render_history_view_records_where_the_list_was_drawn_and_how_far_it_scrolled() {
        let entries = (0..40)
            .rev()
            .map(|seq| crate::worker_client::HistoryEntryDto {
                seq,
                created_at: String::new(),
                op_kind: "node_body".into(),
                applied: true,
                summary: format!("step {seq}"),
            })
            .collect();
        let hv = super::super::app::HistoryViewState {
            entries,
            selected: 35,
            ..Default::default()
        };
        let area = Rect::new(0, 0, 90, 12);
        let _ = render_to_screen(area, |f| render_history_view(f, area, &hv));
        let rect = hv.list_rect.get();
        assert!(
            rect.width > 0 && rect.height > 0,
            "list rect recorded: {rect:?}"
        );
        assert!(
            hv.offset.get() > 0,
            "a deep selection scrolls the list, and the offset is recorded"
        );
        assert!(
            hv.offset.get() <= 35 && 35 < hv.offset.get() + rect.height as usize,
            "selection is on screen"
        );
    }

    #[test]
    fn render_run_history_view_says_a_tty_runs_output_is_not_stored() {
        use crate::worker_client::RunHistoryEntryDto;
        let rv = super::super::app::RunHistoryViewState {
            node_id: "shell".into(),
            block: "interactive".into(),
            tty: true,
            entries: vec![RunHistoryEntryDto {
                id: 1,
                outcome: "exited".into(),
                exit_code: Some(0),
                started_at: "2026-09-30T12:00:00Z".into(),
                duration_ms: Some(4200),
                stale: false,
            }],
            output: Some((1, Vec::new())),
            ..Default::default()
        };
        let area = Rect::new(0, 0, 100, 24);
        let screen = render_to_screen(area, |f| render_run_history_view(f, area, &rv));
        assert!(screen.contains("exit 0"), "{screen}");
        assert!(
            screen.contains("a tty session's output isn't stored"),
            "{screen}"
        );
    }

    #[test]
    fn render_run_history_view_lists_runs_marks_stale_ones_and_shows_the_selected_output() {
        use crate::worker_client::RunHistoryEntryDto;
        let run = |id, exit_code, stale| RunHistoryEntryDto {
            id,
            outcome: "exited".into(),
            exit_code: Some(exit_code),
            started_at: "2026-09-30T12:00:00Z".into(),
            duration_ms: Some(2500),
            stale,
        };
        let rv = super::super::app::RunHistoryViewState {
            node_id: "leaf".into(),
            block: "build".into(),
            entries: vec![run(3, 0, false), run(2, 1, true)],
            selected: 1,
            output: Some((
                2,
                vec![
                    (
                        meshfox_server::stream_exec::OutputStream::Stdout,
                        "\u{1b}[31mred text\u{1b}[0m".to_string(),
                    ),
                    (
                        meshfox_server::stream_exec::OutputStream::Stderr,
                        "boom".to_string(),
                    ),
                ],
            )),
            ..Default::default()
        };
        let area = Rect::new(0, 0, 100, 24);
        let screen = render_to_screen(area, |f| render_run_history_view(f, area, &rv));
        assert!(
            screen.contains("run history — leaf/build"),
            "titled with the block:\n{screen}"
        );
        assert!(
            screen.contains("exit 0") && screen.contains("exit 1"),
            "both runs listed:\n{screen}"
        );
        assert!(screen.contains("2.5s"), "duration shown:\n{screen}");
        assert!(
            screen.contains("stale"),
            "the stale run is marked:\n{screen}"
        );
        assert!(
            screen.contains("red text") && !screen.contains("[31m"),
            "ANSI rendered:\n{screen}"
        );
        assert!(screen.contains("boom"), "stderr shown too:\n{screen}");
        assert!(
            rv.list_rect.get().width > 0,
            "list position recorded for mouse hit-testing"
        );
    }

    #[tokio::test]
    async fn render_var_form_keeps_secret_input_visible_below_long_prompt() {
        let statuses = serde_json::from_value(serde_json::json!([
            {"name": "TOKEN", "type": "string",
             "prompt": "Personal Access Token Firefly III (пусто — пропустить проверку)".repeat(3),
             "secret": true, "resolved": false, "secretStore": "keychain"}
        ]))
        .unwrap();
        let mut vf = super::super::app::var_form_from_statuses_for_test(statuses);
        vf.configuring = true;
        for width in [50, 80, 100] {
            let area = Rect::new(0, 0, width, 16);
            let screen = render_to_screen(area, |f| render_var_form(f, area, &vf));
            assert!(screen.contains("_ (not set)"), "{screen}");
            vf.inputs[0] = "secret-token".repeat(20);
            let screen = render_to_screen(area, |f| render_var_form(f, area, &vf));
            assert!(screen.contains("*_ (not set)"), "{screen}");
            assert!(!screen.contains("secret-token"), "{screen}");
            vf.inputs[0].clear();
        }
    }

    #[tokio::test]
    async fn render_var_form_labels_the_secret_save_toggle_and_shows_a_store_error() {
        let statuses: Vec<crate::worker_client::VarStatus> = serde_json::from_value(serde_json::json!([
            {"name": "TOKEN", "type": "string", "prompt": "TOKEN", "secret": true, "resolved": false,
             "secretStore": "keychain", "secretError": "keychain is locked"}
        ]))
        .unwrap();
        let mut vf = super::super::app::var_form_from_statuses_for_test(statuses);
        let area = Rect::new(0, 0, 100, 12);
        let screen = render_to_screen(area, |f| render_var_form(f, area, &vf));
        assert!(screen.contains("[ ] save→keychain"), "{screen}");
        assert!(screen.contains("keychain is locked"), "{screen}");
        vf.save[0] = true;
        let screen = render_to_screen(area, |f| render_var_form(f, area, &vf));
        assert!(screen.contains("[x] save→keychain"), "{screen}");
    }

    #[tokio::test]
    async fn render_history_view_marks_the_current_step_and_lists_newest_first() {
        use crate::worker_client::HistoryEntryDto;
        let entry = |seq, applied, summary: &str| HistoryEntryDto {
            seq,
            created_at: "2026-09-29T12:41:05Z".into(),
            op_kind: "node_body".into(),
            applied,
            summary: summary.into(),
        };
        let hv = super::super::app::HistoryViewState {
            entries: vec![
                entry(3, false, "third undone"),
                entry(2, true, "second"),
                entry(1, true, "first"),
            ],
            selected: 1,
            ..Default::default()
        };
        let area = Rect::new(0, 0, 90, 20);
        let screen = render_to_screen(area, |f| render_history_view(f, area, &hv));
        let (third, second, first) = (
            screen.find("third undone").expect("redo-tail row"),
            screen.find("second").expect("current row"),
            screen.find("first").expect("oldest row"),
        );
        assert!(third < second && second < first, "newest first:\n{screen}");
        assert!(
            screen.contains("▶ #2"),
            "current (newest applied) step is marked:\n{screen}"
        );
        assert!(screen.contains("ago"), "relative time shown:\n{screen}");
        assert!(
            !screen.contains("2026-09-29T"),
            "not the raw timestamp:\n{screen}"
        );
    }

    // On a terminal too short to fit every keybindings-help line at once,
    // the last line used to just be silently dropped (`Paragraph` clips
    // instead of scrolling without an explicit `.scroll()`). Scrolling
    // (`App::help_scroll`, driven by `on_help_key`) should reach it.
    #[tokio::test]
    async fn render_help_scrolls_to_reach_content_that_does_not_fit() {
        let mut app = test_app().await;
        app.show_help = true;
        let area = Rect::new(0, 0, 100, 10);
        let unscrolled = render_to_screen(area, |f| render_help(f, area, &app));
        assert!(
            !unscrolled.contains("this UI reappears once it exits"),
            "the last help line shouldn't already be visible on such a short terminal:\n{unscrolled}"
        );

        // `render_help` clamps `help_scroll` against the actual wrapped
        // row count (see its own doc comment) — a value this large just
        // means "scroll all the way to the bottom", same as
        // `on_help_key`'s own repeated-`j`/`PageDown` presses would
        // eventually reach.
        app.help_scroll = u16::MAX;
        let scrolled = render_to_screen(area, |f| render_help(f, area, &app));
        assert!(
            scrolled.contains("this UI reappears once it exits"),
            "scrolling down should eventually reach the last help line:\n{scrolled}"
        );
    }

    // TODO.canvas.md: "Tags in TUI" — end to end through `render_tree`
    // itself, over a real `App`/canvas, mirroring the `render_document`
    // integration test above.
    #[tokio::test]
    async fn tree_mouse_targets_follow_wrapped_rows_and_scroll() {
        use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
        use ratatui::{backend::TestBackend, Terminal};
        let dir = std::env::temp_dir().join(format!("meshfox-tree-click-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("canvas.md");
        std::fs::write(&path, "<!-- meshfox:canvas -->\n# A long root title with many words that wraps\n<!-- meshfox:node id=\"root\" -->\n## First\n<!-- meshfox:node id=\"first\" -->\n## Second\n<!-- meshfox:node id=\"second\" -->\n").unwrap();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(path, tx, None, None).await.unwrap();
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| render(f, &mut app)).unwrap();
        let root = app.tree_click_rows[0].0;
        assert!(root.height > 1);
        // A continuation line selects its own item and never folds it.
        let click = |x, y| MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: x,
            row: y,
            modifiers: KeyModifiers::NONE,
        };
        app.on_mouse(click(root.x, root.y + 1)).await;
        assert_eq!(app.selected, 0);
        assert_eq!(app.rows.len(), 3);
        let (first, index) = app.tree_click_rows[1];
        app.on_mouse(click(first.x + 5, first.y)).await;
        assert_eq!(app.selected, index);
        app.selected = 2;
        terminal = Terminal::new(TestBackend::new(80, 7)).unwrap();
        terminal.draw(|f| render(f, &mut app)).unwrap();
        assert!(app.list_state.offset() > 0);
        let (rect, index) = app.tree_click_rows[0];
        app.on_mouse(click(rect.x + 5, rect.y)).await;
        assert_eq!(app.selected, index);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn render_tree_shows_a_nodes_tags_next_to_its_title() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let dir = std::env::temp_dir().join(format!(
            "meshfox-render-tree-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("canvas.canvas.md");
        std::fs::write(
            &path,
            "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" tags=\"bag,improvement\" -->\n",
        )
        .unwrap();

        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(path, tx, None, None)
            .await
            .expect("valid test canvas");

        let backend = TestBackend::new(40, 10);
        let mut terminal = Terminal::new(backend).unwrap();
        let area = Rect::new(0, 0, 40, 10);
        terminal.draw(|f| render_tree(f, area, &mut app)).unwrap();

        let buf = terminal.backend().buffer();
        let mut screen = String::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                if let Some(cell) = buf.cell((x, y)) {
                    screen.push_str(cell.symbol());
                }
            }
            screen.push('\n');
        }
        assert!(
            screen.contains("#bag #improvement"),
            "the node's tags should show up next to its title:\n{screen}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn revealing_block_preserves_visible_headers_and_moves_only_when_needed() {
        assert_eq!(reveal_header_scroll(10, 8, 10), 10);
        assert_eq!(reveal_header_scroll(10, 8, 15), 10);
        assert_eq!(reveal_header_scroll(10, 8, 17), 10);
        assert_eq!(reveal_header_scroll(10, 8, 18), 11);
        assert_eq!(reveal_header_scroll(10, 8, 3), 3);
    }

    #[test]
    fn block_controls_hit_test_after_wrapping_and_wide_glyphs() {
        let line = Line::from("┌─[▾] [code ▾] sh · 構築 [history] [run]");
        let prefix = "┌─[▾] [code ▾] sh · 構築 ";
        let start = Span::raw(prefix).width() as u16;
        let rects = wrapped_click_rects(&line, 18, start, start + 9);
        assert!(!rects.is_empty());
        assert!(rects.iter().all(|r| r.y > 0 && r.x + r.width <= 18));
        let wide = wrapped_click_rects(&Line::from("ab構築z"), 20, 2, 6);
        assert_eq!(wide, vec![Rect::new(2, 0, 4, 1)]);
    }

    #[tokio::test]
    async fn document_heading_shows_color_title_and_tags_and_scrolls_with_body() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let dir =
            std::env::temp_dir().join(format!("meshfox-document-heading-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("canvas.canvas.md");
        std::fs::write(&path, "<!-- meshfox:canvas -->\n# A long node title\n<!-- meshfox:node id=\"root\" color=\"4\" tags=\"bag,improvement\" -->\n\nBody text here.\n").unwrap();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(path, tx, None, None).await.unwrap();
        let area = Rect::new(0, 0, 24, 7);
        let mut terminal = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();
        terminal
            .draw(|f| render_document(f, area, &mut app))
            .unwrap();
        let buf = terminal.backend().buffer();
        let screen: String = (0..area.height)
            .map(|y| {
                let row: String = (0..area.width)
                    .map(|x| buf.cell((x, y)).unwrap().symbol())
                    .collect();
                format!("{row}\n")
            })
            .collect();
        assert!(screen.contains("Document"), "{screen}");
        assert!(screen.contains("● A long node title"), "{screen}");
        assert!(screen.contains("#bag"), "{screen}");
        assert!(screen.contains("#improvement"), "{screen}");
        assert_eq!(buf.cell((1, 1)).unwrap().fg, Color::Rgb(0x3d, 0x9e, 0x4f));

        app.doc_scroll = 3;
        terminal
            .draw(|f| render_document(f, area, &mut app))
            .unwrap();
        let buf = terminal.backend().buffer();
        let scrolled: String = (1..area.height - 1)
            .flat_map(|y| {
                (1..area.width - 1).map(move |x| buf.cell((x, y)).unwrap().symbol().to_string())
            })
            .collect();
        assert!(!scrolled.contains("A long node title"), "{scrolled}");
        assert!(scrolled.contains("Body text here."), "{scrolled}");
        let _ = std::fs::remove_dir_all(dir);
    }

    // TODO.canvas.md: "Переносы строк в дереве нод в TUI" — end to end
    // through `render_tree` itself: a node with a long title, tags, and a
    // runnable+cache badge, in a pane too narrow for any of it to fit on
    // one row. Before wrapping, `List`'s own no-wrap rendering would just
    // clip everything past the pane's width — this asserts the title's
    // own tail, every tag, and the badge all still show up somewhere on
    // screen (on wrapped continuation rows), not silently dropped.
    #[tokio::test]
    async fn render_tree_wraps_a_long_title_instead_of_clipping_it_or_its_tags_and_badge() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let dir = std::env::temp_dir().join(format!(
            "meshfox-render-tree-wrap-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("canvas.canvas.md");
        std::fs::write(
            &path,
            "<!-- meshfox:canvas -->\n\
             # ALFA BRAVO CHARLIE DELTA ECHO\n\
             <!-- meshfox:node id=\"root\" tags=\"bag,improvement\" -->\n\
             \n\
             ```bash name=\"build\" cache\n\
             cargo build\n\
             ```\n",
        )
        .unwrap();

        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(path, tx, None, None)
            .await
            .expect("valid test canvas");

        // Narrow enough that "ALFA BRAVO CHARLIE DELTA ECHO  #bag #improvement  [run,cache]"
        // (73+ chars) cannot possibly fit on one row.
        let backend = TestBackend::new(20, 10);
        let mut terminal = Terminal::new(backend).unwrap();
        let area = Rect::new(0, 0, 20, 10);
        terminal.draw(|f| render_tree(f, area, &mut app)).unwrap();

        let buf = terminal.backend().buffer();
        let mut screen = String::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                if let Some(cell) = buf.cell((x, y)) {
                    screen.push_str(cell.symbol());
                }
            }
            screen.push('\n');
        }
        for needle in ["ALFA", "ECHO", "#bag", "#improvement", "[run,cache]"] {
            assert!(
                screen.contains(needle),
                "{needle:?} should still be visible somewhere on screen, wrapped rather than clipped:\n{screen}"
            );
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    // TODO.canvas.md: "Цвета в TUI" — a node's own `color` (a JSON-Canvas
    // preset "1"-"6" or a literal `#rrggbb` hex) should color its title in
    // the tree pane, same palette the web UI and the PDF export already
    // use (`resolve_color_hex`).
    #[test]
    fn tree_row_color_resolves_a_numbered_preset() {
        assert_eq!(
            tree_row_color(Some("1")),
            Some(Color::Rgb(0xc2, 0x2b, 0x2b))
        );
        assert_eq!(
            tree_row_color(Some("4")),
            Some(Color::Rgb(0x3d, 0x9e, 0x4f))
        );
    }

    #[test]
    fn tree_row_color_resolves_a_literal_hex_string() {
        assert_eq!(
            tree_row_color(Some("#a05dd1")),
            Some(Color::Rgb(0xa0, 0x5d, 0xd1))
        );
    }

    #[test]
    fn tree_row_color_is_none_for_unset_or_malformed_input() {
        assert_eq!(tree_row_color(None), None);
        assert_eq!(tree_row_color(Some("not-a-color")), None);
        assert_eq!(tree_row_color(Some("#zzzzzz")), None);
    }

    // TODO.canvas.md: "Переносы строк в дереве нод в TUI" — a title (plus
    // its tags/badge) too long for the pane should wrap onto more rows,
    // not get silently clipped by `List`'s own no-wrap rendering.
    #[test]
    fn wrap_word_indices_packs_everything_on_one_row_when_it_fits() {
        let widths = [4, 2, 5]; // e.g. "Root", "✓", "#bag" — 4+1+2+1+5 = 13
        assert_eq!(wrap_word_indices(&widths, 20, 20), vec![vec![0, 1, 2]]);
    }

    #[test]
    fn wrap_word_indices_wraps_to_a_new_row_when_the_budget_is_exceeded() {
        // First row (narrow: room for one 4-wide word only) gets just the
        // title; both remaining words then fit together on the wider
        // continuation row.
        let widths = [4, 3, 3];
        assert_eq!(wrap_word_indices(&widths, 4, 10), vec![vec![0], vec![1, 2]]);
    }

    #[test]
    fn wrap_word_indices_uses_the_narrower_continuation_budget_after_the_first_row() {
        // First row has less room (disclosure/type marker prefix); second
        // word alone fits the wider first budget but not appended after
        // the first word, so it starts row 2 — which then has *more* room
        // than row 1, fitting the third word too.
        let widths = [3, 3, 3];
        assert_eq!(wrap_word_indices(&widths, 3, 10), vec![vec![0], vec![1, 2]]);
    }

    #[test]
    fn wrap_word_indices_gives_an_oversized_single_word_its_own_row_without_splitting_it() {
        let widths = [20];
        assert_eq!(wrap_word_indices(&widths, 5, 5), vec![vec![0]]);
    }

    #[test]
    fn wrap_word_indices_of_no_words_is_a_single_empty_row() {
        assert_eq!(wrap_word_indices(&[], 20, 20), vec![Vec::<usize>::new()]);
    }

    #[test]
    fn tail_fit_leaves_a_short_value_untouched() {
        assert_eq!(tail_fit("hello", 10), "hello");
        assert_eq!(tail_fit("hello", 5), "hello");
    }

    #[test]
    fn tail_fit_truncates_from_the_front_with_a_leading_ellipsis() {
        assert_eq!(tail_fit("hello world", 5), "…orld");
        assert_eq!(tail_fit("hello world", 1), "d");
        assert_eq!(tail_fit("hello world", 0), "");
    }

    // TODO.canvas.md: "Горизонтальная промотка в TUI-редакторе" — typing a
    // value long enough to outgrow its row used to just push the trailing
    // cursor marker (`render_var_form`'s `"{shown}_"`) off the right edge
    // of the terminal for good: a plain `List`/`Line` never wraps or
    // scrolls on its own the way edtui's source editor does. Simulates the
    // actual interaction — pushing one character at a time, exactly like
    // `App::on_key`'s `vf.inputs[i].push(c)` — rather than just checking
    // one fixed long string, since the bug was specifically about a value
    // that *grows past* the visible width mid-typing, not one that already
    // starts too long.
    #[test]
    fn typing_past_the_row_width_keeps_the_cursor_marker_visible() {
        let budget = 10;
        let mut value = String::new();
        for ch in "this is a lot longer than the row".chars() {
            value.push(ch);
            let shown = format!("{value}_"); // mirrors render_var_form's own "{shown}{cursor}"
            let fitted = tail_fit(&shown, budget);
            assert!(
                fitted.chars().count() <= budget,
                "fitted value {fitted:?} exceeds the {budget}-char budget"
            );
            assert!(
                fitted.ends_with('_'),
                "cursor marker fell out of view while typing {value:?}, got {fitted:?}"
            );
        }
    }
}
