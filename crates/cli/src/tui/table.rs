//! `display="table"` in the terminal viewer (SPEC.md "Table previews"): the
//! client side of the worker's `/table` and `/table/rows` endpoints.
//!
//! Two views share one [`TableData`] per node:
//!
//! - an **inline** window (header plus the first few rows, built by
//!   [`inline_lines`] as ordinary document text under the node's title), and
//! - a **full-screen** mode ([`TableMode`], drawn by [`render_overlay`]) with
//!   a cursor, sorting, per-column filters and search — the keyboard/mouse
//!   handling lives in `App` (`on_table_key`/`on_table_mouse`), which owns the
//!   background fetches; everything here is state and drawing.
//!
//! Rows arrive in blocks of [`BLOCK_ROWS`] over the same channel the other
//! background fetches use (`BackgroundMsg`). A block is cached under the
//! current *generation*, which bumps whenever the view or the table's
//! version changes, so a response that was already in flight for an older
//! view is dropped instead of painting stale rows.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::time::Instant;

use meshfox_server::tables::{Filter, FilterOp, SortKey, ViewSpec};
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};

use super::theme::{ACCENT, BORDER, FAIL, OK};
use crate::worker_client::{TableColumnDto, TableMetaDto, TableRowsDto};

/// Rows per fetched block — the same figure the web grid uses.
pub const BLOCK_ROWS: usize = 100;
/// Rows the inline window shows.
pub const INLINE_ROWS: usize = 10;
const MAX_CELL_WIDTH: usize = 32;
const MIN_COL_WIDTH: usize = 4;
const COL_GAP: &str = " │ ";

// ---------------------------------------------------------------------------
// Data
// ---------------------------------------------------------------------------

/// What this client knows about one table node: schema, the view being shown
/// and the blocks of rows fetched for it.
#[derive(Default)]
pub struct TableData {
    pub meta: Option<TableMetaDto>,
    /// The last failure to even ask the worker (`GET /table` itself failed).
    pub meta_error: Option<String>,
    pub meta_inflight: bool,
    pub meta_fetched: Option<Instant>,
    /// The sort/filters/search currently applied (empty until the import is
    /// done — the worker can't sort a table it hasn't imported yet).
    pub view: ViewSpec,
    pages: HashMap<usize, Vec<Vec<Option<String>>>>,
    pending: HashSet<usize>,
    matched: Option<u64>,
    /// Rows seen so far while importing (the preview has no total).
    known_rows: usize,
    /// The worker's complaint about the current view (a filter value the
    /// column can't hold, say).
    pub view_error: Option<String>,
    /// Bumped whenever cached rows stop being valid.
    pub generation: u64,
}

impl TableData {
    pub fn columns(&self) -> &[TableColumnDto] {
        self.meta.as_ref().map_or(&[], |m| &m.columns)
    }

    pub fn is_ready(&self) -> bool {
        self.meta.as_ref().is_some_and(|m| m.state == "ready")
    }

    pub fn is_importing(&self) -> bool {
        self.meta.as_ref().is_some_and(|m| m.state == "importing")
    }

    /// Rows in the current view, if known: the table's total for the plain
    /// view, the worker's match count for a sorted/filtered one, and while
    /// importing just how many preview rows have arrived.
    pub fn row_count(&self) -> Option<usize> {
        if self.view.is_empty() {
            if let Some(total) = self.meta.as_ref().and_then(|m| m.total_rows) {
                return Some(total as usize);
            }
        }
        if let Some(m) = self.matched {
            return Some(m as usize);
        }
        (self.is_importing() && self.known_rows > 0).then_some(self.known_rows)
    }

    pub fn total_rows(&self) -> Option<usize> {
        self.meta
            .as_ref()
            .and_then(|m| m.total_rows)
            .map(|t| t as usize)
    }

    pub fn row(&self, index: usize) -> Option<&Vec<Option<String>>> {
        self.pages
            .get(&(index / BLOCK_ROWS))
            .and_then(|rows| rows.get(index % BLOCK_ROWS))
    }

    /// A fresh `GET /table` answer. A new version or a state change (the
    /// import finishing, the file changing) invalidates the cached rows.
    pub fn apply_meta(&mut self, meta: TableMetaDto) {
        let changed = self
            .meta
            .as_ref()
            .is_none_or(|old| old.version != meta.version || old.state != meta.state);
        self.meta = Some(meta);
        self.meta_error = None;
        if changed {
            self.reset_cache();
        }
    }

    pub fn reset_cache(&mut self) {
        self.generation += 1;
        self.pages.clear();
        self.pending.clear();
        self.matched = None;
        self.known_rows = 0;
        self.view_error = None;
    }

    pub fn set_view(&mut self, view: ViewSpec) {
        self.view = view;
        self.reset_cache();
    }

    /// The blocks to fetch so rows `first .. first + count` (plus `margin`
    /// blocks either side) are covered, marking them pending. Until the view's
    /// row count is known only block 0 is asked for.
    pub fn blocks_to_fetch(&mut self, first: usize, count: usize, margin: usize) -> Vec<usize> {
        let wanted: Vec<usize> = match self.row_count() {
            None => vec![0],
            Some(0) => Vec::new(),
            Some(rows) => {
                let last = (first + count).min(rows - 1);
                let from = (first / BLOCK_ROWS).saturating_sub(margin);
                let to = (last / BLOCK_ROWS + margin).min((rows - 1) / BLOCK_ROWS);
                (from..=to).collect()
            }
        };
        let mut missing = Vec::new();
        for b in wanted {
            if !self.pages.contains_key(&b) && self.pending.insert(b) {
                missing.push(b);
            }
        }
        missing
    }

    /// Stores a block, unless it answers a view/version that's gone.
    pub fn apply_rows(&mut self, generation: u64, block: usize, page: TableRowsDto) -> bool {
        self.pending.remove(&block);
        let current = self.meta.as_ref().map(|m| m.version.as_str());
        if generation != self.generation || current != Some(page.version.as_str()) {
            return false;
        }
        match page.matched_rows {
            Some(n) => self.matched = Some(n),
            None => self.known_rows = self.known_rows.max(page.offset + page.rows.len()),
        }
        self.pages.insert(block, page.rows);
        true
    }

    pub fn fail_rows(&mut self, generation: u64, block: usize, message: String) {
        self.pending.remove(&block);
        if generation == self.generation {
            self.view_error = Some(message);
        }
    }
}

// ---------------------------------------------------------------------------
// Filter language and sorting (the same as the web grid's `tableView.ts`)
// ---------------------------------------------------------------------------

const PREFIXES: [(&str, FilterOp); 10] = [
    (">=", FilterOp::Ge),
    ("<=", FilterOp::Le),
    ("!=", FilterOp::Ne),
    ("<>", FilterOp::Ne),
    (">", FilterOp::Gt),
    ("<", FilterOp::Lt),
    ("=", FilterOp::Eq),
    ("^", FilterOp::StartsWith),
    ("$", FilterOp::EndsWith),
    ("~", FilterOp::Contains),
];

/// What a user types into a column's filter prompt: `text` (contains, or
/// equals for numbers and booleans), `>10 >=10 <10 <=10 =x !=x`, `^starts
/// $ends ~contains`, `null`, `!null`. An empty text, or an operator without a
/// value, filters nothing.
pub fn parse_filter_expression(text: &str, kind: &str) -> Option<(FilterOp, Option<String>)> {
    let t = text.trim();
    if t.is_empty() {
        return None;
    }
    match t.to_lowercase().as_str() {
        "null" => return Some((FilterOp::IsNull, None)),
        "!null" => return Some((FilterOp::NotNull, None)),
        _ => {}
    }
    for (prefix, op) in PREFIXES {
        if let Some(rest) = t.strip_prefix(prefix) {
            let value = rest.trim();
            return (!value.is_empty()).then(|| (op, Some(value.to_string())));
        }
    }
    let op = if kind == "number" || kind == "bool" {
        FilterOp::Eq
    } else {
        FilterOp::Contains
    };
    Some((op, Some(t.to_string())))
}

/// The worker-side filters for the per-column filter texts, in column order.
pub fn build_filters(texts: &HashMap<usize, String>, columns: &[TableColumnDto]) -> Vec<Filter> {
    columns
        .iter()
        .filter_map(|c| {
            let (op, value) = parse_filter_expression(texts.get(&c.index)?, &c.kind)?;
            Some(Filter {
                column: c.index,
                op,
                value,
            })
        })
        .collect()
}

/// A sort request on `column`: none → ascending → descending → none. Not
/// `additive` makes it the only key; additive adds to, updates in, or removes
/// from the existing keys.
pub fn toggle_sort(sort: &[SortKey], column: usize, additive: bool) -> Vec<SortKey> {
    let existing = sort.iter().find(|k| k.column == column);
    let next = match existing {
        None => Some(SortKey {
            column,
            desc: false,
        }),
        Some(k) if !k.desc => Some(SortKey { column, desc: true }),
        Some(_) => None,
    };
    if !additive {
        return next.into_iter().collect();
    }
    let mut out: Vec<SortKey> = sort.to_vec();
    match (existing.is_some(), next) {
        (true, Some(n)) => {
            for k in &mut out {
                if k.column == column {
                    *k = n.clone();
                }
            }
        }
        (true, None) => out.retain(|k| k.column != column),
        (false, Some(n)) => out.push(n),
        (false, None) => {}
    }
    out
}

// ---------------------------------------------------------------------------
// Cells and widths
// ---------------------------------------------------------------------------

fn display_width(s: &str) -> usize {
    Span::raw(s).width()
}

/// Control characters would corrupt the terminal; a line break reads as `⏎`.
fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '\n' | '\r' => '⏎',
            '\t' => ' ',
            c if c.is_control() => '�',
            c => c,
        })
        .collect()
}

/// `s` cut to `width` display columns, ending in `…` when cut.
fn fit(s: &str, width: usize) -> String {
    if display_width(s) <= width {
        return s.to_string();
    }
    let mut out = String::new();
    for c in s.chars() {
        let mut next = out.clone();
        next.push(c);
        if display_width(&next) + 1 > width {
            break;
        }
        out = next;
    }
    out.push('…');
    out
}

fn pad(s: &str, width: usize, right: bool) -> String {
    let gap = width.saturating_sub(display_width(s));
    if right {
        format!("{}{s}", " ".repeat(gap))
    } else {
        format!("{s}{}", " ".repeat(gap))
    }
}

fn cell_text(cell: Option<&Option<String>>) -> (String, bool) {
    match cell {
        Some(Some(v)) => (sanitize(v), false),
        Some(None) => ("NULL".to_string(), true),
        None => (String::new(), false),
    }
}

/// Column widths from the header and whatever rows are loaded at the head of
/// the table (so they don't jitter while scrolling), capped per column.
fn column_widths(data: &TableData, marks: bool) -> Vec<usize> {
    // The full-screen header appends a sort arrow (` ▲2`) and a filter mark
    // (` ⧩`) to a column's name — leave room so they aren't cut off.
    let reserve = if marks { 5 } else { 0 };
    data.columns()
        .iter()
        .map(|c| {
            let mut w =
                (display_width(&c.name) + reserve).max(display_width(&c.col_type.to_lowercase()));
            for i in 0..BLOCK_ROWS {
                match data.row(i).and_then(|r| r.get(c.index)) {
                    Some(Some(v)) => w = w.max(display_width(&sanitize(v))),
                    Some(None) => w = w.max(4),
                    None => {}
                }
            }
            w.clamp(MIN_COL_WIDTH, MAX_CELL_WIDTH)
        })
        .collect()
}

fn format_count(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn format_bytes(n: u64) -> String {
    const UNITS: [&str; 4] = ["KiB", "MiB", "GiB", "TiB"];
    if n < 1024 {
        return format!("{n} B");
    }
    let mut v = n as f64 / 1024.0;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if v >= 10.0 {
        format!("{} {}", v.round() as u64, UNITS[i])
    } else {
        format!("{v:.1} {}", UNITS[i])
    }
}

/// `1,234 rows × 5 cols`, `12 of 1,234 rows · 5 cols` when filtered, or the
/// importing note.
pub fn status_text(data: &TableData) -> String {
    let Some(meta) = &data.meta else {
        return "loading…".to_string();
    };
    let cols = meta.columns.len();
    if meta.state == "importing" {
        return format!(
            "importing… first {} rows shown",
            format_count(data.known_rows)
        );
    }
    match (data.row_count(), data.total_rows()) {
        (Some(m), Some(t)) if m != t => {
            format!(
                "{} of {} rows · {cols} cols",
                format_count(m),
                format_count(t)
            )
        }
        (_, Some(t)) => format!("{} rows × {cols} cols", format_count(t)),
        _ => format!("{cols} cols"),
    }
}

// ---------------------------------------------------------------------------
// Inline window
// ---------------------------------------------------------------------------

/// The node's table as document text: status line, header, the first
/// [`INLINE_ROWS`] rows, and the hint for the full-screen mode. Truncated to
/// `max_width` columns (as many table columns as fit, then `…`) so the
/// document pane never has to wrap it.
pub fn inline_lines(data: &TableData, max_width: usize) -> Vec<Line<'static>> {
    let dim = Style::default().fg(Color::DarkGray);
    let err = |m: String| vec![Line::from(Span::styled(m, Style::default().fg(FAIL)))];
    if let Some(e) = &data.meta_error {
        if data.meta.is_none() {
            return err(format!("table preview unavailable: {e}"));
        }
    }
    let Some(meta) = &data.meta else {
        return vec![Line::from(Span::styled("loading table…", dim))];
    };
    if meta.state == "failed" {
        let message = meta
            .error
            .as_ref()
            .map_or("table preview failed", |e| e.message.as_str());
        return err(message.to_string());
    }

    let mut lines = vec![Line::from(Span::styled(
        status_text(data),
        Style::default().add_modifier(Modifier::BOLD),
    ))];
    let widths = column_widths(data, false);
    let columns = data.columns();

    // How many columns fit within `max_width`: every one if they all do,
    // otherwise as many as leave room for the "… +N cols" tail (at least one).
    let cost = |i: usize| widths[i] + if i > 0 { COL_GAP.chars().count() } else { 0 };
    let all: usize = (0..columns.len()).map(cost).sum();
    let mut shown = columns.len();
    if all > max_width {
        const TAIL: usize = 14;
        let mut used = 0;
        shown = 0;
        for i in 0..columns.len() {
            if used + cost(i) + TAIL > max_width && shown > 0 {
                break;
            }
            used += cost(i);
            shown += 1;
        }
    }
    let hidden = columns.len() - shown;

    let row_line = |cells: Vec<(String, Style)>| -> Line<'static> {
        let mut spans = Vec::new();
        for (i, (text, style)) in cells.into_iter().enumerate() {
            if i > 0 {
                spans.push(Span::styled(COL_GAP, dim));
            }
            spans.push(Span::styled(text, style));
        }
        if hidden > 0 {
            spans.push(Span::styled(format!("  … +{hidden} cols"), dim));
        }
        Line::from(spans)
    };

    let header = (0..shown)
        .map(|i| {
            let c = &columns[i];
            (
                pad(&fit(&c.name, widths[i]), widths[i], false),
                Style::default().add_modifier(Modifier::BOLD),
            )
        })
        .collect();
    lines.push(row_line(header));
    let rule: Vec<(String, Style)> = (0..shown).map(|i| ("─".repeat(widths[i]), dim)).collect();
    lines.push(row_line(rule));

    let available = data.row_count().unwrap_or(0).min(INLINE_ROWS);
    for r in 0..available {
        let Some(row) = data.row(r) else {
            lines.push(Line::from(Span::styled("…", dim)));
            continue;
        };
        let cells = (0..shown)
            .map(|i| {
                let c = &columns[i];
                let (text, null) = cell_text(row.get(c.index));
                let right = c.kind == "number";
                (
                    pad(&fit(&text, widths[i]), widths[i], right),
                    if null { dim } else { Style::default() },
                )
            })
            .collect();
        lines.push(row_line(cells));
    }
    if available == 0 && data.row_count() == Some(0) {
        lines.push(Line::from(Span::styled("(no rows)", dim)));
    }

    let size = format_bytes(meta.file_size);
    let hint = if meta.state == "importing" {
        format!("{size} · importing in the background — Enter opens it full screen")
    } else {
        format!("{size} · Enter: open full screen to scroll, sort and filter")
    };
    lines.push(Line::from(Span::styled(fit(&hint, max_width), dim)));
    lines
}

// ---------------------------------------------------------------------------
// Full-screen mode
// ---------------------------------------------------------------------------

/// A line being typed at the bottom of the full-screen mode.
pub enum TableInput {
    Search(String),
    Filter { column: usize, text: String },
}

/// Where the last frame put things — for hit-testing the mouse.
#[derive(Default, Clone)]
pub struct TableGeometry {
    pub header_y: u16,
    pub body: Rect,
    /// `(x_start, x_end_exclusive, column index)` of each drawn column.
    pub columns: Vec<(u16, u16, usize)>,
}

pub struct TableMode {
    pub node_id: String,
    pub title: String,
    pub cursor_row: usize,
    pub cursor_col: usize,
    pub top_row: usize,
    /// First drawn column; adjusted at draw time so the cursor stays visible.
    pub left_col: Cell<usize>,
    /// The expanded view of the selected cell's value.
    pub show_cell: bool,
    pub input: Option<TableInput>,
    /// What's typed in each column's filter prompt (kept so reopening it
    /// shows the current filter).
    pub filter_texts: HashMap<usize, String>,
    pub search_text: String,
    /// A one-off message shown in place of the key hint until the next key
    /// (`copied`, `sorting waits for the import`, ...).
    pub notice: Option<String>,
    /// Rows the body showed last frame, for paging and fetch windows.
    pub body_height: Cell<u16>,
    pub geometry: RefCell<TableGeometry>,
}

impl TableMode {
    pub fn new(node_id: String, title: String) -> Self {
        Self {
            node_id,
            title,
            cursor_row: 0,
            cursor_col: 0,
            top_row: 0,
            left_col: Cell::new(0),
            show_cell: false,
            input: None,
            filter_texts: HashMap::new(),
            search_text: String::new(),
            notice: None,
            body_height: Cell::new(20),
            geometry: RefCell::new(TableGeometry::default()),
        }
    }

    pub fn page(&self) -> usize {
        usize::from(self.body_height.get()).max(2) - 1
    }
}

/// Draws the whole full-screen table over `area`.
pub fn render_overlay(f: &mut Frame, area: Rect, data: &TableData, tm: &TableMode) {
    f.render_widget(Clear, area);
    let title = format!(" {} ", tm.title);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(ACCENT))
        .title(title)
        .title_bottom(Line::from(Span::styled(
            format!(" {} ", status_text(data)),
            Style::default().fg(Color::DarkGray),
        )));
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.height < 6 || inner.width < 10 {
        return;
    }

    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2), // names + types
            Constraint::Min(1),    // rows
            Constraint::Length(1), // detail / input
            Constraint::Length(1), // hint
        ])
        .split(inner);
    let (head_area, body_area, detail_area, hint_area) =
        (layout[0], layout[1], layout[2], layout[3]);
    tm.body_height.set(body_area.height);

    let dim = Style::default().fg(Color::DarkGray);

    // Failure and "still loading" states replace the grid.
    if let Some(meta) = &data.meta {
        if meta.state == "failed" {
            let message = meta
                .error
                .as_ref()
                .map_or("table preview failed", |e| e.message.as_str());
            f.render_widget(
                Paragraph::new(Span::styled(message.to_string(), Style::default().fg(FAIL)))
                    .wrap(Wrap { trim: true }),
                body_area,
            );
            return;
        }
    } else {
        let message = data
            .meta_error
            .as_deref()
            .map_or("loading…".to_string(), |e| format!("unavailable: {e}"));
        f.render_widget(Paragraph::new(Span::styled(message, dim)), body_area);
        return;
    }

    let columns = data.columns();
    let widths = column_widths(data, true);
    let rows = data.row_count().unwrap_or(0);
    let gutter_w = format_count(rows.max(1)).len().max(1);

    // Keep the cursor's column on screen: slide `left_col` until it fits.
    let total_w = usize::from(inner.width);
    let fits = |left: usize| -> bool {
        let mut used = gutter_w;
        for i in left..=tm.cursor_col.min(columns.len().saturating_sub(1)) {
            used += COL_GAP.chars().count() + widths.get(i).copied().unwrap_or(0);
        }
        used <= total_w || left >= tm.cursor_col
    };
    let mut left = tm.left_col.get().min(columns.len().saturating_sub(1));
    if tm.cursor_col < left {
        left = tm.cursor_col;
    }
    while !fits(left) {
        left += 1;
    }
    tm.left_col.set(left);

    // The drawn columns and where each starts.
    let mut drawn: Vec<(usize, u16, u16)> = Vec::new(); // (column, x, width)
    let mut x = inner.x + gutter_w as u16;
    for i in left..columns.len() {
        let w = widths[i] as u16;
        let start = x + COL_GAP.chars().count() as u16;
        if start >= inner.x + inner.width {
            break;
        }
        let w = w.min(inner.x + inner.width - start);
        drawn.push((i, start, w));
        x = start + w;
    }
    {
        let mut g = tm.geometry.borrow_mut();
        g.header_y = head_area.y;
        g.body = body_area;
        g.columns = drawn.iter().map(|(c, s, w)| (*s, *s + *w, *c)).collect();
    }

    let line_of = |cells: Vec<(String, Style)>, lead: String, lead_style: Style| -> Line<'static> {
        let mut spans = vec![Span::styled(lead, lead_style)];
        for (text, style) in cells {
            spans.push(Span::styled(COL_GAP, dim));
            spans.push(Span::styled(text, style));
        }
        Line::from(spans)
    };

    // Header: names with sort marks, then types.
    let bold = Style::default().add_modifier(Modifier::BOLD);
    let names = drawn
        .iter()
        .map(|(i, _, w)| {
            let c = &columns[*i];
            let mark = data
                .view
                .sort
                .iter()
                .position(|k| k.column == *i)
                .map(|rank| {
                    let arrow = if data.view.sort[rank].desc {
                        '▼'
                    } else {
                        '▲'
                    };
                    if data.view.sort.len() > 1 {
                        format!("{arrow}{}", rank + 1)
                    } else {
                        arrow.to_string()
                    }
                })
                .unwrap_or_default();
            let filtered = data.view.filters.iter().any(|f| f.column == *i);
            let label = format!(
                "{}{}{}",
                c.name,
                if filtered { " ⧩" } else { "" },
                if mark.is_empty() {
                    String::new()
                } else {
                    format!(" {mark}")
                }
            );
            let style = if *i == tm.cursor_col {
                bold.fg(ACCENT)
            } else {
                bold
            };
            (
                pad(&fit(&label, usize::from(*w)), usize::from(*w), false),
                style,
            )
        })
        .collect();
    let types = drawn
        .iter()
        .map(|(i, _, w)| {
            (
                pad(
                    &fit(&columns[*i].col_type.to_lowercase(), usize::from(*w)),
                    usize::from(*w),
                    false,
                ),
                dim,
            )
        })
        .collect();
    f.render_widget(
        Paragraph::new(vec![
            line_of(names, pad("#", gutter_w, true), dim),
            line_of(types, " ".repeat(gutter_w), dim),
        ]),
        head_area,
    );

    // Body.
    let mut body: Vec<Line> = Vec::new();
    for offset in 0..usize::from(body_area.height) {
        let r = tm.top_row + offset;
        if r >= rows {
            break;
        }
        let at_cursor_row = r == tm.cursor_row;
        let row_data = data.row(r);
        let cells = drawn
            .iter()
            .map(|(i, _, w)| {
                let c = &columns[*i];
                let (text, null) = match row_data {
                    Some(row) => cell_text(row.get(c.index)),
                    None => ("·".to_string(), true),
                };
                let mut style = if null { dim } else { Style::default() };
                if at_cursor_row {
                    style = style.bg(Color::Rgb(0x2b, 0x2b, 0x30));
                }
                if at_cursor_row && *i == tm.cursor_col {
                    style = Style::default().add_modifier(Modifier::REVERSED);
                }
                (
                    pad(
                        &fit(&text, usize::from(*w)),
                        usize::from(*w),
                        c.kind == "number",
                    ),
                    style,
                )
            })
            .collect();
        body.push(line_of(
            cells,
            pad(&format_count(r + 1), gutter_w, true),
            dim,
        ));
    }
    if rows == 0 && !data.is_importing() {
        body.push(Line::from(Span::styled(
            if data.view.is_empty() {
                "no rows"
            } else {
                "no rows match"
            },
            dim,
        )));
    }
    f.render_widget(Paragraph::new(body), body_area);

    // The line under the grid: the prompt being typed, an error, or the
    // cursor cell's value.
    let detail = if let Some(input) = &tm.input {
        let (label, text) = match input {
            TableInput::Search(t) => ("search".to_string(), t.as_str()),
            TableInput::Filter { column, text } => {
                let name = columns.get(*column).map_or("", |c| c.name.as_str());
                (format!("filter {name}"), text.as_str())
            }
        };
        Line::from(vec![
            Span::styled(format!("{label}: "), Style::default().fg(ACCENT)),
            Span::raw(text.to_string()),
            Span::styled("█", Style::default().fg(ACCENT)),
        ])
    } else if let Some(e) = &data.view_error {
        Line::from(Span::styled(
            e.replace('\n', " "),
            Style::default().fg(FAIL),
        ))
    } else {
        let value = data
            .row(tm.cursor_row)
            .and_then(|r| columns.get(tm.cursor_col).map(|c| (c, r.get(c.index))));
        match value {
            Some((c, Some(Some(v)))) => Line::from(vec![
                Span::styled(format!("{} · ", c.name), dim),
                Span::raw(sanitize(v)),
            ]),
            Some((c, Some(None))) => Line::from(vec![
                Span::styled(format!("{} · ", c.name), dim),
                Span::styled("NULL", dim),
            ]),
            _ => Line::from(""),
        }
    };
    f.render_widget(Paragraph::new(detail), detail_area);

    if let Some(notice) = &tm.notice {
        f.render_widget(
            Paragraph::new(Span::styled(notice.clone(), Style::default().fg(OK))),
            hint_area,
        );
        draw_cell_popup(f, area, data, tm);
        return;
    }
    let hint = if tm.input.is_some() {
        match &tm.input {
            Some(TableInput::Filter { .. }) => {
                "enter apply · esc cancel · text  >10  =x  !=x  ^start  $end  ~has  null  !null"
            }
            _ => "enter apply · esc cancel",
        }
    } else if data.is_importing() {
        "j/k h/l move · g/G · enter cell · y copy · q close · sort/filter/search wait for the import"
    } else {
        "j/k h/l move · g/G · s sort · / search · f filter · x reset · enter cell · y copy · q close"
    };
    f.render_widget(Paragraph::new(Span::styled(hint, dim)), hint_area);

    draw_cell_popup(f, area, data, tm);
}

fn draw_cell_popup(f: &mut Frame, area: Rect, data: &TableData, tm: &TableMode) {
    let columns = data.columns();
    if tm.show_cell {
        if let (Some(row), Some(col)) = (data.row(tm.cursor_row), columns.get(tm.cursor_col)) {
            let value = match row.get(col.index) {
                Some(Some(v)) => v.clone(),
                _ => "NULL".to_string(),
            };
            let w = (area.width * 4 / 5).max(20).min(area.width);
            let h = (area.height * 3 / 5).max(5).min(area.height);
            let rect = Rect {
                x: area.x + area.width.saturating_sub(w) / 2,
                y: area.y + area.height.saturating_sub(h) / 2,
                width: w,
                height: h,
            };
            f.render_widget(Clear, rect);
            f.render_widget(
                Paragraph::new(value).wrap(Wrap { trim: false }).block(
                    Block::default()
                        .borders(Borders::ALL)
                        .border_style(Style::default().fg(BORDER))
                        .title(format!(
                            " row {} · {} · {} ",
                            format_count(tm.cursor_row + 1),
                            col.name,
                            col.col_type.to_lowercase()
                        ))
                        .title_bottom(Line::from(Span::styled(
                            " esc/enter close ",
                            Style::default().fg(OK),
                        ))),
                ),
                rect,
            );
        }
    }
}

/// Copies `text` to the terminal's clipboard with the OSC 52 escape — the
/// one clipboard route that works over ssh and needs no extra dependency
/// (terminals that don't support it just ignore it).
pub fn copy_to_clipboard(text: &str) -> std::io::Result<()> {
    use base64::Engine;
    use std::io::Write;
    let encoded = base64::engine::general_purpose::STANDARD.encode(text);
    let mut out = std::io::stdout();
    write!(out, "\x1b]52;c;{encoded}\x07")?;
    out.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn col(index: usize, name: &str, kind: &str) -> TableColumnDto {
        TableColumnDto {
            index,
            name: name.into(),
            col_type: if kind == "number" {
                "BIGINT"
            } else {
                "VARCHAR"
            }
            .into(),
            kind: kind.into(),
        }
    }

    fn meta(state: &str, total: Option<u64>, version: &str) -> TableMetaDto {
        TableMetaDto {
            state: state.into(),
            error: None,
            columns: vec![col(0, "id", "number"), col(1, "name", "text")],
            total_rows: total,
            file_size: 2048,
            version: version.into(),
            preview: state == "importing",
        }
    }

    fn page(version: &str, offset: usize, n: usize, matched: Option<u64>) -> TableRowsDto {
        TableRowsDto {
            version: version.into(),
            offset,
            rows: (offset..offset + n)
                .map(|i| vec![Some(i.to_string()), Some(format!("name{i}"))])
                .collect(),
            matched_rows: matched,
        }
    }

    #[test]
    fn filter_expressions() {
        use FilterOp::*;
        let p = |t: &str, k: &str| parse_filter_expression(t, k);
        assert_eq!(p(">= 10", "number"), Some((Ge, Some("10".into()))));
        assert_eq!(p("!=x", "text"), Some((Ne, Some("x".into()))));
        assert_eq!(p("^ab", "text"), Some((StartsWith, Some("ab".into()))));
        assert_eq!(p("$ab", "text"), Some((EndsWith, Some("ab".into()))));
        assert_eq!(p("~ab", "number"), Some((Contains, Some("ab".into()))));
        assert_eq!(p("42", "number"), Some((Eq, Some("42".into()))));
        assert_eq!(p("true", "bool"), Some((Eq, Some("true".into()))));
        assert_eq!(
            p("2024-01", "temporal"),
            Some((Contains, Some("2024-01".into())))
        );
        assert_eq!(p("NULL", "text"), Some((IsNull, None)));
        assert_eq!(p("!null", "number"), Some((NotNull, None)));
        assert_eq!(p("   ", "text"), None);
        assert_eq!(p(">", "number"), None);
    }

    #[test]
    fn filters_follow_column_order_and_skip_blanks() {
        let columns = vec![
            col(0, "id", "number"),
            col(1, "name", "text"),
            col(2, "n", "text"),
        ];
        let texts = HashMap::from([
            (1, "ab".to_string()),
            (0, ">3".to_string()),
            (2, "".to_string()),
        ]);
        let filters = build_filters(&texts, &columns);
        assert_eq!(filters.len(), 2);
        assert_eq!((filters[0].column, filters[0].op), (0, FilterOp::Gt));
        assert_eq!((filters[1].column, filters[1].op), (1, FilterOp::Contains));
    }

    #[test]
    fn sort_cycles_and_shift_adds() {
        let s = toggle_sort(&[], 2, false);
        assert_eq!(
            s,
            vec![SortKey {
                column: 2,
                desc: false
            }]
        );
        let s = toggle_sort(&s, 2, false);
        assert_eq!(
            s,
            vec![SortKey {
                column: 2,
                desc: true
            }]
        );
        assert!(toggle_sort(&s, 2, false).is_empty());
        let s = toggle_sort(
            &[
                SortKey {
                    column: 0,
                    desc: false,
                },
                SortKey {
                    column: 1,
                    desc: true,
                },
            ],
            3,
            false,
        );
        assert_eq!(
            s,
            vec![SortKey {
                column: 3,
                desc: false
            }]
        );
        let s = toggle_sort(
            &[SortKey {
                column: 0,
                desc: false,
            }],
            1,
            true,
        );
        assert_eq!(s.len(), 2);
        let s = toggle_sort(&s, 0, true);
        assert_eq!(
            s[0],
            SortKey {
                column: 0,
                desc: true
            }
        );
        let s = toggle_sort(&s, 0, true);
        assert_eq!(
            s,
            vec![SortKey {
                column: 1,
                desc: false
            }]
        );
    }

    #[test]
    fn blocks_are_requested_once_and_only_block_zero_until_the_count_is_known() {
        let mut d = TableData::default();
        d.apply_meta(meta("ready", Some(1000), "v1"));
        assert_eq!(d.blocks_to_fetch(0, 20, 1), vec![0, 1]);
        assert!(d.blocks_to_fetch(0, 20, 1).is_empty(), "already pending");
        assert_eq!(d.blocks_to_fetch(950, 20, 0), vec![9]);

        // A filtered view has no count until its first answer.
        d.set_view(ViewSpec {
            search: Some("x".into()),
            ..Default::default()
        });
        assert_eq!(d.blocks_to_fetch(500, 20, 1), vec![0]);
    }

    #[test]
    fn rows_for_an_old_generation_or_version_are_dropped() {
        let mut d = TableData::default();
        d.apply_meta(meta("ready", Some(300), "v1"));
        let g = d.generation;
        d.blocks_to_fetch(0, 10, 0);
        assert!(d.apply_rows(g, 0, page("v1", 0, 100, Some(300))));
        assert_eq!(d.row(5).unwrap()[1].as_deref(), Some("name5"));

        d.set_view(ViewSpec {
            sort: vec![SortKey {
                column: 0,
                desc: true,
            }],
            ..Default::default()
        });
        assert!(d.row(5).is_none(), "view change clears the cache");
        assert!(
            !d.apply_rows(g, 0, page("v1", 0, 100, Some(300))),
            "stale generation"
        );
        assert!(
            !d.apply_rows(d.generation, 0, page("v2", 0, 100, Some(300))),
            "another version of the file"
        );
        assert!(d.row(0).is_none());
    }

    #[test]
    fn a_new_version_or_state_resets_the_cache() {
        let mut d = TableData::default();
        d.apply_meta(meta("importing", None, "v1"));
        let g = d.generation;
        d.blocks_to_fetch(0, 10, 0);
        assert!(d.apply_rows(g, 0, page("v1", 0, 50, None)));
        assert_eq!(d.row_count(), Some(50), "while importing: rows seen so far");
        d.apply_meta(meta("ready", Some(5000), "v1"));
        assert!(d.row(0).is_none());
        assert_eq!(d.row_count(), Some(5000));
    }

    #[test]
    fn counts_and_sizes_read_naturally() {
        assert_eq!(format_count(1234567), "1,234,567");
        assert_eq!(format_count(12), "12");
        assert_eq!(format_bytes(2048), "2.0 KiB");
        assert_eq!(format_bytes(100), "100 B");
    }

    #[test]
    fn cells_are_cut_padded_and_sanitized() {
        assert_eq!(fit("abcdef", 4), "abc…");
        assert_eq!(fit("abc", 4), "abc");
        assert_eq!(pad("ab", 4, true), "  ab");
        assert_eq!(sanitize("a\nb\tc\u{7}"), "a⏎b c�");
    }

    fn text(lines: &[Line]) -> Vec<String> {
        lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect()
    }

    #[test]
    fn inline_window_shows_status_header_rows_and_hint() {
        let mut d = TableData::default();
        d.apply_meta(meta("ready", Some(1000), "v1"));
        let g = d.generation;
        d.blocks_to_fetch(0, 10, 0);
        d.apply_rows(g, 0, page("v1", 0, 100, Some(1000)));
        let lines = text(&inline_lines(&d, 80));
        assert_eq!(lines[0], "1,000 rows × 2 cols");
        assert!(lines[1].contains("id") && lines[1].contains("name"));
        assert!(lines[3].contains("name0"));
        assert_eq!(lines.len(), 1 + 2 + INLINE_ROWS + 1);
        assert!(lines.last().unwrap().contains("Enter"));
    }

    #[test]
    fn inline_window_drops_columns_that_do_not_fit_and_says_so() {
        let mut d = TableData::default();
        let mut m = meta("ready", Some(3), "v1");
        m.columns = (0..8)
            .map(|i| col(i, &format!("column_{i}"), "text"))
            .collect();
        d.apply_meta(m);
        let lines = text(&inline_lines(&d, 40));
        assert!(
            lines[1].contains("more cols") || lines[1].contains("cols"),
            "{}",
            lines[1]
        );
        assert!(lines.iter().all(|l| display_width(l) <= 40), "{lines:?}");
    }

    #[test]
    fn inline_window_reports_failures_and_loading() {
        let mut d = TableData::default();
        assert!(text(&inline_lines(&d, 80))[0].contains("loading"));
        let mut m = meta("failed", None, "");
        m.error = Some(crate::worker_client::TableFailureDto {
            kind: "duckdb-missing".into(),
            message: "DuckDB CLI not found.".into(),
        });
        d.apply_meta(m);
        assert_eq!(text(&inline_lines(&d, 80))[0], "DuckDB CLI not found.");
    }
}
