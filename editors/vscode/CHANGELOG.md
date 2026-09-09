# Changelog

## 0.2.2

- Fixed: running a `tty` block inside the canvas webview failed with a
  Content Security Policy error instead of connecting — the tty
  WebSocket was built from `location.host`, which inside a real VS Code
  webview is the webview's own `vscode-webview://` origin, not the
  meshfox server; it now uses `document.baseURI`, which follows the
  `<base href>` this extension injects to point at the real server. The
  webview's own CSP also only allowed `http(s)` in `connect-src`, which
  doesn't cover `ws` connections to the same host — it now allows both.

## 0.2.1

- Fixed: pasting text into a node's body editor (or the whole-document
  Source editor) via Ctrl/Cmd+V did nothing inside a real VS Code window,
  despite working fine in a plain browser tab — a real Ctrl/Cmd+V there
  never delivered a native `paste` event to Monaco's input surface at all
  (a VS Code/Electron limitation around its own nested webview iframe, not
  a bug in this extension); it's now detected and worked around by reading
  the clipboard directly when that happens.
- The same editors' right-click menu (Cut/Copy/Paste) is now this
  extension's own instead of the OS's native one — that native menu's own
  Paste suffered the identical gap, and there was no way to keep only it
  disabled while leaving Cut/Copy native.

## 0.2.0

- New **"meshfox: Install"** command — types the same install one-liner
  the root README documents into an integrated terminal (Enter is still
  yours to press) instead of only offering it as a copy-to-clipboard
  string from an error prompt.
- A "↗ open" on a plain (non-canvas) file node now opens a regular VS Code
  editor tab instead of shelling out to the OS's default application for
  it — the coordinator decides how a plain file gets opened, and this
  extension's own answer is "inside the editor".
- The `meshfox` binary is now found even when it isn't on VS Code's own
  PATH (a GUI app launched from Finder/Dock/Spotlight doesn't always
  inherit a login shell's) — falls back to checking `scripts/install.sh`'s
  own default install location (`~/.local/bin`) and a couple of other
  common ones (`/opt/homebrew/bin`, `/usr/local/bin`) before giving up.
  An explicit `meshfox.executablePath` override is unaffected — always
  used as-is.

## 0.1.0

Initial public release.

- `*.canvas.md` opens as an interactive node canvas automatically; any
  other `.md` (including a marker-carrying file without the suffix, like
  a project's own README.md) is available via "Open With..." or the
  **"Open as meshfox canvas"** command.
- The canvas renders as the editor tab's own top-level content (not a
  nested `<iframe>`), so copy/paste and the right-click context menu work
  normally — a long-standing upstream VS Code limitation for nested
  cross-origin iframes in a webview, sidestepped rather than worked around.
- Cross-canvas "↗ open" links open/focus another VS Code tab the same way.
- **"meshfox: Open in Browser"** opens the same running canvas in a real
  browser tab.
- **"meshfox: Check for Updates"** (and a once-a-day background check)
  wraps the existing `meshfox check-updates` CLI command; a missing
  `meshfox` binary gets an install-command prompt instead of a bare error.
- Syntax highlighting for meshfox's own bookkeeping comments
  (`meshfox:node`/`meshfox:edge`/`meshfox:var`/...) wherever the raw file
  is shown as plain text (git diffs/blame, "Open With... → Text Editor").
- macOS/Linux only for now — the coordinator talks to `meshfox view
  --watcher-socket` over a Unix domain socket.
