import { randomBytes } from "crypto";

const shell = (body: string, csp: string) => `<!DOCTYPE html>
<html>
<head>
<meta charset="UTF-8">
<meta http-equiv="Content-Security-Policy" content="${csp}">
<style>
  html, body { height: 100%; margin: 0; padding: 0; }
  body {
    font-family: var(--vscode-font-family, sans-serif);
    color: var(--vscode-foreground);
    background: var(--vscode-editor-background);
    display: flex;
    align-items: center;
    justify-content: center;
  }
  pre { white-space: pre-wrap; max-width: 60ch; }
</style>
</head>
<body>${body}</body>
</html>`;

export function loadingHtml(): string {
  return shell("<p>Starting meshfox…</p>", "default-src 'none'; style-src 'unsafe-inline';");
}

export function errorHtml(message: string): string {
  return shell(`<pre>meshfox failed to start:\n\n${escapeHtml(message)}</pre>`, "default-src 'none'; style-src 'unsafe-inline';");
}

/**
 * Turns `indexHtml` (meshfox's own served `index.html`, fetched as-is from
 * the running worker) into the webview's *own* top-level document, instead
 * of nesting it in an `<iframe>` the way an earlier version did. That
 * nesting was the actual cause of copy/paste and context-menu not working
 * inside the canvas — VS Code webviews mostly work fine for both, but a
 * *nested cross-origin iframe* inside one is a long-standing, still-open
 * upstream limitation (see editors/vscode/README.md's own "Known
 * limitations" section for the specific issues). Making meshfox's app the
 * webview's own content sidesteps that whole bug class rather than
 * chasing its symptoms.
 *
 * `index.html` (Vite's build output) uses root-absolute asset paths
 * (`/assets/...`) that would otherwise resolve against the webview's own
 * `vscode-webview://` origin once this string becomes its document — a
 * `<base href="${baseUrl}">` makes every one of them (and every relative
 * `fetch()`/`XMLHttpRequest` call the app itself makes, e.g. hitting
 * `/api/...` — `<base>` affects a document's base URL for script-driven
 * requests too, not just markup) resolve against the real server instead,
 * with no per-asset rewriting needed. The server's own CORS is already
 * permissive (`CorsLayer::permissive()`), so those cross-origin requests
 * from a `vscode-webview://` page succeed without any change on that side.
 *
 * `'wasm-unsafe-eval'` on `script-src` is load-bearing, not decorative:
 * the app's own code-block syntax highlighting (`web/src/shiki.ts`) uses
 * Shiki's default Oniguruma engine, which is WASM
 * (`WebAssembly.instantiate`, confirmed present in the shipped bundle) —
 * without this, `WebAssembly.instantiate` is a CSP violation Chromium
 * throws on rather than falls back from, `getHighlighter()`'s promise
 * rejects, and every code block quietly renders as plain unhighlighted
 * text instead of erroring visibly. Same CSP a plain browser tab never
 * had to satisfy at all (no CSP there) — this only became a real
 * constraint once the app became the webview's own top-level document
 * instead of an unrestricted nested iframe.
 *
 * The injected `<style>` reset undoes another VS-Code-webview-only
 * default: VS Code injects its own baseline stylesheet into every webview,
 * including a plain-element `code { background-color: var(--vscode-
 * textPreformat-background); ... }` rule meant for a short inline `` `code`
 * `` span in a markdown-preview-style webview. Shiki wraps a highlighted
 * block in `<pre class="shiki"><code>...</code></pre>`; the app's own CSS
 * (`.mesh-shiki .shiki { background-color: var(--shiki-dark-bg) }`) paints
 * the *`<pre>`'s* background, not `<code>`'s own, so VS Code's default
 * background paints on top of/behind the token colors instead of being
 * overridden by it — each individual token's *text* color still wins on
 * specificity (`.mesh-shiki .shiki span` beats a bare `code` selector),
 * so this reads as "present but washed-out" rather than "no color at
 * all". Confirmed live via the webview's own dev tools (Cmd Palette →
 * "Developer: Open Webview Developer Tools") rather than guessed — a
 * plain browser tab never had this rule to fight in the first place.
 *
 * Scoped to a bare `code` selector, not just `.shiki code` — a live/cached
 * run's own stdout/stderr and status-message panels (`MeshNode.tsx`) go
 * through their own plain `<pre><code>` (ANSI-colored text, e.g.
 * `<AnsiText/>`, never actual syntax to tokenize), bypassing Shiki
 * entirely, so `.shiki code` alone left VS Code's default background
 * fighting the exact same way there. `web/src/index.css` has no bare
 * `code {}` rule of its own for this override to fight — anywhere the app
 * *does* want its own code styling, that's a more specific selector than
 * a bare element selector and still wins over this reset regardless.
 */
const CODE_BLOCK_BACKGROUND_RESET = `<style>
code {
  background-color: transparent !important;
  border-radius: 0 !important;
  font-family: inherit !important;
  color: inherit !important;
}
</style>`;

export function canvasAppHtml(indexHtml: string, baseUrl: string, fragment: string | undefined): string {
  const origin = new URL(baseUrl).origin;
  const nonce = randomBytes(16).toString("base64");
  const csp =
    `default-src 'none'; script-src ${origin} 'wasm-unsafe-eval' 'nonce-${nonce}'; ` +
    `style-src ${origin} 'unsafe-inline'; img-src ${origin} data:; font-src ${origin} data:; ` +
    `connect-src ${origin};`;
  // A classic (non-`type="module"`) inline script runs synchronously as
  // the parser reaches it, before any `type="module"` script later in the
  // document (those are deferred by default) — so this always sets the
  // hash before the app's own bundle mounts and reads it, regardless of
  // where `index.html` places its own script tag. Needs the same nonce as
  // the CSP above — `script-src` has no `'unsafe-inline'`, deliberately,
  // so only this one specific injected script (not some hypothetical
  // future inline script from elsewhere) is allowed to run.
  const fragmentScript = fragment
    ? `<script nonce="${nonce}">history.replaceState(null, "", "#" + ${JSON.stringify(fragment)});</script>\n`
    : "";
  const inject =
    `<meta http-equiv="Content-Security-Policy" content="${escapeHtml(csp)}">\n` +
    `<base href="${escapeHtml(baseUrl)}">\n` +
    fragmentScript +
    CODE_BLOCK_BACKGROUND_RESET +
    "\n";
  return indexHtml.replace(/<head[^>]*>/i, (headTag) => `${headTag}\n${inject}`);
}

function escapeHtml(s: string): string {
  return s
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;");
}
