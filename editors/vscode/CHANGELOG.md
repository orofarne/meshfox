# Changelog

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
