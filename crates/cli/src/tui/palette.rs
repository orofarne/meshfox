//! The `Ctrl-p` command palette: a fuzzy-searchable list of commands that
//! works from the main view and from the source editor. Choosing a command
//! that needs a value (a title, a confirmation) asks for it in the same
//! modal; `App` runs the finished `Request`.
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Modifier, Style},
    text::Line,
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph},
    Frame,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommandId {
    NewChild,
    NewSibling,
    Rename,
    ChangeId,
    MoveTo,
    MoveUp,
    MoveDown,
    MovePosition,
    AddEdge,
    RemoveEdge,
    Delete,
    DeleteKeepChildren,
    InsertUnicode,
    SuggestAttributes,
    Stats,
    BlockStats,
    /// Does what pressing this key in the main view does.
    Press(char),
}

/// Where a command makes sense.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Scope {
    /// Changes the node tree through the worker; offered everywhere, but
    /// from the editor only once the buffer is saved.
    Structure,
    /// Acts on the open source editor's buffer.
    Editor,
    /// A main-view action; not offered inside the editor.
    View,
}

struct Entry {
    id: CommandId,
    title: &'static str,
    /// The key that does the same thing without the palette, if any.
    key: &'static str,
    scope: Scope,
}

const ENTRIES: &[Entry] = &[
    view(CommandId::Press('r'), "Run block with its deps chain", "r"),
    view(CommandId::Press('R'), "Run only this block", "R"),
    view(CommandId::Press('K'), "Kill running block", "K"),
    view(CommandId::Press('S'), "Reset session", "S"),
    view(CommandId::Press('v'), "Services", "v"),
    view(CommandId::Press('t'), "Terminal sessions", "t"),
    view(CommandId::Press('H'), "Document history", "H"),
    view(CommandId::Press('L'), "Block run history", "L"),
    view(CommandId::BlockStats, "Block statistics", ""),
    view(CommandId::Stats, "Canvas statistics", ""),
    view(CommandId::Press('o'), "Open the node's file target", "o"),
    view(CommandId::Press('c'), "Configure variables", "c"),
    view(CommandId::Press('e'), "Edit source", "e"),
    view(CommandId::Press('/'), "Search", "/"),
    view(CommandId::Press('f'), "Toggle fullscreen pane", "f"),
    view(CommandId::Press('z'), "Collapse or expand pane", "z"),
    view(CommandId::Press('?'), "Help and keys", "?"),
    view(CommandId::Press('q'), "Quit", "q"),
    Entry {
        id: CommandId::NewChild,
        title: "New child node",
        key: "",
        scope: Scope::Structure,
    },
    Entry {
        id: CommandId::NewSibling,
        title: "New sibling node",
        key: "",
        scope: Scope::Structure,
    },
    Entry {
        id: CommandId::Rename,
        title: "Rename node",
        key: "",
        scope: Scope::Structure,
    },
    Entry {
        id: CommandId::ChangeId,
        title: "Change node id",
        key: "",
        scope: Scope::Structure,
    },
    Entry {
        id: CommandId::MoveTo,
        title: "Move node to another parent",
        key: "",
        scope: Scope::Structure,
    },
    Entry {
        id: CommandId::MoveUp,
        title: "Move node up among siblings",
        key: "",
        scope: Scope::Structure,
    },
    Entry {
        id: CommandId::MoveDown,
        title: "Move node down among siblings",
        key: "",
        scope: Scope::Structure,
    },
    Entry {
        id: CommandId::MovePosition,
        title: "Move node to position among siblings",
        key: "",
        scope: Scope::Structure,
    },
    Entry {
        id: CommandId::AddEdge,
        title: "Add edge from another node",
        key: "",
        scope: Scope::Structure,
    },
    Entry {
        id: CommandId::RemoveEdge,
        title: "Remove an incoming edge",
        key: "",
        scope: Scope::Structure,
    },
    Entry {
        id: CommandId::Delete,
        title: "Delete node and its subtree",
        key: "",
        scope: Scope::Structure,
    },
    Entry {
        id: CommandId::DeleteKeepChildren,
        title: "Delete node, keep its children",
        key: "",
        scope: Scope::Structure,
    },
    Entry {
        id: CommandId::InsertUnicode,
        title: "Insert Unicode symbol",
        key: "Ctrl-g",
        scope: Scope::Editor,
    },
    Entry {
        id: CommandId::SuggestAttributes,
        title: "Suggest attributes",
        key: "Ctrl-Space",
        scope: Scope::Editor,
    },
];

const fn view(id: CommandId, title: &'static str, key: &'static str) -> Entry {
    Entry {
        id,
        title,
        key,
        scope: Scope::View,
    }
}

/// What a finished palette interaction asks `App` to do.
#[derive(Debug, PartialEq, Eq)]
pub struct Request {
    pub id: CommandId,
    /// The typed title or id, or the chosen parent's id, for commands
    /// that take one.
    pub text: Option<String>,
}

pub enum Outcome {
    Stay,
    Close,
    Run(Request),
}

enum Step {
    Search,
    Input {
        id: CommandId,
        label: &'static str,
        value: String,
    },
    Confirm {
        id: CommandId,
    },
    /// Picking the new parent for `MoveTo` from `Palette::parents`.
    Parent {
        id: CommandId,
        query: String,
        results: Vec<usize>,
        state: ListState,
    },
}

pub struct Palette {
    in_editor: bool,
    /// Title and id of the node the commands would act on, for prompts.
    target_title: String,
    target_id: String,
    /// Nodes `MoveTo`/`AddEdge` may pick: `(id, indented title)`.
    parents: Vec<(String, String)>,
    /// The nodes the target has an incoming edge from, for `RemoveEdge`.
    edges: Vec<(String, String)>,
    step: Step,
    pub query: String,
    results: Vec<usize>,
    state: ListState,
    /// Why the last choice was refused; shown under the list.
    pub notice: Option<String>,
}

/// Subsequence match, case-insensitive. Lower is better: a match that
/// starts early and stays contiguous beats a scattered one.
fn score(query: &str, title: &str) -> Option<usize> {
    let title: Vec<char> = title.to_lowercase().chars().collect();
    let mut pos = 0;
    let mut cost = 0;
    let mut last: Option<usize> = None;
    for q in query.to_lowercase().chars().filter(|c| !c.is_whitespace()) {
        let found = title[pos..].iter().position(|&c| c == q)? + pos;
        cost += match last {
            Some(l) => found - l - 1,
            None => found,
        };
        last = Some(found);
        pos = found + 1;
    }
    Some(cost)
}

impl Palette {
    pub fn new(
        in_editor: bool,
        target_title: String,
        target_id: String,
        parents: Vec<(String, String)>,
        edges: Vec<(String, String)>,
    ) -> Self {
        let mut palette = Self {
            in_editor,
            target_title,
            target_id,
            parents,
            edges,
            step: Step::Search,
            query: String::new(),
            results: Vec::new(),
            state: ListState::default(),
            notice: None,
        };
        palette.refresh();
        palette
    }

    fn available(&self, entry: &Entry) -> bool {
        match entry.scope {
            Scope::Structure => true,
            Scope::Editor => self.in_editor,
            Scope::View => !self.in_editor,
        }
    }

    fn refresh(&mut self) {
        let mut scored: Vec<(usize, usize)> = ENTRIES
            .iter()
            .enumerate()
            .filter(|(_, e)| self.available(e))
            .filter_map(|(i, e)| score(&self.query, e.title).map(|s| (s, i)))
            .collect();
        scored.sort_unstable();
        self.results = scored.into_iter().map(|(_, i)| i).collect();
        self.state = ListState::default();
        self.state.select((!self.results.is_empty()).then_some(0));
    }

    fn chosen(&self) -> Option<&'static Entry> {
        self.state
            .selected()
            .and_then(|i| self.results.get(i))
            .map(|&i| &ENTRIES[i])
    }

    fn start(&mut self, id: CommandId) -> Outcome {
        self.notice = None;
        match id {
            CommandId::NewChild | CommandId::NewSibling => {
                self.step = Step::Input {
                    id,
                    label: "Title",
                    value: String::new(),
                };
                Outcome::Stay
            }
            CommandId::Rename => {
                self.step = Step::Input {
                    id,
                    label: "New title",
                    value: self.target_title.clone(),
                };
                Outcome::Stay
            }
            CommandId::ChangeId => {
                self.step = Step::Input {
                    id,
                    label: "New id",
                    value: self.target_id.clone(),
                };
                Outcome::Stay
            }
            CommandId::MoveTo | CommandId::AddEdge | CommandId::RemoveEdge => {
                let count = if id == CommandId::RemoveEdge {
                    self.edges.len()
                } else {
                    self.parents.len()
                };
                if count == 0 {
                    self.notice = Some("no incoming edges on this node".into());
                    return Outcome::Stay;
                }
                let results: Vec<usize> = (0..count).collect();
                let mut state = ListState::default();
                state.select(Some(0));
                self.step = Step::Parent {
                    id,
                    query: String::new(),
                    results,
                    state,
                };
                Outcome::Stay
            }
            CommandId::MovePosition => {
                self.step = Step::Input {
                    id,
                    label: "Sibling position (1-based)",
                    value: String::new(),
                };
                Outcome::Stay
            }
            CommandId::MoveUp | CommandId::MoveDown => Outcome::Run(Request { id, text: None }),
            CommandId::Delete | CommandId::DeleteKeepChildren => {
                self.step = Step::Confirm { id };
                Outcome::Stay
            }
            CommandId::InsertUnicode
            | CommandId::SuggestAttributes
            | CommandId::Stats
            | CommandId::BlockStats
            | CommandId::Press(_) => Outcome::Run(Request { id, text: None }),
        }
    }

    pub fn on_key(&mut self, key: KeyEvent) -> Outcome {
        let plain = !key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER);
        match &mut self.step {
            Step::Search => match key.code {
                KeyCode::Esc => Outcome::Close,
                KeyCode::Char('p') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    Outcome::Close
                }
                KeyCode::Enter => match self.chosen() {
                    Some(entry) => self.start(entry.id),
                    None => Outcome::Stay,
                },
                KeyCode::Up => {
                    self.state
                        .select(self.state.selected().map(|i| i.saturating_sub(1)));
                    Outcome::Stay
                }
                KeyCode::Down => {
                    self.state.select(
                        self.state
                            .selected()
                            .map(|i| (i + 1).min(self.results.len().saturating_sub(1))),
                    );
                    Outcome::Stay
                }
                KeyCode::Backspace => {
                    self.query.pop();
                    self.notice = None;
                    self.refresh();
                    Outcome::Stay
                }
                KeyCode::Char(c) if plain => {
                    self.query.push(c);
                    self.notice = None;
                    self.refresh();
                    Outcome::Stay
                }
                _ => Outcome::Stay,
            },
            Step::Input { id, value, .. } => match key.code {
                KeyCode::Esc => {
                    self.step = Step::Search;
                    Outcome::Stay
                }
                KeyCode::Enter => {
                    let text = value.trim().to_string();
                    if text.is_empty() {
                        return Outcome::Stay;
                    }
                    Outcome::Run(Request {
                        id: *id,
                        text: Some(text),
                    })
                }
                KeyCode::Backspace => {
                    value.pop();
                    Outcome::Stay
                }
                KeyCode::Char(c) if plain => {
                    value.push(c);
                    Outcome::Stay
                }
                _ => Outcome::Stay,
            },
            Step::Parent {
                id,
                query,
                results,
                state,
            } => match key.code {
                KeyCode::Esc => {
                    self.step = Step::Search;
                    Outcome::Stay
                }
                KeyCode::Enter => match state.selected().and_then(|i| results.get(i)) {
                    Some(&i) => Outcome::Run(Request {
                        id: *id,
                        text: Some(
                            if *id == CommandId::RemoveEdge {
                                &self.edges
                            } else {
                                &self.parents
                            }[i]
                                .0
                                .clone(),
                        ),
                    }),
                    None => Outcome::Stay,
                },
                KeyCode::Up => {
                    state.select(state.selected().map(|i| i.saturating_sub(1)));
                    Outcome::Stay
                }
                KeyCode::Down => {
                    state.select(
                        state
                            .selected()
                            .map(|i| (i + 1).min(results.len().saturating_sub(1))),
                    );
                    Outcome::Stay
                }
                KeyCode::Backspace | KeyCode::Char(_)
                    if matches!(key.code, KeyCode::Backspace) || plain =>
                {
                    match key.code {
                        KeyCode::Backspace => {
                            query.pop();
                        }
                        KeyCode::Char(c) => query.push(c),
                        _ => {}
                    }
                    let list = if *id == CommandId::RemoveEdge {
                        &self.edges
                    } else {
                        &self.parents
                    };
                    let mut scored: Vec<(usize, usize)> = list
                        .iter()
                        .enumerate()
                        .filter_map(|(i, (_, label))| score(query, label.trim()).map(|s| (s, i)))
                        .collect();
                    scored.sort_unstable();
                    *results = scored.into_iter().map(|(_, i)| i).collect();
                    *state = ListState::default();
                    state.select((!results.is_empty()).then_some(0));
                    Outcome::Stay
                }
                _ => Outcome::Stay,
            },
            Step::Confirm { id } => match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') => Outcome::Run(Request {
                    id: *id,
                    text: None,
                }),
                KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc | KeyCode::Enter => {
                    self.step = Step::Search;
                    Outcome::Stay
                }
                _ => Outcome::Stay,
            },
        }
    }

    pub fn render(&mut self, frame: &mut Frame, area: Rect) {
        let width = area.width.min(64);
        let height = area.height.min(14);
        let rect = Rect::new(
            area.x + (area.width - width) / 2,
            area.y + (area.height - height) / 2,
            width,
            height,
        );
        frame.render_widget(Clear, rect);
        let block = Block::default().borders(Borders::ALL).title(" Commands ");
        let inner = block.inner(rect);
        frame.render_widget(block, rect);
        let rows = Layout::vertical([
            Constraint::Length(1),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .split(inner);
        match &mut self.step {
            Step::Search => {
                let prompt = format!("> {}▏", self.query);
                let scroll = Line::raw(prompt.as_str())
                    .width()
                    .saturating_sub(rows[0].width as usize);
                frame.render_widget(
                    Paragraph::new(prompt).scroll((0, scroll.min(u16::MAX as usize) as u16)),
                    rows[0],
                );
                if self.results.is_empty() {
                    frame.render_widget(Paragraph::new("No matching commands"), rows[1]);
                } else {
                    let key_width = ENTRIES.iter().map(|e| e.key.len()).max().unwrap_or(0);
                    let title_width = usize::from(rows[1].width).saturating_sub(key_width + 3);
                    let items = self
                        .results
                        .iter()
                        .map(|&i| {
                            let e = &ENTRIES[i];
                            format!("{:<title_width$}  {}", e.title, e.key)
                        })
                        .collect();
                    render_list(frame, rows[1], items, &mut self.state);
                }
                frame.render_widget(
                    Paragraph::new(
                        self.notice
                            .clone()
                            .unwrap_or_else(|| "↑/↓ choose · Enter run · Esc cancel".into()),
                    ),
                    rows[2],
                );
            }
            Step::Input { label, value, .. } => {
                frame.render_widget(Paragraph::new(format!("{label}: {value}▏")), rows[0]);
                frame.render_widget(Paragraph::new("Enter confirm · Esc back"), rows[2]);
            }
            Step::Parent {
                id,
                query,
                results,
                state,
            } => {
                let label = match id {
                    CommandId::MoveTo => "Move under",
                    CommandId::AddEdge => "Edge from",
                    _ => "Remove edge from",
                };
                frame.render_widget(Paragraph::new(format!("{label}: {query}▏")), rows[0]);
                if results.is_empty() {
                    frame.render_widget(Paragraph::new("No matching nodes"), rows[1]);
                } else {
                    let list = if *id == CommandId::RemoveEdge {
                        &self.edges
                    } else {
                        &self.parents
                    };
                    let items = results.iter().map(|&i| list[i].1.clone()).collect();
                    render_list(frame, rows[1], items, state);
                }
                frame.render_widget(
                    Paragraph::new("↑/↓ choose · Enter choose · Esc back"),
                    rows[2],
                );
            }
            Step::Confirm { id } => {
                let what = match id {
                    CommandId::DeleteKeepChildren => "Delete (children move up)",
                    _ => "Delete with all descendants",
                };
                frame.render_widget(
                    Paragraph::new(format!("{what}: \"{}\"?", self.target_title)),
                    rows[0],
                );
                frame.render_widget(Paragraph::new("y confirm · n/Esc back"), rows[2]);
            }
        }
    }
}

/// A scrolling, single-selection list filling `area`.
fn render_list(frame: &mut Frame, area: Rect, items: Vec<String>, state: &mut ListState) {
    let visible_rows = usize::from(area.height).max(1);
    let selected = state.selected().unwrap_or(0);
    let mut offset = state.offset();
    if selected < offset {
        offset = selected;
    }
    if selected >= offset + visible_rows {
        offset = selected + 1 - visible_rows;
    }
    *state.offset_mut() = offset;
    let mut visible_state = ListState::default();
    visible_state.select(Some(selected - offset));
    let items: Vec<_> = items
        .into_iter()
        .skip(offset)
        .take(visible_rows)
        .map(ListItem::new)
        .collect();
    frame.render_stateful_widget(
        List::new(items).highlight_style(Style::default().add_modifier(Modifier::REVERSED)),
        area,
        &mut visible_state,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn type_text(p: &mut Palette, text: &str) {
        for c in text.chars() {
            p.on_key(key(KeyCode::Char(c)));
        }
    }

    #[test]
    fn fuzzy_search_ranks_contiguous_matches_first() {
        let mut p = Palette::new(false, "n".into(), String::new(), Vec::new(), Vec::new());
        type_text(&mut p, "ren");
        assert_eq!(p.chosen().map(|e| e.id), Some(CommandId::Rename));
        type_text(&mut p, "zzz");
        assert!(p.chosen().is_none());
    }

    #[test]
    fn editor_commands_only_show_in_the_editor() {
        let main = Palette::new(false, String::new(), String::new(), Vec::new(), Vec::new());
        assert!(main
            .results
            .iter()
            .all(|&i| ENTRIES[i].scope != Scope::Editor));
        let editor = Palette::new(true, String::new(), String::new(), Vec::new(), Vec::new());
        assert!(editor
            .results
            .iter()
            .any(|&i| ENTRIES[i].scope == Scope::Editor));
    }

    #[test]
    fn sibling_position_is_entered_and_can_be_cancelled() {
        let mut p = Palette::new(false, "Node".into(), "node".into(), vec![], vec![]);
        type_text(&mut p, "move position");
        assert_eq!(p.chosen().unwrap().id, CommandId::MovePosition);
        assert!(matches!(p.on_key(key(KeyCode::Enter)), Outcome::Stay));
        type_text(&mut p, "12");
        assert!(matches!(p.on_key(key(KeyCode::Esc)), Outcome::Stay));
        p.on_key(key(KeyCode::Enter));
        assert!(matches!(p.on_key(key(KeyCode::Enter)), Outcome::Stay));
        type_text(&mut p, "3");
        assert!(
            matches!(p.on_key(key(KeyCode::Enter)), Outcome::Run(Request {
            id: CommandId::MovePosition, text: Some(text),
        }) if text == "3")
        );
    }

    #[test]
    fn new_child_asks_for_a_title_then_runs() {
        let mut p = Palette::new(
            false,
            "Parent".into(),
            String::new(),
            Vec::new(),
            Vec::new(),
        );
        type_text(&mut p, "new child");
        assert!(matches!(p.on_key(key(KeyCode::Enter)), Outcome::Stay));
        // Empty titles are not submitted.
        assert!(matches!(p.on_key(key(KeyCode::Enter)), Outcome::Stay));
        type_text(&mut p, " Hello ");
        match p.on_key(key(KeyCode::Enter)) {
            Outcome::Run(req) => assert_eq!(
                req,
                Request {
                    id: CommandId::NewChild,
                    text: Some("Hello".into())
                }
            ),
            _ => panic!("expected a request"),
        }
    }

    #[test]
    fn rename_is_prefilled_and_delete_needs_confirmation() {
        let mut p = Palette::new(false, "Old".into(), "old".into(), Vec::new(), Vec::new());
        type_text(&mut p, "rename");
        p.on_key(key(KeyCode::Enter));
        p.on_key(key(KeyCode::Backspace));
        type_text(&mut p, "ew");
        match p.on_key(key(KeyCode::Enter)) {
            Outcome::Run(req) => assert_eq!(req.text.as_deref(), Some("Olew")),
            _ => panic!("expected a request"),
        }

        let mut p = Palette::new(false, "Old".into(), "old".into(), Vec::new(), Vec::new());
        type_text(&mut p, "delete node and");
        assert!(matches!(p.on_key(key(KeyCode::Enter)), Outcome::Stay));
        assert!(matches!(p.on_key(key(KeyCode::Char('n'))), Outcome::Stay));
        p.on_key(key(KeyCode::Enter));
        match p.on_key(key(KeyCode::Char('y'))) {
            Outcome::Run(req) => assert_eq!(req.id, CommandId::Delete),
            _ => panic!("expected a request"),
        }
    }

    #[test]
    fn change_id_is_prefilled_and_move_picks_a_parent() {
        let parents = vec![
            ("root".to_string(), "Root".to_string()),
            ("other".to_string(), "  Other".to_string()),
        ];
        let mut p = Palette::new(false, "Leaf".into(), "leaf".into(), parents, Vec::new());
        type_text(&mut p, "change node id");
        p.on_key(key(KeyCode::Enter));
        type_text(&mut p, "2");
        match p.on_key(key(KeyCode::Enter)) {
            Outcome::Run(req) => assert_eq!(req.text.as_deref(), Some("leaf2")),
            _ => panic!("expected a request"),
        }

        let mut p = Palette::new(
            false,
            "Leaf".into(),
            "leaf".into(),
            vec![
                ("root".to_string(), "Root".to_string()),
                ("other".to_string(), "  Other".to_string()),
            ],
            Vec::new(),
        );
        type_text(&mut p, "move node to");
        p.on_key(key(KeyCode::Enter));
        type_text(&mut p, "oth");
        match p.on_key(key(KeyCode::Enter)) {
            Outcome::Run(req) => assert_eq!(
                req,
                Request {
                    id: CommandId::MoveTo,
                    text: Some("other".into())
                }
            ),
            _ => panic!("expected a request"),
        }
    }

    #[test]
    fn main_view_actions_replay_their_keys_and_stay_out_of_the_editor() {
        let mut p = Palette::new(false, "x".into(), String::new(), Vec::new(), Vec::new());
        type_text(&mut p, "quit");
        match p.on_key(key(KeyCode::Enter)) {
            Outcome::Run(req) => assert_eq!(req.id, CommandId::Press('q')),
            _ => panic!("expected a request"),
        }
        let mut p = Palette::new(true, "x".into(), String::new(), Vec::new(), Vec::new());
        type_text(&mut p, "quit");
        assert!(p.chosen().is_none());
        let mut p = Palette::new(false, "x".into(), String::new(), Vec::new(), Vec::new());
        type_text(&mut p, "canvas statistics");
        match p.on_key(key(KeyCode::Enter)) {
            Outcome::Run(req) => assert_eq!(req.id, CommandId::Stats),
            _ => panic!("expected a request"),
        }
    }

    #[test]
    fn edges_are_picked_from_the_right_list() {
        let parents = vec![("a".to_string(), "A".to_string())];
        let edges = vec![("e".to_string(), "Edge source".to_string())];
        let mut p = Palette::new(
            false,
            "x".into(),
            "x".into(),
            parents.clone(),
            edges.clone(),
        );
        type_text(&mut p, "remove an incoming");
        p.on_key(key(KeyCode::Enter));
        match p.on_key(key(KeyCode::Enter)) {
            Outcome::Run(req) => assert_eq!(
                req,
                Request {
                    id: CommandId::RemoveEdge,
                    text: Some("e".into())
                }
            ),
            _ => panic!("expected a request"),
        }
        let mut p = Palette::new(false, "x".into(), "x".into(), parents, edges);
        type_text(&mut p, "add edge");
        p.on_key(key(KeyCode::Enter));
        match p.on_key(key(KeyCode::Enter)) {
            Outcome::Run(req) => assert_eq!(req.text.as_deref(), Some("a")),
            _ => panic!("expected a request"),
        }
        // Nothing to remove: stays on the command list with a notice.
        let mut p = Palette::new(false, "x".into(), "x".into(), Vec::new(), Vec::new());
        type_text(&mut p, "remove an incoming");
        assert!(matches!(p.on_key(key(KeyCode::Enter)), Outcome::Stay));
        assert!(p.notice.is_some());
    }

    #[test]
    fn esc_steps_back_then_closes() {
        let mut p = Palette::new(false, "x".into(), String::new(), Vec::new(), Vec::new());
        type_text(&mut p, "rename");
        p.on_key(key(KeyCode::Enter));
        assert!(matches!(p.on_key(key(KeyCode::Esc)), Outcome::Stay));
        assert!(matches!(p.on_key(key(KeyCode::Esc)), Outcome::Close));
    }

    #[test]
    fn renders_on_small_terminals() {
        let mut p = Palette::new(true, "x".into(), String::new(), Vec::new(), Vec::new());
        for (w, h) in [(80, 24), (12, 4), (1, 1)] {
            let mut terminal =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(w, h)).unwrap();
            terminal.draw(|f| p.render(f, f.area())).unwrap();
        }
    }
}
