# In-browser meshfox (WebAssembly)

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
npm run test:wasm-demo  # Playwright, Chromium + Firefox
```

Open a `.canvas.md`, a whole folder (so `include`s, images and `file` nodes
resolve), or drop either onto the page. The build is a static site with
relative URLs, emitted as `index.html`, so it can be served from any path
(the plan: `https://meshfox.orofarne.net/editor/`).

Browser history: opening a document pushes a history entry (the URL is left
alone, since the app's relative `api/...` requests depend on it), so Back
returns to the launcher. A document lives only in memory, so a reload, or
Forward onto a document that is gone, lands on the launcher with a note.

## How it works

- `main.ts` — the launcher. When a document is chosen it starts the wasm model,
  checks the document renders, installs the backend, then loads `src/main.tsx`
  (the app, unchanged).
- `backend.ts` — replaces `window.fetch` and `window.WebSocket` for the page's
  own `api/...` URLs: `canvas` (rendered once by `renderCanvas`), `info`
  (read-only, no exec), `asset`, `nodes/:id/file-content`, and empty answers for
  history/services/runs/includes/syntax. The live-update socket (`api/watch`)
  is a stub that only sends heartbeats. Anything that would need a worker
  (running, editing, tables) answers 501.
- `files.ts`, `pick.ts` — the in-memory file tree the wasm model reads through
  its `files.read(path)` callback, filled from the picker, a drop or a drop.

## Limits (it is a prototype)

- Read-only. No running blocks, no editing, no `display="table"` (needs DuckDB),
  no link previews, no live reload.
- The toolbar still shows buttons that make no sense here (Console, reset
  session); they do nothing.
- The whole document is rendered once at load; a changed file needs a reload.
- `asset_base` for included files is the included file's directory inside the
  opened tree; images beside an include resolve, deeper cases are untested.
