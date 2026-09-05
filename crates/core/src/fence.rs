//! Scanning Markdown for meshfox's runnable-code-fence convention.
//!
//! A fenced code block is runnable if its info string carries a `name`
//! attribute: `` ```bash name="build" cache `` — see README.md for the
//! full convention.

use std::collections::HashMap;
use std::ops::Range;

#[derive(Debug, Clone, PartialEq)]
pub struct CodeBlock {
    pub lang: String,
    pub name: Option<String>,
    pub cache: bool,
    /// Explicit `default` flag (`default` or `default=true`) — see
    /// `is_default`/`default_block`. A block whose `name` already equals
    /// its owning node's own id counts as default too, without needing
    /// this flag set — this field only tracks the *explicit* marker.
    pub default: bool,
    /// Explicit `tty` flag (`tty` or `tty="true"`) — this block wants a
    /// real interactive terminal instead of captured/streamed output. See
    /// SPEC.md's "Runnable code fences": mutually exclusive with `cache`,
    /// and only allowed as a `deps=` target of another `tty` block — both
    /// enforced by `crate::deps::validate`, not here.
    pub tty: bool,
    /// Explicit `autoclose` flag (`autoclose` or `autoclose="true"`) —
    /// only meaningful on a `tty` block (`crate::deps::validate` rejects it
    /// otherwise): once the interactive process exits, hand control back to
    /// the canvas immediately, rather than the default of leaving its exit
    /// code (and whatever it last printed) on screen until a deliberate
    /// keypress. See SPEC.md's "Interactive (`tty`) blocks".
    pub autoclose: bool,
    /// Explicit `always` flag (`always` or `always="true"`) — opts this
    /// block out of the webui/TUI session-freshness skip entirely (see
    /// `AppState::session_runs`/`App::session_runs`): even when it's
    /// unchanged and already ran successfully earlier in the same
    /// long-lived session, a "⛓ run chain" that pulls it in as a
    /// dependency still runs it for real every time, same as the block
    /// actually requested always does. For a step whose side effect isn't
    /// captured by "looks unchanged" — a migration that always drops and
    /// recreates a table, say, where re-running is the whole point even
    /// though the migration script itself never changes. Meaningless (but
    /// harmless) on a block nothing ever reaches as a *pulled-in*
    /// dependency — the block actually requested already always runs for
    /// real regardless. See SPEC.md's "Runnable code fences".
    pub always: bool,
    /// Other blocks this one depends on (`deps="a,b"`) — run before this
    /// one, automatically, whenever this block runs. See `crate::deps`.
    pub deps: Vec<BlockRef>,
    /// Declared `meshfox:var`s this block wants in its own process
    /// environment (`env="$VAR,LOCAL=$OTHER"`) — see `crate::vars`. A
    /// block with no `env=` gets none of the document's declared
    /// variables injected at all (though it still inherits the ambient
    /// process environment normally, same as any child process); only
    /// variables named here are ever resolved/prompted for on its behalf.
    pub env: Vec<EnvRef>,
    /// `interpreter="..."` — a shebang-style command+flags string
    /// (`interpreter="python3 -u"`) to run this fence's code under instead
    /// of the implicit `bash`/`sh` executor. When set, `lang` no longer
    /// has to be `bash`/`sh` for the fence to count as runnable at all —
    /// see `candidate_fences`. See `crate::exec::split_interpreter` for how
    /// this string is parsed into a program + argument list.
    pub interpreter: Option<String>,
    pub attrs: HashMap<String, String>,
    pub code: String,
    /// Byte range of the whole fence (opening ` ``` ` line through the
    /// closing ` ``` ` line, inclusive) within the source Markdown.
    pub span: Range<usize>,
}

/// True if `block` is its node's "default" block — the one `meshfox run
/// <path-to-node>` (with no trailing block name) addresses. A block
/// qualifies either by the explicit `default` flag, or because its `name`
/// already equals `node_id` (implicitly, via the sole unnamed fence, or
/// explicitly via `name="<node-id>"`) — see SPEC.md's "Runnable code
/// fences" and "CLI".
pub fn is_default(block: &CodeBlock, node_id: &str) -> bool {
    block.default || block.name.as_deref() == Some(node_id)
}

/// The one block in `blocks` (all belonging to the same node `node_id`)
/// that qualifies as default, if any — see `is_default`. `Ok(None)` if no
/// block qualifies. A node may have at most one default block: `Err` lists
/// the names of every block that qualifies when more than one does, so
/// callers can report the conflict (`meshfox validate`) or, for best-effort
/// resolution (`meshfox run`'s node-id shortcut), treat it the same as "no
/// default available" — same convention as any other ambiguous case here
/// (e.g. multiple unnamed fences).
pub fn default_block<'a>(
    node_id: &str,
    blocks: &'a [CodeBlock],
) -> Result<Option<&'a CodeBlock>, Vec<String>> {
    let defaults: Vec<&CodeBlock> = blocks.iter().filter(|b| is_default(b, node_id)).collect();
    match defaults.len() {
        0 => Ok(None),
        1 => Ok(Some(defaults[0])),
        _ => Err(defaults
            .iter()
            .map(|b| b.name.clone().unwrap_or_default())
            .collect()),
    }
}

/// One entry of a fence's `deps=` list: either a bare block name (a block
/// in the same node as the one declaring the dependency) or
/// `node-id/block-name` (a block in another node, addressed the same way
/// `meshfox:edge from=` addresses nodes). A trailing `!` (e.g.
/// `deps="build,schema/migrate!"`) sets `sync` — see its own doc comment.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BlockRef {
    pub node_id: Option<String>,
    pub block_name: String,
    /// Trailing `!` on this `deps=` entry: ties this dependency's own
    /// session-freshness decision to the block declaring the edge, instead
    /// of its own fingerprint. See `crate::deps::compute_forced_reruns`
    /// for the full semantics/rationale — in short, whenever the
    /// declaring block actually runs for real this pass (not skipped as
    /// "already fresh"), this dependency is forced to run for real too,
    /// regardless of its own fingerprint or `always` flag; when the
    /// declaring block is itself skipped, this dependency is left to its
    /// own normal freshness decision. For a setup step whose side effect
    /// (unlike `always`) should only recur alongside one specific
    /// consumer's own rerun — e.g. a migration that must drop/recreate a
    /// schema exactly when (and only when) the load step that repopulates
    /// it is about to run for real, not on every pulled-in reference.
    pub sync: bool,
}

pub(crate) fn parse_block_ref(s: &str) -> BlockRef {
    let (s, sync) = match s.strip_suffix('!') {
        Some(rest) => (rest, true),
        None => (s, false),
    };
    match s.split_once('/') {
        Some((node_id, block_name)) => BlockRef {
            node_id: Some(node_id.to_string()),
            block_name: block_name.to_string(),
            sync,
        },
        None => BlockRef {
            node_id: None,
            block_name: s.to_string(),
            sync,
        },
    }
}

fn parse_deps(attrs: &HashMap<String, String>) -> Vec<BlockRef> {
    attrs
        .get("deps")
        .map(|v| {
            v.split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(parse_block_ref)
                .collect()
        })
        .unwrap_or_default()
}

/// One entry of a fence's `env=` list: which declared `meshfox:var` to
/// pull a value from (`var_name`), and what to call it in this block's own
/// process environment (`local_name`) — see `crate::vars`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct EnvRef {
    pub local_name: String,
    pub var_name: String,
}

/// A leading `$` is purely cosmetic — `env="$VAR"` and `env="VAR"` mean the
/// same thing, as do `env="NAME=$VAR"` and `env="NAME=VAR"`. Optional
/// rather than required so there's no new "malformed attribute" parse
/// failure to invent here: a typo'd variable name reads the same either
/// way, and is caught by `crate::vars::validate_env_refs` (via `meshfox
/// check`) — the same place a `deps=` reference to a block that doesn't
/// exist is already caught, not by the fence grammar itself.
fn strip_dollar(s: &str) -> &str {
    s.strip_prefix('$').unwrap_or(s)
}

fn parse_env_ref(s: &str) -> EnvRef {
    match s.split_once('=') {
        Some((local, value)) => EnvRef {
            local_name: local.trim().to_string(),
            var_name: strip_dollar(value.trim()).to_string(),
        },
        None => {
            let var_name = strip_dollar(s.trim()).to_string();
            EnvRef {
                local_name: var_name.clone(),
                var_name,
            }
        }
    }
}

fn parse_env(attrs: &HashMap<String, String>) -> Vec<EnvRef> {
    attrs
        .get("env")
        .map(|v| {
            v.split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(parse_env_ref)
                .collect()
        })
        .unwrap_or_default()
}

/// Same splitting/trimming `parse_deps` does, but straight from a raw
/// `deps=`-shaped string rather than a whole fence's parsed attrs map —
/// what a caller building one from scratch (`node block --deps`) needs,
/// since it has no fence to have parsed one out of yet.
pub fn parse_deps_list(s: &str) -> Vec<BlockRef> {
    s.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(parse_block_ref)
        .collect()
}

/// Same splitting/trimming `parse_env` does, but straight from a raw
/// `env=`-shaped string — see `parse_deps_list`'s own doc comment.
pub fn parse_env_list(s: &str) -> Vec<EnvRef> {
    s.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(parse_env_ref)
        .collect()
}

/// A top-level fenced block, found the way CommonMark actually specifies:
/// an opening run of 3+ identical `` ` `` or `~` characters, closed only by
/// a line consisting of a run of the *same* character that is *at least as
/// long*. A shorter (or differently-charactered) run of the fence character
/// appearing inside — e.g. a demonstration snippet, or a command's captured
/// stdout that happens to contain some backticks — is just content, not a
/// close, exactly as it would be for any other Markdown renderer. This is
/// what makes nesting (`` ```` `` around `` ``` ``) and arbitrary cached
/// command output safe to embed.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RawFence {
    pub delim_char: char,
    pub delim_len: usize,
    pub info: String,
    pub code: String,
    pub span: Range<usize>,
}

fn fence_open(line: &str) -> Option<(char, usize, &str)> {
    // CommonMark: 4+ spaces of indentation makes a line part of an
    // *indented* code block, not a fence — this is how SPEC.md "escapes"
    // its own illustrative fence examples (see e.g. "Constraint fences",
    // "Runnable code fences") so they read as inert documentation rather
    // than being picked up as real runnable/constraint blocks, including
    // once spliced into another document via `include` (a plain-Markdown
    // include target's body is scanned exactly like any other node's).
    let indent = line.len() - line.trim_start_matches(' ').len();
    if indent >= 4 {
        return None;
    }
    let trimmed = line.trim_start();
    let ch = trimmed.chars().next()?;
    if ch != '`' && ch != '~' {
        return None;
    }
    let len = trimmed.chars().take_while(|&c| c == ch).count();
    if len < 3 {
        return None;
    }
    let rest = &trimmed[len..];
    // Backtick fences can't have a backtick in the info string — that's how
    // CommonMark tells `` `x` `` (inline code) apart from a fence.
    if ch == '`' && rest.contains('`') {
        return None;
    }
    Some((ch, len, rest.trim()))
}

fn fence_close(line: &str, ch: char, min_len: usize) -> bool {
    let trimmed = line.trim();
    !trimmed.is_empty() && trimmed.chars().all(|c| c == ch) && trimmed.chars().count() >= min_len
}

/// All top-level fences in `markdown`, in document order. Content already
/// consumed by one fence is never rescanned, so fences don't nest in the
/// output — matching CommonMark, where what looks like a fence marker
/// inside another fence's content is inert unless it actually closes it.
pub(crate) fn scan_raw_fences(markdown: &str) -> Vec<RawFence> {
    let lines = lines_with_offsets(markdown);
    let mut fences = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let (start_off, line) = lines[i];
        if let Some((ch, len, info)) = fence_open(line) {
            let mut j = i + 1;
            let mut code_lines: Vec<&str> = Vec::new();
            let mut end_off = None;
            while j < lines.len() {
                let (off, l) = lines[j];
                if fence_close(l, ch, len) {
                    end_off = Some(off + l.len());
                    break;
                }
                code_lines.push(l);
                j += 1;
            }
            match end_off {
                Some(end_off) => {
                    fences.push(RawFence {
                        delim_char: ch,
                        delim_len: len,
                        info: info.to_string(),
                        code: code_lines.join("\n"),
                        span: start_off..end_off,
                    });
                    i = j + 1;
                }
                // Unclosed fence: per CommonMark it runs to the end of the
                // document, so there's nothing left outside it to scan.
                None => break,
            }
            continue;
        }
        i += 1;
    }
    fences
}

/// Byte ranges covered by top-level fences (opening delimiter line through
/// closing delimiter line, inclusive), in document order. Anything a
/// heading/structure scanner finds starting inside one of these ranges is
/// fence content, not real document structure.
pub(crate) fn fenced_byte_ranges(markdown: &str) -> Vec<Range<usize>> {
    scan_raw_fences(markdown)
        .into_iter()
        .map(|f| f.span)
        .collect()
}

/// All runnable code blocks (backtick fences with a `name` attribute) in
/// `markdown`, in document order. Fences with no `name` are not included —
/// use `scan_runnable_blocks` where a node's own id is available to also
/// pick up its (at most one) implicitly-named fence.
pub fn scan_code_blocks(markdown: &str) -> Vec<CodeBlock> {
    candidate_fences(markdown)
        .into_iter()
        .filter_map(|(f, lang, attrs)| {
            let name = attrs.get("name").cloned()?;
            Some(build_code_block(f, lang, attrs, name))
        })
        .collect()
}

/// Same as `scan_code_blocks`, but a node may have *one* fence with no
/// `name` at all — it's runnable too, implicitly named after `node_id`
/// (see SPEC.md's "Runnable code fences": this is what lets `meshfox
/// run`/`list` skip a redundant trailing block-name argument when a node
/// is really just "one thing to run"). If a node has *more than one*
/// unnamed fence, none of them get a name — genuinely ambiguous which one
/// would be "the" implicit block, so they're left non-runnable, same as
/// today's behavior for any unnamed fence.
pub fn scan_runnable_blocks(node_id: &str, markdown: &str) -> Vec<CodeBlock> {
    let candidates = candidate_fences(markdown);
    let unnamed_count = candidates
        .iter()
        .filter(|(_, _, attrs)| !attrs.contains_key("name"))
        .count();
    let solo_unnamed = unnamed_count == 1;

    candidates
        .into_iter()
        .filter_map(|(f, lang, attrs)| {
            let name = attrs
                .get("name")
                .cloned()
                .or_else(|| solo_unnamed.then(|| node_id.to_string()))?;
            Some(build_code_block(f, lang, attrs, name))
        })
        .collect()
}

/// Backtick fences with a non-empty info string, and either a language
/// `crate::exec` already knows how to run (`bash`/`sh`) or its own
/// `interpreter=` attribute naming one explicitly — the universe of fences
/// that *could* be runnable, named or not — parsed once so both scan
/// functions above share the work. Excluding a fence that's neither here
/// (not just downstream at execution time) means it never counts as
/// runnable at all, named or not — including for the "sole unnamed fence"
/// implicit-naming rule in `scan_runnable_blocks`, which is what keeps an
/// ordinary Markdown document's own example fences (a `yaml` config
/// sample, a `json` snippet, ...) from being mistaken for "the" runnable
/// block in a node that has no real meshfox structure of its own. A
/// cached-output block's own fence (always unnamed — see
/// `crate::output::render_output_block`, a plain ` ```text `) is excluded
/// here too, on top of `text` already not being a supported language —
/// belt and suspenders against ever growing a spurious implicit block out
/// of a node's own cached output.
fn candidate_fences(markdown: &str) -> Vec<(RawFence, String, HashMap<String, String>)> {
    let output_ranges = crate::output::output_byte_ranges(markdown);
    scan_raw_fences(markdown)
        .into_iter()
        .filter(|f| {
            f.delim_char == '`' && !f.info.is_empty() && !in_output_region(&output_ranges, f.span.start)
        })
        .map(|f| {
            let (lang, attrs) = parse_info_string(&f.info);
            (f, lang, attrs)
        })
        .filter(|(_, lang, attrs)| {
            crate::exec::is_supported_lang(lang) || attrs.contains_key("interpreter")
        })
        .collect()
}

/// True if `pos` falls inside one of `ranges` (each from
/// `crate::output::output_byte_ranges`) — i.e. inside a `<!--
/// meshfox:output ... --> ... <!-- /meshfox:output -->` region. A fence in
/// there is always cached output, never real source: the default
/// text-mode rendering wraps it in exactly one such fence right after the
/// marker line (`output::render_output_block`), and `output="markdown"`
/// mode can additionally splice a command's own raw stdout into the same
/// region, which could otherwise contain what *looks* like a real
/// `name=`/`cache` runnable fence (or a `starlark constraint` one, see
/// `scan_constraint_blocks`) forged by whatever the command printed —
/// belt and suspenders against ever growing a spurious/adversarial block
/// out of a node's own cached output. Also used, `pub(crate)`, by every
/// other root-only comment scanner (`comment::strip`,
/// `options::scan_option_decls`/`unknown_option_attr`,
/// `tag_colors::scan_tag_color_decls`/`unknown_tag_color_attr`,
/// `vars::scan_var_decls`/`unknown_var_attr`) for the same reason — see
/// TODO.canvas.md: "Другие fence-aware сканеры не знают про
/// meshfox:output-регионы".
pub(crate) fn in_output_region(ranges: &[Range<usize>], pos: usize) -> bool {
    ranges.iter().any(|r| r.start <= pos && pos < r.end)
}

/// Every attribute name `build_code_block` actually reads off a runnable
/// fence's own info string — the vocabulary `unknown_fence_attr` (below,
/// `meshfox validate`-only, see `attrs::UnknownAttrError`'s own doc
/// comment) diffs a fence's own attribute keys against. Also what
/// `crates/cli/src/tui/source_editor.rs`'s `Ctrl-p` popup mirrors as
/// `FENCE_VALUE_ATTRS`/`FENCE_FLAG_ATTRS` (split there only because the
/// popup needs to know which shape to insert — irrelevant here, where
/// every key is just a key).
const FENCE_ATTRS: &[&str] = &[
    "name",
    "deps",
    "env",
    "cache",
    "tty",
    "autoclose",
    "always",
    "default",
    "interpreter",
    "output",
];

/// `meshfox validate`-only: the first runnable fence anywhere in
/// `markdown` with an attribute not in `FENCE_ATTRS` — checked over every
/// *candidate* fence (`candidate_fences`, before `scan_code_blocks`'s own
/// `name=`-required filter), so a typo'd attribute on an otherwise-unnamed
/// fence still gets caught.
pub fn unknown_fence_attr(markdown: &str) -> Option<crate::attrs::UnknownAttrError> {
    for (_, _, attrs) in candidate_fences(markdown) {
        if let Some(attr) = crate::attrs::first_unknown(&attrs, FENCE_ATTRS) {
            let label = attrs.get("name").cloned().unwrap_or_else(|| "<unnamed>".to_string());
            return Some(crate::attrs::UnknownAttrError {
                context: format!("the runnable fence {label:?}"),
                attr: attr.to_string(),
            });
        }
    }
    None
}

fn build_code_block(
    f: RawFence,
    lang: String,
    attrs: HashMap<String, String>,
    name: String,
) -> CodeBlock {
    let cache = attrs.get("cache").map(|v| v != "false").unwrap_or(false);
    let default = attrs.get("default").map(|v| v != "false").unwrap_or(false);
    let tty = attrs.get("tty").map(|v| v != "false").unwrap_or(false);
    let autoclose = attrs.get("autoclose").map(|v| v != "false").unwrap_or(false);
    let always = attrs.get("always").map(|v| v != "false").unwrap_or(false);
    let deps = parse_deps(&attrs);
    let env = parse_env(&attrs);
    let interpreter = attrs.get("interpreter").cloned();
    CodeBlock {
        lang,
        name: Some(name),
        cache,
        default,
        tty,
        autoclose,
        always,
        deps,
        env,
        interpreter,
        attrs,
        code: f.code,
        span: f.span,
    }
}

/// One ` ```starlark constraint ` fence found by `scan_constraint_blocks` —
/// a Starlark contract embedded in a node's body (see `crate::constraint`).
#[derive(Debug, Clone, PartialEq)]
pub struct ConstraintBlock {
    /// Explicit `name="..."` attribute, if given — see
    /// `crate::constraint::evaluate` for how a block without one gets a
    /// default label.
    pub name: Option<String>,
    pub code: String,
    pub span: Range<usize>,
}

/// Every ` ```starlark constraint ` fence in `markdown`, in document order
/// — a node's embedded Starlark contracts (see `crate::constraint`). A
/// plain ` ```starlark ` fence without the bare `constraint` flag is left
/// alone (e.g. a documentation example showing Starlark syntax), the same
/// way an unnamed `bash` fence is left alone by `scan_code_blocks` unless
/// it opts in with `name=`.
pub fn scan_constraint_blocks(markdown: &str) -> Vec<ConstraintBlock> {
    let output_ranges = crate::output::output_byte_ranges(markdown);
    scan_raw_fences(markdown)
        .into_iter()
        .filter_map(|f| {
            if in_output_region(&output_ranges, f.span.start) {
                return None;
            }
            let (lang, attrs) = parse_info_string(&f.info);
            if lang != "starlark" {
                return None;
            }
            let is_constraint = attrs
                .get("constraint")
                .map(|v| v != "false")
                .unwrap_or(false);
            if !is_constraint {
                return None;
            }
            Some(ConstraintBlock {
                name: attrs.get("name").cloned(),
                code: f.code.clone(),
                span: f.span.clone(),
            })
        })
        .collect()
}

/// Rewrites every top-level fenced code block's info string down to just its
/// bare language token, dropping meshfox's own attributes (`name=`,
/// `cache`, `deps=`, ...) — for rendering a node's body as plain HTML (see
/// `crate::staticgen`), where those attributes would otherwise leak into the
/// rendered `class="language-..."`. Content outside fences, and each
/// fence's own code, is left byte-for-byte untouched; only the opening
/// delimiter line is rewritten.
pub fn strip_fence_attrs(markdown: &str) -> String {
    let fences = scan_raw_fences(markdown);
    if fences.is_empty() {
        return markdown.to_string();
    }
    let mut out = String::with_capacity(markdown.len());
    let mut cursor = 0;
    for fence in &fences {
        out.push_str(&markdown[cursor..fence.span.start]);
        let (lang, _attrs) = parse_info_string(&fence.info);
        let delim: String = std::iter::repeat_n(fence.delim_char, fence.delim_len)
            .collect();
        out.push_str(&delim);
        out.push_str(&lang);
        out.push('\n');
        out.push_str(&fence.code);
        out.push('\n');
        out.push_str(&delim);
        cursor = fence.span.end;
    }
    out.push_str(&markdown[cursor..]);
    out
}

fn lines_with_offsets(s: &str) -> Vec<(usize, &str)> {
    let mut result = Vec::new();
    let mut offset = 0;
    for line in s.split('\n') {
        result.push((offset, line));
        offset += line.len() + 1;
    }
    result
}

const FNV1A_OFFSET: u32 = 0x811c_9dc5;
const FNV1A_PRIME: u32 = 0x0100_0193;

fn fnv1a(bytes: &[u8]) -> u32 {
    let mut hash = FNV1A_OFFSET;
    for &b in bytes {
        hash ^= b as u32;
        hash = hash.wrapping_mul(FNV1A_PRIME);
    }
    hash
}

/// A short, non-cryptographic fingerprint of everything about a runnable
/// fence that actually changes what running it does — its own code, lang,
/// interpreter, and its `env=`/`deps=` *references* (by name, not a
/// resolved value: a variable's value isn't known at parse time, and it
/// changing elsewhere shouldn't by itself mark this fence stale). Two
/// scans of the same fence (unchanged since) always agree; two different
/// fences *usually* don't (a 32-bit hash isn't collision-free, but a
/// collision here only ever costs an unnecessary rerun, never a wrongly
/// skipped one — nothing safety-critical depends on this being unique).
///
/// FNV-1a over UTF-8 bytes specifically because it's small enough to
/// hand-port byte-for-byte into the web UI's own TypeScript mirror
/// (`web/src/fence.ts`'s `fingerprint`) without either side pulling in a
/// hashing crate/package just for this — see `crate::output` (where this
/// is embedded in a cached-output marker to detect "the fence changed
/// since this ran") and `crate::deps` (where a session keeps its own
/// already-ran-this-fingerprint bookkeeping to skip an unchanged
/// dependency). Keep `web/src/fence.ts`'s `fingerprint` byte-for-byte in
/// sync by hand if this ever changes — there's no way to share the
/// implementation across the Rust/TS boundary, same tradeoff every other
/// two-sided grammar mirror in this codebase already accepts (see SPEC.md's
/// "Formal grammar" intro).
pub fn fingerprint(block: &CodeBlock) -> String {
    let mut parts: Vec<String> = vec![
        block.lang.clone(),
        block.code.clone(),
        block.interpreter.clone().unwrap_or_default(),
    ];
    for env in &block.env {
        parts.push(format!("{}\u{0}{}", env.local_name, env.var_name));
    }
    for dep in &block.deps {
        let mut part = match &dep.node_id {
            Some(node_id) => format!("{node_id}/{}", dep.block_name),
            None => dep.block_name.clone(),
        };
        if dep.sync {
            part.push('!');
        }
        parts.push(part);
    }
    format!("{:08x}", fnv1a(parts.join("\u{0}").as_bytes()))
}

/// `fingerprint` plus the *resolved values* of the variables this block
/// actually references — only its own `env=` list and any `$NAME` its
/// `interpreter=` refers to (`crate::exec::interpreter_var_refs`), same
/// scoping `resolve_block_env` already applies. Session-freshness
/// bookkeeping (`AppState::session_runs`/TUI's `App::session_runs`, see
/// TODO.canvas.md: "Переменные как часть состояния блока") uses this
/// instead of plain `fingerprint` so a variable's *value* changing (not
/// just which variables the block declares) invalidates the skip too — a
/// re-answered `meshfox:var` prompt, a changed `--set`/env override, or an
/// upstream `from=` block now producing something different.
///
/// Deliberately not part of `fingerprint` itself: that one also backs the
/// on-disk `<!-- meshfox:output ... hash="..." -->` cache
/// (`crate::output`), which has no notion of a "currently resolved"
/// variable value to fold in — a value is a purely session-scoped concept.
pub fn session_fingerprint(block: &CodeBlock, resolved_vars: &HashMap<String, String>) -> String {
    let mut names: Vec<String> = block.env.iter().map(|e| e.var_name.clone()).collect();
    if let Some(spec) = &block.interpreter {
        names.extend(crate::exec::interpreter_var_refs(spec));
    }
    names.sort();
    names.dedup();
    let base = fingerprint(block);
    if names.is_empty() {
        return base;
    }
    let mut parts = vec![base];
    for name in names {
        let value = resolved_vars.get(&name).map(String::as_str).unwrap_or("");
        parts.push(format!("{name}\u{0}{value}"));
    }
    format!("{:08x}", fnv1a(parts.join("\u{0}").as_bytes()))
}

fn parse_info_string(info: &str) -> (String, HashMap<String, String>) {
    let mut tokens = crate::attrs::tokenize(info).into_iter();
    let lang = tokens.next().unwrap_or_default();
    let attrs = crate::attrs::attrs_from_tokens(tokens);
    (lang, attrs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_named_runnable_block() {
        let md = "# Section\n\n```bash name=\"build\" cache\ncargo build\n```\n\ntail text\n";
        let blocks = scan_code_blocks(md);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].lang, "bash");
        assert_eq!(blocks[0].name.as_deref(), Some("build"));
        assert!(blocks[0].cache);
        assert_eq!(blocks[0].code, "cargo build");
    }

    #[test]
    fn ignores_fences_without_name() {
        let md = "```bash\necho hi\n```\n";
        assert!(scan_code_blocks(md).is_empty());
    }

    #[test]
    fn ignores_a_four_space_indented_fence_as_an_indented_code_block_not_a_fence() {
        // Same trick SPEC.md itself uses to show a fence's literal syntax
        // as inert documentation (see "Constraint fences", "Runnable code
        // fences") rather than a real runnable/constraint block — must
        // stay inert even once that text is scanned as another node's own
        // body (e.g. spliced in via `include`).
        let md = "    ```bash name=\"build\" cache\n    echo hi\n    ```\n";
        assert!(scan_code_blocks(md).is_empty());
        assert!(
            scan_constraint_blocks("    ```starlark constraint\n    fail(\"x\")\n    ```\n")
                .is_empty()
        );
    }

    #[test]
    fn still_recognizes_a_fence_indented_by_up_to_three_spaces() {
        // e.g. nested one level under a Markdown list item — CommonMark's
        // indented-code-block rule only kicks in at 4+ spaces.
        let md = "  ```bash name=\"build\" cache\n  echo hi\n  ```\n";
        let blocks = scan_code_blocks(md);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].code, "  echo hi");
    }

    #[test]
    fn interpreter_defaults_to_none() {
        let md = "```bash name=\"x\"\necho hi\n```\n";
        assert_eq!(scan_code_blocks(md)[0].interpreter, None);
    }

    #[test]
    fn interpreter_attr_makes_a_non_bash_lang_runnable() {
        // `python` isn't `is_supported_lang`, but an explicit `interpreter=`
        // makes the fence a runnable candidate anyway.
        let md = "```python name=\"seed\" interpreter=\"python3 -u\"\nprint('hi')\n```\n";
        let blocks = scan_code_blocks(md);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].interpreter.as_deref(), Some("python3 -u"));
    }

    #[test]
    fn button_body_is_kept_as_its_own_caption() {
        // A `button` fence has no `label=` attribute at all — its body
        // *is* the caption a UI renders on the button (see
        // `crate::exec::BUTTON_LANG`), same as any other fence's `code`.
        let md = "```button name=\"full-import\" deps=\"parsers/step-5\"\n🚀 Run everything\n```\n";
        let blocks = scan_code_blocks(md);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].code, "🚀 Run everything");
    }

    #[test]
    fn button_lang_is_runnable_with_no_interpreter() {
        // `button` is `is_supported_lang`, unlike `python` above — no
        // `interpreter=` needed for it to count as a runnable candidate.
        let md = "```button name=\"full-import\" deps=\"parsers/step-5\"\n```\n";
        let blocks = scan_code_blocks(md);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].lang, "button");
    }

    #[test]
    fn scan_runnable_blocks_names_a_lone_unnamed_interpreter_fence_after_the_node() {
        let md = "```python interpreter=\"python3\"\nprint('hi')\n```\n";
        let blocks = scan_runnable_blocks("my-node", md);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].name.as_deref(), Some("my-node"));
    }

    #[test]
    fn ignores_named_fence_in_an_unsupported_language() {
        // A `yaml`/`json`/... example fence some `name=` was still added
        // to (or copy-pasted from a bash one) never counts as runnable —
        // meshfox only knows how to execute bash/sh, see `crate::exec`.
        let md = "```yaml name=\"config\"\nkey: value\n```\n";
        assert!(scan_code_blocks(md).is_empty());
    }

    #[test]
    fn scan_runnable_blocks_ignores_the_lone_unnamed_fence_in_an_unsupported_language() {
        // The exact shape of an ordinary (non-canvas) Markdown README: one
        // unnamed `yaml` config example under a heading with no meshfox
        // structure of its own — must not be mistaken for "the" runnable
        // block of its enclosing node just because it's the only fence
        // with a non-empty info string.
        let md = "```yaml\nkey: value\n```\n";
        assert!(scan_runnable_blocks("my-node", md).is_empty());
    }

    #[test]
    fn scan_runnable_blocks_skips_an_unsupported_lang_fence_when_picking_the_lone_unnamed_one() {
        // A supported-language unnamed fence still gets implicitly named,
        // even alongside an unsupported-language one — the latter was
        // never a candidate to begin with, so it doesn't make the choice
        // "ambiguous".
        let md = "```yaml\nkey: value\n```\n\n```bash\necho hi\n```\n";
        let blocks = scan_runnable_blocks("my-node", md);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].lang, "bash");
        assert_eq!(blocks[0].name.as_deref(), Some("my-node"));
    }

    #[test]
    fn scan_runnable_blocks_names_a_lone_unnamed_fence_after_the_node() {
        let md = "```bash\necho hi\n```\n";
        let blocks = scan_runnable_blocks("my-node", md);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].name.as_deref(), Some("my-node"));
    }

    #[test]
    fn scan_runnable_blocks_never_mistakes_cached_output_for_the_implicit_block() {
        // A cached-output block is itself an unnamed ` ```text ` fence
        // (see output::render_output_block) — without excluding it, *every*
        // named+cached block would spuriously grow a second "implicit"
        // block once it had ever been run, named after its own node.
        let md = concat!(
            "```bash name=\"smoke\" cache\necho hi\n```\n",
            "<!-- meshfox:output name=\"smoke\" -->\n",
            "```text\nexit code: 0\n\nhi\n```\n",
            "<!-- /meshfox:output -->\n",
        );
        let blocks = scan_runnable_blocks("smoke-test", md);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].name.as_deref(), Some("smoke"));
    }

    #[test]
    fn scan_runnable_blocks_leaves_multiple_unnamed_fences_unnamed() {
        // Ambiguous which one would be "the" implicit block — neither gets
        // a name, same as scan_code_blocks already treats any unnamed
        // fence (matches ignores_fences_without_name above).
        let md = "```bash\necho a\n```\n\n```bash\necho b\n```\n";
        assert!(scan_runnable_blocks("my-node", md).is_empty());
    }

    #[test]
    fn scan_runnable_blocks_names_the_sole_unnamed_fence_even_alongside_named_ones() {
        let md = "```bash name=\"build\" cache\ncargo build\n```\n\n```bash\necho hi\n```\n";
        let blocks = scan_runnable_blocks("my-node", md);
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].name.as_deref(), Some("build"));
        assert_eq!(blocks[1].name.as_deref(), Some("my-node"));
    }

    #[test]
    fn scan_runnable_blocks_explicit_name_still_wins_over_implicit() {
        // An explicit `name=` always takes precedence — a fence naming
        // itself after the node explicitly behaves exactly the same as
        // the implicit case (both end up with name == node_id), just
        // without needing scan_runnable_blocks's fallback at all.
        let md = "```bash name=\"my-node\"\necho hi\n```\n";
        assert_eq!(scan_code_blocks(md)[0].name.as_deref(), Some("my-node"));
        assert_eq!(
            scan_runnable_blocks("my-node", md)[0].name.as_deref(),
            Some("my-node")
        );
    }

    #[test]
    fn deps_defaults_to_empty() {
        let md = "```bash name=\"x\"\necho hi\n```\n";
        assert!(scan_code_blocks(md)[0].deps.is_empty());
    }

    #[test]
    fn deps_parses_bare_and_qualified_names() {
        let md = "```bash name=\"deploy\" deps=\"build,other-node/test\"\necho hi\n```\n";
        let deps = &scan_code_blocks(md)[0].deps;
        assert_eq!(
            deps,
            &vec![
                BlockRef {
                    node_id: None,
                    block_name: "build".to_string(),
                    sync: false,
                },
                BlockRef {
                    node_id: Some("other-node".to_string()),
                    block_name: "test".to_string(),
                    sync: false,
                },
            ]
        );
    }

    #[test]
    fn deps_parses_trailing_bang_as_sync_on_bare_and_qualified_names() {
        let md =
            "```bash name=\"deploy\" deps=\"build!,other-node/test!\"\necho hi\n```\n";
        let deps = &scan_code_blocks(md)[0].deps;
        assert_eq!(
            deps,
            &vec![
                BlockRef {
                    node_id: None,
                    block_name: "build".to_string(),
                    sync: true,
                },
                BlockRef {
                    node_id: Some("other-node".to_string()),
                    block_name: "test".to_string(),
                    sync: true,
                },
            ]
        );
    }

    #[test]
    fn fingerprint_changes_when_a_deps_entry_gains_the_sync_suffix() {
        let a = &scan_code_blocks("```bash name=\"x\" deps=\"y\"\necho hi\n```\n")[0];
        let b = &scan_code_blocks("```bash name=\"x\" deps=\"y!\"\necho hi\n```\n")[0];
        assert_ne!(fingerprint(a), fingerprint(b));
    }

    #[test]
    fn cache_defaults_to_false() {
        let md = "```bash name=\"x\"\necho hi\n```\n";
        let blocks = scan_code_blocks(md);
        assert!(!blocks[0].cache);
    }

    #[test]
    fn env_defaults_to_empty() {
        let md = "```bash name=\"x\"\necho hi\n```\n";
        assert!(scan_code_blocks(md)[0].env.is_empty());
    }

    #[test]
    fn env_parses_bare_dollar_as_pass_through() {
        let md = "```bash name=\"install\" env=\"$INSTALL_PATH\"\necho hi\n```\n";
        let env = &scan_code_blocks(md)[0].env;
        assert_eq!(
            env,
            &vec![EnvRef {
                local_name: "INSTALL_PATH".to_string(),
                var_name: "INSTALL_PATH".to_string()
            }]
        );
    }

    #[test]
    fn env_dollar_prefix_is_optional_for_pass_through() {
        let md = "```bash name=\"install\" env=\"INSTALL_PATH\"\necho hi\n```\n";
        let env = &scan_code_blocks(md)[0].env;
        assert_eq!(
            env,
            &vec![EnvRef {
                local_name: "INSTALL_PATH".to_string(),
                var_name: "INSTALL_PATH".to_string()
            }]
        );
    }

    #[test]
    fn env_parses_rename_with_and_without_dollar() {
        let md = "```bash name=\"install\" env=\"PREFIX=$INSTALL_PATH,MODE2=MODE\"\necho hi\n```\n";
        let env = &scan_code_blocks(md)[0].env;
        assert_eq!(
            env,
            &vec![
                EnvRef {
                    local_name: "PREFIX".to_string(),
                    var_name: "INSTALL_PATH".to_string()
                },
                EnvRef {
                    local_name: "MODE2".to_string(),
                    var_name: "MODE".to_string()
                },
            ]
        );
    }

    #[test]
    fn env_parses_multiple_comma_separated_entries() {
        let md = "```bash name=\"x\" env=\"$A,$B,C=$D\"\necho hi\n```\n";
        let env = &scan_code_blocks(md)[0].env;
        assert_eq!(
            env,
            &vec![
                EnvRef {
                    local_name: "A".to_string(),
                    var_name: "A".to_string()
                },
                EnvRef {
                    local_name: "B".to_string(),
                    var_name: "B".to_string()
                },
                EnvRef {
                    local_name: "C".to_string(),
                    var_name: "D".to_string()
                },
            ]
        );
    }

    #[test]
    fn default_flag_defaults_to_false() {
        let md = "```bash name=\"x\"\necho hi\n```\n";
        assert!(!scan_code_blocks(md)[0].default);
    }

    #[test]
    fn default_flag_parses_bare_and_explicit_false() {
        let md = "```bash name=\"x\" default\necho hi\n```\n\n```bash name=\"y\" default=false\necho hi\n```\n";
        let blocks = scan_code_blocks(md);
        assert!(blocks[0].default);
        assert!(!blocks[1].default);
    }

    #[test]
    fn tty_flag_defaults_to_false() {
        let md = "```bash name=\"x\"\necho hi\n```\n";
        assert!(!scan_code_blocks(md)[0].tty);
    }

    #[test]
    fn tty_flag_parses_bare_and_explicit_false() {
        let md =
            "```bash name=\"x\" tty\necho hi\n```\n\n```bash name=\"y\" tty=false\necho hi\n```\n";
        let blocks = scan_code_blocks(md);
        assert!(blocks[0].tty);
        assert!(!blocks[1].tty);
    }

    #[test]
    fn autoclose_flag_defaults_to_false() {
        let md = "```bash name=\"x\" tty\necho hi\n```\n";
        assert!(!scan_code_blocks(md)[0].autoclose);
    }

    #[test]
    fn autoclose_flag_parses_bare_and_explicit_false() {
        let md = "```bash name=\"x\" tty autoclose\necho hi\n```\n\n```bash name=\"y\" tty autoclose=false\necho hi\n```\n";
        let blocks = scan_code_blocks(md);
        assert!(blocks[0].autoclose);
        assert!(!blocks[1].autoclose);
    }

    #[test]
    fn always_flag_defaults_to_false() {
        let md = "```bash name=\"x\"\necho hi\n```\n";
        assert!(!scan_code_blocks(md)[0].always);
    }

    #[test]
    fn always_flag_parses_bare_and_explicit_false() {
        let md =
            "```bash name=\"x\" always\necho hi\n```\n\n```bash name=\"y\" always=false\necho hi\n```\n";
        let blocks = scan_code_blocks(md);
        assert!(blocks[0].always);
        assert!(!blocks[1].always);
    }

    #[test]
    fn is_default_true_for_explicit_flag_or_name_matching_node_id() {
        let md = "```bash name=\"build\" default\necho hi\n```\n";
        let flagged = &scan_code_blocks(md)[0];
        assert!(is_default(flagged, "any-node"));

        let md = "```bash name=\"my-node\"\necho hi\n```\n";
        let by_name = &scan_code_blocks(md)[0];
        assert!(is_default(by_name, "my-node"));
        assert!(!is_default(by_name, "other-node"));
    }

    #[test]
    fn default_block_finds_the_sole_qualifying_block() {
        let md =
            "```bash name=\"build\"\necho a\n```\n\n```bash name=\"run\" default\necho b\n```\n";
        let blocks = scan_code_blocks(md);
        let default = default_block("node", &blocks).unwrap();
        assert_eq!(default.unwrap().name.as_deref(), Some("run"));
    }

    #[test]
    fn default_block_ok_none_when_nothing_qualifies() {
        let md = "```bash name=\"build\"\necho a\n```\n\n```bash name=\"run\"\necho b\n```\n";
        let blocks = scan_code_blocks(md);
        assert_eq!(default_block("node", &blocks).unwrap(), None);
    }

    #[test]
    fn default_block_errs_when_more_than_one_qualifies() {
        // "node" is both explicitly named after the node id (implicit
        // default) and there's a second block explicitly flagged default
        // too — only one is allowed.
        let md =
            "```bash name=\"node\"\necho a\n```\n\n```bash name=\"run\" default\necho b\n```\n";
        let blocks = scan_code_blocks(md);
        let err = default_block("node", &blocks).unwrap_err();
        assert_eq!(err.len(), 2);
        assert!(err.contains(&"node".to_string()));
        assert!(err.contains(&"run".to_string()));
    }

    #[test]
    fn multiple_blocks_in_order() {
        let md = "```bash name=\"a\"\necho a\n```\ntext\n```bash name=\"b\"\necho b\n```\n";
        let blocks = scan_code_blocks(md);
        let names: Vec<_> = blocks.iter().map(|b| b.name.clone().unwrap()).collect();
        assert_eq!(names, vec!["a", "b"]);
    }

    #[test]
    fn span_covers_whole_fence() {
        let md = "```bash name=\"a\"\necho a\n```\nafter";
        let blocks = scan_code_blocks(md);
        assert_eq!(
            &md[blocks[0].span.clone()],
            "```bash name=\"a\"\necho a\n```"
        );
    }

    #[test]
    fn shorter_nested_fence_does_not_close_a_longer_outer_one() {
        // A 4-backtick fence wrapping a 3-backtick example, same convention
        // README.md itself uses to show a fence-inside-a-fence. The inner
        // ``` markers must not end the outer fence early.
        let md = "````markdown\n```bash name=\"inner\"\necho hi\n```\n````\n";
        // The inner ``` markers are just content of the outer fence, so
        // scan_code_blocks (backtick fences only) sees the outer fence as
        // unnamed (info = "markdown", no name= attr) and finds no runnable
        // block at all — not a spurious one named "inner".
        assert!(scan_code_blocks(md).is_empty());
    }

    #[test]
    fn closing_fence_must_be_at_least_as_long_as_opening() {
        // A 4-backtick fence is not closed by a 3-backtick line.
        let raw = scan_raw_fences("````text\nline with ``` inside\n````\n");
        assert_eq!(raw.len(), 1);
        assert_eq!(raw[0].delim_len, 4);
        assert_eq!(raw[0].code, "line with ``` inside");
    }

    #[test]
    fn arbitrary_backtick_runs_in_content_do_not_confuse_a_long_enough_fence() {
        // Simulates cached command output containing runs of backticks up
        // to length 5; a 6-backtick wrapper must stay intact around it.
        let md =
            "``````text\nexit code: 0\n\n# not a heading\n```\n````\n`````\ntail\n``````\nafter\n";
        let raw = scan_raw_fences(md);
        assert_eq!(raw.len(), 1);
        assert!(raw[0].code.contains("# not a heading"));
        assert!(raw[0].code.contains("`````"));
    }

    #[test]
    fn scan_constraint_blocks_finds_a_flagged_fence() {
        let md = "prose\n\n```starlark constraint\nfail(\"bad\")\n```\n\nmore prose";
        let blocks = scan_constraint_blocks(md);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].code, "fail(\"bad\")");
        assert_eq!(blocks[0].name, None);
    }

    #[test]
    fn scan_constraint_blocks_ignores_a_plain_starlark_fence() {
        // No bare `constraint` flag — just an example/documentation fence,
        // not a real check.
        let md = "```starlark\nfail(\"bad\")\n```";
        assert!(scan_constraint_blocks(md).is_empty());
    }

    #[test]
    fn scan_constraint_blocks_ignores_wrong_language_even_with_the_flag() {
        let md = "```lua constraint\nfail(\"bad\")\n```";
        assert!(scan_constraint_blocks(md).is_empty());
    }

    #[test]
    fn scan_constraint_blocks_reads_the_name_attribute() {
        let md = "```starlark constraint name=\"table-shape\"\npass\n```";
        let blocks = scan_constraint_blocks(md);
        assert_eq!(blocks[0].name.as_deref(), Some("table-shape"));
    }

    #[test]
    fn scan_constraint_blocks_finds_several_in_document_order() {
        let md = "```starlark constraint name=\"a\"\npass\n```\n\ntext\n\n```starlark constraint name=\"b\"\npass\n```\n";
        let blocks = scan_constraint_blocks(md);
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].name.as_deref(), Some("a"));
        assert_eq!(blocks[1].name.as_deref(), Some("b"));
    }

    #[test]
    fn scan_constraint_blocks_coexists_with_prose_and_other_fences() {
        let md = "some description\n\n```bash name=\"build\"\ncargo build\n```\n\n```starlark constraint\npass\n```\n";
        let blocks = scan_constraint_blocks(md);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].code, "pass");
    }

    #[test]
    fn strip_fence_attrs_drops_meshfox_attributes() {
        let md = "prose\n\n```bash name=\"build\" cache deps=\"x\"\necho hi\n```\n\nmore prose";
        let stripped = strip_fence_attrs(md);
        assert!(stripped.contains("```bash\necho hi\n```"));
        assert!(!stripped.contains("name="));
        assert!(!stripped.contains("cache"));
        assert!(stripped.starts_with("prose\n\n"));
        assert!(stripped.ends_with("more prose"));
    }

    #[test]
    fn strip_fence_attrs_leaves_plain_text_untouched() {
        let md = "just prose, no fences\n";
        assert_eq!(strip_fence_attrs(md), md);
    }

    // TODO.canvas.md: "Ошибка при неизвестных параметрах в validate" —
    // `unknown_fence_attr` is `validate`-only, same split as
    // `mdcanvas::unknown_node_edge_attr`.
    #[test]
    fn unknown_fence_attr_is_none_for_known_attributes_only() {
        let md = "```bash name=\"build\" cache deps=\"other\"\ncargo build\n```\n";
        assert_eq!(unknown_fence_attr(md), None);
    }

    #[test]
    fn unknown_fence_attr_catches_a_typo_d_attribute() {
        let md = "```bash name=\"build\" cach\ncargo build\n```\n";
        let err = unknown_fence_attr(md).expect("cach is not a known attribute");
        assert_eq!(err.attr, "cach");
        assert!(err.context.contains("build"));
    }

    #[test]
    fn unknown_fence_attr_also_checks_an_unnamed_fence() {
        let md = "```bash cach\ncargo build\n```\n";
        let err = unknown_fence_attr(md).expect("cach is not a known attribute");
        assert_eq!(err.attr, "cach");
    }

    #[test]
    fn fingerprint_is_deterministic_for_the_same_block() {
        let md = "```bash name=\"x\" env=\"$A\" deps=\"y\"\necho hi\n```\n";
        let block = &scan_code_blocks(md)[0];
        assert_eq!(fingerprint(block), fingerprint(block));
    }

    #[test]
    fn fingerprint_changes_when_the_code_changes() {
        let a = &scan_code_blocks("```bash name=\"x\"\necho a\n```\n")[0];
        let b = &scan_code_blocks("```bash name=\"x\"\necho b\n```\n")[0];
        assert_ne!(fingerprint(a), fingerprint(b));
    }

    #[test]
    fn fingerprint_changes_when_env_changes() {
        let a = &scan_code_blocks("```bash name=\"x\" env=\"$A\"\necho hi\n```\n")[0];
        let b = &scan_code_blocks("```bash name=\"x\" env=\"$B\"\necho hi\n```\n")[0];
        assert_ne!(fingerprint(a), fingerprint(b));
    }

    #[test]
    fn fingerprint_changes_when_deps_changes() {
        let a = &scan_code_blocks("```bash name=\"x\" deps=\"a\"\necho hi\n```\n")[0];
        let b = &scan_code_blocks("```bash name=\"x\" deps=\"b\"\necho hi\n```\n")[0];
        assert_ne!(fingerprint(a), fingerprint(b));
    }

    #[test]
    fn fingerprint_changes_when_interpreter_changes() {
        let a = &scan_code_blocks("```python name=\"x\" interpreter=\"python3\"\npass\n```\n")[0];
        let b = &scan_code_blocks("```python name=\"x\" interpreter=\"python3.11\"\npass\n```\n")[0];
        assert_ne!(fingerprint(a), fingerprint(b));
    }

    #[test]
    fn session_fingerprint_changes_when_a_referenced_var_value_changes() {
        let block = &scan_code_blocks("```bash name=\"x\" env=\"$A\"\necho hi\n```\n")[0];
        let mut a = HashMap::new();
        a.insert("A".to_string(), "1".to_string());
        let mut b = HashMap::new();
        b.insert("A".to_string(), "2".to_string());
        assert_ne!(session_fingerprint(block, &a), session_fingerprint(block, &b));
    }

    #[test]
    fn session_fingerprint_ignores_an_unrelated_var_value_changing() {
        let block = &scan_code_blocks("```bash name=\"x\" env=\"$A\"\necho hi\n```\n")[0];
        let mut a = HashMap::new();
        a.insert("A".to_string(), "1".to_string());
        a.insert("UNRELATED".to_string(), "1".to_string());
        let mut b = HashMap::new();
        b.insert("A".to_string(), "1".to_string());
        b.insert("UNRELATED".to_string(), "2".to_string());
        assert_eq!(session_fingerprint(block, &a), session_fingerprint(block, &b));
    }

    #[test]
    fn session_fingerprint_changes_when_an_interpreter_referenced_var_value_changes() {
        let block =
            &scan_code_blocks("```python name=\"x\" interpreter=\"$PYTHON -u\"\npass\n```\n")[0];
        let mut a = HashMap::new();
        a.insert("PYTHON".to_string(), "python3".to_string());
        let mut b = HashMap::new();
        b.insert("PYTHON".to_string(), "python3.11".to_string());
        assert_ne!(session_fingerprint(block, &a), session_fingerprint(block, &b));
    }

    #[test]
    fn session_fingerprint_matches_plain_fingerprint_for_a_block_with_no_vars() {
        let block = &scan_code_blocks("```bash name=\"x\"\necho hi\n```\n")[0];
        assert_eq!(session_fingerprint(block, &HashMap::new()), fingerprint(block));
    }

    #[test]
    fn fingerprint_ignores_attributes_that_do_not_affect_execution() {
        // `cache`/`tty`/`default`/`name` itself don't change what running
        // the block actually does — only its code/lang/interpreter/env=/
        // deps= do.
        let a = &scan_code_blocks("```bash name=\"x\" cache\necho hi\n```\n")[0];
        let b = &scan_code_blocks("```bash name=\"y\"\necho hi\n```\n")[0];
        assert_eq!(fingerprint(a), fingerprint(b));
    }

    #[test]
    fn unknown_fence_attr_ignores_an_unsupported_language() {
        // `yaml` isn't `is_supported_lang`, so this is never a candidate
        // fence at all — a typo'd key here is just prose to meshfox.
        let md = "```yaml cach\nkey: value\n```\n";
        assert_eq!(unknown_fence_attr(md), None);
    }
}
