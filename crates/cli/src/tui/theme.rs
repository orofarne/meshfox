//! TUI color roles. The palette itself lives in `meshfox_core::tui_theme`
//! (a built-in `darkfox` plus user themes from `.meshfox/themes/tui/`);
//! this module holds the *active* theme process-wide and turns its colors
//! into `ratatui` ones. Call sites ask for a role (`theme::muted()`,
//! `theme::accent()`) instead of hardcoding a `Color`, so a theme can
//! retune the whole TUI. Until `set` is called (and in tests) the active
//! theme is `darkfox`.

use std::sync::{Arc, RwLock};

use meshfox_core::tui_theme::{AnsiColor, Theme, ThemeColor};
use ratatui::style::Color;

static CURRENT: RwLock<Option<Arc<Theme>>> = RwLock::new(None);

/// Makes `theme` the active one.
pub fn set(theme: Theme) {
    *CURRENT.write().unwrap_or_else(|e| e.into_inner()) = Some(Arc::new(theme));
}

/// The active theme (`darkfox` until `set` is called).
pub fn current() -> Arc<Theme> {
    if let Some(t) = CURRENT.read().unwrap_or_else(|e| e.into_inner()).as_ref() {
        return t.clone();
    }
    let mut slot = CURRENT.write().unwrap_or_else(|e| e.into_inner());
    slot.get_or_insert_with(|| Arc::new(Theme::darkfox()))
        .clone()
}

pub fn to_color(c: ThemeColor) -> Color {
    match c {
        ThemeColor::Rgb(r, g, b) => Color::Rgb(r, g, b),
        ThemeColor::Indexed(n) => Color::Indexed(n),
        ThemeColor::Default => Color::Reset,
        ThemeColor::Ansi(a) => match a {
            AnsiColor::Black => Color::Black,
            AnsiColor::Red => Color::Red,
            AnsiColor::Green => Color::Green,
            AnsiColor::Yellow => Color::Yellow,
            AnsiColor::Blue => Color::Blue,
            AnsiColor::Magenta => Color::Magenta,
            AnsiColor::Cyan => Color::Cyan,
            AnsiColor::Gray => Color::Gray,
            AnsiColor::DarkGray => Color::DarkGray,
            AnsiColor::LightRed => Color::LightRed,
            AnsiColor::LightGreen => Color::LightGreen,
            AnsiColor::LightYellow => Color::LightYellow,
            AnsiColor::LightBlue => Color::LightBlue,
            AnsiColor::LightMagenta => Color::LightMagenta,
            AnsiColor::LightCyan => Color::LightCyan,
            AnsiColor::White => Color::White,
        },
    }
}

macro_rules! roles {
    ($($(#[$m:meta])* $name:ident),+ $(,)?) => {
        $($(#[$m])* pub fn $name() -> Color { to_color(current().ui.$name) })+
    };
}

roles! {
    /// Focused pane border, every popup's border, selection fill.
    accent,
    /// A block's dependency line.
    dep,
    fail,
    ok,
    /// Unfocused pane border.
    border,
    /// Secondary text, separators, hints.
    muted,
    /// Editable values, checked boxes.
    value,
    tag,
    warn,
    error,
    /// Text drawn on an `accent` fill.
    on_accent,
    link,
    search_match_bg,
    search_other_bg,
    search_fg,
    code_bg,
    code_fg,
    table_stripe_bg,
    map_node_bg,
    map_selected_bg,
    map_node_fg,
    callout_note,
    callout_tip,
    callout_important,
    callout_warning,
    callout_caution,
}

/// Read-mode selection shares the editor's selection/foreground pair, so
/// every built-in and custom theme supplies a coordinated contrast pair.
pub fn selection_bg() -> Color {
    to_color(current().editor.selection)
}
pub fn selection_fg() -> Color {
    to_color(current().editor.fg)
}

/// Markdown heading color for 0-based level index `idx` (levels beyond 6
/// reuse the last).
pub fn heading(idx: usize) -> Color {
    let t = current();
    let u = &t.ui;
    to_color(match idx {
        0 => u.heading_1,
        1 => u.heading_2,
        2 => u.heading_3,
        3 => u.heading_4,
        4 => u.heading_5,
        _ => u.heading_6,
    })
}

/// Activates the theme named by `[tui] theme` in `config.toml` (default
/// `darkfox`). On a missing or broken theme, keeps `darkfox` and returns
/// the reason, for the caller to show.
pub fn init(canvas_root: &std::path::Path) -> Option<String> {
    // Tests build many `App`s in parallel and must not pick up the theme
    // from the developer's own `~/.meshfox/config.toml`: they run on
    // `darkfox`.
    if cfg!(test) {
        let _ = canvas_root;
        return None;
    }
    let name = meshfox_core::config::load(canvas_root)
        .get("tui")
        .and_then(|v| v.as_table())
        .and_then(|t| t.get("theme"))
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let Some(name) = name else {
        set(Theme::darkfox());
        return None;
    };
    match Theme::load(canvas_root, &name) {
        Ok(theme) => {
            set(theme);
            None
        }
        Err(e) => {
            set(Theme::darkfox());
            Some(format!("{e}; using darkfox"))
        }
    }
}

/// The colors SVG output is themed with (`var(--mf-fg)` etc., see
/// `meshfox_core::svg`). SVG needs literal colors, so ANSI ones are
/// approximated; a `default` falls back to `darkfox`'s value.
pub fn svg() -> meshfox_core::svg::Theme {
    let t = current();
    // `default` (the terminal's own color) has no literal value; SVG falls
    // back to the dark theme's.
    let hex = |c: ThemeColor, fallback: &str| match c.approx_rgb() {
        Some((r, g, b)) => format!("#{r:02x}{g:02x}{b:02x}"),
        None => fallback.to_string(),
    };
    meshfox_core::svg::Theme {
        fg: hex(t.ui.fg, "#f2ede6"),
        bg: hex(t.ui.bg, "#131316"),
        accent: hex(t.ui.accent, "#ff6e15"),
        border: hex(t.ui.svg_border, "#7a4620"),
    }
}

/// Gives every cell that still has the terminal's default background or
/// foreground the theme's `[ui] bg`/`fg`. Run once on the finished frame,
/// so popups and cleared areas get them too. With `default` for both (as
/// in `darkfox`) it does nothing and the terminal's own colors show; a
/// theme that sets them paints the whole screen, so it looks the same in a
/// light or a dark terminal.
pub fn apply_canvas(buf: &mut ratatui::buffer::Buffer) {
    let theme = current();
    fill_defaults(buf, to_color(theme.ui.bg), to_color(theme.ui.fg));
}

fn fill_defaults(buf: &mut ratatui::buffer::Buffer, bg: Color, fg: Color) {
    if bg == Color::Reset && fg == Color::Reset {
        return;
    }
    for cell in buf.content.iter_mut() {
        if cell.bg == Color::Reset {
            cell.bg = bg;
        }
        if cell.fg == Color::Reset {
            cell.fg = fg;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    use ratatui::style::Style;

    #[test]
    fn fills_only_cells_that_kept_the_terminal_defaults() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 3, 1));
        buf[(1, 0)].set_style(Style::default().fg(Color::Red).bg(Color::Blue));
        fill_defaults(&mut buf, Color::Rgb(1, 2, 3), Color::Rgb(4, 5, 6));
        assert_eq!(buf[(0, 0)].bg, Color::Rgb(1, 2, 3));
        assert_eq!(buf[(0, 0)].fg, Color::Rgb(4, 5, 6));
        assert_eq!(buf[(1, 0)].bg, Color::Blue);
        assert_eq!(buf[(1, 0)].fg, Color::Red);
    }

    #[test]
    fn default_colors_leave_the_frame_alone() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 2, 1));
        fill_defaults(&mut buf, Color::Reset, Color::Reset);
        assert_eq!(buf[(0, 0)].bg, Color::Reset);
        assert_eq!(buf[(0, 0)].fg, Color::Reset);
    }
}
