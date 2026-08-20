// Client-side mirror of crates/core/src/fence.rs's runnable-fence scan plus
// crates/core/src/output.rs's cached-output marker convention. Splits a
// node's Markdown body into alternating prose and runnable-code segments
// so the UI can render each code block with its own Run button and (if
// present) its own parsed output, instead of one opaque blob of Markdown —
// see README.md for the conventions themselves.

function tokenize(s: string): string[] {
  const tokens: string[] = [];
  let cur = "";
  let inQuotes = false;
  for (const c of s) {
    if (c === '"') {
      inQuotes = !inQuotes;
      cur += c;
    } else if (/\s/.test(c) && !inQuotes) {
      if (cur) {
        tokens.push(cur);
        cur = "";
      }
    } else {
      cur += c;
    }
  }
  if (cur) tokens.push(cur);
  return tokens;
}

function unquote(v: string): string {
  if (v.length >= 2 && v.startsWith('"') && v.endsWith('"')) {
    return v.slice(1, -1);
  }
  return v;
}

function attrsFromTokens(tokens: string[]): Record<string, string> {
  const attrs: Record<string, string> = {};
  for (const tok of tokens) {
    const eq = tok.indexOf("=");
    if (eq >= 0) {
      attrs[tok.slice(0, eq)] = unquote(tok.slice(eq + 1));
    } else {
      attrs[tok] = "true";
    }
  }
  return attrs;
}

export interface CachedOutput {
  exitCode: number;
  text: string;
  /** The human-readable duration `core::output::format_duration_ms` already
   * rendered into the header (`"2.3s"`, `"1m 05s"`, ...) — kept as the same
   * string rather than re-parsed back into milliseconds, since nothing here
   * needs to do arithmetic on it, only display it. `undefined` for cached
   * output written before this field existed (no `· <duration>` in the
   * header at all). */
  durationText?: string;
  /** True when the fence's own live fingerprint (see `fingerprint` below)
   * no longer matches the `hash=` its cached-output marker was written
   * with — the code/lang/interpreter/env=/deps= changed since this output
   * was captured. Also true for a marker with no `hash=` at all (cached
   * before this field existed — nothing to compare against, so treated the
   * same as stale). Never hides/discards the cached text itself, only
   * flags it — see SPEC.md's "Cached output". */
  stale: boolean;
}

/** FNV-1a over UTF-8 bytes, returned as 8 lowercase hex digits — must stay
 * byte-for-byte in sync with `core::fence::fingerprint`'s own Rust
 * implementation (see that function's own doc comment for why this is
 * hand-ported rather than shared): same offset/prime constants, same
 * unsigned-32-bit wraparound. `Math.imul` is what gives the 32-bit wrapping
 * multiply JS numbers don't have natively. */
function fnv1aHex(bytes: Uint8Array): string {
  let hash = 0x811c9dc5;
  for (let i = 0; i < bytes.length; i++) {
    hash ^= bytes[i];
    hash = Math.imul(hash, 0x01000193);
  }
  return (hash >>> 0).toString(16).padStart(8, "0");
}

const NUL = "\u0000";

function stripDollar(s: string): string {
  return s.startsWith("$") ? s.slice(1) : s;
}

/** Mirrors `core::fence::fingerprint(block)` field-for-field: `lang`, `code`,
 * `interpreter` (empty string when unset, same as Rust's
 * `unwrap_or_default()`), then each `env=` entry as its own `local\0var`
 * pair (same `local_name`/`var_name` split+dollar-strip
 * `core::fence::parse_env_ref` does), then each raw `deps=` token as-is
 * (already in the exact `node-id/block-name`-or-bare-name shape Rust
 * reconstructs its own parsed `BlockRef`s back into). Joined with NUL,
 * hashed as UTF-8 bytes — see `fnv1aHex`. */
export function fingerprint(
  lang: string,
  code: string,
  interpreter: string | undefined,
  envAttr: string | undefined,
  depsAttr: string | undefined,
): string {
  const parts = [lang, code, interpreter ?? ""];
  for (const raw of (envAttr ?? "").split(",").map((s) => s.trim()).filter((s) => s.length > 0)) {
    const eq = raw.indexOf("=");
    if (eq >= 0) {
      parts.push(`${raw.slice(0, eq).trim()}${NUL}${stripDollar(raw.slice(eq + 1).trim())}`);
    } else {
      const varName = stripDollar(raw);
      parts.push(`${varName}${NUL}${varName}`);
    }
  }
  for (const raw of (depsAttr ?? "").split(",").map((s) => s.trim()).filter((s) => s.length > 0)) {
    parts.push(raw);
  }
  return fnv1aHex(new TextEncoder().encode(parts.join(NUL)));
}

export interface MarkdownSegment {
  type: "markdown";
  content: string;
}

export interface CodeSegment {
  type: "code";
  lang: string;
  name: string;
  cache: boolean;
  /** Mirrors `core::fence::CodeBlock.tty` — this block wants a real
   * interactive terminal instead of the usual captured/streamed output;
   * see SPEC.md's "Interactive (`tty`) blocks". Mutually exclusive with
   * `cache` (a `meshfox validate` error), so a segment is never both. */
  tty: boolean;
  /** Mirrors `core::fence::CodeBlock.autoclose` — only meaningful when
   * `tty` is also set (`meshfox validate` rejects it otherwise): once the
   * interactive process exits, return to the canvas immediately instead of
   * leaving the panel open showing its exit code until closed by hand. */
  autoclose: boolean;
  /** Raw `deps="a,b"` entries, in document order — a bare name is a block
   * in this same node, `node-id/block-name` is a block elsewhere. See
   * `./deps.ts` for resolving these into concrete addresses. */
  deps: string[];
  /** Mirrors `core::fence::CodeBlock.default` — the explicit `default`
   * flag (`` ```bash name="run" default ``). A block also counts as this
   * node's default when `name` equals the node's own id (see `MeshNode.tsx`'s
   * `defaultBlockName`), whether or not this flag is set — same "explicit
   * flag OR self-named" rule `core::fence::is_default` uses. */
  default: boolean;
  /** Mirrors `core::fence::CodeBlock.interpreter` — a shebang-style
   * command+flags string (`interpreter="python3 -u"`) this block runs
   * under instead of the implicit `bash`/`sh` executor. When set, `lang`
   * is purely a syntax-highlighting hint; see `isSupportedLang` below. */
  interpreter?: string;
  code: string;
  output?: CachedOutput;
}

export interface ConstraintSegment {
  type: "constraint";
  /** Explicit `name="..."` attribute, if given — mirrors
   * `core::fence::ConstraintBlock.name`. Together with this segment's
   * position among a node's other constraint segments, this is what the
   * server's `ConstraintStatusDto.label` is derived from; the client never
   * recomputes that itself; it matches this node's `constraintResults` to
   * its constraint segments purely by position (see `MeshNode.tsx`'s
   * `MeshNodeBody`), since both are built from the same document order. */
  name?: string;
  code: string;
}

export type BodySegment = MarkdownSegment | CodeSegment | ConstraintSegment;

const OUTPUT_END_MARKER = "<!-- /meshfox:output -->";

/** A line that's *only* a `<!-- meshfox:whatever ... -->` bookkeeping
 * comment — `meshfox:node`/`meshfox:canvas`/`meshfox:edge` markers never
 * reach here at all (they're structural: the server slices node/body
 * boundaries around them, so they're never part of `node.text` to begin
 * with), but `meshfox:var` declarations live inline in a node's own prose
 * exactly like any other body content, so `parseBody` sees them and would
 * otherwise hand them to `ReactMarkdown` — which renders an HTML comment
 * as plain visible text (no `rehype-raw`, so raw HTML is escaped, not
 * parsed as a comment) — same fate as `meshfox:output`'s markers would
 * have here if `parseBody` didn't already special-case consuming those
 * itself, just below. Matches any `meshfox:` marker generically (not just
 * `var`) so a future bookkeeping comment living inline doesn't need its
 * own one-off filter here too. */
function isBookkeepingCommentLine(line: string): boolean {
  return /^<!--\s*\/?meshfox:\S+.*-->$/.test(line.trim());
}

/**
 * True only when `line`'s leading indentation is under 4 spaces —
 * mirrors `core::fence::fence_open`'s own indent check (CommonMark: 4+
 * spaces of indentation makes a line part of an *indented* code block,
 * not a fence opener; this is how SPEC.md's own illustrative fence
 * examples, e.g. under "Runnable code fences", read as inert
 * documentation instead of being picked up as real runnable blocks).
 * Only the space-character count matters here, same as the Rust side —
 * a tab isn't treated as indentation for this check.
 */
function fenceIndentOk(line: string): boolean {
  let spaces = 0;
  while (spaces < line.length && line[spaces] === " ") spaces++;
  return spaces < 4;
}

/** Languages `crate::exec` actually knows how to run — mirrors
 * `core::exec::is_supported_lang`. A fence in any other language (`yaml`,
 * `starlark`, ...) is never runnable here, named or not, *unless* it
 * carries its own `interpreter=` attribute (see `isRunnableCandidate`) —
 * without this check, a `name=`'d or sole-unnamed fence in an unsupported
 * language with no `interpreter=` either would still get a Run button
 * that just errors when clicked, since the server's own
 * `candidate_fences` never considered it a candidate to begin with. */
function isSupportedLang(lang: string): boolean {
  return lang === "bash" || lang === "sh";
}

/** A fence is a runnable candidate if its `lang` is one `isSupportedLang`
 * already knows, or it carries its own `interpreter=` attribute naming
 * one explicitly — mirrors `core::fence::candidate_fences`'s own
 * `is_supported_lang(lang) || attrs.contains_key("interpreter")` gate. */
function isRunnableCandidate(lang: string, attrs: Record<string, string>): boolean {
  return isSupportedLang(lang) || attrs.interpreter !== undefined;
}

/**
 * Lightweight first pass over `markdown`: every top-level, supported-
 * language fence's `name=` presence, skipping a cached-output block's own
 * (always unnamed) fence — mirrors
 * `core::fence::candidate_fences`/`is_cached_output_fence`. Used only to
 * decide whether a node has exactly one unnamed fence (that node's
 * implicit block, if so — see `parseBody`'s `nodeId` param).
 */
function countUnnamedCandidateFences(markdown: string): number {
  const lines = markdown.split("\n");
  let count = 0;
  let i = 0;
  while (i < lines.length) {
    if (!fenceIndentOk(lines[i]) || !lines[i].trimStart().startsWith("```")) {
      i++;
      continue;
    }
    const trimmed = lines[i].trimStart();
    const info = trimmed.slice(3).trim();
    let j = i + 1;
    let closed = false;
    while (j < lines.length) {
      if (lines[j].trim() === "```") {
        closed = true;
        break;
      }
      j++;
    }
    if (!closed) break;
    if (info) {
      const precededByOutputMarker = (lines[i - 1] ?? "").trim().startsWith("<!-- meshfox:output");
      if (!precededByOutputMarker) {
        const [lang, ...rest] = tokenize(info);
        const attrs = attrsFromTokens(rest);
        if (isRunnableCandidate(lang, attrs) && attrs.name === undefined) count++;
      }
    }
    i = j + 1;
  }
  return count;
}

function parseCachedOutputBlock(inner: string): Omit<CachedOutput, "stale"> {
  // Always exactly `​```text\nexit code: N · <duration>\n\n<output>\n```​` —
  // see core::output::render_output_block. The `· <duration>` half is
  // absent from output cached before that field existed, hence optional.
  const match = /```text\n([\s\S]*?)\n?```/.exec(inner);
  const body = match ? match[1] : inner;
  const exitMatch = /^exit code: (-?\d+)(?: · (.+?))?\n?/.exec(body);
  return {
    exitCode: exitMatch ? Number(exitMatch[1]) : 0,
    durationText: exitMatch ? exitMatch[2] : undefined,
    text: exitMatch ? body.slice(exitMatch[0].length).replace(/^\n/, "") : body,
  };
}

/** Parses a `<!-- meshfox:output name="..." hash="..." -->` marker line's
 * own attributes — `null` if `line` isn't that marker for `name` at all
 * (matched on the `name=` prefix, not a full literal string, since `hash=`
 * varies run to run — same reasoning `core::output::start_marker_prefix`
 * has on the Rust side). */
function parseOutputMarkerAttrs(line: string, name: string): Record<string, string> | null {
  const trimmed = line.trim();
  const prefix = `<!-- meshfox:output name="${name}"`;
  if (!trimmed.startsWith(prefix) || !trimmed.endsWith("-->")) return null;
  const inner = trimmed.slice("<!--".length, -"-->".length).trim();
  const [construct, ...rest] = tokenize(inner);
  if (construct !== "meshfox:output") return null;
  return attrsFromTokens(rest);
}

/**
 * Splits `markdown` into prose and runnable-code segments, in document
 * order. `nodeId` is used only for a fence with no `name=` at all: it's
 * runnable too, implicitly named after `nodeId`, but *only* when it's the
 * node's sole unnamed fence (another one, named or not, makes the
 * omission ambiguous) — mirrors `core::fence::scan_runnable_blocks`; see
 * SPEC.md's "Runnable code fences".
 */
export function parseBody(markdown: string, nodeId: string): BodySegment[] {
  const soloUnnamed = countUnnamedCandidateFences(markdown) === 1;
  const lines = markdown.split("\n");
  const segments: BodySegment[] = [];
  let mdBuffer: string[] = [];

  const flushMarkdown = () => {
    const content = mdBuffer.join("\n");
    if (content.trim().length > 0) {
      segments.push({ type: "markdown", content });
    }
    mdBuffer = [];
  };

  let i = 0;
  while (i < lines.length) {
    if (!fenceIndentOk(lines[i]) || !lines[i].trimStart().startsWith("```")) {
      if (!isBookkeepingCommentLine(lines[i])) {
        mdBuffer.push(lines[i]);
      }
      i++;
      continue;
    }
    const trimmed = lines[i].trimStart();

    const info = trimmed.slice(3).trim();
    let j = i + 1;
    const codeLines: string[] = [];
    let closed = false;
    while (j < lines.length) {
      if (lines[j].trim() === "```") {
        closed = true;
        break;
      }
      codeLines.push(lines[j]);
      j++;
    }

    if (!closed) {
      // Unclosed fence swallows the rest of the doc, same as fence.rs.
      mdBuffer.push(...lines.slice(i));
      break;
    }

    const [lang, ...rest] = tokenize(info);
    const attrs = attrsFromTokens(rest);

    if (lang === "starlark" && attrs.constraint !== undefined && attrs.constraint !== "false") {
      // Mirrors `core::fence::scan_constraint_blocks` — a bare `constraint`
      // flag opts a starlark fence in, same as a runnable fence opts in
      // with `name=`. A plain, unflagged `​```starlark`​` fence (e.g. a
      // documentation example) falls through below instead: `starlark`
      // isn't in `isSupportedLang`, so it's left as inert Markdown.
      flushMarkdown();
      segments.push({ type: "constraint", name: attrs.name, code: codeLines.join("\n") });
      i = j + 1;
      continue;
    }

    if (!isRunnableCandidate(lang, attrs) || (!attrs.name && !soloUnnamed)) {
      // Either not a runnable candidate at all (unsupported language and
      // no `interpreter=` either — see `isRunnableCandidate`), or not a
      // runnable block and not this node's (one and only) implicitly-named
      // fence either — leave it as plain Markdown, fence and all.
      mdBuffer.push(...lines.slice(i, j + 1));
      i = j + 1;
      continue;
    }

    flushMarkdown();
    const name = attrs.name ?? nodeId;
    const cache = attrs.cache !== undefined && attrs.cache !== "false";
    const tty = attrs.tty !== undefined && attrs.tty !== "false";
    const autoclose = attrs.autoclose !== undefined && attrs.autoclose !== "false";
    const isDefault = attrs.default !== undefined && attrs.default !== "false";
    const interpreter = attrs.interpreter;
    const deps = (attrs.deps ?? "")
      .split(",")
      .map((s) => s.trim())
      .filter((s) => s.length > 0);

    // Look for this block's cached-output marker immediately after the
    // fence (see core::output — no blank line is inserted between them).
    let cursor = j + 1;
    const markerAttrs = lines[cursor] !== undefined ? parseOutputMarkerAttrs(lines[cursor], name) : null;
    let output: CachedOutput | undefined;
    if (markerAttrs) {
      const inner: string[] = [];
      let k = cursor + 1;
      let foundEnd = false;
      while (k < lines.length) {
        if (lines[k].trim() === OUTPUT_END_MARKER) {
          foundEnd = true;
          break;
        }
        inner.push(lines[k]);
        k++;
      }
      if (foundEnd) {
        const code = codeLines.join("\n");
        // No `hash=` at all (output cached before this field existed) is
        // treated the same as a mismatch — nothing to compare against, so
        // "can't vouch this is still current" defaults to stale.
        const stale =
          markerAttrs.hash === undefined ||
          markerAttrs.hash !== fingerprint(lang, code, interpreter, attrs.env, attrs.deps);
        output = { ...parseCachedOutputBlock(inner.join("\n")), stale };
        cursor = k + 1;
      }
    }

    segments.push({ type: "code", lang, name, cache, tty, autoclose, deps, default: isDefault, interpreter, code: codeLines.join("\n"), output });
    i = cursor;
  }
  flushMarkdown();
  return segments;
}

/**
 * `nodeId`'s default runnable block, if it has exactly one — mirrors
 * `core::fence::default_block`. A block qualifies via the explicit
 * `default` flag or by sharing the node's own id (implicitly, via the sole
 * unnamed fence, or explicitly via `name="<node-id>"` — see `parseBody`'s
 * `nodeId` param). `null` both when no block qualifies and when more than
 * one does (ambiguous — same as the Rust side, this just isn't eligible
 * for the shortcut; `meshfox check` is what reports the conflict). Returns
 * the whole segment, not just its name, so a caller can also see its
 * `tty` flag — see `defaultBlockName` below and `MeshNode.tsx`'s title bar
 * "▷ run" quick-run button, which needs both.
 */
export function defaultBlock(markdown: string, nodeId: string): CodeSegment | null {
  const candidates = parseBody(markdown, nodeId).filter(
    (seg): seg is CodeSegment => seg.type === "code" && (seg.default || seg.name === nodeId),
  );
  return candidates.length === 1 ? candidates[0] : null;
}

/** Just `defaultBlock`'s own name, for callers that don't need its `tty`
 * flag too. */
export function defaultBlockName(markdown: string, nodeId: string): string | null {
  return defaultBlock(markdown, nodeId)?.name ?? null;
}
