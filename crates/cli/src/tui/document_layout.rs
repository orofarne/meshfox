//! Container-dependent document layout. Markdown keeps tables semantic until
//! this stage; measurement and hit testing share ratatui's wrapping rules.
//!
//! Parsing owns content and logical click coordinates, never terminal geometry.
//! A context receives the actual inner container size; tables and file previews
//! resolve to styled logical lines here. TextLayout measures those lines with
//! the same Paragraph wrapping used to paint and project click regions.
//! VisibleRows applies scrolling to the measured rows for both text and images.
//! Output can materialize physical text rows for its bottom-relative scroll.
//! Resizing only reruns this stage and image protocol preparation from cached
//! bytes; it does not rebuild the parsed document.
use super::markdown::{self, Segment};
use pulldown_cmark::Alignment;
use ratatui::{
    layout::{Rect, Size},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Paragraph, Wrap},
};

#[derive(Clone, Copy)]
pub(super) struct LayoutContext {
    pub size: Size,
}

pub(super) struct TextLayout {
    pub lines: Vec<Line<'static>>,
    line_row_starts: Vec<u16>,
    rows: usize,
    width: u16,
    wrap: bool,
    button: bool,
}
impl TextLayout {
    pub fn rows(&self) -> usize {
        self.rows
    }
    pub fn paragraph(&self) -> Paragraph<'static> {
        let paragraph = Paragraph::new(Text::from(self.lines.clone()));
        if self.wrap {
            paragraph.wrap(Wrap { trim: false })
        } else {
            paragraph
        }
    }
    pub fn click_rects(&self, line: usize, start: u16, end: u16) -> Vec<Rect> {
        if self.button && end == u16::MAX {
            return self
                .lines
                .iter()
                .enumerate()
                .map(|(y, line)| Rect::new(0, y as u16, line.width() as u16, 1))
                .collect();
        }
        let Some(content) = self.lines.get(line) else {
            return Vec::new();
        };
        wrapped_click_rects(content, self.width, start, end)
            .into_iter()
            .map(|mut r| {
                r.y = r.y.saturating_add(self.line_row_starts[line]);
                r
            })
            .collect()
    }
    /// Physical rows for consumers such as Output that scroll a flat line list.
    pub fn into_rows(self) -> Vec<Line<'static>> {
        use ratatui::{buffer::Buffer, widgets::Widget};
        let height = self.rows.min(u16::MAX as usize) as u16;
        let area = Rect::new(0, 0, self.width, height);
        let mut buffer = Buffer::empty(area);
        self.paragraph().render(area, &mut buffer);
        (0..height)
            .map(|y| {
                let mut spans = Vec::new();
                let mut x = 0;
                while x < self.width {
                    let cell = &buffer[(x, y)];
                    let symbol = cell.symbol();
                    spans.push(Span::styled(symbol.to_owned(), cell.style()));
                    x += Span::raw(symbol).width().max(1) as u16;
                }
                Line::from(spans)
            })
            .collect()
    }
}
impl LayoutContext {
    pub fn new(size: Size) -> Self {
        Self { size }
    }

    /// Reserve space already occupied by the parent container (title or frame).
    pub fn inset(self, columns: u16, rows: u16) -> Self {
        Self::new(Size::new(
            self.size.width.saturating_sub(columns).max(1),
            self.size.height.saturating_sub(rows).max(1),
        ))
    }

    pub fn text(self, segment: &Segment) -> Option<TextLayout> {
        // Grids are already laid out; outer paragraph wrapping would split
        // columns again. File previews intentionally clip excess columns.
        let wrap = matches!(segment, Segment::Text(_));
        let lines = match segment {
            Segment::Text(lines) => lines.clone(),
            Segment::Button { caption, selected } => {
                button_lines(caption, *selected, self.size.width)
            }
            Segment::Table { table, prefix } => {
                let inset: usize = prefix.iter().map(Span::width).sum();
                let prefix = if inset < usize::from(self.size.width) {
                    prefix.as_slice()
                } else {
                    &[]
                };
                let inset: usize = prefix.iter().map(Span::width).sum();
                let mut lines =
                    render_table(table, usize::from(self.size.width).saturating_sub(inset));
                for line in &mut lines {
                    line.spans.splice(0..0, prefix.iter().cloned());
                }
                lines
            }
            Segment::TablePreview(data) => {
                super::table::inline_lines(data, usize::from(self.size.width))
            }
            Segment::Image { .. } => return None,
        };
        let mut rows = 0usize;
        let line_row_starts = lines
            .iter()
            .map(|line| {
                let start = rows.min(u16::MAX as usize) as u16;
                let paragraph = Paragraph::new(Text::from(vec![line.clone()]));
                rows += if wrap {
                    paragraph
                        .wrap(Wrap { trim: false })
                        .line_count(self.size.width)
                } else {
                    usize::from(self.size.width > 0)
                };
                start
            })
            .collect();
        Some(TextLayout {
            lines,
            line_row_starts,
            rows,
            width: self.size.width,
            wrap,
            button: matches!(segment, Segment::Button { .. }),
        })
    }

    pub fn image_budget(self, width_percent: Option<u32>, height_percent: Option<u32>) -> Size {
        let viewport = self.size;
        let width = viewport.width.max(1);
        let height = viewport.height.max(1);
        if width_percent.is_none() && height_percent.is_none() {
            return ratatui::layout::Size::new(width.min(56), height.min(24));
        }
        let scale = |available: u16, percent: Option<u32>| {
            (u64::from(available) * u64::from(percent.unwrap_or(100)) / 100)
                .clamp(1, u64::from(available)) as u16
        };
        ratatui::layout::Size::new(scale(width, width_percent), scale(height, height_percent))
    }
}

fn button_lines(caption: &str, selected: bool, available: u16) -> Vec<Line<'static>> {
    let border = Style::default().fg(if selected {
        super::theme::ACCENT
    } else {
        super::theme::BORDER
    });
    let text = if selected {
        Style::default()
            .bg(super::theme::ACCENT)
            .fg(Color::Black)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
            .fg(super::theme::ACCENT)
            .add_modifier(Modifier::BOLD)
    };
    let caption = format!(
        "▶ {}",
        caption.split_whitespace().collect::<Vec<_>>().join(" ")
    );
    if available < 5 {
        return vec![Line::styled(caption, text)];
    }
    let width = (Span::raw(&caption).width() + 4).min(usize::from(available));
    // Use the same grapheme-aware wrapping as table cells so a wide glyph
    // never spills into the frame's right-hand border.
    let content = wrap_table_cell(&[Span::raw(caption)], width - 4);
    let mut lines = vec![Line::styled(format!("╭{}╮", "─".repeat(width - 2)), border)];
    for line in content {
        let label = line.to_string();
        let padding = width.saturating_sub(4 + Span::raw(&label).width());
        lines.push(Line::from(vec![
            Span::styled("│", border),
            Span::styled(format!(" {label}{} ", " ".repeat(padding)), text),
            Span::styled("│", border),
        ]));
    }
    lines.push(Line::styled(format!("╰{}╯", "─".repeat(width - 2)), border));
    lines
}

/// Let ratatui wrap the same text with a private marker style, so mouse
/// regions follow word wrapping, wide glyphs and partially scrolled lines.
pub(super) fn wrapped_click_rects(line: &Line<'_>, width: u16, start: u16, end: u16) -> Vec<Rect> {
    use ratatui::buffer::Buffer;
    use ratatui::widgets::Widget;
    if width == 0 {
        return Vec::new();
    }
    let marker = Color::Rgb(1, 2, 3);
    let mut col = 0usize;
    let spans: Vec<_> = line
        .styled_graphemes(Style::default())
        .map(|g| {
            let w = Span::raw(g.symbol).width();
            let marked = col < usize::from(end) && col + w > usize::from(start);
            col += w;
            Span::styled(
                g.symbol.to_owned(),
                if marked { g.style.bg(marker) } else { g.style },
            )
        })
        .collect();
    let paragraph = Paragraph::new(Line::from(spans)).wrap(Wrap { trim: false });
    let height = paragraph.line_count(width).min(u16::MAX as usize) as u16;
    let mut buffer = Buffer::empty(Rect::new(0, 0, width, height));
    paragraph.render(buffer.area, &mut buffer);
    let mut rects = Vec::new();
    for y in 0..height {
        let mut x = 0;
        while x < width {
            if buffer[(x, y)].bg != marker {
                x += 1;
                continue;
            }
            let begin = x;
            while x < width && buffer[(x, y)].bg == marker {
                x += Span::raw(buffer[(x, y)].symbol()).width().max(1) as u16;
            }
            rects.push(Rect::new(begin, y, x.min(width) - begin, 1));
        }
    }
    rects
}

/// Wrap each cell independently, preserving grapheme widths and inline styles.
fn wrap_table_cell(cell: &[Span<'static>], width: usize) -> Vec<Line<'static>> {
    let mut lines = vec![Line::default()];
    let mut used = 0;
    for span in cell {
        if used + span.width() <= width {
            lines.last_mut().unwrap().spans.push(span.clone());
            used += span.width();
            continue;
        }
        for word in span.content.split_inclusive(char::is_whitespace) {
            let word_span = Span::styled(word, span.style);
            if used > 0 && used + word_span.width() > width {
                lines.push(Line::default());
                used = 0;
            }
            for g in word_span.styled_graphemes(Style::default()) {
                let w = Span::raw(g.symbol).width();
                if used + w > width && used > 0 {
                    lines.push(Line::default());
                    used = 0;
                }
                if used == 0 && g.symbol == " " {
                    continue;
                }
                if w <= width {
                    lines
                        .last_mut()
                        .unwrap()
                        .spans
                        .push(Span::styled(g.symbol.to_owned(), g.style));
                    used += w;
                }
            }
        }
    }
    lines
}

pub(super) fn render_table(t: &markdown::MarkdownTable, available: usize) -> Vec<Line<'static>> {
    let cols = t.rows.iter().map(|r| r.len()).max().unwrap_or(0);
    if cols == 0 {
        return Vec::new();
    }
    if available < cols * 4 - 3 {
        return t
            .rows
            .iter()
            .flat_map(|row| {
                row.iter()
                    .flat_map(|cell| wrap_table_cell(cell, available.max(1)))
                    .chain(std::iter::once(Line::default()))
                    .collect::<Vec<_>>()
            })
            .collect();
    }
    let mut widths = vec![1usize; cols];
    for row in &t.rows {
        for (i, cell) in row.iter().enumerate() {
            widths[i] = widths[i].max(cell.iter().map(Span::width).sum());
        }
    }
    // Keep compact columns intact; take space from the widest columns first.
    let budget = available.saturating_sub((cols - 1) * 3).max(cols);
    let mut total: usize = widths.iter().sum();
    while total > budget {
        let i = (0..cols).max_by_key(|&i| widths[i]).unwrap();
        if widths[i] == 1 {
            break;
        }
        widths[i] -= 1;
        total -= 1;
    }
    let mut lines = Vec::new();
    for (ri, row) in t.rows.iter().enumerate() {
        let cells: Vec<_> = widths
            .iter()
            .enumerate()
            .map(|(i, &w)| wrap_table_cell(row.get(i).map(Vec::as_slice).unwrap_or(&[]), w))
            .collect();
        for y in 0..cells.iter().map(Vec::len).max().unwrap_or(1) {
            let mut spans = Vec::new();
            for (i, &w) in widths.iter().enumerate() {
                if i > 0 {
                    spans.push(Span::styled(" │ ", Style::default().fg(Color::DarkGray)));
                }
                let cell = cells[i].get(y).cloned().unwrap_or_default();
                let gap = w.saturating_sub(cell.width());
                let left = match t.alignments.get(i) {
                    Some(Alignment::Right) => gap,
                    Some(Alignment::Center) => gap / 2,
                    _ => 0,
                };
                spans.push(Span::raw(" ".repeat(left)));
                spans.extend(cell.spans.into_iter().map(|s| {
                    if ri == 0 {
                        s.patch_style(Style::default().add_modifier(Modifier::BOLD))
                    } else {
                        s
                    }
                }));
                spans.push(Span::raw(" ".repeat(gap - left)));
            }
            lines.push(Line::from(spans));
        }
        if ri == 0 {
            let rule = widths
                .iter()
                .map(|&w| "─".repeat(w))
                .collect::<Vec<_>>()
                .join("─┼─");
            lines.push(Line::from(Span::styled(
                rule,
                Style::default().fg(Color::DarkGray),
            )));
        }
    }
    lines
}

/// How much of a `Segment::Text`'s already-wrapped `total` rows (see
/// `render_document` — always `Paragraph::line_count(width)`, the *actual*
/// wrapped row count, never the raw `lines.len()`) to draw this pass:
/// `row_offset` for `Paragraph::scroll` (applied post-wrap, so it can land
/// partway through one long logical line, not just before/after one) and
/// `height` for the rect, already clamped to whatever vertical room is
/// left in the pane.
pub(super) struct VisibleRows {
    pub row_offset: u16,
    pub height: u16,
}

/// `None` means this whole segment is above the current scroll position —
/// the caller should skip it, carrying `skip - total` forward as the
/// remaining skip for the next segment (mirrors `render_document`'s own
/// loop, which is why this doesn't return the reduced skip itself: the
/// caller already has `skip` in scope to subtract from directly).
pub(super) fn wrapped_text_layout(total: u16, skip: u16, available: u16) -> Option<VisibleRows> {
    if skip >= total {
        return None;
    }
    Some(VisibleRows {
        row_offset: skip,
        height: (total - skip).min(available),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{collections::HashMap, path::Path};

    #[test]
    fn button_frame_reflows_and_its_entire_surface_remains_clickable() {
        for width in [12, 24, 80] {
            let context = LayoutContext::new(Size::new(width, 20));
            let segment = Segment::Button {
                caption: "🔨 Install from source 界".into(),
                selected: true,
            };
            let layout = context.text(&segment).unwrap();
            let frame_width = layout.lines[0].width();
            assert!(frame_width <= usize::from(width));
            assert!(
                layout.lines.iter().all(|line| line.width() == frame_width),
                "width {width}: {:?}",
                layout
                    .lines
                    .iter()
                    .map(|l| (l.width(), l.to_string()))
                    .collect::<Vec<_>>()
            );
            assert!(layout
                .lines
                .iter()
                .flat_map(|line| &line.spans)
                .any(|span| span.style.bg == Some(super::super::theme::ACCENT)));
            let rects = layout.click_rects(0, 0, u16::MAX);
            assert_eq!(rects.len(), layout.rows());
            assert!(rects.iter().all(|r| usize::from(r.width) == frame_width));
            assert_eq!(layout.into_rows().len(), rects.len());
        }
    }

    fn parse(md: &str) -> Vec<Segment> {
        markdown::render(
            md,
            Path::new("/nonexistent"),
            &markdown::Highlighter::new(),
            "n",
            &[],
            &HashMap::new(),
            None,
            &HashMap::new(),
        )
        .0
    }

    #[test]
    fn tables_fit_the_pane_and_keep_column_boundaries_on_continuation_rows() {
        let t = markdown::MarkdownTable {
            alignments: vec![Alignment::None; 3],
            rows: vec![
                vec![
                    vec![Span::raw("Режим")],
                    vec![Span::raw("命令")],
                    vec![Span::raw("Ток")],
                ],
                vec![
                    vec![Span::raw("Single shot + power-down")],
                    vec![Span::styled("0x36E0", Style::default().fg(Color::Yellow))],
                    vec![Span::raw("длинное описание режима питания")],
                ],
            ],
        };
        for width in [1, 8, 24, 40, 80] {
            let lines = render_table(&t, width);
            assert!(
                lines.iter().all(|l| l.width() <= width),
                "width {width}: {lines:?}"
            );
            if width >= 24 {
                let positions: Vec<_> = lines
                    .iter()
                    .filter(|l| l.to_string().contains('│'))
                    .map(|l| {
                        let mut x = 0;
                        let mut positions = Vec::new();
                        for span in &l.spans {
                            if span.content == " │ " {
                                positions.push(x);
                            }
                            x += span.width();
                        }
                        positions
                    })
                    .collect();
                assert!(positions.iter().all(|p| p == &positions[0]));
            }
        }
    }

    #[test]
    fn one_parsed_table_reflows_for_each_container_including_output_frames() {
        let mut segments = parse("| Режим | Команда | Ток |\n|---|---:|---|\n| Single shot + power-down | `0x36E0` | длинное описание питания датчика |\n\nAFTER\n");
        let table = segments
            .iter_mut()
            .find(|s| matches!(s, Segment::Table { .. }))
            .unwrap();
        let wide = LayoutContext::new(Size::new(100, 40)).text(table).unwrap();
        let narrow = LayoutContext::new(Size::new(32, 40)).text(table).unwrap();
        assert!(narrow.rows() > wide.rows());
        for width in [1, 8, 32, 100] {
            let layout = LayoutContext::new(Size::new(width, 40))
                .text(table)
                .unwrap();
            assert!(layout.lines.iter().all(|l| l.width() <= usize::from(width)));
            assert_eq!(layout.rows(), layout.lines.len());
        }
        if let Segment::Table { prefix, .. } = table {
            prefix.push(Span::styled("│ ", Style::default().fg(Color::Magenta)));
        }
        let framed = LayoutContext::new(Size::new(32, 40)).text(table).unwrap();
        assert!(framed
            .lines
            .iter()
            .all(|l| l.width() == 32 && l.spans[0].content == "│ "));
        let physical = framed.into_rows();
        assert!(physical
            .iter()
            .all(|l| l.spans[0].style.fg == Some(Color::Magenta)));
    }

    #[test]
    fn narrow_file_preview_uses_grid_rows_without_outer_word_wrapping() {
        let segment = Segment::TablePreview(Box::default());
        let layout = LayoutContext::new(Size::new(5, 20)).text(&segment).unwrap();
        assert_eq!(layout.rows(), 1);
        let rows = layout.into_rows();
        assert_eq!(rows.len(), 1);
        assert!(rows[0].width() <= 5);
    }

    #[test]
    fn text_measurement_pixels_and_clicks_use_the_same_wrapping() {
        let line = Line::from(vec![
            Span::raw("long introduction wraps here "),
            Span::styled("構築 RUN", Style::default().fg(Color::Yellow)),
        ]);
        let start = Span::raw("long introduction wraps here ").width() as u16;
        let layout = LayoutContext::new(Size::new(16, 40))
            .text(&Segment::Text(vec![line]))
            .unwrap();
        let clicks = layout.click_rects(0, start, start + 8);
        assert!(!clicks.is_empty());
        assert!(clicks.iter().all(|r| r.y > 0 && r.x + r.width <= 16));
        let count = layout.rows();
        let rows = layout.into_rows();
        assert_eq!(count, rows.len());
        for rect in clicks {
            assert!(rows[rect.y as usize]
                .spans
                .iter()
                .any(|s| s.style.fg == Some(Color::Yellow)));
        }
    }
}
