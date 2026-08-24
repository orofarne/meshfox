import type * as MonacoNS from "monaco-editor";
import { bundledThemes, createHighlighter, type Highlighter } from "shiki";
import { meshfoxGrammar, withMeshfoxTokenColors } from "./meshfoxGrammar";
import { THEMES } from "./shiki";

/**
 * Highlights meshfox's own syntax extensions on top of Monaco's built-in
 * Markdown tokenizer — the `meshfox:...` HTML comments (`meshfox:node`,
 * `meshfox:edge`, `meshfox:output`, `meshfox:var`, `meshfox:canvas`) and
 * runnable fences' `name=`/`cache`/`deps=`/`env=`/`default` attributes — so
 * the document's actual structure reads at a glance instead of blending
 * into a wall of dimmed HTML-comment text.
 *
 * The marker-*comment* half now uses the real grammar (`meshfoxGrammar.ts`'s
 * `.tmLanguage.json` injection) after all — `monacoSetup.ts` documents why
 * `shikiToMonaco` itself can't carry it into Monaco's own `TokensProvider`
 * API (its per-line `tokenizeLine2` path never resolves injections), but
 * Shiki's own higher-level `codeToTokens` *does* resolve them correctly, so
 * this file calls that itself and turns the result into `deltaDecorations`,
 * the same mechanism this file already used for hand-rolled regexes.
 *
 * Deliberately its **own**, separate `Highlighter` instance
 * (`getMeshfoxMarkerHighlighter` below) — not `shiki.ts`'s shared one.
 * Confirmed directly (isolated repro, not a guess): once *any* additional
 * language gets loaded onto a Shiki highlighter via `loadLanguage()` after
 * its initial construction — even a wholly unrelated one, like a fenced
 * code block's own "rust" — the injection grammar's resolution silently
 * breaks for every *other* language already on that same highlighter,
 * including this one's `markdown`+meshfox injection. `shiki.ts`'s own
 * highlighter does exactly that constantly (`ensureLanguageLoaded`, for
 * every fenced block's own language, most of which aren't known until a
 * canvas is actually opened) — so sharing it here would work fine for
 * exactly as long as no fence anywhere on the canvas used an unbundled-
 * upfront language, then silently stop working the moment one did. A
 * highlighter that only ever loads `markdown`+`meshfoxGrammar`, once, up
 * front, and is never touched by `loadLanguage` afterward, never hits that
 * bug at all.
 *
 * Tokenizing one line at a time (never the whole buffer) keeps this cheap:
 * every meshfox marker comment is single-line by convention (SPEC.md), so
 * no cross-line state is ever needed, and `MESHFOX_MARKER_LINE_RE` skips
 * the (overwhelming) majority of lines — ordinary prose — without ever
 * calling into Shiki at all.
 *
 * The fence-*attribute* half stays hand-rolled regex, unchanged: it isn't
 * something the injection grammar can express at all (confirmed
 * empirically, not assumed — a fence's own opening line gets consumed as
 * one atomic token by Markdown's own fenced-code-block rule before any
 * injected pattern ever gets a chance to match a sub-span within it,
 * begin/end *or* flat `match` rules alike), so there's no grammar-driven
 * alternative to fall back on here the way there is for comments.
 *
 * A distant descendant of the old CodeMirror version (`meshfoxSyntax.ts`,
 * retired once both editable surfaces moved to Monaco) — same decoration
 * shape/CSS classes throughout, just a different computation for half of
 * it now.
 */

let markerHighlighterPromise: Promise<Highlighter> | null = null;

/** `markdown` + `meshfoxGrammar`, loaded once, up front, and never
 * extended afterward — see this file's own top comment for exactly why
 * that "never extended" part is load-bearing, not just tidy. */
function getMeshfoxMarkerHighlighter(): Promise<Highlighter> {
  if (!markerHighlighterPromise) {
    markerHighlighterPromise = (async () => {
      const [light, dark] = await Promise.all([bundledThemes[THEMES.light](), bundledThemes[THEMES.dark]()]);
      return createHighlighter({
        themes: [withMeshfoxTokenColors(light.default, false), withMeshfoxTokenColors(dark.default, true)],
        langs: ["markdown", meshfoxGrammar],
      });
    })();
  }
  return markerHighlighterPromise;
}

interface MarkerRange {
  start: number;
  end: number;
  className: string;
}

/**
 * `key`, `key=value`, `key="quoted value"` — the one attribute shape
 * shared by every meshfox HTML comment (`id="x" x=0 cache`) and every
 * runnable fence's info string (```bash name="build" cache
 * deps="build"```, see SPEC.md). Marks the key and, if present, the value
 * (including its quotes) as two separate ranges. `base` is `text`'s own
 * absolute offset in the document, since `text` is always a slice already
 * pulled out of a larger match.
 */
function attrRanges(text: string, base: number): MarkerRange[] {
  const re = /([\w-]+)(=(?:"[^"]*"|'[^']*'|[^\s`>]*))?/g;
  const out: MarkerRange[] = [];
  let m: RegExpExecArray | null;
  while ((m = re.exec(text))) {
    const nameStart = base + m.index;
    const nameEnd = nameStart + m[1].length;
    out.push({ start: nameStart, end: nameEnd, className: "mesh-marker-attr-name" });
    if (m[2] && m[2].length > 1) {
      out.push({ start: nameEnd + 1, end: nameStart + m[0].length, className: "mesh-marker-attr-value" });
    }
  }
  return out;
}

// ```bash name="build" cache deps="build" env="X"``` — a runnable fence's
// info-string attributes (SPEC.md's "Runnable code fences"); `sh` is the
// same language's alias, and any other `lang` with its own `interpreter=`
// attribute counts too (mirrors `fence.ts`'s `isRunnableCandidate`). Only
// the attributes are decorated here — the language word and fence markers
// already get the markdown language's own styling.
const FENCE_RE = /```([\w-]*)((?:\s+[\w-]+(?:=(?:"[^"]*"|'[^']*'|[^\s`]*))?)*)/g;

function computeFenceAttrRanges(text: string): MarkerRange[] {
  const out: MarkerRange[] = [];
  let m: RegExpExecArray | null;
  FENCE_RE.lastIndex = 0;
  while ((m = FENCE_RE.exec(text))) {
    const lang = m[1];
    const attrs = m[2];
    if (!attrs) continue;
    if (lang !== "bash" && lang !== "sh" && !/\binterpreter=/.test(attrs)) continue;
    out.push(...attrRanges(attrs, m.index + m[0].length - attrs.length));
  }
  return out;
}

/** Cheap prefilter — only a line that could plausibly hold a marker
 * comment is ever handed to Shiki at all. Deliberately loose (matches
 * `<!-- meshfox:` and `<!-- /meshfox:` both) since a false positive just
 * costs one extra (still single-line, still cheap) tokenization call, not
 * a correctness problem — real scope-checking happens after. */
const MESHFOX_MARKER_LINE_RE = /<!--\s*\/?meshfox:/;

const MESHFOX_COMMENT_SCOPE = "comment.block.meshfox";
const MESHFOX_KEYWORD_SCOPE = "keyword.other.meshfox";
const MESHFOX_TAG_END_SCOPE = "punctuation.definition.tag.end.meshfox";
const MESHFOX_ATTR_NAME_SCOPE = "entity.other.attribute-name.meshfox";
const MESHFOX_ATTR_VALUE_SCOPE = "string.unquoted.meshfox";

/**
 * Real grammar-driven ranges for one candidate line (already confirmed by
 * `MESHFOX_MARKER_LINE_RE` to be worth tokenizing) — `lineStart` is that
 * line's own absolute offset into the full document, since `line` itself
 * is tokenized in isolation.
 */
async function computeCommentMarkerRangesForLine(line: string, lineStart: number): Promise<MarkerRange[]> {
  const hl = await getMeshfoxMarkerHighlighter();
  const { tokens } = hl.codeToTokens(line, {
    lang: "markdown",
    theme: THEMES.light,
    includeExplanation: true,
  });

  const out: MarkerRange[] = [];
  let commentStart: number | null = null;
  let commentEnd = 0;
  let offset = 0;
  for (const tokenLine of tokens) {
    for (const token of tokenLine) {
      const start = lineStart + offset;
      const end = start + token.content.length;
      offset += token.content.length;

      const scopes = token.explanation?.[0]?.scopes.map((s) => s.scopeName) ?? [];
      if (!scopes.includes(MESHFOX_COMMENT_SCOPE)) continue;

      commentStart ??= start;
      commentEnd = end;
      if (scopes.includes(MESHFOX_KEYWORD_SCOPE) || scopes.includes(MESHFOX_TAG_END_SCOPE)) {
        out.push({ start, end, className: "mesh-marker-keyword" });
      } else if (scopes.includes(MESHFOX_ATTR_NAME_SCOPE)) {
        out.push({ start, end, className: "mesh-marker-attr-name" });
      } else if (scopes.includes(MESHFOX_ATTR_VALUE_SCOPE)) {
        out.push({ start, end, className: "mesh-marker-attr-value" });
      }
    }
  }
  if (commentStart !== null) {
    out.unshift({ start: commentStart, end: commentEnd, className: "mesh-marker-comment" });
  }
  return out;
}

async function computeCommentMarkerRanges(text: string): Promise<MarkerRange[]> {
  const lines = text.split("\n");
  const jobs: Promise<MarkerRange[]>[] = [];
  let lineStart = 0;
  for (const line of lines) {
    if (MESHFOX_MARKER_LINE_RE.test(line)) {
      jobs.push(computeCommentMarkerRangesForLine(line, lineStart));
    }
    lineStart += line.length + 1; // +1 for the '\n' split() ate
  }
  return (await Promise.all(jobs)).flat();
}

/**
 * Recomputes and applies meshfox's own marker decorations on every content
 * change. Returns a cleanup function — call it (e.g. from an `onMount`
 * effect's own cleanup) when the editor unmounts, so its decorations don't
 * outlive it.
 *
 * `update` is async now (the comment half genuinely awaits Shiki) — a
 * `generation` counter drops a stale result rather than letting a slower,
 * earlier `update()` call clobber a newer one's decorations if two land
 * out of order (rare, but real, once this isn't synchronous start-to-finish
 * anymore).
 */
export function attachMeshfoxMarkers(
  editor: MonacoNS.editor.IStandaloneCodeEditor,
  monaco: typeof MonacoNS,
): () => void {
  let decorationIds: string[] = [];
  let generation = 0;

  const update = async () => {
    const myGeneration = ++generation;
    const model = editor.getModel();
    if (!model) return;
    const text = model.getValue();

    const [commentRanges, fenceRanges] = await Promise.all([
      computeCommentMarkerRanges(text),
      Promise.resolve(computeFenceAttrRanges(text)),
    ]);
    if (myGeneration !== generation) return; // superseded by a newer update mid-flight

    const decorations: MonacoNS.editor.IModelDeltaDecoration[] = [...commentRanges, ...fenceRanges].map((r) => ({
      range: monaco.Range.fromPositions(model.getPositionAt(r.start), model.getPositionAt(r.end)),
      options: { inlineClassName: r.className },
    }));
    decorationIds = editor.deltaDecorations(decorationIds, decorations);
  };

  void update();
  const subscription = editor.onDidChangeModelContent(() => void update());
  return () => {
    subscription.dispose();
    editor.deltaDecorations(decorationIds, []);
  };
}
