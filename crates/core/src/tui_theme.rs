//! TUI color themes: `darkfox` built in, plus any
//! `<canvas>/.meshfox/themes/tui/<name>.toml` or
//! `~/.meshfox/themes/tui/<name>.toml` (local wins over global wins over
//! built-in). Selected by `[tui] theme = "<name>"` in `config.toml`.
//!
//! This module only knows *data*: it has no `ratatui`/`syntect` dependency,
//! so `meshfox-cli` converts a `ThemeColor` into whichever type it needs.
//! A theme file may `extends = "<other theme>"` and set only what differs.
//!
//! Color syntax: `"#rrggbb"`, `"ansi:<name>"` (`ansi:dark-gray`, ...),
//! `"ansi:<0-255>"` or `"default"`.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

use serde::Deserialize;

const DARKFOX: &str = include_str!("themes/darkfox.toml");
const LIGHTFOX: &str = include_str!("themes/lightfox.toml");
const EVERFOREST: &str = include_str!("themes/everforest.toml");
const EVERFOREST_LIGHT: &str = include_str!("themes/everforest-light.toml");
const GRUVBOX_DARK: &str = include_str!("themes/gruvbox-dark.toml");
const GRUVBOX_LIGHT: &str = include_str!("themes/gruvbox-light.toml");
const MONOKAI: &str = include_str!("themes/monokai.toml");
const CATPPUCCIN_LATTE: &str = include_str!("themes/catppuccin-latte.toml");

/// Themes compiled into the binary, `darkfox` (the default) first.
const BUILTINS: [(&str, &str); 8] = [
    (DEFAULT_THEME, DARKFOX),
    ("lightfox", LIGHTFOX),
    ("everforest", EVERFOREST),
    ("everforest-light", EVERFOREST_LIGHT),
    ("gruvbox-dark", GRUVBOX_DARK),
    ("gruvbox-light", GRUVBOX_LIGHT),
    ("monokai", MONOKAI),
    ("catppuccin-latte", CATPPUCCIN_LATTE),
];

fn builtin(name: &str) -> Option<&'static str> {
    BUILTINS.iter().find(|(n, _)| *n == name).map(|(_, t)| *t)
}
pub const DEFAULT_THEME: &str = "darkfox";
const MAX_EXTENDS_DEPTH: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnsiColor {
    Black,
    Red,
    Green,
    Yellow,
    Blue,
    Magenta,
    Cyan,
    Gray,
    DarkGray,
    LightRed,
    LightGreen,
    LightYellow,
    LightBlue,
    LightMagenta,
    LightCyan,
    White,
}

const ANSI_NAMES: [(&str, AnsiColor); 16] = [
    ("black", AnsiColor::Black),
    ("red", AnsiColor::Red),
    ("green", AnsiColor::Green),
    ("yellow", AnsiColor::Yellow),
    ("blue", AnsiColor::Blue),
    ("magenta", AnsiColor::Magenta),
    ("cyan", AnsiColor::Cyan),
    ("gray", AnsiColor::Gray),
    ("dark-gray", AnsiColor::DarkGray),
    ("light-red", AnsiColor::LightRed),
    ("light-green", AnsiColor::LightGreen),
    ("light-yellow", AnsiColor::LightYellow),
    ("light-blue", AnsiColor::LightBlue),
    ("light-magenta", AnsiColor::LightMagenta),
    ("light-cyan", AnsiColor::LightCyan),
    ("white", AnsiColor::White),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThemeColor {
    Rgb(u8, u8, u8),
    Ansi(AnsiColor),
    Indexed(u8),
    /// The terminal's own default foreground/background.
    Default,
}

impl ThemeColor {
    pub fn parse(s: &str) -> Result<ThemeColor, String> {
        let s = s.trim();
        if s == "default" {
            return Ok(ThemeColor::Default);
        }
        if let Some(hex) = s.strip_prefix('#') {
            if hex.len() == 6 && hex.bytes().all(|b| b.is_ascii_hexdigit()) {
                let n = u32::from_str_radix(hex, 16).map_err(|e| e.to_string())?;
                return Ok(ThemeColor::Rgb((n >> 16) as u8, (n >> 8) as u8, n as u8));
            }
            return Err(format!("bad hex color {s:?} (want #rrggbb)"));
        }
        if let Some(rest) = s.strip_prefix("ansi:") {
            if let Ok(n) = rest.parse::<u8>() {
                return Ok(ThemeColor::Indexed(n));
            }
            return ANSI_NAMES
                .iter()
                .find(|(name, _)| *name == rest)
                .map(|(_, c)| ThemeColor::Ansi(*c))
                .ok_or_else(|| format!("unknown ANSI color {rest:?}"));
        }
        Err(format!(
            "bad color {s:?} (want \"#rrggbb\", \"ansi:<name|0-255>\" or \"default\")"
        ))
    }

    /// A concrete RGB for surfaces that cannot take a terminal-decided
    /// color (the syntect editor theme, SVG): exact for `Rgb`, the common
    /// xterm palette for ANSI/indexed colors, `None` for `Default`.
    pub fn approx_rgb(&self) -> Option<(u8, u8, u8)> {
        const ANSI16: [(u8, u8, u8); 16] = [
            (0x00, 0x00, 0x00),
            (0xcd, 0x00, 0x00),
            (0x00, 0xcd, 0x00),
            (0xcd, 0xcd, 0x00),
            (0x00, 0x00, 0xee),
            (0xcd, 0x00, 0xcd),
            (0x00, 0xcd, 0xcd),
            (0xe5, 0xe5, 0xe5),
            (0x7f, 0x7f, 0x7f),
            (0xff, 0x00, 0x00),
            (0x00, 0xff, 0x00),
            (0xff, 0xff, 0x00),
            (0x5c, 0x5c, 0xff),
            (0xff, 0x00, 0xff),
            (0x00, 0xff, 0xff),
            (0xff, 0xff, 0xff),
        ];
        let indexed = |n: u8| -> (u8, u8, u8) {
            match n {
                0..=15 => ANSI16[n as usize],
                16..=231 => {
                    let n = n - 16;
                    let level = |v: u8| if v == 0 { 0 } else { 55 + 40 * v };
                    (level(n / 36), level(n / 6 % 6), level(n % 6))
                }
                _ => {
                    let v = 8 + 10 * (n - 232);
                    (v, v, v)
                }
            }
        };
        match *self {
            ThemeColor::Rgb(r, g, b) => Some((r, g, b)),
            ThemeColor::Indexed(n) => Some(indexed(n)),
            ThemeColor::Ansi(a) => Some(match a {
                AnsiColor::Black => ANSI16[0],
                AnsiColor::Red => ANSI16[1],
                AnsiColor::Green => ANSI16[2],
                AnsiColor::Yellow => ANSI16[3],
                AnsiColor::Blue => ANSI16[4],
                AnsiColor::Magenta => ANSI16[5],
                AnsiColor::Cyan => ANSI16[6],
                AnsiColor::Gray => ANSI16[7],
                AnsiColor::DarkGray => ANSI16[8],
                AnsiColor::LightRed => ANSI16[9],
                AnsiColor::LightGreen => ANSI16[10],
                AnsiColor::LightYellow => ANSI16[11],
                AnsiColor::LightBlue => ANSI16[12],
                AnsiColor::LightMagenta => ANSI16[13],
                AnsiColor::LightCyan => ANSI16[14],
                AnsiColor::White => ANSI16[15],
            }),
            ThemeColor::Default => None,
        }
    }

    /// `#rrggbb`, for surfaces that can only take a literal color (SVG).
    /// `None` for ANSI/default colors, whose shade the terminal decides.
    pub fn hex(&self) -> Option<String> {
        match self {
            ThemeColor::Rgb(r, g, b) => Some(format!("#{r:02x}{g:02x}{b:02x}")),
            _ => None,
        }
    }
}

/// Declares a struct of `ThemeColor` fields plus `set` (by TOML key) and
/// `keys`, so adding a role is one line here and one in `darkfox.toml`.
macro_rules! palette {
    ($(#[$m:meta])* $name:ident { $($field:ident),+ $(,)? }) => {
        $(#[$m])*
        #[derive(Debug, Clone, PartialEq, Eq)]
        pub struct $name { $(pub $field: ThemeColor,)+ }

        impl $name {
            fn blank() -> Self { Self { $($field: ThemeColor::Default,)+ } }

            fn set(&mut self, key: &str, c: ThemeColor) -> bool {
                // `stringify!` keeps the `r#` of a raw identifier (`r#type`).
                $(if key == stringify!($field).trim_start_matches("r#") {
                    self.$field = c;
                    return true;
                })+
                false
            }

            pub fn keys() -> Vec<&'static str> {
                vec![$(stringify!($field).trim_start_matches("r#")),+]
            }
        }
    };
}

palette! {
    /// Chrome, markdown and table colors.
    UiColors {
        accent, dep, fail, ok, border, fg, bg, svg_border,
        muted, value, tag, warn, error, on_accent, link,
        search_match_bg, search_other_bg, search_fg,
        code_bg, code_fg, table_stripe_bg,
        map_node_bg, map_selected_bg, map_node_fg,
        callout_note, callout_tip, callout_important, callout_warning, callout_caution,
        heading_1, heading_2, heading_3, heading_4, heading_5, heading_6,
    }
}

palette! {
    /// Source-editor syntax colors.
    EditorColors {
        fg, bg, selection,
        comment, keyword, string, number, constant, function, r#type, operator, variable,
        meshfox_keyword, meshfox_attr, meshfox_value,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Theme {
    pub name: String,
    pub ui: UiColors,
    pub editor: EditorColors,
}

impl Theme {
    /// The built-in `darkfox`. Panics only if the bundled file is broken,
    /// which the tests below rule out.
    pub fn darkfox() -> Theme {
        parse_with_base(DARKFOX, None).expect("built-in darkfox theme is valid")
    }

    /// Names of every theme `load` can find: built-in first, then those
    /// in the local and global theme directories (sorted, no duplicates).
    pub fn available(canvas_root: &Path) -> Vec<String> {
        let mut names: Vec<String> = BUILTINS.iter().map(|(n, _)| n.to_string()).collect();
        for dir in search_dirs(canvas_root) {
            let Ok(entries) = std::fs::read_dir(dir) else {
                continue;
            };
            let mut found: Vec<String> = entries
                .flatten()
                .filter_map(|e| {
                    let path = e.path();
                    (path.extension()? == "toml")
                        .then(|| path.file_stem()?.to_str().map(str::to_string))?
                })
                .filter(|n| valid_name(n) && !names.contains(n))
                .collect();
            found.sort();
            names.extend(found);
        }
        names
    }

    /// Looks `name` up local → global → built-in. `canvas_root` is the
    /// directory holding the canvas (its `.meshfox/` is the local one).
    pub fn load(canvas_root: &Path, name: &str) -> Result<Theme, ThemeError> {
        load_depth(&search_dirs(canvas_root), name, 0)
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum ThemeError {
    BadName(String),
    NotFound(String),
    Parse { theme: String, message: String },
    ExtendsTooDeep(String),
}

impl fmt::Display for ThemeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ThemeError::BadName(n) => {
                write!(
                    f,
                    "bad theme name {n:?} (letters, digits, '-' and '_' only)"
                )
            }
            ThemeError::NotFound(n) => write!(f, "theme {n:?} not found"),
            ThemeError::Parse { theme, message } => write!(f, "theme {theme:?}: {message}"),
            ThemeError::ExtendsTooDeep(n) => {
                write!(f, "theme {n:?}: `extends` chain too deep (cycle?)")
            }
        }
    }
}

impl std::error::Error for ThemeError {}

/// `<canvas_root>/.meshfox/themes/tui`.
pub fn local_theme_dir(canvas_root: &Path) -> PathBuf {
    canvas_root.join(".meshfox").join("themes").join("tui")
}

/// `~/.meshfox/themes/tui`; `None` without `HOME`.
pub fn global_theme_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|h| Path::new(&h).join(".meshfox").join("themes").join("tui"))
}

fn search_dirs(canvas_root: &Path) -> Vec<PathBuf> {
    let mut dirs = vec![local_theme_dir(canvas_root)];
    dirs.extend(global_theme_dir());
    dirs
}

/// Names become file names, so only a plain slug is accepted — no path
/// separators, dots or empty names can reach the filesystem.
fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

#[derive(Deserialize)]
struct RawTheme {
    name: Option<String>,
    extends: Option<String>,
    #[serde(default)]
    ui: BTreeMap<String, String>,
    #[serde(default)]
    editor: BTreeMap<String, String>,
}

fn load_depth(dirs: &[PathBuf], name: &str, depth: usize) -> Result<Theme, ThemeError> {
    if depth > MAX_EXTENDS_DEPTH {
        return Err(ThemeError::ExtendsTooDeep(name.to_string()));
    }
    if !valid_name(name) {
        return Err(ThemeError::BadName(name.to_string()));
    }
    let file = format!("{name}.toml");
    let text = dirs
        .iter()
        .find_map(|d| std::fs::read_to_string(d.join(&file)).ok())
        .or_else(|| builtin(name).map(str::to_string))
        .ok_or_else(|| ThemeError::NotFound(name.to_string()))?;

    let raw: RawTheme = toml::from_str(&text).map_err(|e| ThemeError::Parse {
        theme: name.to_string(),
        message: e.to_string(),
    })?;
    // A user file named `darkfox` shadows the built-in one, so it must not
    // extend itself; it extends nothing by default and starts from blanks.
    let base = match raw.extends.as_deref() {
        Some(parent) if parent == name && name == DEFAULT_THEME => Some(Theme::darkfox()),
        Some(parent) => Some(load_depth(dirs, parent, depth + 1)?),
        None => None,
    };
    apply(raw, name, base)
}

fn parse_with_base(text: &str, base: Option<Theme>) -> Result<Theme, ThemeError> {
    let raw: RawTheme = toml::from_str(text).map_err(|e| ThemeError::Parse {
        theme: DEFAULT_THEME.to_string(),
        message: e.to_string(),
    })?;
    let name = raw
        .name
        .clone()
        .unwrap_or_else(|| DEFAULT_THEME.to_string());
    apply(raw, &name, base)
}

fn apply(raw: RawTheme, name: &str, base: Option<Theme>) -> Result<Theme, ThemeError> {
    let err = |message: String| ThemeError::Parse {
        theme: name.to_string(),
        message,
    };
    let has_base = base.is_some();
    let mut theme = base.unwrap_or_else(|| Theme {
        name: name.to_string(),
        ui: UiColors::blank(),
        editor: EditorColors::blank(),
    });
    theme.name = raw.name.unwrap_or_else(|| name.to_string());

    for (key, value) in &raw.ui {
        let c = ThemeColor::parse(value).map_err(|e| err(format!("[ui] {key}: {e}")))?;
        if !theme.ui.set(key, c) {
            return Err(err(format!("[ui] unknown key {key:?}")));
        }
    }
    for (key, value) in &raw.editor {
        let c = ThemeColor::parse(value).map_err(|e| err(format!("[editor] {key}: {e}")))?;
        if !theme.editor.set(key, c) {
            return Err(err(format!("[editor] unknown key {key:?}")));
        }
    }
    // A theme without a base must define every role, or the missing ones
    // would silently render as the terminal default.
    if !has_base {
        let missing: Vec<_> = UiColors::keys()
            .into_iter()
            .filter(|k| !raw.ui.contains_key(*k))
            .map(|k| format!("[ui] {k}"))
            .chain(
                EditorColors::keys()
                    .into_iter()
                    .filter(|k| !raw.editor.contains_key(*k))
                    .map(|k| format!("[editor] {k}")),
            )
            .collect();
        if !missing.is_empty() {
            return Err(err(format!(
                "missing {} (add them or set `extends = \"darkfox\"`)",
                missing.join(", ")
            )));
        }
    }
    Ok(theme)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> TempDir {
            let dir = std::env::temp_dir()
                .join(format!("meshfox-tui-theme-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            TempDir(dir)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn parses_every_color_form() {
        assert_eq!(
            ThemeColor::parse("#ff6e15"),
            Ok(ThemeColor::Rgb(0xff, 0x6e, 0x15))
        );
        assert_eq!(
            ThemeColor::parse("ansi:dark-gray"),
            Ok(ThemeColor::Ansi(AnsiColor::DarkGray))
        );
        assert_eq!(ThemeColor::parse("ansi:8"), Ok(ThemeColor::Indexed(8)));
        assert_eq!(ThemeColor::parse("default"), Ok(ThemeColor::Default));
        assert!(ThemeColor::parse("#fff").is_err());
        assert!(ThemeColor::parse("ansi:nope").is_err());
        assert!(ThemeColor::parse("red").is_err());
    }

    #[test]
    fn darkfox_is_complete_and_keeps_ansi_muted() {
        let t = Theme::darkfox();
        assert_eq!(t.ui.accent, ThemeColor::Rgb(0xff, 0x6e, 0x15));
        assert_eq!(t.ui.muted, ThemeColor::Ansi(AnsiColor::DarkGray));
        assert_eq!(t.editor.bg, ThemeColor::Default);
        assert_eq!(t.ui.accent.hex().as_deref(), Some("#ff6e15"));
        assert_eq!(t.ui.muted.hex(), None);
    }

    #[test]
    fn every_builtin_theme_loads_and_everforest_is_distinct() {
        let root = std::env::temp_dir().join("meshfox-tui-theme-builtins-never-created");
        for name in Theme::available(&root) {
            Theme::load(&root, &name).unwrap_or_else(|e| panic!("{name}: {e}"));
        }
        let t = Theme::load(&root, "everforest").unwrap();
        assert_eq!(t.name, "everforest");
        assert_eq!(t.ui.accent, ThemeColor::Rgb(0xa7, 0xc0, 0x80));
        assert_eq!(t.ui.muted, ThemeColor::Rgb(0x85, 0x92, 0x89));
        assert_ne!(t, Theme::darkfox());
    }

    #[test]
    fn extends_overrides_only_what_it_sets() {
        let dir = TempDir::new("t");
        let tdir = local_theme_dir(dir.path());
        std::fs::create_dir_all(&tdir).unwrap();
        std::fs::write(
            tdir.join("mine.toml"),
            "extends = \"darkfox\"\n[ui]\nmuted = \"#888888\"\n",
        )
        .unwrap();
        let t = Theme::load(dir.path(), "mine").unwrap();
        assert_eq!(t.name, "mine");
        assert_eq!(t.ui.muted, ThemeColor::Rgb(0x88, 0x88, 0x88));
        assert_eq!(t.ui.accent, Theme::darkfox().ui.accent);
    }

    #[test]
    fn rejects_unknown_keys_missing_roles_cycles_and_bad_names() {
        let dir = TempDir::new("u");
        let tdir = local_theme_dir(dir.path());
        std::fs::create_dir_all(&tdir).unwrap();
        std::fs::write(
            tdir.join("typo.toml"),
            "extends=\"darkfox\"\n[ui]\nacent=\"#000000\"\n",
        )
        .unwrap();
        std::fs::write(tdir.join("bare.toml"), "[ui]\naccent=\"#000000\"\n").unwrap();
        std::fs::write(tdir.join("a.toml"), "extends=\"b\"\n").unwrap();
        std::fs::write(tdir.join("b.toml"), "extends=\"a\"\n").unwrap();
        let root = dir.path();
        assert!(matches!(
            Theme::load(root, "typo"),
            Err(ThemeError::Parse { .. })
        ));
        assert!(matches!(
            Theme::load(root, "bare"),
            Err(ThemeError::Parse { .. })
        ));
        assert!(matches!(
            Theme::load(root, "a"),
            Err(ThemeError::ExtendsTooDeep(_))
        ));
        assert!(matches!(
            Theme::load(root, "../x"),
            Err(ThemeError::BadName(_))
        ));
        assert!(matches!(
            Theme::load(root, "nope"),
            Err(ThemeError::NotFound(_))
        ));
    }
}
