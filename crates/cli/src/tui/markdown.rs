//! Markdown -> ratatui content, for the TUI's document pane.
//!
//! Deliberately hand-rolled over `pulldown-cmark`'s event stream rather than
//! pulling in a ready-made "markdown to ratatui Text" crate — a `meshfox`
//! node body mixes prose with fenced code (syntax-highlighted via `syntect`)
//! and local images (rendered via `ratatui-image`), and `ratatui::text::Text`
//! alone can't carry the latter: an image is a widget, not styled text. See
//! `Segment` below for the split.

use std::path::{Path, PathBuf};

use pulldown_cmark::{Alignment, CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use syntect::easy::HighlightLines;
use syntect::highlighting::{Theme, ThemeSet};
use syntect::parsing::SyntaxSet;

/// One piece of a rendered node body: either plain styled text (however
/// many lines) or a local image to hand off to `ratatui-image`. Kept as a
/// flat `Vec<Segment>` rather than a tree — a node body is read top to
/// bottom, never nested past what indentation-as-text already conveys.
pub enum Segment {
    Text(Vec<Line<'static>>),
    Image { path: PathBuf, alt: String },
}

/// Loads syntect's bundled (compiled-in, no on-disk assets) syntax/theme
/// sets once and reuses them for every code fence — these sets are a few
/// MB to build and meant to be shared, not rebuilt per fence.
pub struct Highlighter {
    syntax_set: SyntaxSet,
    theme: Theme,
}

impl Highlighter {
    pub fn new() -> Self {
        let syntax_set = SyntaxSet::load_defaults_newlines();
        let theme_set = ThemeSet::load_defaults();
        let theme = theme_set
            .themes
            .get("base16-ocean.dark")
            .cloned()
            .unwrap_or_else(|| {
                theme_set
                    .themes
                    .values()
                    .next()
                    .cloned()
                    .expect("syntect ships at least one theme")
            });
        Highlighter { syntax_set, theme }
    }

    fn highlight(&self, lang: &str, code: &str) -> Vec<Line<'static>> {
        let syntax = self
            .syntax_set
            .find_syntax_by_token(lang)
            .unwrap_or_else(|| self.syntax_set.find_syntax_plain_text());
        self.highlight_with(syntax, code)
    }

    /// Same as `highlight`, but for a whole file (`file` node, `display="code"`
    /// — see SPEC.md) rather than a fenced code block: `lang_hint` is the
    /// node's own explicit `lang=` when it has one, otherwise the syntax is
    /// guessed from the target path's extension, same as the browser UI's
    /// preview does.
    pub fn highlight_file(
        &self,
        lang_hint: Option<&str>,
        path: &std::path::Path,
        code: &str,
    ) -> Vec<Line<'static>> {
        let syntax = lang_hint
            .and_then(|l| self.syntax_set.find_syntax_by_token(l))
            .or_else(|| {
                path.extension()
                    .and_then(|e| e.to_str())
                    .and_then(|e| self.syntax_set.find_syntax_by_extension(e))
            })
            .unwrap_or_else(|| self.syntax_set.find_syntax_plain_text());
        self.highlight_with(syntax, code)
    }

    fn highlight_with(
        &self,
        syntax: &syntect::parsing::SyntaxReference,
        code: &str,
    ) -> Vec<Line<'static>> {
        let mut h = HighlightLines::new(syntax, &self.theme);
        let mut lines = Vec::new();
        for line in syntect::util::LinesWithEndings::from(code) {
            let ranges = h.highlight_line(line, &self.syntax_set).unwrap_or_default();
            let spans: Vec<Span<'static>> = ranges
                .into_iter()
                .map(|(style, text)| Span::styled(text.to_string(), translate_style(style)))
                .collect();
            lines.push(Line::from(spans));
        }
        if lines.is_empty() {
            lines.push(Line::from(""));
        }
        lines
    }
}

/// `syntect::highlighting::Style` -> `ratatui::style::Style`. Small enough
/// to hand-roll rather than pull in a bridging crate — an `a == 0` alpha
/// (syntect's convention for "no color set, inherit the theme's default")
/// maps to `None` so we don't paint over a span with a color the theme
/// never actually chose.
fn translate_style(style: syntect::highlighting::Style) -> Style {
    use syntect::highlighting::FontStyle;

    let color = |c: syntect::highlighting::Color| -> Option<Color> {
        if c.a == 0 {
            None
        } else {
            Some(Color::Rgb(c.r, c.g, c.b))
        }
    };
    let mut out = Style::default();
    if let Some(fg) = color(style.foreground) {
        out = out.fg(fg);
    }
    if let Some(bg) = color(style.background) {
        out = out.bg(bg);
    }
    if style.font_style.contains(FontStyle::BOLD) {
        out = out.add_modifier(Modifier::BOLD);
    }
    if style.font_style.contains(FontStyle::ITALIC) {
        out = out.add_modifier(Modifier::ITALIC);
    }
    if style.font_style.contains(FontStyle::UNDERLINE) {
        out = out.add_modifier(Modifier::UNDERLINED);
    }
    out
}

const HEADING_COLORS: [Color; 6] = [
    Color::LightCyan,
    Color::LightBlue,
    Color::LightGreen,
    Color::LightYellow,
    Color::LightMagenta,
    Color::Gray,
];

fn heading_style(level: HeadingLevel) -> Style {
    let idx = (level as usize).saturating_sub(1).min(5);
    let mut style = Style::default()
        .fg(HEADING_COLORS[idx])
        .add_modifier(Modifier::BOLD);
    if level == HeadingLevel::H1 {
        style = style.add_modifier(Modifier::UNDERLINED);
    }
    style
}

/// Renders `md` (one node's own body text — never the whole document) into
/// a sequence of segments. `base_dir` is the canvas file's own directory,
/// the same boundary local `file`/`link`/image targets are already
/// resolved within elsewhere in meshfox — a relative image path is joined
/// against it; anything that parses as an absolute URL (`http(s)://...`) is
/// shown as a plain link instead of fetched, matching this being a local
/// document viewer, not a browser.
pub fn render(md: &str, base_dir: &Path, hl: &Highlighter) -> Vec<Segment> {
    let mut renderer = Renderer::new(base_dir, hl);
    let options = Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH;
    for event in Parser::new_ext(md, options) {
        renderer.event(event);
    }
    renderer.finish()
}

#[derive(Clone, Copy, PartialEq)]
enum Inline {
    Emphasis,
    Strong,
    Strikethrough,
    Code,
    Link,
}

struct Renderer<'a> {
    base_dir: &'a Path,
    hl: &'a Highlighter,
    segments: Vec<Segment>,
    lines: Vec<Line<'static>>,
    current: Vec<Span<'static>>,
    inline_stack: Vec<Inline>,
    list_stack: Vec<Option<u64>>, // Some(n) = ordered, next number; None = bullet
    quote_depth: usize,
    code_lang: Option<String>,
    code_name: Option<String>,
    code_buf: String,
    table: Option<TableState>,
    pending_heading_style: Option<Style>,
}

struct TableState {
    alignments: Vec<Alignment>,
    rows: Vec<Vec<String>>,
    current_row: Vec<String>,
    current_cell: String,
    in_head: bool,
}

impl<'a> Renderer<'a> {
    fn new(base_dir: &'a Path, hl: &'a Highlighter) -> Self {
        Renderer {
            base_dir,
            hl,
            segments: Vec::new(),
            lines: Vec::new(),
            current: Vec::new(),
            inline_stack: Vec::new(),
            list_stack: Vec::new(),
            quote_depth: 0,
            code_lang: None,
            code_name: None,
            code_buf: String::new(),
            table: None,
            pending_heading_style: None,
        }
    }

    fn indent(&self) -> String {
        "  ".repeat(self.quote_depth + self.list_stack.len())
    }

    fn push_text(&mut self, text: &str, extra: Style) {
        let mut style = Style::default();
        for m in &self.inline_stack {
            style = match m {
                Inline::Emphasis => style.add_modifier(Modifier::ITALIC),
                Inline::Strong => style.add_modifier(Modifier::BOLD),
                Inline::Strikethrough => style.add_modifier(Modifier::CROSSED_OUT),
                Inline::Code => style
                    .bg(Color::Rgb(40, 42, 54))
                    .fg(Color::Rgb(255, 184, 108)),
                Inline::Link => style
                    .fg(Color::LightBlue)
                    .add_modifier(Modifier::UNDERLINED),
            };
        }
        self.current
            .push(Span::styled(text.to_string(), style.patch(extra)));
    }

    fn flush_line(&mut self) {
        if !self.current.is_empty() {
            let mut spans = self.current.drain(..).collect::<Vec<_>>();
            if self.quote_depth > 0 || !self.list_stack.is_empty() {
                spans.insert(0, Span::raw(self.indent()));
            }
            self.lines.push(Line::from(spans));
        }
    }

    fn flush_paragraph(&mut self) {
        self.flush_line();
        if !self.lines.is_empty() {
            let lines = std::mem::take(&mut self.lines);
            self.push_segment(Segment::Text(lines));
        }
    }

    /// Pushes a block-level segment, with a blank line ahead of it whenever
    /// it isn't the very first segment in the document — otherwise adjacent
    /// blocks (a code fence right after a paragraph, two fences back to
    /// back, ...) render with no gap and read as one merged block, since
    /// `render_document` just stacks each segment's lines directly on top
    /// of the next with no spacing of its own.
    fn push_segment(&mut self, seg: Segment) {
        if !self.segments.is_empty() {
            self.segments.push(Segment::Text(vec![Line::from("")]));
        }
        self.segments.push(seg);
    }

    fn finish(mut self) -> Vec<Segment> {
        self.flush_paragraph();
        self.segments
    }

    fn event(&mut self, ev: Event) {
        match ev {
            Event::Start(tag) => self.start(tag),
            Event::End(tag) => self.end(tag),
            Event::Text(t) => {
                if self.code_lang.is_some() {
                    self.code_buf.push_str(&t);
                } else if let Some(table) = &mut self.table {
                    table.current_cell.push_str(&t);
                } else {
                    self.push_text(&t, Style::default());
                }
            }
            Event::Code(t) => {
                self.inline_stack.push(Inline::Code);
                self.push_text(&t, Style::default());
                self.inline_stack.pop();
            }
            Event::SoftBreak => self.push_text(" ", Style::default()),
            Event::HardBreak => self.flush_line(),
            Event::Rule => {
                self.flush_paragraph();
                self.push_segment(Segment::Text(vec![Line::from(Span::styled(
                    "─".repeat(60),
                    Style::default().fg(Color::DarkGray),
                ))]));
            }
            // meshfox's own bookkeeping (`meshfox:node`/`meshfox:output`/...)
            // lives entirely in HTML comments — invisible in any normal
            // Markdown viewer per SPEC.md, so raw HTML is simply dropped
            // here rather than shown.
            Event::Html(_) | Event::InlineHtml(_) => {}
            _ => {}
        }
    }

    fn start(&mut self, tag: Tag) {
        match tag {
            Tag::Paragraph => {}
            Tag::Heading { level, .. } => {
                self.flush_paragraph();
                self.inline_stack.push(Inline::Strong); // reuse bold path, style below overrides
                self.current.push(Span::styled(
                    format!("{} ", "#".repeat(level as usize)),
                    Style::default().fg(Color::DarkGray),
                ));
                self.inline_stack.pop();
                self.pending_heading_style = Some(heading_style(level));
            }
            Tag::BlockQuote(_) => {
                self.flush_paragraph();
                self.quote_depth += 1;
            }
            Tag::CodeBlock(kind) => {
                self.flush_paragraph();
                let (lang, name) = match kind {
                    // The info string carries meshfox's own attributes past
                    // the language token (`name="..."`, `cache`, ...) — see
                    // `meshfox_core::fence`, which this mirrors just enough
                    // to pull out `name` for the block header below.
                    CodeBlockKind::Fenced(info) => {
                        let mut tokens = meshfox_core::attrs::tokenize(&info).into_iter();
                        let lang = tokens.next().unwrap_or_else(|| "text".to_string());
                        let name = meshfox_core::attrs::attrs_from_tokens(tokens).remove("name");
                        (lang, name)
                    }
                    CodeBlockKind::Indented => ("text".to_string(), None),
                };
                self.code_lang = Some(lang);
                self.code_name = name;
                self.code_buf.clear();
            }
            Tag::List(start) => {
                self.list_stack.push(start);
            }
            Tag::Item => {
                let marker = match self.list_stack.last_mut() {
                    Some(Some(n)) => {
                        let m = format!("{n}. ");
                        *n += 1;
                        m
                    }
                    _ => "• ".to_string(),
                };
                self.current.push(Span::raw(marker));
            }
            Tag::Emphasis => self.inline_stack.push(Inline::Emphasis),
            Tag::Strong => self.inline_stack.push(Inline::Strong),
            Tag::Strikethrough => self.inline_stack.push(Inline::Strikethrough),
            Tag::Link { .. } => self.inline_stack.push(Inline::Link),
            Tag::Image {
                dest_url, title, ..
            } => {
                self.flush_paragraph();
                let alt = title.to_string();
                if dest_url.starts_with("http://") || dest_url.starts_with("https://") {
                    self.push_segment(Segment::Text(vec![Line::from(Span::styled(
                        format!("[image: {dest_url}]"),
                        Style::default()
                            .fg(Color::DarkGray)
                            .add_modifier(Modifier::ITALIC),
                    ))]));
                } else if dest_url.starts_with("data:") {
                    // Same `Segment::Image`/`doc_images` path as a real
                    // file — `app::load_image_protocol` decodes this one
                    // from the URL's own base64 payload instead of reading
                    // `path` off disk (there's nothing on disk to read;
                    // the whole data: URL string is just a stable, unique
                    // cache key here, same idea as `app::
                    // link_preview_image_path`'s synthetic path).
                    let path = std::path::PathBuf::from(dest_url.as_ref());
                    self.push_segment(Segment::Image { path, alt });
                } else {
                    let path = self.base_dir.join(dest_url.as_ref());
                    self.push_segment(Segment::Image { path, alt });
                }
            }
            Tag::Table(alignments) => {
                self.flush_paragraph();
                self.table = Some(TableState {
                    alignments,
                    rows: Vec::new(),
                    current_row: Vec::new(),
                    current_cell: String::new(),
                    in_head: false,
                });
            }
            Tag::TableHead => {
                if let Some(t) = &mut self.table {
                    t.in_head = true;
                }
            }
            Tag::TableRow | Tag::TableCell => {}
            _ => {}
        }
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph => self.flush_paragraph(),
            TagEnd::Heading(_) => {
                if let Some(style) = self.pending_heading_style.take() {
                    self.current = self
                        .current
                        .drain(..)
                        .map(|s| Span::styled(s.content, style))
                        .collect();
                }
                self.flush_paragraph();
            }
            TagEnd::BlockQuote(_) => {
                self.flush_line();
                self.quote_depth = self.quote_depth.saturating_sub(1);
            }
            TagEnd::CodeBlock => {
                let lang = self.code_lang.take().unwrap_or_default();
                let name = self.code_name.take();
                let code = std::mem::take(&mut self.code_buf);
                let highlighted = self.hl.highlight(&lang, &code);
                let border = Style::default().fg(Color::DarkGray);
                // Mirrors the web UI's code-block head (lang + run name) so
                // it's clear at a glance what `r` would actually run, and
                // doubles as a visual break between back-to-back fences —
                // see `push_segment` for the blank-line half of that.
                let label = match &name {
                    Some(n) if n != &lang => format!(" {lang} · {n} "),
                    Some(n) => format!(" {n} "),
                    None => format!(" {lang} "),
                };
                // `┌` matches the box-drawing set ratatui's own pane
                // borders already use (see `Borders::ALL` in ui.rs), so the
                // corner reads as the same kind of line, not a stray glyph.
                let mut framed: Vec<Line<'static>> =
                    vec![Line::from(Span::styled(format!("┌─{label}──"), border))];
                framed.extend(highlighted.into_iter().map(|l| {
                    let mut spans = vec![Span::styled("│ ", border)];
                    spans.extend(l.spans);
                    Line::from(spans)
                }));
                framed.push(Line::from(Span::styled("└─", border)));
                self.push_segment(Segment::Text(framed));
            }
            TagEnd::List(_) => {
                self.list_stack.pop();
                self.flush_paragraph();
            }
            TagEnd::Item => self.flush_line(),
            TagEnd::Emphasis => {
                self.inline_stack.retain(|m| *m != Inline::Emphasis);
            }
            TagEnd::Strong => {
                if let Some(pos) = self.inline_stack.iter().rposition(|m| *m == Inline::Strong) {
                    self.inline_stack.remove(pos);
                }
            }
            TagEnd::Strikethrough => {
                self.inline_stack.retain(|m| *m != Inline::Strikethrough);
            }
            TagEnd::Link => {
                self.inline_stack.retain(|m| *m != Inline::Link);
            }
            TagEnd::TableCell => {
                if let Some(t) = &mut self.table {
                    let cell = std::mem::take(&mut t.current_cell);
                    t.current_row.push(cell);
                }
            }
            TagEnd::TableRow | TagEnd::TableHead => {
                if let Some(t) = &mut self.table {
                    let row = std::mem::take(&mut t.current_row);
                    t.rows.push(row);
                    t.in_head = false;
                }
            }
            TagEnd::Table => {
                if let Some(t) = self.table.take() {
                    self.push_segment(Segment::Text(render_table(&t)));
                }
            }
            _ => {}
        }
    }
}

fn render_table(t: &TableState) -> Vec<Line<'static>> {
    let cols = t.rows.iter().map(|r| r.len()).max().unwrap_or(0);
    let mut widths = vec![0usize; cols];
    for row in &t.rows {
        for (i, cell) in row.iter().enumerate() {
            widths[i] = widths[i].max(cell.chars().count());
        }
    }
    let pad = |cell: &str, w: usize, align: Alignment| -> String {
        let len = cell.chars().count();
        let gap = w.saturating_sub(len);
        match align {
            Alignment::Right => format!("{}{cell}", " ".repeat(gap)),
            Alignment::Center => {
                let left = gap / 2;
                format!("{}{cell}{}", " ".repeat(left), " ".repeat(gap - left))
            }
            _ => format!("{cell}{}", " ".repeat(gap)),
        }
    };
    let mut lines = Vec::new();
    for (ri, row) in t.rows.iter().enumerate() {
        let mut spans = Vec::new();
        for (i, w) in widths.iter().enumerate() {
            let cell = row.get(i).map(String::as_str).unwrap_or("");
            let align = t.alignments.get(i).copied().unwrap_or(Alignment::None);
            let text = pad(cell, *w, align);
            let style = if ri == 0 {
                Style::default().add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            spans.push(Span::styled(text, style));
            spans.push(Span::raw(" │ "));
        }
        lines.push(Line::from(spans));
        if ri == 0 {
            let rule: String = widths.iter().map(|w| "─".repeat(w + 3)).collect();
            lines.push(Line::from(Span::styled(
                rule,
                Style::default().fg(Color::DarkGray),
            )));
        }
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    // TODO.canvas.md: "Base64 image" — a `data:` image URL becomes an
    // ordinary `Segment::Image`, same as a real local file, so
    // `app::load_image_protocol` can pick it up through the exact same
    // `doc_images` path (see that function's own `data:` branch) — not
    // the inert "[image: url]" text `http(s)://` gets, and not treated as
    // a relative filesystem path to join under `base_dir`.
    #[test]
    fn a_data_url_image_becomes_a_segment_image_keyed_by_the_url_itself() {
        let hl = Highlighter::new();
        let md = "![a pixel](data:image/png;base64,iVBORw0KGgo=)\n";
        let segments = render(md, Path::new("/nonexistent-base-dir"), &hl);
        let Segment::Image { path, alt } = segments
            .into_iter()
            .find(|s| matches!(s, Segment::Image { .. }))
            .expect("a data: URL should produce a Segment::Image")
        else {
            unreachable!()
        };
        assert_eq!(path, PathBuf::from("data:image/png;base64,iVBORw0KGgo="));
        assert_eq!(alt, "");
    }

    #[test]
    fn an_http_image_is_still_inert_text_not_a_segment_image() {
        let hl = Highlighter::new();
        let md = "![x](https://example.com/pic.png)\n";
        let segments = render(md, Path::new("/nonexistent-base-dir"), &hl);
        assert!(!segments.iter().any(|s| matches!(s, Segment::Image { .. })));
    }
}
