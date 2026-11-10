<!-- meshfox:canvas -->
# Reference
<!-- meshfox:node id="reference" -->

Part of the [meshfox documentation](./README.md). The canvas file format, its Markdown extensions, layout, variables, constraints, configuration and syntax highlighting.

## File format
<!-- meshfox:node id="file-format" type="file" -->

[SPEC.md](./SPEC.md)

## Markdown extensions
<!-- meshfox:node id="markdown-extensions-doc" type="file" -->

[MARKDOWN.md](./MARKDOWN.md)

## Auto-layout
<!-- meshfox:node id="auto-layout" -->

Nobody has to type `x`/`y`/`w`/`h` by hand. The web UI lays out anything still unpositioned live, in the browser.

`GET /api/canvas` sends exactly what's in the file — no computed suggestion, no `suggestedX`/etc. over the wire. `web/src/autolayout.ts` fills in a box client-side for anything still missing a real position: sections (root and its direct children) read top-to-bottom with just a small nudge to the right, same as a document's title followed by its headings — not yet a real indent. From there down it's a classic indented tree view, the same shape as a file tree or a collapsed outline: each section's own content steps fully to the right of its parent, siblings at a given depth stack vertically without overlapping, and further nesting keeps stepping right from there, with a `group`'s box always the bounding box of its resolved members. Width is tier-based: root and its direct children share one width, `60%` of the viewport; everything deeper gets `55%`, uniformly regardless of how much deeper. Viewport width is read once, when the canvas loads — it doesn't recompute on window resize. Height is never estimated: it comes from React Flow's own measurement of each node's actual rendered content, live — the layout self-corrects as soon as a real measurement lands (and again later, e.g. as a running block's output grows). A depth-≥2 node additionally gets a `max-height` cap so one long block can't drag a whole subtree far from its parent; past the cap it scrolls internally (`.mesh-node-body`'s existing `overflow: auto`) instead of growing the box further. None of this is ever written to the file just from loading it — a node only gets its box persisted once it's actually been dragged/resized (see `touchedNodeIds` in `App.tsx`).

Edit mode's toolbar has an **Auto-layout** button that clears every non-group node's stored `x`/`y`/`w`/`h` in the file outright (`POST /api/canvas/clear-layout`), reverting the whole document to auto-placed — behind a confirmation dialog, since it can't be undone from the UI. Useful for backing out of a bunch of hand-placed positions and letting the client lay everything out fresh.

Edit mode's toolbar also has an **⚙ options** button, next to Auto-layout: it toggles `meshfox:option` declarations — `unfold` (flips whether the canvas opens with every subtree expanded or folded to a compact outline by default) and `auto-timestamps` (opts the document in to automatic `createdAt`/`updatedAt` stamping, off by default — see SPEC.md's "Timestamps") — via `PUT /api/options`, writing the same comment hand-editing would. See SPEC.md's "Options".

## Variables
<!-- meshfox:node id="variables" -->

Document-scoped config values a canvas wants from whoever runs it — declared once as `<!-- meshfox:var ... -->` comments in the root node (the project's own [DEVELOPMENT.canvas.md](./DEVELOPMENT.canvas.md) declares exactly one, `INSTALL_PATH`, in its root node — invisible there since it's an HTML comment, same as every other bit of meshfox bookkeeping). Declaring one doesn't put it in any block's environment by itself, though — a block has to opt in with its own `env=` fence attribute to actually reference it (`env="$INSTALL_PATH"`, see "Install" in that file), and only blocks that do ever resolve or prompt for anything: running any *other* block in that file never asks about `INSTALL_PATH`, however many blocks elsewhere might use it. Asked for interactively the first time some block's `env=` actually needs it, then remembered in a local `.meshfox/<filename>.env` cache (`.gitignore`d, analogous to CMake's `CMakeCache.txt`) so it's not asked for again by any block referencing it afterward. `meshfox configure` walks every declared variable up front regardless of `env=` usage; `meshfox run --set NAME=value` supplies one non-interactively (e.g. for CI); the web UI shows a small form in place of a prompt, scoped to just the clicked block's own chain, only for whatever isn't already resolved. See SPEC.md's "Variables" for the full writeup, including `secret` (never cached, always re-asked) and the `int`/`bool`/`select`/`dir`/`file` types.

## Constraints
<!-- meshfox:node id="constraints" -->

A ` ```starlark constraint ` fence is a sandboxed Starlark contract living right in a node's own Markdown body — a way to assert invariants a canvas should hold (e.g. "every node tagged `table` has exactly one `file` child"), checked by `meshfox check` rather than only by convention. See SPEC.md's "Constraint fences" (linked above, under "File format") for the full reference. The worked example below actually runs — `meshfox check examples/constraints.canvas.md` from the repo root — and shows the newest piece: a constraint reading a `file`-type node's own already-declared target (`.content()`/`.json()`/`.yaml()`/`.toml()`/`.csv()`), the same mechanism `LICENSE.canvas.md` below uses for real, to keep this project's own dependency tables honest.

`starlark render` uses the same document API to generate Markdown, such as a live table of node counts by tag. Its source starts collapsed in WebUI and TUI; the result updates with document changes and is never written back to the source file. `md.table()`, `md.text()`, `md.link()`, `md.code()` and `md.raw()` return typed `MarkdownFragment` values so builders can compose markup without escaping it twice. See SPEC.md's "Computed Starlark views" and the tag-summary block in the example below.

### Worked example
<!-- meshfox:node id="constraints-example" type="file" -->

[examples/constraints.canvas.md](./examples/constraints.canvas.md)

## Configuration
<!-- meshfox:node id="configuration" -->

meshfox's own settings — facts about a machine or a project, as opposed to a document's `meshfox:var`s (see "Variables") — live in optional `.meshfox/config.toml` files. meshfox reads `~/.meshfox/config.toml` first, then the configs in every ancestor of the canvas directory, from the filesystem root down to the canvas itself. The canvas directory is canonicalized before searching, and the global config is read only once.

Nearer settings win **key by key, at every nesting level**; arrays replace the inherited array. `[[env]]` sections are resolved per file and overlaid per variable, so child configs can override individual shared defaults. A missing file counts as empty. Ordinary settings retain their permissive handling of unreadable/invalid TOML; file access reports such errors and fails closed. Files are re-read on use, so edits apply without restarting (except where a setting below says otherwise).

To share assets between sibling canvases, put this in their parent's `.meshfox/config.toml`:

```toml
[files]
allow = ["docs"]
```

A canvas in `child/` can then use `![sensor](../docs/sensor.webp)` or link to `../docs/datasheet.pdf`, with no variable declaration. Paths in `allow` are relative to the directory owning `.meshfox` (not `.meshfox` itself); absolute paths and `~/...` also work. `allow = ["."]` permits the owner's whole tree. The nearest explicit `allow` **replaces** inherited grants; `allow = []` removes those grants, while the canvas directory and separately trusted `dir`/`file` variable values remain accessible. Every file read is checked through `FileAccess` after resolving symlinks, including Markdown images/links, file nodes, includes and export. Configuration grants are read permissions; they do not sandbox runnable code.

| Key | Default | What it does |
| --- | --- | --- |
| `server_socket` | unset | Control socket of an external coordinator (the macOS menu-bar app, see "macOS menu-bar app"). When set, every core-launching command (`view`, `tui`, `run`, `node <op>`, MCP `debug_*`) becomes a client of that daemon instead of spawning its own worker; an empty local value turns a global one off for that project. `MESHFOX_SERVER_SOCKET` overrides it for one invocation (an empty value forces it off). |
| `[process_env]` | empty | Extra environment variables for every process a block spawns (plain, `tty` and `@`-macro interpreters). A value may reference other variables — `PATH = "$HOME/.cargo/bin:$PATH"` extends rather than replaces — which is how a worker started by the macOS daemon (launchd's bare environment) finds `npm`/`cargo`. String values only. |
| `secret_store` | `plaintext` | Where `secret` variables are saved: `plaintext` (the `.meshfox/*.env` cache, as always) or `keychain` (the system secret store — the **macOS Keychain**, or the **Secret Service** on Linux). Explicit only, no auto mode; an unknown value is an error. Also enables `secrets = [...]` in `[[env]]` and `meshfox secret set/show/rm`. See SPEC.md's "Secret store". |
| `[files] allow` | `[]` | Additional readable paths, relative to the configuration owner. Inherited by nested canvases; the nearest explicit array replaces the inherited list. |
| `[[env]]` | none | Shared defaults for `meshfox:var` resolution across canvases, optionally scoped with `path=`. Not a flat setting, so it isn't merged key by key; see SPEC.md's "Shared/global config (`[[env]]`)". |
| `[interpreters.agent] provider` | `claude` | Which agent CLI the `@agent` macro calls (`claude`, `codex`, `opencode`, or `custom` with your own `command`/`tty_command`) — see "Calling an AI agent from a block" in [HOWTO.canvas.md](./HOWTO.canvas.md). Every scalar key in the file is also exported to a `@`-macro's process as `MESHFOX_CONFIG_<PATH>` (`MESHFOX_CONFIG_INTERPRETERS_AGENT_PROVIDER`), so a hand-written `interpreter=` script can read it too. |
| `[interpreters.python_venv] python` | unset | Which Python builds the venv for `@python_venv` — a command name or path, e.g. `"python3.12"`; unset uses the `requires-python` of a `toml` block's pyproject, else the `python3` on `PATH`. A venv that no longer matches it is rebuilt. See "A canvas's own Python environment" in [HOWTO.canvas.md](./HOWTO.canvas.md) and SPEC.md's "Choosing the Python for `@python_venv`". |
| `[tui] theme` | `darkfox` | Color theme of the terminal viewer — interface, SVG output and the editor's syntax colors. A built-in name or a file `<name>.toml` in `.meshfox/themes/tui/` (next to the canvas, or `~/.meshfox/themes/tui/`) — see "Terminal UI" in [INTERFACES.canvas.md](./INTERFACES.canvas.md). An unknown or broken theme falls back to `darkfox` and says why in the status line. |
| `[tui] output_height_pct` | `20` | Initial height of the terminal viewer's Output pane, in percent of the terminal (`1`–`90`; anything else is ignored). Dragging the pane's border still changes it for the session. |
| `[tui] editor` | none | Command that `e` runs in the terminal viewer instead of the built-in source editor, e.g. `nvim +{line} {file}` — see "External editor" under "Terminal UI" in [INTERFACES.canvas.md](./INTERFACES.canvas.md). Unset keeps the built-in editor (`$EDITOR` is not consulted). |
| `[tui.open]` / `[web.open]` / `[open]` | none | Per-file-type commands for opening a `file` node (`o` in the terminal viewer, ↗ in the browser UI), e.g. `"xls,xlsx,*.ods" = "sc-im {file}"` — see "Opening files" under "Terminal UI" in [INTERFACES.canvas.md](./INTERFACES.canvas.md). Without a matching entry the file opens in the OS default application. |
| `[session] max_output_bytes` | `262144` (256 KiB) | How much of *each* finished run's output the core keeps in `.meshfox/<canvas>.session.sqlite3`; past the limit the oldest lines of that run are dropped, keeping the tail. `0` stores no output at all (runs, freshness and variables are still kept). Read once when the core starts, so restart it to apply a change. |
| `[tables] cache_max_bytes` | `4294967296` (4 GiB) | Most disk `display="table"` caches may take — the imported tables plus DuckDB's spill files, in `.meshfox/tables/` (or, for a read-only canvas, an owner-only per-process directory under the system temp dir). Past it the least recently used tables not being imported are evicted; a single table that alone doesn't fit is refused with an error naming this key. |
| `[tables] duckdb_path` | unset | The `duckdb` executable `display="table"` drives when it isn't on `PATH` (the `MESHFOX_DUCKDB` environment variable overrides it). Looked up in this order: the variable, this key, `PATH`, then `~/.duckdb/cli/latest/duckdb`, `/opt/homebrew/bin`, `/usr/local/bin`, `/usr/bin`. |
| `[session] max_runs_per_block` | `10` | How many finished runs of one block the core keeps (run record and stored output together); when a run finishes, the oldest beyond this are rotated out. At most `max_runs_per_block × max_output_bytes` of output per block. Minimum `1`. Read once when the core starts. |

When a block's effective `LANG` is unset or empty, Meshfox selects an installed UTF-8 locale (`C.UTF-8`, then `en_US.UTF-8`). If neither is available, it leaves `LANG` unchanged. Explicit `LANG`, `LC_ALL`, and other `LC_*` settings retain their normal precedence.

#### What survives a core restart

Alongside the undo history, the session database (`.meshfox/<canvas>.session.sqlite3`, covered by the repo's `.meshfox/` ignore) keeps what a restarted core needs to pick up where it left off:

- **Run history**: the last `max_runs_per_block` finished runs of every block — exit code, start and end time, and output (each bounded by `max_output_bytes`). In the browser UI the clock button in a block's header (next to `‹/›`) opens the block's run history as a modal: every kept run with its start time, exit code, duration and a `stale` mark on the left, the selected run's output on the right (↑/↓ to move, Esc to close). A reloaded tab shows each block's latest current run again. A `tty` block has a history too — exit code, time and duration, but no output (a terminal session isn't stored) — and its latest run sets the node's status badge like any other block's and is written under the block (`terminal · exit 0 · 2.8s`), also in a fresh tab or after a restart. The same data is behind `GET /api/run/history?nodeId=..&block=..` (the list) and `runId` on `/api/run/subscribe` (replay one run).
- **Which blocks already ran** successfully this session and still look unchanged, so a later "run chain" doesn't redo a dependency it already ran.
- **Values of variables** a `form` fence submitted.

**`secret` variables are never written there** — neither a form's value for one, nor a `from=` block's output for one.

A run counts as *stale* — kept in the history, flagged as such, never shown as the block's latest result — when it no longer describes the document: the block's own code, interpreter or `env=`/`deps=` changed, **or any block in its `deps=`/`from=` chain did**, or the value of a non-secret variable any of them references changed, or the session was reset (the "reset session" action). Undoing an edit makes the runs made before it current again. A `secret`'s value is not part of this check (only the fact that a block references it is), since it can't be reproduced later. Only a new run of that block replaces a stale one on screen; a run still in progress at the moment of a reset isn't marked. The freshness records and form values themselves are wiped by a reset, not just flagged.

A run that was still in flight when the core died leaves no saved output, same as it leaves no process.

**Read-only canvases.** If the canvas file, or the directory its `.meshfox/` state would go in, can't be written (a read-only mount, someone else's checkout, a container image), meshfox serves it read-only instead of failing: blocks run, and nothing is written to disk. No lock file is taken (several workers can serve the same canvas side by side), the session database and variable answers live in memory only, a `cache`d block's output isn't written into the file, and edits — from the browser, TUI, CLI or MCP — are refused with a message saying why. The browser UI hides `Edit`; the TUI's `e` and `H` are unavailable. The local `.meshfox/config.toml` is still read, and a `python_venv` block's venv goes under the system temp dir. See SPEC.md's "Read-only canvases".

**Restricting a core on request.** `--read-only` (or `MESHFOX_READ_ONLY=1`) serves a canvas the same way even when it *is* writable, as a private core that never joins a running one — so any number of them can look at one canvas side by side. `--no-exec` (or `MESHFOX_NO_EXEC=1`) keeps viewing and editing but refuses to run anything: blocks, services, `tty` sessions, debug shells. Together they make a pure viewer. A read-only core doesn't start `service` blocks, and neither switch isolates what the blocks themselves do (a script that writes into the project still does). With a `server_socket` coordinator configured, `--read-only` never uses it (`view` then serves privately and says so), and a `--no-exec` command refuses any worker that can run code instead of joining it. See SPEC.md's "Restricting a core".

## Syntax grammars: built-in vs. loadable
<!-- meshfox:node id="architecture-syntax-grammars" -->

Two independent syntax-highlighting engines, one shared grammar format (mostly): the TUI uses [`syntect`](https://github.com/trishume/syntect) (Rust, via `syntect-tmlanguage` for real `.tmLanguage.json` support), the browser UI uses [Shiki](https://shiki.style/)/[Monaco](https://microsoft.github.io/monaco-editor/) (JS, via `vscode-textmate` — the same tokenizer VS Code itself runs on). Both understand plain TextMate grammars, so one `.tmLanguage.json` file usually works unmodified on either side.

- **Built-in, nothing to configure**: `syntect`'s own bundled defaults (TUI) and Shiki's own large bundled language set (web) already cover most common languages out of the box — this is what a canvas gets with zero setup.
- **meshfox's own grammar**, for `<!-- meshfox:... -->` marker comments, is compiled into *both* binaries rather than loaded from disk — `web/src/grammars/meshfox.tmLanguage.json` (a real TextMate *injection* grammar, auto-applied on top of Markdown) and `crates/cli/src/grammars/meshfox.tmLanguage.json` (an `include`-based grammar, `syntect` has no injection support at all — see below). Same rules (highlight the marker keyword, its attribute names/values), two different shapes because the two engines support different mechanisms for layering on top of an existing language.
- **meshfox's own bundled grammar *pool*** (`grammars/README.md`) — complete, standalone language grammars for anything neither engine bundles by default that meshfox itself needs, currently just Starlark (for ` ```starlark constraint ` fences, SPEC.md's "Constraint fences" — neither `syntect` nor Shiki ships one). One canonical, unmodified file the web side loads directly; where that file doesn't load into `syntect` as-is, a hand-adapted copy lives in `grammars/tui/` instead (same `<dir>/tui/<name>` shape as the user-facing override below) — the *built-in* version of that same mechanism, for meshfox's own pool rather than a user-supplied grammar.
- **User-loadable custom grammars**: drop a `.tmLanguage.json` or `.sublime-syntax` file into `.meshfox/syntax/` (next to the canvas) or `~/.meshfox/syntax/` (global, every project) — one shared repository both sides read from (`crate::syntax_registry` on the TUI side, `GET /api/syntax`/`GET /api/syntax/:name` for the browser — see "Terminal UI" and "Browser UI" in [INTERFACES.canvas.md](./INTERFACES.canvas.md) for each side's own detail). Local wins over global on a same-named clash.
- **TUI-only overrides**, `.meshfox/syntax/tui/` (and `~/.meshfox/syntax/tui/`): some real, unmodified upstream grammars simply can't load into `syntect` at all, override or not — a few (confirmed by grepping the actual upstream files, not guessed) use TextMate's `begin`/`while` construct, which `syntect`'s matching engine has no equivalent for (Markdown: 72 such rules; AsciiDoc: 244; YAML: 2 — plain programming-language grammars essentially never use it). A same-named file dropped in `tui/` overrides the plain one *for the TUI specifically*, hand-rewritten to use only constructs `syntect` supports (typically `include`-ing `syntect`'s own already-loaded default for that language, the way meshfox's own grammar does for Markdown) — the browser never sees this directory (`meshfox-server`'s own listing doesn't recurse into it), so it keeps using the original, unmodified file.
