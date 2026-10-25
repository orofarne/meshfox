//! The TUI's calendar for `date`/`datetime` variables (SPEC.md, "Date and
//! datetime variables"): a modal opened with `Ctrl-O` on such a field in the
//! variable form or a `form` fence, next to the field's own free-text input
//! (typing a value by hand keeps working). This module holds the state and
//! the key semantics (pure, no I/O); `ui.rs` draws it and `app.rs` wires it
//! in, mirroring `path_picker`.

use crossterm::event::{KeyCode, KeyEvent};
use meshfox_core::datevars;
use meshfox_core::VarType;
use time::{Date, Duration, Month, PrimitiveDateTime, Time, UtcOffset};

pub use super::path_picker::PickerTarget;

/// The visible "open the picker" affordance after a `date`/`datetime` field.
pub const PICK_MARKER: &str = "[Pick… ^o]";

/// The clickable marker after a field of this type, if it has a picker.
pub fn marker_for(var_type: VarType) -> Option<&'static str> {
    match var_type {
        VarType::Dir | VarType::File => Some(super::path_picker::BROWSE_MARKER),
        VarType::Date | VarType::DateTime => Some(PICK_MARKER),
        _ => None,
    }
}

/// Whether `Ctrl-O` / the marker opens a picker for this type.
pub fn has_picker(var_type: VarType) -> bool {
    marker_for(var_type).is_some()
}

/// Every offset a picker can step through: UTC-12:00 to UTC+14:00.
const MIN_OFFSET: i32 = -12 * 3600;
const MAX_OFFSET: i32 = 14 * 3600;
const OFFSET_STEP: i32 = 15 * 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DateFocus {
    Calendar,
    Hour,
    Minute,
    Second,
    Offset,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DateAction {
    Stay,
    Cancel,
    /// Write this canonical value into the target field and close.
    Choose(String),
}

pub struct DatePickerState {
    pub target: PickerTarget,
    pub title: String,
    /// `datetime`: the time-of-day and offset rows are shown too.
    pub with_time: bool,
    pub selected: Date,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
    /// Seconds east of UTC.
    pub offset: i32,
    pub focus: DateFocus,
}

fn days_in_month(year: i32, month: Month) -> u8 {
    month.length(year)
}

/// `date` shifted by `months`, the day clamped to the target month's length.
fn add_months(date: Date, months: i32) -> Date {
    let index = date.year() * 12 + (u8::from(date.month()) as i32 - 1) + months;
    let year = index.div_euclid(12);
    let month = Month::try_from(index.rem_euclid(12) as u8 + 1).expect("1..=12");
    let day = date.day().min(days_in_month(year, month));
    Date::from_calendar_date(year, month, day).unwrap_or(date)
}

impl DatePickerState {
    /// A picker for `var_type` (`Date` or `DateTime`) starting from the
    /// field's `current` text when it holds a usable value, otherwise from
    /// now (local time zone).
    pub fn new(
        target: PickerTarget,
        var_type: VarType,
        title: &str,
        current: &str,
    ) -> DatePickerState {
        let with_time = var_type == VarType::DateTime;
        let now = datevars::now_local();
        let mut st = DatePickerState {
            target,
            title: title.to_string(),
            with_time,
            selected: now.date(),
            hour: now.hour(),
            minute: now.minute(),
            second: 0,
            offset: now.offset().whole_seconds(),
            focus: DateFocus::Calendar,
        };
        if with_time {
            let parsed = datevars::parse_datetime(current.trim()).or_else(|| {
                datevars::normalize_datetime_input(current)
                    .ok()
                    .and_then(|v| datevars::parse_datetime(&v))
            });
            if let Some(dt) = parsed {
                st.selected = dt.date();
                st.hour = dt.hour();
                st.minute = dt.minute();
                st.second = dt.second();
                st.offset = dt.offset().whole_seconds();
            }
        } else if let Some(d) = datevars::parse_date(current.trim()) {
            st.selected = d;
        }
        st
    }

    /// The canonical value the picker currently stands for.
    pub fn value(&self) -> String {
        if !self.with_time {
            return datevars::format_date(self.selected);
        }
        let time = Time::from_hms(self.hour, self.minute, self.second).unwrap_or(Time::MIDNIGHT);
        let offset = UtcOffset::from_whole_seconds(self.offset).unwrap_or(UtcOffset::UTC);
        datevars::format_datetime(PrimitiveDateTime::new(self.selected, time).assume_offset(offset))
    }

    /// `(year, month)` of the month on display.
    pub fn view(&self) -> (i32, Month) {
        (self.selected.year(), self.selected.month())
    }

    /// The Monday on or before the 1st of the displayed month: the first of
    /// the 42 cells (6 weeks × 7 days) the grid shows.
    pub fn grid_start(&self) -> Date {
        let (year, month) = self.view();
        let first = Date::from_calendar_date(year, month, 1).unwrap_or(self.selected);
        let back = i64::from(first.weekday().number_days_from_monday());
        first.checked_sub(Duration::days(back)).unwrap_or(Date::MIN)
    }

    /// The date in grid cell `index` (0..42, row-major, Monday first).
    pub fn cell_date(&self, index: usize) -> Option<Date> {
        self.grid_start().checked_add(Duration::days(index as i64))
    }

    fn shift_days(&mut self, days: i64) {
        if let Some(d) = self.selected.checked_add(Duration::days(days)) {
            self.selected = d;
        }
    }

    pub fn shift_months(&mut self, months: i32) {
        self.selected = add_months(self.selected, months);
    }

    /// Selects `date` (a click on a cell); `true` when it already was the
    /// selection, i.e. this click confirms it.
    pub fn click_date(&mut self, date: Date) -> bool {
        self.focus = DateFocus::Calendar;
        let was = self.selected == date;
        self.selected = date;
        was
    }

    pub fn set_today(&mut self) {
        self.selected = datevars::today_local();
    }

    /// Calendar date, wall clock and offset all set to now.
    pub fn set_now(&mut self) {
        let now = datevars::now_local();
        self.selected = now.date();
        if self.with_time {
            self.hour = now.hour();
            self.minute = now.minute();
            self.second = now.second();
            self.offset = now.offset().whole_seconds();
        }
    }

    fn fields(&self) -> &'static [DateFocus] {
        if self.with_time {
            &[
                DateFocus::Calendar,
                DateFocus::Hour,
                DateFocus::Minute,
                DateFocus::Second,
                DateFocus::Offset,
            ]
        } else {
            &[DateFocus::Calendar]
        }
    }

    fn move_focus(&mut self, delta: isize) {
        let fields = self.fields();
        let i = fields.iter().position(|f| *f == self.focus).unwrap_or(0) as isize;
        self.focus = fields[(i + delta).rem_euclid(fields.len() as isize) as usize];
    }

    /// Steps the focused time field (or the offset) by `delta` units.
    fn adjust(&mut self, delta: i32) {
        match self.focus {
            DateFocus::Hour => self.hour = (self.hour as i32 + delta).rem_euclid(24) as u8,
            DateFocus::Minute => self.minute = (self.minute as i32 + delta).rem_euclid(60) as u8,
            DateFocus::Second => self.second = (self.second as i32 + delta).rem_euclid(60) as u8,
            DateFocus::Offset => {
                self.offset = (self.offset + delta * OFFSET_STEP).clamp(MIN_OFFSET, MAX_OFFSET);
            }
            DateFocus::Calendar => {}
        }
    }

    pub fn on_key(&mut self, key: KeyEvent) -> DateAction {
        match key.code {
            KeyCode::Esc => return DateAction::Cancel,
            KeyCode::Enter => return DateAction::Choose(self.value()),
            KeyCode::Tab => self.move_focus(1),
            KeyCode::BackTab => self.move_focus(-1),
            KeyCode::Char('t') => self.set_today(),
            KeyCode::Char('n') => self.set_now(),
            _ => match self.focus {
                DateFocus::Calendar => match key.code {
                    KeyCode::Left => self.shift_days(-1),
                    KeyCode::Right => self.shift_days(1),
                    KeyCode::Up => self.shift_days(-7),
                    KeyCode::Down => self.shift_days(7),
                    KeyCode::PageUp | KeyCode::Char('[') => self.shift_months(-1),
                    KeyCode::PageDown | KeyCode::Char(']') => self.shift_months(1),
                    KeyCode::Home => {
                        let (y, m) = self.view();
                        if let Ok(d) = Date::from_calendar_date(y, m, 1) {
                            self.selected = d;
                        }
                    }
                    KeyCode::End => {
                        let (y, m) = self.view();
                        if let Ok(d) = Date::from_calendar_date(y, m, days_in_month(y, m)) {
                            self.selected = d;
                        }
                    }
                    _ => {}
                },
                _ => match key.code {
                    KeyCode::Up => self.adjust(1),
                    KeyCode::Down => self.adjust(-1),
                    KeyCode::PageUp => self.adjust(10),
                    KeyCode::PageDown => self.adjust(-10),
                    KeyCode::Left => self.move_focus(-1),
                    KeyCode::Right => self.move_focus(1),
                    _ => {}
                },
            },
        }
        DateAction::Stay
    }
}

/// Column headers of the grid, Monday first.
pub const WEEKDAYS: [&str; 7] = ["Mo", "Tu", "We", "Th", "Fr", "Sa", "Su"];

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyModifiers;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn date_picker(current: &str) -> DatePickerState {
        DatePickerState::new(PickerTarget::VarForm(0), VarType::Date, "Day", current)
    }

    fn datetime_picker(current: &str) -> DatePickerState {
        DatePickerState::new(PickerTarget::VarForm(0), VarType::DateTime, "When", current)
    }

    fn d(s: &str) -> Date {
        datevars::parse_date(s).unwrap()
    }

    #[test]
    fn it_starts_from_the_fields_current_value() {
        assert_eq!(date_picker("2026-10-25").selected, d("2026-10-25"));
        let p = datetime_picker("2026-10-25T14:30:15+03:00");
        assert_eq!(p.selected, d("2026-10-25"));
        assert_eq!((p.hour, p.minute, p.second, p.offset), (14, 30, 15, 10800));
        assert_eq!(p.value(), "2026-10-25T14:30:15+03:00");
        // A typed, not yet normalized value is understood too.
        let p = datetime_picker("2026-10-25 08:05Z");
        assert_eq!((p.hour, p.minute, p.offset), (8, 5, 0));
        // Garbage falls back to today instead of failing.
        assert_eq!(date_picker("nonsense").selected, datevars::today_local());
    }

    #[test]
    fn arrows_move_by_day_and_week_and_enter_chooses() {
        let mut p = date_picker("2026-10-25");
        p.on_key(key(KeyCode::Right));
        assert_eq!(p.selected, d("2026-10-26"));
        p.on_key(key(KeyCode::Down));
        assert_eq!(p.selected, d("2026-11-02"));
        p.on_key(key(KeyCode::Left));
        p.on_key(key(KeyCode::Up));
        assert_eq!(p.selected, d("2026-10-25"));
        assert_eq!(
            p.on_key(key(KeyCode::Enter)),
            DateAction::Choose("2026-10-25".into())
        );
        assert_eq!(p.on_key(key(KeyCode::Esc)), DateAction::Cancel);
    }

    #[test]
    fn paging_months_clamps_the_day() {
        let mut p = date_picker("2026-01-31");
        p.on_key(key(KeyCode::PageDown));
        assert_eq!(p.selected, d("2026-02-28"));
        p.on_key(key(KeyCode::PageDown));
        assert_eq!(p.selected, d("2026-03-28"));
        p.on_key(key(KeyCode::PageUp));
        p.on_key(key(KeyCode::PageUp));
        p.on_key(key(KeyCode::PageUp));
        assert_eq!(p.selected, d("2025-12-28"));
    }

    #[test]
    fn home_and_end_jump_within_the_month() {
        let mut p = date_picker("2024-02-10");
        p.on_key(key(KeyCode::End));
        assert_eq!(p.selected, d("2024-02-29"));
        p.on_key(key(KeyCode::Home));
        assert_eq!(p.selected, d("2024-02-01"));
    }

    #[test]
    fn the_grid_is_monday_first_and_covers_the_month() {
        // 2026-10-01 is a Thursday: the grid starts Monday 2026-09-28.
        let p = date_picker("2026-10-25");
        assert_eq!(p.grid_start(), d("2026-09-28"));
        assert_eq!(p.cell_date(3), Some(d("2026-10-01")));
        assert_eq!(p.cell_date(41), Some(d("2026-11-08")));
        // A month starting on Monday starts the grid on its 1st.
        assert_eq!(date_picker("2026-06-15").grid_start(), d("2026-06-01"));
    }

    #[test]
    fn a_date_picker_has_no_time_rows() {
        let mut p = date_picker("2026-10-25");
        p.on_key(key(KeyCode::Tab));
        assert_eq!(p.focus, DateFocus::Calendar);
    }

    #[test]
    fn tab_walks_the_time_fields_and_arrows_adjust_them() {
        let mut p = datetime_picker("2026-10-25T23:59:58+03:00");
        p.on_key(key(KeyCode::Tab));
        assert_eq!(p.focus, DateFocus::Hour);
        p.on_key(key(KeyCode::Up));
        assert_eq!(p.hour, 0); // wraps
        p.on_key(key(KeyCode::Right));
        assert_eq!(p.focus, DateFocus::Minute);
        p.on_key(key(KeyCode::Up));
        assert_eq!(p.minute, 0);
        p.on_key(key(KeyCode::Tab));
        p.on_key(key(KeyCode::PageUp));
        assert_eq!(p.second, 8);
        p.on_key(key(KeyCode::Tab));
        assert_eq!(p.focus, DateFocus::Offset);
        p.on_key(key(KeyCode::Up));
        assert_eq!(p.offset, 10800 + 900);
        p.on_key(key(KeyCode::Tab));
        assert_eq!(p.focus, DateFocus::Calendar);
        assert_eq!(p.value(), "2026-10-25T00:00:08+03:15");
    }

    #[test]
    fn the_offset_stays_within_real_zones() {
        let mut p = datetime_picker("2026-10-25T10:00:00+14:00");
        p.focus = DateFocus::Offset;
        p.on_key(key(KeyCode::Up));
        assert_eq!(p.offset, 14 * 3600);
        let mut p = datetime_picker("2026-10-25T10:00:00-12:00");
        p.focus = DateFocus::Offset;
        p.on_key(key(KeyCode::Down));
        assert_eq!(p.offset, -12 * 3600);
    }

    #[test]
    fn today_and_now_shortcuts() {
        let mut p = date_picker("2001-01-01");
        p.on_key(key(KeyCode::Char('t')));
        assert_eq!(p.selected, datevars::today_local());
        let mut p = datetime_picker("2001-01-01T00:00:00Z");
        p.on_key(key(KeyCode::Char('n')));
        assert!(datevars::parse_datetime(&p.value()).is_some());
        assert_ne!(p.selected, d("2001-01-01"));
    }

    #[test]
    fn clicking_the_selection_again_confirms_it() {
        let mut p = date_picker("2026-10-25");
        assert!(!p.click_date(d("2026-10-27")));
        assert!(p.click_date(d("2026-10-27")));
        assert_eq!(p.value(), "2026-10-27");
    }

    #[test]
    fn markers_follow_the_field_type() {
        assert_eq!(marker_for(VarType::File), Some("[Browse… ^o]"));
        assert_eq!(marker_for(VarType::Date), Some(PICK_MARKER));
        assert_eq!(marker_for(VarType::DateTime), Some(PICK_MARKER));
        assert_eq!(marker_for(VarType::Int), None);
        assert!(has_picker(VarType::Dir));
        assert!(!has_picker(VarType::Select));
    }
}
