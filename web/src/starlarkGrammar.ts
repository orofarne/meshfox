import starlarkGrammarJson from "../../grammars/starlark.tmLanguage.json";
import type { LanguageInput } from "shiki";

/**
 * meshfox's own bundled Starlark grammar (`grammars/README.md` at the repo
 * root) — used for ` ```starlark constraint ` fences (SPEC.md's
 * "Constraint fences"), a language neither Shiki nor `syntect` bundles by
 * default. The *canonical*, unmodified upstream file — the TUI side loads
 * a `syntect`-compatible rewrite instead (`grammars/tui/starlark.tmLanguage.json`,
 * see its own README for why); `vscode-textmate` doesn't share whatever
 * limitation made that rewrite necessary, so this side just uses the
 * original directly.
 *
 * `aliases: ["starlark"]` is added here, not baked into the JSON file
 * itself: Shiki registers a grammar under its own (case-sensitive)
 * `"name"` field — `"Starlark"` upstream — but a fence's info string
 * always writes the lowercase `starlark` (confirmed directly: without
 * this, `codeToHtml(..., { lang: "starlark" })` throws `Language
 * 'starlark' not found` even with the grammar already loaded).
 */
export const starlarkGrammar: LanguageInput = {
  ...(starlarkGrammarJson as object),
  aliases: ["starlark"],
} as LanguageInput;
