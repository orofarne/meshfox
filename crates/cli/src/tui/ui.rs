//! Rendering the TUI's three panes (tree / document / output) plus its
//! modal overlays (block picker, variable prompt, help).

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::Frame;
use ratatui_image::Image;

use super::app::{App, Focus};
use super::markdown::Segment;
use meshfox_core::NodeType;

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

    PaneLayout { tree: main[0], document: main[1], output: chunks[1], footer: chunks[2] }
}

pub fn render(f: &mut Frame, app: &mut App) {
    let area = f.area();
    let layout = compute_layout(area);

    render_tree(f, layout.tree, app);
    render_document(f, layout.document, &*app);
    render_output(f, layout.output, &*app);
    render_footer(f, layout.footer);

    if let Some(bp) = &app.block_picker {
        render_block_picker(f, area, bp);
    } else if let Some(vp) = &app.var_prompt {
        render_var_prompt(f, area, vp);
    } else if app.show_help {
        render_help(f, area);
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
        NodeType::Constraint => "[constraint] ",
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
            let badge = if flags.is_empty() { String::new() } else { format!("  [{}]", flags.join(",")) };
            let line = Line::from(vec![
                Span::raw(indent),
                Span::styled(disclosure, Style::default().fg(Color::DarkGray)),
                Span::styled(type_marker(row.node_type), Style::default().fg(Color::DarkGray)),
                Span::raw(row.title.clone()),
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
                    app.canvas_path.file_name().and_then(|n| n.to_str()).unwrap_or("canvas")
                )),
        )
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED));

    f.render_stateful_widget(list, area, &mut app.list_state);
}

fn render_document(f: &mut Frame, area: Rect, app: &App) {
    let title = app.rows.get(app.selected).map(|r| r.title.as_str()).unwrap_or("");
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
                let rect = Rect { x: inner.x, y, width: inner.width, height };
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
                let rect = Rect { x: inner.x, y, width: inner.width.min(protocol.map(|p| p.size().width).unwrap_or(inner.width)), height };
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
        Text::from(run.lines[start..].iter().map(|l| Line::from(l.as_str())).collect::<Vec<_>>())
    } else if !app.status.is_empty() {
        Text::from(Line::from(Span::styled(app.status.as_str(), Style::default().fg(Color::Yellow))))
    } else {
        Text::from(Line::from(Span::styled(
            "select a node and press r to run its block (R to run without deps)",
            Style::default().fg(Color::DarkGray),
        )))
    };
    f.render_widget(Paragraph::new(text), inner);
}

fn render_footer(f: &mut Frame, area: Rect) {
    let hint = "tab focus · j/k move/scroll · enter expand · h/l collapse/expand · r run · R run (no deps) · K kill · ? help · q quit";
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(hint, Style::default().fg(Color::DarkGray)))),
        area,
    );
}

fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    let x = area.x + (area.width.saturating_sub(width)) / 2;
    let y = area.y + (area.height.saturating_sub(height)) / 2;
    Rect { x, y, width, height }
}

fn render_var_prompt(f: &mut Frame, area: Rect, vp: &super::app::VarPromptState) {
    let rect = centered_rect(60, 6, area);
    f.render_widget(Clear, rect);
    let block = Block::default().borders(Borders::ALL).title(" variable needed ");
    let inner = block.inner(rect);
    f.render_widget(block, rect);

    let masked;
    let shown: &str = if vp.decl.secret {
        masked = "*".repeat(vp.input.chars().count());
        &masked
    } else {
        &vp.input
    };

    let lines = vec![
        Line::from(vp.decl.prompt.clone()),
        Line::from(""),
        Line::from(vec![Span::raw("> "), Span::styled(shown, Style::default().fg(Color::LightGreen)), Span::raw("_")]),
        Line::from(Span::styled("enter confirm · esc cancel run", Style::default().fg(Color::DarkGray))),
    ];
    f.render_widget(Paragraph::new(lines), inner);
}

fn render_block_picker(f: &mut Frame, area: Rect, bp: &super::app::BlockPickerState) {
    let height = (bp.blocks.len() as u16 + 4).min(area.height);
    let rect = centered_rect(56, height, area);
    f.render_widget(Clear, rect);
    let mode = if bp.with_deps { "run (with deps)" } else { "run (no deps)" };
    let block = Block::default().borders(Borders::ALL).title(format!(" {} — which block? ", mode));
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
            let badge = if flags.is_empty() { String::new() } else { format!("  [{}]", flags.join(",")) };
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

fn render_help(f: &mut Frame, area: Rect) {
    let rect = centered_rect(62, 23, area);
    f.render_widget(Clear, rect);
    let block = Block::default().borders(Borders::ALL).title(" keybindings ");
    let inner = block.inner(rect);
    f.render_widget(block, rect);

    let lines = vec![
        "tab             switch focus: tree <-> document",
        "j / k / ↑ / ↓   move selection (tree) or scroll (document)",
        "enter           expand/collapse node",
        "l / →           expand node",
        "h / ←           collapse node, or jump to parent",
        "r               run this node's block, with its deps chain",
        "R               run this node's block only (skip deps)",
        "  (a node with more than one block opens a picker first)",
        "K               kill the running block",
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
    ]
    .into_iter()
    .map(Line::from)
    .collect::<Vec<_>>();
    f.render_widget(Paragraph::new(lines), inner);
}
