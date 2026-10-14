//! ANSI SGR output rendering. Reparse retained output on redraw so styles
//! survive line boundaries; terminal controls are consumed, never emitted.
use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};

#[derive(Default)]
pub(super) struct Ansi {
    style: Style,
    pending: String,
}
impl Ansi {
    pub fn line(&mut self, text: &str, base: Style) -> Line<'static> {
        let input = std::mem::take(&mut self.pending) + text;
        let mut chars = input.chars().peekable();
        let mut spans = Vec::new();
        let mut buf = String::new();
        while let Some(c) = chars.next() {
            if c == '\x1b' {
                if !buf.is_empty() {
                    spans.push(Span::styled(
                        std::mem::take(&mut buf),
                        base.patch(self.style),
                    ));
                }
                let mut escape = String::from("\x1b");
                let Some(kind) = chars.next() else {
                    self.pending = escape;
                    break;
                };
                escape.push(kind);
                if kind == '[' {
                    let mut complete = false;
                    for c in chars.by_ref() {
                        escape.push(c);
                        if ('@'..='~').contains(&c) {
                            if c == 'm' {
                                self.sgr(&escape[2..escape.len() - 1]);
                            }
                            complete = true;
                            break;
                        }
                    }
                    if !complete {
                        self.pending = escape;
                        break;
                    }
                } else if matches!(kind, ']' | 'P' | 'X' | '^' | '_') {
                    let mut complete = false;
                    while let Some(c) = chars.next() {
                        escape.push(c);
                        if c == '\x07' && kind == ']' {
                            complete = true;
                            break;
                        }
                        if c == '\x1b' && chars.peek() == Some(&'\\') {
                            escape.push(chars.next().unwrap());
                            complete = true;
                            break;
                        }
                    }
                    if !complete {
                        self.pending = escape;
                        break;
                    }
                } else if (' '..='/').contains(&kind) {
                    // Escape sequences with intermediate bytes (e.g. charset selection).
                    while let Some(c) = chars.next() {
                        escape.push(c);
                        if ('0'..='~').contains(&c) {
                            break;
                        }
                        if chars.peek().is_none() {
                            self.pending = escape.clone();
                        }
                    }
                }
            } else if c == '\t' {
                buf.push_str("    ");
            } else if !c.is_control() {
                buf.push(c);
            }
        }
        if !buf.is_empty() {
            spans.push(Span::styled(buf, base.patch(self.style)));
        }
        Line::from(spans)
    }
    fn sgr(&mut self, params: &str) {
        let Ok(values) = params
            .replace("::", ":")
            .replace(':', ";")
            .split(';')
            .map(|s| {
                if s.is_empty() {
                    Ok(0)
                } else {
                    s.parse::<u16>()
                }
            })
            .collect::<Result<Vec<_>, _>>()
        else {
            return;
        };
        let mut i = 0;
        while i < values.len() {
            let n = values[i];
            i += 1;
            let modifier = match n {
                1 => Some(Modifier::BOLD),
                2 => Some(Modifier::DIM),
                3 => Some(Modifier::ITALIC),
                4 => Some(Modifier::UNDERLINED),
                5 => Some(Modifier::SLOW_BLINK),
                6 => Some(Modifier::RAPID_BLINK),
                7 => Some(Modifier::REVERSED),
                8 => Some(Modifier::HIDDEN),
                9 => Some(Modifier::CROSSED_OUT),
                _ => None,
            };
            if let Some(m) = modifier {
                self.style = self.style.add_modifier(m);
                continue;
            }
            match n {
                0 => self.style = Style::default(),
                22 => self.style = self.style.remove_modifier(Modifier::BOLD | Modifier::DIM),
                23 => self.style = self.style.remove_modifier(Modifier::ITALIC),
                24 => self.style = self.style.remove_modifier(Modifier::UNDERLINED),
                25 => {
                    self.style = self
                        .style
                        .remove_modifier(Modifier::SLOW_BLINK | Modifier::RAPID_BLINK)
                }
                27 => self.style = self.style.remove_modifier(Modifier::REVERSED),
                28 => self.style = self.style.remove_modifier(Modifier::HIDDEN),
                29 => self.style = self.style.remove_modifier(Modifier::CROSSED_OUT),
                39 => self.style.fg = None,
                49 => self.style.bg = None,
                30..=37 | 90..=97 => {
                    self.style.fg = Some(Color::Indexed(if n < 90 {
                        (n - 30) as u8
                    } else {
                        (n - 90 + 8) as u8
                    }))
                }
                40..=47 | 100..=107 => {
                    self.style.bg = Some(Color::Indexed(if n < 100 {
                        (n - 40) as u8
                    } else {
                        (n - 100 + 8) as u8
                    }))
                }
                38 | 48 => {
                    let color = match values.get(i) {
                        Some(5) if values.get(i + 1).is_some_and(|v| *v <= 255) => {
                            i += 2;
                            Some(Color::Indexed(values[i - 1] as u8))
                        }
                        Some(2)
                            if values
                                .get(i + 1..i + 4)
                                .is_some_and(|v| v.iter().all(|v| *v <= 255)) =>
                        {
                            let c = Color::Rgb(
                                values[i + 1] as u8,
                                values[i + 2] as u8,
                                values[i + 3] as u8,
                            );
                            i += 4;
                            Some(c)
                        }
                        _ => {
                            break;
                        }
                    };
                    if n == 38 {
                        self.style.fg = color;
                    } else {
                        self.style.bg = color;
                    }
                }
                _ => {}
            }
        }
    }
}
pub(super) fn lines(text: &str) -> Vec<Line<'static>> {
    let mut ansi = Ansi::default();
    text.lines()
        .map(|line| ansi.line(line, Style::default()))
        .collect()
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn colors_and_resets_survive_lines() {
        let out = lines("\x1b[1;31mred\nnext\x1b[22;39m normal");
        assert_eq!(out[1].spans[0].style.fg, Some(Color::Indexed(1)));
        assert!(out[1].spans[0].style.add_modifier.contains(Modifier::BOLD));
        assert_eq!(out[1].spans[1].style.fg, None);
        assert!(!out[1].spans[1].style.add_modifier.contains(Modifier::BOLD));
    }
    #[test]
    fn extended_colors_and_split_escape() {
        let mut ansi = Ansi::default();
        assert!(
            ansi.line("\x1b[38;2;12;", Style::default())
                .spans
                .is_empty()
        );
        let line = ansi.line("34;56;48;5;200mцвет", Style::default());
        assert_eq!(line.spans[0].style.fg, Some(Color::Rgb(12, 34, 56)));
        assert_eq!(line.spans[0].style.bg, Some(Color::Indexed(200)));
    }
    #[test]
    fn controls_are_not_rendered_and_default_uses_base() {
        let mut ansi = Ansi::default();
        let line = ansi.line(
            "\x1b]8;;https://example.org\x1b\\link\x1b]8;;\x07\x1b[2J\r\x1b[31mx\x1b[0m!",
            Style::default().fg(Color::Yellow),
        );
        assert_eq!(line.to_string(), "linkx!");
        assert_eq!(line.spans.last().unwrap().style.fg, Some(Color::Yellow));
    }
}
