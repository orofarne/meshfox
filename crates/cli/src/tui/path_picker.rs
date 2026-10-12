//! The TUI's chooser for `dir`/`file` variables (SPEC.md, "Path pickers"):
//! a modal over the worker's `GET /api/fs/list`, opened with `Ctrl-O` on
//! such a field in the variable form or a `form` fence. This module holds
//! the state and the key/mouse semantics (pure, no I/O — navigation comes
//! back as [`PickerAction::Navigate`] for the app to fetch); `ui.rs` draws
//! it and `app.rs` wires it in.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use meshfox_core::fsbrowse::{FsListing, PickKind};
use meshfox_core::{FileMode, VarDecl, VarType};

/// The visible "open the picker" affordance after a `dir`/`file` field —
/// clickable, and naming the key that does the same.
pub const BROWSE_MARKER: &str = "[Browse… ^o]";

/// Rows `PageUp`/`PageDown` move by.
const PAGE: usize = 10;

/// Which field the picked path is written into.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickerTarget {
    /// `App::var_form`'s field at this index.
    VarForm(usize),
    /// `App::active_inline_form`'s field at this index.
    InlineForm(usize),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickerFocus {
    List,
    Filter,
    /// The file-name line of a `mode="save"` picker.
    Name,
}

/// What a key or click asks the app to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PickerAction {
    Stay,
    Cancel,
    /// Write this absolute path into the target field and close.
    Choose(String),
    /// List this path next (honoring `show_hidden`).
    Navigate(String),
}

/// One displayed row: the synthetic `..`, a directory, or a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub name: String,
    pub is_dir: bool,
    pub size: Option<u64>,
    pub is_parent: bool,
}

pub struct PathPickerState {
    pub target: PickerTarget,
    pub kind: PickKind,
    /// `mode="save"`: a new file name may be typed.
    pub save: bool,
    /// `ext=` hint, lowercase without dots.
    pub ext: Vec<String>,
    pub title: String,
    pub listing: Option<FsListing>,
    pub error: Option<String>,
    pub show_hidden: bool,
    /// Narrow files to `ext` (a hint: `e` switches it off).
    pub only_ext: bool,
    pub filter: String,
    pub focus: PickerFocus,
    pub name: String,
    /// Index into [`PathPickerState::rows`].
    pub selected: usize,
}

pub fn join_path(dir: &str, name: &str) -> String {
    if dir.ends_with('/') {
        format!("{dir}{name}")
    } else {
        format!("{dir}/{name}")
    }
}

/// First visible row when `selected` has to stay in a window of `height`
/// rows and the window only ever scrolls as far as needed — what `List`
/// does on its own from offset 0, written out so hit-testing can mirror it.
pub fn window_start(selected: usize, height: usize) -> usize {
    (selected + 1).saturating_sub(height.max(1))
}

fn ext_of(name: &str) -> String {
    match name.rfind('.') {
        Some(i) if i > 0 => name[i + 1..].to_ascii_lowercase(),
        _ => String::new(),
    }
}

impl PathPickerState {
    /// A picker for `decl` (a `dir` or `file` variable) writing into
    /// `target`.
    pub fn new(target: PickerTarget, decl: &VarDecl) -> PathPickerState {
        PathPickerState {
            target,
            kind: if decl.var_type == VarType::Dir {
                PickKind::Dir
            } else {
                PickKind::File
            },
            save: decl.var_type == VarType::File && decl.file_mode == Some(FileMode::Save),
            ext: decl.ext.clone(),
            title: decl.prompt.clone(),
            listing: None,
            error: None,
            show_hidden: false,
            only_ext: true,
            filter: String::new(),
            focus: PickerFocus::List,
            name: String::new(),
            selected: 0,
        }
    }

    /// Takes a fresh listing: the filter is cleared and, when the request
    /// named a file, it is pre-selected (and its name offered for `save`).
    pub fn set_listing(&mut self, listing: FsListing) {
        self.error = None;
        self.filter.clear();
        if self.focus == PickerFocus::Filter {
            self.focus = PickerFocus::List;
        }
        let selected_name = listing.selected.clone();
        self.listing = Some(listing);
        self.selected = 0;
        if let Some(name) = selected_name {
            if self.save {
                self.name = name.clone();
            }
            if let Some(i) = self.rows().iter().position(|r| !r.is_parent && r.name == name) {
                self.selected = i;
            }
        }
    }

    /// The rows currently shown: `..` (when there is a parent and no
    /// filter), then the entries passing the filter and the `ext` hint.
    pub fn rows(&self) -> Vec<Row> {
        let Some(l) = &self.listing else {
            return Vec::new();
        };
        let mut rows = Vec::new();
        let f = self.filter.trim().to_lowercase();
        if l.parent.is_some() && f.is_empty() {
            rows.push(Row {
                name: "..".to_string(),
                is_dir: true,
                size: None,
                is_parent: true,
            });
        }
        let narrow = self.kind == PickKind::File && self.only_ext && !self.ext.is_empty();
        for e in &l.entries {
            if !f.is_empty() && !e.name.to_lowercase().contains(&f) {
                continue;
            }
            if narrow && !e.is_dir && !self.ext.contains(&ext_of(&e.name)) {
                continue;
            }
            rows.push(Row {
                name: e.name.clone(),
                is_dir: e.is_dir,
                size: e.size,
                is_parent: false,
            });
        }
        rows
    }

    /// The path `Enter` in the name line (or `s` on a directory picker)
    /// would choose.
    pub fn chosen_path(&self) -> Option<String> {
        let l = self.listing.as_ref()?;
        match self.kind {
            PickKind::Dir => Some(l.path.clone()),
            PickKind::File => {
                let n = self.name.trim();
                (self.save && !n.is_empty() && !n.contains('/')).then(|| join_path(&l.path, n))
            }
        }
    }

    /// `Enter` semantics on the selected row — shared by the keyboard and a
    /// second click on an already selected row.
    pub fn activate(&mut self) -> PickerAction {
        let rows = self.rows();
        let Some(row) = rows.get(self.selected) else {
            return PickerAction::Stay;
        };
        let Some(l) = &self.listing else {
            return PickerAction::Stay;
        };
        if row.is_parent {
            return l.parent.clone().map_or(PickerAction::Stay, PickerAction::Navigate);
        }
        if row.is_dir {
            return PickerAction::Navigate(join_path(&l.path, &row.name));
        }
        if self.kind != PickKind::File {
            return PickerAction::Stay;
        }
        if self.save {
            self.name = row.name.clone();
            self.focus = PickerFocus::Name;
            PickerAction::Stay
        } else {
            PickerAction::Choose(join_path(&l.path, &row.name))
        }
    }

    /// A left click on row `idx`: selects it, or activates it when it was
    /// already selected.
    pub fn click_row(&mut self, idx: usize) -> PickerAction {
        if idx >= self.rows().len() {
            return PickerAction::Stay;
        }
        if idx == self.selected {
            self.activate()
        } else {
            self.selected = idx;
            self.focus = PickerFocus::List;
            PickerAction::Stay
        }
    }

    pub fn scroll(&mut self, delta: isize) {
        let len = self.rows().len();
        if len == 0 {
            return;
        }
        let next = self.selected as isize + delta;
        self.selected = next.clamp(0, len as isize - 1) as usize;
    }

    pub fn on_key(&mut self, key: KeyEvent) -> PickerAction {
        match self.focus {
            PickerFocus::Filter => self.on_filter_key(key),
            PickerFocus::Name => self.on_name_key(key),
            PickerFocus::List => self.on_list_key(key),
        }
    }

    fn on_filter_key(&mut self, key: KeyEvent) -> PickerAction {
        match key.code {
            KeyCode::Esc => {
                self.filter.clear();
                self.focus = PickerFocus::List;
                self.selected = 0;
            }
            KeyCode::Enter | KeyCode::Tab | KeyCode::Down => self.focus = PickerFocus::List,
            KeyCode::Backspace => {
                self.filter.pop();
                self.selected = 0;
            }
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.filter.push(c);
                self.selected = 0;
            }
            _ => {}
        }
        PickerAction::Stay
    }

    fn on_name_key(&mut self, key: KeyEvent) -> PickerAction {
        match key.code {
            KeyCode::Esc | KeyCode::Tab => self.focus = PickerFocus::List,
            KeyCode::Enter => {
                if let Some(p) = self.chosen_path() {
                    return PickerAction::Choose(p);
                }
            }
            KeyCode::Backspace => {
                self.name.pop();
            }
            KeyCode::Char(c) if c != '/' && !key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.name.push(c)
            }
            _ => {}
        }
        PickerAction::Stay
    }

    fn on_list_key(&mut self, key: KeyEvent) -> PickerAction {
        let len = self.rows().len();
        match key.code {
            KeyCode::Esc => return PickerAction::Cancel,
            KeyCode::Up | KeyCode::Char('k') => self.scroll(-1),
            KeyCode::Down | KeyCode::Char('j') => self.scroll(1),
            KeyCode::PageUp => self.scroll(-(PAGE as isize)),
            KeyCode::PageDown => self.scroll(PAGE as isize),
            KeyCode::Home | KeyCode::Char('g') => self.selected = 0,
            KeyCode::End | KeyCode::Char('G') => self.selected = len.saturating_sub(1),
            KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => return self.activate(),
            KeyCode::Backspace | KeyCode::Left | KeyCode::Char('h') => {
                if let Some(p) = self.listing.as_ref().and_then(|l| l.parent.clone()) {
                    return PickerAction::Navigate(p);
                }
            }
            KeyCode::Char('/') => self.focus = PickerFocus::Filter,
            KeyCode::Char('.') => {
                self.show_hidden = !self.show_hidden;
                if let Some(l) = &self.listing {
                    return PickerAction::Navigate(l.path.clone());
                }
            }
            KeyCode::Char('e') => {
                self.only_ext = !self.only_ext;
                self.selected = 0;
            }
            KeyCode::Char('~') => {
                if let Some(h) = self.listing.as_ref().and_then(|l| l.home.clone()) {
                    return PickerAction::Navigate(h);
                }
            }
            KeyCode::Char('c') => {
                if let Some(c) = self.listing.as_ref().map(|l| l.canvas_dir.clone()) {
                    return PickerAction::Navigate(c);
                }
            }
            KeyCode::Char('s') | KeyCode::Char(' ') if self.kind == PickKind::Dir => {
                if let Some(p) = self.chosen_path() {
                    return PickerAction::Choose(p);
                }
            }
            KeyCode::Tab if self.save => self.focus = PickerFocus::Name,
            _ => {}
        }
        PickerAction::Stay
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use meshfox_core::fsbrowse::FsEntry;

    fn decl(var_type: VarType, mode: Option<FileMode>, ext: &[&str]) -> VarDecl {
        let mut d = meshfox_core::vars::scan_var_decls("<!-- meshfox:var name=\"X\" -->\n")
            .unwrap()
            .remove(0);
        d.var_type = var_type;
        d.file_mode = mode;
        d.ext = ext.iter().map(|s| s.to_string()).collect();
        d
    }

    fn entry(name: &str, is_dir: bool) -> FsEntry {
        FsEntry {
            name: name.to_string(),
            is_dir,
            size: (!is_dir).then_some(3),
        }
    }

    fn listing(selected: Option<&str>) -> FsListing {
        FsListing {
            path: "/media".to_string(),
            parent: Some("/".to_string()),
            home: Some("/home/u".to_string()),
            canvas_dir: "/work".to_string(),
            selected: selected.map(String::from),
            entries: vec![
                entry("raw", true),
                entry("a.mkv", false),
                entry("b.MP4", false),
                entry("notes.txt", false),
            ],
            truncated: false,
        }
    }

    fn key(c: KeyCode) -> KeyEvent {
        KeyEvent::new(c, KeyModifiers::NONE)
    }

    fn picker(var_type: VarType, mode: Option<FileMode>, ext: &[&str]) -> PathPickerState {
        let mut p = PathPickerState::new(PickerTarget::VarForm(0), &decl(var_type, mode, ext));
        p.set_listing(listing(None));
        p
    }

    fn names(p: &PathPickerState) -> Vec<String> {
        p.rows().into_iter().map(|r| r.name).collect()
    }

    #[test]
    fn ext_hint_narrows_files_and_e_switches_it_off() {
        let mut p = picker(VarType::File, Some(FileMode::Open), &["mkv", "mp4"]);
        assert_eq!(names(&p), ["..", "raw", "a.mkv", "b.MP4"]);
        p.on_key(key(KeyCode::Char('e')));
        assert_eq!(names(&p), ["..", "raw", "a.mkv", "b.MP4", "notes.txt"]);
    }

    #[test]
    fn filter_matches_substrings_and_hides_the_parent_row() {
        let mut p = picker(VarType::File, Some(FileMode::Open), &[]);
        p.on_key(key(KeyCode::Char('/')));
        for c in "NOTES".chars() {
            p.on_key(key(KeyCode::Char(c)));
        }
        assert_eq!(names(&p), ["notes.txt"]);
        p.on_key(key(KeyCode::Esc));
        assert_eq!(p.filter, "");
        assert_eq!(p.focus, PickerFocus::List);
    }

    #[test]
    fn enter_descends_into_dirs_and_up_through_the_parent_row() {
        let mut p = picker(VarType::File, Some(FileMode::Open), &[]);
        p.selected = 1; // raw
        assert_eq!(p.on_key(key(KeyCode::Enter)), PickerAction::Navigate("/media/raw".into()));
        p.selected = 0; // ..
        assert_eq!(p.on_key(key(KeyCode::Enter)), PickerAction::Navigate("/".into()));
        assert_eq!(p.on_key(key(KeyCode::Backspace)), PickerAction::Navigate("/".into()));
    }

    #[test]
    fn open_mode_chooses_the_file_on_enter() {
        let mut p = picker(VarType::File, Some(FileMode::Open), &[]);
        p.selected = 2; // a.mkv
        assert_eq!(p.on_key(key(KeyCode::Enter)), PickerAction::Choose("/media/a.mkv".into()));
        // `s` is a directory picker's key, not a file picker's.
        assert_eq!(p.on_key(key(KeyCode::Char('s'))), PickerAction::Stay);
    }

    #[test]
    fn save_mode_takes_a_typed_name() {
        let mut p = picker(VarType::File, Some(FileMode::Save), &[]);
        assert_eq!(p.chosen_path(), None);
        p.on_key(key(KeyCode::Tab));
        assert_eq!(p.focus, PickerFocus::Name);
        for c in "out.mp4".chars() {
            p.on_key(key(KeyCode::Char(c)));
        }
        p.on_key(key(KeyCode::Char('/'))); // never part of a name
        assert_eq!(p.on_key(key(KeyCode::Enter)), PickerAction::Choose("/media/out.mp4".into()));
        // Enter on an existing file only fills the name in.
        p.focus = PickerFocus::List;
        p.selected = 2;
        assert_eq!(p.on_key(key(KeyCode::Enter)), PickerAction::Stay);
        assert_eq!(p.name, "a.mkv");
        assert_eq!(p.focus, PickerFocus::Name);
    }

    #[test]
    fn dir_picker_chooses_the_current_directory() {
        let mut p = picker(VarType::Dir, None, &[]);
        assert_eq!(p.on_key(key(KeyCode::Char('s'))), PickerAction::Choose("/media".into()));
        assert_eq!(p.on_key(key(KeyCode::Char(' '))), PickerAction::Choose("/media".into()));
    }

    #[test]
    fn dot_toggles_hidden_and_asks_for_a_relisting() {
        let mut p = picker(VarType::Dir, None, &[]);
        assert_eq!(p.on_key(key(KeyCode::Char('.'))), PickerAction::Navigate("/media".into()));
        assert!(p.show_hidden);
    }

    #[test]
    fn a_pre_selected_file_is_highlighted_and_offered_as_the_save_name() {
        let mut p = PathPickerState::new(
            PickerTarget::VarForm(0),
            &decl(VarType::File, Some(FileMode::Save), &[]),
        );
        p.set_listing(listing(Some("b.MP4")));
        assert_eq!(p.rows()[p.selected].name, "b.MP4");
        assert_eq!(p.name, "b.MP4");
    }

    #[test]
    fn second_click_activates_and_first_click_selects() {
        let mut p = picker(VarType::File, Some(FileMode::Open), &[]);
        assert_eq!(p.click_row(2), PickerAction::Stay);
        assert_eq!(p.selected, 2);
        assert_eq!(p.click_row(2), PickerAction::Choose("/media/a.mkv".into()));
        assert_eq!(p.click_row(99), PickerAction::Stay);
    }

    #[test]
    fn window_follows_the_selection_only_as_far_as_needed() {
        assert_eq!(window_start(0, 5), 0);
        assert_eq!(window_start(4, 5), 0);
        assert_eq!(window_start(5, 5), 1);
        assert_eq!(window_start(9, 5), 5);
    }

    #[test]
    fn escape_cancels_from_the_list_only() {
        let mut p = picker(VarType::File, Some(FileMode::Save), &[]);
        p.focus = PickerFocus::Name;
        assert_eq!(p.on_key(key(KeyCode::Esc)), PickerAction::Stay);
        assert_eq!(p.on_key(key(KeyCode::Esc)), PickerAction::Cancel);
    }
}
