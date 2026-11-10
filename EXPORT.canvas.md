<!-- meshfox:canvas -->
# Export
<!-- meshfox:node id="export" -->

Part of the [meshfox documentation](./README.md). Exporting a canvas as a static site or as a PDF.

## Static export (experimental)
<!-- meshfox:node id="usage-static" -->

Renders a canvas's node graph — boxes, tags, cached output, and every structural/`meshfox:edge` connection — as a static HTML/CSS/SVG site (no live server), through a user-supplied [Tera](https://keats.github.io/tera/) template: every `*.tera` file in `--template` is rendered with the canvas's data (context key `site`) and written to `--out` at the same relative path minus `.tera`; everything else in the template directory is copied verbatim (CSS, fonts, images, ...). [`site-template/`](./site-template) is the project-page example used below. [`site-template-archive/`](./site-template-archive) is the investigation-board template used by this repo's published site. Both templates use a small browser script to place SVG arrows after layout, including authored waypoints and labels.

A `file` node with `display="table"` is exported as a plain HTML grid of the table's first 100 rows under an "N rows × M cols" caption (see TEMPLATES.md); that needs the `duckdb` CLI, and the export **fails** — naming the node and why — if a table can't be read, rather than leaving it out.

To try the archive with this README: `meshfox static README.md --template site-template-archive -o /tmp/meshfox-archive --force --copy-files --recursive`.

[TEMPLATES.md](./TEMPLATES.md) is the reference for writing a template: what each page receives (`site`, `icons`, ...), the `SiteData` fields, escaping, and `template.toml`. A template's own settings live in an optional `template.toml` right in its own directory (see [`site-template/template.toml`](./site-template/template.toml)) rather than as `static` command-line flags — they're a property of *that template*, not something to repeat on every invocation: `api_version`, the template API version it was written for (required whenever the file exists; a different version is refused); `base_url`, this export's own canonical URL (`--sitemap`'s own `<loc>` prefix, below); `links_base_url`, prefixed onto a relative link/target the command doesn't already copy into `--out` (distinct from `base_url` — this repo's own [`site-template-archive/template.toml`](./site-template-archive/template.toml) points `base_url` at `meshfox.orofarne.net`, the site itself, and `links_base_url` at GitHub, where its canvases' own plain-Markdown source actually lives); and `icons`, a list of `<link rel="..." href="...">` tags (exposed to every template as the `icons` context key) for the page's own favicon/apple-touch-icon set. `template.toml` itself is read, never rendered or copied into `--out`, and a key it doesn't know is an error rather than ignored. A template with no `template.toml` gets an empty config — no `base_url`/`links_base_url` prefixing, no icon tags. Every page is HTML-escaped automatically (node titles and tags are untrusted text); `html_body` and the `script_json` filter's output are the two things a template marks `| safe`.

`--copy-files` copies a `file`-node's own target alongside the site instead of leaving it an unresolved link; `--recursive` (needs `--copy-files`) follows a `.canvas.md` target transitively, rendering it as its own page of the same site rather than refusing it (a plain `.md` target becomes a one-node document page, read like a node opened full screen); `--sitemap` (needs `template.toml`'s own `base_url`) writes a `sitemap.xml` listing every rendered page, and `--sitemap-git-dates` (needs `--sitemap`) sources each page's `<lastmod>` from its own canvas file's last commit date in git rather than leaving it unset. See each flag's own `-h` text below for the full story — this repo's own [`scripts/build-site.sh`](./scripts/build-site.sh) (the Cloudflare Pages build command for this same README, published at `meshfox.orofarne.net`) uses all four together.

```bash name="static-help" cache
meshfox static -h
```
<!-- meshfox:output name="static-help" hash="4bb2cc22" -->
```text
exit code: 0 · 18ms

Experimental: export a canvas as a static site. Resolves includes (same as `validate`/`view`), turns the canvas's node tree into a recursive `SiteData` (context key `site`) and hands it to a user-supplied Tera template. A node with no real, authored `x`/`y`/`width`/`height` gets no computed position at all — the template renders it as an ordinary nested HTML element and the *browser* lays it out and sizes it from its real content (no pre-computed/estimated pixels to get wrong); a node that does have all four real values keeps rendering at exactly that authored pixel position. A structural (parent/child) connector between two flow-positioned nodes is drawn in pure CSS (they're always DOM-adjacent); everything else — a `meshfox:edge` cross-reference, or a structural edge touching a real-positioned node — is left for a small non-interactive JS pass in the template to measure and draw. Every `*.tera` file in `--template` (except one whose basename starts with `_`, a partial meant to be `{% import %}`ed rather than rendered standalone) is rendered and written to `--out` at the same relative path minus `.tera`; every other file is copied verbatim (CSS, fonts, ...) — except `template.toml` itself, the template's own config file (optional, but if present it must declare `api_version`, and unknown keys are an error; a template with none gets no `base_url`/`links_base_url` and no `icons`), read from `--template`'s own directory and never copied to `--out`. Every page is HTML-escaped automatically; `html_body` and `script_json` output are the markup a template marks `| safe`. What a template receives and must do is described in TEMPLATES.md. A local image referenced from a node's Markdown body is copied alongside the output automatically; a `file`-type node's `display="code"` target is read once and inlined into the HTML directly (nothing left to fetch once static). A plain `file`-node target (not `display="code"`) is left as an unresolved link unless `--copy-files` is passed — see that flag's own help. See `site-template/` in this repo for a working example, including its own `template.toml`

Usage: meshfox static [OPTIONS] --template <TEMPLATE> [CANVAS]

Arguments:
  [CANVAS]  Path to the .canvas.md file. If omitted: auto-discover the single candidate in the current directory

Options:
      --canvas <CANVAS>      Same as the positional argument above, spelled as a flag — for parity with `run`/`node <op>`, which only accept this form
  -t, --template <TEMPLATE>  Template directory
  -o, --out <OUT>            Output directory. Refused if it already exists and is non-empty, unless `--force` [default: site]
      --force                Overwrite an existing, non-empty `--out` directory
      --copy-files           Copy a `file`-node's own target alongside the site (same treatment a Markdown image already gets), rewriting the link to point at the copy, for every `file` node whose rendered body still carries a plain link to its target (not `display="code"` — that's inlined already, nothing left to copy). Off by default: unresolved, as-authored links are today's behavior, unchanged unless this is passed. A target that resolves to a `.canvas.md` file is refused instead of copied, unless `--recursive` is also passed — copying an inert canvas source file into `--out` wouldn't give a reader following the link a rendered page
      --recursive            Requires `--copy-files`. A `file`-node target that resolves to a `.canvas.md` file is rendered as its own page of this same site (its own worker, own template pass, own nested `--out` directory — named after its path relative to the canvas this export started from) instead of being refused; the link is rewritten to point at that page. Followed transitively — a page rendered this way can itself link to further canvases — with each distinct canvas (by its real path on disk) rendered at most once even if several nodes, in this canvas or any other one reached this way, link to it; a cycle (A links to B, B links back to A) is graceful, not an error — the back-link just resolves to A's own already-rendered page. A `file`-node target that is a plain `.md` file (not a `.canvas.md`) is rendered as a page too: a one-node "document" (`page_kind` is `document` in the template context) whose title is the file's first `# ` heading
      --sitemap              Write a `sitemap.xml` at the root of `--out`, listing every rendered page (the root canvas's own, plus — with `--recursive` — every nested canvas's). Requires `template.toml`'s own `base_url` to be set (see `TemplateConfig::base_url`) — a sitemap's `<loc>` has to be an absolute URL, and `--sitemap` refuses to guess one
      --sitemap-git-dates    Requires `--sitemap`. Each `<url>`'s `<lastmod>` is the owning canvas file's own last commit date in git (`git log -1 --format=%cI`, relative to the canvas's own directory — same mechanism `canvas_commit`/`meshfox_version` already use for the template context), rather than left unset. Off by default: not every exported canvas is necessarily in a git repository (a temp-directory/CI-checkout export, say), and a file's git date isn't necessarily closer to "when the content last really changed" than the moment of export for every author's workflow either
  -h, --help                 Print help
```
<!-- /meshfox:output -->

Rendering `examples/hello.canvas.md` with that template into a scratch directory:

```bash name="static-example" cache
meshfox static examples/hello.canvas.md --template site-template -o /tmp/meshfox-static-demo --force
ls /tmp/meshfox-static-demo
rm -rf /tmp/meshfox-static-demo
```
<!-- meshfox:output name="static-example" hash="b2417166" -->
```text
exit code: 0 · 64ms

meshfox static: wrote 13 file(s) to /tmp/meshfox-static-demo
apple-touch-icon.png
favicon-16.png
favicon-32.png
favicon.ico
fonts
icon-192.png
index.html
style.css
tui-editor-themes.html
```
<!-- /meshfox:output -->

### Static export templates
<!-- meshfox:node id="static-export-templates-doc" type="file" -->

[TEMPLATES.md](./TEMPLATES.md)

## PDF export (experimental)
<!-- meshfox:node id="pdf-export-experimental" -->

Renders a canvas straight to a PDF file, via a real (headless) Chrome/Chromium rather than a hand-rolled layout engine — a system install is used if one can be found (`CHROME` env var, common binary names on `PATH`, well-known install locations); otherwise a pinned Chromium build is downloaded once and cached for next time. Builds on the same `meshfox_core::staticgen` data `static` uses (see above), so it gets the same real, browser-computed layout instead of guessing at Markdown-body heights in Rust.

A `file` node with `display="table"` prints the first 100 rows of its table as a plain grid, like `meshfox static` does — which needs the `duckdb` CLI, and `meshfox pdf` fails (naming the node and why) when a table can't be read, rather than printing a page with a hole in it.

Two kinds of pages, normally both, in this order:

- a **canvas page** — every node at its own box, full body always shown (never folded, regardless of the document's own fold settings — a printed page has no click to unfold later). A real, authored `x`/`y`/`width` is kept exactly; height always auto-sizes to the node's own real rendered content instead, authored or not, so a fixed size a canvas author set back when this box only showed a title can't clip the real body it shows now. Everything without a real position at all is auto-laid-out by the same tree-recursive algorithm `web/src/autolayout.ts` uses for the live canvas view (branch right per depth, stack siblings, a `group`'s box the bounding box of its resolved members) — but computed client-side, in the printed page's own script, against real measured content height, not guessed at in Rust (no heuristic can guess a Markdown body's height right in general — the same lesson `autolayout.ts`'s own module doc comment already draws from this project's history). So this page always has something worth printing, not just for a canvas someone has hand-positioned every node of. Printed at true 1:1 CSS-px scale, one single custom-sized page (the bounding box of every node's own box, capped at 200cm per side), never scaled to fit a fixed paper size. Connector arrows for both structural (parent → child) and `meshfox:edge` cross-reference relationships; a `group`'s own containment is shown spatially (transparent, dashed box around its members) rather than with a redundant connector line.
- **document page(s)** — the full node tree in flow/document order (headings by depth, tags, body, target, recursing into children), standard A4 pagination. A node with children ends its own block with a row of jump-links to each child's own heading (`meshfox:edge`/structural nesting isn't drawn spatially here the way the canvas page draws it).

`--mode canvas`/`--mode document` renders just one of the two instead of both. Both pages use the same self-hosted Fira Code font the web UI and `static`'s own `site-template/` use — reused straight out of the web UI's own already-embedded `web/dist` bundle (`rust-embed`) rather than a second copy embedded just for `pdf`.

```bash name="pdf-help" cache
meshfox pdf -h
```
<!-- meshfox:output name="pdf-help" -->
```text
exit code: 0

Experimental: export a canvas as a PDF, via a real (headless) Chrome/Chromium — a system install is used if one can be found (`CHROME` env var, common binary names on `PATH`, well-known install locations); otherwise a pinned Chromium build is downloaded once and cached for next time. Two kinds of pages, both by default: a canvas page — every node at its own box, full body always shown (never folded, regardless of the document's own fold settings); a real authored `x`/`y`/`width` is kept exactly, everything else auto-laid-out the same way the live web UI would place it, but height always auto-sizes to the node's own real content, authored or not, so nothing is ever clipped — printed at true 1:1 CSS-px scale on its own custom-sized page rather than scaled to fit a fixed paper size, with connectors for both structural parent/child and `meshfox:edge` cross-references; then the full node tree in flow/document order (headings by depth, tags, body, target, standard A4 pagination)

Usage: meshfox pdf [OPTIONS] [CANVAS]

Arguments:
  [CANVAS]  Path to the .canvas.md file. If omitted: auto-discover the single candidate in the current directory

Options:
      --canvas <CANVAS>  Same as the positional argument above, spelled as a flag — for parity with `run`/`node <op>`, which only accept this form
  -o, --out <OUT>        Output PDF path. Defaults to the canvas filename with its extension replaced by `.pdf`, in the same directory
      --force            Overwrite an existing `--out` file
      --mode <MODE>      Render only the canvas page or only the document page(s) instead of both (the default: canvas page first, then the document page(s)). A node with no real, authored `x`/`y`/`width`/`height` is auto-laid-out on the canvas page the same way the live web UI would place it, so this always has something to render [possible values: canvas, document]
  -h, --help             Print help (see more with '--help')
```
<!-- /meshfox:output -->





Rendering `examples/hello.canvas.md` to a scratch PDF file:

```bash name="pdf-example" cache
meshfox pdf examples/hello.canvas.md --out /tmp/meshfox-pdf-demo.pdf --force
file /tmp/meshfox-pdf-demo.pdf
rm -f /tmp/meshfox-pdf-demo.pdf
```
<!-- meshfox:output name="pdf-example" -->
```text
exit code: 0

meshfox pdf: wrote /tmp/meshfox-pdf-demo.pdf
/tmp/meshfox-pdf-demo.pdf: PDF document, version 1.5, 4 pages
```
<!-- /meshfox:output -->
