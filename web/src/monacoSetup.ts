import EditorWorker from "monaco-editor/esm/vs/editor/editor.worker.js?worker";
// Type-only — erased at compile time, doesn't pull `monaco-editor` itself
// into this module's own (eagerly-loaded) chunk the way a value import
// would.
import type * as Monaco from "monaco-editor";
import { lazy } from "react";
import { getHighlighter } from "./shiki";

/**
 * `@monaco-editor/react`'s `Editor` component, loaded lazily — mirrors
 * `ensureMonacoConfigured` below's own `import("@monaco-editor/react")`.
 * Both `NodeTextEditor.tsx` and `CanvasSourceEditor.tsx` used to `import
 * Editor from "@monaco-editor/react"` directly; a *static* import there
 * defeats this file's own dynamic one (Vite's `INEFFECTIVE_DYNAMIC_IMPORT`
 * warning) and bundles the module into the main chunk regardless, which is
 * exactly what the laziness above is trying to avoid. Both editors already
 * gate mounting on `useMonacoReady()`, so by the time `<LazyEditor>` itself
 * renders, `ensureMonacoConfigured()` has already resolved this same
 * dynamic import — the `<Suspense>` boundary each caller wraps it in only
 * ever needs to cover a single already-cached microtask, never a real
 * loading state.
 */
export const LazyEditor = lazy(() => import("@monaco-editor/react"));

/**
 * Self-hosts Monaco — without this, `@monaco-editor/react`'s default
 * loader fetches Monaco from a CDN (`cdn.jsdelivr.net`) at runtime, which
 * meshfox's local-first, embedded-binary web UI can't rely on (no
 * network, no CDN). `loader.config` points it at the npm-installed
 * `monaco-editor` package (bundled by Vite) instead.
 *
 * Deliberately lazy — call this (and await it) right before rendering the
 * first `<Editor>` (see `NodeTextEditor.tsx`/`CanvasSourceEditor.tsx`'s
 * own `monacoReady` state), not eagerly at app startup. `monaco-editor`
 * itself is multi-megabyte; most page loads only ever look at the
 * read-only canvas view (Shiki-highlighted, see `shiki.ts`) and never
 * open either editable surface at all — an eager top-level
 * `import * as monaco from "monaco-editor"` bundled the whole thing into
 * the main chunk (~4.7 MB before this fix, confirmed via `npm run build`'s
 * own chunk-size output) for every visitor to pay for regardless.
 *
 * Only the base editor worker is wired up — meshfox's two Monaco editors
 * only ever edit Markdown, which Monaco tokenizes without a dedicated
 * language worker; the base `editor.worker` still covers core services
 * every language needs (find, folding, bracket matching). If a future
 * language needs its own worker (JSON/TS-style IntelliSense), add it to
 * `getWorker` below rather than pulling in a general-purpose Vite Monaco
 * plugin for a single extra case.
 *
 * `monaco-editor` itself is pinned to `^0.53.0` in package.json, not
 * latest — 0.54.0 added a `marked`/`dompurify` dependency (for rendering
 * markdown-formatted hover/completion widgets neither editor here ever
 * shows), and every published dompurify version pulled in through 0.56.0
 * has real, unpatched XSS advisories (`npm audit`, GHSA-c2j3-45gr-mqc4 and
 * others) — 0.53.0 predates that dependency entirely, so it's not just
 * "unaffected," the vulnerable package isn't installed at all. Re-check
 * `npm audit` before bumping past 0.53.x.
 *
 * Also registers `shiki.ts`'s own shared highlighter into Monaco via
 * `@shikijs/monaco`'s `shikiToMonaco` — real TextMate tokenization/colors
 * for `markdown` specifically (the one language both Monaco editors here
 * ever set as their buffer's own language), the same base Markdown
 * grammar/theme the read-only canvas preview uses, not Monaco's own
 * built-in (differently-sourced) one.
 *
 * `shikiToMonaco` does **not** give Monaco the same overall language set
 * Shiki has — confirmed directly (logged `highlighter.getLoadedLanguages()`
 * against `monaco.languages.getLanguages()` in a real running app): it
 * only registers a tokenizer for a language that's *both* loaded on the
 * shared highlighter *and* already one of Monaco's own ~90 built-in
 * language ids (`@shikijs/monaco`'s own source: `if
 * (monacoLanguageIds.has(lang)) monaco.languages.setTokensProvider(...)`).
 * `starlarkGrammar.ts`'s "Starlark" is loaded on the highlighter but isn't
 * one of Monaco's built-in ids, so it never gets registered at all — not a
 * bug, just irrelevant in practice, since neither Monaco editor here ever
 * sets its buffer language to anything but `"markdown"` (a fenced block's
 * *own* language only matters to the read-only preview's Shiki path, which
 * doesn't go through Monaco at all).
 *
 * Also doesn't cover `meshfoxGrammar.ts`'s own `<!-- meshfox:... -->`
 * injection grammar, even though it's loaded on this same highlighter —
 * confirmed directly (comparing raw token counts/scopes from `highlighter
 * .getLanguage("markdown").tokenizeLine2(...)`, the exact call
 * `shikiToMonaco`'s own tokenizer provider makes, against
 * `highlighter.codeToHtml(...)`'s output for the same line): the grammar
 * object `getLanguage()` returns resolves its own `_injections` to an
 * empty list regardless of load order/warm-up, while `codeToHtml`'s
 * internal (different, injection-aware) tokenization path colors the same
 * text correctly. A real gap in `@shikijs/monaco`'s (or Shiki's own
 * `getLanguage()`) support for injection grammars, not a setup mistake
 * here — but not a dead end either: `meshfoxMarkers.ts` gets the injection
 * into Monaco anyway, just via a different path (its own dedicated
 * highlighter's `codeToTokens(...)`, the one that *does* resolve
 * injections, called directly and turned into `deltaDecorations` itself)
 * rather than through `shikiToMonaco`'s own `TokensProvider` registration.
 * It deliberately does *not* reuse this file's shared highlighter for that
 * — see `meshfoxMarkers.ts`'s own top comment for why sharing it there
 * would work at first and then silently break. Only the fence-attribute
 * half of `meshfoxMarkers.ts` is still hand-rolled regex — that one has no
 * grammar-driven path at all, on *either* side (see its own doc comment for
 * why).
 */
let configured: Promise<void> | null = null;

export function ensureMonacoConfigured(): Promise<void> {
  if (!configured) {
    configured = Promise.all([
      import("monaco-editor"),
      import("@monaco-editor/react"),
      import("@shikijs/monaco"),
      getHighlighter(),
    ]).then(async ([monaco, { loader }, { shikiToMonaco }, highlighter]) => {
      (self as typeof self & { MonacoEnvironment: Monaco.Environment }).MonacoEnvironment = {
        getWorker() {
          return new EditorWorker();
        },
      };
      loader.config({ monaco });
      shikiToMonaco(highlighter, monaco);
    });
  }
  return configured;
}
