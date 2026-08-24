import { bundledLanguages, bundledThemes, createHighlighter, type Highlighter } from "shiki";
import { fetchSyntaxGrammar, fetchSyntaxList } from "./api";
import { meshfoxGrammar, withMeshfoxTokenColors } from "./meshfoxGrammar";
import { starlarkGrammar } from "./starlarkGrammar";

/**
 * Read-only code highlighting for the canvas view (`MeshNode.tsx`'s fenced
 * blocks and `file`-node previews) — Shiki rather than a live editor,
 * chosen specifically for this many-simultaneous-instances case: it
 * renders static HTML per block instead of spinning up a live editor per
 * instance. (The two *editable* surfaces, `NodeTextEditor`/
 * `CanvasSourceEditor`, use Monaco — see `monacoSetup.ts`, which registers
 * this same highlighter's themes/tokenizer into Monaco via
 * `@shikijs/monaco` so both sides render identically, not just similarly.)
 *
 * One shared highlighter instance for the whole app (module-level, not
 * per-node) — creating it is not free, and every node's own preview wants
 * the same language/theme set anyway. Exported (not just used internally)
 * so `monacoSetup.ts` can register the exact same instance into Monaco,
 * rather than a second, independently-loaded one.
 */
export const THEMES = { light: "github-light", dark: "github-dark" } as const;

let highlighterPromise: Promise<Highlighter> | null = null;
export function getHighlighter(): Promise<Highlighter> {
  if (!highlighterPromise) {
    highlighterPromise = (async () => {
      // Extended (not the bare bundled themes) so `keyword.other.meshfox`/
      // `entity.other.attribute-name.meshfox`/`string.unquoted.meshfox`
      // (meshfoxGrammar.ts's own injection grammar) resolve to real colors
      // instead of just falling through to each theme's plain default
      // foreground. `meshfoxGrammar` itself is loaded eagerly here (not
      // lazily like a fenced block's own language, via
      // `ensureLanguageLoaded`) — an injection grammar with no host
      // language loaded yet to inject into wouldn't do anything anyway,
      // and every node's `markdown` segment needs it every time.
      // `starlarkGrammar` is eager too, simply because it's meshfox's own
      // bundled pool (`grammars/README.md`) rather than an on-demand
      // custom grammar — no reason to defer loading something that ships
      // with the app itself.
      const [light, dark] = await Promise.all([bundledThemes[THEMES.light](), bundledThemes[THEMES.dark]()]);
      return createHighlighter({
        themes: [withMeshfoxTokenColors(light.default, false), withMeshfoxTokenColors(dark.default, true)],
        langs: ["markdown", meshfoxGrammar, starlarkGrammar],
      });
    })();
  }
  return highlighterPromise;
}

/** `lang`s already attempted via the custom-grammar fallback (below) —
 * tried at most once per language per page load, success or failure,
 * rather than re-fetching `/api/syntax` on every single code block that
 * uses an unrecognized language. */
const customGrammarAttempts = new Map<string, Promise<boolean>>();

/**
 * `filename` (e.g. `"elixir.tmLanguage.json"`) matches `lang` (e.g.
 * `"elixir"`) once its own grammar-file suffix is stripped — the same
 * bare-name convention a fenced code block's own info string already uses.
 */
function grammarNameMatches(filename: string, lang: string): boolean {
  const stem = filename.replace(/\.tmLanguage\.json$/i, "").replace(/\.sublime-syntax$/i, "");
  return stem.toLowerCase() === lang.toLowerCase();
}

/**
 * Registers `lang` into the shared highlighter if it isn't loaded yet —
 * Shiki's own large bundled language set first (covers the common case,
 * including languages meshfox never used to support, like Elixir), then
 * the shared local/global custom-grammar repository (`GET /api/syntax`,
 * see `meshfox_core::syntax_dirs`) as a fallback for anything Shiki
 * doesn't bundle. Never throws — a language nobody has a grammar for just
 * means the caller renders plain, unhighlighted text instead.
 */
async function ensureLanguageLoaded(hl: Highlighter, lang: string): Promise<boolean> {
  if (hl.getLoadedLanguages().includes(lang)) return true;

  if (lang in bundledLanguages) {
    try {
      await hl.loadLanguage(lang as keyof typeof bundledLanguages);
      return true;
    } catch {
      return false;
    }
  }

  let attempt = customGrammarAttempts.get(lang);
  if (!attempt) {
    attempt = (async () => {
      try {
        const entries = await fetchSyntaxList();
        const match = entries.find((e) => grammarNameMatches(e.name, lang));
        if (!match) return false;
        const raw = await fetchSyntaxGrammar(match.name);
        // `.sublime-syntax` (YAML) isn't something this client-side path
        // can parse — only `.tmLanguage.json` grammars round-trip through
        // `JSON.parse` directly into the shape Shiki's `loadLanguage`
        // wants.
        if (!match.name.toLowerCase().endsWith(".tmlanguage.json")) return false;
        const grammar = JSON.parse(raw);
        await hl.loadLanguage(grammar);
        return true;
      } catch {
        return false;
      }
    })();
    customGrammarAttempts.set(lang, attempt);
  }
  return attempt;
}

/**
 * Renders `code` as syntax-highlighted HTML for `lang` (falling back to
 * plain, unhighlighted `text` when nothing can highlight it) — dual-theme
 * (`--shiki-light`/`--shiki-dark` CSS variables, `defaultColor: false`),
 * so the same markup adapts to the app's own light/dark toggle purely via
 * CSS (see index.css's `.mesh-shiki` rules) without re-rendering.
 */
export async function highlightToHtml(code: string, lang: string): Promise<string> {
  const hl = await getHighlighter();
  const loaded = await ensureLanguageLoaded(hl, lang);
  return hl.codeToHtml(code, {
    lang: loaded ? lang : "text",
    themes: THEMES,
    defaultColor: false,
  });
}
