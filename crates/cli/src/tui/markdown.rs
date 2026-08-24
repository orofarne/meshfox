//! Markdown -> ratatui content, for the TUI's document pane.
//!
//! Deliberately hand-rolled over `pulldown-cmark`'s event stream rather than
//! pulling in a ready-made "markdown to ratatui Text" crate — a `meshfox`
//! node body mixes prose with fenced code (syntax-highlighted via `syntect`)
//! and local images (rendered via `ratatui-image`), and `ratatui::text::Text`
//! alone can't carry the latter: an image is a widget, not styled text. See
//! `Segment` below for the split.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use pulldown_cmark::{
    Alignment, BlockQuoteKind, CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd,
};
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
    Image {
        path: PathBuf,
        alt: String,
        /// `{width=NN%}`/`{height=NN%}` right after the image (see
        /// `meshfox_core::image_attrs`) — a terminal has no pixel grid to
        /// map an absolute `width=300` onto, so only the `%` form is
        /// honored here: it scales `app::load_image_protocol`'s own fixed
        /// size budget. A literal `width=300` (no `%`) is parsed but has
        /// no effect in the TUI — same "narrow support, no crash" fallback
        /// the rest of this syntax uses elsewhere.
        width_percent: Option<u32>,
        height_percent: Option<u32>,
    },
}

/// Loads syntect's bundled (compiled-in, no on-disk assets) syntax/theme
/// sets once and reuses them for every code fence — these sets are a few
/// MB to build and meant to be shared, not rebuilt per fence. `syntax_set`
/// is `Arc`-wrapped so it can be shared with `edtui`'s own full-screen
/// editor too (`edtui::SyntaxHighlighter::with_sets`, see `tui::ui`) — both
/// TUI surfaces then know about the same custom grammars, not two
/// independently-loaded sets.
pub struct Highlighter {
    syntax_set: Arc<SyntaxSet>,
    theme: Theme,
}

impl Highlighter {
    /// Defaults-only — only the real app's own `with_extra_syntaxes` runs
    /// outside tests, so this is `cfg(test)` rather than plain `pub`.
    #[cfg(test)]
    pub fn new() -> Self {
        Self::with_syntax_set(SyntaxSet::load_defaults_newlines())
    }

    /// Same as `new`, but the `SyntaxSet` also includes whatever custom
    /// grammars `crate::syntax_registry::build_syntax_set` found under
    /// `canvas_root` (locally, `.meshfox/syntax/`) or `~/.meshfox/syntax/`
    /// (globally) — the constructor the real app uses; `new` stays
    /// defaults-only for tests that don't care about local grammars.
    pub fn with_extra_syntaxes(canvas_root: &Path) -> Self {
        Self::with_syntax_set(crate::syntax_registry::build_syntax_set(canvas_root))
    }

    fn with_syntax_set(syntax_set: SyntaxSet) -> Self {
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
        Highlighter {
            syntax_set: Arc::new(syntax_set),
            theme,
        }
    }

    /// The underlying `SyntaxSet`, `Arc`-shared so a caller (the TUI's
    /// `edtui`-based full-screen editor) can hand the exact same grammar
    /// set to `edtui::SyntaxHighlighter::with_sets` instead of loading its
    /// own separate one.
    pub fn syntax_set(&self) -> &Arc<SyntaxSet> {
        &self.syntax_set
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

/// Icon, label, and terminal color for a GFM alert blockquote's title
/// line (`Tag::BlockQuote(Some(kind))`) — same five roles and, loosely,
/// the same colors as `site-template/style.css`'s own `--alert-*`
/// variables, so the type reads the same way across every renderer.
fn alert_style(kind: BlockQuoteKind) -> (&'static str, &'static str, Color) {
    match kind {
        BlockQuoteKind::Note => ("ℹ", "Note", Color::LightBlue),
        BlockQuoteKind::Tip => ("💡", "Tip", Color::LightGreen),
        BlockQuoteKind::Important => ("❗", "Important", Color::LightMagenta),
        BlockQuoteKind::Warning => ("⚠", "Warning", Color::LightYellow),
        BlockQuoteKind::Caution => ("🛑", "Caution", Color::LightRed),
    }
}

/// An inline footnote-reference marker (`Event::FootnoteReference`) —
/// real Unicode superscript when every character of `label` has one (see
/// `meshfox_core::subsup::to_unicode`; true for the common case of a
/// plain numeric label like `"1"`), falling back to a bracketed literal
/// (`[note]`) otherwise, same "don't half-transliterate" fallback the
/// `~sub~`/`^sup^` syntax itself uses. Doesn't attempt to renumber labels
/// into sequential display order the way `pulldown-cmark`'s own HTML
/// writer does — this is a single streaming pass with no lookahead across
/// the whole document, and in practice a footnote's own label is already
/// written as the sequence number a document's author wants shown.
fn footnote_reference_marker(label: &str) -> String {
    match meshfox_core::subsup::to_unicode(label, meshfox_core::subsup::Script::Sup) {
        Some(sup) => sup,
        None => format!("[{label}]"),
    }
}

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
    // `ENABLE_GFM` is what makes `pulldown-cmark` recognize `> [!NOTE]`/...
    // alert blockquotes (`Tag::BlockQuote(Some(kind))`, marker line
    // already stripped) — see `start`'s own `Tag::BlockQuote` arm below.
    // `ENABLE_TASKLISTS`/`ENABLE_FOOTNOTES` are handled by `event`'s own
    // `Event::TaskListMarker`/`Event::FootnoteReference` arms and `start`'s
    // `Tag::FootnoteDefinition` arm — without a handler for the events they
    // introduce, turning these flags on with no further changes would have
    // been a regression (a checkbox/footnote marker silently dropped
    // instead of showing as plain literal text), which is why they weren't
    // enabled here before now.
    let options = Options::ENABLE_TABLES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_GFM
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_FOOTNOTES;
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
    /// This fence's own `interpreter=` attribute, if any — mirrors the web
    /// UI's `#!interpreter` suffix on the code-block head (see
    /// `web/src/MeshNode.tsx`'s `mesh-code-interpreter`).
    code_interpreter: Option<String>,
    code_buf: String,
    table: Option<TableState>,
    pending_heading_style: Option<Style>,
    /// Parallel stack to `quote_depth` — which (if any) GFM alert kind
    /// each currently-open blockquote is, so `TagEnd::BlockQuote` can pop
    /// in step. Only ever read at `Tag::BlockQuote`'s own start (to print
    /// the title line); nothing downstream needs the current top.
    alert_stack: Vec<Option<BlockQuoteKind>>,
    /// Set right after pushing a `Segment::Image` — the very next
    /// `Event::Text`, if it matches `meshfox_core::image_attrs`'s narrow
    /// `{width=..}`/`{height=..}` grammar, is consumed as sizing for that
    /// image instead of being rendered as literal text (see `event`).
    /// Cleared on every other event so only a marker written with no gap
    /// right after the image counts.
    pending_image_attrs: bool,
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
            code_interpreter: None,
            code_buf: String::new(),
            table: None,
            pending_heading_style: None,
            alert_stack: Vec::new(),
            pending_image_attrs: false,
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

    /// Applies a `{width=NN%}`/`{height=NN%}` marker (see
    /// `pending_image_attrs`) to the `Segment::Image` just pushed —
    /// always the last segment at this point, since nothing else can run
    /// between pushing it and the very next event being checked. Only the
    /// `%` form has any effect (see `Segment::Image`'s own doc comment).
    fn apply_pending_image_attrs(&mut self, attrs: &meshfox_core::image_attrs::ImageAttrs) {
        let Some(Segment::Image {
            width_percent,
            height_percent,
            ..
        }) = self.segments.last_mut()
        else {
            return;
        };
        if let Some(w) = attrs.width {
            if w.percent {
                *width_percent = Some(w.value);
            }
        }
        if let Some(h) = attrs.height {
            if h.percent {
                *height_percent = Some(h.value);
            }
        }
    }

    fn event(&mut self, ev: Event) {
        // `pending_image_attrs` (see its own doc comment) only ever
        // applies to the very next event, and only if that event is
        // `Text` — anything else (another tag, a line break, ...) means
        // there was no `{width=..}` marker right after the image, so the
        // flag is cleared unconditionally here rather than only on a
        // successful match.
        if let Event::Text(t) = &ev {
            if self.pending_image_attrs {
                self.pending_image_attrs = false;
                if let Some((attrs, consumed)) = meshfox_core::image_attrs::parse(t) {
                    self.apply_pending_image_attrs(&attrs);
                    let rest = t[consumed..].to_string();
                    if !rest.is_empty() {
                        self.push_text(&meshfox_core::subsup::render_unicode(&rest), Style::default());
                    }
                    return;
                }
            }
        } else {
            self.pending_image_attrs = false;
        }
        match ev {
            Event::Start(tag) => self.start(tag),
            Event::End(tag) => self.end(tag),
            Event::Text(t) => {
                if self.code_lang.is_some() {
                    self.code_buf.push_str(&t);
                } else if let Some(table) = &mut self.table {
                    table
                        .current_cell
                        .push_str(&meshfox_core::subsup::render_unicode(&t));
                } else {
                    self.push_text(&meshfox_core::subsup::render_unicode(&t), Style::default());
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
            // Fires right after `Start(Item)` for a task-list item — same
            // spot the item's own bullet/number marker was just pushed
            // into `self.current` by `Tag::Item` below, so this simply
            // appends the checkbox right after it (`• [ ] text`/`1. [x]
            // text`) rather than replacing the bullet outright.
            Event::TaskListMarker(checked) => {
                let (text, style) = if checked {
                    ("[x] ", Style::default().fg(Color::LightGreen))
                } else {
                    ("[ ] ", Style::default())
                };
                self.current.push(Span::styled(text, style));
            }
            // A footnote *reference* (the inline `[^1]` citation, not its
            // definition — see `Tag::FootnoteDefinition` below). Rendered
            // as real Unicode superscript when the label's characters all
            // have one (true for the common case of a plain numeric
            // label), same `subsup::to_unicode` fallback-to-bracketed-
            // literal the sub/superscript syntax itself uses when a
            // character has no small-form glyph — so `[^note]` reads as
            // `[note]` rather than a silently-dropped reference.
            Event::FootnoteReference(label) => {
                let marker = footnote_reference_marker(&label);
                self.push_text(&marker, Style::default().fg(Color::Cyan));
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
            Tag::BlockQuote(kind) => {
                self.flush_paragraph();
                self.quote_depth += 1;
                self.alert_stack.push(kind);
                // GFM alert (`> [!NOTE]`/...) — `pulldown-cmark` (with
                // `Options::ENABLE_GFM`) already stripped the marker line
                // and handed us the kind directly, so there's no text to
                // parse here: just a title line, at this quote's own
                // indent, styled per kind. The body underneath renders as
                // an ordinary indented blockquote, unchanged.
                if let Some(kind) = kind {
                    let (icon, label, color) = alert_style(kind);
                    let indent = self.indent();
                    self.lines.push(Line::from(vec![
                        Span::raw(indent),
                        Span::styled(
                            format!("{icon} {label}"),
                            Style::default().fg(color).add_modifier(Modifier::BOLD),
                        ),
                    ]));
                }
            }
            // A footnote *definition* — the block the reference (`Event::
            // FootnoteReference` above) points at, wherever in the
            // document it's actually written (often the bottom, but
            // nothing requires that). Rendered as its own segment: a
            // bracketed label line (`[1]`, literal — not superscript here,
            // unlike the inline reference marker; a label heading its own
            // block reads clearer as plain bracketed text than floating
            // superscript characters would) followed by the definition's
            // own body, which flows in normally via the ordinary
            // paragraph/text handling right after this.
            Tag::FootnoteDefinition(label) => {
                self.flush_paragraph();
                self.lines.push(Line::from(Span::styled(
                    format!("[{label}]"),
                    Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
                )));
            }
            Tag::CodeBlock(kind) => {
                self.flush_paragraph();
                let (lang, name, interpreter) = match kind {
                    // The info string carries meshfox's own attributes past
                    // the language token (`name="..."`, `cache`, ...) — see
                    // `meshfox_core::fence`, which this mirrors just enough
                    // to pull out `name`/`interpreter` for the block header
                    // below.
                    CodeBlockKind::Fenced(info) => {
                        let mut tokens = meshfox_core::attrs::tokenize(&info).into_iter();
                        let lang = tokens.next().unwrap_or_else(|| "text".to_string());
                        let mut attrs = meshfox_core::attrs::attrs_from_tokens(tokens);
                        let name = attrs.remove("name");
                        let interpreter = attrs.remove("interpreter");
                        (lang, name, interpreter)
                    }
                    CodeBlockKind::Indented => ("text".to_string(), None, None),
                };
                self.code_lang = Some(lang);
                self.code_name = name;
                self.code_interpreter = interpreter;
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
                    self.push_segment(Segment::Image {
                        path,
                        alt,
                        width_percent: None,
                        height_percent: None,
                    });
                } else {
                    let path = self.base_dir.join(dest_url.as_ref());
                    self.push_segment(Segment::Image {
                        path,
                        alt,
                        width_percent: None,
                        height_percent: None,
                    });
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
                self.alert_stack.pop();
            }
            TagEnd::CodeBlock => {
                let lang = self.code_lang.take().unwrap_or_default();
                let name = self.code_name.take();
                let interpreter = self.code_interpreter.take();
                let code = std::mem::take(&mut self.code_buf);
                let highlighted = self.hl.highlight(&lang, &code);
                let border = Style::default().fg(Color::DarkGray);
                // Mirrors the web UI's code-block head (lang + run name) so
                // it's clear at a glance what `r` would actually run, and
                // doubles as a visual break between back-to-back fences —
                // see `push_segment` for the blank-line half of that.
                let mut label = match &name {
                    Some(n) if n != &lang => format!(" {lang} · {n}"),
                    Some(n) => format!(" {n}"),
                    None => format!(" {lang}"),
                };
                // `#!interpreter`, exactly as written in the attribute (no
                // case-folding) — mirrors the web UI's own
                // `mesh-code-interpreter` suffix right after the lang/name.
                if let Some(interpreter) = &interpreter {
                    label.push_str(&format!(" #!{interpreter}"));
                }
                label.push(' ');
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
            TagEnd::Image => {
                self.pending_image_attrs = true;
            }
            TagEnd::FootnoteDefinition => {
                self.flush_paragraph();
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
        let Segment::Image { path, alt, .. } = segments
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

    fn line_text(line: &Line) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    fn segment_text(segments: &[Segment]) -> String {
        segments
            .iter()
            .flat_map(|s| match s {
                Segment::Text(lines) => lines.iter().map(line_text).collect::<Vec<_>>(),
                Segment::Image { .. } => vec![],
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    // TODO.canvas.md: "Формальные граматики для meshfox:*" subtree ->
    // "Атрибуты картинок в markdown" — `{width=NN%}`/`{height=NN%}` right
    // after an image becomes a sizing hint on the resulting
    // `Segment::Image`; only the `%` form has any effect in the TUI (see
    // `app::image_size_budget`).
    #[test]
    fn image_percent_attrs_become_a_sizing_hint() {
        let hl = Highlighter::new();
        let md = "![alt](pic.png){width=50% height=25%}\n";
        let segments = render(md, Path::new("/nonexistent-base-dir"), &hl);
        let Segment::Image {
            width_percent,
            height_percent,
            ..
        } = segments
            .into_iter()
            .find(|s| matches!(s, Segment::Image { .. }))
            .unwrap()
        else {
            unreachable!()
        };
        assert_eq!(width_percent, Some(50));
        assert_eq!(height_percent, Some(25));
    }

    #[test]
    fn image_absolute_attrs_are_parsed_but_have_no_tui_effect() {
        let hl = Highlighter::new();
        let md = "![alt](pic.png){width=300}\n";
        let segments = render(md, Path::new("/nonexistent-base-dir"), &hl);
        let Segment::Image { width_percent, .. } = segments
            .into_iter()
            .find(|s| matches!(s, Segment::Image { .. }))
            .unwrap()
        else {
            unreachable!()
        };
        assert_eq!(width_percent, None);
    }

    #[test]
    fn text_right_after_an_image_with_no_attrs_marker_is_rendered_normally() {
        let hl = Highlighter::new();
        let md = "![alt](pic.png) just text\n";
        let segments = render(md, Path::new("/nonexistent-base-dir"), &hl);
        assert!(segment_text(&segments).contains("just text"));
    }

    // TODO.canvas.md: same subtree -> "Подстрочный/надстрочный".
    #[test]
    fn subscript_and_superscript_render_as_unicode_small_forms() {
        let hl = Highlighter::new();
        let md = "H~2~O and x^n^\n";
        let segments = render(md, Path::new("/nonexistent-base-dir"), &hl);
        assert_eq!(segment_text(&segments), "H₂O and xⁿ");
    }

    #[test]
    fn subsup_falls_back_to_literal_when_not_fully_mapped_to_unicode() {
        let hl = Highlighter::new();
        let md = "x~query~\n";
        let segments = render(md, Path::new("/nonexistent-base-dir"), &hl);
        assert_eq!(segment_text(&segments), "x~query~");
    }

    #[test]
    fn subsup_never_applies_inside_a_code_block() {
        let hl = Highlighter::new();
        let md = "```text\nx~2~\n```\n";
        let segments = render(md, Path::new("/nonexistent-base-dir"), &hl);
        assert!(segment_text(&segments).contains("x~2~"));
    }

    // TODO.canvas.md: same subtree -> "Admonition/callout-блоки" (GFM
    // variant).
    #[test]
    fn a_gfm_alert_blockquote_gets_a_styled_title_line() {
        let hl = Highlighter::new();
        let md = "> [!WARNING]\n> be careful\n";
        let segments = render(md, Path::new("/nonexistent-base-dir"), &hl);
        let text = segment_text(&segments);
        assert!(text.contains("Warning"), "{text}");
        assert!(text.contains("be careful"), "{text}");
        assert!(!text.contains("[!WARNING]"), "{text}");
    }

    #[test]
    fn an_ordinary_blockquote_gets_no_title_line() {
        let hl = Highlighter::new();
        let md = "> just a quote\n";
        let segments = render(md, Path::new("/nonexistent-base-dir"), &hl);
        let text = segment_text(&segments);
        assert!(text.contains("just a quote"), "{text}");
        assert!(!text.contains("Note"), "{text}");
    }

    // Follow-up to the comparison table in MARKDOWN.md: task lists and
    // footnotes were parsed-but-unhandled (`Options::ENABLE_TASKLISTS`/
    // `ENABLE_FOOTNOTES` were off) — enabling the flags with no event
    // handling would have silently dropped the checkbox/reference marker
    // rather than showing plain literal text, a regression from the prior
    // no-flag behavior. These tests cover the new handling instead.
    #[test]
    fn task_list_items_show_a_checkbox_after_their_bullet() {
        let hl = Highlighter::new();
        let md = "- [ ] todo\n- [x] done\n";
        let segments = render(md, Path::new("/nonexistent-base-dir"), &hl);
        let text = segment_text(&segments);
        assert!(text.contains("[ ] todo"), "{text}");
        assert!(text.contains("[x] done"), "{text}");
    }

    #[test]
    fn a_numeric_footnote_reference_renders_as_unicode_superscript() {
        let hl = Highlighter::new();
        let md = "See[^1].\n\n[^1]: A note.\n";
        let segments = render(md, Path::new("/nonexistent-base-dir"), &hl);
        let text = segment_text(&segments);
        assert!(text.contains("See¹."), "{text}");
        assert!(!text.contains("[^1]"), "{text}");
    }

    #[test]
    fn a_footnote_definition_gets_a_bracketed_label_and_its_body() {
        let hl = Highlighter::new();
        let md = "See[^1].\n\n[^1]: A note.\n";
        let segments = render(md, Path::new("/nonexistent-base-dir"), &hl);
        let text = segment_text(&segments);
        assert!(text.contains("[1]"), "{text}");
        assert!(text.contains("A note."), "{text}");
    }

    #[test]
    fn a_named_footnote_reference_falls_back_to_a_bracketed_label() {
        let hl = Highlighter::new();
        // 'q' has no superscript Unicode glyph, so "note" (which does map
        // fully) is deliberately not used here — want the fallback path.
        let md = "See[^query].\n\n[^query]: A note.\n";
        let segments = render(md, Path::new("/nonexistent-base-dir"), &hl);
        let text = segment_text(&segments);
        assert!(text.contains("See[query]."), "{text}");
    }

    #[test]
    fn a_fences_own_interpreter_attr_shows_up_as_a_shebang_suffix_on_its_header() {
        let hl = Highlighter::new();
        let md = "```python name=\"seed\" interpreter=\"python3 -u\"\nprint(1)\n```\n";
        let segments = render(md, Path::new("/nonexistent-base-dir"), &hl);
        let text = segment_text(&segments);
        // Exactly as written in the attribute — no case-folding — mirrors
        // the web UI's own `mesh-code-interpreter` suffix.
        assert!(text.contains("python · seed #!python3 -u"), "{text}");
    }

    #[test]
    fn a_fence_with_no_interpreter_attr_has_no_shebang_suffix() {
        let hl = Highlighter::new();
        let md = "```bash name=\"build\" cache\necho hi\n```\n";
        let segments = render(md, Path::new("/nonexistent-base-dir"), &hl);
        let text = segment_text(&segments);
        assert!(!text.contains("#!"), "{text}");
    }
}
