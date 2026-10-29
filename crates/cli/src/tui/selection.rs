//! Read-mode selection: physical rows retain their logical source, so wrapping
//! does not introduce newlines into copied prose/code. A selection owns a frozen
//! snapshot while live output continues to accumulate behind it.
use super::app::Focus;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Paragraph, Widget, Wrap},
    Frame,
};
use std::{ops::Range, sync::Arc};

#[derive(Clone, Debug)]
pub struct Row {
    pub line: Line<'static>,
    source: Arc<str>,
    start: usize,
    end: usize,
    allowed: Range<usize>,
    glyphs: Vec<(u16, Range<usize>)>,
}

fn copy_range(s: &str) -> Range<usize> {
    let trimmed = s.trim_start();
    // Viewer chrome, never part of the document's text.
    if trimmed.starts_with(['┌', '└', '├', '┬', '┴', '┼', '╭', '╰'])
        || trimmed.starts_with("›─")
        || (!trimmed.is_empty()
            && trimmed
                .chars()
                .all(|c| "─━═┐┘┤┬┴┼│┌└├┓┗┏┛╭╮╰╯ ".contains(c)))
    {
        return 0..0;
    }
    let prefix = if s.starts_with("│ ") {
        "│ ".len()
    } else {
        0
    };
    prefix..s.len()
}

/// Materialize precisely the same Paragraph wrapping as document_layout.
pub fn rows(lines: &[Line<'static>], width: u16, wrap: bool, clean_chrome: bool) -> Vec<Row> {
    let mut result = Vec::new();
    let width = width.max(1);
    for line in lines {
        let source: Arc<str> = line.to_string().into();
        let allowed = if clean_chrome {
            copy_range(&source)
        } else {
            0..source.len()
        };
        // Output does not wrap: a single long line must not force every
        // short line to allocate a buffer as wide as the longest one.
        let width = if wrap {
            width
        } else {
            line.width().min(usize::from(width)).max(1) as u16
        };
        let paragraph = Paragraph::new(line.clone());
        let paragraph = if wrap {
            paragraph.wrap(Wrap { trim: false })
        } else {
            paragraph
        };
        let height = paragraph.line_count(width).max(1).min(u16::MAX as usize) as u16;
        let area = Rect::new(0, 0, width, height);
        let mut buffer = Buffer::empty(area);
        paragraph.render(area, &mut buffer);
        let mut consumed = 0;
        for y in 0..height {
            let mut spans = Vec::new();
            let mut glyphs = Vec::new();
            let mut visible = String::new();
            let mut x = 0;
            while x < width {
                let cell = &buffer[(x, y)];
                let symbol = cell.symbol();
                let start = visible.len();
                visible.push_str(symbol);
                glyphs.push((x, start..visible.len()));
                spans.push(Span::styled(symbol.to_owned(), cell.style()));
                x += Span::raw(symbol).width().max(1) as u16;
            }
            let visible = visible.trim_end_matches(' ');
            let painted = glyphs
                .iter()
                .take_while(|(_, r)| r.start < visible.len())
                .count();
            spans.truncate(painted);
            // Wrap may discard boundary whitespace; keep it in the logical
            // source and restore it when joining two physical rows.
            let mut mapped = Vec::new();
            for (x, r) in glyphs {
                if r.end > visible.len() {
                    break;
                }
                let symbol = &visible[r];
                if let Some(offset) = source[consumed..].find(symbol) {
                    let start = consumed + offset;
                    consumed = start + symbol.len();
                    mapped.push((x, start..consumed));
                }
            }
            let start = mapped.first().map(|(_, r)| r.start).unwrap_or(consumed);
            let end = mapped.last().map(|(_, r)| r.end).unwrap_or(consumed);
            let glyphs = mapped;
            result.push(Row {
                line: Line::from(spans),
                source: Arc::clone(&source),
                start,
                end,
                allowed: allowed.clone(),
                glyphs,
            });
            consumed = end;
        }
    }
    result
}

impl Row {
    pub fn blank() -> Self {
        let mut row = rows(&[Line::raw(" ")], 1, false, false).remove(0);
        row.allowed = 0..0;
        row
    }
    fn last_col(&self) -> u16 {
        self.glyphs.last().map(|(x, _)| *x).unwrap_or(0)
    }
    fn clamp_col(&self, col: u16) -> u16 {
        self.glyphs
            .iter()
            .rev()
            .find(|(x, _)| *x <= col)
            .map(|(x, _)| *x)
            .unwrap_or(0)
    }
    fn byte_range(&self, start: u16, end: u16) -> Range<usize> {
        let first = self
            .glyphs
            .iter()
            .find(|(x, _)| *x >= start)
            .map(|(_, r)| r.start)
            .unwrap_or(self.end);
        let last = self
            .glyphs
            .iter()
            .rev()
            .find(|(x, _)| *x <= end)
            .map(|(_, r)| r.end)
            .unwrap_or(self.start);
        first.max(self.allowed.start)..last.min(self.allowed.end)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Point {
    pub row: usize,
    pub col: u16,
}

pub struct Selection {
    pub pane: Focus,
    pub rows: Vec<Row>,
    pub area: Rect,
    pub top: usize,
    pub left: u16,
    pub anchor: Point,
    pub selecting: bool,
    pub cursor: Point,
    pub mouse_down: Option<MouseEvent>,
    pub dragged: bool,
    pub notice: Option<String>,
    pub copy_flash_until: Option<std::time::Instant>,
}

impl Selection {
    pub fn new(pane: Focus, rows: Vec<Row>, area: Rect, top: usize, left: u16) -> Option<Self> {
        if rows.is_empty() || area.is_empty() {
            return None;
        }
        let top = top.min(rows.len() - 1);
        let row = (top..rows.len())
            .find(|&i| rows[i].allowed.end > rows[i].allowed.start)
            .unwrap_or(top);
        let col = rows[row]
            .glyphs
            .iter()
            .find(|(_, r)| r.start >= rows[row].allowed.start)
            .map(|(x, _)| *x)
            .unwrap_or(0);
        let point = Point { row, col };
        let mut s = Self {
            pane,
            rows,
            area,
            top,
            left,
            anchor: point,
            selecting: false,
            cursor: point,
            mouse_down: None,
            dragged: false,
            notice: None,
            copy_flash_until: None,
        };
        s.reveal();
        Some(s)
    }
    pub fn point_at(&self, column: u16, row: u16) -> Point {
        let row = (self.top
            + usize::from(
                row.saturating_sub(self.area.y)
                    .min(self.area.height.saturating_sub(1)),
            ))
        .min(self.rows.len() - 1);
        Point {
            row,
            col: self.rows[row]
                .clamp_col(column.saturating_sub(self.area.x).saturating_add(self.left)),
        }
    }
    pub fn drag(&mut self, column: u16, row: u16) {
        self.selecting = true;
        if row < self.area.y {
            self.top = self.top.saturating_sub(1);
        }
        if row >= self.area.bottom() {
            self.top = (self.top + 1).min(
                self.rows
                    .len()
                    .saturating_sub(usize::from(self.area.height)),
            );
        }
        self.cursor = self.point_at(column, row);
        self.dragged = true;
    }
    pub fn select_word(&mut self) {
        self.selecting = true;
        let row = &self.rows[self.cursor.row];
        let Some((_, glyph)) = row.glyphs.iter().find(|(x, _)| *x == self.cursor.col) else {
            return;
        };
        let is_word = |c: char| c.is_alphanumeric() || c == '_';
        let word = row.source[glyph.clone()]
            .chars()
            .next()
            .is_some_and(is_word);
        let mut start = glyph.start;
        let mut end = glyph.end;
        if word {
            for (i, c) in row.source[..start].char_indices().rev() {
                if !is_word(c) {
                    break;
                }
                start = i;
            }
            for c in row.source[end..].chars() {
                if !is_word(c) {
                    break;
                }
                end += c.len_utf8();
            }
        }
        self.anchor.col = row
            .glyphs
            .iter()
            .find(|(_, r)| r.start >= start)
            .map(|(x, _)| *x)
            .unwrap_or(self.cursor.col);
        self.cursor.col = row
            .glyphs
            .iter()
            .rev()
            .find(|(_, r)| r.end <= end)
            .map(|(x, _)| *x)
            .unwrap_or(self.cursor.col);
        self.dragged = true;
    }
    pub fn on_key(&mut self, key: KeyEvent) -> bool {
        if key.code == KeyCode::Char(' ') {
            self.selecting = !self.selecting;
            self.anchor = self.cursor;
            self.notice = None;
            self.copy_flash_until = None;
            return true;
        }
        if !matches!(
            key.code,
            KeyCode::Left
                | KeyCode::Right
                | KeyCode::Up
                | KeyCode::Down
                | KeyCode::Home
                | KeyCode::End
                | KeyCode::PageUp
                | KeyCode::PageDown
                | KeyCode::Char('h' | 'j' | 'k' | 'l')
        ) {
            return false;
        }
        if key.modifiers.contains(KeyModifiers::SHIFT) && !self.selecting {
            self.anchor = self.cursor;
            self.selecting = true;
        }
        self.notice = None;
        self.copy_flash_until = None;
        match key.code {
            KeyCode::Left | KeyCode::Char('h') => {
                if let Some((col, _)) = self.rows[self.cursor.row]
                    .glyphs
                    .iter()
                    .rev()
                    .find(|(x, _)| *x < self.cursor.col)
                {
                    self.cursor.col = *col;
                } else if self.cursor.row > 0 {
                    self.cursor.row -= 1;
                    self.cursor.col = self.rows[self.cursor.row].last_col();
                }
            }
            KeyCode::Right | KeyCode::Char('l') => {
                if let Some((col, _)) = self.rows[self.cursor.row]
                    .glyphs
                    .iter()
                    .find(|(x, _)| *x > self.cursor.col)
                {
                    self.cursor.col = *col;
                } else if self.cursor.row + 1 < self.rows.len() {
                    self.cursor.row += 1;
                    self.cursor.col = 0;
                }
            }
            KeyCode::Up | KeyCode::Char('k') => self.cursor.row = self.cursor.row.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => {
                self.cursor.row = (self.cursor.row + 1).min(self.rows.len() - 1)
            }
            KeyCode::PageUp => {
                self.cursor.row = self
                    .cursor
                    .row
                    .saturating_sub(usize::from(self.area.height))
            }
            KeyCode::PageDown => {
                self.cursor.row =
                    (self.cursor.row + usize::from(self.area.height)).min(self.rows.len() - 1)
            }
            KeyCode::Home => self.cursor.col = 0,
            KeyCode::End => self.cursor.col = self.rows[self.cursor.row].last_col(),
            _ => return false,
        }
        self.cursor.col = self.rows[self.cursor.row].clamp_col(self.cursor.col);
        if !self.selecting {
            self.anchor = self.cursor;
        }
        self.reveal();
        true
    }
    fn reveal(&mut self) {
        if self.cursor.row < self.top {
            self.top = self.cursor.row;
        }
        if self.cursor.row >= self.top + usize::from(self.area.height) {
            self.top = self.cursor.row + 1 - usize::from(self.area.height);
        }
        if self.cursor.col < self.left {
            self.left = self.cursor.col;
        }
        if self.cursor.col >= self.left.saturating_add(self.area.width) {
            self.left = self.cursor.col + 1 - self.area.width;
        }
    }
    pub fn text(&self) -> String {
        if !self.selecting {
            return String::new();
        }
        let (a, b) = if self.anchor <= self.cursor {
            (self.anchor, self.cursor)
        } else {
            (self.cursor, self.anchor)
        };
        let mut text = String::new();
        let mut previous: Option<(&Arc<str>, usize)> = None;
        for i in a.row..=b.row {
            let row = &self.rows[i];
            if row.allowed.is_empty() && !row.source.is_empty() {
                continue;
            }
            let r = row.byte_range(
                if i == a.row { a.col } else { 0 },
                if i == b.row { b.col } else { u16::MAX },
            );
            if r.start > r.end {
                continue;
            }
            if let Some((source, end)) = previous {
                if Arc::ptr_eq(source, &row.source) {
                    if end <= r.start {
                        text.push_str(&row.source[end..r.start]);
                    }
                } else {
                    text.push('\n');
                }
            }
            text.push_str(&row.source[r.clone()]);
            previous = Some((&row.source, r.end));
        }
        text
    }
    pub fn flash_copy(&mut self, now: std::time::Instant) {
        self.copy_flash_until = Some(now + std::time::Duration::from_millis(250));
    }
    pub fn render(&self, f: &mut Frame) {
        self.render_at(f, std::time::Instant::now());
    }
    fn render_at(&self, f: &mut Frame, now: std::time::Instant) {
        let flashing = self.copy_flash_until.is_some_and(|until| now < until);
        let (bg, fg) = if flashing {
            (super::theme::accent(), super::theme::on_accent())
        } else {
            (super::theme::selection_bg(), super::theme::selection_fg())
        };
        f.render_widget(ratatui::widgets::Clear, self.area);
        let (a, b) = if self.anchor <= self.cursor {
            (self.anchor, self.cursor)
        } else {
            (self.cursor, self.anchor)
        };
        for (screen_row, row) in self
            .rows
            .iter()
            .skip(self.top)
            .take(usize::from(self.area.height))
            .enumerate()
        {
            let y = self.area.y + screen_row as u16;
            f.render_widget(
                Paragraph::new(row.line.clone()).scroll((0, self.left)),
                Rect::new(self.area.x, y, self.area.width, 1),
            );
            let i = self.top + screen_row;
            if !self.selecting || i < a.row || i > b.row {
                continue;
            }
            for (x, r) in &row.glyphs {
                if (i == a.row && *x < a.col)
                    || (i == b.row && *x > b.col)
                    || r.end <= row.allowed.start
                    || r.start >= row.allowed.end
                {
                    continue;
                }
                let width = Span::raw(&row.source[r.clone()]).width().max(1) as u16;
                for col in *x..x.saturating_add(width) {
                    if col >= self.left && col - self.left < self.area.width {
                        let cell = &mut f.buffer_mut()[(self.area.x + col - self.left, y)];
                        cell.set_style(
                            Style::default()
                                .bg(bg)
                                .fg(fg)
                                .remove_modifier(Modifier::REVERSED | Modifier::DIM),
                        );
                    }
                }
            }
        }
        if let Some(position) = self.cursor_position() {
            f.buffer_mut()[position]
                .set_style(Style::default().add_modifier(Modifier::UNDERLINED | Modifier::BOLD));
        }
    }
    /// The terminal cursor follows grapheme cells and the pane's scroll.
    pub fn cursor_position(&self) -> Option<(u16, u16)> {
        let y = self.cursor.row.checked_sub(self.top)?;
        let x = self.cursor.col.checked_sub(self.left)?;
        (y < usize::from(self.area.height) && x < self.area.width)
            .then_some((self.area.x + x, self.area.y + y as u16))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn all(lines: &[&str], width: u16, clean: bool) -> Selection {
        let lines = lines
            .iter()
            .map(|s| Line::raw(s.to_string()))
            .collect::<Vec<_>>();
        let rows = rows(&lines, width, true, clean);
        let mut s = Selection::new(Focus::Document, rows, Rect::new(2, 2, width, 4), 0, 0).unwrap();
        s.selecting = true;
        s.anchor = Point { row: 0, col: 0 };
        s.cursor = Point {
            row: s.rows.len() - 1,
            col: s.rows.last().unwrap().last_col(),
        };
        s
    }
    #[test]
    fn cursor_moves_before_anchoring_a_middle_fragment() {
        let lines = [Line::raw("alpha beta gamma")];
        let mut s = Selection::new(
            Focus::Document,
            rows(&lines, 20, false, false),
            Rect::new(2, 2, 20, 4),
            0,
            0,
        )
        .unwrap();
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        for _ in 0..6 {
            s.on_key(key(KeyCode::Right));
        }
        assert_eq!(s.cursor_position(), Some((8, 2)));
        assert_eq!(s.text(), "");
        s.on_key(key(KeyCode::Char(' ')));
        for _ in 0..3 {
            s.on_key(key(KeyCode::Right));
        }
        assert_eq!(s.text(), "beta");
        s.on_key(key(KeyCode::Char(' ')));
        assert_eq!(s.text(), "");
        s.on_key(KeyEvent::new(KeyCode::Right, KeyModifiers::SHIFT));
        assert_eq!(s.text(), "a ");
    }
    #[test]
    fn copy_flash_uses_theme_accent_expires_and_restarts() {
        let mut s = all(&["hello"], 10, false);
        let now = std::time::Instant::now();
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(20, 10)).unwrap();
        s.flash_copy(now);
        terminal.draw(|f| s.render_at(f, now)).unwrap();
        let cell = &terminal.backend().buffer()[(2, 2)];
        assert_eq!(cell.bg, super::super::theme::accent());
        assert_eq!(cell.fg, super::super::theme::on_accent());
        let later = now + std::time::Duration::from_millis(250);
        terminal.draw(|f| s.render_at(f, later)).unwrap();
        assert_eq!(
            terminal.backend().buffer()[(2, 2)].bg,
            super::super::theme::selection_bg()
        );
        assert_eq!(s.text(), "hello");
        s.flash_copy(later);
        assert_eq!(
            s.copy_flash_until,
            Some(later + std::time::Duration::from_millis(250))
        );
    }
    #[test]
    fn a_long_output_line_does_not_pad_every_short_line() {
        let lines = [Line::raw("x".repeat(2000)), Line::raw("short")];
        let r = rows(&lines, 2000, false, false);
        assert_eq!(r[1].line.width(), 5);
        assert_eq!(r[1].glyphs.len(), 5);
    }
    #[test]
    fn images_and_buttons_do_not_add_copied_blank_lines() {
        let mut s = all(&["before", "after"], 20, true);
        s.rows.insert(1, Row::blank());
        s.cursor.row = 2;
        assert_eq!(s.text(), "before\nafter");
    }
    #[test]
    fn control_characters_do_not_break_source_mapping() {
        let s = all(&["a\tb界", "x\u{1b}y"], 20, false);
        // Source controls which the renderer hides still belong to the
        // copied logical text; source byte offsets remain UTF-8 boundaries.
        assert_eq!(s.text(), "a\tb界\nx\u{1b}y");
    }
    #[test]
    fn soft_wrap_preserves_whitespace_and_hard_newlines() {
        let source = "alpha beta  gamma delta epsilon";
        let s = all(&[source, "", "next line"], 12, false);
        assert_eq!(s.text(), format!("{source}\n\nnext line"));
    }
    #[test]
    fn code_gutters_and_frames_are_excluded_in_both_directions() {
        let mut s = all(
            &[
                "┌─ bash · test ──",
                "│ echo hello",
                "│ echo world",
                "└────────────────",
            ],
            24,
            true,
        );
        assert_eq!(s.text(), "echo hello\necho world");
        std::mem::swap(&mut s.anchor, &mut s.cursor);
        assert_eq!(s.text(), "echo hello\necho world");
    }
    #[test]
    fn wide_and_combining_graphemes_are_atomic() {
        let mut s = all(&["a界e\u{301}🙂z"], 20, false);
        s.anchor = Point { row: 0, col: 1 };
        s.cursor = Point { row: 0, col: 4 };
        assert_eq!(s.text(), "界e\u{301}🙂");
        s.on_key(KeyEvent::new(
            KeyCode::Left,
            crossterm::event::KeyModifiers::NONE,
        ));
        assert_eq!(s.text(), "界e\u{301}");
    }
    #[test]
    fn keyboard_reveals_rows_and_horizontal_columns() {
        let mut s = all(&["one", "two", "three", "four", "five", "six"], 5, false);
        s.cursor = Point { row: 0, col: 0 };
        s.anchor = s.cursor;
        s.on_key(KeyEvent::new(
            KeyCode::PageDown,
            crossterm::event::KeyModifiers::NONE,
        ));
        assert_eq!(s.cursor.row, 4);
        assert_eq!(s.top, 1);
        s.on_key(KeyEvent::new(
            KeyCode::End,
            crossterm::event::KeyModifiers::NONE,
        ));
        assert_eq!(s.text(), "one\ntwo\nthree\nfour\nfive");
    }
    #[test]
    fn selection_uses_themes_selection_pair() {
        let s = all(&["hello"], 10, false);
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(20, 10)).unwrap();
        terminal.draw(|f| s.render(f)).unwrap();
        let cell = &terminal.backend().buffer()[(2, 2)];
        assert_eq!(cell.bg, super::super::theme::selection_bg());
        assert_eq!(cell.fg, super::super::theme::selection_fg());
    }
}
