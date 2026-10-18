//! Canvas find: source occurrences use the same title/body order as webui.
//! Byte ranges always refer to the original UTF-8, even when lowercasing
//! expands a character (for example İ), so styled spans can be split safely.
use std::{collections::HashSet, ops::Range};

#[derive(Default)]
pub(super) struct Search {
    pub open: bool,
    pub query: String,
    pub cursor: usize,
    pub index: usize,
    pub expanded_before: HashSet<String>,
    pub revealed: HashSet<String>,
    pub reveal_pending: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct Match {
    pub node_id: String,
    pub occurrence: usize,
}

impl Search {
    pub fn previous_char(&self) -> usize {
        self.query[..self.cursor]
            .char_indices()
            .next_back()
            .map_or(0, |(i, _)| i)
    }

    pub fn next_char(&self) -> usize {
        self.query[self.cursor..]
            .chars()
            .next()
            .map_or(self.cursor, |c| self.cursor + c.len_utf8())
    }

    pub fn matches(&self, canvas: &meshfox_core::Canvas) -> Vec<Match> {
        let query = self.query.trim().to_lowercase();
        if query.is_empty() {
            return Vec::new();
        }
        canvas
            .nodes
            .iter()
            .flat_map(|node| {
                let count = node.title.to_lowercase().matches(query.as_str()).count()
                    + node.text.to_lowercase().matches(query.as_str()).count();
                (0..count).map(|occurrence| Match {
                    node_id: node.id.clone(),
                    occurrence,
                })
            })
            .collect()
    }
}

pub(super) fn ranges(text: &str, query: &str) -> Vec<Range<usize>> {
    let query = query.to_lowercase();
    if query.is_empty() {
        return Vec::new();
    }
    let lower = text.to_lowercase();
    let mut source = Vec::new();
    for (start, ch) in text.char_indices() {
        let end = start + ch.len_utf8();
        for c in ch.to_lowercase() {
            source.extend(std::iter::repeat_n(start..end, c.len_utf8()));
        }
    }
    lower
        .match_indices(&query)
        .map(|(start, _)| source[start].start..source[start + query.len() - 1].end)
        .collect()
}

/// Add backgrounds across syntax/style boundaries without changing text or
/// display width. The return value is the occurrence's cell range for layout.
pub(super) fn highlight(
    line: &mut ratatui::text::Line<'static>,
    query: &str,
    next: &mut usize,
    current: Option<usize>,
) -> Option<(u16, u16)> {
    use ratatui::{
        style::{Color, Modifier},
        text::Span,
    };
    let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
    let matches = ranges(&text, query);
    let mut target = None;
    let mut offset = 0;
    let mut spans = Vec::new();
    for span in &line.spans {
        let end = offset + span.content.len();
        let mut cuts = vec![offset, end];
        for range in &matches {
            if range.start > offset && range.start < end {
                cuts.push(range.start);
            }
            if range.end > offset && range.end < end {
                cuts.push(range.end);
            }
        }
        cuts.sort_unstable();
        cuts.dedup();
        for pair in cuts.windows(2) {
            let mut style = span.style;
            if let Some(index) = matches
                .iter()
                .position(|r| r.start <= pair[0] && pair[0] < r.end)
            {
                style = if current == Some(*next + index) {
                    style
                        .bg(Color::Rgb(0, 215, 255))
                        .fg(Color::Black)
                        .add_modifier(Modifier::BOLD)
                } else {
                    style.bg(Color::Yellow).fg(Color::Black)
                };
            }
            spans.push(Span::styled(
                span.content[pair[0] - offset..pair[1] - offset].to_owned(),
                style,
            ));
        }
        offset = end;
    }
    for (i, range) in matches.iter().enumerate() {
        if current == Some(*next + i) {
            target = Some((
                Span::raw(&text[..range.start]).width() as u16,
                Span::raw(&text[..range.end]).width() as u16,
            ));
        }
    }
    *next += matches.len();
    line.spans = spans;
    target
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unicode_ranges_keep_original_byte_boundaries() {
        assert_eq!(ranges("İПРИВЕТ привет", "привет"), vec![2..14, 15..27]);
        assert_eq!(ranges("İx", "x"), vec![2..3]);
        assert_eq!(ranges("aaaa", "aa"), vec![0..2, 2..4]);
        assert_eq!(ranges("ΟΣ", "ος"), vec![0..4]);
    }
    #[test]
    fn highlights_cross_span_boundaries_and_keep_styles() {
        use ratatui::{
            style::{Color, Modifier, Style},
            text::{Line, Span},
        };
        let mut line = Line::from(vec![
            Span::styled("ab", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw("CD abcd"),
        ]);
        let mut next = 0;
        assert_eq!(highlight(&mut line, "bc", &mut next, Some(1)), Some((6, 8)));
        assert_eq!(next, 2);
        assert_eq!(line.to_string(), "abCD abcd");
        assert!(line.spans[0].style.add_modifier.contains(Modifier::BOLD));
        assert!(line
            .spans
            .iter()
            .any(|s| s.style.bg == Some(Color::Rgb(0, 215, 255))
                && s.style.add_modifier.contains(Modifier::BOLD)));
    }
}
