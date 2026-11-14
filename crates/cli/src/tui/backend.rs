//! Keep Unicode width disagreements from shifting subsequent terminal writes.
use ratatui::{
    backend::{Backend, ClearType, CrosstermBackend, WindowSize},
    buffer::Cell,
    layout::{Position, Size},
};
use std::io::{self, Write};

pub(super) struct PositionedBackend<W: Write>(CrosstermBackend<W>);
impl<W: Write> PositionedBackend<W> {
    pub(super) fn new(writer: W) -> Self {
        Self(CrosstermBackend::new(writer))
    }
}
impl<W: Write> Write for PositionedBackend<W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.write(bytes)
    }
    fn flush(&mut self) -> io::Result<()> {
        Write::flush(&mut self.0)
    }
}
impl<W: Write> Backend for PositionedBackend<W> {
    type Error = io::Error;
    fn draw<'a, I>(&mut self, content: I) -> io::Result<()>
    where
        I: Iterator<Item = (u16, u16, &'a Cell)>,
    {
        let mut content = content.peekable();
        while content.peek().is_some() {
            // Crossterm starts each draw with an absolute cursor move. End
            // the run after Unicode: the next write must not infer cursor
            // position from a width the terminal may disagree with. ASCII
            // runs retain the usual batching. Box Drawing is NOT exempt: terminals
            // with ambiguous-width=double render it two columns wide. Do not alter
            // glyphs or image protocol payloads, and do not query the terminal
            // cursor.
            //
            // `scroll_region_up/down` are feature-gated in ratatui
            // (`scrolling-regions`, not enabled here); delegate them if that
            // feature is ever turned on.
            let mut ended = false;
            self.0.draw(std::iter::from_fn(|| {
                if ended {
                    return None;
                }
                let item = content.next()?;
                ended = !item.2.symbol().is_ascii();
                Some(item)
            }))?;
        }
        Ok(())
    }
    fn append_lines(&mut self, n: u16) -> io::Result<()> {
        self.0.append_lines(n)
    }
    fn hide_cursor(&mut self) -> io::Result<()> {
        self.0.hide_cursor()
    }
    fn show_cursor(&mut self) -> io::Result<()> {
        self.0.show_cursor()
    }
    fn get_cursor_position(&mut self) -> io::Result<Position> {
        self.0.get_cursor_position()
    }
    fn set_cursor_position<P: Into<Position>>(&mut self, p: P) -> io::Result<()> {
        self.0.set_cursor_position(p)
    }
    fn clear(&mut self) -> io::Result<()> {
        self.0.clear()
    }
    fn clear_region(&mut self, t: ClearType) -> io::Result<()> {
        self.0.clear_region(t)
    }
    fn size(&self) -> io::Result<Size> {
        self.0.size()
    }
    fn window_size(&mut self) -> io::Result<WindowSize> {
        self.0.window_size()
    }
    fn flush(&mut self) -> io::Result<()> {
        Backend::flush(&mut self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::Color;

    #[test]
    fn width_disagreements_do_not_shift_text_or_borders() {
        // Simulate terminals rendering a glyph narrower OR wider than the
        // layout expects. Feed the actual backend output to a VT parser.
        for glyph in ["⬛︎", "☀︎", "♥️", "👩‍💻", "界"] {
            for physical in ["X", "XX"] {
                let mut bytes = Vec::new();
                let mut backend = PositionedBackend::new(&mut bytes);
                let mut square = Cell::default();
                square.set_symbol(glyph).set_fg(Color::Rgb(255, 0, 0));
                let mut text = Cell::default();
                text.set_symbol("f");
                let mut border = Cell::default();
                border.set_symbol("│");
                backend
                    .draw([(1, 0, &square), (2, 0, &text), (3, 0, &border)].into_iter())
                    .unwrap();
                let raw = String::from_utf8(bytes).unwrap();
                assert!(raw.contains(glyph), "source glyph must survive");
                let mut parser = vt100::Parser::new(2, 10, 0);
                parser.process(raw.replace(glyph, physical).as_bytes());
                assert_eq!(
                    parser.screen().cell(0, 2).unwrap().contents(),
                    "f",
                    "{glyph} as {physical}"
                );
                assert_eq!(
                    parser.screen().cell(0, 3).unwrap().contents(),
                    "│",
                    "{glyph} as {physical}"
                );
                // Erase the old content as a later diff would when changing
                // nodes. No shifted f or broken border may survive.
                let mut erase = Vec::new();
                let mut backend = PositionedBackend::new(&mut erase);
                let blank = Cell::default();
                backend
                    .draw([(1, 0, &blank), (2, 0, &blank)].into_iter())
                    .unwrap();
                parser.process(&erase);
                assert_eq!(parser.screen().contents().trim(), "│");
            }
        }
    }

    #[test]
    fn width_disagreements_mid_run_and_consecutive_do_not_shift_text() {
        for physical in ["X", "XX"] {
            let mut bytes = Vec::new();
            let mut backend = PositionedBackend::new(&mut bytes);
            let cell = |s: &str| {
                let mut c = Cell::default();
                c.set_symbol(s);
                c
            };
            let (a, b, g1, g2, c, d) = (
                cell("a"),
                cell("b"),
                cell("☀︎"),
                cell("♥️"),
                cell("c"),
                cell("│"),
            );
            // a b ☀ _ ♥ _ c │ — each glyph occupies two layout columns.
            backend
                .draw(
                    [
                        (0, 0, &a),
                        (1, 0, &b),
                        (2, 0, &g1),
                        (4, 0, &g2),
                        (6, 0, &c),
                        (7, 0, &d),
                    ]
                    .into_iter(),
                )
                .unwrap();
            let raw = String::from_utf8(bytes)
                .unwrap()
                .replace("☀︎", physical)
                .replace("♥️", physical);
            let mut parser = vt100::Parser::new(2, 12, 0);
            parser.process(raw.as_bytes());
            let screen = parser.screen();
            assert_eq!(screen.cell(0, 6).unwrap().contents(), "c", "as {physical}");
            assert_eq!(screen.cell(0, 7).unwrap().contents(), "│", "as {physical}");
        }
    }

    #[test]
    fn box_drawing_rendered_double_width_does_not_shift_text() {
        // Ambiguous-width=double terminals draw `│` two columns wide.
        for physical in ["│", "││"] {
            let mut bytes = Vec::new();
            let mut backend = PositionedBackend::new(&mut bytes);
            let cell = |s: &str| {
                let mut c = Cell::default();
                c.set_symbol(s);
                c
            };
            let (border, text, right) = (cell("│"), cell("f"), cell("┐"));
            backend
                .draw([(0, 0, &border), (1, 0, &text), (2, 0, &right)].into_iter())
                .unwrap();
            let raw = String::from_utf8(bytes).unwrap().replace('│', physical);
            let mut parser = vt100::Parser::new(2, 10, 0);
            parser.process(raw.as_bytes());
            let screen = parser.screen();
            assert_eq!(screen.cell(0, 1).unwrap().contents(), "f", "as {physical}");
            assert_eq!(screen.cell(0, 2).unwrap().contents(), "┐", "as {physical}");
        }
    }

    #[test]
    fn ascii_runs_keep_cursor_move_batching() {
        let mut bytes = Vec::new();
        let mut backend = PositionedBackend::new(&mut bytes);
        let mut cell = Cell::default();
        cell.set_symbol("a");
        backend.draw((0..80).map(|x| (x, 0, &cell))).unwrap();
        assert_eq!(String::from_utf8(bytes).unwrap().matches('H').count(), 1);
    }
}
