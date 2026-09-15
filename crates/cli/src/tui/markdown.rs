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

/// What clicking a `ClickRegion` (below) should do — resolved once, at
/// render time, into everything `App::on_mouse` needs to act on it without
/// re-parsing the document's own Markdown.
#[derive(Clone)]
pub enum ClickTarget {
    /// A `button` fence's own `▶ caption` marker — runs its `deps=` chain
    /// (plus its own, always-empty body), same as pressing `r` on it would.
    RunBlock { node_id: String, block_name: String },
    /// A block name inside a deps line (`├─ after: …`/`via …`) — the TUI
    /// counterpart to the web UI's `jumpTo`: moves the tree's own selection
    /// to the named block's owning node.
    JumpToNode { node_id: String },
    /// One field row of a `form`-lang fence — opens (or refocuses)
    /// `App::active_inline_form` on this field, in edit mode. `field_index`
    /// indexes that form's own `fields`/`decls`/`inputs`, in document
    /// order. See SPEC.md's "Form fences".
    FormField {
        node_id: String,
        block_name: String,
        field_index: usize,
    },
    /// A `form`-lang fence's own `[send caption]` row — submits it
    /// directly, no prior field focus needed, same one-click convention
    /// `RunBlock` already has for a `button` fence's marker.
    FormSend { node_id: String, block_name: String },
}

/// One clickable span inside a rendered `Segment::Text`, in the segment's
/// own (pre-scroll, pre-wrap) coordinates — `ui::render_document` is what
/// translates this into an actual on-screen `Rect` for `App::on_mouse` to
/// hit-test against, the same "render decides where things land on screen,
/// this only decides what's clickable" split `App::doc_segments` itself
/// already has from `ui::render_document`.
/// `(col_start, col_end, node_id)` for one not-yet-a-`ClickRegion` deps-line
/// block-name span — see `Renderer::dep_line`/`push_dep_clicks`.
type DepClicks = Vec<(u16, u16, String)>;

pub struct ClickRegion {
    /// Index into the `Vec<Segment>` `render` returns alongside these.
    pub segment_index: usize,
    /// Index into that segment's own `Vec<Line>` (`Segment::Text` only —
    /// nothing here ever targets a `Segment::Image`).
    pub line_index: usize,
    /// Column range within that one line, in terminal columns (not bytes)
    /// — `[col_start, col_end)`.
    pub col_start: u16,
    pub col_end: u16,
    pub target: ClickTarget,
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
        Self::with_syntax_set(SyntaxSet::load_defaults_newlines(), crate::tui::ui::SOURCE_EDITOR_THEME)
    }

    /// Same as `new`, but the `SyntaxSet` also includes whatever custom
    /// grammars `crate::syntax_registry::build_syntax_set` found under
    /// `canvas_root` (locally, `.meshfox/syntax/`) or `~/.meshfox/syntax/`
    /// (globally) — the constructor the real app uses; `new` stays
    /// defaults-only for tests that don't care about local grammars.
    /// `theme_name` is `crate::syntax_registry::resolve_editor_theme`'s own
    /// result — same theme the full-screen source editor uses (`tui::ui`),
    /// so this read-only preview pane and that editor read as one product,
    /// not two independently-themed surfaces.
    pub fn with_extra_syntaxes(canvas_root: &Path, theme_name: &str) -> Self {
        Self::with_syntax_set(crate::syntax_registry::build_syntax_set(canvas_root), theme_name)
    }

    fn with_syntax_set(syntax_set: SyntaxSet, theme_name: &str) -> Self {
        let theme_set = ThemeSet::load_defaults();
        let theme = theme_set
            .themes
            .get(theme_name)
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
/// never actually chose. Deliberately never carries the theme's own
/// per-token `background` across — the bundled `syntect` theme fills it in
/// densely enough to read as a solid gray box behind every code line,
/// which doesn't match the web UI's plain, borderless code text; only
/// `foreground` (the actual syntax coloring) survives the translation.
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
pub fn render(
    md: &str,
    base_dir: &Path,
    hl: &Highlighter,
    node_id: &str,
    decls: &[meshfox_core::vars::VarDecl],
    form_values: &std::collections::HashMap<String, String>,
    // `Some((block_name, selected_index))` while this node's own inline
    // form is actively `editing` (see `App::active_inline_form`'s own doc
    // comment) — lets the form-lang branch of `TagEnd::CodeBlock` draw a
    // visible focus indicator (reversed row, blinking `_` text cursor) on
    // whichever field/Send row is currently selected, the same convention
    // `render_var_form` (`ui.rs`) already uses for the unrelated "configure
    // variables" modal. `None` whenever this form is merely open (not
    // `editing`) or this isn't the node it belongs to — no focus to show.
    form_focus: Option<(&str, usize)>,
    // Live output from the most recent run, for whichever of this node's own
    // blocks it covers — keyed by block name (already scoped to this
    // `node_id` by the caller, see `App::render_current_document`). The TUI
    // equivalent of the web UI's `LiveRunOutput`: shown right under a
    // runnable fence in `TagEnd::CodeBlock`, same spot an on-disk cached
    // `OutputRegion` would occupy, but sourced from `App::step_output`
    // instead of the document's own text — this never touches `md` at all.
    live_output: &std::collections::HashMap<String, super::app::StepOutput>,
) -> (Vec<Segment>, Vec<ClickRegion>) {
    // Pre-scanned once so `Tag::CodeBlock`'s own handling (`start`) can
    // resolve a fence's *real* run name — including the implicit "sole
    // unnamed fence in this node" rule (`fence::scan_runnable_blocks`'s own
    // doc comment) — by matching its byte span, rather than re-deriving
    // that same implicit-naming rule a second time here.
    let runnable = meshfox_core::fence::scan_runnable_blocks(node_id, md);
    let mut renderer = Renderer::new(base_dir, hl, node_id, decls, &runnable, form_values, form_focus, live_output);
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
    // `into_offset_iter` (not the plain `Parser` iterator `event` alone
    // would give) so `start` can resolve a code fence's click target by
    // where it actually sits in `md`, the same reasoning as above.
    for (event, range) in Parser::new_ext(md, options).into_offset_iter() {
        renderer.event(event, range.start);
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
    /// The node this body belongs to — bare `deps=`/`env=` references
    /// resolve against this (see `dep_line`), same convention
    /// `deps::resolve_ref`/`vars::scan_all_var_decls` already use.
    node_id: &'a str,
    /// Every declared `meshfox:var` in the whole document — see `dep_line`.
    decls: &'a [meshfox_core::vars::VarDecl],
    /// Every runnable fence in this same body, pre-scanned by `render` —
    /// `Tag::CodeBlock`'s own click-target resolution matches the current
    /// fence's byte span against this to find its *real* run name,
    /// including one only assigned implicitly (see `render`'s own doc
    /// comment).
    runnable: &'a [meshfox_core::fence::CodeBlock],
    /// Current display value for every `form`-field-targeted variable,
    /// keyed by var name — `App::render_current_document`'s own
    /// `session_vars`/declared-default fallback, with whichever field is
    /// actively being typed into (`App::active_inline_form`) overlaid on
    /// top. Consulted only by the `form`-lang branch of `TagEnd::CodeBlock`
    /// — see `Segment`'s own doc comment for why this has to be resolved
    /// by the caller rather than here: a form field's value can come from
    /// a live, per-keystroke edit buffer that has nothing to do with this
    /// node body's own Markdown.
    form_values: &'a std::collections::HashMap<String, String>,
    /// `render`'s own `form_focus` parameter, threaded straight through —
    /// see that parameter's own doc comment.
    form_focus: Option<(&'a str, usize)>,
    /// `render`'s own `live_output` parameter, threaded straight through —
    /// see that parameter's own doc comment.
    live_output: &'a std::collections::HashMap<String, super::app::StepOutput>,
    segments: Vec<Segment>,
    click_regions: Vec<ClickRegion>,
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
    /// This fence's own raw `deps=`/`env=` attributes, if any — parsed in
    /// `TagEnd::CodeBlock` into `dep_line`'s explicit/implicit deps line.
    code_deps: Option<String>,
    code_env: Option<String>,
    /// This fence's own `send=` attribute, if any — only meaningful on a
    /// `lang="form"` fence (see `TagEnd::CodeBlock`'s own form branch);
    /// falls back to a plain `"Send"` when omitted, same default
    /// `meshfox_core::form::FormBlock::send` itself documents.
    code_send: Option<String>,
    /// This fence's own *resolved* run name — its explicit `name=`, or (a
    /// lone unnamed fence) the implicit one `fence::scan_runnable_blocks`
    /// would assign it — looked up against `runnable` when the fence
    /// starts (`start`'s own `Tag::CodeBlock` arm). Kept separate from
    /// `code_name` (the raw, possibly-absent explicit attribute, still used
    /// for the header's own display label) so resolving this for click
    /// purposes never changes what a fence's header actually shows.
    /// `None` for a fence `scan_runnable_blocks` wouldn't consider runnable
    /// at all (wrong language, or one of several unnamed siblings).
    code_click_name: Option<String>,
    code_buf: String,
    /// Set between a `<!-- meshfox:output name="..." ... -->` marker and
    /// its matching `<!-- /meshfox:output -->` (an `output="markdown"`
    /// block's own cached, spliced-in result — see `crate::output`) — see
    /// `OutputRegion`'s own doc comment for what happens inside one.
    output_region: Option<OutputRegion>,
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

/// State inside a `<!-- meshfox:output name="..." ... --> ... <!--
/// /meshfox:output -->` region — an `output="markdown"` block's own real
/// result gets its own purple frame, same color identity as its source
/// block's (`theme::DEP` — see `dep_line`), so it reads as visually
/// grouped with it instead of blending into surrounding prose.
/// `render_output_block_markdown` (`crates/core/src/output.rs`) always
/// writes stderr, if any, first, as its own `` ```text `` fence, *before*
/// the real spliced Markdown — that gets its own separate frame (handled
/// directly in `TagEnd::CodeBlock`, never through `push_segment`'s own
/// wrapping below), not nested inside the Markdown one, so the two read
/// as two related-but-distinct panels rather than one frame accidentally
/// doubled.
struct OutputRegion {
    name: String,
    /// Cleared by whichever code path handles this region's very first
    /// segment — the leading stderr fence special-case in
    /// `TagEnd::CodeBlock`, or `push_segment`'s own generic path — so
    /// exactly one of them claims it.
    first_segment_pending: bool,
    /// Whether this region's own "markdown" frame's header has already
    /// been pushed — opened lazily, right before the first segment that
    /// isn't the leading stderr fence (`push_segment`).
    markdown_frame_open: bool,
}

impl<'a> Renderer<'a> {
    fn new(
        base_dir: &'a Path,
        hl: &'a Highlighter,
        node_id: &'a str,
        decls: &'a [meshfox_core::vars::VarDecl],
        runnable: &'a [meshfox_core::fence::CodeBlock],
        form_values: &'a std::collections::HashMap<String, String>,
        form_focus: Option<(&'a str, usize)>,
        live_output: &'a std::collections::HashMap<String, super::app::StepOutput>,
    ) -> Self {
        Renderer {
            base_dir,
            hl,
            node_id,
            decls,
            runnable,
            form_values,
            form_focus,
            live_output,
            segments: Vec::new(),
            click_regions: Vec::new(),
            lines: Vec::new(),
            current: Vec::new(),
            inline_stack: Vec::new(),
            list_stack: Vec::new(),
            quote_depth: 0,
            code_lang: None,
            code_name: None,
            code_interpreter: None,
            code_deps: None,
            code_env: None,
            code_send: None,
            code_click_name: None,
            code_buf: String::new(),
            output_region: None,
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
    ///
    /// Inside an `output_region` (see its own doc comment), this is also
    /// where that region's own "markdown" frame gets opened, lazily,
    /// right before whichever segment is the first one it actually claims
    /// (`TagEnd::CodeBlock`'s leading-stderr special case claims one
    /// itself instead, via `push_segment_plain`, before this ever runs).
    /// The header is pushed (via `push_segment_plain`, so its own blank
    /// separator reads as *outside* the frame) before
    /// `markdown_frame_open` flips to `true` — only then does `seg`'s own
    /// blank separator, computed fresh inside `push_segment_plain`, read
    /// as *inside* it and get the same `│ ` border `seg` itself does.
    fn push_segment(&mut self, seg: Segment) {
        let needs_frame = matches!(&self.output_region, Some(r) if !r.markdown_frame_open);
        if needs_frame {
            let name = self.output_region.as_ref().unwrap().name.clone();
            self.push_segment_plain(Segment::Text(vec![Line::from(Span::styled(
                format!("┌─ output: {name} · markdown ──"),
                Style::default().fg(super::theme::DEP),
            ))]));
            if let Some(region) = &mut self.output_region {
                region.first_segment_pending = false;
                region.markdown_frame_open = true;
            }
        }
        self.push_segment_plain(self.wrap_in_output_border(seg));
    }

    /// `push_segment`, minus the frame-*opening* decision above — still
    /// applies the region's own border to its own blank separator (via
    /// `wrap_in_output_border`, checked fresh against whatever
    /// `output_region` state holds *right now*), just never opens/claims
    /// the frame itself. Used for a segment that has already fully
    /// decided its own framing: the leading-stderr special case in
    /// `TagEnd::CodeBlock` (frame not open yet — its own separator comes
    /// out unwrapped, correctly sitting outside/before the markdown
    /// frame), the markdown frame's own header just above (same reason),
    /// and its closing line in `handle_output_marker` (frame *is* still
    /// open at that point — its own separator comes out wrapped, closing
    /// the border cleanly down to the `└─` corner).
    fn push_segment_plain(&mut self, seg: Segment) {
        if !self.segments.is_empty() {
            let blank = self.wrap_in_output_border(Segment::Text(vec![Line::from("")]));
            self.segments.push(blank);
        }
        self.segments.push(seg);
    }

    /// Turns `dep_line`'s own `(col_start, col_end, node_id)` triples into
    /// real `ClickRegion`s, now that the segment they belong to is actually
    /// on `self.segments` (always its last entry — nothing else can have
    /// run between `push_segment`/`push_segment_plain` and this) and its
    /// index is known. The deps line is always line `1` within its own
    /// segment: line `0` is the fence's header, pushed right before it (see
    /// `TagEnd::CodeBlock`). A no-op for a fence with no deps line at all
    /// (`clicks` empty).
    fn push_dep_clicks(&mut self, clicks: DepClicks) {
        if clicks.is_empty() {
            return;
        }
        let segment_index = self.segments.len() - 1;
        for (col_start, col_end, node_id) in clicks {
            self.click_regions.push(ClickRegion {
                segment_index,
                line_index: 1,
                col_start,
                col_end,
                target: ClickTarget::JumpToNode { node_id },
            });
        }
    }

    /// Renders `block_name`'s own live output (`App::step_output`, the most
    /// recent run's already-finished result for this exact block) right
    /// under the fence it belongs to — same framed-box convention the
    /// on-disk `OutputRegion` splice uses for *cached* output, distinguished
    /// by a "· live" marker in the header, since the two can legitimately
    /// coexist (a fresh live run of a block whose last `cache`d result is
    /// still sitting in the document). Deliberately simpler than
    /// `OutputRegion`'s own handling: always plain text (no nested Markdown
    /// re-render for `output="markdown"` blocks — that richer treatment
    /// stays specific to the Output pane's own chain-wide view for now).
    fn push_live_output(&mut self, block_name: &str, live: &super::app::StepOutput) {
        let border = Style::default().fg(super::theme::DEP);
        // Flagged, not specially rendered — see this method's own doc
        // comment on why `output="markdown"` doesn't get the Output pane's
        // richer nested-Markdown treatment here (yet).
        let kind = if live.output_markdown { " · markdown" } else { "" };
        // `live.duration_ms`/`exit_code` are just placeholders until a run
        // discovered passively (`App::on_external_run_event`) reaches its
        // own terminal event — showing "done · 0ms" the moment its first
        // line streams in would be a straight-up lie about a run that's
        // still going. A self-triggered run never has `running: true` here
        // at all (see `StepOutput::running`'s own doc comment) — its own
        // exit code/duration are already real by the time this struct
        // exists.
        let header = if live.running {
            format!("┌─ output: {block_name} · live{kind} · running ──")
        } else {
            let status = if live.exit_code == 0 { "done" } else { "failed" };
            format!(
                "┌─ output: {block_name} · live{kind} · {status} · {} ──",
                meshfox_core::format_duration_ms(live.duration_ms)
            )
        };
        let mut framed: Vec<Line<'static>> = vec![Line::from(Span::styled(header, border))];
        let mut any_output = false;
        for text in [&live.stdout, &live.stderr] {
            for line in text.lines() {
                any_output = true;
                framed.push(Line::from(vec![
                    Span::styled("│ ", border),
                    Span::raw(line.to_string()),
                ]));
            }
        }
        if !any_output {
            framed.push(Line::from(Span::styled("│ (no output)", border)));
        }
        framed.push(Line::from(Span::styled("└─", border)));
        self.push_segment(Segment::Text(framed));
    }

    /// Prefixes every line of `seg` with a purple `│ ` — the "markdown"
    /// frame's own left border, while it's actually open (not merely
    /// while inside `output_region` at all — see `push_segment`'s own
    /// doc comment for why the two aren't the same thing).
    fn wrap_in_output_border(&self, seg: Segment) -> Segment {
        let open = matches!(&self.output_region, Some(r) if r.markdown_frame_open);
        if !open {
            return seg;
        }
        match seg {
            Segment::Text(lines) => Segment::Text(
                lines
                    .into_iter()
                    .map(|l| {
                        let mut spans =
                            vec![Span::styled("│ ", Style::default().fg(super::theme::DEP))];
                        spans.extend(l.spans);
                        Line::from(spans)
                    })
                    .collect(),
            ),
            other => other, // Segment::Image — no border to draw around a widget.
        }
    }

    fn finish(mut self) -> (Vec<Segment>, Vec<ClickRegion>) {
        self.flush_paragraph();
        (self.segments, self.click_regions)
    }

    /// Recognizes a `<!-- meshfox:output name="..." ... -->`/
    /// `<!-- /meshfox:output -->` pair (`crate::output`'s own markers —
    /// mirrored here as plain string literals since both are private to
    /// that module, nothing public to import) inside a raw-HTML `text`
    /// event, opening/closing `output_region` around whatever's between
    /// them. Every other HTML comment (`meshfox:node`, `meshfox:var`,
    /// ...) is still just dropped, same as before this existed.
    fn handle_output_marker(&mut self, text: &str) {
        let trimmed = text.trim();
        if let Some(rest) = trimmed.strip_prefix("<!-- meshfox:output ") {
            let attrs_str = rest.strip_suffix("-->").unwrap_or(rest).trim();
            let attrs =
                meshfox_core::attrs::attrs_from_tokens(meshfox_core::attrs::tokenize(attrs_str));
            let name = attrs.get("name").cloned().unwrap_or_default();
            self.flush_paragraph();
            // No frame pushed yet — deferred to the region's first real
            // content, so an empty region (or one whose only content is
            // the leading stderr fence, handled separately) never leaves
            // a stray, contentless frame behind.
            self.output_region = Some(OutputRegion {
                name,
                first_segment_pending: true,
                markdown_frame_open: false,
            });
        } else if trimmed == "<!-- /meshfox:output -->" {
            self.flush_paragraph();
            // `output_region` is only cleared *after* the closer is
            // pushed — `push_segment_plain`'s own blank separator (right
            // above the `└─` corner) still needs to see the frame as open
            // to get wrapped, closing the border cleanly instead of
            // leaving a gap right before it.
            let frame_open = matches!(&self.output_region, Some(r) if r.markdown_frame_open);
            if frame_open {
                self.push_segment_plain(Segment::Text(vec![Line::from(Span::styled(
                    "└─",
                    Style::default().fg(super::theme::DEP),
                ))]));
            }
            self.output_region = None;
        }
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

    fn event(&mut self, ev: Event, span_start: usize) {
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
            Event::Start(tag) => self.start(tag, span_start),
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
            // here rather than shown — except a `meshfox:output`/
            // `/meshfox:output` pair, which still isn't shown itself but
            // now toggles `in_output_region` around whatever's between
            // them (see `wrap_in_output_border`).
            Event::Html(text) | Event::InlineHtml(text) => self.handle_output_marker(&text),
            _ => {}
        }
    }

    fn start(&mut self, tag: Tag, span_start: usize) {
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
                self.code_send = None;
                let (lang, name, interpreter, deps, env) = match kind {
                    // The info string carries meshfox's own attributes past
                    // the language token (`name="..."`, `cache`, ...) — see
                    // `meshfox_core::fence`, which this mirrors just enough
                    // to pull out `name`/`interpreter`/`deps`/`env` for the
                    // block header and its own deps line (`dep_line`) below.
                    CodeBlockKind::Fenced(info) => {
                        let mut tokens = meshfox_core::attrs::tokenize(&info).into_iter();
                        let lang = tokens.next().unwrap_or_else(|| "text".to_string());
                        let mut attrs = meshfox_core::attrs::attrs_from_tokens(tokens);
                        let name = attrs.remove("name");
                        let interpreter = attrs.remove("interpreter");
                        let deps = attrs.remove("deps");
                        let env = attrs.remove("env");
                        self.code_send = attrs.remove("send");
                        (lang, name, interpreter, deps, env)
                    }
                    CodeBlockKind::Indented => ("text".to_string(), None, None, None, None),
                };
                // Matched by byte span (not just re-deriving the "explicit
                // name, else sole-unnamed-fence" rule here) since only
                // `fence::scan_runnable_blocks` knows whether this fence is
                // really the sole unnamed one in the whole node — this
                // renderer only ever sees one fence at a time.
                self.code_click_name = self
                    .runnable
                    .iter()
                    .find(|b| b.span.contains(&span_start))
                    .and_then(|b| b.name.clone());
                self.code_lang = Some(lang);
                self.code_name = name;
                self.code_interpreter = interpreter;
                self.code_deps = deps;
                self.code_env = env;
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

    /// A fenced block's own dependency line, right under its header —
    /// mirrors the web UI's `.mesh-code-deps` row (`web/src/MeshNode.tsx`):
    /// explicit `deps=` first (`after: …`), then a `via VAR: …` hint for
    /// every declared variable this block's `env=`/`interpreter=`
    /// transitively needs (through `default_var=`/`choices_var=` chains —
    /// see `vars::close_over_var_refs`) that's itself `from=`-computed —
    /// the same implicit dependency `deps::implicit_from_deps` folds into
    /// the run-chain graph. `None` when the block has neither kind.
    /// `(col_start, col_end, node_id)` for one block-name span in the
    /// returned `Line` — `dep_line`'s caller turns each into a `ClickRegion`
    /// once it knows which segment/line the line actually landed at (see
    /// `TagEnd::CodeBlock`), the same "this only decides what's clickable"
    /// split `ClickRegion` itself documents.
    fn dep_line(
        &self,
        deps_raw: Option<&str>,
        env_raw: Option<&str>,
        interpreter: Option<&str>,
    ) -> Option<(Line<'static>, DepClicks)> {
        let explicit: Vec<(String, String)> = deps_raw
            .map(meshfox_core::fence::parse_deps_list)
            .unwrap_or_default()
            .iter()
            .map(|r| {
                let addr = meshfox_core::deps::resolve_ref(self.node_id, r);
                (
                    super::app::dep_label(self.node_id, &addr.node_id, &addr.block_name),
                    addr.node_id,
                )
            })
            .collect();

        let env_refs = env_raw.map(meshfox_core::fence::parse_env_list).unwrap_or_default();
        let interp_refs: Vec<String> = interpreter
            .map(meshfox_core::interpreter_var_refs)
            .unwrap_or_default();
        let seed = env_refs
            .iter()
            .map(|e| e.var_name.as_str())
            .chain(interp_refs.iter().map(String::as_str));
        // Sorted for a stable render — `close_over_var_refs` returns a
        // `HashSet`, whose iteration order isn't something this pane
        // should flicker between redraws over.
        let mut var_names: Vec<String> = meshfox_core::vars::close_over_var_refs(self.decls, seed)
            .into_iter()
            .collect();
        var_names.sort();
        // `(var_name, block_label, block_node_id)` — kept as a tuple (not
        // pre-joined into "via VAR: label" the way this used to) so the
        // span-building loop below can style/click-region just the label
        // half of each, same reasoning as `explicit` above.
        let implicit: Vec<(String, String, String)> = var_names
            .into_iter()
            .filter_map(|var_name| {
                let decl = self.decls.iter().find(|d| d.name == var_name)?;
                let from = decl.from.as_ref()?;
                let addr = meshfox_core::deps::resolve_ref(self.node_id, from);
                Some((
                    var_name,
                    super::app::dep_label(self.node_id, &addr.node_id, &addr.block_name),
                    addr.node_id,
                ))
            })
            .collect();

        if explicit.is_empty() && implicit.is_empty() {
            return None;
        }

        let style = Style::default().fg(super::theme::DEP);
        // Underlined so a clickable block name reads as clickable at a
        // glance, the same visual convention as `push_text`'s own
        // `Inline::Link` — everything else on this line (the literal
        // `after:`/`via VAR:` text) stays plain, since only the name itself
        // is ever a `ClickRegion`.
        let click_style = style.add_modifier(Modifier::UNDERLINED);
        let mut spans: Vec<Span<'static>> = Vec::new();
        let mut clicks: DepClicks = Vec::new();
        let mut col: u16 = 0;
        let push_literal = |spans: &mut Vec<Span<'static>>, col: &mut u16, text: String| {
            *col += text.chars().count() as u16;
            spans.push(Span::styled(text, style));
        };
        let push_click = |spans: &mut Vec<Span<'static>>, col: &mut u16, text: String| {
            *col += text.chars().count() as u16;
            spans.push(Span::styled(text, click_style));
        };
        // `├─` (not `│`, the code lines' own left border) — this line is
        // metadata branching off the header, not code content, and reusing
        // `│` made it read as the first line of the block's own body. `├─`
        // is still drawn from the same box-drawing set as the frame's other
        // corners/edges, so it reads as "part of the frame, not a stray
        // glyph" the same way the header's own `┌─` already does — the
        // trailing `─` matches the header's own corner-plus-dash shape
        // (`┌─`), rather than a bare `├` whose own built-in horizontal arm
        // is visibly shorter than a real `─` glyph beside it.
        push_literal(&mut spans, &mut col, "├─ ".to_string());
        let mut first_part = true;
        if !explicit.is_empty() {
            push_literal(&mut spans, &mut col, "after: ".to_string());
            for (i, (label, node_id)) in explicit.into_iter().enumerate() {
                if i > 0 {
                    push_literal(&mut spans, &mut col, ", ".to_string());
                }
                let start = col;
                push_click(&mut spans, &mut col, label);
                clicks.push((start, col, node_id));
            }
            first_part = false;
        }
        for (var_name, label, node_id) in implicit {
            if !first_part {
                push_literal(&mut spans, &mut col, "  ".to_string());
            }
            first_part = false;
            push_literal(&mut spans, &mut col, format!("via {var_name}: "));
            let start = col;
            push_click(&mut spans, &mut col, label);
            clicks.push((start, col, node_id));
        }
        Some((Line::from(spans), clicks))
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
                let deps_raw = self.code_deps.take();
                let env_raw = self.code_env.take();
                let send_raw = self.code_send.take();
                let click_name = self.code_click_name.take();
                let code = std::mem::take(&mut self.code_buf);
                if lang == meshfox_core::FORM_LANG {
                    // Same non-executing, no-frame family as `button`
                    // (below) — a form has no real code to
                    // syntax-highlight, just a `field var=` list to show
                    // as labeled rows plus a Send row. `parse_form_body`
                    // never errors out this renderer even on a malformed
                    // body (a stray non-`field` line, say) — that's
                    // `meshfox validate`'s job to report; here it's just
                    // "nothing usable to show" (see `name.clone()`'s own
                    // fallback for `form_name`, used only for the parse
                    // error this renderer then discards).
                    let form_name = name.clone().unwrap_or_default();
                    let fields = meshfox_core::parse_form_body(&form_name, &code).unwrap_or_default();
                    let send_caption = send_raw.unwrap_or_else(|| "Send".to_string());
                    let label_style = Style::default().fg(super::theme::DEP);
                    let value_style = Style::default()
                        .fg(super::theme::ACCENT)
                        .add_modifier(Modifier::UNDERLINED);
                    let marker_style = Style::default()
                        .fg(super::theme::ACCENT)
                        .add_modifier(Modifier::BOLD | Modifier::UNDERLINED);
                    // Which row (a field, by index, or the virtual Send row
                    // at `fields.len()`) is actually focused right now, if
                    // any — only when `self.form_focus` names *this* fence's
                    // own resolved block name, not some other form fence
                    // elsewhere in the same node. See `render`'s own
                    // `form_focus` doc comment.
                    let focused_index = match (self.form_focus, click_name.as_deref()) {
                        (Some((focus_block, idx)), Some(this_block)) if focus_block == this_block => {
                            Some(idx)
                        }
                        _ => None,
                    };

                    let mut lines: Vec<Line<'static>> = Vec::new();
                    let mut col_ends: Vec<u16> = Vec::new();
                    for (i, field) in fields.iter().enumerate() {
                        let focused = focused_index == Some(i);
                        let label = field.label.clone().unwrap_or_else(|| field.var.clone());
                        let value = self.form_values.get(&field.var).cloned().unwrap_or_default();
                        let prefix = format!("{label}: ");
                        let col_end = (prefix.chars().count() + value.chars().count().max(1)) as u16;
                        // A blinking `_` text cursor on the focused row's
                        // own value, same convention `render_var_form`
                        // (`ui.rs`) already uses for its own text fields —
                        // but only for a `String`/`Int` var (a `Bool`/
                        // `Select` field is a toggle/cycle, not free text,
                        // so there's nothing for a text cursor to mark; an
                        // undeclared/unknown var — `None` here — defaults
                        // to showing one anyway, same as a plain text field
                        // would, rather than silently showing none).
                        let var_type = self.decls.iter().find(|d| d.name == field.var).map(|d| d.var_type);
                        let cursor = focused
                            && !matches!(
                                var_type,
                                Some(meshfox_core::vars::VarType::Bool)
                                    | Some(meshfox_core::vars::VarType::Select)
                            );
                        let mut shown = if value.is_empty() { " ".to_string() } else { value };
                        if cursor {
                            shown.push('_');
                        }
                        let mut value_span = Span::styled(shown, value_style);
                        let mut label_span = Span::styled(prefix, label_style);
                        if focused {
                            label_span = label_span.patch_style(Style::default().add_modifier(Modifier::REVERSED));
                            value_span = value_span.patch_style(
                                Style::default().add_modifier(if cursor {
                                    Modifier::REVERSED | Modifier::SLOW_BLINK
                                } else {
                                    Modifier::REVERSED
                                }),
                            );
                        }
                        lines.push(Line::from(vec![label_span, value_span]));
                        col_ends.push(col_end);
                    }
                    let send_line = format!("[{send_caption}]");
                    let send_col_end = send_line.chars().count() as u16;
                    let mut send_span = Span::styled(send_line, marker_style);
                    if focused_index == Some(fields.len()) {
                        send_span = send_span.patch_style(Style::default().add_modifier(Modifier::REVERSED));
                    }
                    lines.push(Line::from(send_span));

                    self.push_segment(Segment::Text(lines));
                    if let Some(block_name) = click_name {
                        let segment_index = self.segments.len() - 1;
                        for (i, col_end) in col_ends.into_iter().enumerate() {
                            self.click_regions.push(ClickRegion {
                                segment_index,
                                line_index: i,
                                col_start: 0,
                                col_end,
                                target: ClickTarget::FormField {
                                    node_id: self.node_id.to_string(),
                                    block_name: block_name.clone(),
                                    field_index: i,
                                },
                            });
                        }
                        self.click_regions.push(ClickRegion {
                            segment_index,
                            line_index: fields.len(),
                            col_start: 0,
                            col_end: send_col_end,
                            target: ClickTarget::FormSend {
                                node_id: self.node_id.to_string(),
                                block_name,
                            },
                        });
                    }
                    return;
                }
                if lang == meshfox_core::BUTTON_LANG {
                    // No frame, no fill — a bold accent marker instead.
                    // The caption *is* the fence's own body (falling back
                    // to `name` when blank), same as the web UI's
                    // frameless button; no real code to syntax-highlight
                    // here, so the ordinary framed-code rendering below
                    // doesn't apply at all. Clicking the marker itself runs
                    // its `deps=` chain, same as `r` on the tree's selected
                    // node would (see `ClickTarget::RunBlock`) — the
                    // `(r to run)` hint still spells out the keyboard route,
                    // since a TUI has no visual convention suggesting a
                    // plain-text marker is also clickable the way a real
                    // button widget would.
                    let caption = code.trim();
                    let caption = if caption.is_empty() {
                        name.clone().unwrap_or_default()
                    } else {
                        caption.to_string()
                    };
                    // Underlined so the clickable "▶ caption" part reads as
                    // clickable at a glance — the same convention
                    // `dep_line`'s own click spans use — while the
                    // `(r to run)` hint after it (not itself clickable)
                    // stays plain.
                    let marker = Style::default()
                        .fg(super::theme::ACCENT)
                        .add_modifier(Modifier::BOLD | Modifier::UNDERLINED);
                    let hint = Style::default().fg(Color::DarkGray);
                    self.push_segment(Segment::Text(vec![Line::from(vec![
                        Span::styled("▶ ", marker),
                        Span::styled(caption.clone(), marker),
                        Span::styled("  (r to run)", hint),
                    ])]));
                    if let Some(block_name) = click_name {
                        let segment_index = self.segments.len() - 1;
                        // Covers "▶ caption" (not the "(r to run)" hint) —
                        // `2` for "▶ "'s own column width.
                        let col_end = 2 + caption.chars().count() as u16;
                        self.click_regions.push(ClickRegion {
                            segment_index,
                            line_index: 0,
                            col_start: 0,
                            col_end,
                            target: ClickTarget::RunBlock {
                                node_id: self.node_id.to_string(),
                                block_name,
                            },
                        });
                    }
                    return;
                }
                let highlighted = self.hl.highlight(&lang, &code);
                let border = Style::default().fg(super::theme::DEP);
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
                // Deps-line click regions (`dep_line`'s own col-range half)
                // can't become real `ClickRegion`s until `framed` is
                // actually pushed as a segment below — only then is
                // `segment_index` known.
                let mut dep_clicks: DepClicks = Vec::new();
                if let Some((dep_line, clicks)) =
                    self.dep_line(deps_raw.as_deref(), env_raw.as_deref(), interpreter.as_deref())
                {
                    framed.push(dep_line);
                    dep_clicks = clicks;
                }
                framed.extend(highlighted.into_iter().map(|l| {
                    let mut spans = vec![Span::styled("│ ", border)];
                    spans.extend(l.spans);
                    Line::from(spans)
                }));
                framed.push(Line::from(Span::styled("└─", border)));

                // Inside an output region, a leading `` ```text `` fence
                // is `render_output_block_markdown`'s own stderr capture
                // (see `OutputRegion`'s own doc comment) — it gets
                // relabeled and pushed as its own frame, right here,
                // rather than folded into the region's "markdown" one via
                // the ordinary `push_segment` path below.
                if let Some(region) = &mut self.output_region {
                    if region.first_segment_pending && lang == "text" {
                        region.first_segment_pending = false;
                        let name = region.name.clone();
                        framed[0] = Line::from(Span::styled(
                            format!("┌─ output: {name} · text ──"),
                            Style::default().fg(super::theme::DEP),
                        ));
                        self.push_segment_plain(Segment::Text(framed));
                        self.push_dep_clicks(dep_clicks);
                        return;
                    }
                }
                self.push_segment(Segment::Text(framed));
                self.push_dep_clicks(dep_clicks);
                if let Some(block_name) = &click_name {
                    if let Some(live) = self.live_output.get(block_name.as_str()) {
                        self.push_live_output(block_name, live);
                    }
                }
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
        let (segments, _clicks) =
            render(md, Path::new("/nonexistent-base-dir"), &hl, "n", &[], &std::collections::HashMap::new(), None, &std::collections::HashMap::new());
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

    // TUI complaint: an inline form's currently-focused field had no visible
    // indicator at all — every row looked identical regardless of which one
    // arrow keys/tab would actually edit. The focused row should now come
    // back reversed (same convention `render_var_form` already uses for its
    // own modal), and a `String`/`Int` field's value should carry a blinking
    // `_` text cursor while it's the focused one — not otherwise.
    #[test]
    fn a_focused_string_field_gets_a_reversed_row_and_a_blinking_cursor() {
        let hl = Highlighter::new();
        let md = "```form name=\"f\"\nfield var=\"NAME\" label=\"Name\"\n```\n";
        let decls = vec![meshfox_core::vars::VarDecl {
            name: "NAME".to_string(),
            prompt: "Name".to_string(),
            var_type: meshfox_core::vars::VarType::String,
            default: None,
            choices: Vec::new(),
            secret: false,
            required: false,
            from: None,
            session: false,
            default_var: None,
            choices_var: None,
        }];
        let mut values = std::collections::HashMap::new();
        values.insert("NAME".to_string(), "abc".to_string());

        let (unfocused, _) = render(md, Path::new("/x"), &hl, "n", &decls, &values, None, &std::collections::HashMap::new());
        let (focused, _) = render(md, Path::new("/x"), &hl, "n", &decls, &values, Some(("f", 0)), &std::collections::HashMap::new());

        let value_span = |segs: &[Segment]| -> Span<'static> {
            let Segment::Text(lines) = segs.iter().find(|s| matches!(s, Segment::Text(_))).unwrap()
            else {
                unreachable!()
            };
            lines[0].spans[1].clone()
        };
        let unfocused_value = value_span(&unfocused);
        let focused_value = value_span(&focused);

        assert_eq!(unfocused_value.content.as_ref(), "abc");
        assert!(
            !unfocused_value.style.add_modifier.contains(Modifier::REVERSED),
            "an unfocused field row shouldn't be reversed"
        );
        assert_eq!(
            focused_value.content.as_ref(),
            "abc_",
            "the focused field's own value should carry a trailing text cursor"
        );
        assert!(
            focused_value.style.add_modifier.contains(Modifier::REVERSED),
            "the focused field's row should be reversed so it's visibly the one in focus"
        );
    }

    #[test]
    fn an_http_image_is_still_inert_text_not_a_segment_image() {
        let hl = Highlighter::new();
        let md = "![x](https://example.com/pic.png)\n";
        let (segments, _clicks) =
            render(md, Path::new("/nonexistent-base-dir"), &hl, "n", &[], &std::collections::HashMap::new(), None, &std::collections::HashMap::new());
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
        let (segments, _clicks) =
            render(md, Path::new("/nonexistent-base-dir"), &hl, "n", &[], &std::collections::HashMap::new(), None, &std::collections::HashMap::new());
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
        let (segments, _clicks) =
            render(md, Path::new("/nonexistent-base-dir"), &hl, "n", &[], &std::collections::HashMap::new(), None, &std::collections::HashMap::new());
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
        let (segments, _clicks) =
            render(md, Path::new("/nonexistent-base-dir"), &hl, "n", &[], &std::collections::HashMap::new(), None, &std::collections::HashMap::new());
        assert!(segment_text(&segments).contains("just text"));
    }

    // TODO.canvas.md: same subtree -> "Подстрочный/надстрочный".
    #[test]
    fn subscript_and_superscript_render_as_unicode_small_forms() {
        let hl = Highlighter::new();
        let md = "H~2~O and x^n^\n";
        let (segments, _clicks) =
            render(md, Path::new("/nonexistent-base-dir"), &hl, "n", &[], &std::collections::HashMap::new(), None, &std::collections::HashMap::new());
        assert_eq!(segment_text(&segments), "H₂O and xⁿ");
    }

    #[test]
    fn subsup_falls_back_to_literal_when_not_fully_mapped_to_unicode() {
        let hl = Highlighter::new();
        let md = "x~query~\n";
        let (segments, _clicks) =
            render(md, Path::new("/nonexistent-base-dir"), &hl, "n", &[], &std::collections::HashMap::new(), None, &std::collections::HashMap::new());
        assert_eq!(segment_text(&segments), "x~query~");
    }

    #[test]
    fn subsup_never_applies_inside_a_code_block() {
        let hl = Highlighter::new();
        let md = "```text\nx~2~\n```\n";
        let (segments, _clicks) =
            render(md, Path::new("/nonexistent-base-dir"), &hl, "n", &[], &std::collections::HashMap::new(), None, &std::collections::HashMap::new());
        assert!(segment_text(&segments).contains("x~2~"));
    }

    // TODO.canvas.md: same subtree -> "Admonition/callout-блоки" (GFM
    // variant).
    #[test]
    fn a_gfm_alert_blockquote_gets_a_styled_title_line() {
        let hl = Highlighter::new();
        let md = "> [!WARNING]\n> be careful\n";
        let (segments, _clicks) =
            render(md, Path::new("/nonexistent-base-dir"), &hl, "n", &[], &std::collections::HashMap::new(), None, &std::collections::HashMap::new());
        let text = segment_text(&segments);
        assert!(text.contains("Warning"), "{text}");
        assert!(text.contains("be careful"), "{text}");
        assert!(!text.contains("[!WARNING]"), "{text}");
    }

    #[test]
    fn an_ordinary_blockquote_gets_no_title_line() {
        let hl = Highlighter::new();
        let md = "> just a quote\n";
        let (segments, _clicks) =
            render(md, Path::new("/nonexistent-base-dir"), &hl, "n", &[], &std::collections::HashMap::new(), None, &std::collections::HashMap::new());
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
        let (segments, _clicks) =
            render(md, Path::new("/nonexistent-base-dir"), &hl, "n", &[], &std::collections::HashMap::new(), None, &std::collections::HashMap::new());
        let text = segment_text(&segments);
        assert!(text.contains("[ ] todo"), "{text}");
        assert!(text.contains("[x] done"), "{text}");
    }

    #[test]
    fn a_numeric_footnote_reference_renders_as_unicode_superscript() {
        let hl = Highlighter::new();
        let md = "See[^1].\n\n[^1]: A note.\n";
        let (segments, _clicks) =
            render(md, Path::new("/nonexistent-base-dir"), &hl, "n", &[], &std::collections::HashMap::new(), None, &std::collections::HashMap::new());
        let text = segment_text(&segments);
        assert!(text.contains("See¹."), "{text}");
        assert!(!text.contains("[^1]"), "{text}");
    }

    #[test]
    fn a_footnote_definition_gets_a_bracketed_label_and_its_body() {
        let hl = Highlighter::new();
        let md = "See[^1].\n\n[^1]: A note.\n";
        let (segments, _clicks) =
            render(md, Path::new("/nonexistent-base-dir"), &hl, "n", &[], &std::collections::HashMap::new(), None, &std::collections::HashMap::new());
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
        let (segments, _clicks) =
            render(md, Path::new("/nonexistent-base-dir"), &hl, "n", &[], &std::collections::HashMap::new(), None, &std::collections::HashMap::new());
        let text = segment_text(&segments);
        assert!(text.contains("See[query]."), "{text}");
    }

    #[test]
    fn a_fences_own_interpreter_attr_shows_up_as_a_shebang_suffix_on_its_header() {
        let hl = Highlighter::new();
        let md = "```python name=\"seed\" interpreter=\"python3 -u\"\nprint(1)\n```\n";
        let (segments, _clicks) =
            render(md, Path::new("/nonexistent-base-dir"), &hl, "n", &[], &std::collections::HashMap::new(), None, &std::collections::HashMap::new());
        let text = segment_text(&segments);
        // Exactly as written in the attribute — no case-folding — mirrors
        // the web UI's own `mesh-code-interpreter` suffix.
        assert!(text.contains("python · seed #!python3 -u"), "{text}");
    }

    #[test]
    fn a_fence_with_no_interpreter_attr_has_no_shebang_suffix() {
        let hl = Highlighter::new();
        let md = "```bash name=\"build\" cache\necho hi\n```\n";
        let (segments, _clicks) =
            render(md, Path::new("/nonexistent-base-dir"), &hl, "n", &[], &std::collections::HashMap::new(), None, &std::collections::HashMap::new());
        let text = segment_text(&segments);
        assert!(!text.contains("#!"), "{text}");
    }

    #[test]
    fn a_button_fence_renders_its_body_as_the_caption_with_a_run_hint() {
        let hl = Highlighter::new();
        let md = "```button name=\"full-import\" deps=\"build\"\n🚀 Run everything\n```\n";
        let (segments, _clicks) =
            render(md, Path::new("/nonexistent-base-dir"), &hl, "n", &[], &std::collections::HashMap::new(), None, &std::collections::HashMap::new());
        let text = segment_text(&segments);
        assert!(text.contains("🚀 Run everything"), "{text}");
        assert!(text.contains("(r to run)"), "{text}");
        // No framed-code rendering (no `┌─`/`│ `/`└─` border) — a button
        // fence has no real code to put in a frame.
        assert!(!text.contains('┌'), "{text}");
    }

    #[test]
    fn a_button_fence_falls_back_to_its_name_when_the_body_is_blank() {
        let hl = Highlighter::new();
        let md = "```button name=\"full-import\" deps=\"build\"\n```\n";
        let (segments, _clicks) =
            render(md, Path::new("/nonexistent-base-dir"), &hl, "n", &[], &std::collections::HashMap::new(), None, &std::collections::HashMap::new());
        let text = segment_text(&segments);
        assert!(text.contains("full-import"), "{text}");
    }

    #[test]
    fn a_button_fences_caption_is_rendered_as_a_bold_accent_marker() {
        let hl = Highlighter::new();
        let md = "```button name=\"full-import\" deps=\"build\"\nRun everything\n```\n";
        let (segments, _clicks) =
            render(md, Path::new("/nonexistent-base-dir"), &hl, "n", &[], &std::collections::HashMap::new(), None, &std::collections::HashMap::new());
        let Segment::Text(lines) = &segments[0] else {
            panic!("expected a text segment");
        };
        let caption_span = lines[0]
            .spans
            .iter()
            .find(|s| s.content.contains("Run everything"))
            .expect("caption span");
        assert_eq!(caption_span.style.bg, None, "no fill — see design choice above");
        assert_eq!(caption_span.style.fg, Some(crate::tui::theme::ACCENT));
        assert!(caption_span.style.add_modifier.contains(Modifier::BOLD));

        let marker_span = lines[0]
            .spans
            .iter()
            .find(|s| s.content.contains('▶'))
            .expect("▶ marker span");
        assert_eq!(marker_span.style.fg, Some(crate::tui::theme::ACCENT));
    }
}
