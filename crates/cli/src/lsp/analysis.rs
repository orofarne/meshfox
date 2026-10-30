//! What `meshfox lsp` knows about a document, as pure functions of the
//! text the editor sent: no filesystem, no worker. The text is the editor's
//! buffer — possibly unsaved, which is exactly why this is not something the
//! worker (which only sees the saved file) can answer.

use std::collections::HashSet;
use std::ops::Range;

use lsp_types::{
    CodeAction, CodeActionKind, CompletionItem, CompletionItemKind, CompletionTextEdit, Diagnostic,
    DiagnosticSeverity, DocumentSymbol, InsertTextFormat, Position, SymbolKind, TextEdit, Uri,
    WorkspaceEdit,
};
use meshfox_core::mdcanvas::{self, HeadingSpan, NodeSpan, ParseError};
use meshfox_core::suggest::{self, ItemKind};
use meshfox_core::Canvas;

/// Byte offsets ⇄ LSP positions (a line and a UTF-16 column).
pub struct LineIndex<'a> {
    text: &'a str,
    starts: Vec<usize>,
}

impl<'a> LineIndex<'a> {
    pub fn new(text: &'a str) -> Self {
        let mut starts = vec![0];
        starts.extend(text.match_indices('\n').map(|(i, _)| i + 1));
        LineIndex { text, starts }
    }

    pub fn position(&self, offset: usize) -> Position {
        let offset = offset.min(self.text.len());
        let line = self.starts.partition_point(|&s| s <= offset) - 1;
        let col = self.text[self.starts[line]..offset]
            .chars()
            .map(char::len_utf16)
            .sum::<usize>();
        Position::new(line as u32, col as u32)
    }

    pub fn range(&self, r: Range<usize>) -> lsp_types::Range {
        lsp_types::Range::new(self.position(r.start), self.position(r.end))
    }

    /// The byte offset of `pos`, clamped to the line's end. Used by the
    /// tests now; completion needs it next.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn offset(&self, pos: Position) -> usize {
        let Some(&start) = self.starts.get(pos.line as usize) else {
            return self.text.len();
        };
        let line_end = self
            .starts
            .get(pos.line as usize + 1)
            .copied()
            .unwrap_or(self.text.len());
        let mut units = 0;
        for (i, c) in self.text[start..line_end].char_indices() {
            if units >= pos.character as usize || c == '\n' {
                return start + i;
            }
            units += c.len_utf16();
        }
        line_end
    }

    pub fn line_of(&self, offset: usize) -> usize {
        self.position(offset).line as usize
    }
}

/// `r` without a trailing `\n` / `\r\n`.
fn without_eol(text: &str, r: Range<usize>) -> Range<usize> {
    let mut end = r.end;
    if text[r.start..end].ends_with('\n') {
        end -= 1;
        if text[r.start..end].ends_with('\r') {
            end -= 1;
        }
    }
    r.start..end
}

/// The marker line of a node if it has one, else its heading.
fn node_range(text: &str, span: &NodeSpan) -> Range<usize> {
    without_eol(text, span.marker.clone().unwrap_or(span.heading.clone()))
}

fn span_for_id<'a>(spans: &'a [NodeSpan], id: &str) -> Option<&'a NodeSpan> {
    // The last match: for a duplicate id, the second one is the offender.
    spans
        .iter()
        .rev()
        .find(|s| s.id.as_deref() == Some(id) || s.explicit_id.as_deref() == Some(id))
}

struct Problem {
    range: Range<usize>,
    message: String,
}

fn first_line(text: &str) -> Range<usize> {
    without_eol(text, 0..text.find('\n').map_or(text.len(), |i| i + 1))
}

fn locate_parse_error(e: &ParseError, text: &str, spans: &[NodeSpan]) -> Range<usize> {
    let by_id = |id: &String| span_for_id(spans, id).map(|s| node_range(text, s));
    match e {
        ParseError::NoRoot => None,
        ParseError::MultipleRoots => spans
            .iter()
            .filter(|s| s.level == 1)
            .nth(1)
            .map(|s| node_range(text, s)),
        ParseError::DuplicateId(id)
        | ParseError::InvalidId(id)
        | ParseError::ReservedId(id)
        | ParseError::UnknownParent(id, _)
        | ParseError::UnknownExplicitParent(id, _)
        | ParseError::CyclicParent(id)
        | ParseError::UnknownNodeType(id, _)
        | ParseError::PreviewRequiresLinkType(id)
        | ParseError::InvalidCsvStrict(id)
        | ParseError::GroupHasBody(id)
        | ParseError::InvalidLinkBody(id, _)
        | ParseError::InvalidTimestamp(id, _, _) => by_id(id),
    }
    .unwrap_or_else(|| first_line(text))
}

/// The `attr` key on the first meshfox marker or fence-info line that has it.
fn locate_attr(text: &str, attr: &str) -> Option<Range<usize>> {
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        let t = line.trim_start();
        if t.starts_with("<!-- meshfox:") || t.starts_with("```") || t.starts_with("~~~") {
            for (i, _) in line.match_indices(attr) {
                let before_ok = line[..i]
                    .chars()
                    .next_back()
                    .is_none_or(|c| c.is_whitespace());
                let after_ok = line[i + attr.len()..]
                    .chars()
                    .next()
                    .is_none_or(|c| c == '=' || c.is_whitespace() || c == '-');
                if before_ok && after_ok {
                    return Some(offset + i..offset + i + attr.len());
                }
            }
        }
        offset += line.len();
    }
    None
}

/// The message names a node as a quoted id (`{:?}` of the id).
fn locate_message(msg: &str, text: &str, spans: &[NodeSpan]) -> Range<usize> {
    if let Err(e) = meshfox_core::validate_known_attrs(text) {
        if e.to_string() == msg {
            if let Some(r) = locate_attr(text, &e.attr) {
                return r;
            }
        }
    }
    spans
        .iter()
        .filter(|s| {
            [s.id.as_deref(), s.explicit_id.as_deref()]
                .into_iter()
                .flatten()
                .any(|id| msg.contains(&format!("{id:?}")))
        })
        .map(|s| node_range(text, s))
        .next()
        .unwrap_or_else(|| first_line(text))
}

/// The document's first failing rule — the same rules `meshfox validate`
/// applies, except what needs the filesystem (`include` targets).
fn first_problem(text: &str, spans: &[NodeSpan]) -> Result<Canvas, Problem> {
    let canvas = Canvas::from_markdown(text).map_err(|e| Problem {
        range: locate_parse_error(&e, text, spans),
        message: e.to_string(),
    })?;
    let check = meshfox_core::args::validate(&canvas)
        .map_err(|e| e.to_string())
        .and_then(|()| crate::validate_document_rules(&canvas, text));
    match check {
        Ok(()) => Ok(canvas),
        Err(message) => Err(Problem {
            range: locate_message(&message, text, spans),
            message,
        }),
    }
}

pub fn diagnostics(text: &str) -> Vec<Diagnostic> {
    let index = LineIndex::new(text);
    let spans = mdcanvas::node_spans(text);
    let diagnostic =
        |range: Range<usize>, severity, code: Option<&str>, message: String| Diagnostic {
            range: index.range(range),
            severity: Some(severity),
            code: code.map(|c| lsp_types::NumberOrString::String(c.to_string())),
            source: Some("meshfox".to_string()),
            message,
            ..Default::default()
        };
    match first_problem(text, &spans) {
        Err(p) => vec![diagnostic(
            p.range,
            DiagnosticSeverity::ERROR,
            None,
            p.message,
        )],
        Ok(canvas) => meshfox_core::diagnostics::warnings(&canvas)
            .unwrap_or_default()
            .into_iter()
            .map(|w| {
                let range = span_for_id(&spans, &w.node_id)
                    .map(|s| node_range(text, s))
                    .unwrap_or_else(|| first_line(text));
                diagnostic(range, DiagnosticSeverity::WARNING, Some(w.code), w.message)
            })
            .collect(),
    }
}

/// The node tree by heading nesting (what a reader of the file sees).
pub fn symbols(text: &str) -> Vec<DocumentSymbol> {
    let index = LineIndex::new(text);
    let spans = mdcanvas::node_spans(text);
    // The subtree of node i runs to the next node at its level or above.
    let subtree_end = |i: usize| {
        spans[i + 1..]
            .iter()
            .find(|s| s.level <= spans[i].level)
            .map_or(text.len(), |s| s.heading.start)
    };
    // children[i]: direct children, by a stack of open ancestors.
    let mut children = vec![Vec::new(); spans.len()];
    let mut roots = Vec::new();
    let mut stack: Vec<usize> = Vec::new();
    for (i, span) in spans.iter().enumerate() {
        while stack.last().is_some_and(|&p| spans[p].level >= span.level) {
            stack.pop();
        }
        match stack.last() {
            Some(&p) => children[p].push(i),
            None => roots.push(i),
        }
        stack.push(i);
    }

    #[allow(deprecated)] // `deprecated` is a required field of the struct
    fn build(
        i: usize,
        spans: &[NodeSpan],
        children: &[Vec<usize>],
        index: &LineIndex,
        text: &str,
        end_of: &dyn Fn(usize) -> usize,
    ) -> DocumentSymbol {
        let span = &spans[i];
        let kids = children[i]
            .iter()
            .map(|&c| build(c, spans, children, index, text, end_of))
            .collect::<Vec<_>>();
        DocumentSymbol {
            name: if span.title.is_empty() {
                "(untitled)".to_string()
            } else {
                span.title.clone()
            },
            detail: span.id.clone().or_else(|| span.explicit_id.clone()),
            kind: if span.level == 1 {
                SymbolKind::MODULE
            } else {
                SymbolKind::OBJECT
            },
            tags: None,
            deprecated: None,
            range: index.range(span.heading.start..end_of(i)),
            selection_range: index.range(without_eol(text, span.heading.clone())),
            children: (!kids.is_empty()).then_some(kids),
        }
    }
    roots
        .into_iter()
        .map(|i| build(i, &spans, &children, &index, text, &subtree_end))
        .collect()
}

/// `<!-- meshfox:node id="…" -->` for each of `headings` (a heading that
/// ends the file without a newline gets one first).
fn node_marker_edits(
    text: &str,
    index: &LineIndex,
    headings: &[&HeadingSpan],
    used: &mut HashSet<String>,
) -> Vec<TextEdit> {
    headings
        .iter()
        .map(|h| {
            let id = mdcanvas::random_node_id(used);
            used.insert(id.clone());
            let marker = format!("<!-- meshfox:node id=\"{id}\" -->\n");
            let at = index.position(h.span.end);
            let new_text = if text[h.span.clone()].ends_with('\n') {
                marker
            } else {
                format!("\n{marker}")
            };
            TextEdit {
                range: lsp_types::Range::new(at, at),
                new_text,
            }
        })
        .collect()
}

/// "Turn this heading into a node" for the heading under `cursor`, and the
/// bulk version for every plain heading of the document — what `Ctrl-n` does
/// in the TUI's source editor.
pub fn code_actions(text: &str, uri: &Uri, range: lsp_types::Range) -> Vec<CodeAction> {
    let index = LineIndex::new(text);
    let headings = mdcanvas::headings(text);
    let plain: Vec<&HeadingSpan> = headings.iter().filter(|h| !h.is_node).collect();
    if plain.is_empty() {
        return Vec::new();
    }
    let mut used: HashSet<String> = mdcanvas::node_spans(text)
        .into_iter()
        .flat_map(|s| [s.id, s.explicit_id])
        .flatten()
        .collect();
    let (first, last) = (range.start.line as usize, range.end.line as usize);
    let under_cursor: Vec<&HeadingSpan> = plain
        .iter()
        .copied()
        .filter(|h| (first..=last).contains(&index.line_of(h.span.start)))
        .collect();
    let action = |title: String, hs: &[&HeadingSpan], used: &mut HashSet<String>| CodeAction {
        title,
        kind: Some(CodeActionKind::REFACTOR_REWRITE),
        edit: Some(WorkspaceEdit {
            changes: Some(
                [(uri.clone(), node_marker_edits(text, &index, hs, used))]
                    .into_iter()
                    .collect(),
            ),
            ..Default::default()
        }),
        ..Default::default()
    };
    let mut actions = Vec::new();
    if let Some(h) = under_cursor.first() {
        let mut u = used.clone();
        actions.push(action(
            format!("Turn \"{}\" into a meshfox node", h.title),
            &[h],
            &mut u,
        ));
    }
    if plain.len() > under_cursor.len().min(1) {
        actions.push(action(
            format!("Turn all {} plain headings into meshfox nodes", plain.len()),
            &plain,
            &mut used,
        ));
    }
    actions
}

/// Completion items at `pos` — the same vocabulary and values as `Ctrl-p` in
/// the TUI's source editor (`meshfox_core::suggest`), plus the marker
/// snippets. The client filters by what is typed in the replaced range.
pub fn completion(text: &str, pos: Position) -> Vec<CompletionItem> {
    let row = pos.line as usize;
    let Some(line) = text.lines().nth(row) else {
        return Vec::new();
    };
    // LSP columns are UTF-16 units; `suggest` counts chars.
    let (mut units, mut col) = (0, 0);
    for c in line.chars() {
        if units >= pos.character as usize {
            break;
        }
        units += c.len_utf16();
        col += 1;
    }
    let Some(found) = suggest::complete(text, row, col) else {
        return Vec::new();
    };
    let to_utf16 =
        |chars: usize| line.chars().take(chars).map(char::len_utf16).sum::<usize>() as u32;
    let range = lsp_types::Range::new(
        Position::new(pos.line, to_utf16(found.start)),
        Position::new(pos.line, to_utf16(found.end)),
    );
    found
        .items
        .into_iter()
        .enumerate()
        .map(|(i, item)| CompletionItem {
            kind: Some(match item.kind {
                ItemKind::Attribute => CompletionItemKind::PROPERTY,
                ItemKind::Flag => CompletionItemKind::KEYWORD,
                ItemKind::Value => CompletionItemKind::VALUE,
                ItemKind::Snippet => CompletionItemKind::SNIPPET,
            }),
            // Keep the vocabulary's own order (the client sorts otherwise).
            sort_text: Some(format!("{i:04}")),
            filter_text: item.filter,
            detail: item.detail,
            insert_text_format: Some(if item.snippet {
                InsertTextFormat::SNIPPET
            } else {
                InsertTextFormat::PLAIN_TEXT
            }),
            text_edit: Some(CompletionTextEdit::Edit(TextEdit {
                range,
                new_text: item.insert,
            })),
            label: item.label,
            ..Default::default()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOC: &str = "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n\n## Tests\n<!-- meshfox:node id=\"tests\" -->\n\n### Smoke\n<!-- meshfox:node id=\"smoke\" -->\n\n## Plain\ntext\n";

    fn uri() -> Uri {
        "file:///tmp/x.canvas.md".parse().unwrap()
    }

    fn lines(d: &Diagnostic) -> (u32, u32) {
        (d.range.start.line, d.range.end.line)
    }

    #[test]
    fn positions_count_utf16_units_and_round_trip() {
        let text = "a😀b\nsecond\n";
        let index = LineIndex::new(text);
        // 😀 is 4 bytes in UTF-8 and 2 units in UTF-16.
        assert_eq!(index.position(5), Position::new(0, 3));
        assert_eq!(index.offset(Position::new(0, 3)), 5);
        assert_eq!(index.position(text.len()), Position::new(2, 0));
        assert_eq!(index.offset(Position::new(1, 100)), 13);
    }

    #[test]
    fn a_valid_document_has_no_diagnostics() {
        assert!(diagnostics(DOC).is_empty());
    }

    #[test]
    fn a_missing_root_is_reported_on_the_first_line() {
        let d = diagnostics("just text\n## Sub\n");
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].severity, Some(DiagnosticSeverity::ERROR));
        assert_eq!(lines(&d[0]), (0, 0));
        assert!(d[0].message.contains("top-level"), "{}", d[0].message);
    }

    #[test]
    fn a_duplicate_id_marks_the_second_marker_line() {
        let text = DOC.replace("id=\"smoke\"", "id=\"tests\"");
        let d = diagnostics(&text);
        assert_eq!(d.len(), 1, "{d:?}");
        // The second `id="tests"` marker is on line 8 (0-based).
        assert_eq!(lines(&d[0]), (8, 8));
        assert!(d[0].message.contains("duplicate"), "{}", d[0].message);
    }

    #[test]
    fn an_unknown_attribute_is_underlined_exactly() {
        let text = DOC.replace("id=\"tests\"", "id=\"tests\" colour=\"red\"");
        let d = diagnostics(&text);
        assert_eq!(d.len(), 1, "{d:?}");
        let r = d[0].range;
        assert_eq!(r.start.line, 5);
        assert_eq!(r.end.character - r.start.character, "colour".len() as u32);
        assert!(d[0].message.contains("colour"), "{}", d[0].message);
    }

    #[test]
    fn a_bad_fence_dependency_points_at_the_node_it_names() {
        let text = format!("{DOC}\n```sh name=\"b\" deps=\"nope/x\"\ntrue\n```\n");
        let d = diagnostics(&text);
        assert_eq!(d.len(), 1, "{d:?}");
        assert_eq!(d[0].severity, Some(DiagnosticSeverity::ERROR));
    }

    #[test]
    fn symbols_nest_by_heading_level() {
        let s = symbols(DOC);
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].name, "Root");
        assert_eq!(s[0].detail.as_deref(), Some("root"));
        let kids = s[0].children.as_ref().unwrap();
        assert_eq!(kids.len(), 1);
        assert_eq!(kids[0].name, "Tests");
        let grand = kids[0].children.as_ref().unwrap();
        assert_eq!(grand[0].name, "Smoke");
        // A parent's range contains its child's.
        assert!(kids[0].range.start <= grand[0].range.start);
        assert!(kids[0].range.end >= grand[0].range.end);
    }

    #[test]
    fn symbols_still_work_on_a_document_with_duplicate_ids() {
        let text = DOC.replace("id=\"smoke\"", "id=\"tests\"");
        assert_eq!(symbols(&text).len(), 1);
    }

    fn apply(text: &str, edits: &[TextEdit]) -> String {
        let index = LineIndex::new(text);
        let mut out = text.to_string();
        let mut sorted: Vec<_> = edits.iter().collect();
        sorted.sort_by_key(|e| std::cmp::Reverse(index.offset(e.range.start)));
        for e in sorted {
            out.replace_range(
                index.offset(e.range.start)..index.offset(e.range.end),
                &e.new_text,
            );
        }
        out
    }

    fn edits_of(action: &CodeAction) -> Vec<TextEdit> {
        action
            .edit
            .as_ref()
            .unwrap()
            .changes
            .as_ref()
            .unwrap()
            .values()
            .next()
            .unwrap()
            .clone()
    }

    #[test]
    fn the_heading_under_the_cursor_becomes_a_node() {
        let cursor = lsp_types::Range::new(Position::new(10, 3), Position::new(10, 3));
        let actions = code_actions(DOC, &uri(), cursor);
        let one = &actions[0];
        assert!(one.title.contains("Plain"), "{}", one.title);
        let out = apply(DOC, &edits_of(one));
        let spans = mdcanvas::node_spans(&out);
        assert!(spans
            .iter()
            .any(|s| s.title == "Plain" && s.marker.is_some()));
        assert!(Canvas::from_markdown(&out).is_ok());
        assert!(headings_all_nodes(&out));
    }

    fn headings_all_nodes(text: &str) -> bool {
        mdcanvas::headings(text).iter().all(|h| h.is_node)
    }

    #[test]
    fn no_action_is_offered_when_every_heading_is_a_node() {
        let cursor = lsp_types::Range::new(Position::new(1, 0), Position::new(1, 0));
        let text = DOC.replace("## Plain\ntext\n", "");
        assert!(code_actions(&text, &uri(), cursor).is_empty());
    }

    #[test]
    fn the_bulk_action_numbers_every_plain_heading_and_a_last_line_without_newline() {
        let text = format!("{DOC}### Another\ntail\n#### Last");
        let cursor = lsp_types::Range::new(Position::new(0, 0), Position::new(0, 0));
        let actions = code_actions(&text, &uri(), cursor);
        // Cursor is not on a plain heading: only the bulk action.
        assert_eq!(actions.len(), 1);
        assert!(actions[0].title.contains("all 3"), "{}", actions[0].title);
        let out = apply(&text, &edits_of(&actions[0]));
        assert!(headings_all_nodes(&out), "{out}");
        assert!(Canvas::from_markdown(&out).is_ok(), "{out}");
    }

    #[test]
    fn completion_maps_utf16_columns_and_offers_a_snippet() {
        // Cursor after `<!-- m` on line 4; a multi-byte title above must not matter.
        let text = "# Корень 😀\n<!-- meshfox:node id=\"root\" -->\n\n## Child\n<!-- m\n";
        let items = completion(text, Position::new(4, 6));
        let node = items.iter().find(|i| i.label == "meshfox:node").unwrap();
        assert_eq!(node.insert_text_format, Some(InsertTextFormat::SNIPPET));
        let Some(CompletionTextEdit::Edit(edit)) = &node.text_edit else {
            panic!("{node:?}")
        };
        assert_eq!(
            edit.range,
            lsp_types::Range::new(Position::new(4, 0), Position::new(4, 6))
        );
        assert!(edit.new_text.contains("id=\"${1:"));
        assert_eq!(node.filter_text.as_deref(), Some("<!-- meshfox:node"));
    }

    #[test]
    fn completion_columns_after_wide_characters_are_utf16() {
        // `😀` is 2 UTF-16 units (1 char): with the cursor after `😀,` the
        // replaced range is empty and starts at the cursor, in UTF-16.
        let text = "# Root\n<!-- meshfox:node id=\"root\" tags=\"😀,\" -->\n## B\n<!-- meshfox:node tags=\"beta\" -->\n";
        let col = "<!-- meshfox:node id=\"root\" tags=\"😀,"
            .encode_utf16()
            .count() as u32;
        let items = completion(text, Position::new(1, col));
        assert_eq!(
            items.iter().map(|i| i.label.as_str()).collect::<Vec<_>>(),
            ["beta"]
        );
        let Some(CompletionTextEdit::Edit(edit)) = &items[0].text_edit else {
            panic!("{:?}", items[0])
        };
        assert_eq!(
            edit.range,
            lsp_types::Range::new(Position::new(1, col), Position::new(1, col))
        );
    }
}
