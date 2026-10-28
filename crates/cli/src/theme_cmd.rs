//! `meshfox theme-preview`: shows what the available TUI themes — the
//! built-in ones and any in `.meshfox/themes/tui/` — look like. By default
//! it paints them into the current terminal, so ANSI colors (`ansi:…`)
//! appear in the terminal's own palette, exactly as the TUI will show
//! them; with `--html` it writes one HTML page instead.
//!
//! Both outputs come from one model — `preview_rows` builds each theme's
//! preview, frame included, as rows of colored segments — and differ only
//! in how a segment is written (SGR codes or a `<span>`), so they always
//! show the same thing. The editor sample goes through the same grammar and
//! palette-built `syntect` theme the TUI's editor uses.

use std::path::{Path, PathBuf};

use meshfox_core::tui_theme::{AnsiColor, Theme, ThemeColor};
use syntect::easy::HighlightLines;
use syntect::highlighting::Color;
use syntect::parsing::{SyntaxReference, SyntaxSet};

const PAGE: &str = include_str!("templates/theme_preview.html");

const SAMPLE: &str = "<!-- meshfox:canvas -->
# kotoprovod.gitazone.org
<!-- meshfox:node id=\"root\" -->

## Локальная разработка

```bash name=\"run\" deps=\"npm-install\" service
npm run start
```

| Поле   | Назначение  |
| ------ | ----------- |
| status | catalog-new |
";

pub fn run(dir: PathBuf, html: bool, out: Option<PathBuf>, only: &[String]) -> Result<(), String> {
    let text = if html {
        render_html(&dir, only)?
    } else {
        render_terminal(&dir, only)?
    };
    match out {
        Some(path) => std::fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display())),
        None => {
            print!("{text}");
            Ok(())
        }
    }
}

/// The themes to show: all available ones, or just those named.
fn selected(canvas_root: &Path, only: &[String]) -> Result<Vec<String>, String> {
    let available = Theme::available(canvas_root);
    if only.is_empty() {
        return Ok(available);
    }
    for name in only {
        if !available.contains(name) {
            return Err(format!(
                "no theme {name:?} (available: {})",
                available.join(", ")
            ));
        }
    }
    Ok(only.to_vec())
}

/// A run of text in one foreground/background (`None`: leave as is).
struct Seg {
    text: String,
    fg: Option<ThemeColor>,
    bg: Option<ThemeColor>,
}

/// One preview row: segments plus the visible width, so rows can be padded
/// and boxed without counting escape codes or tags.
#[derive(Default)]
struct Row {
    segs: Vec<Seg>,
    width: usize,
}

impl Row {
    fn push(&mut self, text: &str, fg: Option<ThemeColor>, bg: Option<ThemeColor>) {
        self.width += text.chars().count();
        self.segs.push(Seg {
            text: text.to_string(),
            fg,
            bg,
        });
    }

    /// Pads to `width` with `bg` (or the terminal's own background).
    fn pad_to(&mut self, width: usize, bg: Option<ThemeColor>) {
        self.push(&" ".repeat(width.saturating_sub(self.width)), None, bg);
    }

    fn append(&mut self, other: Row) {
        self.width += other.width;
        self.segs.extend(other.segs);
    }
}

/// `┌─ title ───┐`, `inner + 2` columns wide.
fn box_top(inner: usize, title: &str, color: ThemeColor, title_color: ThemeColor) -> Row {
    let label = format!(" {title} ");
    let rest = inner.saturating_sub(1 + label.chars().count());
    let mut row = Row::default();
    row.push("┌─", Some(color), None);
    row.push(&label, Some(title_color), None);
    row.push(&format!("{}┐", "─".repeat(rest)), Some(color), None);
    row
}

/// `└─ label ───┘`; an empty `label` leaves the line plain.
fn box_bottom(inner: usize, label: &str, color: ThemeColor, label_color: ThemeColor) -> Row {
    let label = if label.is_empty() {
        String::new()
    } else {
        format!(" {label} ")
    };
    let rest = inner.saturating_sub(1 + label.chars().count());
    let mut row = Row::default();
    row.push("└─", Some(color), None);
    row.push(&label, Some(label_color), None);
    row.push(&format!("{}┘", "─".repeat(rest)), Some(color), None);
    row
}

/// `row` padded to `inner` columns between `│` borders.
fn side(mut row: Row, inner: usize, color: ThemeColor) -> Row {
    row.pad_to(inner, None);
    let mut out = Row::default();
    out.push("│", Some(color), None);
    out.append(row);
    out.push("│", Some(color), None);
    out
}

/// One theme's preview, frame included: an unfocused pane (`border`)
/// showing the interface colors, with the editor sample in a focused one
/// (`accent`) inside it, as in the TUI. Every row is the same width.
fn preview_rows(
    name: &str,
    theme: &Theme,
    syntaxes: &SyntaxSet,
    syntax: &SyntaxReference,
) -> Result<Vec<Row>, String> {
    let u = &theme.ui;

    // Interface colors, on the terminal's own background as in the TUI.
    let mut strip = Row::default();
    strip.push(" ", None, None);
    for (i, (label, c)) in [
        ("accent", u.accent),
        ("dep", u.dep),
        ("ok", u.ok),
        ("fail", u.fail),
        ("muted", u.muted),
        ("value", u.value),
        ("tag", u.tag),
        ("warn", u.warn),
        ("error", u.error),
        ("link", u.link),
    ]
    .into_iter()
    .enumerate()
    {
        if i > 0 {
            strip.push("  ", None, None);
        }
        strip.push(label, Some(c), None);
    }

    let mut marks = Row::default();
    marks.push(" ", None, None);
    for (i, c) in [
        u.heading_1,
        u.heading_2,
        u.heading_3,
        u.heading_4,
        u.heading_5,
        u.heading_6,
    ]
    .into_iter()
    .enumerate()
    {
        marks.push(&format!("H{} ", i + 1), Some(c), None);
    }
    marks.push("  ", None, None);
    for (label, c) in [
        ("note", u.callout_note),
        ("tip", u.callout_tip),
        ("important", u.callout_important),
        ("warning", u.callout_warning),
        ("caution", u.callout_caution),
    ] {
        marks.push(label, Some(c), None);
        marks.push("  ", None, None);
    }

    let mut chips = Row::default();
    chips.push(" ", None, None);
    chips.push(" selected row ", Some(u.on_accent), Some(u.accent));
    chips.push("  ", None, None);
    chips.push(" match ", Some(u.search_fg), Some(u.search_match_bg));
    chips.push("  ", None, None);
    chips.push(" inline code ", Some(u.code_fg), Some(u.code_bg));

    // The editor sample: syntect only takes literal colors, so ANSI ones
    // show up as the approximated xterm shade here, as in the TUI.
    let syntect_theme = crate::syntax_registry::syntect_theme_from_palette(&theme.editor);
    let editor_bg = Some(theme.editor.bg);
    let mut hl = HighlightLines::new(syntax, &syntect_theme);
    let sample_width = SAMPLE.lines().map(|l| l.chars().count()).max().unwrap_or(0) + 2;
    let mut sample = Vec::new();
    for line in SAMPLE.lines() {
        let ranges = hl
            .highlight_line(line, syntaxes)
            .map_err(|e| e.to_string())?;
        let mut row = Row::default();
        row.push(" ", None, editor_bg);
        for (style, text) in &ranges {
            let Color { r, g, b, .. } = style.foreground;
            row.push(text, Some(ThemeColor::Rgb(r, g, b)), editor_bg);
        }
        row.pad_to(sample_width, editor_bg);
        sample.push(row);
    }

    let inner = [&strip, &marks, &chips]
        .iter()
        .map(|r| r.width)
        .max()
        .unwrap_or(0)
        .max(sample_width + 6);
    // The nested box sits between `│ ` and ` │`: 4 columns of margin.
    let focus_inner = inner - 4;
    let mut focused = vec![box_top(focus_inner, "Source", u.accent, u.accent)];
    focused.extend(sample.into_iter().map(|r| side(r, focus_inner, u.accent)));
    focused.push(box_bottom(focus_inner, "", u.accent, u.muted));

    let mut rows = vec![box_top(inner, name, u.border, u.accent)];
    rows.extend(
        [strip, marks, chips, Row::default()]
            .into_iter()
            .map(|r| side(r, inner, u.border)),
    );
    for line in focused {
        let mut padded = Row::default();
        padded.push(" ", None, None);
        padded.append(line);
        padded.push(" ", None, None);
        rows.push(side(padded, inner, u.border));
    }
    rows.push(box_bottom(
        inner,
        &format!("[tui] theme = \"{name}\""),
        u.border,
        u.muted,
    ));
    Ok(rows)
}

/// Every selected theme's preview rows, with the theme they came from.
fn previews(canvas_root: &Path, only: &[String]) -> Result<Vec<(Theme, Vec<Row>)>, String> {
    let syntaxes = crate::syntax_registry::build_syntax_set(canvas_root);
    let syntax = syntaxes
        .find_syntax_by_name(crate::syntax_registry::MESHFOX_MARKDOWN_SYNTAX_NAME)
        .ok_or("the bundled meshfox-markdown grammar is missing")?;
    let mut out = Vec::new();
    for name in selected(canvas_root, only)? {
        match Theme::load(canvas_root, &name) {
            Ok(theme) => {
                let rows = preview_rows(&name, &theme, &syntaxes, syntax)?;
                out.push((theme, rows));
            }
            Err(e) => eprintln!("meshfox theme-preview: skipping {name}: {e}"),
        }
    }
    Ok(out)
}

/// Whether the terminal takes 24-bit colors; otherwise RGB is mapped to
/// the 256-color palette.
fn truecolor() -> bool {
    std::env::var("COLORTERM").is_ok_and(|v| v == "truecolor" || v == "24bit")
}

fn cube_256(r: u8, g: u8, b: u8) -> u8 {
    let level = |v: u8| ((v as u16 * 5 + 127) / 255) as u8;
    16 + 36 * level(r) + 6 * level(g) + level(b)
}

/// SGR parameters for a foreground (`bg == false`) or background color.
fn sgr(color: ThemeColor, bg: bool, truecolor: bool) -> String {
    let (base, ext) = if bg { (40, 48) } else { (30, 38) };
    match color {
        ThemeColor::Default => format!("{}", base + 9),
        ThemeColor::Indexed(n) => format!("{ext};5;{n}"),
        ThemeColor::Rgb(r, g, b) if truecolor => format!("{ext};2;{r};{g};{b}"),
        ThemeColor::Rgb(r, g, b) => format!("{ext};5;{}", cube_256(r, g, b)),
        ThemeColor::Ansi(a) => {
            let (n, bright) = match a {
                AnsiColor::Black => (0, false),
                AnsiColor::Red => (1, false),
                AnsiColor::Green => (2, false),
                AnsiColor::Yellow => (3, false),
                AnsiColor::Blue => (4, false),
                AnsiColor::Magenta => (5, false),
                AnsiColor::Cyan => (6, false),
                AnsiColor::Gray => (7, false),
                AnsiColor::DarkGray => (0, true),
                AnsiColor::LightRed => (1, true),
                AnsiColor::LightGreen => (2, true),
                AnsiColor::LightYellow => (3, true),
                AnsiColor::LightBlue => (4, true),
                AnsiColor::LightMagenta => (5, true),
                AnsiColor::LightCyan => (6, true),
                AnsiColor::White => (7, true),
            };
            format!("{}", base + n + if bright { 60 } else { 0 })
        }
    }
}

/// `text` painted `fg` on `bg` (either may be `None` for "leave as is").
fn paint(text: &str, fg: Option<ThemeColor>, bg: Option<ThemeColor>, tc: bool) -> String {
    let mut params = Vec::new();
    if let Some(bg) = bg {
        params.push(sgr(bg, true, tc));
    }
    if let Some(fg) = fg {
        params.push(sgr(fg, false, tc));
    }
    if params.is_empty() {
        return text.to_string();
    }
    format!("\x1b[{}m{text}\x1b[0m", params.join(";"))
}

fn render_terminal(canvas_root: &Path, only: &[String]) -> Result<String, String> {
    let tc = truecolor();
    let mut out = String::new();
    for (theme, rows) in previews(canvas_root, only)? {
        out.push('\n');
        for row in rows {
            for seg in &row.segs {
                // Like the TUI, fill what has no color of its own with the
                // theme's screen colors (no-ops for `default`).
                let fg = seg.fg.or(Some(theme.ui.fg));
                let bg = seg.bg.or(Some(theme.ui.bg));
                out.push_str(&paint(&seg.text, fg, bg, tc));
            }
            out.push('\n');
        }
    }
    Ok(out)
}

/// `#rrggbb` for CSS: exact for RGB, the common xterm shade for ANSI
/// colors (a browser has no terminal palette), `None` for `default`.
fn css_hex(c: ThemeColor) -> Option<String> {
    c.approx_rgb()
        .map(|(r, g, b)| format!("#{r:02x}{g:02x}{b:02x}"))
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn render_html(canvas_root: &Path, only: &[String]) -> Result<String, String> {
    let mut cards = String::new();
    for (theme, rows) in previews(canvas_root, only)? {
        // The "terminal" the preview sits on: the theme's own background
        // and text color.
        let backdrop = css_hex(theme.ui.bg).unwrap_or_else(|| "#131316".into());
        let text = css_hex(theme.ui.fg).unwrap_or_else(|| "#f2ede6".into());
        let mut body = String::new();
        for row in &rows {
            for seg in &row.segs {
                let mut style = String::new();
                if let Some(fg) = seg.fg.and_then(css_hex) {
                    style.push_str(&format!("color:{fg};"));
                }
                if let Some(bg) = seg.bg.and_then(css_hex) {
                    style.push_str(&format!("background:{bg};"));
                }
                if style.is_empty() {
                    body.push_str(&escape(&seg.text));
                } else {
                    body.push_str(&format!(
                        "<span style=\"{style}\">{}</span>",
                        escape(&seg.text)
                    ));
                }
            }
            body.push('\n');
        }
        cards.push_str(&format!(
            "    <section class=\"card\">\n      <div class=\"term\" style=\"background:{backdrop};color:{text}\">{body}</div>\n    </section>\n"
        ));
    }
    Ok(PAGE.replace("{{CARDS}}", &cards))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The text of the first preview card: tags dropped, entities undone.
    fn html_text(html: &str) -> String {
        let start = html.find("<div class=\"term\"").unwrap();
        let start = start + html[start..].find('>').unwrap() + 1;
        let end = start + html[start..].find("</div>").unwrap();
        let mut text = String::new();
        let mut in_tag = false;
        for c in html[start..end].chars() {
            match c {
                '<' => in_tag = true,
                '>' => in_tag = false,
                c if !in_tag => text.push(c),
                _ => {}
            }
        }
        text.replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&quot;", "\"")
            .replace("&amp;", "&")
    }

    #[test]
    fn colors_become_the_right_sgr_codes() {
        let rgb = ThemeColor::Rgb(255, 110, 21);
        assert_eq!(sgr(rgb, false, true), "38;2;255;110;21");
        assert_eq!(sgr(rgb, true, true), "48;2;255;110;21");
        assert_eq!(sgr(rgb, false, false), "38;5;208");
        assert_eq!(
            sgr(ThemeColor::Ansi(AnsiColor::DarkGray), false, true),
            "90"
        );
        assert_eq!(sgr(ThemeColor::Ansi(AnsiColor::Red), true, true), "41");
        assert_eq!(sgr(ThemeColor::Indexed(8), false, true), "38;5;8");
        assert_eq!(sgr(ThemeColor::Default, true, true), "49");
    }

    #[test]
    fn unpainted_text_is_left_alone_and_painted_text_is_reset() {
        assert_eq!(paint("x", None, None, true), "x");
        assert_eq!(
            paint("x", Some(ThemeColor::Ansi(AnsiColor::Green)), None, true),
            "\x1b[32mx\x1b[0m"
        );
    }

    #[test]
    fn terminal_preview_names_every_builtin_theme_and_rejects_unknown_ones() {
        let root = std::env::temp_dir().join("meshfox-theme-preview-never-created");
        let text = render_terminal(&root, &[]).unwrap();
        for name in Theme::available(&root) {
            assert!(text.contains(&name), "{name}");
        }
        assert!(render_terminal(&root, &["nope".into()]).is_err());
        assert!(
            html_text(&render_html(&root, &["monokai".into()]).unwrap()).contains("┌─ monokai ")
        );
    }

    /// The point of one shared model: the terminal and the HTML page show
    /// the same text, frame included, row for row.
    #[test]
    fn terminal_and_html_previews_show_exactly_the_same_text() {
        let root = std::env::temp_dir().join("meshfox-theme-preview-never-created");
        let only = ["everforest".to_string()];

        let mut terminal = String::new();
        let painted = render_terminal(&root, &only).unwrap();
        let mut chars = painted.chars();
        while let Some(c) = chars.next() {
            if c == '\x1b' {
                for c in chars.by_ref() {
                    if c == 'm' {
                        break;
                    }
                }
            } else {
                terminal.push(c);
            }
        }

        let from_html = html_text(&render_html(&root, &only).unwrap());

        assert_eq!(terminal.trim(), from_html.trim());
        assert!(from_html.contains("┌─ everforest "));
        assert!(from_html.contains("┌─ Source "));
    }
}
