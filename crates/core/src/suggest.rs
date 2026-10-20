//! What a line of a canvas may say next: the attribute vocabularies of the
//! `meshfox:node` / `meshfox:edge` comments and of a runnable fence's info
//! string, which attributes are still open on a given line, and the values
//! an attribute takes. Pure functions of the text, shared by every editor
//! integration — the TUI's source editor (`Ctrl-p`) and `meshfox lsp` — so
//! they cannot drift apart. Columns are `char` columns, not bytes.

use crate::mdcanvas::{EDGE_ATTRS, NODE_ATTRS};

/// A runnable fence's own attribute vocabulary (`crates/core/src/fence.rs`)
/// — value-taking (`name="..."`) vs. bare presence flags (`cache`, no
/// `=value` at all) need different insertion shapes, see `AttrCandidate`.
pub const FENCE_VALUE_ATTRS: &[&str] = &[
    "name",
    "deps",
    "lease",
    "defer",
    "inputs",
    "outputs",
    "env",
    "interpreter",
    "output",
    "output-attrs",
    "render",
    "send",
    "argv",
];

pub const FENCE_FLAG_ATTRS: &[&str] = &[
    "cache",
    "tty",
    "autoclose",
    "service",
    "autostop",
    "always",
    "default",
    "confirm",
    "autorun",
    "fold",
    "tangle",
];

/// Which attribute vocabulary a suggestible line belongs to — see
/// `detect_attr_context`/`attr_candidates`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttrKind {
    Node,
    Edge,
    Fence,
}

impl AttrKind {
    pub fn label(self) -> &'static str {
        match self {
            AttrKind::Node => "node",
            AttrKind::Edge => "edge",
            AttrKind::Fence => "fence",
        }
    }
}

/// One entry in the `Ctrl-p` popup. `is_flag` distinguishes a bare
/// presence flag (a runnable fence's `cache`/`tty`/`default` — inserted as
/// just the bare word) from a `key="value"`-shaped attribute (every
/// `meshfox:node`/`meshfox:edge` attribute, plus a fence's own
/// `name`/`deps`/`env` — inserted as `key=""` with the cursor landing
/// between the quotes, ready to type the value).
#[derive(Debug, Clone, Copy)]
pub struct AttrCandidate {
    pub name: &'static str,
    pub is_flag: bool,
}

/// Locates a `tags="..."` attribute's value on `line`, as a char-column
/// range — `start` right after the opening quote, `end` at the closing
/// quote (so `end - start` chars sit strictly between them; `start ==
/// end` for an empty value, `tags=""`). `None` if the line has no `tags=`
/// at all, or the value's closing quote is missing. Char columns, not byte
/// offsets — matches `insertion_col`/`Index2` elsewhere in this file.
pub fn tags_value_range(line: &str) -> Option<(usize, usize)> {
    let key = "tags=\"";
    let start_byte = line.find(key)?;
    let value_start_byte = start_byte + key.len();
    let rest = &line[value_start_byte..];
    let close_byte_in_rest = rest.find('"')?;
    let start_col = line[..value_start_byte].chars().count();
    let end_col = start_col + rest[..close_byte_in_rest].chars().count();
    Some((start_col, end_col))
}

/// Ignore fence bodies (including literal meshfox comments) and closing delimiters.
pub fn document_attr_context(text: &str, row: usize) -> Option<AttrKind> {
    let mut fence: Option<(char, usize)> = None;
    for (index, line) in text.lines().enumerate() {
        let trimmed = line.trim_start();
        let ch = trimmed.chars().next().unwrap_or(' ');
        let len = trimmed.chars().take_while(|c| *c == ch).count();
        if let Some((delimiter, minimum)) = fence {
            if ch == delimiter && len >= minimum && trimmed[len..].trim().is_empty() {
                fence = None;
            }
            if index == row {
                return None;
            }
            continue;
        }
        let is_fence = line.len() - trimmed.len() < 4 && matches!(ch, '`' | '~') && len >= 3;
        if index == row {
            return if is_fence {
                let info = trimmed[len..].trim();
                let language = info.split_whitespace().next().unwrap_or("");
                let attrs = crate::attrs::parse_attrs(info);
                if matches!(language, "bash" | "sh" | "button" | "starlark")
                    || attrs.contains_key("interpreter")
                {
                    Some(AttrKind::Fence)
                } else {
                    None
                }
            } else {
                detect_attr_context(line)
            };
        }
        if is_fence {
            fence = Some((ch, len));
        }
    }
    None
}

/// Attribute values in character columns, including empty and unfinished values.
pub fn attribute_value_at(line: &str, col: usize) -> Option<(String, usize, usize)> {
    let chars: Vec<char> = line.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i].is_whitespace() {
            i += 1;
            continue;
        }
        let key_start = i;
        while i < chars.len() && !chars[i].is_whitespace() && chars[i] != '=' {
            i += 1;
        }
        if chars.get(i) != Some(&'=') {
            continue;
        }
        let name: String = chars[key_start..i].iter().collect();
        i += 1;
        let quote = chars.get(i).copied().filter(|c| matches!(c, '\'' | '"'));
        if quote.is_some() {
            i += 1;
        }
        let start = i;
        while i < chars.len()
            && match quote {
                Some(q) => chars[i] != q,
                None => !chars[i].is_whitespace(),
            }
        {
            i += 1;
        }
        if col >= start && col <= i {
            return Some((name, start, i));
        }
        if quote.is_some() && i < chars.len() {
            i += 1;
        }
    }
    None
}

pub fn finite_values(kind: AttrKind, name: &str) -> &'static [&'static str] {
    match (kind, name) {
        (AttrKind::Node, "type") => &["text", "file", "link", "group", "include"],
        (AttrKind::Node, "display") => &["link", "code", "table"],
        (AttrKind::Node, "preview" | "fold") => &["true", "false"],
        (AttrKind::Node, "edgeSourceSide" | "edgeTargetSide")
        | (AttrKind::Edge, "sourceSide" | "targetSide") => &["left", "right", "top", "bottom"],
        (AttrKind::Edge, "style") => &["solid", "dashed", "dotted"],
        (AttrKind::Edge, "arrowStart" | "arrowEnd") => &["none", "arrow"],
        (AttrKind::Fence, "output") => &["text", "markdown", "image"],
        (AttrKind::Fence, "argv") => &["false", "true"],
        (AttrKind::Fence, "render") => crate::fence::RENDER_KINDS,
        (AttrKind::Fence, name) if FENCE_FLAG_ATTRS.contains(&name) => &["true", "false"],
        _ => &[],
    }
}

pub fn inside_attribute_value(line: &str, col: usize) -> bool {
    let chars: Vec<char> = line.chars().collect();
    let mut quote = None;
    let mut value = false;
    for &c in chars.iter().take(col) {
        if let Some(q) = quote {
            if c == q {
                quote = None;
                value = false;
            }
        } else if value && matches!(c, '\'' | '"') {
            quote = Some(c);
        } else if c == '=' {
            value = true;
        } else if c.is_whitespace() {
            value = false;
        }
    }
    quote.is_some() || (value && chars.get(col).is_some_and(|c| !c.is_whitespace()))
}

/// Attribute vocabulary for a standalone structural line.
pub fn detect_attr_context(line: &str) -> Option<AttrKind> {
    let trimmed = line.trim_start();
    if trimmed.starts_with("<!-- meshfox:node") {
        Some(AttrKind::Node)
    } else if trimmed.starts_with("<!-- meshfox:edge") {
        Some(AttrKind::Edge)
    } else if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
        Some(AttrKind::Fence)
    } else {
        None
    }
}

/// Exclude parsed attribute keys and flags that conflict with the current block.
pub fn attr_candidates(kind: AttrKind, line: &str) -> Vec<AttrCandidate> {
    if kind == AttrKind::Fence
        && line
            .trim_start()
            .trim_start_matches(['`', '~'])
            .trim_start()
            .starts_with("starlark ")
    {
        let attrs = crate::attrs::parse_attrs(line);
        let mut result = vec![];
        if !attrs.contains_key("name") {
            result.push(AttrCandidate {
                name: "name",
                is_flag: false,
            });
        }
        if !attrs.contains_key("constraint") && !attrs.contains_key("render") {
            result.push(AttrCandidate {
                name: "constraint",
                is_flag: true,
            });
            result.push(AttrCandidate {
                name: "render",
                is_flag: true,
            });
        }
        if attrs.get("render").is_some_and(|v| v != "false") && !attrs.contains_key("output") {
            result.push(AttrCandidate {
                name: "output",
                is_flag: false,
            });
        }
        return result;
    }
    let (value_attrs, flag_attrs): (&[&str], &[&str]) = match kind {
        AttrKind::Node => (NODE_ATTRS, &[]),
        AttrKind::Edge => (EDGE_ATTRS, &[]),
        AttrKind::Fence => (FENCE_VALUE_ATTRS, FENCE_FLAG_ATTRS),
    };
    let attrs = crate::attrs::parse_attrs(line);
    let value_present = |name: &str| attrs.contains_key(name);
    let enabled = |name: &str| attrs.get(name).is_some_and(|v| v != "false");
    let flag_present = |name: &str| attrs.contains_key(name);
    let compatible = |name: &str| {
        kind != AttrKind::Fence
            || match name {
                "cache" => !enabled("tty") && !enabled("service"),
                "tty" => !enabled("cache") && !enabled("service"),
                "service" => {
                    !enabled("cache")
                        && !enabled("tty")
                        && !line.trim_start().starts_with("```button")
                }
                "autoclose" => enabled("tty"),
                "autostop" => {
                    enabled("service") || attrs.get("interpreter").is_some_and(|v| v == "@service")
                }
                "tangle" => value_present("interpreter"),
                _ => true,
            }
    };
    value_attrs
        .iter()
        .filter(|n| !value_present(n))
        .map(|&name| AttrCandidate {
            name,
            is_flag: false,
        })
        .chain(
            flag_attrs
                .iter()
                .filter(|n| !flag_present(n) && compatible(n))
                .map(|&name| AttrCandidate {
                    name,
                    is_flag: true,
                }),
        )
        .collect()
}

/// Where `insert_attr_candidate` types a new attribute in `line` — right
/// before a node/edge comment's closing `-->` (so the new attribute lands
/// *inside* the comment, not after it), or the plain end of line for a
/// fence's own attribute line (nothing closes it).
pub fn insertion_col(line: &str, kind: AttrKind) -> usize {
    if kind != AttrKind::Fence {
        if let Some(close) = line.rfind("-->") {
            return line[..close].trim_end().chars().count();
        }
    }
    line.chars().count()
}

// ---------------------------------------------------------------------------
// Completion at a cursor position.

use std::collections::HashSet;

/// What a value can be drawn from: the document itself.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct DocFacts {
    /// Every node id (explicit, or the slug `parse` would derive).
    pub ids: Vec<String>,
    /// Every tag used on a node or an edge.
    pub tags: Vec<String>,
    /// `(node id, block name)` for each named runnable block.
    pub blocks: Vec<(String, String)>,
    /// Names declared by `meshfox:var` comments.
    pub vars: Vec<String>,
}

impl DocFacts {
    /// Works on a half-edited document: nothing here needs `parse` to
    /// succeed.
    pub fn from_text(text: &str) -> Self {
        let mut facts = DocFacts::default();
        for span in crate::mdcanvas::node_spans(text) {
            let Some(id) = span.id.clone().or_else(|| span.explicit_id.clone()) else {
                continue;
            };
            if let Some(marker) = &span.marker {
                let attrs = crate::attrs::parse_attrs(&text[marker.clone()]);
                facts
                    .tags
                    .extend(crate::mdcanvas::parse_tags(attrs.get("tags")));
            }
            for block in crate::fence::scan_runnable_blocks(&id, &text[span.body.clone()]) {
                if let Some(name) = block.name {
                    facts.blocks.push((id.clone(), name));
                }
            }
            facts.ids.push(id);
        }
        for line in text.lines().map(str::trim_start) {
            if line.starts_with("<!-- meshfox:edge") {
                let attrs = crate::attrs::parse_attrs(line);
                facts
                    .tags
                    .extend(crate::mdcanvas::parse_tags(attrs.get("tags")));
            } else if line.starts_with("<!-- meshfox:var") {
                if let Some(name) = crate::attrs::parse_attrs(line).get("name") {
                    facts.vars.push(name.clone());
                }
            }
        }
        for list in [&mut facts.tags, &mut facts.vars] {
            list.sort();
            list.dedup();
        }
        facts
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemKind {
    Attribute,
    Flag,
    Value,
    Snippet,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    pub label: String,
    /// What replaces the range; LSP snippet syntax when `snippet`.
    pub insert: String,
    pub snippet: bool,
    pub kind: ItemKind,
    pub detail: Option<String>,
    /// What a client should match the typed text against, when that is not
    /// the label (a snippet that replaces `<!-- me…`).
    pub filter: Option<String>,
}

/// Candidates for the range `start..end` (char columns) of one line.
#[derive(Debug, Clone, PartialEq)]
pub struct Completions {
    pub start: usize,
    pub end: usize,
    pub items: Vec<Item>,
}

/// Completions for the cursor at char column `col` of line `row`: attribute
/// names and values inside a `meshfox:node`/`meshfox:edge` comment or a
/// runnable fence's info string, or a `<!-- meshfox:… -->` snippet while the
/// start of a comment is being typed.
pub fn complete(text: &str, row: usize, col: usize) -> Option<Completions> {
    let line = text.lines().nth(row)?;
    let chars: Vec<char> = line.chars().collect();
    let col = col.min(chars.len());
    match document_attr_context(text, row) {
        Some(kind) => complete_attr(text, row, line, &chars, col, kind),
        None => complete_snippet(text, row, &chars, col)
            .or_else(|| complete_plain_fence(text, row, line, &chars, col)),
    }
}

fn complete_attr(
    text: &str,
    row: usize,
    line: &str,
    chars: &[char],
    col: usize,
    kind: AttrKind,
) -> Option<Completions> {
    let before: String = chars[..col].iter().collect();
    // Past the keyword (or the fence's language), before the comment closes.
    let keyword = match kind {
        AttrKind::Node => Some("<!-- meshfox:node"),
        AttrKind::Edge => Some("<!-- meshfox:edge"),
        AttrKind::Fence => None,
    };
    let opened = match keyword {
        Some(k) => before
            .trim_start()
            .strip_prefix(k)
            .is_some_and(|rest| rest.starts_with(char::is_whitespace)),
        None => {
            let rest = before.trim_start().trim_start_matches(['`', '~']);
            rest.contains(char::is_whitespace) && !rest.trim().is_empty()
        }
    };
    if !opened {
        return None;
    }
    if kind != AttrKind::Fence {
        if let Some(close) = line.find("-->") {
            if col > line[..close].chars().count() {
                return None;
            }
        }
    }

    if let Some((name, start, end)) = attribute_value_at(line, col) {
        return complete_value(text, row, chars, col, kind, &name, (start, end));
    }
    if inside_attribute_value(line, col) {
        return None;
    }
    let (start, end) = word_range(chars, col);
    let detail = format!("{} attribute", kind.label());
    let items: Vec<Item> = attr_candidates(kind, line)
        .into_iter()
        .map(|c| {
            let (insert, snippet, item_kind) = if c.is_flag {
                (c.name.to_string(), false, ItemKind::Flag)
            } else if matches!(c.name, "x" | "y" | "w" | "h") {
                (format!("{}=0", c.name), false, ItemKind::Attribute)
            } else {
                (format!("{}=\"$1\"", c.name), true, ItemKind::Attribute)
            };
            Item {
                label: c.name.to_string(),
                insert,
                snippet,
                kind: item_kind,
                detail: Some(detail.clone()),
                filter: None,
            }
        })
        .collect();
    (!items.is_empty()).then_some(Completions { start, end, items })
}

/// The attribute name being typed at `col`: back to the previous blank,
/// forward over the rest of the word.
fn word_range(chars: &[char], col: usize) -> (usize, usize) {
    let start = (0..col)
        .rev()
        .take_while(|&i| !chars[i].is_whitespace())
        .last()
        .unwrap_or(col);
    let mut end = col;
    while end < chars.len()
        && (chars[end].is_alphanumeric()
            || chars[end] == '_'
            || (chars[end] == '-' && chars.get(end + 1).is_some_and(|c| c.is_alphanumeric())))
    {
        end += 1;
    }
    (start, end)
}

/// What `interpreter=` takes: the embedded macros and the usual programs.
/// Only the command word is completed; its arguments are the user's.
fn interpreter_completions(
    chars: &[char],
    col: usize,
    (start, end): (usize, usize),
) -> Option<Completions> {
    let word_end = (start..end)
        .find(|&i| chars[i].is_whitespace())
        .unwrap_or(end);
    if col > word_end {
        return None;
    }
    let item = |label: String, detail: &str| Item {
        insert: label.clone(),
        label,
        snippet: false,
        kind: ItemKind::Value,
        detail: Some(detail.to_string()),
        filter: None,
    };
    let items: Vec<Item> = crate::builtin_interpreter::BUILTINS
        .iter()
        .map(|b| item(format!("@{}", b.name), "built-in macro"))
        .chain(["python3", "node"].map(|p| item(p.to_string(), "program")))
        .collect();
    Some(Completions {
        start,
        end: word_end,
        items,
    })
}

/// The opening line of a fence meshfox doesn't run (`python`, `rust`, …):
/// it becomes a runnable block with `interpreter="…"`, so that is the one
/// attribute worth offering — without it nothing else on the line means
/// anything, and the full vocabulary would only be noise.
fn complete_plain_fence(
    text: &str,
    row: usize,
    line: &str,
    chars: &[char],
    col: usize,
) -> Option<Completions> {
    let trimmed = line.trim_start();
    if !trimmed.starts_with("```") {
        return None;
    }
    let offset: usize = text.split_inclusive('\n').take(row).map(str::len).sum();
    // An opener (including one whose closing fence isn't typed yet, which
    // the fence scanner doesn't report): a ``` line that is not inside
    // another fence.
    let inside_a_fence = crate::fence::fenced_byte_ranges(text)
        .iter()
        .any(|r| r.start < offset && offset < r.end);
    if inside_a_fence {
        return None;
    }
    // Past the language word.
    let before: String = chars[..col].iter().collect();
    let rest = before.trim_start().trim_start_matches('`');
    if !(rest.contains(char::is_whitespace) && !rest.trim().is_empty()) {
        return None;
    }
    if let Some((name, start, end)) = attribute_value_at(line, col) {
        return (name == "interpreter")
            .then(|| interpreter_completions(chars, col, (start, end)))
            .flatten();
    }
    if inside_attribute_value(line, col) {
        return None;
    }
    let (start, end) = word_range(chars, col);
    Some(Completions {
        start,
        end,
        items: vec![Item {
            label: "interpreter".to_string(),
            insert: "interpreter=\"$1\"".to_string(),
            snippet: true,
            kind: ItemKind::Attribute,
            detail: Some("makes this fence a runnable block".to_string()),
            filter: None,
        }],
    })
}

/// Attributes whose value is a comma-separated list.
fn is_list_attr(kind: AttrKind, name: &str) -> bool {
    matches!(
        (kind, name),
        (AttrKind::Node | AttrKind::Edge, "tags")
            | (AttrKind::Edge, "from")
            | (AttrKind::Fence, "deps" | "lease" | "defer" | "env")
    )
}

/// The comma-separated item around `col` within the value `start..end`
/// (commas inside `[...]` don't split — `deps="extract[lang=en,hy]"`),
/// leading blanks skipped.
fn list_segment(chars: &[char], col: usize, (start, end): (usize, usize)) -> (usize, usize) {
    let mut depth = 0i32;
    let mut seg_start = start;
    for (i, &c) in chars.iter().enumerate().take(col).skip(start) {
        match c {
            '[' => depth += 1,
            ']' => depth -= 1,
            ',' if depth == 0 => seg_start = i + 1,
            _ => {}
        }
    }
    let mut seg_end = end;
    depth = 0;
    for (i, &c) in chars.iter().enumerate().take(end).skip(col) {
        match c {
            '[' => depth += 1,
            ']' => depth -= 1,
            ',' if depth == 0 => {
                seg_end = i;
                break;
            }
            _ => {}
        }
    }
    while seg_start < seg_end && chars[seg_start].is_whitespace() {
        seg_start += 1;
    }
    (seg_start, seg_end)
}

fn complete_value(
    text: &str,
    row: usize,
    chars: &[char],
    col: usize,
    kind: AttrKind,
    name: &str,
    value: (usize, usize),
) -> Option<Completions> {
    if (kind, name) == (AttrKind::Fence, "interpreter") {
        return interpreter_completions(chars, col, value);
    }
    let facts = DocFacts::from_text(text);
    let current = current_node(text, row);
    let list = is_list_attr(kind, name);

    // The part of the value being edited: all of it, or for a list the
    // item between the commas around the cursor.
    let (mut seg_start, seg_end) = if list {
        list_segment(chars, col, value)
    } else {
        value
    };
    // `env` entries read `NAME`, `$NAME` or `LOCAL=$NAME`: complete the name.
    if (kind, name) == (AttrKind::Fence, "env") {
        if let Some(eq) = (seg_start..col).rev().find(|&i| chars[i] == '=') {
            seg_start = eq + 1;
        }
        if chars.get(seg_start) == Some(&'$') {
            seg_start += 1;
        }
    }
    let seg_start = seg_start.min(col);
    let seg_end = seg_end.max(col);

    // The item being typed is itself part of the buffer, so it shows up
    // among the document's tags (as `be` of `tags="be"`): never offer it
    // back, and skip the other items already in the list.
    let own: String = chars[seg_start..seg_end].iter().collect();
    let own = own.trim().to_string();
    let others: HashSet<String> = if list {
        let whole: String = chars[value.0..value.1].iter().collect();
        whole
            .split(',')
            .map(|s| s.trim().trim_end_matches('!'))
            .map(|s| s.split('[').next().unwrap_or(s).to_string())
            .filter(|s| !s.is_empty() && *s != own)
            .collect()
    } else {
        HashSet::new()
    };

    let tagged = |items: &[String], detail: &str| -> Vec<(String, Option<String>)> {
        items
            .iter()
            .map(|v| (v.clone(), Some(detail.to_string())))
            .collect()
    };
    let starlark_render = chars
        .iter()
        .collect::<String>()
        .trim_start()
        .trim_start_matches(['`', '~'])
        .trim_start()
        .starts_with("starlark ");
    let finite = if starlark_render && (kind, name) == (AttrKind::Fence, "output") {
        &["markdown"][..]
    } else {
        finite_values(kind, name)
    };
    let values: Vec<(String, Option<String>)> = if !finite.is_empty() {
        finite.iter().map(|v| (v.to_string(), None)).collect()
    } else {
        match (kind, name) {
            (AttrKind::Node, "parent") => {
                let ids: Vec<String> = facts
                    .ids
                    .iter()
                    .filter(|id| Some(id.as_str()) != current.as_deref())
                    .cloned()
                    .collect();
                tagged(&ids, "node")
            }
            (AttrKind::Edge, "from") => tagged(&facts.ids, "node"),
            (AttrKind::Node | AttrKind::Edge, "tags") => tagged(&facts.tags, "tag"),
            (AttrKind::Fence, "deps" | "lease" | "defer") => facts
                .blocks
                .iter()
                .map(|(node, block)| {
                    if Some(node.as_str()) == current.as_deref() {
                        (
                            block.clone(),
                            Some(format!("block of this node ({node}/{block})")),
                        )
                    } else {
                        (format!("{node}/{block}"), Some("block".to_string()))
                    }
                })
                .collect(),
            (AttrKind::Fence, "env") => tagged(&facts.vars, "variable"),
            _ => Vec::new(),
        }
    };
    let items: Vec<Item> = values
        .into_iter()
        .filter(|(v, _)| !others.contains(v) && !(list && *v == own))
        .map(|(v, detail)| Item {
            label: v.clone(),
            insert: v,
            snippet: false,
            kind: ItemKind::Value,
            detail,
            filter: None,
        })
        .collect();
    (!items.is_empty()).then_some(Completions {
        start: seg_start,
        end: seg_end,
        items,
    })
}

/// The id of the node whose section contains line `row`.
fn current_node(text: &str, row: usize) -> Option<String> {
    let offset: usize = text.split_inclusive('\n').take(row).map(str::len).sum();
    crate::mdcanvas::node_spans(text)
        .into_iter()
        .rfind(|s| s.heading.start <= offset && offset < s.body.end.max(s.heading.end))
        .and_then(|s| s.id.or(s.explicit_id))
}

fn complete_snippet(text: &str, row: usize, chars: &[char], col: usize) -> Option<Completions> {
    let indent = chars.iter().take_while(|c| c.is_whitespace()).count();
    if col <= indent {
        return None;
    }
    let typed: String = chars[indent..col].iter().collect();
    // Real structure only: not a line inside a fence.
    let offset: usize = text.split_inclusive('\n').take(row).map(str::len).sum();
    if crate::fence::fenced_byte_ranges(text)
        .iter()
        .any(|r| r.contains(&offset))
    {
        return None;
    }
    let used: HashSet<String> = DocFacts::from_text(text).ids.into_iter().collect();
    let id = crate::mdcanvas::random_node_id(&used);
    let candidates = [
        (
            "meshfox:node",
            "<!-- meshfox:node",
            format!("<!-- meshfox:node id=\"${{1:{id}}}\" -->$0"),
            "a node marker with a fresh id",
        ),
        (
            "meshfox:edge",
            "<!-- meshfox:edge",
            "<!-- meshfox:edge from=\"$1\" -->$0".to_string(),
            "an extra edge to this node",
        ),
    ];
    let items: Vec<Item> = candidates
        .into_iter()
        .filter(|(_, full, _, _)| full.starts_with(&typed))
        .map(|(label, full, insert, detail)| Item {
            label: label.to_string(),
            insert,
            snippet: true,
            kind: ItemKind::Snippet,
            detail: Some(detail.to_string()),
            filter: Some(full.to_string()),
        })
        .collect();
    (!items.is_empty()).then_some(Completions {
        start: indent,
        end: col,
        items,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOC: &str = "# Root\n<!-- meshfox:node id=\"root\" tags=\"alpha,beta\" -->\n\n```bash name=\"build\"\ntrue\n```\n\n## Child\n<!-- meshfox:node id=\"child\" parent=\"root\" -->\n\n```bash name=\"test\" deps=\"\"\ntrue\n```\n";

    /// Completions at the `|` in `with_cursor`.
    fn at(with_cursor: &str) -> Option<Completions> {
        let offset = with_cursor.find('|').unwrap();
        let text = with_cursor.replacen('|', "", 1);
        let row = text[..offset].matches('\n').count();
        let col = text[..offset].rsplit('\n').next().unwrap().chars().count();
        complete(&text, row, col)
    }

    fn labels(c: &Completions) -> Vec<&str> {
        c.items.iter().map(|i| i.label.as_str()).collect()
    }

    #[test]
    fn doc_facts_collect_ids_tags_blocks_and_vars() {
        let text = format!(
            "{DOC}<!-- meshfox:var name=\"REGION\" default=\"x\" -->\n<!-- meshfox:edge from=\"root\" tags=\"gamma\" -->\n"
        );
        let f = DocFacts::from_text(&text);
        assert_eq!(f.ids, ["root", "child"]);
        assert_eq!(f.tags, ["alpha", "beta", "gamma"]);
        assert_eq!(
            f.blocks,
            [
                ("root".to_string(), "build".to_string()),
                ("child".to_string(), "test".to_string())
            ]
        );
        assert_eq!(f.vars, ["REGION"]);
    }

    #[test]
    fn attribute_names_exclude_the_ones_already_there_and_replace_the_typed_word() {
        let c = at("# Root\n<!-- meshfox:node id=\"root\" co| -->\n").unwrap();
        let names = labels(&c);
        assert!(names.contains(&"color") && names.contains(&"tags"));
        assert!(!names.contains(&"id"));
        // The typed "co" is replaced.
        assert_eq!((c.start, c.end), (28, 30));
        let color = c.items.iter().find(|i| i.label == "color").unwrap();
        assert_eq!(color.insert, "color=\"$1\"");
        assert!(color.snippet);
        let x = c.items.iter().find(|i| i.label == "x").unwrap();
        assert_eq!((x.insert.as_str(), x.snippet), ("x=0", false));
    }

    #[test]
    fn nothing_is_offered_before_the_keyword_or_after_the_comment_closes() {
        assert!(at("# Root\n<!-- meshfox:node| id=\"root\" -->\n").is_none());
        assert!(at("# Root\n<!-- meshfox:node id=\"root\" -->|\n").is_none());
    }

    #[test]
    fn a_fence_offers_flags_and_values_only_past_its_language() {
        let c = at("```bash |\ntrue\n```\n").unwrap();
        let flag = c.items.iter().find(|i| i.label == "cache").unwrap();
        assert_eq!((flag.insert.as_str(), flag.kind), ("cache", ItemKind::Flag));
        assert!(at("```ba|sh\ntrue\n```\n").is_none());
        // A fence meshfox doesn't run offers just the way to make it run.
        let c = at("```rust |\nfn main() {}\n```\n").unwrap();
        assert_eq!(labels(&c), ["interpreter"]);
        assert!(at("```rust|\nfn main() {}\n```\n").is_none());
    }

    #[test]
    fn enumerated_values_and_ids_complete_inside_the_quotes() {
        let c = at("# Root\n<!-- meshfox:node id=\"root\" type=\"|\" -->\n").unwrap();
        assert_eq!(labels(&c), ["text", "file", "link", "group", "include"]);
        let text = format!("{DOC}## Third\n<!-- meshfox:node id=\"third\" parent=\"r|\" -->\n");
        let c = at(&text).unwrap();
        // The node itself is not offered as its own parent.
        assert_eq!(labels(&c), ["root", "child"]);
        assert_eq!(c.end - c.start, 1);
    }

    #[test]
    fn tags_complete_the_item_under_the_cursor_and_skip_the_others() {
        let text = DOC.replacen("parent=\"root\"", "tags=\"alpha,|\"", 1);
        let c = at(&text).unwrap();
        assert_eq!(labels(&c), ["beta"]);
        assert_eq!(c.start, c.end);
        let text = DOC.replacen("parent=\"root\"", "tags=\"be|,alpha\"", 1);
        let c = at(&text).unwrap();
        assert_eq!(labels(&c), ["beta"]);
        assert_eq!(c.end - c.start, 2);
    }

    #[test]
    fn deps_offer_blocks_bare_in_this_node_and_qualified_elsewhere() {
        let text = DOC.replacen("deps=\"\"", "deps=\"|\"", 1);
        let c = at(&text).unwrap();
        assert_eq!(labels(&c), ["root/build", "test"]);
        let text = DOC.replacen("deps=\"\"", "deps=\"root/build,|\"", 1);
        assert_eq!(labels(&at(&text).unwrap()), ["test"]);
    }

    #[test]
    fn env_offers_declared_variables_after_a_dollar_or_an_equals() {
        let base = format!("{DOC}<!-- meshfox:var name=\"REGION\" default=\"x\" -->\n");
        let with_env = |env: &str| base.replacen("deps=\"\"", &format!("env=\"{env}\""), 1);
        let c = at(&with_env("A,$|")).unwrap();
        assert_eq!(labels(&c), ["REGION"]);
        assert_eq!(c.end - c.start, 0);
        let c = at(&with_env("LOCAL=$RE|")).unwrap();
        assert_eq!(labels(&c), ["REGION"]);
        assert_eq!(c.end - c.start, 2);
    }

    #[test]
    fn typing_the_start_of_a_marker_offers_a_snippet_with_a_fresh_id() {
        let c = at("# Root\n<!-- meshfox:node id=\"root\" -->\n\n## Child\n<!-- m|\n").unwrap();
        let node = c.items.iter().find(|i| i.label == "meshfox:node").unwrap();
        assert!(node.snippet && node.kind == ItemKind::Snippet);
        assert!(
            node.insert.starts_with("<!-- meshfox:node id=\"${1:"),
            "{}",
            node.insert
        );
        assert!(node.insert.ends_with("}\" -->$0"));
        assert_eq!(node.filter.as_deref(), Some("<!-- meshfox:node"));
        assert_eq!((c.start, c.end), (0, 6));
        // `<!-- meshfox:n` narrows it to the node marker.
        let c = at("# Root\n<!-- meshfox:n|\n").unwrap();
        assert_eq!(labels(&c), ["meshfox:node"]);
    }

    #[test]
    fn no_snippet_on_a_blank_line_in_prose_or_inside_a_fence() {
        assert!(at("# Root\n|\n").is_none());
        assert!(at("# Root\nsome text|\n").is_none());
        assert!(at("# Root\n```text\n<!-- m|\n```\n").is_none());
    }

    #[test]
    fn a_python_fence_is_offered_interpreter_and_then_its_values() {
        let c = at("```python name=\"test-py\" |\nprint(1)\n```\n").unwrap();
        assert_eq!(labels(&c), ["interpreter"]);
        assert_eq!(c.items[0].insert, "interpreter=\"$1\"");
        // The value: the macros first, then programs; only the command word.
        let c = at("```python interpreter=\"|\"\nprint(1)\n```\n").unwrap();
        assert_eq!(
            labels(&c),
            ["@agent", "@python_venv", "@node_env", "python3", "node"]
        );
        assert!(at("```python interpreter=\"python3 -|\"\nprint(1)\n```\n").is_none());
        // With `interpreter=` the fence is runnable: the full vocabulary.
        let c = at("```python interpreter=\"python3\" |\nprint(1)\n```\n").unwrap();
        assert!(labels(&c).contains(&"cache") && labels(&c).contains(&"deps"));
        // Not a fence opener: nothing.
        assert!(at("```python name=\"a\"\nprint(1) |\n```\n").is_none());
        // Still being typed, no closing fence yet.
        let c = at("# Root\n```python name=\"a\" |").unwrap();
        assert_eq!(labels(&c), ["interpreter"]);
        // A ``` line inside another fence is content, not an opener.
        assert!(at("````md\n```python name=\"a\" |\n```\n````\n").is_none());
    }
}

#[cfg(test)]
mod starlark_render_tests {
    use super::*;
    #[test]
    fn starlark_has_its_own_fence_vocabulary() {
        let candidates = attr_candidates(AttrKind::Fence, "```starlark ");
        assert!(candidates.iter().any(|c| c.name == "render" && c.is_flag));
        assert!(candidates
            .iter()
            .any(|c| c.name == "constraint" && c.is_flag));
        assert!(!candidates.iter().any(|c| c.name == "interpreter"));
        let candidates = attr_candidates(AttrKind::Fence, "```starlark render ");
        assert!(candidates.iter().any(|c| c.name == "output"));
        assert!(!candidates.iter().any(|c| c.name == "constraint"));
        let line = "```starlark render output=\"\"";
        let output = complete(line, 0, line.chars().count() - 1).unwrap();
        assert_eq!(
            output
                .items
                .iter()
                .map(|i| i.label.as_str())
                .collect::<Vec<_>>(),
            ["markdown"]
        );
        assert!(complete("```starlark ", 0, 12)
            .unwrap()
            .items
            .iter()
            .any(|c| c.label == "render"));
    }
}
