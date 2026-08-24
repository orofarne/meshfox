import type { ThemeRegistrationAny } from "shiki";
import meshfoxGrammarJson from "./grammars/meshfox.tmLanguage.json";

/**
 * meshfox's own `.tmLanguage.json` — a real TextMate injection grammar
 * (`injectionSelector`/`injectTo: ["text.html.markdown"]`) for the
 * `<!-- meshfox:... -->` marker comments SPEC.md's "File structure"/
 * "Variables"/"Cached output" define, layered on top of Shiki's own
 * bundled Markdown grammar rather than replacing it.
 *
 * TODO.canvas.md's "Единая база языков подсветки" — Фаза 4: the earlier
 * hand-rolled approach (two flat regexes, `meshfoxSyntax.ts`/
 * `meshfoxMarkers.ts`) covered both the marker comments *and* a runnable
 * fence's own `name=`/`cache`/`deps=`/... attributes. Only the comment
 * half moved to a real grammar here — a fence's attributes are parsed out
 * into structured data by `fence.ts` and rendered as their own React UI
 * (buttons/badges), never shown as literal highlighted source text in the
 * read-only canvas preview at all, so there's nothing for Shiki to
 * highlight there; `meshfoxMarkers.ts`'s fence-attribute half stays
 * exactly as it was, decorating Monaco's *editable* raw-source view (the
 * one place that text is ever actually on screen as text).
 *
 * A real grammar rather than another regex pass because the same rules
 * now double as Monaco's own tokenizer too (via `@shikijs/monaco`, see
 * `monacoSetup.ts`) — one definition, not two independently-maintained
 * regexes doing the same job for two different rendering paths.
 *
 * Not shared with the TUI (`syntect`) side: `syntect-tmlanguage` can't
 * translate a grammar that itself needs `begin`/`while` (confirmed via its
 * own README — the *real* `markdown.tmLanguage.json` needs exactly that
 * for its own fenced-code/blockquote rules), so meshfox's TUI markdown
 * highlighting stays on `syntect`'s own bundled, differently-sourced
 * Markdown grammar; `source_editor.rs::meshfox_highlights` remains its
 * separate, TUI-native implementation of the same idea.
 */
export const meshfoxGrammar = meshfoxGrammarJson;

/** Literal hex duplicates of `index.css`'s `--accent`/`--syntax-attr`/
 * `--syntax-value` (light and dark, `:root`'s own bare and
 * `prefers-color-scheme: dark` values respectively) — NOT a `var(...)`
 * reference, even though that would keep one definition instead of two.
 * Confirmed the hard way: Shiki's own HTML output writes a token's
 * `foreground` string verbatim as CSS (`style="color:..."`), so
 * `var(--accent)` renders correctly there — but `@shikijs/monaco` doesn't
 * hand that string to the DOM at all. It converts each theme's
 * `tokenColors` into Monaco's own `IStandaloneThemeData.rules[].foreground`
 * (real hex Monaco parses itself, see `monaco.editor.defineTheme`) and
 * then, per token, reverse-looks-up which rule produced a given resolved
 * *color* to find its scope name — a `var(...)` string never resolves to
 * a real color at that layer, so the lookup silently misses and the token
 * falls back to whatever Monaco's base theme already had (confirmed
 * empirically: Monaco's own rendering was unaffected by a `var(...)`
 * foreground, while Shiki's plain HTML preview picked it up fine). Two
 * literal palettes is the actual fix — if `index.css`'s own values change,
 * these need updating too. */
const MESHFOX_LIGHT_HEX = { accent: "#ea580c", attr: "#96660a", value: "#1b7a43" };
const MESHFOX_DARK_HEX = { accent: "#ff6e15", attr: "#d8b656", value: "#6fcf97" };

function meshfoxTokenColors(hex: {
  accent: string;
  attr: string;
  value: string;
}): NonNullable<ThemeRegistrationAny["tokenColors"]> {
  return [
    { scope: "keyword.other.meshfox", settings: { foreground: hex.accent, fontStyle: "bold" } },
    { scope: "entity.other.attribute-name.meshfox", settings: { foreground: hex.attr } },
    { scope: "string.unquoted.meshfox", settings: { foreground: hex.value } },
  ];
}

/**
 * Returns a copy of `theme` with meshfox's own token-color rules appended
 * — same `name` (so it still resolves under its original bundled name,
 * e.g. `"github-light"`), just with a few extra `tokenColors` entries
 * Shiki's bundled themes obviously don't ship with. Doesn't mutate `theme`
 * itself, since Shiki may cache/reuse the object it's handed. `isDark`
 * picks which of the two literal palettes above to use — callers pass
 * `THEMES.light`'s own theme with `false` and `THEMES.dark`'s with `true`.
 */
export function withMeshfoxTokenColors(theme: ThemeRegistrationAny, isDark: boolean): ThemeRegistrationAny {
  return {
    ...theme,
    tokenColors: [...(theme.tokenColors ?? []), ...meshfoxTokenColors(isDark ? MESHFOX_DARK_HEX : MESHFOX_LIGHT_HEX)],
  };
}
