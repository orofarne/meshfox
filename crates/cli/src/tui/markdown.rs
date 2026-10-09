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
        /// `{bg=#rrggbb}` — painted behind an SVG when it's rasterized
        /// (`svg_raster::rasterize`), so a transparent diagram drawn for a
        /// light page stays legible on the terminal's dark background.
        /// Only SVGs are rasterized by us, so it has no effect on a PNG/
        /// JPEG (those already carry their own pixels).
        bg: Option<meshfox_core::image_attrs::Background>,
    },
}

/// Session-local presentation of a runnable code block. `None` follows `fold`.
#[derive(Clone, Default)]
pub struct BlockView {
    pub collapsed: bool,
    pub source_expanded: Option<bool>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockAction {
    Select,
    ToggleBlock,
    ToggleSource,
    History,
    Run,
    Chain,
}

/// What clicking a `ClickRegion` (below) should do — resolved once, at
/// render time, into everything `App::on_mouse` needs to act on it without
/// re-parsing the document's own Markdown.
#[derive(Clone)]
pub enum ClickTarget {
    Block {
        node_id: String,
        block_name: String,
        action: BlockAction,
    },
    /// A `button` fence's own `▶ caption` marker — runs its `deps=` chain
    /// (plus its own, always-empty body), same as pressing `r` on it would.
    RunBlock { node_id: String, block_name: String },
    /// A block name inside a deps line (`├─ deps: …`/`via var: …`) — the TUI
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
/// block-name span — see `Renderer::dep_lines`/`push_dep_clicks`.
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
        Self::with_syntax_set(
            SyntaxSet::load_defaults_newlines(),
            crate::tui::ui::SOURCE_EDITOR_THEME,
        )
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
        Self::with_syntax_set(
            crate::syntax_registry::build_syntax_set(canvas_root),
            theme_name,
        )
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
#[allow(clippy::too_many_arguments)]
pub fn render(
    md: &str,
    base_dir: &Path,
    hl: &Highlighter,
    node_id: &str,
    decls: &[meshfox_core::vars::VarDecl],
    form_values: &std::collections::HashMap<String, String>,
    form_focus: Option<(&str, usize)>,
    live_output: &std::collections::HashMap<String, super::app::StepOutput>,
) -> (Vec<Segment>, Vec<ClickRegion>) {
    render_with_blocks(
        md,
        base_dir,
        hl,
        node_id,
        decls,
        form_values,
        form_focus,
        live_output,
        &Default::default(),
        None,
        false,
        true,
    )
}

#[allow(clippy::too_many_arguments)]
pub fn render_with_blocks(
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
    block_views: &std::collections::HashMap<String, BlockView>,
    selected_block: Option<&str>,
    focused: bool,
    runnable_controls: bool,
) -> (Vec<Segment>, Vec<ClickRegion>) {
    // Pre-scanned once so `Tag::CodeBlock`'s own handling (`start`) can
    // resolve a fence's *real* run name — including the implicit "sole
    // unnamed fence in this node" rule (`fence::scan_runnable_blocks`'s own
    // doc comment) — by matching its byte span, rather than re-deriving
    // that same implicit-naming rule a second time here.
    let runnable = meshfox_core::fence::scan_runnable_blocks(node_id, md);
    let mut renderer = Renderer::new(
        base_dir,
        hl,
        node_id,
        decls,
        &runnable,
        form_values,
        form_focus,
        live_output,
    );
    renderer.block_views = block_views.clone();
    renderer.selected_block = selected_block.map(str::to_owned);
    renderer.focused = focused;
    renderer.runnable_controls = runnable_controls;
    let signatures = meshfox_core::args::scan_signatures(node_id, md).unwrap_or_default();
    for block in &runnable {
        let mut details = Vec::new();
        if let Some(signature) = signatures.iter().find(|sig| sig.block.span == block.span) {
            for arg in &signature.args {
                let mut label = format!("args: {}: {}", arg.name, arg.var_type.as_str());
                if !arg.choices.is_empty() {
                    label.push_str(&format!(" [{}]", arg.choices.join(" | ")));
                }
                if arg.is_required() {
                    label.push_str(" · required");
                }
                if let Some(default) = &arg.default {
                    label.push_str(&format!(
                        " · {}: {:?}",
                        if arg.is_required() {
                            "suggestion"
                        } else {
                            "default"
                        },
                        default
                    ));
                }
                details.push(label);
            }
        }
        for decl in decls.iter().filter(|decl| {
            decl.from.as_ref().is_some_and(|source| {
                source.node_id.as_deref() == Some(node_id)
                    && Some(source.block_name.as_str()) == block.name.as_deref()
            })
        }) {
            details.push(format!("exports: {}: {}", decl.name, decl.var_type.as_str()));
        }
        for kind in ["inputs", "outputs"] {
            if let Some(paths) = block.attrs.get(kind) {
                for path in paths.split(',').map(str::trim).filter(|p| !p.is_empty()) {
                    details.push(format!("{kind}: {path}"));
                }
            }
        }
        for env in block.env.iter().filter(|env| env.var_name.contains("${")) {
            details.push(format!("env: {} ← {}", env.local_name, env.var_name));
            let mut applications: Vec<_> = live_output.keys().collect();
            applications.sort();
            for application in applications {
                let Ok(app) = meshfox_core::args::Application::parse(application) else { continue; };
                if Some(app.definition.as_str()) != block.name.as_deref() { continue; }
                let arguments = app.bindings.into_iter().filter_map(|(name, binding)| match binding {
                    meshfox_core::args::Binding::Literal(value) => Some((name, value)),
                    _ => None,
                }).collect();
                if let Ok(selected) = meshfox_core::args::bind_env_name(&env.var_name, &arguments) {
                    let source = decls.iter().find(|decl| decl.name == selected)
                        .and_then(|decl| decl.from.as_ref())
                        .map(|source| format!(" ← {}/{}", source.node_id.as_deref().unwrap_or(node_id), source.block_name))
                        .unwrap_or_default();
                    details.push(format!("env: {application}: {selected}{source}"));
                }
            }
        }
        renderer.block_details.insert(block.span.start, details);
    }
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
    block_views: std::collections::HashMap<String, BlockView>,
    selected_block: Option<String>,
    focused: bool,
    runnable_controls: bool,
    base_dir: &'a Path,
    hl: &'a Highlighter,
    /// The node this body belongs to — bare `deps=`/`env=` references
    /// resolve against this (see `dep_lines`), same convention
    /// `deps::resolve_ref`/`vars::scan_all_var_decls` already use.
    node_id: &'a str,
    /// Every declared `meshfox:var` in the whole document — see `dep_lines`.
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
    /// `TagEnd::CodeBlock` into `dep_lines`'s explicit/implicit deps line.
    code_deps: Option<String>,
    code_env: Option<String>,
    block_details: std::collections::HashMap<usize, Vec<String>>,
    code_details: Vec<String>,
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
    /// `Some` between `Tag::Image` and its `TagEnd::Image`: the image's own
    /// alt text (`![this part](url)`) arrives as ordinary `Text` events in
    /// between, which belong to the image (its "failed to load" fallback,
    /// `Segment::Image::alt`) — not to the paragraph text after it, where
    /// they used to leak as a stray caption line under every image.
    image_alt: Option<String>,
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
/// block's (`theme::DEP` — see `dep_lines`), so it reads as visually
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
    /// This block has live output (`Renderer::live_output`), which takes
    /// over from the cached copy in the file — the web UI's `RunOutput`
    /// shows one or the other, never both — so everything this region
    /// would draw is dropped (`push_segment_plain`). Showing both put two
    /// same-looking, different-data results (the fresh run's, and the
    /// last-saved one's) under one block.
    suppressed: bool,
}

impl<'a> Renderer<'a> {
    #[allow(clippy::too_many_arguments)]
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
            block_views: Default::default(),
            selected_block: None,
            focused: false,
            runnable_controls: true,
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
            block_details: Default::default(),
            code_details: Vec::new(),
            code_send: None,
            code_click_name: None,
            code_buf: String::new(),
            output_region: None,
            table: None,
            pending_heading_style: None,
            alert_stack: Vec::new(),
            pending_image_attrs: false,
            image_alt: None,
        }
    }

    fn block_border(&self, name: Option<&str>) -> Style {
        let selected = name.is_some() && name == self.selected_block.as_deref();
        Style::default().fg(if selected {
            if self.focused {
                super::theme::ACCENT
            } else {
                Color::DarkGray
            }
        } else {
            super::theme::DEP
        })
    }

    fn block_header(&self, name: Option<&str>) -> Style {
        let border = self.block_border(name);
        if name.is_some() && name == self.selected_block.as_deref() {
            border.add_modifier(Modifier::BOLD).bg(if self.focused {
                super::theme::MAP_SELECTED_BG
            } else {
                super::theme::MAP_NODE_BG
            })
        } else {
            border
        }
    }

    fn output_border(&self) -> Style {
        self.block_border(self.output_region.as_ref().map(|r| r.name.as_str()))
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
        if matches!(&self.output_region, Some(r) if r.suppressed) {
            return;
        }
        let needs_frame = matches!(&self.output_region, Some(r) if !r.markdown_frame_open);
        if needs_frame {
            let name = self.output_region.as_ref().unwrap().name.clone();
            self.push_segment_plain(Segment::Text(vec![Line::from(Span::styled(
                format!("┌─ output: {name} · markdown ──"),
                self.block_header(Some(&name)),
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
        if matches!(&self.output_region, Some(r) if r.suppressed) {
            return;
        }
        if !self.segments.is_empty() {
            let blank = self.wrap_in_output_border(Segment::Text(vec![Line::from("")]));
            self.segments.push(blank);
        }
        self.segments.push(seg);
    }

    /// Turns `dep_lines`'s own `(col_start, col_end, node_id)` triples into
    /// real `ClickRegion`s, now that the segment they belong to is actually
    /// on `self.segments` (always its last entry — nothing else can have
    /// run between `push_segment`/`push_segment_plain` and this) and its
    /// index is known. Each contract row carries its actual line index;
    /// this applies to framed code and button blocks alike.
    fn push_dep_clicks(&mut self, rows: Vec<(usize, DepClicks)>) {
        let segment_index = self.segments.len() - 1;
        for (line_index, clicks) in rows {
            for (col_start, col_end, node_id) in clicks {
                self.click_regions.push(ClickRegion {
                    segment_index,
                    line_index,
                    col_start,
                    col_end,
                    target: ClickTarget::JumpToNode { node_id },
                });
            }
        }
    }

    /// Renders `block_name`'s own live output (`App::step_output`, the most
    /// recent run's already-finished result for this exact block) right
    /// under the fence it belongs to — same framed-box convention the
    /// on-disk `OutputRegion` splice uses for *cached* output, distinguished
    /// by a "· live" marker in the header, since the two can legitimately
    /// coexist (a fresh live run of a block whose last `cache`d result is
    /// still sitting in the document). An `output="markdown"` block's stdout
    /// is plain text while the run is still going and re-rendered as real
    /// Markdown once it's done — same "raw while running, rendered once
    /// done" the web UI's `LiveRunOutput` and the Output pane
    /// (`ui::render_output`) give it; stderr always stays plain text.
    fn push_live_output(&mut self, block_name: &str, live: &super::app::StepOutput) {
        let border = self.block_border(Some(block_name));
        let kind = if live.output_markdown {
            " · markdown"
        } else {
            ""
        };
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
            let status = if live.exit_code == 0 {
                "done"
            } else {
                "failed"
            };
            format!(
                "┌─ output: {block_name} · live{kind} · {status} · {} ──",
                meshfox_core::format_duration_ms(live.duration_ms)
            )
        };
        let mut framed: Vec<Line<'static>> = vec![Line::from(Span::styled(
            header,
            self.block_header(Some(block_name)),
        ))];
        let mut any_output = false;
        // Only the first piece of this frame goes through `push_segment`
        // (which adds the blank separator above it); every later piece is
        // part of the same frame, so it is appended directly.
        let mut first_piece = true;
        let mut flush = |this: &mut Self, lines: Vec<Line<'static>>| {
            if lines.is_empty() {
                return;
            }
            if first_piece {
                first_piece = false;
                this.push_segment(Segment::Text(lines));
            } else {
                this.segments.push(Segment::Text(lines));
            }
        };

        // The Markdown to render this finished run's stdout as: its stdout
        // itself for an `output="markdown"` block, or — for an
        // `output="image"` block, whose stdout is a raw SVG — the same
        // image line the cached copy has (`None` for a failed run or
        // non-SVG stdout, which then shows as the plain text it is below).
        let markdown_stdout: Option<String> = if live.running {
            None
        } else if live.output_markdown && !live.stdout.trim().is_empty() {
            Some(live.stdout.clone())
        } else {
            self.runnable
                .iter()
                .find(|b| b.name.as_deref() == Some(block_name))
                .filter(|b| b.attrs.get("output").map(String::as_str) == Some("image"))
                .and_then(|b| {
                    meshfox_core::output::image_output_markdown(
                        block_name,
                        &live.stdout,
                        live.exit_code,
                        b.attrs.get("output-attrs").map(String::as_str),
                    )
                })
        };
        if let Some(markdown) = markdown_stdout {
            any_output = true;
            // stderr first, muted, then a rule — the web UI's
            // `MarkdownOutput` does the same: stderr was never this block's
            // Markdown, so it stays visibly apart from the rendered part
            // instead of running straight into the last table row.
            if !live.stderr.trim().is_empty() {
                let muted = Style::default().fg(super::theme::BORDER);
                for line in live.stderr.lines() {
                    framed.push(Line::from(vec![
                        Span::styled("│ ", border),
                        Span::styled(line.to_string(), muted),
                    ]));
                }
                framed.push(Line::from(Span::styled("│ ────", border)));
            }
            let (segs, _clicks) = render_with_blocks(
                &markdown,
                self.base_dir,
                self.hl,
                self.node_id,
                &[],
                &std::collections::HashMap::new(),
                None,
                &std::collections::HashMap::new(),
                &Default::default(),
                None,
                false,
                false,
            );
            let mut first_seg = true;
            for seg in segs {
                match seg {
                    Segment::Text(seg_lines) => {
                        if !first_seg {
                            framed.push(Line::from(Span::styled("│", border)));
                        }
                        for l in seg_lines {
                            let mut spans = vec![Span::styled("│ ", border)];
                            spans.extend(l.spans);
                            framed.push(Line::from(spans));
                        }
                    }
                    // A widget can't sit inside a text border: close off the
                    // lines so far, let the image through as-is, continue.
                    image @ Segment::Image { .. } => {
                        flush(self, std::mem::take(&mut framed));
                        self.segments.push(image);
                    }
                }
                first_seg = false;
            }
        } else {
            for text in [&live.stdout, &live.stderr] {
                for line in text.lines() {
                    any_output = true;
                    framed.push(Line::from(vec![
                        Span::styled("│ ", border),
                        Span::raw(line.to_string()),
                    ]));
                }
            }
        }
        if !any_output {
            framed.push(Line::from(Span::styled("│ (no output)", border)));
        }
        framed.push(Line::from(Span::styled("└─", border)));
        flush(self, framed);
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
                        let mut spans = vec![Span::styled("│ ", self.output_border())];
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
            let suppressed = self.live_output.contains_key(name.as_str())
                || self.block_views.get(&name).is_some_and(|v| v.collapsed);
            self.output_region = Some(OutputRegion {
                name,
                first_segment_pending: true,
                markdown_frame_open: false,
                suppressed,
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
                    self.output_border(),
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
            bg,
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
        *bg = attrs.bg;
    }

    fn event(&mut self, ev: Event, span_start: usize) {
        if let Some(alt) = &mut self.image_alt {
            match &ev {
                Event::Text(t) | Event::Code(t) => {
                    alt.push_str(t);
                    return;
                }
                Event::SoftBreak | Event::HardBreak => {
                    alt.push(' ');
                    return;
                }
                _ => {}
            }
        }
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
                        self.push_text(
                            &meshfox_core::subsup::render_unicode(&rest),
                            Style::default(),
                        );
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
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
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
                    // block header and its own deps line (`dep_lines`) below.
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
                self.code_details = self
                    .runnable
                    .iter()
                    .find(|b| b.span.contains(&span_start))
                    .and_then(|b| self.block_details.get(&b.span.start))
                    .cloned()
                    .unwrap_or_default();
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
                // The alt text itself isn't known yet (its `Text` events
                // follow) — `TagEnd::Image` fills it in; the `title=` is
                // only the fallback for an image with no alt text.
                self.image_alt = Some(String::new());
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
                        bg: None,
                    });
                } else {
                    let path = self.base_dir.join(dest_url.as_ref());
                    self.push_segment(Segment::Image {
                        path,
                        alt,
                        width_percent: None,
                        height_percent: None,
                        bg: None,
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

    /// Always-visible contract rows: one explicit dependency or computed
    /// variable source per row, with jump targets for the source names.
    fn dep_lines(
        &self,
        deps_raw: Option<&str>,
        env_raw: Option<&str>,
        interpreter: Option<&str>,
    ) -> Vec<(Line<'static>, DepClicks)> {
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

        let env_refs = env_raw
            .map(meshfox_core::fence::parse_env_list)
            .unwrap_or_default();
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

        let style = Style::default().fg(super::theme::DEP);
        let click_style = style.add_modifier(Modifier::UNDERLINED);
        let row = |prefix: String, label: String, node_id: String| {
            let start = prefix.chars().count() as u16;
            let end = start + label.chars().count() as u16;
            (
                Line::from(vec![
                    Span::styled(prefix, style),
                    Span::styled(label, click_style),
                ]),
                vec![(start, end, node_id)],
            )
        };
        let mut rows = Vec::new();
        for (label, node_id) in explicit {
            rows.push(row("├─ deps: ".into(), label, node_id));
        }
        for (var_name, label, node_id) in implicit {
            rows.push(row(format!("├─ via var: {var_name} → "), label, node_id));
        }
        rows
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
                let click_name = self
                    .code_click_name
                    .take()
                    .filter(|_| self.runnable_controls);
                let details = std::mem::take(&mut self.code_details);
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
                    let fields =
                        meshfox_core::parse_form_body(&form_name, &code).unwrap_or_default();
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
                        (Some((focus_block, idx)), Some(this_block))
                            if focus_block == this_block =>
                        {
                            Some(idx)
                        }
                        _ => None,
                    };

                    let mut lines: Vec<Line<'static>> = Vec::new();
                    let mut col_ends: Vec<u16> = Vec::new();
                    for (i, field) in fields.iter().enumerate() {
                        let focused = focused_index == Some(i);
                        let label = field.label.clone().unwrap_or_else(|| field.var.clone());
                        let value = self
                            .form_values
                            .get(&field.var)
                            .cloned()
                            .unwrap_or_default();
                        let prefix = format!("{label}: ");
                        let col_end =
                            (prefix.chars().count() + value.chars().count().max(1)) as u16;
                        // A blinking `_` text cursor on the focused row's
                        // own value, same convention `render_var_form`
                        // (`ui.rs`) already uses for its own text fields —
                        // but only for a `String`/`Int` var (a `Bool`/
                        // `Select` field is a toggle/cycle, not free text,
                        // so there's nothing for a text cursor to mark; an
                        // undeclared/unknown var — `None` here — defaults
                        // to showing one anyway, same as a plain text field
                        // would, rather than silently showing none).
                        let var_type = self
                            .decls
                            .iter()
                            .find(|d| d.name == field.var)
                            .map(|d| d.var_type);
                        let cursor = focused
                            && !matches!(
                                var_type,
                                Some(meshfox_core::vars::VarType::Bool)
                                    | Some(meshfox_core::vars::VarType::Select)
                            );
                        let mut shown = if value.is_empty() {
                            " ".to_string()
                        } else {
                            value
                        };
                        if cursor {
                            shown.push('_');
                        }
                        let mut value_span = Span::styled(shown, value_style);
                        let mut label_span = Span::styled(prefix, label_style);
                        if focused {
                            label_span = label_span
                                .patch_style(Style::default().add_modifier(Modifier::REVERSED));
                            value_span =
                                value_span.patch_style(Style::default().add_modifier(if cursor {
                                    Modifier::REVERSED | Modifier::SLOW_BLINK
                                } else {
                                    Modifier::REVERSED
                                }));
                        }
                        lines.push(Line::from(vec![label_span, value_span]));
                        col_ends.push(col_end);
                    }
                    let send_line = format!("[{send_caption}]");
                    let send_col_end = send_line.chars().count() as u16;
                    let mut send_span = Span::styled(send_line, marker_style);
                    if focused_index == Some(fields.len()) {
                        send_span = send_span
                            .patch_style(Style::default().add_modifier(Modifier::REVERSED));
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
                    // `dep_lines`'s own click spans use — while the
                    // `(r to run)` hint after it (not itself clickable)
                    // stays plain.
                    let marker = Style::default()
                        .fg(super::theme::ACCENT)
                        .add_modifier(Modifier::BOLD | Modifier::UNDERLINED);
                    let hint = Style::default().fg(Color::DarkGray);
                    let mut lines = vec![Line::from(vec![
                        Span::styled("▶ ", marker),
                        Span::styled(caption.clone(), marker),
                        Span::styled("  (r to run)", hint),
                    ])];
                    let mut dep_clicks = Vec::new();
                    for (line, clicks) in self.dep_lines(
                        deps_raw.as_deref(),
                        env_raw.as_deref(),
                        interpreter.as_deref(),
                    ) {
                        dep_clicks.push((lines.len(), clicks));
                        lines.push(line);
                    }
                    lines.extend(details.into_iter().map(|detail| {
                        Line::from(Span::styled(
                            format!("  {detail}"),
                            Style::default().fg(super::theme::DEP),
                        ))
                    }));
                    self.push_segment(Segment::Text(lines));
                    self.push_dep_clicks(dep_clicks);
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
                let view = click_name
                    .as_ref()
                    .and_then(|n| self.block_views.get(n))
                    .cloned()
                    .unwrap_or_default();
                let block = click_name
                    .as_ref()
                    .and_then(|n| self.runnable.iter().find(|b| b.name.as_ref() == Some(n)));
                let source_expanded = view
                    .source_expanded
                    .unwrap_or(!block.is_some_and(|b| b.fold));
                let border = self.block_border(
                    click_name
                        .as_deref()
                        .or_else(|| self.output_region.as_ref().map(|r| r.name.as_str())),
                );
                let highlighted = if !view.collapsed && source_expanded {
                    self.hl.highlight(&lang, &code)
                } else {
                    Vec::new()
                };
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
                let mut controls = Vec::new();
                let mut header = if self.focused && click_name.is_some()
                    && click_name.as_deref() == self.selected_block.as_deref() {
                    "›─".to_string()
                } else { "┌─".to_string() };
                if click_name.is_some() {
                    let mut control = |text: &str, action| {
                        let start = Span::raw(header.as_str()).width() as u16;
                        header.push_str(text);
                        controls.push((start, Span::raw(header.as_str()).width() as u16, action));
                        header.push(' ');
                    };
                    control(
                        if view.collapsed { "[▸]" } else { "[▾]" },
                        BlockAction::ToggleBlock,
                    );
                    if !view.collapsed {
                        control(
                            if source_expanded {
                                "[code ▾]"
                            } else {
                                "[code ▸]"
                            },
                            BlockAction::ToggleSource,
                        );
                    }
                }
                header.push_str(&label);
                if let Some(block) = block {
                    for (text, action, show) in [
                        (" [history]", BlockAction::History, !block.service),
                        (" [run]", BlockAction::Run, true),
                        (" [chain]", BlockAction::Chain, !block.deps.is_empty()),
                    ] {
                        if show {
                            let start = Span::raw(header.as_str()).width() as u16;
                            header.push_str(text);
                            controls.push((
                                start,
                                Span::raw(header.as_str()).width() as u16,
                                action,
                            ));
                        }
                    }
                }
                let header_style = self.block_header(click_name.as_deref());
                let mut framed = vec![Line::from(Span::styled(header, header_style))];
                // Deps-line click regions (`dep_lines`'s own col-range half)
                // can't become real `ClickRegion`s until `framed` is
                // actually pushed as a segment below — only then is
                // `segment_index` known.
                let mut dep_clicks = Vec::new();
                if !view.collapsed {
                    for (dep_line, clicks) in self.dep_lines(
                        deps_raw.as_deref(),
                        env_raw.as_deref(),
                        interpreter.as_deref(),
                    ) {
                        dep_clicks.push((framed.len(), clicks));
                        framed.push(dep_line);
                    }
                    framed.extend(
                        details
                            .into_iter()
                            .map(|detail| Line::from(Span::styled(format!("├─ {detail}"), border))),
                    );
                    framed.extend(highlighted.into_iter().map(|l| {
                        let mut spans = vec![Span::styled("│ ", border)];
                        spans.extend(l.spans);
                        Line::from(spans)
                    }));
                    framed.push(Line::from(Span::styled("└─", border)));
                }

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
                            border,
                        ));
                        self.push_segment_plain(Segment::Text(framed));
                        self.push_dep_clicks(dep_clicks);
                        return;
                    }
                }
                self.push_segment(Segment::Text(framed));
                self.push_dep_clicks(dep_clicks);
                if let Some(block_name) = &click_name {
                    let segment_index = self.segments.len() - 1;
                    for (col_start, col_end, action) in controls {
                        self.click_regions.push(ClickRegion {
                            segment_index,
                            line_index: 0,
                            col_start,
                            col_end,
                            target: ClickTarget::Block {
                                node_id: self.node_id.to_owned(),
                                block_name: block_name.clone(),
                                action,
                            },
                        });
                    }
                    // Controls win hit-testing over the fallback selection region.
                    if let Segment::Text(lines) = &self.segments[segment_index] {
                        for (line_index, line) in lines.iter().enumerate() {
                            self.click_regions.push(ClickRegion {
                                segment_index,
                                line_index,
                                col_start: 0,
                                col_end: line.width().min(u16::MAX as usize) as u16,
                                target: ClickTarget::Block {
                                    node_id: self.node_id.to_owned(),
                                    block_name: block_name.clone(),
                                    action: BlockAction::Select,
                                },
                            });
                        }
                    }
                    if !view.collapsed {
                        if let Some(live) = self.live_output.get(block_name.as_str()) {
                            self.push_live_output(block_name, live);
                        }
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
                if let Some(text) = self.image_alt.take() {
                    if !text.is_empty() {
                        if let Some(Segment::Image { alt, .. }) = self.segments.last_mut() {
                            *alt = text;
                        }
                    }
                }
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

    // TODO.canvas.md: "TUI: инлайн live-вывод для output=\"markdown\" блоков
    // ... как plain text" — a finished `output="markdown"` block's live
    // stdout is rendered as real Markdown inside its frame; while still
    // running (or for a non-markdown block) it stays raw text.
    fn live_doc_text(live: super::super::app::StepOutput) -> String {
        let hl = Highlighter::new();
        let md = "```bash name=\"t\"\necho hi\n```\n";
        let mut live_output = std::collections::HashMap::new();
        live_output.insert("t".to_string(), live);
        let (segments, _clicks) = render(
            md,
            Path::new("/nonexistent-base-dir"),
            &hl,
            "n",
            &[],
            &std::collections::HashMap::new(),
            None,
            &live_output,
        );
        let mut out = String::new();
        for seg in segments {
            if let Segment::Text(lines) = seg {
                for line in lines {
                    for span in line.spans {
                        out.push_str(&span.content);
                    }
                    out.push('\n');
                }
            }
        }
        out
    }

    fn table_output(output_markdown: bool, running: bool) -> super::super::app::StepOutput {
        super::super::app::StepOutput {
            stdout: "| score | name |\n|---|---|\n| 1.0 | ZMARKERZ |\n".to_string(),
            stderr: "some warning\n".to_string(),
            output_markdown,
            exit_code: 0,
            duration_ms: 5,
            running,
        }
    }

    #[test]
    fn block_contract_rows_show_arguments_and_file_templates() {
        let hl = Highlighter::new();
        let md = concat!(
            "<!-- meshfox:arg name=\"lang\" type=\"select\" choices=\"en,hy\" default=\"en\" -->\n",
            "<!-- meshfox:arg name=\"n\" type=\"int\" default=\"09\" required -->\n",
            "```bash name=\"extract\" inputs=\"pdf_${lang}.pdf,index.txt\" outputs=\"csv_${lang}.csv\"\necho ok\n```\n",
        );
        let (segments, _) = render(
            md,
            Path::new("/nonexistent-base-dir"),
            &hl,
            "root",
            &[],
            &Default::default(),
            None,
            &Default::default(),
        );
        let text = segment_text(&segments);
        for expected in [
            "args: lang: select [en | hy] · default: \"en\"",
            "args: n: int · required · suggestion: \"09\"",
            "inputs: pdf_${lang}.pdf",
            "inputs: index.txt",
            "outputs: csv_${lang}.csv",
        ] {
            assert!(text.contains(expected), "missing {expected:?}: {text}");
        }
    }

    #[test]
    fn export_contract_matches_producer_nodes_and_shows_types_before_execution() {
        let md = "```bash name=\"observe\"\ntrue\n```\n";
        let canvas = meshfox_core::Canvas::from_markdown(concat!(
            "# Root\n<!-- meshfox:node id=\"root\" -->\n",
            "<!-- meshfox:var name=\"URL\" from=\"download/observe\" -->\n",
            "<!-- meshfox:var name=\"COUNT\" type=\"int\" from=\"download/observe\" -->\n",
            "<!-- meshfox:var name=\"OTHER\" from=\"other/observe\" -->\n",
            "<!-- meshfox:var name=\"CONFIG\" default=\"configured\" -->\n",
            "## Download\n<!-- meshfox:node id=\"download\" -->\n",
            "<!-- meshfox:var name=\"LOCAL\" type=\"bool\" from=\"observe\" -->\n",
        )).unwrap();
        let decls = meshfox_core::vars::declared_vars(&canvas).unwrap();
        let (segments, _) = render(md, Path::new("/nonexistent-base-dir"), &Highlighter::new(), "download", &decls, &Default::default(), None, &Default::default());
        let text = segment_text(&segments);
        for expected in ["exports: URL: string", "exports: COUNT: int", "exports: LOCAL: bool"] {
            assert!(text.contains(expected), "{text}");
        }
        assert!(!text.contains("exports: OTHER"), "{text}");
        assert!(!text.contains("exports: CONFIG"), "{text}");
    }

    #[test]
    fn env_contract_shows_selected_names_and_sources_for_each_application() {
        let md = "<!-- meshfox:arg name=\"lang\" -->\n```bash name=\"fetch\" env=\"URL=URL_${lang}\"\necho ok\n```\n";
        let decls = meshfox_core::vars::scan_var_decls(
            "<!-- meshfox:var name=\"URL_hy\" from=\"download/observe\" -->",
        )
        .unwrap();
        let output =
            std::collections::HashMap::from([("fetch[lang=hy]".into(), table_output(false, false))]);
        let (segments, _) = render(
            md,
            Path::new("/nonexistent-base-dir"),
            &Highlighter::new(),
            "root",
            &decls,
            &Default::default(),
            None,
            &output,
        );
        let text = segment_text(&segments);
        assert!(text.contains("env: URL ← URL_${lang}"), "{text}");
        assert!(
            text.contains("env: fetch[lang=hy]: URL_hy ← download/observe"),
            "{text}"
        );
    }

    #[test]
    fn finished_markdown_live_output_is_rendered_inside_its_frame() {
        let text = live_doc_text(table_output(true, false));
        assert!(text.contains("ZMARKERZ"), "cell text should show:\n{text}");
        assert!(
            !text.contains("|---|---|"),
            "raw table syntax should be rendered, not literal:\n{text}"
        );
        assert!(
            text.contains("output: t · live · markdown · done"),
            "frame header:\n{text}"
        );
        let table_line = text.lines().find(|l| l.contains("ZMARKERZ")).unwrap();
        assert!(
            table_line.starts_with("│ "),
            "rendered rows stay inside the frame border:\n{text}"
        );
        assert!(
            text.contains("│ some warning"),
            "stderr stays plain text in the frame:\n{text}"
        );
        // stderr sits above the rendered part, set off by a rule — not run
        // together with the last table row.
        let (warn, rule, table) = (
            text.find("some warning").unwrap(),
            text.find("│ ────").expect("a rule after stderr"),
            text.find("ZMARKERZ").unwrap(),
        );
        assert!(
            warn < rule && rule < table,
            "stderr, rule, then the rendered markdown:\n{text}"
        );
        assert!(text.trim_end().ends_with("└─"), "frame is closed:\n{text}");
    }

    // `output="image"`: a finished run's stdout is a raw SVG; the live frame
    // shows it as the same image line the cached copy has, with the fence's
    // `output-attrs=` applied — and plain text when it isn't one.
    fn image_live_segments(stdout: &str, exit_code: i32, running: bool) -> Vec<Segment> {
        let hl = Highlighter::new();
        let md = "```bash name=\"t\" cache output=\"image\" output-attrs=\"bg=#fff\"\nx\n```\n";
        let mut live_output = std::collections::HashMap::new();
        live_output.insert(
            "t".to_string(),
            super::super::app::StepOutput {
                stdout: stdout.to_string(),
                stderr: String::new(),
                output_markdown: false,
                exit_code,
                duration_ms: 5,
                running,
            },
        );
        render(
            md,
            Path::new("/nonexistent-base-dir"),
            &hl,
            "n",
            &[],
            &std::collections::HashMap::new(),
            None,
            &live_output,
        )
        .0
    }

    const LIVE_SVG: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" width="4" height="4"><rect width="4" height="4"/></svg>"#;

    #[test]
    fn finished_image_live_output_becomes_an_image_segment_with_its_bg() {
        let segments = image_live_segments(LIVE_SVG, 0, false);
        let image = segments.iter().find_map(|s| match s {
            Segment::Image { path, bg, .. } => Some((path.clone(), *bg)),
            _ => None,
        });
        let (path, bg) = image.expect("an image segment for the SVG");
        assert!(path
            .to_string_lossy()
            .starts_with("data:image/svg+xml;base64,"));
        assert_eq!(bg, meshfox_core::image_attrs::Background::parse("#fff"));
    }

    #[test]
    fn a_cached_image_region_is_drawn_as_an_image_with_its_attrs() {
        let hl = Highlighter::new();
        let region = meshfox_core::output::write_output(
            "# R\n<!-- meshfox:node id=\"r\" -->\n\n```bash name=\"t\" cache output=\"image\" output-attrs=\"width=50% bg=#fff\"\nx\n```\n",
            "t",
            &meshfox_core::output::ExecOutput {
                exit_code: 0,
                output: LIVE_SVG.to_string(),
                duration_ms: 1,
                stdout: LIVE_SVG.to_string(),
                stderr: "warn\n".to_string(),
            },
        )
        .unwrap();
        let (segments, _) = render(
            &region,
            Path::new("/nonexistent-base-dir"),
            &hl,
            "r",
            &[],
            &std::collections::HashMap::new(),
            None,
            &std::collections::HashMap::new(),
        );
        let image = segments.iter().find_map(|s| match s {
            Segment::Image {
                path,
                width_percent,
                bg,
                ..
            } => Some((path.clone(), *width_percent, *bg)),
            _ => None,
        });
        let (path, width, bg) = image.expect("the cached region's image");
        assert!(path
            .to_string_lossy()
            .starts_with("data:image/svg+xml;base64,"));
        assert_eq!(width, Some(50));
        assert_eq!(bg, meshfox_core::image_attrs::Background::parse("#fff"));
    }

    #[test]
    fn running_or_failed_image_live_output_stays_plain_text() {
        for (stdout, code, running) in [
            (LIVE_SVG, 0, true),
            (LIVE_SVG, 2, false),
            ("not an svg", 0, false),
        ] {
            let segments = image_live_segments(stdout, code, running);
            assert!(
                !segments.iter().any(|s| matches!(s, Segment::Image { .. })),
                "{stdout:?} {code} {running}"
            );
        }
    }

    #[test]
    fn runnable_examples_inside_live_markdown_output_have_no_run_controls() {
        let hl = Highlighter::new();
        let mut output = table_output(true, false);
        output.stdout = "```sh\nprintf example\n```\n".into();
        let live = std::collections::HashMap::from([("actual".into(), output)]);
        let (segments, _) = render(
            "```sh name=actual\ntrue\n```\n",
            Path::new("/nonexistent-base-dir"),
            &hl,
            "n",
            &[],
            &Default::default(),
            None,
            &live,
        );
        let text = segments
            .into_iter()
            .filter_map(|seg| match seg {
                Segment::Text(lines) => Some(
                    lines
                        .iter()
                        .map(Line::to_string)
                        .collect::<Vec<_>>()
                        .join("\n"),
                ),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("printf example"));
        assert_eq!(text.matches("[run]").count(), 1, "{text}");
    }

    // The web UI's `RunOutput` shows live output *instead of* the cached
    // copy in the file, never both — the TUI showed both (a fresh run's
    // frame, then the last-saved one's right below it), which read as a
    // duplicated, subtly different result.
    #[test]
    fn live_output_replaces_the_cached_output_region_of_the_same_block() {
        let hl = Highlighter::new();
        let md = concat!(
            "```bash name=\"t\" cache output=\"markdown\"\necho hi\n```\n",
            "<!-- meshfox:output name=\"t\" exit=\"0\" -->\n",
            "CACHEDMARKER\n",
            "<!-- /meshfox:output -->\n\n",
            "```bash name=\"u\" cache\necho other\n```\n",
            "<!-- meshfox:output name=\"u\" exit=\"0\" -->\n",
            "```text\nOTHERCACHED\n```\n",
            "<!-- /meshfox:output -->\n",
        );
        let mut live_output = std::collections::HashMap::new();
        live_output.insert("t".to_string(), table_output(true, false));
        let (segments, _clicks) = render(
            md,
            Path::new("/nonexistent-base-dir"),
            &hl,
            "n",
            &[],
            &std::collections::HashMap::new(),
            None,
            &live_output,
        );
        let mut text = String::new();
        for seg in segments {
            if let Segment::Text(lines) = seg {
                for line in lines {
                    for span in line.spans {
                        text.push_str(&span.content);
                    }
                    text.push('\n');
                }
            }
        }
        assert!(text.contains("ZMARKERZ"), "the live frame shows:\n{text}");
        assert!(
            !text.contains("CACHEDMARKER"),
            "the block's cached copy is superseded:\n{text}"
        );
        assert!(
            text.contains("OTHERCACHED"),
            "a block with no live output keeps its cached copy:\n{text}"
        );
    }

    // examples/pandas-dataframe.canvas.md prints its chart as a
    // `![..](data:image/png;base64,..)` in stdout — inside a live
    // `output="markdown"` frame that must still become a real image segment
    // (drawn by `app::load_image_protocol`), not literal text, with the
    // frame's text on either side of it kept.
    #[test]
    fn an_image_in_finished_markdown_live_output_becomes_an_image_segment() {
        let hl = Highlighter::new();
        let md = "```bash name=\"t\"\necho hi\n```\n";
        let mut live = table_output(true, false);
        live.stdout
            .push_str("\n![chart](data:image/png;base64,iVBORw0KGgo=)\n");
        let mut live_output = std::collections::HashMap::new();
        live_output.insert("t".to_string(), live);
        let (segments, _clicks) = render(
            md,
            Path::new("/nonexistent-base-dir"),
            &hl,
            "n",
            &[],
            &std::collections::HashMap::new(),
            None,
            &live_output,
        );
        let images: Vec<_> = segments
            .iter()
            .filter_map(|s| match s {
                Segment::Image { path, .. } => Some(path.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(
            images,
            vec![PathBuf::from("data:image/png;base64,iVBORw0KGgo=")]
        );
        let last_text = segments.iter().rev().find_map(|s| match s {
            Segment::Text(lines) => Some(lines.clone()),
            _ => None,
        });
        let last_text: String = last_text
            .unwrap()
            .iter()
            .flat_map(|l| l.spans.iter().map(|sp| sp.content.to_string()))
            .collect();
        assert!(
            last_text.contains("└─"),
            "the frame still closes after the image: {last_text}"
        );
    }

    #[test]
    fn markdown_live_output_stays_raw_while_the_run_is_still_going() {
        let text = live_doc_text(table_output(true, true));
        assert!(
            text.contains("|---|---|"),
            "raw text while running:\n{text}"
        );
    }

    #[test]
    fn non_markdown_live_output_stays_raw() {
        let text = live_doc_text(table_output(false, false));
        assert!(
            text.contains("|---|---|"),
            "no output=\"markdown\", so no rendering:\n{text}"
        );
    }

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
        let (segments, _clicks) = render(
            md,
            Path::new("/nonexistent-base-dir"),
            &hl,
            "n",
            &[],
            &std::collections::HashMap::new(),
            None,
            &std::collections::HashMap::new(),
        );
        let Segment::Image { path, alt, .. } = segments
            .into_iter()
            .find(|s| matches!(s, Segment::Image { .. }))
            .expect("a data: URL should produce a Segment::Image")
        else {
            unreachable!()
        };
        assert_eq!(path, PathBuf::from("data:image/png;base64,iVBORw0KGgo="));
        // The alt text belongs to the image (its load-failure fallback) —
        // it must not also leak into the document as a caption line.
        assert_eq!(alt, "a pixel");
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

        let (unfocused, _) = render(
            md,
            Path::new("/x"),
            &hl,
            "n",
            &decls,
            &values,
            None,
            &std::collections::HashMap::new(),
        );
        let (focused, _) = render(
            md,
            Path::new("/x"),
            &hl,
            "n",
            &decls,
            &values,
            Some(("f", 0)),
            &std::collections::HashMap::new(),
        );

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
            !unfocused_value
                .style
                .add_modifier
                .contains(Modifier::REVERSED),
            "an unfocused field row shouldn't be reversed"
        );
        assert_eq!(
            focused_value.content.as_ref(),
            "abc_",
            "the focused field's own value should carry a trailing text cursor"
        );
        assert!(
            focused_value
                .style
                .add_modifier
                .contains(Modifier::REVERSED),
            "the focused field's row should be reversed so it's visibly the one in focus"
        );
    }

    #[test]
    fn an_http_image_is_still_inert_text_not_a_segment_image() {
        let hl = Highlighter::new();
        let md = "![x](https://example.com/pic.png)\n";
        let (segments, _clicks) = render(
            md,
            Path::new("/nonexistent-base-dir"),
            &hl,
            "n",
            &[],
            &std::collections::HashMap::new(),
            None,
            &std::collections::HashMap::new(),
        );
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
        let (segments, _clicks) = render(
            md,
            Path::new("/nonexistent-base-dir"),
            &hl,
            "n",
            &[],
            &std::collections::HashMap::new(),
            None,
            &std::collections::HashMap::new(),
        );
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
        let (segments, _clicks) = render(
            md,
            Path::new("/nonexistent-base-dir"),
            &hl,
            "n",
            &[],
            &std::collections::HashMap::new(),
            None,
            &std::collections::HashMap::new(),
        );
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
    fn an_images_alt_text_is_not_rendered_as_a_caption_under_it() {
        let hl = Highlighter::new();
        let md = "![Temperature and humidity](pic.png)\n\nafter\n";
        let (segments, _clicks) = render(
            md,
            Path::new("/nonexistent-base-dir"),
            &hl,
            "n",
            &[],
            &std::collections::HashMap::new(),
            None,
            &std::collections::HashMap::new(),
        );
        let text = segment_text(&segments);
        assert!(
            !text.contains("Temperature"),
            "no stray alt-text paragraph: {text:?}"
        );
        assert!(
            text.contains("after"),
            "the real paragraph after the image stays: {text:?}"
        );
        let alt = segments
            .iter()
            .find_map(|s| match s {
                Segment::Image { alt, .. } => Some(alt.clone()),
                _ => None,
            })
            .expect("an image segment");
        assert_eq!(alt, "Temperature and humidity");
    }

    #[test]
    fn text_right_after_an_image_with_no_attrs_marker_is_rendered_normally() {
        let hl = Highlighter::new();
        let md = "![alt](pic.png) just text\n";
        let (segments, _clicks) = render(
            md,
            Path::new("/nonexistent-base-dir"),
            &hl,
            "n",
            &[],
            &std::collections::HashMap::new(),
            None,
            &std::collections::HashMap::new(),
        );
        assert!(segment_text(&segments).contains("just text"));
    }

    // TODO.canvas.md: same subtree -> "Подстрочный/надстрочный".
    #[test]
    fn subscript_and_superscript_render_as_unicode_small_forms() {
        let hl = Highlighter::new();
        let md = "H~2~O and x^n^\n";
        let (segments, _clicks) = render(
            md,
            Path::new("/nonexistent-base-dir"),
            &hl,
            "n",
            &[],
            &std::collections::HashMap::new(),
            None,
            &std::collections::HashMap::new(),
        );
        assert_eq!(segment_text(&segments), "H₂O and xⁿ");
    }

    #[test]
    fn subsup_falls_back_to_literal_when_not_fully_mapped_to_unicode() {
        let hl = Highlighter::new();
        let md = "x~query~\n";
        let (segments, _clicks) = render(
            md,
            Path::new("/nonexistent-base-dir"),
            &hl,
            "n",
            &[],
            &std::collections::HashMap::new(),
            None,
            &std::collections::HashMap::new(),
        );
        assert_eq!(segment_text(&segments), "x~query~");
    }

    #[test]
    fn subsup_never_applies_inside_a_code_block() {
        let hl = Highlighter::new();
        let md = "```text\nx~2~\n```\n";
        let (segments, _clicks) = render(
            md,
            Path::new("/nonexistent-base-dir"),
            &hl,
            "n",
            &[],
            &std::collections::HashMap::new(),
            None,
            &std::collections::HashMap::new(),
        );
        assert!(segment_text(&segments).contains("x~2~"));
    }

    // TODO.canvas.md: same subtree -> "Admonition/callout-блоки" (GFM
    // variant).
    #[test]
    fn a_gfm_alert_blockquote_gets_a_styled_title_line() {
        let hl = Highlighter::new();
        let md = "> [!WARNING]\n> be careful\n";
        let (segments, _clicks) = render(
            md,
            Path::new("/nonexistent-base-dir"),
            &hl,
            "n",
            &[],
            &std::collections::HashMap::new(),
            None,
            &std::collections::HashMap::new(),
        );
        let text = segment_text(&segments);
        assert!(text.contains("Warning"), "{text}");
        assert!(text.contains("be careful"), "{text}");
        assert!(!text.contains("[!WARNING]"), "{text}");
    }

    #[test]
    fn an_ordinary_blockquote_gets_no_title_line() {
        let hl = Highlighter::new();
        let md = "> just a quote\n";
        let (segments, _clicks) = render(
            md,
            Path::new("/nonexistent-base-dir"),
            &hl,
            "n",
            &[],
            &std::collections::HashMap::new(),
            None,
            &std::collections::HashMap::new(),
        );
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
        let (segments, _clicks) = render(
            md,
            Path::new("/nonexistent-base-dir"),
            &hl,
            "n",
            &[],
            &std::collections::HashMap::new(),
            None,
            &std::collections::HashMap::new(),
        );
        let text = segment_text(&segments);
        assert!(text.contains("[ ] todo"), "{text}");
        assert!(text.contains("[x] done"), "{text}");
    }

    #[test]
    fn a_numeric_footnote_reference_renders_as_unicode_superscript() {
        let hl = Highlighter::new();
        let md = "See[^1].\n\n[^1]: A note.\n";
        let (segments, _clicks) = render(
            md,
            Path::new("/nonexistent-base-dir"),
            &hl,
            "n",
            &[],
            &std::collections::HashMap::new(),
            None,
            &std::collections::HashMap::new(),
        );
        let text = segment_text(&segments);
        assert!(text.contains("See¹."), "{text}");
        assert!(!text.contains("[^1]"), "{text}");
    }

    #[test]
    fn a_footnote_definition_gets_a_bracketed_label_and_its_body() {
        let hl = Highlighter::new();
        let md = "See[^1].\n\n[^1]: A note.\n";
        let (segments, _clicks) = render(
            md,
            Path::new("/nonexistent-base-dir"),
            &hl,
            "n",
            &[],
            &std::collections::HashMap::new(),
            None,
            &std::collections::HashMap::new(),
        );
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
        let (segments, _clicks) = render(
            md,
            Path::new("/nonexistent-base-dir"),
            &hl,
            "n",
            &[],
            &std::collections::HashMap::new(),
            None,
            &std::collections::HashMap::new(),
        );
        let text = segment_text(&segments);
        assert!(text.contains("See[query]."), "{text}");
    }

    #[test]
    fn a_fences_own_interpreter_attr_shows_up_as_a_shebang_suffix_on_its_header() {
        let hl = Highlighter::new();
        let md = "```python name=\"seed\" interpreter=\"python3 -u\"\nprint(1)\n```\n";
        let (segments, _clicks) = render(
            md,
            Path::new("/nonexistent-base-dir"),
            &hl,
            "n",
            &[],
            &std::collections::HashMap::new(),
            None,
            &std::collections::HashMap::new(),
        );
        let text = segment_text(&segments);
        // Exactly as written in the attribute — no case-folding — mirrors
        // the web UI's own `mesh-code-interpreter` suffix.
        assert!(text.contains("python · seed #!python3 -u"), "{text}");
    }

    #[test]
    fn a_fence_with_no_interpreter_attr_has_no_shebang_suffix() {
        let hl = Highlighter::new();
        let md = "```bash name=\"build\" cache\necho hi\n```\n";
        let (segments, _clicks) = render(
            md,
            Path::new("/nonexistent-base-dir"),
            &hl,
            "n",
            &[],
            &std::collections::HashMap::new(),
            None,
            &std::collections::HashMap::new(),
        );
        let text = segment_text(&segments);
        assert!(!text.contains("#!"), "{text}");
    }

    #[test]
    fn a_button_fence_renders_its_body_as_the_caption_with_a_run_hint() {
        let hl = Highlighter::new();
        let md = "```button name=\"full-import\" deps=\"build\"\n🚀 Run everything\n```\n";
        let (segments, _clicks) = render(
            md,
            Path::new("/nonexistent-base-dir"),
            &hl,
            "n",
            &[],
            &std::collections::HashMap::new(),
            None,
            &std::collections::HashMap::new(),
        );
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
        let (segments, _clicks) = render(
            md,
            Path::new("/nonexistent-base-dir"),
            &hl,
            "n",
            &[],
            &std::collections::HashMap::new(),
            None,
            &std::collections::HashMap::new(),
        );
        let text = segment_text(&segments);
        assert!(text.contains("full-import"), "{text}");
    }

    #[test]
    fn a_button_fences_caption_is_rendered_as_a_bold_accent_marker() {
        let hl = Highlighter::new();
        let md = "```button name=\"full-import\" deps=\"build\"\nRun everything\n```\n";
        let (segments, _clicks) = render(
            md,
            Path::new("/nonexistent-base-dir"),
            &hl,
            "n",
            &[],
            &std::collections::HashMap::new(),
            None,
            &std::collections::HashMap::new(),
        );
        let Segment::Text(lines) = &segments[0] else {
            panic!("expected a text segment");
        };
        let caption_span = lines[0]
            .spans
            .iter()
            .find(|s| s.content.contains("Run everything"))
            .expect("caption span");
        assert_eq!(
            caption_span.style.bg, None,
            "no fill — see design choice above"
        );
        assert_eq!(caption_span.style.fg, Some(crate::tui::theme::ACCENT));
        assert!(caption_span.style.add_modifier.contains(Modifier::BOLD));

        let marker_span = lines[0]
            .spans
            .iter()
            .find(|s| s.content.contains('▶'))
            .expect("▶ marker span");
        assert_eq!(marker_span.style.fg, Some(crate::tui::theme::ACCENT));
    }
    #[test]
    fn dependency_rows_keep_distinct_click_positions_for_code_and_buttons() {
        let hl = Highlighter::new();
        for lang in ["bash", "button"] {
            let md = format!("```{lang} name=\"go\" deps=\"first/build,second/check\"\nRun\n```\n");
            let (segments, clicks) = render(
                &md, Path::new("/nonexistent-base-dir"), &hl, "n", &[],
                &std::collections::HashMap::new(), None, &std::collections::HashMap::new(),
            );
            let jumps: Vec<_> = clicks.iter().filter(|c| matches!(c.target, ClickTarget::JumpToNode { .. })).collect();
            assert_eq!(jumps.len(), 2);
            assert_ne!(jumps[0].line_index, jumps[1].line_index);
            for (click, label) in jumps.iter().zip(["first/build", "second/check"]) {
                let Segment::Text(lines) = &segments[click.segment_index] else { panic!("text expected") };
                let text: String = lines[click.line_index].spans.iter().map(|s| s.content.as_ref()).collect();
                let hit: String = text.chars().skip(click.col_start as usize).take((click.col_end - click.col_start) as usize).collect();
                assert_eq!(hit, label);
            }
            if lang == "button" {
                assert!(clicks.iter().any(|c| c.line_index == 0 && matches!(c.target, ClickTarget::RunBlock { .. })));
            }
        }
    }

}
