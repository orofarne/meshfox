//! Pandoc-shaped inline spans with a deliberately bounded color vocabulary.
use pulldown_cmark::{Event, Parser};
use std::ops::Range;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Colors {
    pub color: Option<(u8, u8, u8)>,
    pub background: Option<(u8, u8, u8)>,
}

impl Colors {
    pub fn style(self) -> String {
        let mut out = String::new();
        for (key, value) in [("color", self.color), ("background-color", self.background)] {
            if let Some((r, g, b)) = value {
                out.push_str(&format!("{key}:#{r:02x}{g:02x}{b:02x};"));
            }
        }
        out
    }
}

/// No arbitrary CSS, duplicate keys, or partially valid attribute lists.
pub fn parse(input: &str) -> Option<(Colors, usize)> {
    let tail = input.strip_prefix('{')?;
    let end = tail.find('}')?;
    let mut colors = Colors::default();
    for pair in tail[..end].split_whitespace() {
        let (key, value) = pair.split_once('=')?;
        let value = if value.starts_with('"') || value.starts_with('\'') {
            let quote = value.chars().next()?;
            value.strip_prefix(quote)?.strip_suffix(quote)?
        } else {
            value
        };
        let crate::image_attrs::Background::Rgb(r, g, b) =
            crate::image_attrs::Background::parse(value)?
        else {
            return None;
        };
        let slot = match key {
            "color" => &mut colors.color,
            "background-color" => &mut colors.background,
            _ => return None,
        };
        if slot.replace((r, g, b)).is_some() {
            return None;
        }
    }
    (colors != Colors::default()).then_some((colors, end + 2))
}

/// Generated spans stay distinct from authored raw HTML.
pub enum SpanEvent<'a> {
    Markdown(Event<'a>),
    Start(Colors),
    End,
}
impl<'a> SpanEvent<'a> {
    pub fn into_html(self) -> Event<'a> {
        match self {
            Self::Markdown(event) => event,
            Self::Start(colors) => {
                Event::InlineHtml(format!("<span style=\"{}\">", colors.style()).into())
            }
            Self::End => Event::InlineHtml("</span>".into()),
        }
    }
}

/// Work on parser-confirmed literal text only: code, HTML, link/image syntax,
/// escaped brackets and entities cannot accidentally become delimiters. Source
/// offsets survive the transformation for TUI code-block click targets.
pub fn events<'a>(
    source: &'a str,
    input: Vec<(Event<'a>, Range<usize>)>,
) -> Vec<(SpanEvent<'a>, Range<usize>)> {
    let mut stack = Vec::new();
    let mut depth = 0usize;
    let mut image_depth = 0usize;
    let mut in_code = false;
    let mut covered_until = 0usize;
    let mut literal = vec![false; source.len()];
    for (event, range) in &input {
        if matches!(event, Event::Text(_)) {
            literal[range.clone()].fill(true);
        }
    }
    let mut edits: Vec<(Range<usize>, Option<Colors>)> = Vec::new();
    for (event, range) in &input {
        match event {
            Event::Start(tag) => {
                use pulldown_cmark::Tag;
                if matches!(tag, Tag::CodeBlock(_)) {
                    in_code = true;
                }
                if matches!(tag, Tag::Image { .. }) {
                    image_depth += 1;
                }
                if matches!(
                    tag,
                    Tag::Emphasis
                        | Tag::Strong
                        | Tag::Strikethrough
                        | Tag::Link { .. }
                        | Tag::Image { .. }
                ) {
                    depth += 1;
                } else {
                    stack.clear();
                    depth = 0;
                }
            }
            Event::End(tag) => {
                use pulldown_cmark::TagEnd;
                if matches!(tag, TagEnd::CodeBlock) {
                    in_code = false;
                }
                if matches!(tag, TagEnd::Image) {
                    image_depth = image_depth.saturating_sub(1);
                }
                if matches!(
                    tag,
                    TagEnd::Emphasis
                        | TagEnd::Strong
                        | TagEnd::Strikethrough
                        | TagEnd::Link
                        | TagEnd::Image
                ) {
                    depth = depth.saturating_sub(1);
                } else {
                    stack.clear();
                    depth = 0;
                }
            }
            Event::Text(text)
                if !in_code
                    && image_depth == 0
                    && source.get(range.clone()) == Some(text.as_ref()) =>
            {
                for (offset, ch) in text.char_indices() {
                    let pos = range.start + offset;
                    if pos < covered_until || !matches!(ch, '[' | ']') {
                        continue;
                    }
                    if source[..pos]
                        .bytes()
                        .rev()
                        .take_while(|b| *b == b'\\')
                        .count()
                        % 2
                        != 0
                    {
                        continue;
                    }
                    if ch == '[' {
                        stack.push((pos, depth));
                    }
                    if ch == ']' {
                        if let Some((start, start_depth)) = stack.pop() {
                            if start_depth != depth {
                                continue;
                            }
                            if let Some((colors, consumed)) = parse(&source[pos + 1..]) {
                                let end = pos + 1 + consumed;
                                if !literal[pos..end].iter().all(|v| *v) {
                                    continue;
                                }
                                edits.push((start..start + 1, Some(colors)));
                                edits.push((pos..end, None));
                                covered_until = end;
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
    edits.sort_by_key(|(r, _)| r.start);
    let mut out = Vec::new();
    for (event, range) in input {
        if !matches!(event, Event::Text(_)) {
            out.push((SpanEvent::Markdown(event), range));
            continue;
        }
        let relevant: Vec<_> = edits
            .iter()
            .filter(|(r, _)| r.start < range.end && r.end > range.start)
            .collect();
        if relevant.is_empty() {
            out.push((SpanEvent::Markdown(event), range));
            continue;
        }
        let mut cursor = range.start;
        for (cut, html) in relevant {
            let before = cut.start.min(range.end).max(cursor);
            if before > cursor {
                push_text(&mut out, &source[cursor..before], cursor..before);
            }
            if cut.start >= range.start {
                out.push((html.map_or(SpanEvent::End, SpanEvent::Start), cut.clone()));
            }
            cursor = cut.end.min(range.end).max(cursor);
        }
        if cursor < range.end {
            push_text(&mut out, &source[cursor..range.end], cursor..range.end);
        }
    }
    out
}

fn push_text<'a>(out: &mut Vec<(SpanEvent<'a>, Range<usize>)>, text: &str, range: Range<usize>) {
    // Decode CommonMark escapes/entities when an edit splits a text event.
    let padded = format!("x{text}x");
    let decoded: String = Parser::new(&padded)
        .filter_map(|e| match e {
            Event::Text(t) => Some(t.to_string()),
            _ => None,
        })
        .collect();
    let decoded = &decoded[1..decoded.len() - 1];
    if !decoded.is_empty() {
        out.push((
            SpanEvent::Markdown(Event::Text(decoded.to_owned().into())),
            range,
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pulldown_cmark::{html, Options};
    fn render(s: &str) -> String {
        let mut out = String::new();
        html::push_html(
            &mut out,
            events(
                s,
                Parser::new_ext(s, Options::empty())
                    .into_offset_iter()
                    .collect(),
            )
            .into_iter()
            .map(|(e, _)| e.into_html()),
        );
        out
    }
    #[test]
    fn colors_nested_formatting_and_literals() {
        let s = render("[outer **bold** [inner]{color='#abc'} tail]{background-color=\"#fff3b0\"}");
        assert!(s.contains("<span style=\"background-color:#fff3b0;\">outer <strong>bold</strong> <span style=\"color:#aabbcc;\">inner</span> tail</span>"), "{s}");
        for s in [
            "`[code]{color=#fff}`",
            "```\n[code]{color=#fff}\n```",
            "    [code]{color=#fff}",
            "![alt [text]{color=#fff}](image.png)",
            "[cross\n\nparagraph]{color=#fff}",
            "[cross *emphasis]{color=#fff} end*",
            r"\[escaped]{color=#fff}",
            "[bad]{color=red}",
            "[bad]{color=#fff color=#000}",
            "[link](https://example.com){color=#fff}",
        ] {
            assert!(!render(s).contains("<span"), "{s}");
        }
    }
}
