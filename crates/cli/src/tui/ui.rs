//! Rendering the TUI's three panes (tree / document / output) plus its
//! modal overlays (block picker, variable form, help).

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::Frame;
use ratatui_image::Image;
use std::sync::Arc;
use syntect::parsing::SyntaxSet;

use edtui::{EditorView, LineNumbers, SyntaxHighlighter};

use super::app::{App, Focus};
use super::markdown::Segment;
use super::source_editor::SourceEditorState;
use super::tree::TreeRow;
use crate::pdf::render::resolve_color_hex;
use meshfox_core::{NodeType, VarType};

/// Resolves a `meshfox:node`/`meshfox:edge` `color` attribute (a JSON-Canvas
/// preset `"1"`-`"6"` or a literal `#rrggbb` hex string — `resolve_color_hex`
/// handles the preset lookup, always returning a `#rrggbb` string either
/// way) to an actual terminal color for the tree pane. `None` for anything
/// that isn't a valid color — same "malformed input just renders with no
/// explicit color" fallback `resolve_color_hex`'s own callers already rely
/// on, rather than erroring.
fn tree_row_color(color: Option<&str>) -> Option<Color> {
    let hex = resolve_color_hex(color?)?;
    let hex = hex.strip_prefix('#')?;
    let r = u8::from_str_radix(hex.get(0..2)?, 16).ok()?;
    let g = u8::from_str_radix(hex.get(2..4)?, 16).ok()?;
    let b = u8::from_str_radix(hex.get(4..6)?, 16).ok()?;
    Some(Color::Rgb(r, g, b))
}

/// Everything a tree row's title line is made of, flattened into
/// individually-wrappable words, each carrying its own style — the title's
/// own words, then (if present) the constraint mark, each tag, and the
/// runnable/cache/tty badge, in that order. Keeping this as a flat word
/// list (rather than a handful of pre-joined strings) is what lets
/// `wrap_word_indices` below wrap the *whole* row — title, tags, and badge
/// alike — instead of only the title while silently clipping the rest.
fn tree_row_words(row: &TreeRow, title_style: Style) -> Vec<(String, Style)> {
    let mut words: Vec<(String, Style)> = row
        .title
        .split_whitespace()
        .map(|w| (w.to_string(), title_style))
        .collect();
    match row.constraint_ok {
        Some(true) => words.push(("✓".to_string(), Style::default().fg(Color::Green))),
        Some(false) => words.push(("✗".to_string(), Style::default().fg(Color::Red))),
        None => {}
    }
    for tag in &row.tags {
        words.push((format!("#{tag}"), Style::default().fg(Color::Cyan)));
    }
    let mut flags = Vec::new();
    if row.runnable_count > 0 {
        flags.push("run");
    }
    if row.has_cache {
        flags.push("cache");
    }
    if row.has_tty {
        flags.push("tty");
    }
    if !flags.is_empty() {
        words.push((
            format!("[{}]", flags.join(",")),
            Style::default().fg(Color::Green),
        ));
    }
    words
}

/// Greedy word-wrap over a row's own words (by index, so callers keep
/// ownership of the words themselves): packs as many as fit within
/// `first_width` on the first output row, `cont_width` on every row after
/// (the tree pane's continuation rows lose the disclosure/type-marker
/// prefix the first row has, so they get more room). A single word wider
/// than its own row's budget still gets a whole row to itself rather than
/// being split mid-word — same "don't break tokens" choice `ratatui`'s own
/// `WordWrapper` defaults to, and what keeps e.g. a lone long tag legible.
fn wrap_word_indices(word_widths: &[usize], first_width: usize, cont_width: usize) -> Vec<Vec<usize>> {
    if word_widths.is_empty() {
        return vec![Vec::new()];
    }
    let mut result: Vec<Vec<usize>> = Vec::new();
    let mut current: Vec<usize> = Vec::new();
    let mut current_width = 0usize;
    let mut budget = first_width.max(1);
    for (i, &w) in word_widths.iter().enumerate() {
        let needed = if current.is_empty() {
            w
        } else {
            current_width + 1 + w
        };
        if !current.is_empty() && needed > budget {
            result.push(current);
            current = Vec::new();
            current_width = 0;
            budget = cont_width.max(1);
        }
        current_width = if current.is_empty() {
            w
        } else {
            current_width + 1 + w
        };
        current.push(i);
    }
    result.push(current);
    result
}

/// Theme/language edtui's own bundled `syntect` highlighting uses for the
/// source editor — always Markdown, since that's what every file this
/// editor can open (a canvas or a plain-Markdown include target) actually
/// is. "dracula" is the theme edtui's own docs/examples default to; not
/// otherwise meaningful here.
const SOURCE_EDITOR_THEME: &str = "dracula";
/// Falls back to plain `"md"` (`find_syntax_by_token`, extension-based) only
/// if `crate::syntax_registry::MESHFOX_MARKDOWN_SYNTAX_NAME` somehow isn't
/// registered — never actually expected (it's bundled into the binary, see
/// `syntax_registry::build_syntax_set`), but resolving a language is one
/// `Option` chain either way, so there's no cost to not panicking on it.
const SOURCE_EDITOR_LANG: &str = "md";

const OUTPUT_HEIGHT: u16 = 9;
const FOOTER_HEIGHT: u16 = 1;

/// The three panes' rects for a given terminal size — computed once here
/// and shared by both rendering (`render`) and mouse hit-testing
/// (`app::App::on_mouse`), so the two can never drift apart.
pub struct PaneLayout {
    pub tree: Rect,
    pub document: Rect,
    pub output: Rect,
    pub footer: Rect,
}

pub fn compute_layout(area: Rect) -> PaneLayout {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(6),
            Constraint::Length(OUTPUT_HEIGHT),
            Constraint::Length(FOOTER_HEIGHT),
        ])
        .split(area);

    let main = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(30), Constraint::Percentage(70)])
        .split(chunks[0]);

    PaneLayout {
        tree: main[0],
        document: main[1],
        output: chunks[1],
        footer: chunks[2],
    }
}

pub fn render(f: &mut Frame, app: &mut App) {
    let area = f.area();

    // The source editor is a genuine full-terminal takeover, not an
    // overlay on top of the usual 3-pane layout — see `source_editor.rs`'s
    // own module docs.
    if let Some(se) = &mut app.source_editor {
        let syntax_set = Arc::clone(app.highlighter.syntax_set());
        render_source_editor(f, area, se, &syntax_set);
        return;
    }

    let layout = compute_layout(area);

    render_tree(f, layout.tree, app);
    render_document(f, layout.document, &*app);
    render_output(f, layout.output, &*app);
    render_footer(f, layout.footer, &*app);

    if let Some(bp) = &app.block_picker {
        render_block_picker(f, area, bp);
    } else if let Some(vf) = &app.var_form {
        render_var_form(f, area, vf);
    } else if app.reset_session_confirm {
        render_reset_session_confirm(f, area);
    } else if app.show_help {
        render_help(f, area, &*app);
    }
}

fn pane_border(focused: bool) -> Style {
    if focused {
        Style::default().fg(Color::Yellow)
    } else {
        Style::default()
    }
}

fn type_marker(t: NodeType) -> &'static str {
    match t {
        NodeType::Text => "",
        NodeType::File => "[file] ",
        NodeType::Link => "[link] ",
        NodeType::Group => "[group] ",
        NodeType::Include => "[include] ",
    }
}

fn render_tree(f: &mut Frame, area: Rect, app: &mut App) {
    // Borders on both sides of the list eat 2 columns of `area.width`.
    let content_width = area.width.saturating_sub(2) as usize;

    let items: Vec<ListItem> = app
        .rows
        .iter()
        .map(|row| {
            let indent = "  ".repeat(row.depth);
            let disclosure = if !row.has_children {
                "  "
            } else if row.expanded {
                "▾ "
            } else {
                "▸ "
            };
            let type_mark = type_marker(row.node_type);
            let title_style = match tree_row_color(row.color.as_deref()) {
                Some(c) => Style::default().fg(c),
                None => Style::default(),
            };

            let words = tree_row_words(row, title_style);
            let word_widths: Vec<usize> = words.iter().map(|(t, _)| t.chars().count()).collect();
            // Row 0 also carries the indent + disclosure marker + type
            // marker; continuation rows only re-indent by the disclosure
            // marker's own width, so wrapped text still lines up under it.
            let first_width = content_width
                .saturating_sub(indent.chars().count() + disclosure.chars().count() + type_mark.chars().count());
            let cont_width = content_width.saturating_sub(indent.chars().count() + disclosure.chars().count());
            let wrapped = wrap_word_indices(&word_widths, first_width, cont_width);

            let lines: Vec<Line> = wrapped
                .iter()
                .enumerate()
                .map(|(row_i, word_indices)| {
                    let mut spans = Vec::new();
                    if row_i == 0 {
                        spans.push(Span::raw(indent.clone()));
                        spans.push(Span::styled(disclosure, Style::default().fg(Color::DarkGray)));
                        spans.push(Span::styled(type_mark, Style::default().fg(Color::DarkGray)));
                    } else {
                        spans.push(Span::raw(format!("{indent}{}", " ".repeat(disclosure.chars().count()))));
                    }
                    for (i, &wi) in word_indices.iter().enumerate() {
                        if i > 0 {
                            spans.push(Span::raw(" "));
                        }
                        let (text, style) = &words[wi];
                        spans.push(Span::styled(text.clone(), *style));
                    }
                    Line::from(spans)
                })
                .collect();

            ListItem::new(Text::from(lines))
        })
        .collect();

    app.list_state.select(Some(app.selected));

    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(pane_border(app.focus == Focus::Tree))
                .title(format!(
                    " {} ",
                    app.canvas_path
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("canvas")
                )),
        )
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED));

    f.render_stateful_widget(list, area, &mut app.list_state);
}

/// How much of a `Segment::Text`'s already-wrapped `total` rows (see
/// `render_document` — always `Paragraph::line_count(width)`, the *actual*
/// wrapped row count, never the raw `lines.len()`) to draw this pass:
/// `row_offset` for `Paragraph::scroll` (applied post-wrap, so it can land
/// partway through one long logical line, not just before/after one) and
/// `height` for the rect, already clamped to whatever vertical room is
/// left in the pane.
struct TextLayout {
    row_offset: u16,
    height: u16,
}

/// `None` means this whole segment is above the current scroll position —
/// the caller should skip it, carrying `skip - total` forward as the
/// remaining skip for the next segment (mirrors `render_document`'s own
/// loop, which is why this doesn't return the reduced skip itself: the
/// caller already has `skip` in scope to subtract from directly).
fn wrapped_text_layout(total: u16, skip: u16, available: u16) -> Option<TextLayout> {
    if skip >= total {
        return None;
    }
    Some(TextLayout {
        row_offset: skip,
        height: (total - skip).min(available),
    })
}

fn render_document(f: &mut Frame, area: Rect, app: &App) {
    let title = app
        .rows
        .get(app.selected)
        .map(|r| r.title.as_str())
        .unwrap_or("");
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(pane_border(app.focus == Focus::Document))
        .title(format!(" {title} "));
    let inner = block.inner(area);
    f.render_widget(block, area);

    if inner.height == 0 || inner.width == 0 {
        return;
    }

    let mut y = inner.y;
    let bottom = inner.y + inner.height;
    let mut skip = app.doc_scroll;

    for seg in &app.doc_segments {
        if y >= bottom {
            break;
        }
        match seg {
            Segment::Text(lines) => {
                // A segment's own *rendered* (word-wrapped, `inner.width`)
                // row count can be well past `lines.len()` — a single
                // long logical `Line` (a wordy bullet point, say) wraps
                // into several screen rows on its own. Sizing this
                // segment's `rect`/advancing `y` by `lines.len()` instead
                // (as this used to) starves `Paragraph`'s own wrapping of
                // the rows it actually needs, silently clipping the rest —
                // and, since `y` then advances by too little, every
                // segment after it ends up drawn overlapping the tail of
                // this one instead of below it. `line_count` runs the
                // exact wrapping `Paragraph` itself will use to render
                // (unlike a hand-rolled `width()/inner.width` estimate,
                // which can disagree with it at word-boundary edge cases),
                // so measuring and rendering never disagree on a segment's
                // own height. `skip`/scrolling now also count wrapped
                // rows, via `Paragraph::scroll`'s own row offset (applied
                // post-wrap) rather than slicing `lines` itself, since a
                // partial skip can now land *inside* one long logical line
                // — not just before/after one, like it always could when
                // "line" and "row" were still the same thing.
                let paragraph = Paragraph::new(Text::from(lines.clone())).wrap(Wrap { trim: false });
                let total = paragraph.line_count(inner.width) as u16;
                let layout = match wrapped_text_layout(total, skip, bottom - y) {
                    None => {
                        skip -= total;
                        continue;
                    }
                    Some(l) => l,
                };
                skip = 0;
                let rect = Rect {
                    x: inner.x,
                    y,
                    width: inner.width,
                    height: layout.height,
                };
                f.render_widget(paragraph.scroll((layout.row_offset, 0)), rect);
                y += layout.height;
            }
            Segment::Image { path, alt, .. } => {
                let protocol = app.doc_images.get(path).and_then(|o| o.as_ref());
                let rows = protocol.map(|p| p.size().height).unwrap_or(1);
                if skip >= rows {
                    skip -= rows;
                    continue;
                }
                let height = rows.saturating_sub(skip).min(bottom - y);
                let rect = Rect {
                    x: inner.x,
                    y,
                    width: inner
                        .width
                        .min(protocol.map(|p| p.size().width).unwrap_or(inner.width)),
                    height,
                };
                match protocol {
                    Some(p) => f.render_widget(Image::new(p), rect),
                    None => f.render_widget(
                        Paragraph::new(format!("[image failed to load: {alt}]"))
                            .style(Style::default().fg(Color::Red)),
                        rect,
                    ),
                }
                skip = 0;
                y += height;
            }
        }
    }
}

fn render_output(f: &mut Frame, area: Rect, app: &App) {
    let title = if let Some(run) = &app.run {
        if run.proc.is_some() {
            " Output (running — K to kill) ".to_string()
        } else if run.killed {
            " Output (killed) ".to_string()
        } else if run.had_failure {
            " Output (failed) ".to_string()
        } else {
            " Output (done) ".to_string()
        }
    } else if let Some(run) = &app.file_run {
        if run.proc.is_some() {
            " Output (running — K to kill) ".to_string()
        } else if run.had_failure {
            " Output (failed) ".to_string()
        } else {
            " Output (done) ".to_string()
        }
    } else {
        " Output ".to_string()
    };
    let block = Block::default().borders(Borders::ALL).title(title);
    let inner = block.inner(area);
    f.render_widget(block, area);

    let lines: Option<&[String]> = if let Some(run) = &app.run {
        Some(&run.lines)
    } else {
        app.file_run.as_ref().map(|run| run.lines.as_slice())
    };
    let text: Text = if let Some(lines) = lines {
        let take = inner.height as usize;
        let start = lines.len().saturating_sub(take);
        Text::from(
            lines[start..]
                .iter()
                .map(|l| Line::from(l.as_str()))
                .collect::<Vec<_>>(),
        )
    } else if !app.status.is_empty() {
        Text::from(Line::from(Span::styled(
            app.status.as_str(),
            Style::default().fg(Color::Yellow),
        )))
    } else {
        Text::from(Line::from(Span::styled(
            "select a node and press r to run its block (R to run without deps)",
            Style::default().fg(Color::DarkGray),
        )))
    };
    f.render_widget(Paragraph::new(text), inner);
}

fn render_footer(f: &mut Frame, area: Rect, app: &App) {
    let mut hint = String::from(
        "tab focus · j/k move/scroll · enter expand · h/l collapse/expand · r run · R run (no deps) · K kill · e edit",
    );
    if app.selected_is_open_target() {
        hint.push_str(" · o open");
    }
    if app.has_configurable_vars() {
        hint.push_str(" · c configure");
    }
    hint.push_str(" · ? help · q quit");

    let mut spans = Vec::new();
    if let Some((total, failed)) = app.constraint_stats {
        let (text, color) = if failed > 0 {
            (format!("{failed}/{total} constraints failing"), Color::Red)
        } else {
            (
                format!(
                    "all {total} constraint{} pass",
                    if total == 1 { "" } else { "s" }
                ),
                Color::Green,
            )
        };
        spans.push(Span::styled(text, Style::default().fg(color)));
        spans.push(Span::styled("  ·  ", Style::default().fg(Color::DarkGray)));
    }
    spans.push(Span::styled(hint, Style::default().fg(Color::DarkGray)));

    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    let x = area.x + (area.width.saturating_sub(width)) / 2;
    let y = area.y + (area.height.saturating_sub(height)) / 2;
    Rect {
        x,
        y,
        width,
        height,
    }
}

/// Every field in `vf` at once, one per row — arrow keys/Tab move which
/// row is focused (highlighted), typing edits only that row's value, and
/// Enter submits every row's current value together, same "whole form at
/// once" shape as the web UI's `VarsForm` rather than one field at a time.
/// Keeps a growing text field's own cursor on screen — `List`/`Line`
/// (unlike edtui's own source editor, see `source_editor.rs`'s
/// `prime_viewport`) never wrap or scroll on their own, so a value once it
/// outgrew the row's width used to just push its trailing cursor marker
/// off the right edge of the terminal, with no way to bring it back into
/// view (TODO.canvas.md: "Горизонтальная промотка в TUI-редакторе").
/// Every field this renders only ever grows/shrinks from its own right end
/// (`push`/`pop` — see `App::on_key`), so the tail is always where the
/// action (and the cursor right after it) is — showing the last `max`
/// characters keeps that in view, at the cost of scrolling the *front* of
/// a long value out of sight instead; a leading "…" marks when that's
/// happened. `max` is a character count, not a byte count — matches every
/// other width calculation in this module (see e.g. `masked` above).
fn tail_fit(s: &str, max: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= max {
        return s.to_string();
    }
    if max <= 1 {
        return chars[chars.len() - max..].iter().collect();
    }
    let start = chars.len() - (max - 1);
    format!("…{}", chars[start..].iter().collect::<String>())
}

fn render_var_form(f: &mut Frame, area: Rect, vf: &super::app::VarFormState) {
    let height = (vf.decls.len() as u16 + 4).min(area.height);
    let rect = centered_rect(64, height, area);
    f.render_widget(Clear, rect);
    let title = if vf.configuring {
        " configure variables "
    } else {
        " variables needed "
    };
    let block = Block::default().borders(Borders::ALL).title(title);
    let inner = block.inner(rect);
    f.render_widget(block, rect);

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(inner);

    let items: Vec<ListItem> = vf
        .decls
        .iter()
        .zip(vf.inputs.iter())
        .enumerate()
        .map(|(i, (decl, input))| {
            let masked;
            let shown: &str = if decl.secret {
                masked = "*".repeat(input.chars().count());
                &masked
            } else {
                input.as_str()
            };
            // `Bool`/`Select` are a left/right toggle/cycle, not free text
            // (see `App::cycle_var_form_field`) — the `‹ ›` framing marks
            // that visually, on every such row, not just the focused one,
            // same way `VarsForm` renders them as a checkbox/dropdown
            // rather than a text input. `String`/`Int` keep the plain
            // text-cursor look, shown only on the focused row.
            let value = match decl.var_type {
                VarType::Bool | VarType::Select => format!("‹ {shown} ›"),
                VarType::String | VarType::Int => {
                    format!("{shown}{}", if i == vf.selected { "_" } else { "" })
                }
            };
            let prefix = format!("{}: ", decl.prompt);
            let value_budget = (inner.width as usize).saturating_sub(prefix.chars().count());
            let value = tail_fit(&value, value_budget);
            ListItem::new(Line::from(vec![
                Span::raw(prefix),
                Span::styled(value, Style::default().fg(Color::LightGreen)),
            ]))
        })
        .collect();

    let mut state = ListState::default();
    state.select(Some(vf.selected));
    let list = List::new(items).highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    f.render_stateful_widget(list, rows[0], &mut state);

    let esc_hint = if vf.configuring {
        "↑/↓/tab field · ←/→ toggle/cycle · enter save all · esc cancel configure"
    } else {
        "↑/↓/tab field · ←/→ toggle/cycle · enter confirm all · esc cancel run"
    };
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            esc_hint,
            Style::default().fg(Color::DarkGray),
        ))),
        rows[1],
    );
}

fn render_block_picker(f: &mut Frame, area: Rect, bp: &super::app::BlockPickerState) {
    let height = (bp.blocks.len() as u16 + 4).min(area.height);
    let rect = centered_rect(56, height, area);
    f.render_widget(Clear, rect);
    let mode = if bp.with_deps {
        "run (with deps)"
    } else {
        "run (no deps)"
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" {} — which block? ", mode));
    let inner = block.inner(rect);
    f.render_widget(block, rect);

    let items: Vec<ListItem> = bp
        .blocks
        .iter()
        .map(|b| {
            let mut flags = Vec::new();
            if b.is_default {
                flags.push("default");
            }
            if b.is_button {
                flags.push("button");
            }
            if b.cache {
                flags.push("cache");
            }
            if b.tty {
                flags.push("tty");
            }
            let badge = if flags.is_empty() {
                String::new()
            } else {
                format!("  [{}]", flags.join(","))
            };
            ListItem::new(Line::from(vec![
                Span::raw(b.name.clone()),
                Span::styled(badge, Style::default().fg(Color::Green)),
            ]))
        })
        .collect();

    let mut state = ListState::default();
    state.select(Some(bp.selected));
    let list = List::new(items).highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    f.render_stateful_widget(list, inner, &mut state);
}

/// The `S` (reset session) confirm prompt — see `App::reset_session_confirm`
/// and `App::on_reset_session_confirm_key`. Fixed size rather than
/// `render_block_picker`/`render_var_form`'s content-driven height (`bp
/// .blocks.len()`/`vf.decls.len()`) since this has no list to size around,
/// just the one static message.
fn render_reset_session_confirm(f: &mut Frame, area: Rect) {
    let rect = centered_rect(60, 8, area);
    f.render_widget(Clear, rect);
    let block = Block::default().borders(Borders::ALL).title(" reset session? ");
    let inner = block.inner(rect);
    f.render_widget(block, rect);

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(inner);

    let message = Paragraph::new(
        "Forgets which blocks already ran successfully this session, so the next chain run \
         re-runs every dependency instead of skipping unchanged ones. Doesn't touch the canvas \
         file or any saved output.",
    )
    .wrap(Wrap { trim: true });
    f.render_widget(message, rows[0]);

    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "y/enter confirm · n/esc cancel",
            Style::default().fg(Color::DarkGray),
        ))),
        rows[1],
    );
}

fn render_help(f: &mut Frame, area: Rect, app: &App) {
    let mut items = vec![
        "tab             switch focus: tree <-> document",
        "j / k / ↑ / ↓   move selection (tree) or scroll (document)",
        "enter           expand/collapse node",
        "l / →           expand node",
        "h / ←           collapse node, or jump to parent",
        "r               run this node's block, with its deps chain",
        "R               run this node's block only (skip deps)",
        "  (a node with more than one block opens a picker first)",
        "K               kill the running block",
        "S               reset session — forget which blocks already ran,",
        "                so the next chain run re-runs every dependency",
        "e               edit this node's own file, full-screen (Ctrl-s save,",
        "                Ctrl-f switch file, Ctrl-n heading->node, Ctrl-p",
        "                suggest attributes/tags, mouse click/drag/scroll, esc close)",
    ];
    if app.selected_is_open_target() {
        items.push("o               open this file node's target in the OS's default application");
    }
    if app.has_configurable_vars() {
        items.push(
            "c               configure every declared variable (see SPEC.md's \"Variables\")",
        );
    }
    items.extend([
        "PageUp/Down     scroll the document pane",
        "Ctrl-u / Ctrl-d scroll the document pane",
        "?               toggle this help",
        "q / esc         quit",
        "",
        "mouse: click a tree row to select it, or its ▾/▸ marker to",
        "expand/collapse; scroll wheel over the tree moves selection,",
        "over the document scrolls it",
        "",
        "running a `tty` block hands the real terminal over to it,",
        "same as `meshfox run` — this UI reappears once it exits",
    ]);

    let rect = centered_rect(62, items.len() as u16 + 2, area);
    f.render_widget(Clear, rect);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" keybindings ");
    let inner = block.inner(rect);
    f.render_widget(block, rect);

    let lines = items.into_iter().map(Line::from).collect::<Vec<_>>();
    f.render_widget(Paragraph::new(lines), inner);
}

/// The fullscreen source editor (`e`) — header (which file, dirty state),
/// the `edtui` buffer itself, and a footer (error, or the keybinding
/// hint) — plus the file-switcher (`Ctrl-f`) as a `render_block_picker`-
/// style overlay on top when open.
fn render_source_editor(f: &mut Frame, area: Rect, se: &mut SourceEditorState, syntax_set: &Arc<SyntaxSet>) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(area);

    let kind = if se.is_canvas {
        "canvas"
    } else {
        "plain markdown"
    };
    let dirty = if se.dirty() { " [modified]" } else { "" };
    let header = Line::from(vec![
        Span::styled(
            format!(" {} ", se.path.display()),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("({kind}){dirty}"),
            Style::default().fg(Color::DarkGray),
        ),
    ]);
    f.render_widget(Paragraph::new(header), chunks[0]);

    // `with_sets` (not `new`, which would load its own separate, defaults-
    // only SyntaxSet) so this editor sees the exact same custom grammars as
    // the read-only preview pane — see `crate::syntax_registry`.
    let syntax_highlighter = syntax_set
        .find_syntax_by_name(crate::syntax_registry::MESHFOX_MARKDOWN_SYNTAX_NAME)
        .or_else(|| syntax_set.find_syntax_by_token(SOURCE_EDITOR_LANG))
        .cloned()
        .map(|syntax_ref| {
            let theme_set = syntect::highlighting::ThemeSet::load_defaults();
            let theme = theme_set.themes.get(SOURCE_EDITOR_THEME).cloned();
            let theme = theme.or_else(|| theme_set.themes.values().next().cloned());
            let theme = theme.expect("syntect ships at least one theme");
            // Bundled `syntect` themes have no rules at all for meshfox's
            // own scope names — without this, a `<!-- meshfox:... -->`
            // marker's keyword/attribute-name/attribute-value would all
            // render in one plain color, same as any other comment text.
            let theme = crate::syntax_registry::with_meshfox_scope_colors(theme);
            SyntaxHighlighter::with_sets(theme, Arc::new(theme_set), syntax_ref, Arc::clone(syntax_set))
        });
    let view = EditorView::new(&mut se.editor)
        .line_numbers(LineNumbers::Absolute)
        .wrap(true);
    let view = match syntax_highlighter {
        Some(h) => view.syntax_highlighter(Some(h)),
        None => view,
    };
    f.render_widget(view, chunks[1]);

    let footer = match &se.error {
        Some(msg) => Line::from(Span::styled(msg.as_str(), Style::default().fg(Color::Red))),
        None => Line::from(Span::styled(
            "Ctrl-s save · Ctrl-f switch file · Ctrl-n heading→node · Ctrl-p suggest params · \
             esc close (vim keys inside the buffer)",
            Style::default().fg(Color::DarkGray),
        )),
    };
    f.render_widget(Paragraph::new(footer), chunks[2]);

    if se.file_picker_open {
        render_source_file_picker(f, area, se);
    }
    if se.attr_suggest_open {
        render_attr_suggest_popup(f, area, se);
    }
    if se.tag_suggest_open {
        render_tag_suggest_popup(f, area, se);
    }
}

/// TODO.canvas.md: "Саджесты и подсветка синтаксиса в TUI", item 2 —
/// `Ctrl-p`'s popup, same floating-`List`-over-`Clear` shape as
/// `render_source_file_picker` right above. Each row shows the attribute
/// name with a trailing `=` for a value-taking one (nothing for a bare
/// fence flag — see `AttrCandidate::is_flag`), so the list itself hints at
/// what selecting it will actually type.
fn render_attr_suggest_popup(f: &mut Frame, area: Rect, se: &SourceEditorState) {
    let height = (se.attr_suggest_candidates.len() as u16 + 2).min(area.height);
    let rect = centered_rect(36, height, area);
    f.render_widget(Clear, rect);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" {} attribute ", se.attr_suggest_label()));
    let inner = block.inner(rect);
    f.render_widget(block, rect);

    let items: Vec<ListItem> = se
        .attr_suggest_candidates
        .iter()
        .map(|c| {
            let suffix = if c.is_flag { "" } else { "=" };
            ListItem::new(Line::from(format!("{}{suffix}", c.name)))
        })
        .collect();

    let mut state = ListState::default();
    state.select(Some(se.attr_suggest_selected));
    let list = List::new(items).highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    f.render_stateful_widget(list, inner, &mut state);
}

/// TODO.canvas.md: "Саджест по тегам в TUI" — `Ctrl-p`'s other popup
/// shape, shown instead of `render_attr_suggest_popup` when the cursor was
/// inside a `tags="..."` value (see `SourceEditorState::open_attr_suggest`).
/// Same floating-`List`-over-`Clear` layout, candidates are already plain
/// tag strings so there's no per-item suffix logic to branch on.
fn render_tag_suggest_popup(f: &mut Frame, area: Rect, se: &SourceEditorState) {
    let height = (se.tag_suggest_candidates.len() as u16 + 2).min(area.height);
    let rect = centered_rect(36, height, area);
    f.render_widget(Clear, rect);
    let block = Block::default().borders(Borders::ALL).title(" tag ");
    let inner = block.inner(rect);
    f.render_widget(block, rect);

    let items: Vec<ListItem> = se
        .tag_suggest_candidates
        .iter()
        .map(|t| ListItem::new(Line::from(format!("#{t}"))))
        .collect();

    let mut state = ListState::default();
    state.select(Some(se.tag_suggest_selected));
    let list = List::new(items).highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    f.render_stateful_widget(list, inner, &mut state);
}

fn render_source_file_picker(f: &mut Frame, area: Rect, se: &SourceEditorState) {
    let count = se.files.len() + 1; // +1 for "this document"
    let height = (count as u16 + 2).min(area.height);
    let rect = centered_rect(64, height, area);
    f.render_widget(Clear, rect);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" switch file ");
    let inner = block.inner(rect);
    f.render_widget(block, rect);

    let mut items = vec![ListItem::new(Line::from("this document"))];
    items.extend(se.files.iter().map(|inc| {
        let indent = "  ".repeat(inc.depth as usize);
        ListItem::new(Line::from(format!(
            "{indent}↳ {} ({})",
            inc.title, inc.target
        )))
    }));

    let mut state = ListState::default();
    state.select(Some(se.file_picker_selected));
    let list = List::new(items).highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    f.render_stateful_widget(list, inner, &mut state);
}

#[cfg(test)]
mod tests {
    use super::*;

    // A real bug, found live: the document pane used to size a
    // `Segment::Text`'s rect (and advance `y`) by its raw `lines.len()`
    // rather than how many rows it actually needs once word-wrapped to
    // the pane's width — a single long bullet point (common in prose,
    // rare in this codebase's own short example bodies, which is why this
    // went unnoticed) wraps into several screen rows but was only ever
    // given one, so `Paragraph`'s own wrapping silently clipped the rest,
    // and every segment after it drew overlapping the tail of this one
    // instead of below it (reported against README.md's own new "Source
    // editor keybindings" node, whose long bullets tripped this exactly).
    #[test]
    fn wrapped_text_layout_renders_a_whole_segment_when_it_fits() {
        let layout = wrapped_text_layout(3, 0, 10).expect("segment is below the scroll offset");
        assert_eq!(layout.row_offset, 0);
        assert_eq!(layout.height, 3);
    }

    #[test]
    fn wrapped_text_layout_skips_a_whole_segment_entirely_scrolled_past() {
        assert!(wrapped_text_layout(3, 5, 10).is_none());
    }

    #[test]
    fn wrapped_text_layout_can_land_partway_through_a_long_wrapped_line() {
        // `skip` (2) is less than `total` (5) — this used to only be
        // reachable at all between whole *logical* lines; now that a
        // segment's "rows" are wrapped rows, a scroll offset can land
        // inside what's still just one long `Line` underneath.
        let layout = wrapped_text_layout(5, 2, 10).unwrap();
        assert_eq!(layout.row_offset, 2);
        assert_eq!(layout.height, 3);
    }

    #[test]
    fn wrapped_text_layout_clamps_height_to_the_room_left_in_the_pane() {
        let layout = wrapped_text_layout(5, 0, 2).unwrap();
        assert_eq!(layout.height, 2);
    }

    #[test]
    fn a_long_wrapped_line_is_not_clipped_when_measured_with_line_count() {
        use ratatui::buffer::Buffer;
        use ratatui::widgets::Widget;

        let long_line = "word ".repeat(40); // wraps to several rows at width 20
        let paragraph =
            Paragraph::new(Text::from(vec![Line::from(long_line.trim().to_string())]))
                .wrap(Wrap { trim: false });
        let width = 20u16;
        let total = paragraph.line_count(width) as u16;
        assert!(total > 1, "this line should need more than one wrapped row");

        let area = Rect::new(0, 0, width, total);
        let mut buf = Buffer::empty(area);
        paragraph.render(area, &mut buf);

        // The very last wrapped row (what a too-short rect, sized by
        // `lines.len()` instead of `line_count`, used to cut off) must
        // still have real content in it, not be left blank.
        let last_row: String = (0..width)
            .map(|x| {
                buf.cell((x, total - 1))
                    .and_then(|c| c.symbol().chars().next())
                    .unwrap_or(' ')
            })
            .collect();
        assert!(
            !last_row.trim().is_empty(),
            "the last wrapped row should still have visible text, got {last_row:?}"
        );
    }

    // Same bug, end to end through `render_document` itself this time
    // (the tests above cover the two pieces it's built from in isolation)
    // — a real `App` over a real canvas file whose root body has one long
    // bullet plus a short line right after it, rendered through a real
    // `Terminal`/`TestBackend`. Both the long line's own tail and the
    // short line after it must actually appear somewhere on screen — the
    // pre-fix version clipped the former and drew the latter overlapping
    // the former's own last (visible) row instead of below it.
    #[test]
    fn render_document_does_not_clip_a_long_wrapped_line_or_the_segment_after_it() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let dir = std::env::temp_dir().join(format!(
            "meshfox-render-document-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("canvas.canvas.md");
        // Distinct, numbered tokens rather than one repeated word — a
        // repeated word's own tail ("...docious") would still show up even
        // if every repetition past the first got clipped, since it's a
        // substring of the *first* one too; numbered tokens can't lie
        // about which repetitions actually survived.
        let long_line: String = (1..=8).map(|n| format!("TOKEN{n:02}")).collect::<Vec<_>>().join(" ");
        std::fs::write(
            &path,
            format!(
                "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n\n{long_line}\n\nTAIL-MARKER-LINE\n"
            ),
        )
        .unwrap();

        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let app = App::new(path, tx).expect("valid test canvas");

        let backend = TestBackend::new(20, 20);
        let mut terminal = Terminal::new(backend).unwrap();
        let area = Rect::new(0, 0, 20, 20);
        terminal.draw(|f| render_document(f, area, &app)).unwrap();

        let buf = terminal.backend().buffer();
        let mut screen = String::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                if let Some(cell) = buf.cell((x, y)) {
                    screen.push_str(cell.symbol());
                }
            }
            screen.push('\n');
        }
        assert!(
            screen.contains("TOKEN08"),
            "the long line's own last wrapped row should still be on screen:\n{screen}"
        );
        assert!(
            screen.contains("TAIL-MARKER-LINE"),
            "the short segment after the long line should be on screen, not hidden behind it:\n{screen}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    // TODO.canvas.md: "Tags in TUI" — end to end through `render_tree`
    // itself, over a real `App`/canvas, mirroring the `render_document`
    // integration test above.
    #[test]
    fn render_tree_shows_a_nodes_tags_next_to_its_title() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let dir = std::env::temp_dir().join(format!(
            "meshfox-render-tree-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("canvas.canvas.md");
        std::fs::write(
            &path,
            "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" tags=\"bag,improvement\" -->\n",
        )
        .unwrap();

        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(path, tx).expect("valid test canvas");

        let backend = TestBackend::new(40, 10);
        let mut terminal = Terminal::new(backend).unwrap();
        let area = Rect::new(0, 0, 40, 10);
        terminal.draw(|f| render_tree(f, area, &mut app)).unwrap();

        let buf = terminal.backend().buffer();
        let mut screen = String::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                if let Some(cell) = buf.cell((x, y)) {
                    screen.push_str(cell.symbol());
                }
            }
            screen.push('\n');
        }
        assert!(
            screen.contains("#bag #improvement"),
            "the node's tags should show up next to its title:\n{screen}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    // TODO.canvas.md: "Переносы строк в дереве нод в TUI" — end to end
    // through `render_tree` itself: a node with a long title, tags, and a
    // runnable+cache badge, in a pane too narrow for any of it to fit on
    // one row. Before wrapping, `List`'s own no-wrap rendering would just
    // clip everything past the pane's width — this asserts the title's
    // own tail, every tag, and the badge all still show up somewhere on
    // screen (on wrapped continuation rows), not silently dropped.
    #[test]
    fn render_tree_wraps_a_long_title_instead_of_clipping_it_or_its_tags_and_badge() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let dir = std::env::temp_dir().join(format!(
            "meshfox-render-tree-wrap-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("canvas.canvas.md");
        std::fs::write(
            &path,
            "<!-- meshfox:canvas -->\n\
             # ALFA BRAVO CHARLIE DELTA ECHO\n\
             <!-- meshfox:node id=\"root\" tags=\"bag,improvement\" -->\n\
             \n\
             ```bash name=\"build\" cache\n\
             cargo build\n\
             ```\n",
        )
        .unwrap();

        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(path, tx).expect("valid test canvas");

        // Narrow enough that "ALFA BRAVO CHARLIE DELTA ECHO  #bag #improvement  [run,cache]"
        // (73+ chars) cannot possibly fit on one row.
        let backend = TestBackend::new(20, 10);
        let mut terminal = Terminal::new(backend).unwrap();
        let area = Rect::new(0, 0, 20, 10);
        terminal.draw(|f| render_tree(f, area, &mut app)).unwrap();

        let buf = terminal.backend().buffer();
        let mut screen = String::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                if let Some(cell) = buf.cell((x, y)) {
                    screen.push_str(cell.symbol());
                }
            }
            screen.push('\n');
        }
        for needle in ["ALFA", "ECHO", "#bag", "#improvement", "[run,cache]"] {
            assert!(
                screen.contains(needle),
                "{needle:?} should still be visible somewhere on screen, wrapped rather than clipped:\n{screen}"
            );
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    // TODO.canvas.md: "Цвета в TUI" — a node's own `color` (a JSON-Canvas
    // preset "1"-"6" or a literal `#rrggbb` hex) should color its title in
    // the tree pane, same palette the web UI and the PDF export already
    // use (`resolve_color_hex`).
    #[test]
    fn tree_row_color_resolves_a_numbered_preset() {
        assert_eq!(tree_row_color(Some("1")), Some(Color::Rgb(0xc2, 0x2b, 0x2b)));
        assert_eq!(tree_row_color(Some("4")), Some(Color::Rgb(0x3d, 0x9e, 0x4f)));
    }

    #[test]
    fn tree_row_color_resolves_a_literal_hex_string() {
        assert_eq!(tree_row_color(Some("#a05dd1")), Some(Color::Rgb(0xa0, 0x5d, 0xd1)));
    }

    #[test]
    fn tree_row_color_is_none_for_unset_or_malformed_input() {
        assert_eq!(tree_row_color(None), None);
        assert_eq!(tree_row_color(Some("not-a-color")), None);
        assert_eq!(tree_row_color(Some("#zzzzzz")), None);
    }

    // TODO.canvas.md: "Переносы строк в дереве нод в TUI" — a title (plus
    // its tags/badge) too long for the pane should wrap onto more rows,
    // not get silently clipped by `List`'s own no-wrap rendering.
    #[test]
    fn wrap_word_indices_packs_everything_on_one_row_when_it_fits() {
        let widths = [4, 2, 5]; // e.g. "Root", "✓", "#bag" — 4+1+2+1+5 = 13
        assert_eq!(wrap_word_indices(&widths, 20, 20), vec![vec![0, 1, 2]]);
    }

    #[test]
    fn wrap_word_indices_wraps_to_a_new_row_when_the_budget_is_exceeded() {
        // First row (narrow: room for one 4-wide word only) gets just the
        // title; both remaining words then fit together on the wider
        // continuation row.
        let widths = [4, 3, 3];
        assert_eq!(wrap_word_indices(&widths, 4, 10), vec![vec![0], vec![1, 2]]);
    }

    #[test]
    fn wrap_word_indices_uses_the_narrower_continuation_budget_after_the_first_row() {
        // First row has less room (disclosure/type marker prefix); second
        // word alone fits the wider first budget but not appended after
        // the first word, so it starts row 2 — which then has *more* room
        // than row 1, fitting the third word too.
        let widths = [3, 3, 3];
        assert_eq!(wrap_word_indices(&widths, 3, 10), vec![vec![0], vec![1, 2]]);
    }

    #[test]
    fn wrap_word_indices_gives_an_oversized_single_word_its_own_row_without_splitting_it() {
        let widths = [20];
        assert_eq!(wrap_word_indices(&widths, 5, 5), vec![vec![0]]);
    }

    #[test]
    fn wrap_word_indices_of_no_words_is_a_single_empty_row() {
        assert_eq!(wrap_word_indices(&[], 20, 20), vec![Vec::<usize>::new()]);
    }

    #[test]
    fn tail_fit_leaves_a_short_value_untouched() {
        assert_eq!(tail_fit("hello", 10), "hello");
        assert_eq!(tail_fit("hello", 5), "hello");
    }

    #[test]
    fn tail_fit_truncates_from_the_front_with_a_leading_ellipsis() {
        assert_eq!(tail_fit("hello world", 5), "…orld");
        assert_eq!(tail_fit("hello world", 1), "d");
        assert_eq!(tail_fit("hello world", 0), "");
    }

    // TODO.canvas.md: "Горизонтальная промотка в TUI-редакторе" — typing a
    // value long enough to outgrow its row used to just push the trailing
    // cursor marker (`render_var_form`'s `"{shown}_"`) off the right edge
    // of the terminal for good: a plain `List`/`Line` never wraps or
    // scrolls on its own the way edtui's source editor does. Simulates the
    // actual interaction — pushing one character at a time, exactly like
    // `App::on_key`'s `vf.inputs[i].push(c)` — rather than just checking
    // one fixed long string, since the bug was specifically about a value
    // that *grows past* the visible width mid-typing, not one that already
    // starts too long.
    #[test]
    fn typing_past_the_row_width_keeps_the_cursor_marker_visible() {
        let budget = 10;
        let mut value = String::new();
        for ch in "this is a lot longer than the row".chars() {
            value.push(ch);
            let shown = format!("{value}_"); // mirrors render_var_form's own "{shown}{cursor}"
            let fitted = tail_fit(&shown, budget);
            assert!(
                fitted.chars().count() <= budget,
                "fitted value {fitted:?} exceeds the {budget}-char budget"
            );
            assert!(
                fitted.ends_with('_'),
                "cursor marker fell out of view while typing {value:?}, got {fitted:?}"
            );
        }
    }
}
