# meshfox for VS Code

Opens `.canvas.md` files as an interactive node canvas — the same web UI
`meshfox view` serves in a browser tab, embedded directly in an editor tab
instead. Requires the `meshfox` binary (see the repo root README for
install instructions). Resolved from PATH first, then `scripts/install.sh`'s
own default install location (`~/.local/bin`) and a couple of other common
ones (`/opt/homebrew/bin`, `/usr/local/bin`) if that fails — GUI apps
launched from Finder/Dock/Spotlight don't always inherit a login shell's
PATH, so this covers the common case of a real install that VS Code just
can't see. Set `meshfox.executablePath` in settings to override both.

More about meshfox itself — the format, the CLI, the TUI and the MCP
server — at [meshfox.orofarne.net](https://meshfox.orofarne.net/).

On the [VS Code Marketplace](https://marketplace.visualstudio.com/items?itemName=Orofarne.meshfox-vscode)
as `Orofarne.meshfox-vscode` — install from there (Extensions view, search
"meshfox", or `ext install Orofarne.meshfox-vscode`) rather than building
it yourself. (Building and testing the extension: `README.canvas.md`
next to this file.)

To start a new canvas, run **meshfox: New canvas** from the Command Palette
(or just create an empty `*.canvas.md` file yourself — meshfox fills in the
starting structure the first time it opens a blank file; a non-empty file
without a `#` root heading is still rejected).

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
with `ENOENT`), an error notification offers **Install** — same as running
the **"meshfox: Install"** command directly — which types the same install
one-liner the root README documents into an integrated terminal, Enter
left to you: never runs it on your behalf.
