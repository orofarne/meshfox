<!-- meshfox:canvas -->
# meshfox

<!-- meshfox:var name="INSTALL_PATH" prompt="Install prefix?" default="/usr/local/bin" -->

An executable canvas: a hierarchical, node-based document where nodes hold Markdown, and code blocks inside that Markdown can be run — with their output optionally cached right next to the code.

Status: **early bootstrap**. This document is itself a valid meshfox canvas — every `##` section here is a node, nested under this root. The leading HTML comment above the title is the `meshfox:canvas` marker (see [SPEC.md](./SPEC.md), or browse this file with `meshfox view` — "File format" below includes it live); it's what lets tooling recognize this as a canvas despite the plain `.md` extension.


## Concept
<!-- meshfox:node id="concept" -->

- A project is one **canvas**: a tree of nodes starting from a single **root** node.
- From the root, large **section** nodes branch off (e.g. one section per feature).
- From sections, further **block** nodes branch off. Blocks hold Markdown.
- Markdown inside a block can contain fenced code. A fence can be marked *runnable*; running it executes the code and (optionally) writes the result back into the same node, right under the code, so nobody has to re-run it just to see what happened last time.
- Two ways to interact with the same files:
  - a browser UI (canvas view + block runner) backed by a small Rust server — opens read-only, so pulling up a canvas to look around never one-click-modifies it: running a block is always allowed (you're still explicitly clicking "run", and it's the whole point of a canvas), but a `cache`d block's output isn't written back to the file until an explicit "Edit" button is clicked, which also unlocks dragging, resizing, and saving layout. Output streams into the browser live as the block runs (not just once it's finished), and a running block gets a Kill button, for when one hangs.
  - a CLI that runs blocks non-interactively, `make`-style, for use in scripts/CI — `meshfox list` prints every runnable block as a tree, so there's no need to go spelunking through the file to find out what's runnable. `meshfox run` streams output live too (the same async, killable executor as the browser UI); Ctrl+C kills whichever step is currently running, whole process group and all, and stops there — whatever earlier steps in the chain already completed stays cached.

## File format
<!-- meshfox:node id="file-format" type="include" -->

[SPEC.md](./SPEC.md)

## Auto-layout
<!-- meshfox:node id="auto-layout" -->

Nobody has to type `x`/`y`/`w`/`h` by hand. There are two independent auto-layout engines now — one on demand in `meshfox fmt`, one live in the web UI for whatever's still unpositioned — deliberately not required to agree pixel-for-pixel; each leans on inputs the other doesn't have.

- **On demand**: `meshfox fmt` runs `crates/core/src/layout.rs`'s tree-aware heuristic and actually writes the result into the file (via the usual surgical `set_node_meta` patch, one node at a time). The root and its direct children ("sections") read top-to-bottom with just a small nudge to the right, same as a document's title followed by its headings — not yet a real indent. From there down it's a classic indented tree view, the same shape as a file tree or a collapsed outline: each section's own content steps fully to the right of its parent, siblings at a given depth stack vertically, and further nesting keeps stepping right from there. Box size is estimated from each node's content (line count, whether it has a code fence, whether it needs a run-button row) rather than being a flat constant, so a stub and a node with a paragraph plus cached output don't come out the same size. It's a simple heuristic, not a publication-quality tree drawer — adversarial trees might still look rough:

  ```sh
  meshfox fmt              # fills in position/size only for nodes missing one
  meshfox fmt --force      # recomputes and overwrites every non-group node
  ```

  `group` nodes are always skipped — their box is derived, never stored, whether or not `--force` is given. Without `--force`, a node keeps whatever it already has; `fmt` only fills gaps, so hand-placed (or already-formatted) nodes aren't disturbed by running it again — the second worked example above, for instance, is already fully positioned, so `meshfox fmt` on it is a no-op.

- **Live in the browser**: `GET /api/canvas` sends exactly what's in the file — no computed suggestion, no `suggestedX`/etc. over the wire. `web/src/autolayout.ts` fills in a box client-side for anything still missing a real position, using the same overall tree shape as `layout.rs` (sections top-to-bottom, deeper nesting branching right, siblings stacked without overlap, a `group`'s box as the bounding box of its resolved members) but with real browser-only inputs instead of a text-length heuristic: root and its direct children share one width, `60%` of the viewport; everything deeper gets `40%`, uniformly regardless of how much deeper. Viewport width is read once, when the canvas loads — it doesn't recompute on window resize. Height is never estimated: it comes from React Flow's own measurement of each node's actual rendered content, live — the layout self-corrects as soon as a real measurement lands (and again later, e.g. as a running block's output grows). A depth-≥2 node additionally gets a `max-height` cap so one long block can't drag a whole subtree far from its parent; past the cap it scrolls internally (`.mesh-node-body`'s existing `overflow: auto`) instead of growing the box further. None of this is ever written to the file just from loading it — same "don't fight the user's own drag" rule as before (see `touchedNodeIds` in `App.tsx`), a node only gets its box persisted once it's actually been dragged/resized, or `meshfox fmt` gives it a real one.

  Edit mode's toolbar has an **Auto-layout** button that clears every non-group node's stored `x`/`y`/`w`/`h` in the file outright (`POST /api/canvas/clear-layout`), reverting the whole document to auto-placed — behind a confirmation dialog, since it can't be undone from the UI. Useful for backing out of a bunch of hand-placed positions and letting the client lay everything out fresh.

## Variables
<!-- meshfox:node id="variables" -->

Document-scoped config values a canvas wants from whoever runs it — declared once as `<!-- meshfox:var ... -->` comments in the root node (this document declares exactly one, `INSTALL_PATH`, right above "Concept" — invisible here since it's an HTML comment, same as every other bit of meshfox bookkeeping). Declaring one doesn't put it in any block's environment by itself, though — a block has to opt in with its own `env=` fence attribute to actually reference it (`env="$INSTALL_PATH"`, see "Install" below), and only blocks that do ever resolve or prompt for anything: running any *other* block in this file never asks about `INSTALL_PATH`, however many blocks elsewhere might use it. Asked for interactively the first time some block's `env=` actually needs it, then remembered in a local `.meshfox/<filename>.env` cache (`.gitignore`d, analogous to CMake's `CMakeCache.txt`) so it's not asked for again by any block referencing it afterward. `meshfox configure` walks every declared variable up front regardless of `env=` usage; `meshfox run --set NAME=value` supplies one non-interactively (e.g. for CI); the web UI shows a small form in place of a prompt, scoped to just the clicked block's own chain, only for whatever isn't already resolved. See SPEC.md's "Variables" for the full writeup, including `secret` (never cached, always re-asked) and the `int`/`bool`/`select` types.

## Usage
<!-- meshfox:node id="usage" -->

A few commands, run for real against the installed `meshfox` binary (see "Development" below for building/installing it). The output below is cached from actually invoking it (see "Cached output" above) rather than typed by hand, so it can't quietly drift from what the CLI does — and it doubles as an end-to-end check of the runnable-block feature itself: this section's own blocks are ordinary `name=`/`cache` fences, executed the same way any project's would be.

### CLI help
<!-- meshfox:node id="usage-help" -->

Run `meshfox spec` to print the full format specification (`SPEC.md`,
embedded in the binary at compile time) — not cached here since dumping it
verbatim into this section would nest the whole `.canvas.md` grammar inside
an example of the format it's describing.

```bash name="usage-help" cache
meshfox -h
```
<!-- meshfox:output name="usage-help" -->
```text
exit code: 0

CLI and local web viewer/editor for a meshfox canvas. Run `meshfox spec` for the full .canvas.md format specification.

Usage: meshfox [OPTIONS] [COMMAND]

Commands:
  run        Run one or more named code blocks
  configure  Interactively resolve every declared `meshfox:var` (see SPEC.md's "Variables") and save the answers to the on-disk cache (`.meshfox/<filename>.env`, next to the canvas file) so `run` doesn't have to ask again. Shows each variable's currently-resolved value as the prompt's own default — press Enter to keep it. Secret variables are never cached, so there's nothing for this to save for them; they're skipped here and asked for fresh at run time instead. Requires an interactive terminal
  fmt        Fill in x/y/w/h for nodes that don't have them yet, using a simple tree-aware auto-layout (see `meshfox_core::layout`). Never touches `group` nodes — their box is always derived from their children, never stored — and by default leaves any node's position/size alone once it has one, so hand-placed nodes survive a format
  create     Create a new, empty canvas file: just the `meshfox:canvas` marker followed by a lone root heading (`#`) named after the file itself (its name with a trailing `.canvas.md`/`.md` stripped). Fails if the file already exists — this never overwrites
  view       Start the local web UI: canvas view, run buttons. Opens read-only — running a block is always allowed, but click "Edit" in the browser to unlock dragging, resizing, saving layout, and persisting a `cache`d block's output back into the file
  validate   Validate that a file parses as a meshfox canvas — same checks `run`/`fmt`/`view` already do before touching anything (single root, no duplicate ids, no dangling `meshfox:edge` targets, `group`/ `file`/`link` body rules) — without executing anything or writing the file back. Exits non-zero on a parse error, so it's usable as a pre-commit/CI check
  check      Run every `constraint`-type node's Starlark contract against the document (see `crate::constraint`/SPEC.md's "Constraint nodes") and report which passed. Distinct from `validate`: `validate` checks that the file *parses* as a well-formed canvas; `check` asks whether the document as a whole satisfies whatever rules its own constraint nodes declare (e.g. "every node tagged `table` has exactly one `file` child") — implies `validate` first, since an unparseable file has no constraints to run. Exits non-zero if the file fails to parse or any constraint fails, so it's usable as a pre-commit/CI check alongside (or instead of) `validate`
  list       Print every runnable code block in the canvas as an indented tree, each with a ready-to-paste `meshfox run <path...> <name>` — so you don't have to go spelunking through the file to find out what's runnable. Same raw-file-only scope as `run`/`fmt`/`validate` (no include resolution)
  node       Structural edits to individual nodes in a canvas file: add, move, rename, delete, or set a node's body/position/style/edges — the CLI counterpart to the web UI's Edit-mode node operations (the same `mdcanvas` surgical patches `meshfox view`'s `/api/nodes*` routes use), for scripting/CI or whenever a hand-rewrite would risk getting heading depth, sibling order, or dangling-edge cleanup wrong. Every subcommand validates the fully-patched document still parses before writing it back, same as every other mutating command here
  spec       Print the full .canvas.md format specification (SPEC.md, embedded in this binary at compile time) — the canonical reference for the format, available offline wherever `meshfox` is installed
  help       Print this message or the help of the given subcommand(s)

Options:
      --agent-help  Print usage guidance for AI coding agents (when to prefer `node` subcommands over hand-editing, non-interactive `run`, etc.) and exit
  -h, --help        Print help
  -V, --version     Print version

Agent Usage:
  If you are an AI coding agent, run `meshfox --agent-help` before hand-editing a
  .canvas.md file. It covers when to prefer `meshfox node <verb>` over a raw text
  edit, how to run non-interactively, and other guidance not covered above.
```
<!-- /meshfox:output -->

#### Node commands
<!-- meshfox:node id="usage-node" -->

`meshfox node <op>` exposes the same per-node surgical patches
(`insert_child_node`, `delete_node`, `reparent_node`, `set_node_title`,
`set_node_body`, `set_node_meta`, `set_node_edges`, `reorder_by_position` —
all in `mdcanvas`) that back the web UI's Edit-mode operations over its
`/api/nodes*` routes — so a structural change (adding a child at the right
heading depth, moving a subtree without breaking its nesting, deleting a
node without leaving a dangling `meshfox:edge` behind) can be scripted or
run in CI without going through the browser, and without hand-rewriting
Markdown heading levels yourself. Every subcommand takes an optional
`--canvas <path>` (auto-discovered like `fmt`/`validate`/`list` when omitted)
and validates the whole patched document still parses before writing it
back — the same validate-before-commit shape every mutating server
handler already uses. As with any other write in this file, running
`meshfox validate` afterwards is still worth doing: parsing is validated
here, but a deletion or a rename can still leave a `deps=`/`env=`
reference elsewhere dangling, which is `validate`'s job to catch, not
`node`'s.

- `add <parent-id> <title>` — insert an empty child node, last in the parent's subtree; prints the new (slugged) id
- `rm <node-id> [--keep-children]` — delete a node and its subtree, or (with the flag) just the node, promoting its direct children to its former parent
- `mv <node-id> <new-parent-id>` — move a node under a new structural parent in one step (the web UI needs two: link, then promote)
- `rename <node-id> <title>` — change heading text only; id, heading level, and body untouched
- `body <node-id> [--file <path>]` — replace a node's whole body, from a file or stdin
- `meta <node-id> [--x --y --w --h --color --type --display --lang]` — set position/size/style; an omitted flag keeps the node's current value; `group` positions are rejected (their box is always derived, never stored, same as `fmt`)
- `edges <node-id> [--from <id>]... [--clear]` — replace a node's extra (`meshfox:edge`) parents
- `reorder` — resync sibling heading order in the file to match current x/y, the same resync the server runs on every UI save
- `show <node-id>` — print a node's parent/children/extra-parents/type/position (read-only)

```bash name="usage-node" cache
meshfox node -h
```
<!-- meshfox:output name="usage-node" -->
```text
exit code: 0

Structural edits to individual nodes in a canvas file: add, move, rename, delete, or set a node's body/position/style/edges — the CLI counterpart to the web UI's Edit-mode node operations (the same `mdcanvas` surgical patches `meshfox view`'s `/api/nodes*` routes use), for scripting/CI or whenever a hand-rewrite would risk getting heading depth, sibling order, or dangling-edge cleanup wrong. Every subcommand validates the fully-patched document still parses before writing it back, same as every other mutating command here

Usage: meshfox node <COMMAND>

Commands:
  add      Add a new, empty-bodied child node under `parent-id`, as the last item in its existing subtree (`mdcanvas::insert_child_node`) — same as the web UI's "add child" button. No position is set, so it stays unpositioned — auto-placed by whatever's viewing it (the web UI's own client-side layout, or `meshfox fmt`) — until something gives it a real one. Prints the new node's id: a slug of `title`, de-duplicated against every id already in the file
  rm       Delete a node. By default the whole subtree goes with it (`mdcanvas::delete_node`), and any `meshfox:edge from="..."` elsewhere that pointed into the deleted subtree is dropped too, so the file can't be left with a dangling reference. `--keep-children` instead deletes just this node, promoting its direct children (and everything under them, untouched otherwise) to its own former parent (`mdcanvas::delete_node_reparent_children`). Refuses to delete the root either way
  mv       Move a node to a new structural parent (`mdcanvas::reparent_node`). That core function only ever promotes an *existing* extra-parent edge to structural parent — the web UI's two-step dance (drag a new edge onto the node, then promote it) — so this adds the `meshfox:edge from="new-parent-id"` line itself first, making the move a single atomic step from the CLI. Refuses to move the root, or to move a node into itself or one of its own descendants (would make the tree cyclic)
  rename   Rename a node's heading text, leaving its id, heading level, and body untouched (`mdcanvas::set_node_title`) — a node's id is pinned the first time it's written and never follows later title edits
  body     Replace a node's whole Markdown body (`mdcanvas::set_node_body`) — what the web UI's in-node editor would send, if it had one yet (see README's roadmap; for now the UI can reposition and run, not edit text). For a `file`/`link` node the body is its one Markdown link (`[title](target)`); a `group` node's body must stay empty. Reads the new body from `--file`, or from stdin if `--file` is omitted
  meta     Set a node's position/size/style fields (`mdcanvas::set_node_meta`) — `--x`/`--y`/`--w`/`--h` for a manual position/size override (`meshfox fmt` is the usual way to fill these in), `--color`/ `--type`/`--display`/`--lang` for style/type. Any field left unset keeps its current value. `group` nodes never store a position (`fmt` skips them too, deriving their box from their children instead), so `--x`/`--y`/`--w`/`--h` are rejected for one
  edges    Replace a node's whole set of extra incoming edges (`meshfox:edge from="..."` lines, `mdcanvas::set_node_edges`) — the non-structural, non-nesting cross-references JSON Canvas-style graphs use. The given `--from` list (repeatable) *replaces* whatever was already there, it doesn't add to it; `--clear` removes them all
  reorder  Reorder every parent's direct children in the file to match their canvas layout (`mdcanvas::reorder_by_position`, sorted by `y` then `x` among ties) — the same resync the server runs on every save from the web UI, exposed standalone for whenever positions changed by hand (or via `node meta`/`fmt`) and the on-disk heading order should catch up to match what's actually drawn
  show     Print one node's parent, children, extra parents, type, and position/style fields — a read-only lookup, since eyeballing the tree shape directly from the file gets harder the deeper it nests
  help     Print this message or the help of the given subcommand(s)

Options:
  -h, --help  Print help
```
<!-- /meshfox:output -->



`add` then `show` on a scratch copy, so this doesn't touch the tracked example file:

```bash name="node-add-example" cache
cp examples/hello.canvas.md /tmp/meshfox-node-demo.canvas.md
meshfox node add --canvas /tmp/meshfox-node-demo.canvas.md tests "Regression Test"
meshfox node show --canvas /tmp/meshfox-node-demo.canvas.md regression-test
rm -f /tmp/meshfox-node-demo.canvas.md
```
<!-- meshfox:output name="node-add-example" -->
```text
exit code: 0

meshfox node add: added "regression-test" under "tests" in /tmp/meshfox-node-demo.canvas.md
id: regression-test
title: Regression Test
type: text
parent: tests
children: (none)
extra parents: (none)
position: x=? y=? w=? h=?
```
<!-- /meshfox:output -->

### Running a block
<!-- meshfox:node id="usage-run" -->

Runs the `smoke` block in `examples/hello.canvas.md` for real, updating its cached output in place — the same surgical patch described above, not a simulation:

```bash name="run-example" cache
meshfox run examples/hello.canvas.md tests smoke-test smoke
```
<!-- meshfox:output name="run-example" -->
```text
exit code: 0

==> smoke
hello from meshfox
Sun Jul 26 15:59:41 +04 2026
(exit 0)
```
<!-- /meshfox:output -->

### Formatting a copy
<!-- meshfox:node id="usage-fmt" -->

`fmt` on a scratch copy, so this doesn't touch the tracked example file:

```bash name="fmt-example" cache
cp examples/hello.canvas.md /tmp/meshfox-fmt-demo.canvas.md
meshfox fmt /tmp/meshfox-fmt-demo.canvas.md
rm -f /tmp/meshfox-fmt-demo.canvas.md
```
<!-- meshfox:output name="fmt-example" -->
```text
exit code: 0

meshfox fmt: placed 0 node(s) in /tmp/meshfox-fmt-demo.canvas.md
```
<!-- /meshfox:output -->

### Interactive (`tty`) blocks
<!-- meshfox:node id="usage-tty" -->

A block flagged `tty` hands its process a real interactive terminal instead of the captured/streamed output every other block gets — for anything that actually needs to talk to a terminal: an editor, a REPL, `ssh`. See SPEC.md's "Interactive (`tty`) blocks" for the full writeup, including why it's mutually exclusive with `cache` (nothing here to freeze into an output block, so — unlike every other example in this section — there's no cached output shown below).

```bash name="vim-demo" tty
vim
```

Run it from a real terminal — `meshfox run README.md usage usage-tty vim-demo` — and it drops you straight into `vim` editing that scratch file, same as running `vim` directly would; `:wq` (or `:q!`) hands the terminal back same as it always does. `meshfox run` checks stdin/stdout are actually a terminal before starting a `tty` block and errors out otherwise, rather than hanging a script or CI job that happens to reach one. The web UI runs the same block over a real pseudo-terminal instead: clicking "run vim-demo" in `meshfox view` opens it as a floating terminal panel over the canvas rather than filling in the node's own inline output area.

## Architecture
<!-- meshfox:node id="architecture" -->

Rust workspace + a small browser frontend:

```
crates/
  core/     canvas model, .canvas.md parsing/rendering (crate::mdcanvas),
            tree derivation, Markdown fence scanning, output-block
            rewriting, executors (bash, ...), auto-layout
  server/   library crate: axum HTTP backend (load/save a canvas file,
            execute a block) + the built web/ UI, embedded at compile time
            via rust-embed. No [[bin]] of its own — meshfox-cli links it.
  cli/      the only binary: `meshfox`. `run`/`fmt` use crates/core
            directly; `view` starts crates/server's backend (with the UI
            baked in) on localhost, read-only until the browser's "Edit"
            button is clicked. One executable, no separate server process
            to install or start.
web/        React + React Flow editor: renders the node/edge graph,
            in-node Markdown editing, run buttons wired to the server API.
            Built once (`npm run build`) and embedded into the `meshfox`
            binary — not shipped or loaded separately at runtime.
```

`core` is the shared brain; `server` wraps it in an HTTP API (and owns the embedded UI); `cli` is the single front door a user actually runs. Every write to the source file — cached output, saved layout — goes through a surgical patch (`mdcanvas::set_node_body` / `set_node_meta`) that touches only the node(s) that actually changed, never a full-document reformat.

## Development
<!-- meshfox:node id="development" -->

Requires Rust (stable, via [rustup](https://rustup.rs/)) and Node.js for the web UI.

`crates/server` embeds `web/dist` at compile time (via `rust-embed`), so **build the frontend before building anything that depends on it** — a fresh clone has an empty `web/dist` (just a tracked `.gitkeep`, see `.gitignore`) and `meshfox view` will happily start and serve the API, but its UI route will just say the assets weren't built rather than show anything.

```sh
cd web && npm install && npm run build   # do this first — see above
cargo build --workspace                  # build core/server/cli into the one `meshfox` binary
cargo test --workspace                   # run core's unit tests

# run block "smoke" on the node reached via tests -> smoke-test
cargo run -p meshfox-cli -- run examples/hello.canvas.md tests smoke-test smoke

# validate, fmt, and view all take the canvas path the same way
cargo run -p meshfox-cli -- validate examples/hello.canvas.md
cargo run -p meshfox-cli -- fmt examples/hello.canvas.md
cargo run -p meshfox-cli -- view examples/hello.canvas.md   # UI + API on :4590, opens read-only,
                                                              # launches your browser (--no-open to skip)
# (once meshfox is on your PATH: `meshfox view README.md`, `meshfox run
# README.md usage usage-help help`, etc. `run` recognizes a leading path by
# its .md suffix — node ids never have one — so it stays unambiguous
# alongside its own path/block-name arguments; auto-discovery still works
# everywhere too when the path is omitted)
```

Working on the frontend itself: the embedded copy is a snapshot from your last `npm run build`, so for live-reloading UI development run `cd web && npm run dev` (on :5173) in a second terminal alongside `meshfox view` (on :4590) — Vite proxies `/api` to :4590, same as before.

### Unit tests
<!-- meshfox:node id="unit-tests" -->

`cargo test --workspace` runs the Rust workspace's own unit/integration tests — `crates/core`'s parsing (`mdcanvas`), tree derivation, auto-layout, fence-scanning, dependency-resolution, and variable-handling logic is the bulk of the coverage, each in its own `#[cfg(test)]` module next to the code it exercises, plus a handful of higher-level tests in `crates/server`/`crates/cli` (e.g. the `tty` websocket path). Pure Rust — no Node.js, no built frontend, no browser needed, unlike the end-to-end suite below. `name=`d so it's runnable like any other block here, deliberately without `cache`, same reasoning as "End-to-end tests"/"Release build": the log is per-run noise, not something worth freezing into this file. Flagged `default` since its block name (`run`) doesn't match its node id (`unit-tests`):

```sh name="run" default
cargo test --workspace
```

### End-to-end tests
<!-- meshfox:node id="e2e-tests" -->

`web/e2e/` is a [Playwright](https://playwright.dev) suite that drives the real UI in a real browser against a real `meshfox view` — not a mocked frontend — because the bugs this suite exists to catch (a dependency badge clipped by `overflow: hidden`, a highlight's `box-shadow` eaten by that same `overflow: hidden`) were only visible in the genuinely rendered, genuinely laid-out canvas; a component-level test wouldn't have seen either. It runs against three fixture canvases (`web/e2e/fixtures/*.canvas.md`, one each for dependency-chain UI, scroll/pan interaction, and text selection) — deterministic (no `date`/timestamps) and separate from `examples/hello.canvas.md`, so test stability never depends on the documentation example's own content. Every test runs the UI in its default read-only mode (never clicks "Edit"), so nothing in the suite ever writes back into a fixture file. Each suite runs against both Chromium and Firefox (`chrome-*`/`firefox-*` projects in `playwright.config.ts`); a small, explicitly-commented handful of assertions are skipped on Firefox where they hit confirmed Gecko-specific limitations (nested-scrollframe wheel-event chaining; drag-selection anchoring under a CSS `transform` ancestor) rather than anything wrong in this app's own code.

`e2e-prep` installs `web/`'s npm dependencies and Playwright's own Chromium and Firefox copies (separate from any browser already on your system) — both idempotent, so re-running once already done costs nothing. `run` depends on it (`deps="e2e-prep"`), so the dependency chain always pulls prep in first — no manual first-time step to remember. Both `name=`d so they're runnable like any other block here, deliberately without `cache`, same reasoning as "Release build" below: their logs are per-run noise (install/test timings) rather than something worth freezing into this file. `run` is also flagged `default` — the one block per node `meshfox run <path>` can address without a trailing block name — so `meshfox run development e2e-tests` runs it (and its `e2e-prep` dependency) directly, without needing to spell out `... e2e-tests run`:

```sh name="e2e-prep"
cd web
npm install
npx playwright install chromium firefox
```

```sh name="run" deps="e2e-prep" default
cd web
npm run test:e2e
```

For interactive debugging instead of a one-shot run, use `npm run test:e2e:ui` (opens Playwright's UI mode — not runnable here, since it doesn't exit on its own).

`playwright.config.ts`'s `webServer` starts `meshfox view` itself (via `cargo run`, rebuilding only if the Rust side changed) — no server needs to be already running, and no separate `npm run build` step either, since `test:e2e` runs it as a `pretest:e2e` hook. Debug builds of `meshfox-server` read `web/dist` fresh off disk on every request (`rust-embed`'s `debug-embed` feature, which would force compile-time embedding even in a debug build, isn't enabled — see its `Cargo.toml`), so a frontend-only change just needs `npm run build` again, not a Rust rebuild, between test runs.

### Release build
<!-- meshfox:node id="release-build" -->

An optimized, distributable single binary. `name=`d so it's runnable like any other block here, deliberately without `cache` — a full release compile is slow and its build log isn't worth freezing into this file on every run:

```sh name="release-build"
cd web && npm install && npm run build
cargo build --workspace --release
echo "binary: target/release/meshfox"
```

### Install
<!-- meshfox:node id="install" -->

Copies the release binary to `$INSTALL_PATH` — see "Variables" above, where it's declared. `env="$INSTALL_PATH"` is what actually pulls it into this block's environment: the first `run`/`configure` that reaches a block with this `env=` prompts for it (default `/usr/local/bin`) and remembers the answer in `.meshfox/README.md.env` afterward, so this doesn't ask again on repeat installs — and, since no other block in this document declares `env=` at all, `INSTALL_PATH` is never resolved or asked about by anything else here. `deps="release-build/release-build"` (a cross-node reference — see "Runnable code fences" in SPEC.md — since `release-build` is a block in a different node than this one) means installing always builds fresh first. This block's own name already matches its node's id (`install`), so it's the node's implicit `default` block — no explicit `default` flag needed, unlike `e2e-tests`' `run` above, which needed one since its block is named `run`, not `e2e-tests`; both mechanisms are demonstrated in this document. Deliberately without `cache`, same reasoning as "Release build": the log is per-run noise, not something worth freezing into this file:

```sh name="install" deps="release-build/release-build" env="$INSTALL_PATH"
mkdir -p "$INSTALL_PATH"
cp target/release/meshfox "$INSTALL_PATH/meshfox"
echo "installed to $INSTALL_PATH/meshfox"
```

## License
<!-- meshfox:node id="license" -->

meshfox is MIT-licensed.

### LICENSE
<!-- meshfox:node id="license-file" type="file" display="code" -->

[LICENSE](./LICENSE)

### Dependency licenses
<!-- meshfox:node id="license-deps" type="link" -->

[LICENSE.canvas.md](./LICENSE.canvas.md)
