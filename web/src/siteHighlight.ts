import { createHighlighterCoreSync } from "@shikijs/core";
import { createJavaScriptRegexEngine } from "@shikijs/engine-javascript";
import githubDark from "@shikijs/themes/github-dark";
import githubLight from "@shikijs/themes/github-light";
import bash from "@shikijs/langs/shellscript";
import diff from "@shikijs/langs/diff";
import ini from "@shikijs/langs/ini";
import javascript from "@shikijs/langs/javascript";
import json from "@shikijs/langs/json";
import jsonc from "@shikijs/langs/jsonc";
import markdown from "@shikijs/langs/markdown";
import python from "@shikijs/langs/python";
import rust from "@shikijs/langs/rust";
import toml from "@shikijs/langs/toml";
import typescript from "@shikijs/langs/typescript";
import yaml from "@shikijs/langs/yaml";
import { meshfoxGrammar, withMeshfoxTokenColors } from "./meshfoxGrammar";
import { starlarkGrammar } from "./starlarkGrammar";

/**
 * Highlighting for the static site (`meshfox static` templates): the same
 * Shiki, theme colors and meshfox/Starlark grammars the canvas view uses
 * (`shiki.ts`), but synchronous, wasm-free (JS regex engine) and with a
 * fixed language set, so it ships as one self-contained classic script a
 * template can drop next to its CSS — see `scripts/build-site-highlight.mjs`.
 * A language outside the set simply stays plain text.
 *
 * Dual theme, like the canvas view: tokens carry `--shiki-light`/
 * `--shiki-dark` CSS variables and each template's own stylesheet picks one
 * (`prefers-color-scheme` in `site-template`, always dark in
 * `site-template-archive`'s dark code cards), so this script knows nothing
 * about a template's light/dark mode. Built once, copied into both.
 */
const THEMES = { light: "github-light", dark: "github-dark" } as const;
const highlighter = createHighlighterCoreSync({
  themes: [withMeshfoxTokenColors(githubLight, false), withMeshfoxTokenColors(githubDark, true)],
  langs: [
    ...bash, ...diff, ...ini, ...javascript, ...json, ...jsonc, ...markdown,
    ...python, ...rust, ...toml, ...typescript, ...yaml,
    meshfoxGrammar, starlarkGrammar,
  ] as never,
  engine: createJavaScriptRegexEngine({ forgiving: true }),
});

const loaded = new Set(highlighter.getLoadedLanguages());

function highlightAll(): void {
  for (const code of document.querySelectorAll<HTMLElement>('pre > code[class*="language-"]')) {
    const lang = /(?:^|\s)language-([^\s]+)/.exec(code.className)?.[1]?.toLowerCase();
    if (!lang || !loaded.has(lang)) continue;
    try {
      const out = document.createElement("div");
      out.innerHTML = highlighter.codeToHtml((code.textContent ?? "").replace(/\n$/, ""), { lang, themes: THEMES, defaultColor: false });
      const highlighted = out.querySelector("code");
      if (!highlighted) continue;
      code.innerHTML = highlighted.innerHTML;
      code.classList.add("highlighted");
    } catch {
      // A grammar the JS engine cannot run leaves that one block plain.
    }
  }
}

if (document.readyState === "loading") document.addEventListener("DOMContentLoaded", highlightAll);
else highlightAll();
