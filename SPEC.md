# meshfox canvas format

Reference spec for `.canvas.md`. Also available at any time via `meshfox spec`.

A canvas is a Markdown outline: heading nesting *is* the node tree; bookkeeping
(id, position, extra edges) lives in HTML comments, invisible to any normal
Markdown viewer. Model comes from JSON Canvas (jsoncanvas.org); this format
moves the same tree/graph-of-Markdown-nodes model into plain Markdown instead
of JSON, for readable diffs and hand-editability.

## File structure

- **`#` (H1)** — exactly one per document. Content up to the first `##` is
  the **root** node. Always a node, no marker needed (it's the only H1).
- **`##`, `###`, ...** — a heading is a node *only if* immediately followed
  by a `<!-- meshfox:node ... -->` comment. Without one, the heading (and
  everything under it) is just prose inside its enclosing node — headings
  can be freely used for sub-structure without fragmenting the canvas.
  A node's parent is the nearest enclosing shallower node heading (or root)
  — unless overridden by `parent=` (below).
- **`<!-- meshfox:node ... -->`** — right after a heading line. Turns the
  heading into a node and holds its bookkeeping as `key="value"` attributes:
  `id`, `type`, `x`, `y`, `w`, `h`, `color`, `tags`, `parent`, `fold`,
  `edgeLabel`. All optional; a
  bare `<!-- meshfox:node -->` is enough. `id` defaults to a slug of the
  heading text; only write it explicitly for a stable handle that survives
  renames (e.g. because an edge references it). First write-back (running a
  cached block, saving layout) pins the id used, so identity is stable
  afterward. `x`/`y` are absolute document coordinates for every node
  *except* a direct child of a `group` node (see "Node types" below) — for
  a group member, `x`/`y` are relative to that group's own `x`/`y` instead
  (its own top-left corner), so moving the group moves every member with
  it without rewriting each one's stored position. A group's own `x`/`y`
  is an optional anchor, draggable like any other node's — but its `w`/`h`
  stay always derived from its members' own resolved boxes, never stored,
  even once it has an anchor. `fold="true"` or `fold="false"` overrides,
  for this one node only, whether the web UI shows it folded or expanded
  by default — see "Options" below for the document-wide default this
  overrides. Omitted (the default) means "no override": follow the
  document's own default. `tags="a,b,c"` is a comma-separated list of free-form labels —
  purely descriptive (no structural meaning), shown as small chips on the
  node in the web UI. `parent="other-id"` overrides the heading-nesting-implied
  parent — needed once a subtree is already `######` (H6, CommonMark's
  ceiling: headings can't nest any deeper), where a further child has nowhere
  left to go but *another* `######` heading, which plain heading nesting
  alone would read as a sibling rather than a child. Nesting past H6 keeps
  working — every deeper node just keeps writing `######` and disambiguates
  with `parent=` instead of heading depth. Written automatically ("add
  child" in the UI reaches for it exactly when it's needed); hand-editing it
  is only for restructuring an already-flattened deep subtree. `edgeLabel=`
  is arrow text for the *structural* edge from this node's own parent into
  it — the implicit nesting edge has no line of its own to carry attributes
  the way a `meshfox:edge` does (below), so it lives here instead, on the
  child end: "the label of the edge that points at me". Unlike a
  `meshfox:edge`, a structural edge has no color/style/arrowhead attributes
  of its own — just this one piece of text. Omitted means no label, same as
  every other optional attribute here.
- **`<!-- meshfox:edge from="other-id" ... -->`** — one per line, right
  after a node's `meshfox:node` line. Declares an extra incoming edge from
  another node, for graphs that aren't a clean nesting tree. Any number
  allowed, in addition to the one implicit nesting-parent edge. Besides
  `from`, an edge line accepts the same kind of optional styling attributes
  the web UI's on-canvas edge editor writes: `label` (arrow text), `color`
  (hex or a `"1"`–`"6"` preset, same palette as a node's own `color`),
  `style` (`"solid"`, `"dashed"` — the default look when omitted — or
  `"dotted"`), `arrowStart`/`arrowEnd` (`"none"` or `"arrow"` — an edge with
  neither set gets an arrowhead only at `arrowEnd`, matching the pre-styling
  default), and `tags` (comma-separated, same convention as a node's own).
  All are omitted from the line unless explicitly set — a plain
  `from="other-id"` with nothing else is exactly the old, pre-styling form.
  These extra (`meshfox:edge`) edges render as a curved connector in the web
  UI, distinct from the structural nesting tree's right-angle routing.
- **`<!-- meshfox:canvas -->`** — optional, first line of the file. Marks a
  plain `*.md` file as a canvas even though it isn't named `*.canvas.md`
  (used so this doubles as auto-discovery hint; not required for parsing —
  any correctly-structured file parses as a canvas regardless of name).

## Node types

`type=` on `meshfox:node` picks one of JSON Canvas's four kinds. Defaults to
(and is omitted for) `text`.

- **`text`** (default) — freeform Markdown body.
- **`group`** — purely organizational; body must be empty. Children are
  whatever nests under it structurally (no separate containment mechanism);
  a direct child's own `x`/`y` is relative to the group's, not absolute —
  see `x`/`y` above.
- **`file`** / **`link`** — body must be *exactly* one Markdown link and
  nothing else: `[label](target)`. A `file` node also accepts two optional
  display attributes on its `meshfox:node` line:
  - `display="link"` (default) or `display="code"` — `code` shows the
    target's own file content as a read-only, non-runnable syntax-highlighted
    preview instead of a plain clickable link. The file is read fresh from
    disk on every view, confined to the canvas's own directory tree (same
    boundary `include` targets are resolved within) — never written back.
  - `lang="..."` — syntax-highlighting language hint for `display="code"`
    (e.g. `lang="rust"`). Optional; when omitted, the language is guessed
    from the target's file extension. Ignored when `display` isn't `code`.
  - `interpreter="..."` — an executable (e.g. `interpreter="python"`) to run
    against `target`, making the node runnable: `interpreter target` (the
    target's path resolved relative to the canvas's own directory,
    confined to it — same boundary `display="code"`/`include` targets are
    resolved within). Optional; omitted means the node isn't runnable this
    way. The web UI's "▷ run" button (next to "expand", in a runnable
    node's title bar) invokes this the same way it does a `text` node's
    default code block.

    ```
    <!-- meshfox:node type="file" display="code" lang="rust" -->

    [main](./src/main.rs)
    ```

    ```
    <!-- meshfox:node type="file" interpreter="python" -->

    [seed data](./scripts/seed.py)
    ```

  `link` nodes don't support `display`/`code`/`interpreter` — their target
  is an external URL, not something meshfox reads from or runs off disk.
- **`include`** — same one-link body as `file`/`link`, but the target
  (another `.md` or `.canvas.md` file) is spliced in *dynamically* by
  whatever consumer resolves includes — never written back to disk.
  `run`/`validate` see the bare link, same as `file`/`link`. See
  "Includes" below.

Any other `type=` value, a non-empty `group` body, or a `file`/`link`/
`include` body that isn't a single link is a parse error (`meshfox
validate` catches these, plus a missing/cyclic/unparseable include target).

There's no node type for a constraint contract — a Starlark check is an
embedded fence living in any node's ordinary body, alongside its prose and
runnable code, same as a `` ```bash name="..." `` fence. See "Constraint
fences" below.

## Includes

`type="include"` dynamically splices another file's content into this
node, resolved fresh every time a consumer asks for it (e.g. the server,
before serving `GET /api/canvas` to `meshfox view`) — nothing is ever
written into the including file. `run`/`validate` operate on a single
file's raw text and never resolve includes; only `validate` reaches far
enough in to catch a broken target, a parse error in it, or a cycle.

The target is told apart as a **canvas** or **plain Markdown** the same way
auto-discovery already does: `.canvas.md` suffix, or a plain `.md` file
that opens with the `meshfox:canvas` marker.

- **canvas target** — parsed and spliced in as real children. Every
  spliced node's `id` is namespaced `{include_id}/{original_id}` to avoid
  collisions with the including document (and with any other include
  spliced in alongside it), and every level is shifted down by the include
  node's own level. The include node itself becomes a `group`.
- **plain Markdown target** — has no meshfox structure of its own, so it
  becomes the include node's own body verbatim, except every heading in it
  is shifted down (clamped to H6, CommonMark's ceiling) by the include
  node's own level — so e.g. the target's top-level `#` doesn't read as a
  second document root once nested. The include node becomes `text`.

An included file can itself declare includes; those are resolved too,
with a cycle (A includes B includes A) reported as an error rather than
recursing forever.

## Runnable code fences

Lives inside a node's Markdown text, as fence-info-string attributes:

    ```bash name="build" cache
    cargo build --workspace
    ```

- `name` — identifies a fence for `meshfox run`/output caching; must be
  unique within its node. Normally required to make a fence runnable —
  except a node may have *one* fence with no `name` at all, which is
  runnable too, implicitly named after its own node `id`. A second
  unnamed fence in the same node makes the omission ambiguous, so neither
  gets a name (same as any unnamed fence today — not an error, just not
  runnable). This is what lets `meshfox run`/`list` skip a redundant
  trailing block-name argument that would just repeat the node's own id —
  see "CLI" below — and applies the same way to a fence whose *explicit*
  `name` happens to already match its node's `id`.
- `cache` — optional flag (`cache` or `cache=true`); opts into persisting
  output back into the file.
- `default` — optional flag (`default` or `default=true`); marks this
  fence as its node's **default** block — the one `meshfox run
  <path-to-node>` addresses without a trailing block name (see "CLI"
  below). A fence whose (implicit or explicit) `name` already equals its
  own node's `id` counts as default too, without needing this flag. At
  most one block per node may be default (explicitly, implicitly, or one
  of each) — `meshfox validate` reports a conflict as an error.
- `deps` — optional, comma-separated list of other blocks this one runs
  after. Each entry is either a bare block name (a block in the *same*
  node) or `node-id/block-name` (a block in another node, addressed by
  node `id` the same way `meshfox:edge from=` addresses nodes). Running a
  block always runs its full dependency chain first, automatically —
  each dependency's own `deps` are resolved transitively, in order, with
  no block run twice even if several blocks in the chain depend on it.
  A cycle, or a `deps` entry naming a block that doesn't exist, is a
  `meshfox validate` error.
- `env` — optional, comma-separated list of declared `meshfox:var`s (see
  "Variables" below) this block wants in its own process environment.
  Each entry is a bare name (pass the declared variable through under the
  same name) or `local=name` (expose it under a different name in this
  block's own environment) — a leading `$` on the variable-name side is
  accepted and stripped, but purely cosmetic: `env="$X,LOCAL=$Y"` and
  `env="X,LOCAL=Y"` mean exactly the same thing. A block with no `env=`
  never resolves or prompts for *any* declared variable, however many the
  document declares as a whole — this is what scopes "does running this
  block need to ask about anything" to the block itself, not the whole
  canvas. An `env=` entry naming a variable nothing declares is a
  `meshfox validate` error.
- `tty` — optional flag (`tty` or `tty=true`); this block wants a real
  interactive terminal instead of the usual captured/streamed output —
  e.g. `bash` on its own (a login-style shell), or anything else that reads
  from its own stdin expecting a real terminal (`ssh`, `git commit`'s
  editor, a REPL, a curses UI). Mutually exclusive with `cache` (`meshfox
  validate` error) — an interactive session isn't the deterministic
  "exit code plus text" `cache` saves/replays. A `tty` block may only be a
  `deps=` target of *another* `tty` block (`meshfox validate` error
  otherwise) — a non-interactive chain auto-running one as a dependency
  would mean an unrequested interactive step ambushing it; two chained
  `tty` blocks just hand the terminal over twice, back to back, in
  dependency order (each still running its own `deps=` first, same as any
  other block). See "Interactive (`tty`) blocks" below for how the CLI and
  web UI actually run one.

Supported languages: `bash` (`sh` is an alias for it). A fence in any other
language never counts as runnable at all — not with an explicit `name=`,
and not as a node's sole unnamed fence — so an ordinary Markdown
document's own example fences (a `yaml` config sample, a `json` snippet,
...) never get mistaken for something to run just because meshfox was
pointed at the file directly (the `meshfox:canvas` marker/`.canvas.md`
suffix is only required for *auto-discovery* — see "CLI" below — an
explicitly-given path still parses whatever heading structure it finds).

    ```bash name="build" cache
    cargo build --workspace
    ```

    ```bash name="test" cache deps="build"
    cargo test --workspace
    ```

Running `test` here always runs `build` first.

## Constraint fences

A ` ```starlark constraint ` fence, living in any node's ordinary
Markdown body alongside its prose and runnable code, is a sandboxed
[Starlark](https://github.com/bazelbuild/starlark) contract over the
document tree — a way to assert invariants a canvas should hold (e.g.
"every node tagged `table` has exactly one `file` child") as part of the
document itself, checked by `meshfox check` rather than enforced only by
convention. There's no dedicated node type for this (see "Node types"
above) and no restriction on what else shares the node's body — a node may
carry prose, runnable fences, and any number of constraint fences, in any
order:

    ## Entities
    <!-- meshfox:node -->

    ```starlark constraint
    for n in self.descendants():
        if "table" in n.tags:
            files = [c for c in n.children() if c.type == "file"]
            if len(files) != 1:
                fail(n.id + ": expected exactly one file child, got " + str(len(files)))
    ```

    ### Users
    <!-- meshfox:node tags="table" -->
    ...

The `constraint` flag (`` ```starlark constraint ``, mirroring the bare
`cache`/`default`/`tty` flags on a runnable fence) is what opts a fence in
— a plain ` ```starlark ` fence with no flag is left alone, e.g. a
documentation example showing Starlark syntax that was never meant to
actually run. An optional `name="..."` attribute labels a fence for
`meshfox check`'s output when a node carries more than one (see below);
unnamed fences are identified by their enclosing node's id alone if it's
the node's only one, or `<node-id>#<n>` (1-based, in document order)
when it isn't.

There's no separate "document" object: `doc` is simply the root node, and
every node — `doc` included — exposes the same navigation methods, so a
constraint scopes a check to its own subtree with `self.descendants()` the
exact same way it would reach the whole document via `doc.descendants()`.
`self` is the node whose body the fence lives in — a constraint typically
governs the subtree of the node it's placed in, so a natural place for one
is directly in the node that's the natural parent of whatever it's
checking (like `Entities` above), rather than needing a dedicated node of
its own just to sit above that subtree.

The script sees:

- **`doc`** — the document's root node.
- **`self`** — the node whose body this fence lives in, so a script can
  find its place in the tree without hardcoding its own id.
- On every node (both of the above, and any node reached through them):
  - **`.id`**, **`.title`**, **`.type`** (a string, e.g. `"file"`),
    **`.parent`** (a string, or `None` for the root), **`.tags`** (a list
    of strings) — plain read-only fields.
  - **`.children()`** — its direct structural children (same tree
    `Canvas::children` walks — not extra `meshfox:edge` parents).
  - **`.descendants()`** — everything in its subtree (children, their
    children, ...), not just direct children — the usual way to scope a
    check to "this fence's own node's subtree" (`self.descendants()`)
    instead of the whole document (`doc.descendants()`).
  - **`.node(id)`** — the node with that id anywhere in the document, or
    `None`.
  - **`.nodes_with_tag(tag)`** — every node in the *whole document* whose
    `tags` includes `tag`, regardless of where it's called from. Prefer
    `self.descendants()` filtered by tag when a constraint should only
    govern its own subtree; reach for this only when a rule is genuinely
    document-wide.
- **`fail(msg)`** — records a violation *without* stopping the script, so
  one constraint can report every offending node in a single run instead of
  just the first. A script that never calls `fail` passes.

Beyond these, and Starlark's own built-ins (`len`, `range`, string
methods, list/dict comprehensions, ...), the sandbox has nothing: no file
I/O, no network, no way to see any other node's fully-resolved include
tree, and no way to mutate the document — a constraint only ever reads and
reports. Evaluation is resource-bounded (instruction count, call depth,
heap size); a script that times out or errors (syntax error, unbound
name, ...) counts as a failing constraint, with that error as its one
message. Each constraint fence gets its own fresh sandbox — nothing
persists between them, and nothing carries over between one `meshfox
check` run and the next.

`meshfox check` runs every constraint fence in the document and reports
pass/fail per fence, exiting non-zero if any fails (or if the file doesn't
parse — `validate`'s job, which `check` implies). Distinct from `meshfox
validate`: `validate` asks whether the file *parses* as a well-formed
canvas; `check` asks whether the document, once parsed, actually satisfies
whatever rules its own constraint fences declare.

## Interactive (`tty`) blocks

A `tty` block (see its flag above) hands its process the real terminal
instead of the captured/streamed output every other block gets — CLI and
web UI each do this differently, since only one of them actually has a
terminal to hand over.

- **CLI** — before running a `tty` step, `meshfox run` checks that both
  stdin *and* stdout are an interactive terminal; if either isn't (piped,
  redirected, CI), it errors out rather than hanging or silently running
  non-interactively. When they are, the block's process is connected
  directly to the real terminal (not captured line-by-line the way every
  other block's output is) — a script can `read` from the user, run
  `vim`, prompt for a password, whatever a normal interactive shell
  command could do. Earlier steps in the same `deps=` chain still run
  captured/streamed exactly as usual; only the `tty` step(s) themselves
  take over the terminal, handing it back once each one exits.
- **Web UI** — `meshfox view` gives the block a real pseudo-terminal
  (not just piped stdout/stderr, which can't do cursor movement, raw
  input mode, or terminal-size queries) and streams it to an in-browser
  terminal, keys typed there going back to the process's stdin. Clicking
  run on a `tty` block opens this as a floating panel over the canvas
  (draggable/collapsible, like the node text editor), rather than filling
  in the node's own inline output area the way a normal run does.
- Either way, output from a `tty` block is never written back into the
  file — that's what the `cache` conflict (above) rules out.
- A real terminal (CLI's own, or the web UI's pseudo-terminal) is a
  genuine tty, so the "commands run without a pseudo-terminal" note under
  "Cached output" below doesn't apply to `tty` blocks — a tool that
  auto-detects color support (`cargo`, `git`, ...) sees a real terminal
  and colors its output without needing `--color=always`.

## Variables

Document-scoped configuration values a canvas wants from whoever runs
it — an install prefix, a log level, an API token — asked for once and
then remembered, the same idea as CMake's cached variables or a
`./configure` step. Declared as `<!-- meshfox:var ... -->` comments,
**only inside the root node's own body** — a `meshfox:var` found in any
other node is a `meshfox validate` error, not silently ignored, since a
variable is always document-wide, never per-node:

    <!-- meshfox:var name="INSTALL_PATH" prompt="Install prefix?" default="/usr/local/bin" -->
    <!-- meshfox:var name="LOG_LEVEL" type="select" choices="debug,info,warn,error" default="info" -->
    <!-- meshfox:var name="API_TOKEN" secret -->
    <!-- meshfox:var name="REGION" default="us-east-1" required -->

Attributes:

- `name` — required. What a runnable fence's own `env=` (see "Runnable
  code fences" above) refers to when it wants this variable — declaring
  one here doesn't, by itself, put it in any block's environment; that's
  opt-in per block, see "Consumption" below.
- `type` — `string` (default), `int`, `bool`, or `select`. Purely a hint
  for how to *prompt* (a `bool` prompts y/n, a `select` shows its
  `choices` as a menu, ...) and how a UI renders an input for it — an
  incoming value (from `--set`, the environment, the cache, or a typed
  answer) is never validated against it; it's just a string either way.
- `prompt` — question text to show when asking for a value; defaults to
  `name` itself.
- `default` — used if nothing else resolves the variable (see below).
- `choices` — comma-separated; required when `type="select"`.
- `secret` — flag (`secret` or `secret=true`). A secret variable is
  never read from or written to the on-disk cache (see below) and never
  pre-filled anywhere — the only way to supply one without an
  interactive prompt is `--set`/the process environment. It's asked for
  fresh every single time it's needed.
- `required` — flag (`required` or `required=true`). A `required`
  variable's own `default` is never taken silently, even when nothing
  else resolves it — it still needs one explicit interactive answer, the
  same as if it had no `default` at all, with `default` offered as the
  prompt's own pre-filled suggestion (so confirming it is just pressing
  Enter). This is only a one-time confirmation, not a standing "ask every
  run": like any other non-secret answer, whatever's confirmed is written
  to the cache, so the next run of any block referencing it resolves
  straight from there without prompting again. Without `required`, a
  variable with a `default` is simply used, never prompted for, unless it
  has no `default` at all (in which case it always needs an answer either
  way).

### Consumption (`env=`)

A `meshfox:var` declaration on its own does nothing to any block — it's
only a name a fence's own `env=` attribute can *reference* (see "Runnable
code fences" above). Only variables a block actually lists in its own
`env=` are ever resolved or prompted for on its behalf, and only those
end up in its process environment (under whatever local name it asked
for): running a block that declares no `env=` at all never touches
`meshfox:var` resolution, however many variables the document as a whole
declares. This is what keeps, say, `meshfox README.md run some-unrelated-block`
from ever being asked about an `INSTALL_PATH` that only some *other*
block in the document actually uses.

`meshfox run <path...> <a,b,c>` resolves each requested block's (and its
`deps=` chain's) own `env=` independently, right before that specific
block runs — not the whole document's variables up front. If the same
variable is referenced by more than one block in a single invocation
(directly or via `deps=`), it's only ever resolved/prompted for once: the
first block to need it answers the prompt, which is immediately cached,
so every later block referencing the same variable in that same
invocation just reads the cached answer.

### Resolution

Resolving one declared variable (for whichever block's `env=` asked for
it) tries, in order: an explicit override (`meshfox run --set
NAME=value`, or a value submitted through the web UI's form) → the
process environment → the on-disk cache (skipped entirely for `secret`)
→ the declaration's own `default` (skipped entirely for `required` — see
above). Whatever isn't resolved by any of those needs an interactive
answer — a terminal prompt for the CLI, a form for the web UI — which,
for a non-secret variable, is then written to the cache so a later run of
*any* block referencing the same variable doesn't ask again (this is what
turns a `required` variable's mandatory first confirmation into an
ordinary cache hit on every run after).

The cache lives at `<dir>/.meshfox/<filename>.env`, next to the canvas
file itself (e.g. `examples/hello.canvas.md` ->
`examples/.meshfox/hello.canvas.md.env`) — a plain `NAME=value`-per-line
file, meant to be `.gitignore`d, the same way `CMakeCache.txt` usually
is. It's safe to hand-edit or delete: deleting it just means every
non-secret variable gets asked about again next time some block's `env=`
needs it.

### CLI

- `meshfox configure [canvas]` — the one place that *does* walk every
  declared non-secret variable in the whole document, regardless of
  which (if any) block currently references it — showing its
  currently-resolved value (cache / env / default) as the prompt's own
  default, and writing whatever you answer (even if unchanged) back to
  the cache. The explicit "set these up now" step, the same role
  `ccmake`/`cmake -L` plays for a CMake cache; nothing else in meshfox
  requires running it. Requires a terminal; refuses to run (rather than
  silently do nothing or apply bare defaults) if stdin isn't one. Secret
  variables are never shown here — asking for one that's never cached
  and immediately discarded again wouldn't do anything useful.
- `meshfox run` resolves each executed block's own `env=` *lazily*: only
  a variable that block actually references, and that's still
  unresolved after the above precedence, gets an interactive prompt,
  right before that block runs — no separate configure step required,
  and no prompt at all for a block whose `env=` is empty or already
  fully resolved. `--set NAME=value` (repeatable) supplies overrides on
  the command line, the non-interactive equivalent of answering a
  prompt — the same flag CI would use in place of a TTY, which `run`
  otherwise requires whenever some referenced variable is still missing
  after `--set`/env/cache/default. A `--set` value is saved to the cache
  regardless of whether anything in the current invocation actually
  references it, same as `cmake -D` always updating `CMakeCache.txt`.

## Options

Document-wide settings that flip a default behavior for the whole canvas —
distinct from `meshfox:var` (a value asked for from whoever runs the
document) in that an option has no prompt and no value: it's either
declared or it isn't. Declared as `<!-- meshfox:option name="..." -->`
comments, **only inside the root node's own body** — same restriction as
`meshfox:var`, and for the same reason: an option is always document-wide,
never per-node. A `meshfox:option` found in any other node, one missing
`name`, or the same `name` declared twice, is a `meshfox validate` error.

    <!-- meshfox:option name="unfold" -->

Currently defined options:

- `unfold` — the web UI's default is every node folded to a compact
  title-only chip except the root, so a large canvas opens navigable
  rather than as a wall of expanded nodes; declaring `unfold` flips that
  default to everything expanded. A single node can still override
  whichever default applies to it with its own `fold=` attribute (see
  "File structure" above) — the option only sets what an *unset* node
  falls back to.

An unrecognized `name` is not an error — options are meant to grow over
time, and an older meshfox binary should still open a canvas written for a
newer one, just without acting on whichever option it doesn't know about.

Hand-editing the comment directly always works, but the web UI's toolbar
also has an "options" button that toggles a known option (currently just
`unfold`) without touching the file by hand — it writes the same comment.
An unrecognized declaration already in the file is left exactly as-is
either way, whichever recognized ones are also toggled alongside it.

## Cached output

Running a `cache`d block writes/updates a fenced block immediately after the
source, wrapped in markers so re-runs replace just that region:

    ```bash name="build" cache
    cargo build --workspace
    ```
    <!-- meshfox:output name="build" -->
    ```text
    exit code: 0
    ...
    ```
    <!-- /meshfox:output -->

Output (live or cached) that contains ANSI SGR color/style escape codes
renders in color in the web UI, both while a block is still streaming and
for a previously-cached block's saved output. Commands run without a
pseudo-terminal, so a tool that auto-detects "not a real terminal" and
disables its own color output (`cargo`, `git`, plain `ls`) prints plain
text here unless it's told to force color (`--color=always`, or a script
emitting raw escape codes itself).

## Minimal example

````markdown
<!-- meshfox:canvas -->
# Hello Project
<!-- meshfox:node id="root" -->

Project root.

## Tests
<!-- meshfox:node id="tests" type="group" -->

### Smoke Test
<!-- meshfox:node id="smoke-test" -->

```bash name="smoke" cache
echo "hello from meshfox"
```
````

Addressed as: `meshfox run tests smoke-test smoke` (node-id path from root,
then the block name).

## CLI

- `meshfox run [--no-deps] <path...> <names>` — run one or more
  comma-separated named blocks reached by walking node ids from the root.
  Each named block's `deps` chain runs first, automatically, in dependency
  order; a dependency shared by several requested blocks only runs once.
  `--no-deps` skips this and runs only the named blocks themselves, in the
  order given — the CLI equivalent of the web UI's plain "run" button next
  to "⛓ run chain". If a node has a `default` block (see "Runnable code
  fences" above — one explicitly flagged `default`, or one whose name
  already matches the node's own `id`), the trailing name can be dropped:
  `meshfox run tests smoke-test` addresses that node's default block
  directly, instead of `meshfox run tests smoke-test <block-name>`, tried
  first as an ordinary address and only falling back to this when that
  doesn't resolve, so it never changes the meaning of an address that
  already worked. Output prints line by line as the block produces it, not
  all at once after it exits — the exit code, printed last, genuinely
  isn't known any sooner. Ctrl+C kills whichever step is currently running
  (the whole process group it spawned, not just `bash`) and stops there;
  whatever earlier steps in the chain already completed stays cached on
  disk.
- `meshfox list` — print every runnable block as an indented tree, each
  with its `[cache]`/`[default]`/`[tty]`/`[deps: ...]`/`[env: ...]` flags and a
  ready-to-paste `meshfox run <path...> <name>` — so you don't have to go spelunking
  through the file to find out what's runnable. A node whose only block
  is its default gets a single merged tree line instead of a separate one
  for the node and another for the block; a node with a default block
  *and* other blocks besides gets the node-id shortcut printed on its own
  header line instead, alongside its other blocks' own lines.
- `meshfox view [--port] [--no-open]` — local web UI, read-only until
  "Edit" is clicked in the browser. A run's output streams into the
  browser live, line by line, as it happens, rather than appearing all at
  once when the block finishes; a running block gets a Kill button, for
  when one hangs. A `tty` block instead opens a real interactive terminal
  panel — see "Interactive (`tty`) blocks" above.
- `meshfox validate` — parse-only validation (single root, no duplicate ids,
  no dangling edges, type body rules, no dangling/cyclic `deps=`); no
  execution, no writes. Exit non-zero on error — usable in CI/pre-commit.
- `meshfox check` — run every embedded constraint fence's Starlark contract
  (see "Constraint fences" above) and report pass/fail per fence. Exit
  non-zero if the file fails to parse or any constraint fails — usable in
  CI/pre-commit alongside (or instead of) `validate`.
- `meshfox spec` — print this specification.

`list`/`view`/`validate`/`check` take the canvas path as an optional
positional argument; `run` takes it as an optional leading argument (recognized by its
`.md` suffix, since node ids never have one). Omit it and any of them
auto-discover the single `*.canvas.md` (or marked `*.md`) file in the
current directory.
