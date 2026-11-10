<!-- meshfox:canvas -->
# CLI
<!-- meshfox:node id="cli" -->

Part of the [meshfox documentation](./README.md).

A few commands, run for real against the installed `meshfox` binary (see [DEVELOPMENT.canvas.md](./DEVELOPMENT.canvas.md) for building/installing it). The output below is cached from actually invoking it (see "Cached output" above) rather than typed by hand, so it can't quietly drift from what the CLI does — and it doubles as an end-to-end check of the runnable-block feature itself: this section's own blocks are ordinary `name=`/`cache` fences, executed the same way any project's would be.

## CLI help
<!-- meshfox:node id="usage-help" -->

Run `meshfox spec` to print the full format specification (`SPEC.md`,
embedded in the binary at compile time) — not cached here since dumping it
verbatim into this section would nest the whole `.canvas.md` grammar inside
an example of the format it's describing.

```bash name="usage-help" cache
meshfox -h
```
<!-- meshfox:output name="usage-help" hash="d456ec0a" -->
````text
exit code: 0 · 17ms


 /\_/\
( ¬‿¬ )──●──●──●
  c c


CLI and local web viewer/editor for a meshfox canvas. Run `meshfox spec` for the full .canvas.md format specification.

Usage: meshfox [OPTIONS] [COMMAND]

Commands:
  stats          Summarize retained invocation journals (not all-time execution history)
  logs           Read a block's or chain's saved transcript. Follow never starts another run
  console        Observe an already running core; Ctrl-C detaches without stopping tasks
  run            Run one or more named code blocks
  configure      Interactively resolve every declared `meshfox:var` (see SPEC.md's "Variables") and save the answers to the on-disk cache (`.meshfox/<filename>.env`, next to the canvas file) so `run` doesn't have to ask again. Shows each variable's currently-resolved value as the prompt's own default — press Enter to keep it. Secret variables are never cached, so there's nothing for this to save for them; they're skipped here and asked for fresh at run time instead. Requires an interactive terminal
  session        Manage a canvas's *session*: what the core remembers between runs — which blocks already ran and still look unchanged (so a later `run` skips them as already-fresh dependencies), and the values a `form` fence submitted. It survives restarts, so a build step whose result depends on files rather than on its own text can look "fresh" forever; `meshfox run --fresh` forces one run, `session reset` forgets everything. See SPEC.md's "Session"
  secret         Manage values in the system secret store (`secret_store = "keychain"` in `.meshfox/config.toml`; macOS and Linux): `set`, `show` (prints a value only with `--reveal`), `rm`. See SPEC.md's "Secret store"
  prune          Find — and with `--yes` remove — the regenerable files meshfox leaves behind in a directory: tangled files (`.meshfox-tangle-*`) whose block or canvas is gone, `.meshfox/<canvas>.venv` and `.meshfox/<canvas>.node` environments whose canvas file is gone, and stale `meshfox-<pid>-*` temp files (older than a day) in the system temp dir. Only lists them unless `--yes` is given. A canvas with a running worker is skipped. Never touches run history (`.session.sqlite3`) or variable caches (`.env`) — delete those by hand. See also `meshfox secret prune` for the secret store
  mv             Move a canvas file, or a whole directory of canvases, together with what meshfox keeps for them: `.meshfox/<canvas>.env` and `.session.sqlite3` follow a renamed file (a moved directory takes its `.meshfox/` along), and secrets saved in the system store (`secret_store = "keychain"`) are filed under the new path. The Python/Node environments are deleted, not moved — they hold absolute paths and are rebuilt by the next run of their setup block. Nothing is overwritten: the destination must not exist. Refused while a worker is running for any of the canvases. Other canvases that refer to the moved one are listed, not rewritten
  env            List or delete the Python and Node environments meshfox builds for a canvas (`.meshfox/<canvas>.venv` and `.meshfox/<canvas>.node`): `list` shows them, `rm` deletes a canvas's so the next run of its setup block builds them again (e.g. after a Python upgrade). Unlike `prune`, `rm` works on canvases that still exist
  theme-preview  Preview the available TUI themes (built-in and from `.meshfox/themes/tui/`) in this terminal, so ANSI colors show in your own palette; `--html` writes an HTML page instead
  serve          Run the persistent coordinator: the headless, cross-platform counterpart of the macOS menu-bar daemon. Listens on the socket `server_socket` points at (or `--socket`; or the one systemd hands over under socket activation), gets-or-spawns a core per canvas and is controlled with `meshfox cores list|open|kill`
  cores          Inspect and control the cores (per-canvas workers) of the persistent coordinator `server_socket` points at: `ls`, `open`, `kill`
  create         Create a new, empty canvas file: just the `meshfox:canvas` marker followed by a lone root heading (`#`) named after the file itself (its name with a trailing `.canvas.md`/`.md` stripped). Fails if the file already exists — this never overwrites
  view           Start the local web UI: canvas view, run buttons. Opens read-only — running a block is always allowed, but click "Edit" in the browser to unlock dragging, resizing, saving layout, and persisting a `cache`d block's output back into the file
  tui            An ncurses-style terminal viewer — browse the node tree, read a node's rendered Markdown body (syntax-highlighted code, local images shown inline where the terminal supports it), and run blocks with live streamed output, right in the terminal. Same deps-chain/cache/`meshfox:var` handling as `meshfox run`/`meshfox view`. A `tty` block hands the real terminal over to it, same as `meshfox run`'s own `tty` handling. The tree/document panes' own mouse support covers clicking a tree row to select it (or its ▾/▸ marker to expand/collapse) and scrolling either pane; each row's title is also colored to match the node's own `color=`. `e` opens a fullscreen raw-source editor (vim-style modal input via `edtui`, meshfox-specific syntax highlighting, full mouse support — click to position the cursor, drag to select, scroll to move the viewport) on the selected node's own file — the terminal counterpart to the browser UI's Source mode. `Ctrl-f` switches between the document and any `include`d file; `Ctrl-n` turns the heading under the cursor into a node in one keystroke; `Ctrl-p` suggests attributes for the current `meshfox:node`/`meshfox:edge` comment or runnable-fence line, or, with the cursor inside a `tags=` value, tags already used elsewhere in the document. Still no *structural* editing beyond that (use `meshfox node ...` or the browser UI's Edit mode for that)
  mcp            An MCP stdio server giving an AI agent tool-call access to every canvas file under the current directory, without shelling out to this same binary. Takes no arguments — a host launches it the same way as any other stdio MCP server: `{"command": "meshfox", "args": ["mcp"]}`, and whichever directory it's started in becomes its root. Multi-canvas by design, in one process: `canvas_open`/`canvas_close`/`canvas_list` manage a registry of canvases, each served by its own file's worker (a hung worker fails only that canvas's calls) — resolved only under that root directory, never above it. Every other tool requires that `canvas_id` as its first argument, mirroring its single-canvas equivalent exactly: a stateful debug session (`debug_start`/`debug_send`/`debug_stop` — a persistent `bash` kept alive in a node/block's own resolved cwd/env, so a multi-step snippet's state — exported vars, files it wrote — survives between calls, unlike a one-shot `meshfox run`) and thin wrappers around the whole `node <op>` surface — every subcommand, not just a subset: `show`/`find` (find as structured JSON, CSS-selector matching, same as `node find`) and the mutating `add`/`meta`/`body`/`block`/`rm`/`mv`/`rename`/`set_id`/`edges`/ `move`/`reorder`. Deliberately does *not* attempt batch/ transactional multi-edit or optimistic-concurrency write conflicts (see TODO.canvas.md's own "MCP-редактирование файла"/"Оптимистичная конкурентность" — still open design questions, not implemented here) — every write here is the same immediate read-modify-write `node <op>` already does
  lsp            A Language Server Protocol server on stdio for editing canvases: diagnostics (the same checks as `validate`, minus `include` targets), a node outline, attribute/value/tag/id completion and code actions such as "turn this heading into a node". Takes no arguments — an editor launches it (`meshfox lsp`) and sends it the open buffers, so unsaved text is what gets analysed. It never reads or writes a canvas file itself; anything that needs the canvas's saved state goes through the file's worker
  validate       Validate that a file parses as a meshfox canvas — same checks `run`/`view` already do before touching anything (single root, no duplicate ids, no dangling `meshfox:edge` targets, `group`/ `file`/`link` body rules) — without executing anything or writing the file back. Exits non-zero on a parse error, so it's usable as a pre-commit/CI check
  check          Run every embedded ` ```starlark constraint ` fence's Starlark contract against the document (see `crate::constraint`/SPEC.md's "Constraint nodes") and report which passed. Distinct from `validate`: `validate` checks that the file *parses* as a well-formed canvas; `check` asks whether the document as a whole satisfies whatever rules its own constraint fences declare (e.g. "every node tagged `table` has exactly one `file` child") — implies `validate` first, since an unparseable file has no constraints to run. Resolves includes first (same as `validate`/`view`/`static`), so a constraint sees the fully resolved document — an `include` node's own dumped-in content included — same tree the web UI checks, not just this file in isolation. Exits non-zero if the file (or any include target) fails to parse, an include is broken, or any constraint fails, so it's usable as a pre-commit/CI check alongside (or instead of) `validate`
  render         Prints the Markdown each ` ```starlark render ` fence computes — the view the web UI and TUI show under its `starlark · name` header — so a computed view can be read and verified from a terminal or a script. Evaluates against the include-resolved document, same as `check` (which also fails on a render block that errors). With several blocks selected each is preceded by a `<!-- render: node/name -->` line. A block that fails prints its diagnostic to stderr; exits non-zero if any selected block failed or none matched
  undo           Reverts the most recent still-undoable edit to the canvas — any node/edge/reorder change, from any client (web UI, TUI, another CLI invocation, MCP). Routes through the running worker for this canvas (starting one if needed, same as `node <op>`), since undo history lives in the worker's own session database (`.meshfox/<canvas>.session.sqlite3`), not in this process. A no-op, not an error, when there's nothing left to undo
  redo           The mirror image of `undo`: reapplies the most recent still-redoable edit. A no-op, not an error, when there's nothing left to redo — including right after any fresh edit, which always drops whatever redo history existed before it
  history        Lists the last `--limit` applied edits plus the entire current redo tail (never capped by `--limit`), each with a human-readable summary and its own seq — or, with `--goto`, jumps directly to a specific one (a seq a plain `meshfox history` call just listed), undoing or redoing as many steps as that takes in one call
  list           Print every runnable code block in the canvas as an indented tree, each with a ready-to-paste `meshfox run <path...> <name>` — so you don't have to go spelunking through the file to find out what's runnable. Same raw-file-only scope as `run`/`validate` (no include resolution)
  static         Experimental: export a canvas as a static site. Resolves includes (same as `validate`/`view`), turns the canvas's node tree into a recursive `SiteData` (context key `site`) and hands it to a user-supplied Tera template. A node with no real, authored `x`/`y`/`width`/`height` gets no computed position at all — the template renders it as an ordinary nested HTML element and the *browser* lays it out and sizes it from its real content (no pre-computed/estimated pixels to get wrong); a node that does have all four real values keeps rendering at exactly that authored pixel position. A structural (parent/child) connector between two flow-positioned nodes is drawn in pure CSS (they're always DOM-adjacent); everything else — a `meshfox:edge` cross-reference, or a structural edge touching a real-positioned node — is left for a small non-interactive JS pass in the template to measure and draw. Every `*.tera` file in `--template` (except one whose basename starts with `_`, a partial meant to be `{% import %}`ed rather than rendered standalone) is rendered and written to `--out` at the same relative path minus `.tera`; every other file is copied verbatim (CSS, fonts, ...) — except `template.toml` itself, the template's own config file (optional, but if present it must declare `api_version`, and unknown keys are an error; a template with none gets no `base_url`/`links_base_url` and no `icons`), read from `--template`'s own directory and never copied to `--out`. Every page is HTML-escaped automatically; `html_body` and `script_json` output are the markup a template marks `| safe`. What a template receives and must do is described in TEMPLATES.md. A local image referenced from a node's Markdown body is copied alongside the output automatically; a `file`-type node's `display="code"` target is read once and inlined into the HTML directly (nothing left to fetch once static). A plain `file`-node target (not `display="code"`) is left as an unresolved link unless `--copy-files` is passed — see that flag's own help. See `site-template/` in this repo for a working example, including its own `template.toml`
  pdf            Experimental: export a canvas as a PDF, via a real (headless) Chrome/Chromium — a system install is used if one can be found (`CHROME` env var, common binary names on `PATH`, well-known install locations); otherwise a pinned Chromium build is downloaded once and cached for next time. Two kinds of pages, both by default: a canvas page — every node at its own box, full body always shown (never folded, regardless of the document's own fold settings); a real authored `x`/`y`/`width` is kept exactly, everything else auto-laid-out the same way the live web UI would place it, but height always auto-sizes to the node's own real content, authored or not, so nothing is ever clipped — printed at true 1:1 CSS-px scale on its own custom-sized page rather than scaled to fit a fixed paper size, with connectors for both structural parent/child and `meshfox:edge` cross-references; then the full node tree in flow/document order (headings by depth, tags, body, target, standard A4 pagination)
  pack           Pack a canvas and everything it reads into one `.canvas.tar.xz` (tar, compressed as one xz stream) to hand to someone else: the canvas, its includes, images and linked files, and the canvases it links to (`--shallow`: not those). Only what the canvas references goes in, and every reference must stay below the pack's root (the canvas's own directory, or the directory `--root` names) — one that leads outside it, to a directory, to a missing file or to a variable or `@tmp` fails the command and writes nothing, and says which root would make it work. The canvas's worker does the reading, with the same file access every other front end has, in a private read-only worker, so nothing is written next to the canvas. `--list` shows what would go in; `--root DIR` widens the pack's root; `-o` names the output; an existing file is replaced only with `-f`. See "Packing a canvas"
  unpack         Extract a `.canvas.tar.xz` into a new (or empty) directory after checking the whole pack — manifest, sizes, BLAKE3 hashes, paths, limits; a pack that fails a check writes nothing and nothing is overwritten. Prints the entry canvas's path. The result is an ordinary directory (not read-only, not `no-exec`). `meshfox view PACK` opens a pack without extracting it, read-only and with nothing in it running
  node           Structural edits to individual nodes in a canvas file: add, move, rename, delete, or set a node's body/position/style/edges — the CLI counterpart to the web UI's Edit-mode node operations (the same `mdcanvas` surgical patches `meshfox view`'s `/api/nodes*` routes use), for scripting/CI or whenever a hand-rewrite would risk getting heading depth, sibling order, or dangling-edge cleanup wrong. Every subcommand validates the fully-patched document still parses before writing it back, same as every other mutating command here
  spec           Print the full .canvas.md format specification (SPEC.md, embedded in this binary at compile time) — the canonical reference for the format, available offline wherever `meshfox` is installed
  check-updates  Check github.com/orofarne/meshfox's releases for a newer version than this binary and, if one exists, offer to download and install it in place (replacing the running executable). A CLI bundled in Meshfox.app is updated with the whole app via Meshfox.pkg instead. Package-managed installations must be updated through their package manager; --yes does not override this restriction. A no-op if this build wasn't made from a release tag (e.g. a local/dev build) — there's no version to compare against a release with, so it just says so and exits
  completions    Print a shell completion script to stdout. Source it directly or write it to the completions directory your shell scans on startup, e.g. `meshfox completions zsh > ~/.zfunc/_meshfox` (with `~/.zfunc` on `fpath`), or `meshfox completions bash > /etc/bash_completion.d/meshfox`
  help           Print this message or the help of the given subcommand(s)

Options:
      --agent-help  Print usage guidance for AI coding agents (when to prefer `node` subcommands over hand-editing, non-interactive `run`, etc.) and exit
      --read-only   Serve canvases read-only: nothing is written, whether or not the files are writable. Blocks still run. Cores started this way are private to this process, so any number of them can serve one canvas side by side. Same as `MESHFOX_READ_ONLY=1`
      --no-exec     Serve canvases without running anything: no blocks, services, interactive sessions or debug shells. Viewing and editing still work. Same as `MESHFOX_NO_EXEC=1`
  -h, --help        Print help
  -V, --version     Print version

Website: https://meshfox.orofarne.net/

Agent Usage:
  If you are an AI coding agent, run `meshfox --agent-help` before hand-editing a
  .canvas.md file. It covers when to prefer `meshfox node <verb>` over a raw text
  edit, how to run non-interactively, and other guidance not covered above.
````
<!-- /meshfox:output -->

### Node commands
<!-- meshfox:node id="usage-node" updatedAt="2026-08-29T07:32:30.452469Z" -->
`meshfox node <op>` exposes the same per-node surgical patches
(`insert_child_node`, `delete_node`, `reparent_node`, `set_node_title`,
`set_node_body`, `set_node_meta`, `set_node_edges`, `reorder_by_position` —
all in `mdcanvas`) that back the web UI's Edit-mode operations over its
`/api/nodes*` routes — so a structural change (adding a child at the right
heading depth, moving a subtree without breaking its nesting, deleting a
node without leaving a dangling `meshfox:edge` behind) can be scripted or
run in CI without going through the browser, and without hand-rewriting
Markdown heading levels yourself. Every subcommand takes an optional
`--canvas <path>` (auto-discovered like `validate`/`list` when omitted)
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
- `body <node-id> --base-rev <rev> [--file <path>] [--merge]` — replace a node's whole body, from a file or stdin (`--file -` is stdin too); `--base-rev` is the `body-rev` `show` printed, and a body that changed since is refused with its current text and revision (`append` needs none). With `--merge` the worker instead finds the body you read in the canvas's undo history and three-way merges (by line) your text with the current one: a clean merge is written and printed; overlapping edits write nothing and report the overlapping lines plus the body with conflict markers
- `append <node-id> [--file <path>]` — append to a node's existing body, from a file or stdin (`--file -` is stdin too), without reading the current body back first just to resend it unchanged; bumps `updatedAt=` the same way `body` does (see SPEC.md's "Timestamps")
- `meta <node-id> [--x --y --w --h --color --type --display --lang --interpreter --fold --created-at]` — set position/size/style; an omitted flag keeps the node's current value; `--w`/`--h` on a `group` are rejected (its box is always derived from its members), but `--x`/`--y` are accepted — a group's own position is a real anchor its members' own `x`/`y` are relative to (see SPEC.md); `--fold true`/`--fold false` sets a per-node fold override, `--fold default` clears it back to following the document's own default (see SPEC.md's "Options" section); `--created-at` overrides `createdAt=` (RFC3339), mainly for backfilling/importing existing data — meshfox stamps a fresh one automatically on `add` (see SPEC.md's "Timestamps")
- `edges <node-id> [--add <id>]... [--remove <id>]...` — add or remove a node's extra (`meshfox:edge`) parents; never replaces the whole set
- `reorder` — resync sibling heading order in the file to match current x/y, the same resync the server runs on every UI save
- `show <node-id>` — print a node's parent/children/extra-parents/type/position/created/updated (read-only)

```bash name="usage-node" cache
meshfox node -h
```
<!-- meshfox:output name="usage-node" hash="dba20bec" -->
```text
exit code: 0 · 22ms

Structural edits to individual nodes in a canvas file: add, move, rename, delete, or set a node's body/position/style/edges — the CLI counterpart to the web UI's Edit-mode node operations (the same `mdcanvas` surgical patches `meshfox view`'s `/api/nodes*` routes use), for scripting/CI or whenever a hand-rewrite would risk getting heading depth, sibling order, or dangling-edge cleanup wrong. Every subcommand validates the fully-patched document still parses before writing it back, same as every other mutating command here

Usage: meshfox node <COMMAND>

Commands:
  add      Add a new child node under `parent-id`, as the last item in its existing subtree (`mdcanvas::insert_child_node`) — same as the web UI's "add child" button. Empty-bodied and unpositioned by default, same as before `--body-file`/the position/style flags below existed — either can still be set later with `node body`/`node meta` instead, if not given here. Prints the new node's id: a slug of `title`, de-duplicated against every id already in the file
  rm       Delete a node. By default the whole subtree goes with it (`mdcanvas::delete_node`), and any `meshfox:edge from="..."` elsewhere that pointed into the deleted subtree is dropped too, so the file can't be left with a dangling reference. `--keep-children` instead deletes just this node, promoting its direct children (and everything under them, untouched otherwise) to its own former parent (`mdcanvas::delete_node_reparent_children`). Refuses to delete the root either way
  mv       Move a node to a new structural parent (`mdcanvas::reparent_node`). That core function only ever promotes an *existing* extra-parent edge to structural parent — the web UI's two-step dance (drag a new edge onto the node, then promote it) — so this adds the `meshfox:edge from="new-parent-id"` line itself first, making the move a single atomic step from the CLI. Refuses to move the root, or to move a node into itself or one of its own descendants (would make the tree cyclic)
  rename   Rename a node's heading text, leaving its id, heading level, and body untouched (`mdcanvas::set_node_title`) — a node's id is pinned the first time it's written and never follows later title edits
  set-id   Change a node's id (`mdcanvas::rename_node_id`) — the stable handle used for CLI/API addressing, `meshfox:edge from=`/`parent=` references, and `deps="node-id/block"` fence references. Rewrites every reference to the old id it can find: other nodes' `parent=` and `meshfox:edge from=` attributes are updated exactly (they're structurally tracked by the parser), and `deps=` references are updated best-effort (plain text, not parser-validated — run `meshfox validate` afterward to catch anything this missed, e.g. a reference that was already stale). Fails if `new-id` is empty, contains a `"` character, or is already used by another node
  body     Replace a node's whole Markdown body (`mdcanvas::set_node_body`) — what the web UI's in-node editor would send, if it had one yet (see README's roadmap; for now the UI can reposition and run, not edit text). For a `file`/`link` node the body is its one Markdown link (`[title](target)`); a `group` node's body must stay empty. Requires `--base-rev` (the `body-rev` `node show` printed): a body that changed since it was read is not overwritten, the error carries the current body and revision to retry with. Reads the new body from `--file`, or from stdin if `--file` is omitted
  append   Appends to the end of a node's existing Markdown body (`mdcanvas::append_node_body`) — after whatever's already there, still before its first child's own heading — without having to first read the current body back just to hand it to `node body` unchanged. Reads the text to append from `--file`, or from stdin if omitted, same convention as `node body`. Bumps `updatedAt=` the same way `node body` does (see SPEC.md's "Timestamps"), since it's implemented on top of the same `set_node_body`
  block    Rewrites just one runnable fence's own info-string attributes (and, optionally, its code — `--code-file`/`--code -`) inside a node (`mdcanvas::set_fence_attrs`) — every other fence in the node, the rest of its body, and the rest of the document are left byte-for- byte untouched. Unlike `node body`, never needs the whole node body reconstructed just to flip one flag on one block. `block-name` is resolved the same way `meshfox run`/`meshfox list` already do (explicit `name=`, the sole unnamed fence, or an explicit `default` flag) — see SPEC.md's "Runnable code fences". Any field left entirely unset keeps its current value; a paired `--no-`/`--clear-` flag explicitly removes it instead. `--deps` is validated (existing targets, no cycle) against the whole document right away, not deferred to a separate `meshfox validate` For a runnable `file` node the block is its implicit one (`<node> <node>`) and the attributes are the ones on its `meshfox:node` line — see SPEC.md's "Runnable file nodes" for what it refuses
  meta     Set a node's position/size/style fields (`mdcanvas::set_node_meta`) — `--x`/`--y`/`--w`/`--h` for a manual position/size override, `--color`/`--type`/`--display`/`--lang`/`--interpreter`/`--tags` for style/type. Any field left unset keeps its current value. `group` nodes never store a *size* (its box is always derived from its children instead), so `--w`/`--h` are rejected for one — but a group's own *position* is a real anchor its members' own `x`/`y` are relative to, so `--x`/`--y` is allowed on a group same as any other node
  edges    Add or remove extra incoming edges on a node (`meshfox:edge from="..."` lines, `mdcanvas::set_node_edges`) — the non-structural, non-nesting cross-references JSON Canvas-style graphs use. Never replaces the whole set: `--add` leaves an edge that's already there exactly as it is (label, route and all) and `--remove` drops just the ones named
  move     Moves a node's whole subtree to sit immediately before or after another sibling under the same structural parent (`mdcanvas::move_sibling`) — the on-disk heading order is a node's *only* sibling order until it also has a real `x`/`y` (see `node reorder`), so this is the CLI's way to change it directly instead of hand-editing the file. Exactly one of `--before`/`--after` is required. Fails if the two nodes aren't siblings — moving to a *different* parent's children is `node mv`'s job
  reorder  Reorder every parent's direct children in the file to match their canvas layout (`mdcanvas::reorder_by_position`, sorted by `y` then `x` among ties) — the same resync the server runs on every save from the web UI, exposed standalone for whenever positions changed by hand (or via `node meta`) and the on-disk heading order should catch up to match what's actually drawn
  show     Print one node's parent, children, extra parents, type, and position/style fields — a read-only lookup, since eyeballing the tree shape directly from the file gets harder the deeper it nests
  find     Finds every node matching a CSS selector, a text substring, and/or a created/updated date range — the three axes AND together; any may be omitted. `selector` alone (its original form, still the default when nothing else is given) behaves byte-for-byte as before: the CSS engine stays the right tool for structure (`#todo > .bag`, tag/type/color matching, arbitrary-depth nesting) — `--text`/the date flags are independent predicates layered next to it, not a CSS extension, since CSS selectors have no substring-search or numeric-range primitives to begin with. The tree maps onto CSS almost directly: a node is an element, each tag is a class (`.bag`), `id`/`type`/`color` are ordinary attributes (`[type="file"]`), and structural nesting is DOM nesting — `#todo > .bag` for direct children, `#todo .bag` for descendants at any depth. Matching runs against a synthetic HTML document built from the canvas tree (never against real rendered content) via `scraper` — the same CSS engine a browser uses, not a bespoke query language to learn
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
<!-- meshfox:output name="node-add-example" hash="ac207f97" -->
```text
exit code: 0 · 23ms

meshfox node add: added "regression-test" under "tests" in /tmp/meshfox-node-demo.canvas.md
id: regression-test
title: Regression Test
type: text
parent: tests
children: (none)
extra parents: (none)
position: x=? y=? w=? h=?
created: 2026-08-28T22:14:34.735736Z
updated: 2026-08-28T22:14:34.735736Z
```
<!-- /meshfox:output -->

## Running a block
<!-- meshfox:node id="usage-run" -->

Runs the `smoke` block in `examples/hello.canvas.md` for real, updating its cached output in place — the same surgical patch described above, not a simulation:

```bash name="run-example" cache
meshfox examples/hello.canvas.md run tests smoke-test smoke
```
<!-- meshfox:output name="run-example" -->
```text
exit code: 0

==> smoke
hello from meshfox
Sat Aug 15 18:01:48 +04 2026
(exit 0)
```
<!-- /meshfox:output -->

## Interactive (`tty`) blocks
<!-- meshfox:node id="usage-tty" -->

A block flagged `tty` hands its process a real interactive terminal instead of the captured/streamed output every other block gets — for anything that actually needs to talk to a terminal: an editor, a REPL, `ssh`. See SPEC.md's "Interactive (`tty`) blocks" for the full writeup, including why it's mutually exclusive with `cache` (nothing here to freeze into an output block, so — unlike every other example in this section — there's no cached output shown below).

```bash name="vim-demo" tty
vim
```

Run it from a real terminal — `meshfox CLI.canvas.md run usage-tty vim-demo` — and it drops you straight into `vim` editing that scratch file, same as running `vim` directly would; `:wq` (or `:q!`) hands the terminal back same as it always does. `meshfox run` checks stdin/stdout are actually a terminal before starting a `tty` block and errors out otherwise, rather than hanging a script or CI job that happens to reach one. The web UI runs the same block over a real pseudo-terminal instead: clicking "run vim-demo" in `meshfox view` opens it as a floating terminal panel over the canvas rather than filling in the node's own inline output area.

## Saved logs (`logs`)
<!-- meshfox:node id="saved-logs-logs" -->

`meshfox logs` reads the latest saved chain transcript. Add a block address to see only its latest participation, or `--chain` to show the whole invocation requested for that target:

```text
meshfox project.canvas.md logs
meshfox project.canvas.md logs build
meshfox project.canvas.md logs build --chain
meshfox project.canvas.md logs --list
meshfox project.canvas.md logs --id 42 --effective
meshfox project.canvas.md logs --id 42 --block tests smoke
meshfox project.canvas.md logs build --chain --follow
```

The default transcript shows what happened in that invocation, including skips and steps not reached. `--effective` expands the exact earlier block output used by a skipped step, labelled with its original run ID and time; later runs cannot change it. `--follow` replays and follows just the selected invocation until it ends. Ctrl-C detaches without stopping it. Use `meshfox console` to observe successive runs and ongoing service logs instead.

`--json` emits NDJSON. History is read through the worker API and survives worker restarts; transcripts start with the version that introduced `logs`. TTY terminal bytes are not recorded. `[session] max_chain_runs` (default 100) and `max_chain_output_bytes` (default 1 MiB) bound retention independently of per-block history. See SPEC.md's "Invocation transcripts" for provenance, truncation and storage behavior.

`meshfox project.canvas.md run -d build` (or `--detach`) requires a configured, reachable coordinator (`server_socket`); it never falls back to a standalone core or starts a background watcher. It resolves inputs and confirmations, waits for the worker to accept the chain, prints its numeric ID, and returns. Exit code 0 means accepted; inspect the final outcome with `meshfox project.canvas.md logs --id 42 --follow`, or watch the canvas with `console`. Services keep running according to their normal lifecycle; run-scoped leases and deferred cleanup still finish in the worker. TTY chains (including TTY cleanup) are rejected. For a comma-separated block list, each target is admitted independently and prints one ID; they may execute concurrently, and a later admission error does not cancel an earlier accepted chain.

## Saved-journal statistics (`stats`)
<!-- meshfox:node id="saved-journal-statistics-stats" -->

`meshfox stats` summarizes the invocation journals still retained for a canvas. The report shows its actual history coverage, outcomes, median/p95 durations, skips, slowest blocks and frequently failing blocks:

```text
meshfox project.canvas.md stats
meshfox project.canvas.md stats build --chain
meshfox project.canvas.md stats tests smoke
meshfox project.canvas.md stats --since 7d
meshfox project.canvas.md stats --json
```

A block address selects that block's participation across chains; `--chain` selects chains requested for the target and includes their dependency steps. The block view also shows recent outcomes. `--since` filters invocation start time and accepts durations or RFC3339 timestamps.

Coverage is explicitly limited to saved journals: requesting a month does not recover rotated-out history. Skips, cancellations, interruptions, running steps and service startup/reuse are shown separately from successful executions. Median and p95 use completed success/failure durations, with chain time separate from step time. Reading statistics through the worker API executes no blocks and creates no persistent aggregates. See SPEC.md's "Saved-journal statistics" for counting and percentile rules.

## Chain status (`status`)
<!-- meshfox:node id="chain-status-status" -->

`meshfox status` folds the saved chain transcript that `logs` reads into one row per step — what has finished, what is running, what is still pending and what failed — so a long chain such as a full check can be watched without scrolling its output:

```text
meshfox project.canvas.md status
meshfox project.canvas.md status --id 42
meshfox project.canvas.md status build --chain
meshfox project.canvas.md status --json
meshfox project.canvas.md status --watch --json
```

Each step is `pending`, `running`, `ready` (a service that reported readiness; it has no end of its own), `succeeded`, `failed`, `skipped`, `killed` or `not-started`, with its duration and exit code. Totals are recognised from `cargo test`, Playwright and `node --test` summary lines in the step's recorded output, so a test step reads `2228 passed, 0 failed`. A failed or killed step lists its failing tests, or the last lines of its output, under `Failed:`. It selects a chain the same way `logs` does and goes through the worker; it never runs anything.

The exit code is 0 for a running or succeeded chain, 1 for a failed, killed or interrupted one and 2 when no chain can be read, so `--watch` — which keeps polling until the chain ends and then exits with its code — works as a plain shell gate.

`--json` prints no terminal formatting. Without `--watch` it is one `snapshot` object (`run`, `steps`, `current`, `failures`). With `--watch` it is NDJSON: a first `snapshot`, a `step` line whenever a step changes state, a `failure` line when a step first fails (with `tests` and `tail`) and a final `done` line with the run and its `exitCode`, which suits a monitor or an agent far better than grepping `logs`. Output stored per chain is bounded by `max_chain_output_bytes`; when it was truncated (`run.truncated`) the test totals may be incomplete.

## Packing a canvas

`meshfox pack` turns a canvas and everything it reads into one file you can mail, attach or archive, and `meshfox view` (or the [browser editor](#online-editor-in-the-browser)) opens it without unpacking by hand:

```sh
meshfox report.canvas.md pack                 # -> report.canvas.tar.xz
meshfox report.canvas.md pack --list          # what would go in, nothing written
meshfox report.canvas.md pack --shallow       # not the canvases it links to
meshfox co2/README.canvas.md pack --root .      # ...below this directory: it may show ../docs/x.png
meshfox view report.canvas.tar.xz             # read-only; nothing in it runs
meshfox unpack report.canvas.tar.xz           # -> ./report/ (new or empty directory)
```

The file is an ordinary tar archive compressed as one xz stream (`tar xf` opens it). It holds the canvas, its `include` targets, the images and links in its Markdown, its `file` nodes' targets, and the canvases they point to, followed the same way. Nothing else: no `.meshfox/` state, no files only a block wrote, no directory walked wholesale. Cached output already in the canvas's text travels with it. A manifest at the front records every file's size and BLAKE3 hash; whoever opens the pack checks all of it, and refuses a pack that was cut off, tampered with or that would expand beyond the limits (20 000 files, 256 MiB for one, 512 MiB in all).

A pack is complete or it does not exist. A reference that leaves the pack's root (an absolute path, a symbolic link that goes out, or `../` above the root), points at a directory or at a file that is missing, or names a `dir` variable or `@tmp` fails the command, naming the node and the path.

The root is the canvas's own directory unless you say otherwise. When a canvas reaches into a sibling directory (`esp32/co2-01/README.canvas.md` showing `../docs/sensor.webp`), widen it: `meshfox esp32/co2-01/README.canvas.md pack --root esp32`. The pack then keeps the layout, with the canvas at `co2-01/README.canvas.md` and the picture at `docs/sensor.webp`, and nothing in any file is rewritten. The error for a reference that leaves the root names the `--root` that would cover it. `meshfox view` and the browser editor open such a pack without further ado; an extracted copy (`meshfox unpack`) is an ordinary directory, where meshfox reads `../docs` only if `[files] allow = ["."]` says so, and `unpack` tells you.

A pack is someone else's canvas, and a canvas can run code. `meshfox view PACK` therefore always serves it read-only and with nothing running (`--read-only --no-exec`, see "Restricting a core" in SPEC.md), handed on to any canvas opened from it; the browser editor never runs anything. `meshfox unpack` gives you an ordinary directory, with no such restrictions, once you have decided to trust it.
