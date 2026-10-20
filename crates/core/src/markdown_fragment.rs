//! Typed Markdown builders for embedded Starlark. Fragments retain their
//! identity until the final print; strings are escaped at their insertion site.
use allocative::Allocative;
use starlark::environment::GlobalsBuilder;
use starlark::values::{
    list::ListRef, starlark_value, NoSerialize, ProvidesStaticType, StarlarkPagable, StarlarkValue,
    Value, ValueLike,
};

#[derive(Debug, ProvidesStaticType, NoSerialize, StarlarkPagable, Allocative)]
pub struct MarkdownFragment {
    markdown: String,
    block: bool,
}
starlark::starlark_simple_value!(MarkdownFragment);
impl std::fmt::Display for MarkdownFragment {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.markdown)
    }
}
#[starlark_value(type = "MarkdownFragment")]
impl<'v> StarlarkValue<'v> for MarkdownFragment {}

fn escape(text: &str) -> String {
    let mut out = String::new();
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '|' => out.push_str("&#124;"),
            '\\' | '`' | '*' | '_' | '[' | ']' | '(' | ')' | '#' | '!' | '+' | '-' | '.' | '~'
            | '^' => {
                out.push('\\');
                out.push(c);
            }
            _ => out.push(c),
        }
    }
    out
}
fn inline(value: Value<'_>) -> anyhow::Result<String> {
    if let Some(fragment) = value.downcast_ref::<MarkdownFragment>() {
        anyhow::ensure!(
            !fragment.block && !fragment.markdown.contains(['\n', '\r']),
            "a block MarkdownFragment cannot be used inline"
        );
        Ok(fragment.markdown.clone())
    } else {
        Ok(escape(&value.to_str()).replace(['\n', '\r'], " "))
    }
}
fn list<'v>(value: Value<'v>) -> anyhow::Result<&'v ListRef<'v>> {
    ListRef::from_value(value).ok_or_else(|| anyhow::anyhow!("expected a list"))
}
fn table_cell(value: Value<'_>) -> anyhow::Result<String> {
    let text = inline(value)?;
    let mut out = String::new();
    let mut backslashes = 0;
    for c in text.chars() {
        if c == '|' && backslashes % 2 == 0 {
            out.push('\\');
        }
        out.push(c);
        backslashes = if c == '\\' { backslashes + 1 } else { 0 };
    }
    Ok(out)
}
fn fragment(markdown: String, block: bool) -> anyhow::Result<MarkdownFragment> {
    anyhow::ensure!(
        markdown.len() <= crate::starlark_render::MAX_OUTPUT_BYTES,
        "MarkdownFragment exceeds 1 MiB"
    );
    Ok(MarkdownFragment { markdown, block })
}

fn colored(value: Value<'_>, key: &str, color: &str) -> anyhow::Result<MarkdownFragment> {
    let Some(crate::image_attrs::Background::Rgb(r, g, b)) =
        crate::image_attrs::Background::parse(color)
    else {
        anyhow::bail!("expected a hex color (#rgb or #rrggbb)");
    };
    fragment(
        format!("[{}]{{{key}=\"#{r:02x}{g:02x}{b:02x}\"}}", inline(value)?),
        false,
    )
}

#[starlark::starlark_module]
pub fn markdown_globals(builder: &mut GlobalsBuilder) {
    fn text(value: Value<'_>) -> anyhow::Result<MarkdownFragment> {
        fragment(escape(&value.to_str()), false)
    }
    fn raw(text: &str) -> anyhow::Result<MarkdownFragment> {
        anyhow::ensure!(
            text.len() <= crate::starlark_render::MAX_OUTPUT_BYTES,
            "MarkdownFragment exceeds 1 MiB"
        );
        fragment(text.to_owned(), text.contains(['\n', '\r']))
    }
    fn link(label: Value<'_>, url: &str) -> anyhow::Result<MarkdownFragment> {
        anyhow::ensure!(
            url.len() <= crate::starlark_render::MAX_OUTPUT_BYTES,
            "MarkdownFragment exceeds 1 MiB"
        );
        let url = url
            .replace(' ', "%20")
            .replace('(', "%28")
            .replace(')', "%29")
            .replace('<', "%3C")
            .replace('>', "%3E")
            .replace('"', "%22")
            .replace('\\', "%5C")
            .replace('|', "%7C")
            .replace('\n', "%0A")
            .replace('\r', "%0D");
        fragment(format!("[{}](<{}>)", inline(label)?, url), false)
    }
    /// Set a foreground color on escaped text or an existing inline fragment.
    fn color(value: Value<'_>, color: &str) -> anyhow::Result<MarkdownFragment> {
        colored(value, "color", color)
    }
    /// Set a background color; nests with md.color and other inline fragments.
    fn background_color(value: Value<'_>, color: &str) -> anyhow::Result<MarkdownFragment> {
        colored(value, "background-color", color)
    }
    fn code(text: &str, #[starlark(default = "")] lang: &str) -> anyhow::Result<MarkdownFragment> {
        anyhow::ensure!(!lang.contains(['\n', '\r', '`']), "invalid code language");
        anyhow::ensure!(
            text.len().saturating_add(lang.len()) <= crate::starlark_render::MAX_OUTPUT_BYTES,
            "MarkdownFragment exceeds 1 MiB"
        );
        let fence = "`".repeat(crate::output::safe_fence_len(text));
        fragment(format!("{fence}{lang}\n{text}\n{fence}"), true)
    }
    fn table(headers: Value<'_>, rows: Value<'_>) -> anyhow::Result<MarkdownFragment> {
        let headers = list(headers)?;
        anyhow::ensure!(!headers.is_empty(), "a table needs at least one column");
        let row = |cells: &ListRef<'_>| -> anyhow::Result<String> {
            anyhow::ensure!(
                cells.len() == headers.len(),
                "table row width must match headers"
            );
            let mut line = String::from("| ");
            for cell in cells.iter() {
                let cell = table_cell(cell)?;
                anyhow::ensure!(
                    line.len().saturating_add(cell.len()).saturating_add(3)
                        <= crate::starlark_render::MAX_OUTPUT_BYTES,
                    "MarkdownFragment exceeds 1 MiB"
                );
                line.push_str(&cell);
                line.push_str(" | ");
            }
            line.pop();
            line.push('\n');
            Ok(line)
        };
        let mut out = row(headers)?;
        out.push_str(&format!("| {} |\n", vec!["---"; headers.len()].join(" | ")));
        for cells in list(rows)?.iter() {
            let next = row(list(cells)?)?;
            anyhow::ensure!(
                out.len().saturating_add(next.len()) <= crate::starlark_render::MAX_OUTPUT_BYTES,
                "MarkdownFragment exceeds 1 MiB"
            );
            out.push_str(&next);
        }
        fragment(out, true)
    }
}
