//! Small, dependency-free helpers for SVG that arrives as a Markdown image
//! (`![](data:image/svg+xml;base64,…)`, or `output="image"`'s own generated
//! one) — shared by every Rust consumer (`meshfox tui`'s rasterizer,
//! `output::render_output_block_image`'s content sniffing).
//!
//! # Theme contract
//!
//! An SVG shown through `<img>`/a rasterizer can't see the page's CSS, so a
//! diagram drawn black-on-transparent vanishes on a dark theme. The
//! platform therefore rewrites the SVG's *text* right before showing it
//! (never stored — the file keeps whatever the tool produced), giving a
//! tool two ways to follow the theme without any cooperation beyond using
//! them:
//!
//! - `currentColor` resolves to the theme's foreground (`inject_theme`
//!   sets `color` on the root element);
//! - `var(--mf-fg)`, `var(--mf-bg)`, `var(--mf-accent)`, `var(--mf-border)`
//!   — with an optional fallback, `var(--mf-fg, #000)`, which is what an
//!   unthemed viewer (GitHub, a plain browser) sees.
//!
//! A browser resolves `var()` itself from the `:root` custom properties
//! `inject_theme` also writes. `resvg` (the TUI's rasterizer) has no CSS
//! variable support at all — not even the fallback — so `resolve_vars`
//! substitutes them textually first.
//!
//! The web UI has its own twin of both (`web/src/svgTheme.ts`) reading the
//! page's live theme; keep the variable names and the injected rules in
//! step with it.

/// Concrete colors for one theme. CSS color strings, written verbatim into
/// the SVG, so they must already be valid (`#rrggbb` — nothing from a
/// document ever reaches these).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Theme {
    pub fg: String,
    pub bg: String,
    pub accent: String,
    pub border: String,
}

impl Theme {
    pub fn dark() -> Theme {
        Theme {
            fg: "#f2ede6".into(),
            bg: "#131316".into(),
            accent: "#ff6e15".into(),
            border: "#7a4620".into(),
        }
    }

    pub fn light() -> Theme {
        Theme {
            fg: "#201a14".into(),
            bg: "#ffffff".into(),
            accent: "#ea580c".into(),
            border: "#e5ddd2".into(),
        }
    }

    fn value(&self, name: &str) -> Option<&str> {
        match name {
            "fg" => Some(&self.fg),
            "bg" => Some(&self.bg),
            "accent" => Some(&self.accent),
            "border" => Some(&self.border),
            _ => None,
        }
    }
}

/// Whether `text` is an SVG document: an optional BOM/whitespace, an
/// optional XML prolog/comments/doctype, then a root `<svg`. Deliberately
/// a prefix sniff, not a parse — it only has to tell an SVG apart from
/// ordinary text on stdout.
pub fn looks_like_svg(text: &str) -> bool {
    let mut rest = text.trim_start_matches('\u{feff}').trim_start();
    loop {
        if rest.starts_with("<svg") {
            return rest[4..]
                .chars()
                .next()
                .is_some_and(|c| c.is_whitespace() || c == '>' || c == '/');
        }
        let skip_to = if rest.starts_with("<?") {
            "?>"
        } else if rest.starts_with("<!--") {
            "-->"
        } else if rest.starts_with("<!") {
            ">"
        } else {
            return false;
        };
        match rest.find(skip_to) {
            // A DOCTYPE may carry an internal `[...]` subset with `>`
            // inside it; stopping at the first `>` there just means we
            // miss the SVG, which is the safe direction for a sniff.
            Some(i) => rest = rest[i + skip_to.len()..].trim_start(),
            None => return false,
        }
    }
}

/// The CSS `inject_theme` writes — one place, so the TUI/static/test
/// expectations and the docs can't drift from each other.
pub fn theme_css(theme: &Theme) -> String {
    format!(
        "svg{{color:{fg}}}:root{{--mf-fg:{fg};--mf-bg:{bg};--mf-accent:{accent};--mf-border:{border}}}",
        fg = theme.fg,
        bg = theme.bg,
        accent = theme.accent,
        border = theme.border,
    )
}

/// Byte offset just past the `>` closing the root `<svg ...>` start tag
/// (quote-aware, so a `>` inside an attribute value doesn't count), or
/// `None` if there's no root tag, it's unterminated, or it's self-closing
/// (`<svg .../>` — nothing to put a style into).
fn root_tag_end(svg: &str) -> Option<usize> {
    let bytes = svg.as_bytes();
    let mut i = 0;
    // Find the root `<svg` the same way `looks_like_svg` does, but by
    // scanning for it rather than skipping a prolog structurally — the
    // prolog may legitimately mention "<svg" inside a comment, so only an
    // `<svg` that isn't inside one counts.
    let mut start = None;
    while i < bytes.len() {
        if svg[i..].starts_with("<!--") {
            i += svg[i..].find("-->").map(|e| e + 3)?;
            continue;
        }
        if svg[i..].starts_with("<svg")
            && svg[i + 4..]
                .chars()
                .next()
                .is_some_and(|c| c.is_whitespace() || c == '>' || c == '/')
        {
            start = Some(i);
            break;
        }
        i += 1;
    }
    let mut i = start? + 4;
    let mut quote: Option<u8> = None;
    while i < bytes.len() {
        let b = bytes[i];
        match quote {
            Some(q) if b == q => quote = None,
            Some(_) => {}
            None if b == b'"' || b == b'\'' => quote = Some(b),
            None if b == b'>' => {
                return if i > 0 && bytes[i - 1] == b'/' {
                    None
                } else {
                    Some(i + 1)
                };
            }
            None => {}
        }
        i += 1;
    }
    None
}

/// Inserts a `<style>` carrying `theme_css` right after the root `<svg>`
/// start tag, so it sits *before* the tool's own styles and a tool's own
/// rule for the same selector still wins. Returns the SVG unchanged if no
/// such tag is found (including a self-closing root).
pub fn inject_theme(svg: &str, theme: &Theme) -> String {
    match root_tag_end(svg) {
        Some(at) => {
            let mut out = String::with_capacity(svg.len() + 160);
            out.push_str(&svg[..at]);
            out.push_str("<style>");
            out.push_str(&theme_css(theme));
            out.push_str("</style>");
            out.push_str(&svg[at..]);
            out
        }
        None => svg.to_string(),
    }
}

/// Replaces every `var(--mf-NAME)` / `var(--mf-NAME, fallback)` with the
/// theme's value for NAME. An unknown NAME (`var(--mf-nope, #000)`) becomes
/// its fallback, or is left untouched with no fallback — a renderer that
/// understands `var()` then deals with it, and one that doesn't draws the
/// default, exactly what it would have done anyway. Other `var(...)`
/// (anything not starting `--mf-`) is never touched.
pub fn resolve_vars(svg: &str, theme: &Theme) -> String {
    const OPEN: &str = "var(--mf-";
    let mut out = String::with_capacity(svg.len());
    let mut rest = svg;
    while let Some(at) = rest.find(OPEN) {
        out.push_str(&rest[..at]);
        let after = &rest[at + OPEN.len()..];
        let Some(close) = matching_paren(after) else {
            // Unbalanced — emit the rest as-is rather than guess.
            out.push_str(&rest[at..]);
            return out;
        };
        let inner = &after[..close];
        let (name, fallback) = match inner.split_once(',') {
            Some((n, f)) => (n.trim(), Some(f.trim())),
            None => (inner.trim(), None),
        };
        match (theme.value(name), fallback) {
            (Some(v), _) => out.push_str(v),
            (None, Some(f)) => out.push_str(f),
            (None, None) => out.push_str(&rest[at..at + OPEN.len() + close + 1]),
        }
        rest = &after[close + 1..];
    }
    out.push_str(rest);
    out
}

/// Byte index of the `)` closing an already-open `var(`, tracking nested
/// parentheses (`var(--mf-fg, rgb(0, 0, 0))`). `None` if never closed.
fn matching_paren(s: &str) -> Option<usize> {
    let mut depth = 1usize;
    for (i, b) in s.bytes().enumerate() {
        match b {
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const SVG: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10"><rect fill="currentColor"/></svg>"#;

    #[test]
    fn sniffs_plain_svg_and_prologs() {
        assert!(looks_like_svg(SVG));
        assert!(looks_like_svg("\n  <svg>"));
        assert!(looks_like_svg(
            "\u{feff}<?xml version=\"1.0\"?>\n<svg xmlns=\"x\">"
        ));
        assert!(looks_like_svg(
            "<?xml version=\"1.0\"?><!-- made by x --><!DOCTYPE svg PUBLIC \"-//W3C//DTD SVG 1.1//EN\" \"http://www.w3.org/Graphics/SVG/1.1/DTD/svg11.dtd\"><svg>"
        ));
    }

    #[test]
    fn does_not_sniff_other_text_as_svg() {
        assert!(!looks_like_svg(""));
        assert!(!looks_like_svg("hello"));
        assert!(!looks_like_svg("<html><svg></svg></html>"));
        assert!(!looks_like_svg("<svgfoo>"));
        assert!(!looks_like_svg("<?xml version=\"1.0\"?>"));
        assert!(!looks_like_svg("Error: <svg> missing"));
    }

    #[test]
    fn inject_theme_puts_a_style_right_after_the_root_tag() {
        let out = inject_theme(SVG, &Theme::dark());
        assert!(out.starts_with(
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10"><style>svg{color:#f2ede6}"#
        ));
        assert!(out.contains("--mf-bg:#131316"));
        assert!(out.ends_with(r#"<rect fill="currentColor"/></svg>"#));
    }

    #[test]
    fn inject_theme_is_quote_aware_and_skips_comments() {
        let svg = r#"<!-- <svg> --><svg data-x="a>b" viewBox="0 0 1 1"><g/></svg>"#;
        let out = inject_theme(svg, &Theme::light());
        assert!(
            out.contains(r#"data-x="a>b" viewBox="0 0 1 1"><style>"#),
            "{out}"
        );
        assert!(out.starts_with("<!-- <svg> -->"));
    }

    #[test]
    fn inject_theme_leaves_a_self_closing_or_missing_root_alone() {
        assert_eq!(inject_theme("<svg/>", &Theme::dark()), "<svg/>");
        assert_eq!(
            inject_theme("<svg width=\"1\"/>", &Theme::dark()),
            "<svg width=\"1\"/>"
        );
        assert_eq!(inject_theme("plain", &Theme::dark()), "plain");
        assert_eq!(
            inject_theme("<svg width=\"1", &Theme::dark()),
            "<svg width=\"1"
        );
    }

    #[test]
    fn resolve_vars_substitutes_known_names_and_keeps_fallbacks_for_unknown() {
        let t = Theme::dark();
        assert_eq!(
            resolve_vars(r#"fill="var(--mf-fg, #000)" stroke="var(--mf-border)""#, &t),
            r##"fill="#f2ede6" stroke="#7a4620""##
        );
        assert_eq!(resolve_vars("var(--mf-nope, #123)", &t), "#123");
        assert_eq!(resolve_vars("var(--mf-nope)", &t), "var(--mf-nope)");
        assert_eq!(resolve_vars("var(--other, red)", &t), "var(--other, red)");
    }

    #[test]
    fn resolve_vars_handles_nested_parens_in_the_fallback() {
        let t = Theme::light();
        assert_eq!(
            resolve_vars(
                "a var(--mf-nope, rgb(1, 2, 3)) b var(--mf-fg, rgb(0,0,0)) c",
                &t
            ),
            "a rgb(1, 2, 3) b #201a14 c"
        );
    }

    #[test]
    fn resolve_vars_leaves_an_unbalanced_var_alone() {
        assert_eq!(
            resolve_vars("x var(--mf-fg, #000", &Theme::dark()),
            "x var(--mf-fg, #000"
        );
    }
}
