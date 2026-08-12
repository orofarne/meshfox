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
  code: string;
  output?: CachedOutput;
}

export type BodySegment = MarkdownSegment | CodeSegment;

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

/** Languages `crate::exec` actually knows how to run — mirrors
 * `core::exec::is_supported_lang`. A fence in any other language (`yaml`,
 * `starlark`, ...) is never runnable here, named or not: without this
 * check, a `name=`'d or sole-unnamed fence in an unsupported language would
 * still get a Run button that just errors when clicked, since the server's
 * own `candidate_fences` never considered it a candidate to begin with. */
function isSupportedLang(lang: string): boolean {
  return lang === "bash" || lang === "sh";
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
    const trimmed = lines[i].trimStart();
    if (!trimmed.startsWith("```")) {
      i++;
      continue;
    }
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
        if (isSupportedLang(lang) && attrsFromTokens(rest).name === undefined) count++;
      }
    }
    i = j + 1;
  }
  return count;
}

function parseCachedOutputBlock(inner: string): CachedOutput {
  // Always exactly `​```text\nexit code: N\n\n<output>\n```​` — see
  // core::output::render_output_block.
  const match = /```text\n([\s\S]*?)\n?```/.exec(inner);
  const body = match ? match[1] : inner;
  const exitMatch = /^exit code: (-?\d+)\n?/.exec(body);
  return {
    exitCode: exitMatch ? Number(exitMatch[1]) : 0,
    text: exitMatch ? body.slice(exitMatch[0].length).replace(/^\n/, "") : body,
  };
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
    const trimmed = lines[i].trimStart();
    if (!trimmed.startsWith("```")) {
      if (!isBookkeepingCommentLine(lines[i])) {
        mdBuffer.push(lines[i]);
      }
      i++;
      continue;
    }

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
    if (!isSupportedLang(lang) || (!attrs.name && !soloUnnamed)) {
      // Either an unsupported language (never runnable, regardless of
      // naming — see `isSupportedLang`), or not a runnable block and not
      // this node's (one and only) implicitly-named fence either — leave
      // it as plain Markdown, fence and all.
      mdBuffer.push(...lines.slice(i, j + 1));
      i = j + 1;
      continue;
    }

    flushMarkdown();
    const name = attrs.name ?? nodeId;
    const cache = attrs.cache !== undefined && attrs.cache !== "false";
    const tty = attrs.tty !== undefined && attrs.tty !== "false";
    const isDefault = attrs.default !== undefined && attrs.default !== "false";
    const deps = (attrs.deps ?? "")
      .split(",")
      .map((s) => s.trim())
      .filter((s) => s.length > 0);

    // Look for this block's cached-output marker immediately after the
    // fence (see core::output — no blank line is inserted between them).
    let cursor = j + 1;
    const startMarker = `<!-- meshfox:output name="${name}" -->`;
    let output: CachedOutput | undefined;
    if (lines[cursor]?.trim() === startMarker) {
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
        output = parseCachedOutputBlock(inner.join("\n"));
        cursor = k + 1;
      }
    }

    segments.push({ type: "code", lang, name, cache, tty, deps, default: isDefault, code: codeLines.join("\n"), output });
    i = cursor;
  }
  flushMarkdown();
  return segments;
}

/**
 * The name of `nodeId`'s default runnable block, if it has exactly one —
 * mirrors `core::fence::default_block`. A block qualifies via the explicit
 * `default` flag or by sharing the node's own id (implicitly, via the sole
 * unnamed fence, or explicitly via `name="<node-id>"` — see `parseBody`'s
 * `nodeId` param). `null` both when no block qualifies and when more than
 * one does (ambiguous — same as the Rust side, this just isn't eligible
 * for the shortcut; `meshfox check` is what reports the conflict). Used to
 * show the title bar's "▷ run" quick-run button (see `MeshNode.tsx`).
 */
export function defaultBlockName(markdown: string, nodeId: string): string | null {
  const candidates = parseBody(markdown, nodeId).filter(
    (seg): seg is CodeSegment => seg.type === "code" && (seg.default || seg.name === nodeId),
  );
  return candidates.length === 1 ? candidates[0].name : null;
}
