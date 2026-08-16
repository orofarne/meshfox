//! Rendering the TUI's three panes (tree / document / output) plus its
//! modal overlays (block picker, variable form, help).

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::Frame;
use ratatui_image::Image;

use edtui::{EditorView, LineNumbers, SyntaxHighlighter};

use super::app::{App, Focus};
use super::markdown::Segment;
use super::source_editor::SourceEditorState;
use meshfox_core::{NodeType, VarType};

/// Theme/language edtui's own bundled `syntect` highlighting uses for the
/// source editor — always Markdown, since that's what every file this
/// editor can open (a canvas or a plain-Markdown include target) actually
/// is. "dracula" is the theme edtui's own docs/examples default to; not
/// otherwise meaningful here.
const SOURCE_EDITOR_THEME: &str = "dracula";
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
        render_source_editor(f, area, se);
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
            let badge = if flags.is_empty() {
                String::new()
            } else {
                format!("  [{}]", flags.join(","))
            };
            let constraint_mark = match row.constraint_ok {
                Some(true) => Span::styled("  ✓", Style::default().fg(Color::Green)),
                Some(false) => Span::styled("  ✗", Style::default().fg(Color::Red)),
                None => Span::raw(""),
            };
            let line = Line::from(vec![
                Span::raw(indent),
                Span::styled(disclosure, Style::default().fg(Color::DarkGray)),
                Span::styled(
                    type_marker(row.node_type),
                    Style::default().fg(Color::DarkGray),
                ),
                Span::raw(row.title.clone()),
                constraint_mark,
                Span::styled(badge, Style::default().fg(Color::Green)),
            ]);
            ListItem::new(line)
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
                let take = lines.len() as u16;
                if skip >= take {
                    skip -= take;
                    continue;
                }
                let visible = &lines[skip as usize..];
                skip = 0;
                let height = (visible.len() as u16).min(bottom - y);
                let rect = Rect {
                    x: inner.x,
                    y,
                    width: inner.width,
                    height,
                };
                let text = Text::from(visible.to_vec());
                f.render_widget(Paragraph::new(text).wrap(Wrap { trim: false }), rect);
                y += height;
            }
            Segment::Image { path, alt } => {
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
    let title = match &app.run {
        Some(run) if run.proc.is_some() => " Output (running — K to kill) ".to_string(),
        Some(run) if run.killed => " Output (killed) ".to_string(),
        Some(run) if run.had_failure => " Output (failed) ".to_string(),
        Some(_) => " Output (done) ".to_string(),
        None => " Output ".to_string(),
    };
    let block = Block::default().borders(Borders::ALL).title(title);
    let inner = block.inner(area);
    f.render_widget(block, area);

    let text: Text = if let Some(run) = &app.run {
        let take = inner.height as usize;
        let start = run.lines.len().saturating_sub(take);
        Text::from(
            run.lines[start..]
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
            ListItem::new(Line::from(vec![
                Span::raw(format!("{}: ", decl.prompt)),
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
        "e               edit this node's own file, full-screen (Ctrl-s save,",
        "                Ctrl-f switch file, esc close)",
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
fn render_source_editor(f: &mut Frame, area: Rect, se: &mut SourceEditorState) {
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

    let syntax_highlighter = SyntaxHighlighter::new(SOURCE_EDITOR_THEME, SOURCE_EDITOR_LANG).ok();
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
            "Ctrl-s save · Ctrl-f switch file · esc close (vim keys inside the buffer)",
            Style::default().fg(Color::DarkGray),
        )),
    };
    f.render_widget(Paragraph::new(footer), chunks[2]);

    if se.file_picker_open {
        render_source_file_picker(f, area, se);
    }
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
