//! LaTeX math for the TUI's Markdown renderer.
//!
//! `$$…$$` (display) is typeset by RaTeX (pure Rust, KaTeX-compatible) into a
//! self-contained SVG and handed to the same `data:image/svg+xml` path as any
//! other SVG image (`svg_raster` → `ratatui-image`). `$…$` (inline) can't be
//! an image — a terminal image can't sit inside a line of text — so it stays
//! text: a Unicode approximation (`inline_text`).

use base64::Engine;
use ratex_layout::{layout, to_display_list, validate_layout, LayoutOptions};
use ratex_parser::parser::parse;
use ratex_svg::{render_to_svg, SvgOptions};
use ratex_types::math_style::MathStyle;

/// Longest TeX source we typeset; anything bigger falls back to plain text
/// rather than risking a slow layout on a pasted wall of math.
const MAX_TEX_LEN: usize = 8 * 1024;

/// Typesets `tex` to a standalone SVG whose ink is `currentColor`, so
/// `svg_raster`'s theming (the TUI's foreground) applies. `None` if the TeX
/// doesn't parse/lay out.
pub fn to_svg(tex: &str, display: bool) -> Option<String> {
    let tex = tex.trim();
    if tex.is_empty() || tex.len() > MAX_TEX_LEN {
        return None;
    }
    let ast = parse(tex).ok()?;
    let style = if display {
        MathStyle::Display
    } else {
        MathStyle::Text
    };
    let lbox = layout(&ast, &LayoutOptions::default().with_style(style));
    validate_layout(&lbox).ok()?;
    let svg = render_to_svg(
        &to_display_list(&lbox),
        &SvgOptions {
            embed_glyphs: true,
            ..Default::default()
        },
    );
    // RaTeX paints in an explicit color (black by default).
    Some(svg.replace("rgba(0,0,0,1)", "currentColor"))
}

/// `![](data:image/svg+xml;base64,…)` destination for a display formula.
pub fn svg_data_url(tex: &str) -> Option<String> {
    let svg = to_svg(tex, true)?;
    Some(format!(
        "data:image/svg+xml;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(svg)
    ))
}

const SYMBOLS: &[(&str, &str)] = &[
    ("alpha", "α"),
    ("beta", "β"),
    ("gamma", "γ"),
    ("delta", "δ"),
    ("epsilon", "ε"),
    ("varepsilon", "ε"),
    ("zeta", "ζ"),
    ("eta", "η"),
    ("theta", "θ"),
    ("vartheta", "ϑ"),
    ("iota", "ι"),
    ("kappa", "κ"),
    ("lambda", "λ"),
    ("mu", "μ"),
    ("nu", "ν"),
    ("xi", "ξ"),
    ("pi", "π"),
    ("rho", "ρ"),
    ("sigma", "σ"),
    ("tau", "τ"),
    ("upsilon", "υ"),
    ("phi", "φ"),
    ("varphi", "φ"),
    ("chi", "χ"),
    ("psi", "ψ"),
    ("omega", "ω"),
    ("Gamma", "Γ"),
    ("Delta", "Δ"),
    ("Theta", "Θ"),
    ("Lambda", "Λ"),
    ("Xi", "Ξ"),
    ("Pi", "Π"),
    ("Sigma", "Σ"),
    ("Phi", "Φ"),
    ("Psi", "Ψ"),
    ("Omega", "Ω"),
    ("sum", "∑"),
    ("prod", "∏"),
    ("int", "∫"),
    ("oint", "∮"),
    ("partial", "∂"),
    ("nabla", "∇"),
    ("infty", "∞"),
    ("pm", "±"),
    ("mp", "∓"),
    ("times", "×"),
    ("cdot", "·"),
    ("div", "÷"),
    ("leq", "≤"),
    ("le", "≤"),
    ("geq", "≥"),
    ("ge", "≥"),
    ("neq", "≠"),
    ("ne", "≠"),
    ("approx", "≈"),
    ("equiv", "≡"),
    ("sim", "∼"),
    ("propto", "∝"),
    ("in", "∈"),
    ("notin", "∉"),
    ("subset", "⊂"),
    ("subseteq", "⊆"),
    ("supset", "⊃"),
    ("cup", "∪"),
    ("cap", "∩"),
    ("emptyset", "∅"),
    ("forall", "∀"),
    ("exists", "∃"),
    ("neg", "¬"),
    ("land", "∧"),
    ("lor", "∨"),
    ("to", "→"),
    ("rightarrow", "→"),
    ("leftarrow", "←"),
    ("leftrightarrow", "↔"),
    ("Rightarrow", "⇒"),
    ("Leftarrow", "⇐"),
    ("Leftrightarrow", "⇔"),
    ("mapsto", "↦"),
    ("ldots", "…"),
    ("cdots", "⋯"),
    ("dots", "…"),
    ("sqrt", "√"),
    ("langle", "⟨"),
    ("rangle", "⟩"),
    ("circ", "∘"),
    ("degree", "°"),
    ("hbar", "ℏ"),
    ("ell", "ℓ"),
    ("Re", "ℜ"),
    ("Im", "ℑ"),
];

fn sup(c: char) -> Option<char> {
    "0123456789+-=()ni".chars().position(|x| x == c).map(|i| {
        [
            '⁰', '¹', '²', '³', '⁴', '⁵', '⁶', '⁷', '⁸', '⁹', '⁺', '⁻', '⁼', '⁽', '⁾', 'ⁿ', 'ⁱ',
        ][i]
    })
}

fn sub(c: char) -> Option<char> {
    "0123456789+-=()".chars().position(|x| x == c).map(|i| {
        [
            '₀', '₁', '₂', '₃', '₄', '₅', '₆', '₇', '₈', '₉', '₊', '₋', '₌', '₍', '₎',
        ][i]
    })
}

/// Reads a script argument after `^`/`_`: one char or a `{…}` group.
fn script_arg(chars: &mut std::iter::Peekable<std::str::Chars>) -> String {
    match chars.next() {
        Some('{') => {
            let mut depth = 1;
            let mut out = String::new();
            for c in chars.by_ref() {
                match c {
                    '{' => depth += 1,
                    '}' => {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                    _ => {}
                }
                out.push(c);
            }
            out
        }
        Some(c) => c.to_string(),
        None => String::new(),
    }
}

/// One-line Unicode approximation of an inline formula. Not a typesetter:
/// symbols, simple scripts (when every character has a Unicode script form,
/// else `^{…}`/`_{…}` stays visible), `a/b` left as-is.
pub fn inline_text(tex: &str) -> String {
    let mut out = String::new();
    let mut chars = tex.trim().chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                let mut name = String::new();
                while let Some(&n) = chars.peek() {
                    if n.is_ascii_alphabetic() {
                        name.push(n);
                        chars.next();
                    } else {
                        break;
                    }
                }
                if name.is_empty() {
                    match chars.next() {
                        Some(',') | Some(';') | Some(':') | Some('!') => {}
                        Some(o) => out.push(o),
                        None => {}
                    }
                } else if let Some((_, s)) = SYMBOLS.iter().find(|(k, _)| *k == name) {
                    out.push_str(s);
                } else if matches!(
                    name.as_str(),
                    "left"
                        | "right"
                        | "displaystyle"
                        | "mathrm"
                        | "mathbf"
                        | "text"
                        | "operatorname"
                ) {
                } else if name == "frac" {
                    let a = script_arg(&mut chars);
                    let b = script_arg(&mut chars);
                    out.push_str(&format!("({})/({})", inline_text(&a), inline_text(&b)));
                } else {
                    out.push('\\');
                    out.push_str(&name);
                }
            }
            '^' | '_' => {
                let arg = inline_text(&script_arg(&mut chars));
                let map = if c == '^' { sup } else { sub };
                let converted: Option<String> = arg.chars().map(map).collect();
                match converted {
                    Some(s) if !s.is_empty() => out.push_str(&s),
                    _ => out.push_str(&format!("{c}{{{arg}}}")),
                }
            }
            '{' | '}' => {}
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_formula_becomes_a_themable_svg() {
        let svg = to_svg(r"\frac{a}{b}", true).unwrap();
        assert!(svg.starts_with("<svg"));
        assert!(svg.contains("currentColor"));
        assert!(!svg.contains("rgba(0,0,0,1)"));
    }

    #[test]
    fn bad_tex_and_oversized_input_are_rejected() {
        assert!(to_svg(r"\foo{bad", true).is_none());
        assert!(to_svg("", true).is_none());
        assert!(to_svg(&"x".repeat(MAX_TEX_LEN + 1), true).is_none());
    }

    #[test]
    fn data_url_rasterizes_through_the_shared_svg_path() {
        let url = svg_data_url(r"\int_0^1 x\,dx").unwrap();
        let b64 = url.strip_prefix("data:image/svg+xml;base64,").unwrap();
        let svg = base64::engine::general_purpose::STANDARD
            .decode(b64)
            .unwrap();
        let img = super::super::svg_raster::rasterize(std::str::from_utf8(&svg).unwrap(), None);
        assert!(img.is_some());
    }

    #[test]
    fn inline_text_uses_unicode_where_it_can() {
        assert_eq!(inline_text(r"E = mc^2"), "E = mc²");
        assert_eq!(inline_text(r"\alpha_1 + \beta"), "α₁ + β");
        assert_eq!(inline_text(r"x^{n+1}"), "xⁿ⁺¹");
        assert_eq!(inline_text(r"e^{i\pi}"), "e^{iπ}");
        assert_eq!(inline_text(r"\frac{a}{b}"), "(a)/(b)");
        assert_eq!(inline_text(r"\foo"), "\\foo");
    }
}
