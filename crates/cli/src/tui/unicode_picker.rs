//! A small source-editor palette. Unicode names are cached across openings.
use std::sync::OnceLock;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Modifier, Style},
    text::Line,
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph},
    Frame,
};

struct Symbol {
    character: char,
    name: String,
}

fn visible(c: char) -> bool {
    !c.is_control()
        && !c.is_whitespace()
        && !matches!(c as u32, 0x115f..=0x1160 | 0x2800 | 0x3164 | 0xffa0)
        && Line::raw(c.to_string()).width() > 0
}

fn catalog() -> &'static [Symbol] {
    static SYMBOLS: OnceLock<Vec<Symbol>> = OnceLock::new();
    SYMBOLS.get_or_init(|| {
        (0..=0x10ffff)
            .filter_map(char::from_u32)
            .filter_map(|character| {
                let name = unicode_names2::name(character)?;
                visible(character).then(|| Symbol {
                    character,
                    name: name.to_string(),
                })
            })
            .collect()
    })
}

pub struct UnicodePicker {
    pub query: String,
    results: Vec<usize>,
    state: ListState,
}

impl UnicodePicker {
    pub fn new() -> Self {
        let mut picker = Self {
            query: String::new(),
            results: Vec::new(),
            state: ListState::default(),
        };
        picker.refresh();
        picker
    }

    fn refresh(&mut self) {
        let query = self.query.trim().to_ascii_uppercase();
        let symbols = catalog();
        self.results = if query.is_empty() {
            "→←↑↓↔⇒⇐⇔∞±×÷≠≈≤≥√∑∏∈∉⊂⊆∪∩∧∨¬∀∃αβγδεθλμπσφω°•…✓✗★☆"
                .chars()
                .filter_map(|c| symbols.binary_search_by_key(&c, |s| s.character).ok())
                .collect()
        } else if let Some(hex) = query.strip_prefix("U+") {
            u32::from_str_radix(hex, 16)
                .ok()
                .and_then(char::from_u32)
                .and_then(|c| symbols.binary_search_by_key(&c, |s| s.character).ok())
                .into_iter()
                .collect()
        } else {
            let mut matches: Vec<_> = symbols
                .iter()
                .enumerate()
                .filter(|(_, s)| s.name.contains(&query) || s.character.to_string() == self.query)
                .map(|(i, s)| {
                    (
                        u8::from(s.name != query),
                        u8::from(!s.name.starts_with(&query)),
                        i,
                    )
                })
                .collect();
            matches.sort_unstable();
            matches.into_iter().map(|(_, _, i)| i).collect()
        };
        self.state = ListState::default();
        self.state.select((!self.results.is_empty()).then_some(0));
    }

    pub fn chosen(&self) -> Option<char> {
        self.state
            .selected()
            .and_then(|i| self.results.get(i))
            .map(|&i| catalog()[i].character)
    }

    pub fn on_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Up => self
                .state
                .select(self.state.selected().map(|i| i.saturating_sub(1))),
            KeyCode::Down => self.state.select(
                self.state
                    .selected()
                    .map(|i| (i + 1).min(self.results.len().saturating_sub(1))),
            ),
            KeyCode::Backspace => {
                self.query.pop();
                self.refresh();
            }
            KeyCode::Char(c)
                if !key.modifiers.intersects(
                    KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER,
                ) =>
            {
                self.query.push(c);
                self.refresh();
            }
            _ => {}
        }
    }

    pub fn render(&mut self, frame: &mut Frame, area: Rect) {
        let width = area.width.min(72);
        let height = area.height.min(18);
        let rect = Rect::new(
            area.x + (area.width - width) / 2,
            area.y + (area.height - height) / 2,
            width,
            height,
        );
        frame.render_widget(Clear, rect);
        let block = Block::default()
            .borders(Borders::ALL)
            .title(" Unicode · name or U+XXXX ");
        let inner = block.inner(rect);
        frame.render_widget(block, rect);
        let rows = Layout::vertical([
            Constraint::Length(1),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .split(inner);
        // Keep the end of a long query visible without moving the editor's cursor.
        let prompt = format!("Search: {}▏", self.query);
        let scroll = Line::raw(prompt.as_str())
            .width()
            .saturating_sub(rows[0].width as usize);
        frame.render_widget(
            Paragraph::new(prompt).scroll((0, scroll.min(u16::MAX as usize) as u16)),
            rows[0],
        );
        if self.results.is_empty() {
            frame.render_widget(Paragraph::new("No matching symbols"), rows[1]);
        } else {
            let visible_rows = usize::from(rows[1].height).max(1);
            let selected = self.state.selected().unwrap_or(0);
            let mut offset = self.state.offset();
            if selected < offset {
                offset = selected;
            }
            if selected >= offset + visible_rows {
                offset = selected + 1 - visible_rows;
            }
            *self.state.offset_mut() = offset;
            let mut visible_state = ListState::default();
            visible_state.select(Some(selected - offset));
            let items: Vec<_> = self
                .results
                .iter()
                .skip(offset)
                .take(visible_rows)
                .map(|&i| {
                    let s = &catalog()[i];
                    ListItem::new(format!(
                        "{}  U+{:04X}  {}",
                        s.character, s.character as u32, s.name
                    ))
                })
                .collect();
            frame.render_stateful_widget(
                List::new(items).highlight_style(Style::default().add_modifier(Modifier::REVERSED)),
                rows[1],
                &mut visible_state,
            );
        }
        frame.render_widget(
            Paragraph::new("↑/↓ choose · Enter insert · Esc cancel"),
            rows[2],
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn search(p: &mut UnicodePicker, query: &str) {
        p.query = query.into();
        p.refresh();
    }

    #[test]
    fn searches_names_codes_and_filters_invisible_characters() {
        let mut p = UnicodePicker::new();
        assert_eq!(p.chosen(), Some('→'));
        search(&mut p, "infinity");
        assert_eq!(p.chosen(), Some('∞'));
        search(&mut p, "arrow");
        assert!(p.results.iter().any(|&i| catalog()[i].character == '→'));
        search(&mut p, "u+1F680");
        assert_eq!(p.chosen(), Some('🚀'));
        for query in [
            "U+001B",
            "U+200B",
            "U+D800",
            "U+110000",
            "U+",
            "no such symbol",
        ] {
            search(&mut p, query);
            assert_eq!(p.chosen(), None, "{query}");
        }
        search(&mut p, "U+221E");
        p.on_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        assert_eq!(p.query, "U+221");
    }

    #[test]
    fn renders_scrolled_selection_and_small_terminals() {
        let mut p = UnicodePicker::new();
        for _ in 0..30 {
            p.on_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        }
        for (w, h) in [(72, 18), (12, 4), (1, 1)] {
            let mut terminal =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(w, h)).unwrap();
            terminal.draw(|f| p.render(f, f.area())).unwrap();
        }
    }
}
