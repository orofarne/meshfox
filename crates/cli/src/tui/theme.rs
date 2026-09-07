//! TUI color palette — mirrors the web UI's own CSS custom properties
//! (`web/src/index.css`, dark theme — a terminal is effectively always
//! "dark chrome" regardless of the browser's light/dark toggle) so the two
//! surfaces read as the same product rather than two independently themed
//! ones. Shared between `ui.rs` (panes/popups/badges) and `markdown.rs`
//! (a block's own deps line) — edit here, not at each call site, so the
//! whole TUI can be re-tuned in one place while iterating on it.

use ratatui::style::Color;

/// Mirrors `--accent` (`#ff6e15` in the dark theme) — the web UI's run
/// buttons/links/focus color. Used here for the *focused* pane's border
/// and every popup/modal's border (a popup is always "the thing that has
/// focus right now", the same role an accent-colored focus ring plays in
/// the browser).
pub const ACCENT: Color = Color::Rgb(0xff, 0x6e, 0x15);

/// Mirrors `--dep` (`#c68ae8` in the dark theme) — a block's own explicit/
/// implicit dependency line (`markdown.rs`'s `dep_line`), same identity as
/// `web/src/MeshNode.tsx`'s `after: …`/`via …` text.
pub const DEP: Color = Color::Rgb(0xc6, 0x8a, 0xe8);

/// Mirrors `--fail` (`#e56a6a` in the dark theme).
pub const FAIL: Color = Color::Rgb(0xe5, 0x6a, 0x6a);

/// Mirrors `--ok` (`#6fcf97` in the dark theme).
pub const OK: Color = Color::Rgb(0x6f, 0xcf, 0x97);

/// A visible neutral border for chrome that isn't currently focused/a
/// popup (the *unfocused* pane border, the always-on Output pane) — the
/// web UI's `--node-border` role: a real, deliberate border color, not
/// "whatever the terminal's own default foreground happens to render as".
/// Warmer and lighter than `--node-border`'s own dark-theme value
/// (`#7a4620`, which reads as muddy/low-contrast on a typically pure-black
/// terminal background) — picked for terminal legibility first, brand hue
/// second.
pub const BORDER: Color = Color::Rgb(0x9a, 0x93, 0x8c);
