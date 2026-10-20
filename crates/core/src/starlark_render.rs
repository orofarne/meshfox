//! Computed Markdown: evaluated by the document owner, never written into
//! authored text and never fed back to Starlark's document snapshot.
use crate::{canvas::Canvas, dirvars::FileAccess};
use serde::{Deserialize, Serialize};
use starlark::environment::{GlobalsBuilder, LibraryExtension, Module};
use std::{cell::RefCell, path::Path};

pub(crate) const MAX_OUTPUT_BYTES: usize = 1024 * 1024;
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RenderResult {
    pub label: String,
    pub markdown: String,
    pub error: Option<String>,
}
#[derive(Default)]
struct Output(RefCell<String>);
impl starlark::PrintHandler for Output {
    fn println(&self, text: &str) -> starlark::Result<()> {
        let mut out = self.0.borrow_mut();
        if out.len().saturating_add(text.len()).saturating_add(1) > MAX_OUTPUT_BYTES {
            return Err(starlark::Error::new_other(anyhow::anyhow!(
                "render output exceeds 1 MiB"
            )));
        }
        out.push_str(text);
        out.push('\n');
        Ok(())
    }
}

pub fn annotate(canvas: &mut Canvas, base_dir: Option<&Path>, access: &FileAccess) {
    if !canvas
        .nodes
        .iter()
        .any(|n| !crate::fence::scan_render_blocks(&n.text).is_empty())
    {
        for node in &mut canvas.nodes {
            node.render_results.clear();
        }
        return;
    }
    let prelude = crate::constraint::build_prelude(canvas, base_dir, access);
    let mut globals =
        GlobalsBuilder::extended_by(&[LibraryExtension::StructType, LibraryExtension::Print]);
    globals.namespace("md", crate::markdown_fragment::markdown_globals);
    let globals = globals.with(crate::constraint::sequence_globals).build();
    for node in &mut canvas.nodes {
        node.render_results = crate::fence::scan_render_blocks(&node.text)
            .into_iter()
            .enumerate()
            .map(|(i, block)| {
                let label = format!(
                    "{}/{}",
                    node.id,
                    block
                        .name
                        .as_deref()
                        .unwrap_or(&format!("render-{}", i + 1))
                );
                let output = Output::default();
                let setup = format!(
                    "{prelude}self = doc.node({})\n",
                    serde_json::to_string(&node.id).unwrap()
                );
                let error = block.error.or_else(|| {
                    Module::with_temp_heap(|module| {
                        crate::constraint::run_module(
                            &module,
                            &setup,
                            &block.code,
                            &label,
                            &globals,
                            None,
                            Some(&output),
                        )
                        .err()
                    })
                });
                RenderResult {
                    label,
                    markdown: if error.is_none() {
                        output.0.into_inner()
                    } else {
                        String::new()
                    },
                    error,
                }
            })
            .collect();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn canvas(code: &str) -> Canvas {
        Canvas::from_markdown(&format!("# Root\n<!-- meshfox:node id=\"root\" -->\n```starlark render name=\"summary\" output=\"markdown\"\n{code}\n```\n## Task\n<!-- meshfox:node id=\"task\" tags=\"todo,task\" -->\n")).unwrap()
    }
    fn render(code: &str) -> RenderResult {
        let mut c = canvas(code);
        annotate(&mut c, None, &FileAccess::none());
        c.nodes[0].render_results.remove(0)
    }
    #[test]
    fn sum_adds_numbers_and_honors_start() {
        let result =
            render("print(sum([1, 2, 3]), sum([0.5, 0.25]), sum([], 7), sum([[1], [2]], []))");
        assert_eq!(result.error, None);
        assert_eq!(result.markdown.trim(), "6 0.75 7 [1, 2]");
        assert!(render("print(sum(['a']))").error.is_some());
    }
    #[test]
    fn colored_fragments_validate_and_compose() {
        let result = render(
            "print(md.background_color(md.color(md.raw('**Alert**'), '#e06c75'), '#fff3b0'))",
        );
        assert!(result.error.is_none(), "{:?}", result.error);
        assert!(result
            .markdown
            .contains("[[**Alert**]{color=\"#e06c75\"}]{background-color=\"#fff3b0\"}"));
        assert!(render("print(md.color('text', 'red'))").error.is_some());
        assert!(render("print(md.color(md.code('block'), '#fff'))")
            .error
            .is_some());
    }

    #[test]
    fn typed_fragments_compose_without_double_escaping() {
        let result = render(
            r#"print(type(md.text("hello")))
print(md.table(headers=["Document", "Count"], rows=[[md.link("A | B", "https://example.com/a%20b"), 2], ["<script> | *x*", 1]]))"#,
        );
        assert_eq!(result.error, None);
        assert!(
            result
                .markdown
                .starts_with("MarkdownFragment\n| Document | Count |\n"),
            "{}",
            result.markdown
        );
        assert!(result
            .markdown
            .contains("[A &#124; B](<https://example.com/a%20b>)"));
        assert!(result.markdown.contains("&lt;script&gt; &#124; \\*x\\*"));
        assert!(!result.markdown.contains("&amp;#124;"));
    }
    #[test]
    fn fragments_adapt_to_table_cells_without_reescaping_markup() {
        let result = render(
            r#"print(md.table(headers=["A", "B"], rows=[[md.raw("*x|y*"), md.raw("a\\|b")], ["line\nnext", md.text("A & B")]]))"#,
        );
        assert_eq!(result.error, None);
        let parser = pulldown_cmark::Parser::new_ext(
            &result.markdown,
            pulldown_cmark::Options::ENABLE_TABLES,
        );
        let mut html = String::new();
        pulldown_cmark::html::push_html(&mut html, parser);
        assert_eq!(html.matches("<td>").count(), 4, "{html}");
        assert!(html.contains("<em>x|y</em>"), "{html}");
        assert!(html.contains("<td>a|b</td>"), "{html}");
        assert!(html.contains("line next"), "{html}");
        assert!(html.contains("A &amp; B"), "{html}");
    }
    #[test]
    fn diagnostics_use_script_line_numbers() {
        let result = render("print('hello')\nundefined_name()");
        assert!(result.error.unwrap().contains("summary.star:2:"));
    }
    #[test]
    fn tag_counts_refresh_from_authored_snapshot() {
        let mut c = canvas(
            r#"counts = {}
for n in [doc] + doc.descendants():
    for tag in n.tags:
        counts[tag] = counts.get(tag, 0) + 1
print(md.table(headers=["Tag", "Nodes"], rows=[[tag, counts[tag]] for tag in sorted(counts)]))"#,
        );
        let text = c.nodes[0].text.clone();
        annotate(&mut c, None, &FileAccess::none());
        assert!(c.nodes[0].render_results[0]
            .markdown
            .contains("| todo | 1 |"));
        c.nodes[1].tags = vec!["done".into()];
        annotate(&mut c, None, &FileAccess::none());
        assert!(c.nodes[0].render_results[0]
            .markdown
            .contains("| done | 1 |"));
        assert!(!c.nodes[0].render_results[0].markdown.contains("todo"));
        assert_eq!(text, c.nodes[0].text);
        assert!(c.nodes[0].constraint_results.is_empty());
    }
    #[test]
    fn invalid_tables_and_blocks_inline_are_errors() {
        for code in [
            r#"print(md.table(headers=["A"], rows=[[1, 2]]))"#,
            r#"print(md.table(headers=[], rows=[]))"#,
            r#"print(md.table(headers=["A"], rows=[[md.code("x")]]))"#,
        ] {
            assert!(render(code).error.is_some(), "{code}");
        }
    }
    #[test]
    fn output_and_execution_are_bounded_and_fail_atomically() {
        for code in [
            "print('partial')\nprint('x' * 1048576)",
            "for i in range(100000000):\n    pass",
            "print('partial')\nundefined_name()",
        ] {
            let result = render(code);
            assert!(result.error.is_some());
            assert!(result.markdown.is_empty());
        }
    }
    #[test]
    fn code_fences_are_safe_and_rendered_output_is_not_executed() {
        let result =
            render("print(md.code('```starlark render\\nprint(123)\\n```', lang='markdown'))");
        assert_eq!(result.error, None);
        assert!(result.markdown.starts_with("````markdown\n"));
    }
    #[test]
    fn plain_starlark_is_inert_and_modes_are_validated() {
        let mut c = Canvas::from_markdown("# Root\n```starlark\nnot_valid()\n```\n```starlark constraint render\nprint(1)\n```\n```starlark render output=\"text\"\nprint(2)\n```").unwrap();
        annotate(&mut c, None, &FileAccess::none());
        assert_eq!(c.nodes[0].render_results.len(), 2);
        assert!(c.nodes[0].render_results.iter().all(|r| r.error.is_some()));
        assert!(crate::constraint::evaluate(&c, None).is_empty());
    }
}
