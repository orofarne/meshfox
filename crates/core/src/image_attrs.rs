//! Narrow Pandoc/GitLab-style image-size syntax: `{width=300}`,
//! `{height=50%}`, or both, written with no space directly after an
//! image's closing `)` — see SPEC.md's "Formal grammar" and TODO.canvas.md's
//! `image-attrs` task for why this is deliberately narrow (only
//! `width=`/`height=`, a bare integer or integer+`%`, not the full Pandoc
//! `{.class #id ...}` grammar). Shared by every consumer that needs to
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

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ImageAttrs {
    pub width: Option<Size>,
    pub height: Option<Size>,
}

impl ImageAttrs {
    pub fn is_empty(&self) -> bool {
        self.width.is_none() && self.height.is_none()
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
    let inner = &rest[..close];
    let mut attrs = ImageAttrs::default();
    for tok in inner.split_whitespace() {
        let (key, val) = tok.split_once('=')?;
        let size = parse_size(val)?;
        match key {
            "width" if attrs.width.is_none() => attrs.width = Some(size),
            "height" if attrs.height.is_none() => attrs.height = Some(size),
            _ => return None,
        }
    }
    if attrs.is_empty() {
        return None;
    }
    Some((attrs, close + 2)) // '{' + inner + '}'
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

/// ` width="300" height="50%"`-style HTML attribute fragment — leading
/// space included when non-empty, nothing at all when both are unset.
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
        };
        assert_eq!(html_attrs(&attrs), r#" width="300" height="50%""#);
    }

    #[test]
    fn html_attrs_empty_for_no_attrs() {
        assert_eq!(html_attrs(&ImageAttrs::default()), "");
    }
}
