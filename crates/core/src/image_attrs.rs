//! Narrow Pandoc/GitLab-style image attribute syntax: `{width=300}`,
//! `{height=50%}`, `{bg=#fff}`, or any of them together, written with no
//! space directly after an image's closing `)` — see SPEC.md's "Formal
//! grammar" and TODO.canvas.md's `image-attrs` task for why this is
//! deliberately narrow (only `width=`/`height=` — a bare integer or
//! integer+`%` — and `bg=` — a hex color or `transparent` — not the full
//! Pandoc `{.class #id ...}` grammar, and never free-form CSS). Shared by every consumer that needs to
//! recognize this syntax after an image (`staticgen.rs`, the TUI's
//! `markdown.rs`) so it's parsed identically everywhere rather than as a
//! third copy of the same small grammar.

use std::fmt;

/// A parsed `width=`/`height=` value: a bare integer, or an integer
/// followed by `%`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Size {
    pub value: u32,
    pub percent: bool,
}

impl fmt::Display for Size {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.percent {
            write!(f, "{}%", self.value)
        } else {
            write!(f, "{}", self.value)
        }
    }
}

/// A `bg=` value: a fixed backing color painted behind an image (so a
/// transparent SVG drawn for a light page stays legible on a dark theme),
/// or an explicit `transparent` (no backing at all). Only hex colors —
/// a named-color table would have to be duplicated in every consumer
/// (`web/src/remarkImageAttrs.ts`, the TUI's rasterizer), and a bounded
/// grammar is what keeps this safe to splice into a `style` attribute.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Background {
    Transparent,
    Rgb(u8, u8, u8),
}

impl Background {
    /// Parses `#rgb` or `#rrggbb` (any case) or `transparent`.
    pub fn parse(s: &str) -> Option<Background> {
        if s.eq_ignore_ascii_case("transparent") {
            return Some(Background::Transparent);
        }
        let hex = s.strip_prefix('#')?;
        if !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        let pair = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
        match hex.len() {
            3 => {
                let nib = |i: usize| u8::from_str_radix(&hex[i..i + 1], 16).ok().map(|n| n * 17);
                Some(Background::Rgb(nib(0)?, nib(1)?, nib(2)?))
            }
            6 => Some(Background::Rgb(pair(0)?, pair(2)?, pair(4)?)),
            _ => None,
        }
    }
}

impl fmt::Display for Background {
    /// The canonical CSS spelling — always `#rrggbb` (lowercase) or
    /// `transparent`, so it can go straight into a `style` attribute.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Background::Transparent => write!(f, "transparent"),
            Background::Rgb(r, g, b) => write!(f, "#{r:02x}{g:02x}{b:02x}"),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ImageAttrs {
    pub width: Option<Size>,
    pub height: Option<Size>,
    pub bg: Option<Background>,
}

impl ImageAttrs {
    pub fn is_empty(&self) -> bool {
        self.width.is_none() && self.height.is_none() && self.bg.is_none()
    }

    /// The canonical `{width=.. height=.. bg=..}` spelling (`parse`
    /// round-trips it), or an empty string when nothing is set.
    pub fn to_braces(&self) -> String {
        let mut parts = Vec::new();
        if let Some(w) = self.width {
            parts.push(format!("width={w}"));
        }
        if let Some(h) = self.height {
            parts.push(format!("height={h}"));
        }
        if let Some(bg) = self.bg {
            parts.push(format!("bg={bg}"));
        }
        if parts.is_empty() {
            String::new()
        } else {
            format!("{{{}}}", parts.join(" "))
        }
    }
}

/// If `text` starts with a `{...}` matching this narrow grammar, returns
/// the parsed attributes and the byte length of the leading `{...}` span
/// (so a caller can strip exactly that much and leave the rest of `text`
/// alone). `None` for anything else — no `{` at the very start, unclosed
/// braces, an unknown key, a malformed value, or an empty `{}` — in which
/// case the text is left completely alone and rendered as ordinary
/// literal text, same as before this syntax existed.
pub fn parse(text: &str) -> Option<(ImageAttrs, usize)> {
    let rest = text.strip_prefix('{')?;
    let close = rest.find('}')?;
    let attrs = parse_inner(&rest[..close])?;
    Some((attrs, close + 2)) // '{' + inner + '}'
}

/// The same grammar as `parse`, for just what sits *between* the braces —
/// what a fence's `output-attrs="width=50% bg=#fff"` carries. `None` for
/// the same reasons `parse` is, including empty/whitespace-only input.
pub fn parse_inner(inner: &str) -> Option<ImageAttrs> {
    let mut attrs = ImageAttrs::default();
    for tok in inner.split_whitespace() {
        let (key, val) = tok.split_once('=')?;
        match key {
            "width" if attrs.width.is_none() => attrs.width = Some(parse_size(val)?),
            "height" if attrs.height.is_none() => attrs.height = Some(parse_size(val)?),
            "bg" if attrs.bg.is_none() => attrs.bg = Some(Background::parse(val)?),
            _ => return None,
        }
    }
    if attrs.is_empty() {
        return None;
    }
    Some(attrs)
}

fn parse_size(s: &str) -> Option<Size> {
    let (num, percent) = match s.strip_suffix('%') {
        Some(n) => (n, true),
        None => (s, false),
    };
    if num.is_empty() || !num.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let value: u32 = num.parse().ok()?;
    Some(Size { value, percent })
}

/// ` width="300" height="50%" style="background:#ffffff"`-style HTML
/// attribute fragment — leading space included when non-empty, nothing at
/// all when everything is unset.
/// Only `staticgen.rs` needs this (the TUI has no HTML to write and
/// applies these as a terminal-image sizing hint instead).
pub fn html_attrs(attrs: &ImageAttrs) -> String {
    let mut out = String::new();
    if let Some(w) = attrs.width {
        out.push_str(&format!(" width=\"{w}\""));
    }
    if let Some(h) = attrs.height {
        out.push_str(&format!(" height=\"{h}\""));
    }
    if let Some(bg) = attrs.bg {
        out.push_str(&format!(" style=\"background:{bg}\""));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_width_only() {
        let (attrs, len) = parse("{width=300} rest").unwrap();
        assert_eq!(
            attrs.width,
            Some(Size {
                value: 300,
                percent: false
            })
        );
        assert_eq!(attrs.height, None);
        assert_eq!(len, "{width=300}".len());
    }

    #[test]
    fn parses_width_and_height_percent() {
        let (attrs, len) = parse("{width=50% height=200}").unwrap();
        assert_eq!(
            attrs.width,
            Some(Size {
                value: 50,
                percent: true
            })
        );
        assert_eq!(
            attrs.height,
            Some(Size {
                value: 200,
                percent: false
            })
        );
        assert_eq!(len, "{width=50% height=200}".len());
    }

    #[test]
    fn rejects_unknown_key() {
        assert_eq!(parse(r#"{class="x"}"#), None);
    }

    #[test]
    fn rejects_non_numeric_value() {
        assert_eq!(parse("{width=big}"), None);
    }

    #[test]
    fn rejects_duplicate_key() {
        assert_eq!(parse("{width=1 width=2}"), None);
    }

    #[test]
    fn rejects_empty_braces() {
        assert_eq!(parse("{}"), None);
    }

    #[test]
    fn none_without_leading_brace() {
        assert_eq!(parse("plain text"), None);
        assert_eq!(parse(""), None);
    }

    #[test]
    fn none_without_closing_brace() {
        assert_eq!(parse("{width=300"), None);
    }

    #[test]
    fn html_attrs_formats_both() {
        let attrs = ImageAttrs {
            width: Some(Size {
                value: 300,
                percent: false,
            }),
            height: Some(Size {
                value: 50,
                percent: true,
            }),
            bg: None,
        };
        assert_eq!(html_attrs(&attrs), r#" width="300" height="50%""#);
    }

    #[test]
    fn html_attrs_empty_for_no_attrs() {
        assert_eq!(html_attrs(&ImageAttrs::default()), "");
    }

    #[test]
    fn parses_bg_hex_and_transparent() {
        let (attrs, len) = parse("{bg=#fff}").unwrap();
        assert_eq!(attrs.bg, Some(Background::Rgb(255, 255, 255)));
        assert_eq!(len, "{bg=#fff}".len());
        let (attrs, _) = parse("{bg=#1A2b3C}").unwrap();
        assert_eq!(attrs.bg, Some(Background::Rgb(0x1a, 0x2b, 0x3c)));
        let (attrs, _) = parse("{bg=transparent}").unwrap();
        assert_eq!(attrs.bg, Some(Background::Transparent));
    }

    #[test]
    fn parses_bg_alongside_size() {
        let (attrs, _) = parse("{width=50% bg=#fff}").unwrap();
        assert_eq!(attrs.width.map(|s| s.value), Some(50));
        assert_eq!(attrs.bg, Some(Background::Rgb(255, 255, 255)));
    }

    #[test]
    fn rejects_bad_bg_values() {
        assert_eq!(parse("{bg=white}"), None);
        assert_eq!(parse("{bg=#ff}"), None);
        assert_eq!(parse("{bg=#ggg}"), None);
        assert_eq!(parse("{bg=#ffff}"), None);
        assert_eq!(parse("{bg=#ffffffff}"), None);
        assert_eq!(parse("{bg=red;x:y}"), None);
        assert_eq!(parse("{bg=}"), None);
        assert_eq!(parse("{bg=#fff bg=#000}"), None);
    }

    #[test]
    fn html_attrs_emits_a_canonical_background_style() {
        let (attrs, _) = parse("{bg=#FFF}").unwrap();
        assert_eq!(html_attrs(&attrs), r#" style="background:#ffffff""#);
        let (attrs, _) = parse("{bg=transparent}").unwrap();
        assert_eq!(html_attrs(&attrs), r#" style="background:transparent""#);
    }

    #[test]
    fn parse_inner_and_to_braces_round_trip() {
        let attrs = parse_inner("bg=#FFF  width=50%").unwrap();
        assert_eq!(attrs.to_braces(), "{width=50% bg=#ffffff}");
        assert_eq!(parse(&attrs.to_braces()).unwrap().0, attrs);
        assert_eq!(parse_inner(""), None);
        assert_eq!(parse_inner("  "), None);
        assert_eq!(parse_inner("width=1}"), None);
        assert_eq!(ImageAttrs::default().to_braces(), "");
    }
}
