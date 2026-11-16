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

/// A `max-width=` value: an upper bound on an image's displayed width — a
/// bare integer, integer+`%`, or `none` (no bound at all: a deliberately wide
/// image may extend past its container and be scrolled).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MaxSize {
    None,
    Size(Size),
}

impl fmt::Display for MaxSize {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MaxSize::None => write!(f, "none"),
            MaxSize::Size(size) => size.fmt(f),
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
    pub max_width: Option<MaxSize>,
    pub bg: Option<Background>,
}

impl ImageAttrs {
    pub fn is_empty(&self) -> bool {
        self.width.is_none()
            && self.height.is_none()
            && self.max_width.is_none()
            && self.bg.is_none()
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
        if let Some(m) = self.max_width {
            parts.push(format!("max-width={m}"));
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
            "max-width" if attrs.max_width.is_none() => {
                attrs.max_width = Some(if val == "none" {
                    MaxSize::None
                } else {
                    MaxSize::Size(parse_size(val)?)
                })
            }
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
    let mut style = String::new();
    if let Some(m) = attrs.max_width {
        // CSS needs a unit: a bare number is pixels, like `width="300"`.
        match m {
            MaxSize::Size(Size {
                value,
                percent: false,
            }) => style.push_str(&format!("max-width:{value}px;")),
            m => style.push_str(&format!("max-width:{m};")),
        }
    }
    if let Some(bg) = attrs.bg {
        style.push_str(&format!("background:{bg};"));
    }
    if !style.is_empty() {
        out.push_str(&format!(" style=\"{}\"", style.trim_end_matches(';')));
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
            max_width: None,
            bg: None,
        };
        assert_eq!(html_attrs(&attrs), r#" width="300" height="50%""#);
    }

    #[test]
    fn parses_max_width_in_every_form() {
        let size = |value, percent| MaxSize::Size(Size { value, percent });
        for (text, want) in [
            ("{max-width=800}", size(800, false)),
            ("{max-width=60%}", size(60, true)),
            ("{max-width=none}", MaxSize::None),
        ] {
            let (attrs, len) = parse(text).unwrap();
            assert_eq!(attrs.max_width, Some(want), "{text}");
            assert_eq!(len, text.len());
        }
        assert_eq!(parse("{max-width=wide}"), None);
        assert_eq!(parse("{max-width=1 max-width=2}"), None);
        // The underscore spelling is not an alias.
        assert_eq!(parse("{max_width=1}"), None);
    }

    #[test]
    fn max_width_round_trips_and_shares_the_style_attribute_with_bg() {
        let (attrs, _) = parse("{width=100% max-width=none bg=#fff}").unwrap();
        assert_eq!(attrs.to_braces(), "{width=100% max-width=none bg=#ffffff}");
        assert_eq!(parse(&attrs.to_braces()).unwrap().0, attrs);
        assert_eq!(
            html_attrs(&attrs),
            r#" width="100%" style="max-width:none;background:#ffffff""#
        );
        let (only, _) = parse("{max-width=640}").unwrap();
        assert_eq!(html_attrs(&only), r#" style="max-width:640px""#);
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

/// Local image destinations in rendered Markdown (including reference images).
/// Code fences, remote URLs, fragments and inline data have no local file.
pub fn local_image_destinations(markdown: &str) -> Vec<String> {
    use pulldown_cmark::{Event, Parser, Tag};
    Parser::new(markdown)
        .filter_map(|event| {
            let Event::Start(Tag::Image { dest_url, .. }) = event else {
                return None;
            };
            let url = dest_url.as_ref();
            if url.is_empty() || url.starts_with('#') || url.starts_with("//") || url.contains(':')
            {
                return None;
            }
            Some(decode_image_path(
                url.split(['?', '#']).next().unwrap_or(url),
            ))
        })
        .collect()
}

#[cfg(test)]
mod image_dependency_tests {
    use super::*;
    #[test]
    fn image_paths_decode_url_escapes_and_preserve_literal_plus() {
        assert_eq!(
            local_image_destinations("![x](plot%20one.svg?x=1#view)"),
            vec!["plot one.svg"]
        );
        assert_eq!(decode_image_path("%D0%B0+%25.svg"), "а+%.svg");
    }
    #[test]
    fn markdown_images_include_references_and_cached_output_but_not_code() {
        let md = "![a](a.png)\n\n![b][ref]\n\n[ref]: b.svg\n\n```md\n![ignored](code.png)\n```\n\n<!-- meshfox:output name=\"x\" -->\n\n![out](out.png)\n\n<!-- /meshfox:output -->\n\n![inline](data:image/png;base64,AA) ![remote](https://example.org/x)";
        assert_eq!(
            local_image_destinations(md),
            vec!["a.png", "b.svg", "out.png"]
        );
    }
}

/// Decode URL path escapes once, without treating a literal `+` as a space.
pub fn decode_image_path(path: &str) -> String {
    let mut decoded = Vec::new();
    let bytes = path.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(a), Some(b)) = (
                (bytes[i + 1] as char).to_digit(16),
                (bytes[i + 2] as char).to_digit(16),
            ) {
                decoded.push((a * 16 + b) as u8);
                i += 3;
                continue;
            }
        }
        decoded.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(decoded).unwrap_or_else(|_| path.to_string())
}
