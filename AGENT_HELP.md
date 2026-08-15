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

- Add a child node → `meshfox node add <parent-id> <title>`
- Delete a node (and its subtree) → `meshfox node rm <id>`; keep its
  children by reparenting them instead → `meshfox node rm <id>
  --keep-children`
- Move a node to a new parent → `meshfox node mv <id> <new-parent-id>`
- Rename a node's heading text → `meshfox node rename <id> <title>`
- Change a node's stable id (updates every reference: `parent=`, `meshfox:edge
  from=`, best-effort `deps=`) → `meshfox node set-id <id> <new-id>`
- Replace a node's body → `meshfox node body <id> --file <path>` (or pipe to
  stdin)
- Set position/size/style (`x`/`y`/`w`/`h`/`color`/`type`/`display`/`lang`)
  → `meshfox node meta <id> [flags...]`. `color` is either a literal hex
  string or one of six numbered presets — the same ones the web UI's swatch
  picker uses — so `--color 4` is shorthand for green:
  - `1` red, `2` orange, `3` yellow, `4` green, `5` blue, `6` purple
  - e.g. `meshfox node meta <id> --color 4` for green, or `--color ""` to
    clear it back to no color
- Replace a node's extra incoming edges → `meshfox node edges <id> --from
  <id>... ` (or `--clear`)
- Resync on-disk heading order to match on-canvas layout after moving things
  by position → `meshfox node reorder`
- Inspect a node before changing it (parent, children, extra parents, type,
  position/style) → `meshfox node show <id>`, instead of reading the raw
  file and guessing

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
directory.
