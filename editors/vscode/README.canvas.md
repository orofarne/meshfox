<!-- meshfox:canvas -->
# meshfox for VS Code — development
<!-- meshfox:node id="root" -->

<!-- meshfox:comment -->
> This is a [meshfox](https://meshfox.orofarne.net/) document — open it with `meshfox view` (or `meshfox tui`) for the interactive canvas. This note is only visible here, in a plain Markdown viewer.
<!-- /meshfox:comment -->

Everything about working on the VS Code extension in `editors/vscode/`: how it works, building it, and its end-to-end tests. The Marketplace page (the user-facing description) is `README.md`, shown in the first node below; it is kept separate because `vsce` publishes it as the extension's store page. Every runnable block here runs in this directory (`editors/vscode/`): `meshfox editors/vscode/README.canvas.md run <node> <block>`, or the run button in `meshfox tui`/`view`.

## Marketplace README
<!-- meshfox:node id="marketplace-readme" type="file" -->

[README.md](./README.md)

## How it works
<!-- meshfox:node id="how-it-works" -->

The extension acts as its own private coordinator (see `src/coordinator.ts`)
— it binds a Unix socket, and for each file opened through either
`viewType` spawns `meshfox view <path> --port 0 --watcher-socket <that
socket>`. Once the worker reports its bound port over the socket, the
editor tab's webview fetches that worker's own `index.html` and becomes it
directly (`src/canvasEditorProvider.ts`/`canvasHtml.ts`) — not an
`<iframe>` pointed at it. A `<base href="http://127.0.0.1:<port>/">`
injected into the fetched page's `<head>` makes every one of its
root-absolute asset/`fetch()` references resolve against that server
instead of the webview's own origin, so the app runs exactly as it does in
a real browser tab, just hosted directly in the tab's own webview document.
Closing the tab kills that worker; a "↗ open" link to another canvas
inside the UI opens (or focuses) another VS Code tab the same way.

Being the webview's own top-level document means it's also subject to two
things a real browser tab never has to deal with, both worked around in
`canvasAppHtml` (`canvasHtml.ts`): the injected CSP needs `'wasm-unsafe-
eval'` on `script-src` (Shiki's default syntax-highlighting engine is
WASM — without it, every code block quietly renders unhighlighted rather
than erroring), and a small `<style>` reset undoes VS Code's own default
webview stylesheet, which paints a `background-color` onto every bare
`<code>` element (meant for a short inline `` `snippet` `` in a
markdown-preview-style webview) — Shiki's `<pre class="shiki"><code>`
wrapping means that paints over/behind a highlighted block's own intended
background and washes out its colors otherwise.

This wasn't the original design — an earlier version *did* use an
`<iframe>`, and copy/paste plus the right-click context menu silently
didn't work inside it. That turned out to be a long-standing, still-open
upstream VS Code limitation specific to a *nested cross-origin iframe*
inside a webview, not to a webview's own top-level content (see
[microsoft/vscode#180234](https://github.com/microsoft/vscode/issues/180234),
[#68799](https://github.com/microsoft/vscode/issues/68799),
[#121580](https://github.com/microsoft/vscode/issues/121580)) — switching
to loading the app as the webview's own document sidesteps the whole bug
class rather than working around its symptoms. If anything still feels off
compared to a real browser tab, the **"meshfox: Open in Browser"** command
(also a button in the editor tab's own title bar) opens the exact same
already-running worker in a real browser tab instead, which is never
subject to any webview limitation at all.

## Build
<!-- meshfox:node id="build" -->

Install the dependencies and compile the extension (`tsc` into `out/`, which is what VS Code loads):

```sh name="install"
npm install
```

```sh name="compile" deps="install" default
npm run compile
```

`npm run watch` recompiles on save if you would rather keep a dev host open across edits (reload it with `Cmd/Ctrl+R` after each change instead of relaunching). To try the extension, press **F5** (Run → "Run meshfox extension") to launch an Extension Development Host with it loaded — this compiles automatically via the `npm: compile` pre-launch task.

## Local .vsix
<!-- meshfox:node id="local-vsix" -->

For testing a dev build without waiting on a Marketplace release, package it — this produces `meshfox-vscode-<version>.vsix`. `vsce package` needs no publisher account or token; that is only for publishing a new version to the Marketplace.

```sh name="package" deps="build/install"
npx @vscode/vsce package
```

Install the newest one into your VS Code (or use the Extensions view → "..." → "Install from VSIX"):

```sh name="install-vsix" deps="package"
code --install-extension "$(ls -t meshfox-vscode-*.vsix | head -1)"
```

## End-to-end tests
<!-- meshfox:node id="e2e-tests" -->

`editors/vscode/e2e/` is a separate [Playwright](https://playwright.dev) suite from the browser one in `web/e2e` — it drives a real, already-installed VS Code via Playwright's Electron support (`_electron.launch()`), not headless Chromium, because the bug it exists to catch (TODO.canvas.md: "VSCode: вставка текста (Cmd+V и контекстное меню) в редактор ноды не работает") only reproduces inside a real VS Code window: a genuinely trusted Cmd+V against `web/src/NodeTextEditor.tsx`'s Monaco editor fires a normal `paste` DOM event in any real browser tab (confirmed directly, and covered by `web/e2e/copy-paste.spec.ts`'s own round-trip test) but never does inside VS Code's own webview — a long-standing, still-open class of upstream VS Code/Electron limitation (nested-iframe focus resolution for a native accelerator-driven paste command; see [microsoft/vscode#129178](https://github.com/microsoft/vscode/issues/129178)), not a bug in this app. `web/src/textPasteFallback.ts` works around it (falls back to `navigator.clipboard.readText()` when a real `paste` event doesn't show up within 250ms of a Ctrl/Cmd+V keydown) — this suite is what actually proves that fallback works, end to end, in the one environment that matters for it. Confirmed the same investigation's way: the node body editor's own title field and NodeSettings' ID field (both plain `<input>`s, no `EditContext` involved) already paste correctly via real Cmd+V in VS Code with no fallback needed at all — the gap is specific to Monaco's `EditContext` input surface, not general to every input in the app.

Heavier and more fragile than the browser suite on purpose (see `playwright.config.ts`'s own doc comment): a real VS Code + extension-host + meshfox-worker launch costs ~15-20s per file, needs a real VS Code install on the machine running it (`VSCODE_ELECTRON_PATH` env var overrides the default macOS/Linux install-path guesses in `helpers.ts`), and it needs a real VS Code on the machine, so it runs from the project's `full-check` (through a `meshfox` fence in the root README) but not in a plain CI job. Run it by hand when touching real-VS-Code-specific behavior (`editors/vscode/`, or anything paste/clipboard-related in the Monaco editors).

The suite does not build what it drives, so `build-app` does that first: the web UI, then the `meshfox` binary (`rust-embed` bakes `web/dist` into the server crate, so the web build comes first) and the extension itself.

```sh name="e2e-prep"
cd e2e
npm install
```

```sh name="build-app" deps="build/compile"
export CARGO_TERM_COLOR=always
(cd ../../web && npm run build)
(cd ../.. && cargo build -p meshfox-cli)
```

```sh name="e2e-run" deps="e2e-prep,build-app" default
export CARGO_TERM_COLOR=always FORCE_COLOR=1
cd e2e
npm test
```
