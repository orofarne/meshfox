# meshfox — usage guidance for AI coding agents

Also available at any time via `meshfox --agent-help`, wherever this binary
is installed — this file ships embedded in it, so it doesn't depend on any
per-project instructions. Not part of the `.canvas.md` format itself; for
that, see `meshfox spec`.

## Prefer `meshfox node <verb>` over hand-editing the file

A `.canvas.md` file is plain Markdown, so it's tempting to just open it and
edit the text directly. Don't, for structural changes — `mdcanvas` (the
engine behind every `node` subcommand) enforces invariants that are easy to
get subtly wrong by hand: single root, no duplicate ids, no dangling
`meshfox:edge` targets, heading-depth vs. `parent=` handling once a subtree
is already at H6 (Markdown's nesting ceiling), sibling order matching
on-disk position. Every `node` subcommand validates the *whole* resulting
document before writing anything back, so a bad edit fails loudly instead of
landing as a corrupt file.

Map your intent to a subcommand instead:

- Add a child node → `meshfox node add <parent-id> <title>` (the root's id is
  `root` in every canvas, unless that canvas's root declares another one —
  `meshfox node show` of any node prints its parent). Prints the new
  node's id (a slug of `title`) — pass `--body-file <path>` (or `--body-file
  -` for stdin) and/or any `node meta` flag below in that same call to give
  it real starting content/position/style too, instead of a separate
  `node body`/`node meta` follow-up that needs that id again.
- Delete a node (and its subtree) → `meshfox node rm <id>`; keep its
  children by reparenting them instead → `meshfox node rm <id>
  --keep-children`
- Move a node to a new parent → `meshfox node mv <id> <new-parent-id>`
- Rename a node's heading text → `meshfox node rename <id> <title>`
- Change a node's stable id (updates every reference: `parent=`, `meshfox:edge
  from=`, best-effort `deps=`) → `meshfox node set-id <id> <new-id>`
- Replace a node's body → `meshfox node body <id> --base-rev <rev> --file
  <path>` (or pipe to stdin). `--base-rev` is required: the `body-rev:` line
  `meshfox node show <id>` prints. If the body changed since you read it
  (another tab, an agent, an editor), nothing is written and the error gives
  the current body and revision — merge, then retry with that revision. Add
  `--merge` to have meshfox do that for you: it finds the body you read in
  the canvas's undo history and three-way merges your text with the current
  one by line. A clean merge is written and printed (read it — a line-level
  merge of prose can read wrong); if your edit overlaps the other one,
  nothing is written and the error lists the overlapping lines and the body
  with `<<<<<<< mine` / `=======` / `>>>>>>> theirs` markers — resolve that
  and write it back against the current revision. A body too old to still be
  in the history is the ordinary refusal. Just adding to it? `node append`
  needs no revision.
- Append to a node's existing body, without reading it back first → `meshfox
  node append <id> --file <path>` (or pipe to stdin)
- Set position/size/style/tags (`x`/`y`/`w`/`h`/`color`/`type`/`display`/
  `lang`/`interpreter`/`tags`) → `meshfox node meta <id> [flags...]` — same
  flags `node add` above also accepts. `color` is either a literal hex
  string or one of six numbered presets — the same ones the web UI's swatch
  picker uses — so `--color 4` is shorthand for green:
  - `1` red, `2` orange, `3` yellow, `4` green, `5` blue, `6` purple
  - e.g. `meshfox node meta <id> --color 4` for green, or `--color ""` to
    clear it back to no color
  - tags are only ever added or removed, never replaced as a whole list:
    `--add-tag "bag,fixed"` / `--remove-tag "old"` (comma-separated, same
    spelling as the file's own `tags=`). A tag already there keeps its
    place. (`node add` takes `--tags "bag,fixed"` for a new node's initial
    tags; `node meta --tags` is gone.)
- Add or remove a node's extra incoming edges → `meshfox node edges <id>
  --add <id>... --remove <id>...` (never replaces the whole set; adding an
  edge that's already there leaves its label and route alone)
- Resync on-disk heading order to match on-canvas layout after moving things
  by position → `meshfox node reorder`
- Inspect a node before changing it (parent, children, extra parents, type,
  position/style, tags/color when set) → `meshfox node show <id>`, instead
  of reading the raw file and guessing

Hand-editing is fine for prose *inside* an existing node's body that doesn't
touch structure or bookkeeping comments. Either way, validate afterward:

    meshfox validate <file>

It runs the same parse checks every other command does, without executing or
writing anything, and exits non-zero on failure — safe to chain in a script
or run right after any edit, by hand or via `node`.

If the document has any embedded ` ```starlark constraint ` fences (a
Starlark contract over the tree, living in any node's own body, e.g.
"every node tagged `table` has exactly one `file` child" — see `meshfox
spec`'s "Constraint fences"), also run:

    meshfox check <file>

This is a *different* check than `validate`: `validate` only asks whether
the file parses; `check` asks whether the document actually satisfies its
own declared rules. A structural edit that leaves the file well-formed can
still break a constraint (e.g. deleting a table's only `file` child) —
`check` is what catches that, `validate` won't.

## Running non-interactively

`meshfox configure` and any unresolved `meshfox:var` prompt during `run`
require an interactive terminal — don't use them. Supply variable values
directly instead:

    meshfox run <path...> <block> --set NAME=VALUE --set OTHER=VALUE

`--no-deps` skips a block's `deps=` chain and runs only what's named on the
command line, if that's what's intended instead of the full chain.

## Other discovery commands

- `meshfox list` — every runnable code block as a ready-to-paste `meshfox
  run` invocation, instead of reading the file to find block names.
- `meshfox spec` — the full `.canvas.md` format reference, if a hand-edit is
  actually warranted.
- `meshfox node find` — the general lookup tool: a CSS selector
  (`.tag`, `[type="file"]`, `#parent > .child`) for structure, `--text
  <substring>` for a case-insensitive full-text search over title/body
  (each match comes back with a short excerpt, so a separate `node show`
  usually isn't needed just to see why it matched), and
  `--created-after`/`--created-before`/`--updated-after`/`--updated-before`/
  `--since` (RFC3339 or a relative duration like `7d`) for filtering by
  when a node was made or last changed. All of these AND together; any can
  be omitted, and the selector itself defaults to `*` (every node) when
  dropped — e.g. `meshfox node find --since 7d` for "what changed
  recently," instead of rereading the whole tree every session.

## MCP, if available

`meshfox mcp` is a stdio MCP server — experimental, and not something this
binary itself starts (a host launches it: `{"command": "meshfox", "args":
["mcp"]}`). It takes no arguments; whichever directory it's started in
becomes its root. It's multi-canvas: `canvas_open`/`canvas_close`/
`canvas_list` manage a registry of open canvases (each its own isolated
spawned process — a crash or hung debug session on one canvas can't touch
another), and **every other tool requires a `canvas_id` as its first
argument** — there's no implicit "current" canvas. A `canvas_id` is the
file's path relative to the root, so a call with the id of a canvas that
isn't open (never opened, closed after sitting idle, or whose process died)
just reopens it; `canvas_open` is only needed to create a file or to get the
id. A canvas whose process is hung (alive, not answering) is stopped after
three unanswered pings or a call with no answer in 90 s and reopened by the
next call; `canvas_list` reports each canvas's `busy` and `health` and never
waits on one. `canvas_open` only resolves files under that root directory;
nothing above it is reachable. If this session already has that configured, its tools are
usually a better fit than shelling out through this same CLI for two
specific cases:

- **Running a block** (`run`, `session_reset`): `run` executes a block with
  its `deps=` chain and returns every step's exit code, duration and output
  tail as JSON. A dependency that already ran this session and looks
  unchanged is skipped — for a build/test step that depends on files rather
  than on the block's own text, pass `fresh: true` (the whole chain runs for
  real this once; nothing is forgotten), or call `session_reset` to forget
  everything. The CLI equivalents are `meshfox run --fresh` and `meshfox
  session reset`. Blocks marked `confirm` require explicit user approval for
  each invocation, including when reached as dependencies. If `run` reports
  confirmation required, ask the user before retrying with `confirm: true`;
  never infer approval from a prior run. CLI uses `--confirm` for automation.
- **A multi-step debug session** (`debug_start`/`debug_send`/`debug_stop`):
  a persistent shell in a node/block's own resolved cwd/env, so state
  (exported vars, files a snippet wrote) survives between calls — a
  one-shot `meshfox run` re-resolves everything from scratch every time.
- **Structured reads/single edits** — the whole `node <op>` surface, not
  just a subset: `node_show`/`node_find`/`node_add`/`node_body`/
  `node_append`/`node_meta`/`node_block`/`node_rm`/`node_mv`/`node_rename`/
  `node_set_id`/`node_edges`/`node_move`/`node_reorder` — `node_show`
  returns JSON, not text to re-parse, including each node's `body_rev`:
  `node_body` requires it back as `base_rev` and refuses a body that changed
  since (the error carries the current body and revision to retry with);
  `merge: true` merges instead — see `meshfox node body --merge` above — and
  returns `merged: true` with the stored `body` and its `body_rev`.
- **`validate`/`check`** — same checks `meshfox validate`/`meshfox check`
  run, as structured JSON instead of text to re-parse: `validate` returns a
  tool error naming the specific problem on failure (same convention
  `node_add` etc. use for a bad write), not just a non-zero exit code;
  `check` returns `{ok, results}` with one entry per constraint fence
  (`ok`/`messages`) — a single constraint failing isn't itself a tool
  error, so a caller can inspect what's wrong without try/catch.

Everything else (list/static/pdf, batch edits, anything `--no-deps`-shaped)
still only exists via the CLI below — the MCP tools are a narrower,
additive surface, not a replacement for it.

## Canvas path

Most commands take the canvas path as an optional trailing argument or
`--canvas` flag (`run` and `node <op>` only take `--canvas`, not a bare
trailing path — their own positionals are node-id/block-name args instead).
The path can also be given once, before the subcommand instead of after it
— `meshfox path/to.canvas.md run tests smoke` — which a subcommand's own
path/`--canvas` overrides if both are given. Omitting it entirely
auto-discovers the single `.canvas.md` candidate in the current directory — fine for a one-off in a
directory known to have exactly one, but pass it explicitly whenever that's
not certain, so behavior doesn't depend on what else happens to be in the
directory. `run foo.md tests smoke` (path *after* the subcommand instead of
before) is a common slip — `foo.md` there just gets swallowed as if it were
a node-id path segment instead, since `run`'s own positionals have no such
convention of their own; both this and a "did you mean" typo/missing-
ancestor suggestion are surfaced back in the error when it happens, so it's
recoverable, but getting the ordering right the first time avoids the
detour.

### Terminal status for CLI runs

`meshfox run --terminal-status auto|off|iterm2|progress|title` controls the
terminal's run indicator. `MESHFOX_TERMINAL_STATUS` supplies the default;
the flag takes precedence. This is a CLI presentation option, not a canvas
or worker setting.

Auto selects Session Status (OSC 21337) for iTerm2 and OSC 9;4 progress for
Ghostty, Contour and Windows Terminal, using `TERM_PROGRAM` and `WT_SESSION`.
Unknown terminals and tmux/screen are disabled in auto mode. Explicit modes
can override detection, but stderr must still be a terminal. Older terminal
versions may ignore the sequences. No multiplexer passthrough is configured.

Runs show the current step; meshfox's own input prompts show waiting.
Interactive child programs remain running: their input waits are not inferred.
Success/failure remain visible in iTerm2; progress is withdrawn on success
and marked as failed on error. Handled Ctrl-C clears the indicator.
Progress is refreshed once per second for terminals that expire stale state.
The optional title mode changes the terminal title; the shell may replace it
at its next prompt. Status sequences are written to stderr, never stdout.

`meshfox tui --terminal-status auto|off|iterm2|progress|title` uses the same
option and environment default. It aggregates the current canvas's local
and external runs, including TTY sessions. Input forms and run confirmations
show waiting; normal navigation does not. Parallel runs retain a failure
until a new batch starts. Finished runs already present when opening a canvas
do not count as a new result. Nested canvases retain independent results.
Healthy background services show `ready: N services` in iTerm2/title mode;
starting/stopping/restarting services count as activity, and a crashed service
or failed stop shows failure. Progress-only terminals withdraw the indicator
when services are ready. TUI exit and handled shutdown signals clear status.

### Observe an existing core

`meshfox console [CANVAS]` (or `meshfox CANVAS console`, or `--canvas CANVAS`)
attaches to an already running core. It never starts a core or executes blocks.
Without a path it discovers a single `.canvas.md` file by name, without reading
canvas contents. For a marked plain `.md` canvas, pass its path. A missing core is an
error; a configured but unreachable coordinator is not bypassed.

The console replays buffered output of running plain blocks, follows new runs,
and reports service state and bounded service-log snapshots. Historical finished
runs at initial attachment are omitted. TTY sessions are outside its scope.
Output carries `node::block` prefixes; stderr output goes to stderr. Service
snapshots are reconciled by their overlapping lines (identical repeated lines
in a completely rolled-over buffer cannot be distinguished).

`--terminal-status auto|off|iterm2|progress|title` and
`MESHFOX_TERMINAL_STATUS` work as for run/TUI. The indicator aggregates active
blocks, failures and ready services, shows waiting while disconnected, and
clears on exit. Ctrl-C detaches without cancelling runs or stopping services.
The console retries discovery after connection loss, including port changes.
Rust passive run subscriptions resume with sequence cursors and a persisted
run identity, so reconnecting cannot splice another invocation's output into
the original stream. Workers without persisted run identities cannot resume
that stream safely.

### Native calls between canvases

A `meshfox` fence is YAML with required `canvas` and `run`, optional `set`
(string values), `fresh`, `no-deps` and `skip-fresh`. It uses a native server
executor, not a shell wrapper or a nested CLI. Child document variables and
confirmation gates are prepared before parent side effects. Child events carry
optional `child: {canvas, port, event}` metadata; CLI/TUI terminal status reads
this metadata to show the child step. Cancellation names the exact child run.

See SPEC.md's "Calling another canvas" and `examples/canvas-call.canvas.md`
for the runnable parent/child example with an embedded SVG architecture diagram.
Required child application arguments must be bound in `run`. TTY relay is not
supported; service-starting calls need an independently running child worker.
The parent's YAML cannot auto-approve child confirmation gates.
