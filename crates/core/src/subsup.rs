//! Narrow Pandoc/kramdown-style subscript/superscript syntax: `x~2~`
//! (subscript) and `x^2^` (superscript) — see SPEC.md and TODO.canvas.md's
//! `sub-superscript` task. Deliberately single-delimiter and
//! whitespace-free inside, both to keep the grammar narrow and to avoid
//! any ambiguity with GFM's own `~~strikethrough~~` (a *doubled* `~`,
//! already consumed by `pulldown-cmark`/`remark-gfm` before this ever
//! sees raw text — but `scan` below still refuses to treat a delimiter
//! that's itself part of a doubled run as an opener/closer, so stray
//! `~~`/`^^` sequences are never misparsed as an empty or nested mark).
//!
//! `scan` is the one piece shared by every consumer (`staticgen.rs`
//! wraps a `Marked` run in `<sub>`/`<sup>`; the TUI's `markdown.rs`
//! substitutes Unicode small-form characters via `render_unicode` below
//! instead, since a terminal can't apply real subscript/superscript
//! styling) — the web side mirrors this same scan in its own small
//! remark plugin (`web/src/remarkSubSup.ts`), since nothing here is
//! reachable from TypeScript.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Script {
    Sub,
    Sup,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Piece<'a> {
    Text(&'a str),
    Marked(Script, &'a str),
}

fn delim_for(c: char) -> Option<Script> {
    match c {
        '~' => Some(Script::Sub),
        '^' => Some(Script::Sup),
        _ => None,
    }
}

/// Splits `text` into plain-text and marked (`~..~`/`^..^`) runs. A
/// delimiter only opens/closes a run when: it isn't itself part of a
/// doubled `~~`/`^^` sequence, the run's content is non-empty, and the
/// content contains no whitespace (matching Pandoc/kramdown's own rule —
/// this is what lets `scan` tell "a real mark" apart from a stray `~`
/// used as ordinary punctuation without needing a closing-delimiter
/// escape hatch). Anything that doesn't parse this way is left as
/// ordinary text, delimiters included — the same "fails closed to
/// literal text" fallback every other narrow meshfox Markdown extension
/// uses.
pub fn scan(text: &str) -> Vec<Piece<'_>> {
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let n = chars.len();
    let mut pieces = Vec::new();
    let mut text_start = 0usize;
    let mut i = 0usize;
    while i < n {
        let (pos, c) = chars[i];
        let Some(script) = delim_for(c) else {
            i += 1;
            continue;
        };
        let prev_same = i > 0 && chars[i - 1].1 == c;
        let next_same = i + 1 < n && chars[i + 1].1 == c;
        if prev_same || next_same {
            // Part of a doubled `~~`/`^^` run (GFM strikethrough or a
            // plain stray double punctuation) — never one of ours.
            i += 1;
            continue;
        }
        let mut j = i + 1;
        let mut close = None;
        while j < n {
            let (_, cj) = chars[j];
            if cj.is_whitespace() {
                break;
            }
            if cj == c {
                let close_next_same = j + 1 < n && chars[j + 1].1 == c;
                if !close_next_same {
                    close = Some(j);
                }
                break;
            }
            j += 1;
        }
        let Some(close_idx) = close else {
            i += 1;
            continue;
        };
        if pos > text_start {
            pieces.push(Piece::Text(&text[text_start..pos]));
        }
        let content_start = chars[i + 1].0;
        let content_end = chars[close_idx].0;
        pieces.push(Piece::Marked(script, &text[content_start..content_end]));
        text_start = content_end + c.len_utf8();
        i = close_idx + 1;
    }
    if text_start < text.len() {
        pieces.push(Piece::Text(&text[text_start..]));
    }
    pieces
}

/// Unicode small-form substitute for one base character, if one exists —
/// coverage is genuinely incomplete (especially for subscript: no
/// uppercase letters at all, and several lowercase letters have no
/// small-form glyph in Unicode either). `None` means no substitute
/// exists, which `to_unicode` turns into "leave the whole run as literal
/// source text" rather than a half-transliterated mix.
fn map_char(c: char, script: Script) -> Option<char> {
    match script {
        Script::Sup => Some(match c {
            '0' => '⁰',
            '1' => '¹',
            '2' => '²',
            '3' => '³',
            '4' => '⁴',
            '5' => '⁵',
            '6' => '⁶',
            '7' => '⁷',
            '8' => '⁸',
            '9' => '⁹',
            '+' => '⁺',
            '-' => '⁻',
            '=' => '⁼',
            '(' => '⁽',
            ')' => '⁾',
            'a' => 'ᵃ',
            'b' => 'ᵇ',
            'c' => 'ᶜ',
            'd' => 'ᵈ',
            'e' => 'ᵉ',
            'f' => 'ᶠ',
            'g' => 'ᵍ',
            'h' => 'ʰ',
            'i' => 'ⁱ',
            'j' => 'ʲ',
            'k' => 'ᵏ',
            'l' => 'ˡ',
            'm' => 'ᵐ',
            'n' => 'ⁿ',
            'o' => 'ᵒ',
            'p' => 'ᵖ',
            'r' => 'ʳ',
            's' => 'ˢ',
            't' => 'ᵗ',
            'u' => 'ᵘ',
            'v' => 'ᵛ',
            'w' => 'ʷ',
            'x' => 'ˣ',
            'y' => 'ʸ',
            'z' => 'ᶻ',
            _ => return None,
        }),
        Script::Sub => Some(match c {
            '0' => '₀',
            '1' => '₁',
            '2' => '₂',
            '3' => '₃',
            '4' => '₄',
            '5' => '₅',
            '6' => '₆',
            '7' => '₇',
            '8' => '₈',
            '9' => '₉',
            '+' => '₊',
            '-' => '₋',
            '=' => '₌',
            '(' => '₍',
            ')' => '₎',
            'a' => 'ₐ',
            'e' => 'ₑ',
            'h' => 'ₕ',
            'i' => 'ᵢ',
            'j' => 'ⱼ',
            'k' => 'ₖ',
            'l' => 'ₗ',
            'm' => 'ₘ',
            'n' => 'ₙ',
            'o' => 'ₒ',
            'p' => 'ₚ',
            'r' => 'ᵣ',
            's' => 'ₛ',
            't' => 'ₜ',
            'u' => 'ᵤ',
            'v' => 'ᵥ',
            'x' => 'ₓ',
            _ => return None,
        }),
    }
}

/// `Some` only when *every* character in `text` has a Unicode small-form
/// substitute, so the whole run transliterates cleanly — see `map_char`.
pub fn to_unicode(text: &str, script: Script) -> Option<String> {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        out.push(map_char(c, script)?);
    }
    Some(out)
}

/// TUI-only convenience: scans `text` for `~..~`/`^..^` runs and replaces
/// each with its Unicode small-form substitute where one fully exists
/// (`to_unicode`), or leaves it as the original literal `~text~`/`^text^`
/// (delimiters included) otherwise — a terminal has no real subscript/
/// superscript styling to fall back on, so an incomplete substitution
/// would just be confusing.
pub fn render_unicode(text: &str) -> String {
    let pieces = scan(text);
    if pieces.len() == 1 && matches!(pieces[0], Piece::Text(t) if t == text) {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    for piece in pieces {
        match piece {
            Piece::Text(t) => out.push_str(t),
            Piece::Marked(script, inner) => match to_unicode(inner, script) {
                Some(sub) => out.push_str(&sub),
                None => {
                    let delim = match script {
                        Script::Sub => '~',
                        Script::Sup => '^',
                    };
                    out.push(delim);
                    out.push_str(inner);
                    out.push(delim);
                }
            },
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scans_a_simple_subscript() {
        assert_eq!(
            scan("x~2~ y"),
            vec![
                Piece::Text("x"),
                Piece::Marked(Script::Sub, "2"),
                Piece::Text(" y"),
            ]
        );
    }

    #[test]
    fn scans_a_simple_superscript() {
        assert_eq!(
            scan("E=mc^2^."),
            vec![
                Piece::Text("E=mc"),
                Piece::Marked(Script::Sup, "2"),
                Piece::Text("."),
            ]
        );
    }

    #[test]
    fn leaves_doubled_tilde_strikethrough_untouched() {
        assert_eq!(scan("a ~~b~~ c"), vec![Piece::Text("a ~~b~~ c")]);
    }

    #[test]
    fn leaves_whitespace_broken_content_untouched() {
        assert_eq!(scan("a ~b c~ d"), vec![Piece::Text("a ~b c~ d")]);
    }

    #[test]
    fn leaves_unclosed_delimiter_untouched() {
        assert_eq!(scan("a ~b c"), vec![Piece::Text("a ~b c")]);
    }

    #[test]
    fn leaves_empty_marker_untouched() {
        assert_eq!(scan("a ~~ b"), vec![Piece::Text("a ~~ b")]);
    }

    #[test]
    fn scans_more_than_one_mark_in_the_same_text() {
        assert_eq!(
            scan("H~2~O and x^2^"),
            vec![
                Piece::Text("H"),
                Piece::Marked(Script::Sub, "2"),
                Piece::Text("O and x"),
                Piece::Marked(Script::Sup, "2"),
            ]
        );
    }

    #[test]
    fn to_unicode_full_coverage() {
        assert_eq!(to_unicode("2", Script::Sub).as_deref(), Some("₂"));
        assert_eq!(to_unicode("2", Script::Sup).as_deref(), Some("²"));
        assert_eq!(to_unicode("th", Script::Sup).as_deref(), Some("ᵗʰ"));
    }

    #[test]
    fn to_unicode_none_on_any_unmapped_char() {
        // 'q' has no subscript or superscript Unicode glyph.
        assert_eq!(to_unicode("q", Script::Sub), None);
        assert_eq!(to_unicode("Q", Script::Sup), None); // uppercase unmapped
    }

    #[test]
    fn render_unicode_substitutes_when_fully_mapped() {
        assert_eq!(render_unicode("x~2~ + y^n^"), "x₂ + yⁿ");
    }

    #[test]
    fn render_unicode_falls_back_to_literal_when_not_fully_mapped() {
        assert_eq!(render_unicode("x~query~"), "x~query~");
    }

    #[test]
    fn render_unicode_is_a_no_op_without_any_marks() {
        assert_eq!(render_unicode("plain text"), "plain text");
    }
}
