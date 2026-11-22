//! LaTeX math: typesetting for the TUI, `meshfox static` and `meshfox pdf`.
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

/// `to_svg` sized for a web page: the root's `pt` width and height become
/// `em` (RaTeX lays out at 40 units per em), so a formula is as big as the
/// text around it and follows the reader's font size.
pub fn html_svg(tex: &str, display: bool) -> Option<String> {
    const UNITS_PER_EM: f64 = 40.0;
    let svg = to_svg(tex, display)?;
    // The root ends `... viewBox="0 0 W H" width="Wpt" height="Hpt">`.
    let at = svg.find("\" width=\"")?;
    let (head, tail) = svg.split_at(at + 1);
    let (w, rest) = tail
        .strip_prefix(" width=\"")?
        .split_once("pt\" height=\"")?;
    let (h, rest) = rest.split_once("pt\">")?;
    let em = |v: &str| v.parse::<f64>().ok().map(|v| v / UNITS_PER_EM);
    Some(format!(
        "{head} width=\"{:.3}em\" height=\"{:.3}em\">{rest}",
        em(w)?,
        em(h)?
    ))
}

/// `![](data:image/svg+xml;base64,…)` destination for a display formula.
pub fn svg_data_url(tex: &str) -> Option<String> {
    let svg = to_svg(tex, true)?;
    Some(format!(
        "data:image/svg+xml;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(svg)
    ))
}

/// Longest `$$` block (in lines) that is joined onto one line.
const MAX_DISPLAY_LINES: usize = 200;

fn is_fence_line(line: &str) -> bool {
    let t = line.trim_start_matches(' ');
    line.len() - t.len() <= 3 && (t.starts_with("```") || t.starts_with("~~~"))
}

/// Makes a block-form display formula
///
/// ```text
/// $$
/// a
/// + b
/// $$
/// ```
///
/// survive `pulldown-cmark`, which finds math *inside* a paragraph: a formula
/// line starting with `+`, `-`, `*`, `1.`, `>` or a following `=` would be read
/// as a list item / quote / setext heading first and cut the formula apart
/// (`remark-math`, on the web, parses `$$` as a block, so it has no such
/// problem). The newlines inside the block become spaces, which keeps every
/// byte offset of the document unchanged (span lookups depend on them) and
/// turns the block into the one-line `$$ … $$` form the parser handles.
///
/// Left alone: `$$` inside a code fence, a block with a blank line or a `%`
/// comment in it (a joined `%` would swallow the rest of the formula), one
/// that isn't closed within `MAX_DISPLAY_LINES`, and anything not starting on
/// a line of its own `$$`.
pub fn join_display_math(md: &str) -> std::borrow::Cow<'_, str> {
    if !md.contains("$$") {
        return std::borrow::Cow::Borrowed(md);
    }
    let mut lines: Vec<(usize, &str)> = Vec::new();
    let mut pos = 0;
    for line in md.split_inclusive('\n') {
        lines.push((pos, line));
        pos += line.len();
    }
    fn body(l: &str) -> &str {
        l.trim_end_matches(['\n', '\r'])
    }
    let mut out: Option<String> = None;
    let mut in_fence = false;
    let mut i = 0;
    while i < lines.len() {
        let line = body(lines[i].1);
        if is_fence_line(line) {
            in_fence = !in_fence;
        } else if !in_fence && line.trim() == "$$" {
            let end = (i + 1..lines.len().min(i + 1 + MAX_DISPLAY_LINES)).find(|&j| {
                let t = body(lines[j].1).trim();
                t == "$$" || t.is_empty()
            });
            if let Some(j) = end.filter(|&j| body(lines[j].1).trim() == "$$") {
                let inner = &lines[i + 1..j];
                if !inner.iter().any(|(_, l)| l.contains('%')) {
                    let buf = out.get_or_insert_with(|| md.to_string());
                    // Newline terminating each line from the opener to the
                    // one before the closer (a `\r` before it stays).
                    for &(start, l) in &lines[i..j] {
                        let nl = start + l.len() - 1;
                        if l.ends_with('\n') {
                            buf.replace_range(nl..=nl, " ");
                        }
                    }
                    i = j;
                }
            }
        }
        i += 1;
    }
    match out {
        Some(s) => std::borrow::Cow::Owned(s),
        None => std::borrow::Cow::Borrowed(md),
    }
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
    fn html_svg_is_sized_in_em() {
        let svg = html_svg("x^2", false).unwrap();
        assert!(svg.starts_with("<svg "), "{svg}");
        assert!(svg.contains("em\" height=\""), "{svg}");
        assert!(!svg.contains("pt\""), "{svg}");
        assert!(svg.ends_with("</svg>"));
    }

    /// Every `$$` block and every `$…$` span of the shipped example typesets
    /// (the example is the canonical "does real-world math work" document).
    #[test]
    fn the_navier_stokes_example_typesets_completely() {
        let doc = include_str!("../../../examples/navier-stokes.canvas.md");
        let mut display = 0;
        let mut rest = doc;
        while let Some(i) = rest.find("\n$$\n") {
            let after = &rest[i + 4..];
            let end = after.find("\n$$").expect("closed display block");
            assert!(to_svg(&after[..end], true).is_some(), "{}", &after[..end]);
            display += 1;
            rest = &after[end + 3..];
        }
        assert_eq!(display, 4);
        for tex in [
            r"\mathrm{Re} = \rho U L / \mu",
            r"\tau_{ij}",
            r"q_x, q_y, q_z",
        ] {
            assert!(to_svg(tex, false).is_some(), "{tex}");
        }
    }

    #[test]
    fn join_display_math_keeps_the_byte_length_and_skips_unsafe_blocks() {
        let md = "a\n\n$$\nx\n+ y\n$$\n\n```\n$$\nq\n$$\n```\n\n$$\nr % c\ns\n$$\n\n$$\nunclosed\n\nend\n";
        let out = join_display_math(md);
        assert_eq!(out.len(), md.len());
        assert!(out.contains("$$ x + y $$"), "{out}");
        assert!(out.contains("```\n$$\nq\n$$\n```"), "{out}");
        assert!(out.contains("$$\nr % c\ns\n$$"), "{out}");
        assert!(out.contains("$$\nunclosed\n\nend"), "{out}");
        assert!(matches!(
            join_display_math("no math\n"),
            std::borrow::Cow::Borrowed(_)
        ));
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
