# meshfox for VS Code

Opens `.canvas.md` files as an interactive node canvas — the same web UI
`meshfox view` serves in a browser tab, embedded directly in an editor tab
instead. Requires the `meshfox` binary (see the repo root README for
install instructions); set `meshfox.executablePath` in settings if it's
not on `PATH`.

`*.canvas.md` opens as a canvas automatically (it's the default editor for
that pattern). A plain `.md` file that's a canvas without the suffix — a
marker-carrying file like this project's own README.md — doesn't
auto-open as one (VS Code has no reliable way to sniff a file's content
before opening it, and an earlier attempt to fake that by opening the
canvas editor and immediately bailing out for non-canvases broke VS
Code's own tab bookkeeping). Instead, this extension is also registered as
an *optional* editor for any `.md`, so it always shows up in "Open
With..." (right-click a file → **Open With...**, or the **"Open as
meshfox canvas"** command/Explorer context-menu entry this extension
adds). Pick it once for a file like README.md, and if you want that to
stick permanently, add it to `workbench.editorAssociations` in settings —
either your own user settings, or this repo's own `.vscode/settings.json`
if you want it for every contributor:

```jsonc
"workbench.editorAssociations": {
  "README.md": "meshfox.canvasEditorAny"
}
```

Read-only from VS Code's own perspective, same as the browser UI: click
"Edit" inside the canvas itself to unlock dragging/resizing/saving layout
and persisting a cached block's output — see the root README's "Concept".

**Platform support: macOS/Linux only for now.** The extension talks to
`meshfox view --watcher-socket <path>` over a Unix domain socket, the same
protocol `crates/cli/src/watcher.rs` speaks — a known gap tracked in the
repo's `TODO.canvas.md` under "Полноценная поддержка Windows".

## Syntax highlighting for raw `.md`/`.canvas.md` text

Wherever the raw file is shown as text rather than through the canvas
editor — git diffs/blame, "Open With... → Text Editor", any `.md` without
a canvas marker — an injection grammar (`syntaxes/meshfox-marker.injection
.json`) highlights meshfox's own bookkeeping comments (`<!-- meshfox:node
id="..." ... -->`, `meshfox:edge`, `meshfox:var`, `meshfox:option`,
`meshfox:tag-color`, `meshfox:output`/`/meshfox:output`,
`meshfox:comment`/`/meshfox:comment`) — the construct name as a keyword,
each `key="value"`/bare-flag attribute per SPEC.md's own `attr-list`
grammar. It reuses standard TextMate scope names (`entity.other.attribute-
name`, `string.quoted.double`, `keyword.control`, ...) so it picks up
whatever colors the user's current theme already assigns those, no custom
theme needed. Ordinary code fences (` ```bash `, etc.) already get
embedded-language highlighting from VS Code's own bundled Markdown grammar
with no help needed here.

Deliberately scoped to *just* the marker comments for now — a fenced
runnable block's own trailing attributes (` ```bash name="..." deps="..."
cache `) aren't highlighted yet. Getting that right without risking the
embedded-language highlighting *inside* the fence body (a real regression,
worse than the current gap) needs live verification of exactly how VS
Code's bundled Markdown grammar tokenizes a fence's opening line, which
this repo's history is a good reminder not to guess at (see `git log` for
the custom-editor bail-out that broke tab state — same lesson: TextMate/VS
Code API interactions that look right on paper still need an actual F5
check before trusting them).

## Keeping `meshfox` itself up to date

Once a day (throttled via `globalState`, not on every window), the
extension checks GitHub's Releases API for a newer `meshfox` than the one
`meshfox.executablePath` resolves to and — only for a release build (a
`v1.2.3`-style version; a local/dev build has nothing to compare against,
same as `meshfox check-updates`'s own no-op case) — shows a toast with an
**Update** button if one exists. Nothing is downloaded or installed by
that check itself; clicking **Update** (or running the **"meshfox: Check
for Updates"** command directly) asks for confirmation once more, then
runs `meshfox check-updates -y` — the existing CLI command
(`self_update`-based, downloads from the same GitHub releases) that
actually replaces the binary, reused rather than reimplemented here.

If the configured executable can't be found at all (spawning it fails
with `ENOENT`), an error notification offers to copy the same install
one-liner the root README documents — never runs it for you.

## How it works

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

## Developing

```sh
cd editors/vscode
npm install
```

Press **F5** (or Run → "Run meshfox extension") to launch an Extension
Development Host with the extension loaded — this compiles automatically
via the `npm: compile` pre-launch task. `npm run watch` recompiles on save
if you'd rather keep a dev host open across edits (reload it with
`Cmd/Ctrl+R` after each change instead of relaunching).

## Packaging (no Marketplace publish needed)

```sh
npx @vscode/vsce package
```

produces `meshfox-vscode-<version>.vsix`, installable with:

```sh
code --install-extension meshfox-vscode-<version>.vsix
```

or via the Extensions view → "..." → "Install from VSIX". `vsce package`
needs no publisher account/token — only `vsce publish` does.
