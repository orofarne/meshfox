# meshfox editor (in the browser, WebAssembly)

The real meshfox UI, with no `meshfox` process behind it. The document model
(`crates/model`, compiled to WebAssembly by `crates/wasm`) runs in the page and
answers the same `api/...` requests the app normally sends to a server.

```sh
# one-time: the wasm target and a wasm-bindgen CLI matching Cargo.lock
rustup target add wasm32-unknown-unknown
cargo install wasm-bindgen-cli --version "$(grep -A1 '^name = "wasm-bindgen"$' ../Cargo.lock | sed -n 's/^version = "\(.*\)"/\1/p')" --locked

cd web
npm install
npm run build:wasm      # builds the wasm package, then the static site into web/dist-wasm
npm run preview:wasm    # http://localhost:4600/
npm run test:wasm-editor  # Playwright, Chromium + Firefox + WebKit
```

Open a `.canvas.md`, a whole folder (so `include`s, images and `file` nodes
resolve), or drop either onto the page. The root of what was opened is the
boundary a canvas reads within: one in a subdirectory shows `../docs/x.png` of
the folder, and nothing above the folder is reachable. The build is a static site with
relative URLs, emitted as `index.html`, so it can be served from any path
(published at `https://meshfox-editor.orofarne.net/`).

A packed canvas (`.canvas.tar.xz`, made by `meshfox pack`) can be picked or
dropped as well: the model (`openPack`, the same code `meshfox unpack` and
`meshfox view` run) checks all of it — manifest, sizes, BLAKE3 hashes, paths,
limits — and refuses a damaged or hostile pack with the reason before anything
is shown. Its files then become the in-memory tree, exactly like a folder, so
includes, images and linked canvases resolve inside the pack. "download pack"
(`buildPack`) hands back a new pack with the edits, under the name of the one
that was opened; "download canvas" gives the text of the canvas now shown, and
the file that was opened is never changed.

A document with an `include` (or image, or file link) whose file was not
picked still opens: the node shows a notice and the strip at the bottom lists
what is missing. "add files…" / "add folder…" there put more files in place and
the page refreshes in place (a file lands where the missing target points when
its name matches, otherwise beside the canvas; a folder is taken as the same
root as the first pick).

Unsaved work is kept in the browser (IndexedDB, `drafts.ts`) while it differs
from what was saved: closing the tab, a crash or a reload loses nothing. The
launcher lists such drafts ("Unsaved work in this browser": Restore / Discard),
and opening the same file again offers the one it finds ("restore them" /
"discard"; it is kept until you decide). A draft goes away when the text is
saved or undone back to the saved text. "download" saves a file; "save" (only
where the browser lets a page write to a file: Chromium) writes into a file you
choose once, then again without asking. Only the canvas text is kept — it is the
only file the editor changes.

Browser history: opening a document pushes a history entry (the URL is left
alone, since the app's relative `api/...` requests depend on it), so Back
returns to the launcher. A document lives only in memory, so a reload, or
Forward onto a document that is gone, lands on the launcher with a note.

## How it works

- `main.ts` — the launcher. When a document is chosen it starts the wasm model,
  checks the document renders, installs the backend, then loads `src/main.tsx`
  (the app, unchanged).
- `backend.ts` — replaces `window.fetch` and `window.WebSocket` for the page's
  own `api/...` URLs and answers them with the wasm model: the canvas, every
  document edit (`meshfox_model::edit`, the same code `meshfox view` runs),
  undo/redo/history (`session.ts`, over the model's `UndoLog`), `asset`,
  `nodes/:id/file-content`, opening another canvas of the folder, and empty
  answers for services/runs/includes/syntax. The live-update socket
  (`api/watch`) only sends heartbeats and the `changed` events the page itself
  causes. Anything that would need a worker (running, tables) answers 501.
- `engine.ts` — fetches the wasm with progress; `drafts.ts`, `svg.ts`,
  `securityHeaders.ts` — see the sections above and below.
- `files.ts`, `pick.ts` — the in-memory file tree the wasm model reads through
  its `files.read(path)` callback, filled from the picker, a drop or a drop.

## Limits

- No running blocks, forms or services (the page says `noexec`), no
  `display="table"` (needs DuckDB), no link preview cards.
- Files a canvas includes are shown but not edited here; only the canvas text
  is.
- A pack is saved whole: "download pack" holds every file the page has (the
  pack's own, plus anything added with "add files…"), not only what the canvases
  still reference.
- Safari's folder picker leaves out every dot-file, so a *picked* folder cannot
  honour `.meshfoxignore` there (a dropped one can). The suite also runs on
  WebKit (Safari's engine, which every iOS browser uses).
- A browser or privacy tool that locks `img.src`, `fetch`, `WebSocket` or
  `setAttribute` (iOS content blockers do) cannot run the editor: the launcher
  says so and starts nothing (`unsupportedBrowser` in `backend.ts`).
- The whole opened folder is read into memory (limits: 20 000 files, 25 MB
  each).
- `asset_base` for included files is the included file's directory inside the
  opened tree; images beside an include resolve, deeper cases are untested.

## Several canvases in one folder

A `file` node whose target is a `.canvas.md` keeps its "↗" button: it shows that
canvas in the same page (nothing else can be opened from a page, so a plain
file has no such button). Each canvas keeps its own edits and undo history for
as long as the page lives, Back and Forward move between them, and the draft
(see above) is kept per canvas. A canvas that is not among the opened files
says so; "add files…" brings it in. A `#node-id` in the link puts the view on
that node, as `meshfox view` does.

## Serving it (security headers)

The `editor-bundle` block in the root `README.md` (node `online-editor-in-the-browser`) builds the site into `editor-bundle/`, ready for
Cloudflare Pages. The build writes `_headers` (from `securityHeaders.ts`; Pages
reads it itself); the preview server the tests run against sends the same
headers, so the suite runs under the Content-Security-Policy.

Serve it from its **own origin** (a subdomain such as
`meshfox-editor.orofarne.net`): drafts live in that origin's IndexedDB, and
nothing else may run beside them. What a document can make the page do:
Markdown HTML is escaped; a link to an SVG opens it as text; a drawn SVG is
cleaned of scripts before it gets a URL; remote images load (a document can see
that it was opened).
