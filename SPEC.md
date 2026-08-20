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
  - `interpreter="..."` — a shebang-style command, e.g. `interpreter="python3
    -u"` (word-split the same way a `#!/usr/bin/env -S ...` shebang line's
    own arguments would be — quoting is honored), to run against `target`,
    making the node runnable: `interpreter target` (the target's path
    resolved relative to the canvas's own directory, confined to it — same
    boundary `display="code"`/`include` targets are resolved within).
    Optional; omitted means the node isn't runnable this way. The web UI's
    "▷ run" button (next to "expand", in a runnable node's title bar)
    invokes this the same way it does a `text` node's default code block.
    A runnable code *fence* (see "Runnable code fences" below) can carry
    the same `interpreter=` attribute — this is that mechanism's origin.

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
  A `link` node accepts its own single attribute instead:
  - `preview="true"` — fetches the target's OpenGraph metadata
    (title/description/image) and shows it as a card below the plain
    link, in both the web UI and the terminal viewer. `false` (the
    default, omitted from the file) shows just the plain link, same as
    before this attribute existed. Setting `preview=` on a non-`link`
    node is a parse error, same as an unknown `type=`.

    Fetched over the network on first view and cached in memory for the
    life of the `meshfox view`/`meshfox tui` process — not persisted, not
    shared across processes, and never retried within that process once a
    fetch has failed. Since a canvas file's `link` targets are often
    attacker-controllable (the file itself may come from an untrusted
    source), the fetch is hardened against SSRF: only `http`/`https`,
    resolved addresses that are loopback/private/link-local/etc. are
    rejected before ever connecting, and redirects are followed manually
    with the same check re-run on every hop.

    ```
    <!-- meshfox:node type="link" preview="true" -->

    [meshfox](https://github.com/orofarne/meshfox)
    ```
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

### What crosses the include boundary

Position (`x`/`y`) needs no special handling: a spliced node's coordinates
are already relative to its nearest `group` ancestor (see "File structure"
above), and the include node itself becomes a `group`, so an included
subtree lays out correctly with no rewriting at all. A structural
`meshfox:edge` inside the included content is rewritten to the namespaced
id automatically, same as `parent`.

Two other features interact with includes very differently, because one
runs against the raw single file and the other against the fully composed
tree:

- **Runnable-fence `deps=`** (see "Runnable code fences") never crosses an
  include boundary, deliberately: `run`/`list`/`deps::validate` all work
  on one file's raw text (per "Includes" above), so a `deps=` reference is
  only ever resolved against that same file's own, un-namespaced node
  ids. A block inside an included canvas can't be depended on from the
  including document, or vice versa — and an *internal* cross-node
  `deps="other-node/block"` reference inside a file stays valid whether
  that file is run standalone or spliced into a parent, precisely because
  it's never evaluated post-splice.
- **Constraint fences** (see "Constraint fences") are the opposite: `meshfox
  view`, the terminal viewer, and `meshfox check` all evaluate constraints
  against the fully resolved, composed document — so a constraint fence
  living inside an included canvas is checked there too, and `self`/
  `doc.children()`/`.descendants()`/`.nodes_with_tag(...)` navigation from
  it sees the same spliced-in tree everything else does. The one thing
  that *doesn't* survive splicing is a constraint script that hardcodes a
  literal node id (`doc.node("some-id")`): once the file it's written in
  gets included elsewhere, that id is renamed to
  `{include_id}/{original_id}` and the hardcoded reference stops
  resolving. Prefer relative navigation (`self`, `.children()`,
  `.descendants()`) or tag lookups (`.nodes_with_tag(...)`) over a literal
  id in any constraint that might end up inside an included file.

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
- `interpreter="..."` — a shebang-style command, e.g. `interpreter="python3
  -u"`, to run this fence's code under instead of the implicit `bash`/`sh`
  executor: the fence's own body is written to a fresh temp file and run
  as `interpreter target-tmpfile` (word-split the same way a
  `#!/usr/bin/env -S ...` shebang line's own arguments would be — quoting
  is honored, so `interpreter="env \"my python\" -u"` runs `env` with `my
  python` as a single argument). Optional; when set, `lang` no longer has
  to be `bash`/`sh` for the fence to count as runnable at all — `lang`
  becomes purely a syntax-highlighting hint, same role it already plays on
  a `file` node's own `interpreter=` (see "Node types" above, which this
  attribute is the fenced-block counterpart of — both parse the same way).
  Works on a `tty` block too — the interactive session runs directly under
  `interpreter` (given a real pty/terminal) instead of `bash`, e.g.
  `interpreter="python3 -i"` for an interactive Python REPL.
- `tty` — optional flag (`tty` or `tty=true`); this block wants a real
  interactive terminal instead of the usual captured/streamed output —
  e.g. `bash` on its own (a login-style shell), or anything else that reads
  from its own stdin expecting a real terminal (`ssh`, `git commit`'s
  editor, a REPL, a curses UI). Mutually exclusive with `cache` (`meshfox
  validate` error) — an interactive session isn't the deterministic
  "exit code plus text" `cache` saves/replays. A `tty` block may be a
  `deps=`/`from=` target of *any* other block, `tty` or not — each runner
  (CLI, TUI, the web UI) already hands the terminal/pty over at exactly
  that point in the chain and continues once it exits, the same way two
  chained `tty` blocks already hand it over twice, back to back (each
  still running its own `deps=` first, same as any other block). See
  "Interactive (`tty`) blocks" below for how the CLI and web UI actually
  run one, and that section's own `autoclose` for returning to the canvas
  automatically once it exits.

Supported languages without an `interpreter=` attribute: `bash` (`sh` is an
alias for it). A fence in any other language never counts as runnable at
all — not with an explicit `name=`, and not as a node's sole unnamed fence
— *unless* it carries its own `interpreter=` (see above), which makes it
runnable under that command regardless of `lang`. This is what keeps an
ordinary Markdown document's own example fences (a `yaml` config sample, a
`json` snippet, ...) from being mistaken for something to run just because
meshfox was pointed at the file directly (the `meshfox:canvas`
marker/`.canvas.md` suffix is only required for *auto-discovery* — see
"CLI" below — an explicitly-given path still parses whatever heading
structure it finds) — a plain documentation fence has no `interpreter=`
either.

    ```python name="seed" interpreter="python3 -u" cache
    print("seeding...")
    ```

    ```bash name="build" cache
    cargo build --workspace
    ```

    ```bash name="test" cache deps="build"
    cargo test --workspace
    ```

Running `test` here always runs `build` first.

In the web UI and the TUI (long-lived processes — `meshfox view`/`meshfox
tui`, as opposed to a one-shot `meshfox run` invocation), running a chain
this way skips re-running a pulled-in dependency (never the block actually
requested — that one always runs for real) that already ran successfully
earlier in the *same* session and hasn't changed since — same
`meshfox_core::fence::fingerprint` (code/lang/`interpreter=`/`env=`/`deps=`)
comparison `crate::output`'s cached-output staleness uses, so editing a
dependency's code (or its `env=`/`deps=`) makes it eligible to run again
immediately, no explicit cache-busting needed. A skipped step still folds
forward whatever it last wrote via `from=` (see "Computed variables"
below), so a later step in the same chain that depends on that value isn't
affected by the skip. Restarting the process starts fresh — this is
session-scoped, never written to disk.

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
    of strings), **`.text`** (its own raw Markdown body, unrendered) —
    plain read-only fields.
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
  - **`.content()`** — a `file`-type node's own target, read fresh off
    disk and confined to the canvas's own directory (same boundary/cap the
    `display="code"` preview uses — see "Node types"), as a plain
    string. **`.json()`**/**`.yaml()`**/**`.toml()`** parse that same
    content each their own way, handed back as nested Starlark
    dicts/lists/strings/numbers/bools; **`.csv()`** parses it as tabular
    data instead — a list of dicts, one per row, keyed by header. All five
    return `None` — not an error — for anything that isn't a `file` node,
    has no target, doesn't resolve, or (for the four parsers) doesn't
    parse that way; a constraint decides for itself whether that's
    `fail`-worthy. This is the sandbox's one deliberate window onto
    something outside the document itself — but only a target the
    document's own author already committed to in the node's own link, and
    only when whatever's running `meshfox check` chose to make disk access
    available at all (see below).
- **`fail(msg)`** — records a violation *without* stopping the script, so
  one constraint can report every offending node in a single run instead of
  just the first. A script that never calls `fail` passes.

Beyond these, and Starlark's own built-ins (`len`, `range`, string
methods, list/dict comprehensions, ...), the sandbox has nothing: no
network, no way to see any other node's fully-resolved include tree, and
no way to mutate the document — a constraint only ever reads and reports.
The only I/O it can trigger at all is the five `file`-node methods above,
and even those don't run arbitrary code or touch an arbitrary path: the
target was the document author's own choice, visible right there as the
node's link, and the read only happens when the tool driving `meshfox
check` (or the server, evaluating every constraint on every canvas load)
passes it a base directory to resolve targets against in the first place —
an in-memory canvas that was never read from a real file makes every one
of these calls return `None`. A `file` node spliced in from an `include`
target resolves its own target against *that* target's own directory
instead, same as any other relative reference in an included node's body
— the tool-supplied base directory is only the fallback for a `file` node
that lives directly in the document being checked. Evaluation is
resource-bounded (instruction
count, call depth, heap size); a script that times out or errors (syntax
error, unbound name, ...) counts as a failing constraint, with that error
as its one message. Each constraint fence gets its own fresh sandbox —
nothing persists between them, and nothing carries over between one
`meshfox check` run and the next; every `file`-node target that exists in
the document is read and parsed once per `meshfox check` run (while
preparing the fences to evaluate, not lazily per-fence), whether or not
any fence actually calls these methods on it.

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

By default, once a `tty` block's process exits, its exit code (and
whatever it last printed) stays visible until a deliberate action returns
to the canvas — a keypress in `meshfox tui`, closing the panel by hand in
`meshfox view` — same as leaving a real terminal window open after a
command finishes. `autoclose` (a flag, `autoclose` or `autoclose="true"`;
only meaningful on a `tty` block — `meshfox validate` rejects it on
anything else) skips that and returns to the canvas the instant the
process exits instead:

    ```bash name="shell" tty autoclose
    bash
    ```

This only affects `meshfox tui`/`meshfox view` (both long-lived, with a
canvas to actually return *to*) — plain `meshfox run` has no such
distinction to make, a `tty` step there always just hands the terminal
back the moment its process exits, `autoclose` or not.
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
most naturally inside the root node's own body, where a declaration is
document-wide and shows up in `./configure`/the web UI's "Configure
variables":

    <!-- meshfox:var name="INSTALL_PATH" prompt="Install prefix?" default="/usr/local/bin" -->
    <!-- meshfox:var name="LOG_LEVEL" type="select" choices="debug,info,warn,error" default="info" -->
    <!-- meshfox:var name="API_TOKEN" secret -->
    <!-- meshfox:var name="REGION" default="us-east-1" required -->

A `meshfox:var` may also be declared inside any other node — a
**node-scoped** variable, for a value only one block (or a small cluster
of related ones) genuinely needs, e.g. a search term only one ad hoc query
block uses. It's visible only to `env=` on a runnable fence *inside that
node's own subtree* (`meshfox validate` catches a reference from outside
it — see `meshfox_core::validate_var_scope` — though `run`/`view` stay
lenient about it, same as every other `validate`-only check); it's
implicitly `session` (see below) whether or not `session` is written on it
explicitly, so it's never written to the on-disk cache and never shows up
in the document-wide `configure` list — asking about it up front, or
remembering an answer past the current session, wouldn't make sense for a
value this narrowly scoped. `session` only ever changes *whether an answer
is remembered*, though, never *whether one gets asked for* — a variable
with a `default` and no `required` still resolves silently from it, every
time, the same as any other non-`required` declaration; being `session`
just means there's nothing left afterward to fall back on before that
default kicks in. For a value meant to be typed fresh each run despite
having a sensible default (a search term like this one, not a fixed path),
pair it with `required` too — the combination this section's own `session`
bullet already calls out as guaranteeing "a real prompt on *every* run":

    <!-- meshfox:var name="MANUFACTURER_QUERY" prompt="Manufacturer name" default="Sanofi" required -->

Variable names still share one flat namespace across the whole document
regardless of where they're declared — a duplicate `name=`, root or not,
is a `meshfox validate`/`declared_vars` error either way.

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
  Mutually exclusive with `default_var` (`meshfox validate` error).
- `default_var` — `default_var="OTHER_NAME"`: this variable's own
  `default` comes from another declared variable's *resolved* value
  instead of a literal string — e.g. a default install path computed by
  actually running `pwd` in some relevant directory (declare that as its
  own `from=`-computed variable, then reference it here by name). Not a
  `$`-prefixed overload of `default=` itself — `default=`'s value is
  genuinely freeform text, where a `$name` convention would be ambiguous
  with a literal default that happens to look like one (unlike `env=`'s
  dollar-stripping, which is safe only because an `env=` entry is *always*
  a name reference, never literal text). Mutually exclusive with a literal
  `default` and with `from=` (`meshfox validate` error either way) — a
  computed variable is never prompted for, so it has no `default` to
  supply in the first place.
- `choices` — comma-separated; required when `type="select"` and
  `choices_var` isn't given. Mutually exclusive with `choices_var`
  (`meshfox validate` error).
- `choices_var` — `choices_var="OTHER_NAME"`, same idea as `default_var`
  but for `choices` (the referenced variable's resolved value is split the
  same comma-separated way a literal `choices=` already is) — e.g. a
  `select`'s options populated by actually running `aws list-regions`.
  Requires `type="select"`, same as a literal `choices=` does. Mutually
  exclusive with a literal `choices` and with `from=`, same reasoning as
  `default_var`.
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
- `session` — flag (`session` or `session=true`). Never read from or
  written to the on-disk cache — unlike `secret`, input isn't masked; this
  is about *lifetime* (never remembered past the current `meshfox run`
  invocation), not confidentiality. Always true for a node-scoped
  declaration (one outside the root node) whether or not it's written
  explicitly — see above. Combined with `required`, this
  guarantees a real prompt on *every* run rather than silently falling
  back to a cached/default answer — e.g. picking which of several
  configurations to deploy, every time. A plain `session` without
  `required` still silently resolves from `--set`/the environment/a
  `default` when one of those supplies it, exactly like any other
  declaration — it just never *remembers* the answer for next time. A
  variable referenced by more than one block in a single `meshfox run`
  invocation is still only ever prompted for once *within that
  invocation* — `session` only skips the cache, not the same
  once-per-invocation reuse every other variable already gets (see
  "Consumption" below). Mutually exclusive with `from=` (a computed value
  is already never cached, so `session` on it would be a no-op).
- `from` — `from="node-id/block-name"` (or a bare `from="block-name"`,
  meaning a block in the *same node this variable is itself declared in* —
  the same shorthand `deps=`'s same-node form uses; for a root-declared
  variable that's the root node, same as before node-scoped variables
  existed). Makes this a **computed** variable: instead of
  being prompted/defaulted/cached, its value comes from actually running
  the named block and reading back what it wrote to its own
  `MESHFOX_VARS_OUT` file — see "Computed variables (`from=`)" below.
  Mutually exclusive with `default`/`default_var`, `required`, `secret`,
  `session`, and `choices_var` (a `meshfox validate` error to combine any
  of them with `from`) — none of those mean anything for a value that's
  never cached, defaulted, or prompted for. `type`/`choices` still apply,
  validated against whatever the source block actually produced.

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
first block to need it answers the prompt, which is immediately fed back
into that invocation's own resolved-answers (the same override slot
`--set` occupies, not necessarily the on-disk cache — see `session`
below), so every later block referencing the same variable in that same
invocation just reads it from there without asking again.

### Resolution

Resolving one declared variable (for whichever block's `env=` asked for
it) tries, in order: an explicit override (`meshfox run --set
NAME=value`, or a value submitted through the web UI's form) → the
process environment → the on-disk cache (skipped entirely for `secret`/
`session`) → the declaration's own `default` (skipped entirely for
`required` — see above). Whatever isn't resolved by any of those needs an
interactive answer — a terminal prompt for the CLI, a form for the web
UI — which, for a non-secret, non-session variable, is then written to
the cache so a later run of *any* block referencing the same variable
doesn't ask again (this is what turns a `required` variable's mandatory
first confirmation into an
ordinary cache hit on every run after).

The cache lives at `<dir>/.meshfox/<filename>.env`, next to the canvas
file itself (e.g. `examples/hello.canvas.md` ->
`examples/.meshfox/hello.canvas.md.env`) — a plain `NAME=value`-per-line
file, meant to be `.gitignore`d, the same way `CMakeCache.txt` usually
is. It's safe to hand-edit or delete: deleting it just means every
non-secret, non-session variable gets asked about again next time some
block's `env=` needs it.

### Computed variables (`from=`)

A `meshfox:var` with `from=` (see above) never goes through the
override/env/cache/default/prompt chain the rest of this section
describes — its value is *computed*, by running its `from=` block first
and reading back what that block produced:

    <!-- meshfox:var name="RESOURCE_ID" from="provision/create" -->

    ```bash name="create"
    id=$(some-tool create-thing)
    echo "RESOURCE_ID=$id" >> "$MESHFOX_VARS_OUT"
    ```

    ```bash name="deploy" env="$RESOURCE_ID"
    echo "deploying $RESOURCE_ID"
    ```

- **Why not just read the block's own environment?** A process's
  environment is gone the moment it exits, regardless of what language it
  was written in — there's no portable way to peek at a finished child's
  env from outside. Instead, whenever a block being run is a `from=`
  target for some declared variable, meshfox hands it a fresh, empty file
  and tells it where via the `MESHFOX_VARS_OUT` environment variable —
  the block is expected to append `NAME=value` lines to it (same
  `KEY=value`-per-line format the var cache uses) before exiting. A
  temp file, not a pipe: no `mkfifo`/blocking-open deadlock risk, no
  kernel pipe-buffer size limit, and it works the same on every platform
  meshfox runs on. This needs zero per-language support in meshfox itself
  — the same reason CI systems facing the identical problem (GitHub
  Actions' `$GITHUB_OUTPUT`, etc.) converged on the same shape. A block
  that isn't a `from=` target for anything never sees `MESHFOX_VARS_OUT`
  at all.
- **Only trusted on a `0` exit.** A nonzero exit fails the run the same
  way any other step's nonzero exit does — whatever the block wrote (or
  didn't write) to its vars-out file is never read. A `0` exit that
  didn't produce a value for some variable declared `from=` it is also a
  hard failure, not silently treated as "still missing" — a computed
  variable is never prompted for, so there'd be nothing else to fall back
  to.
- **Ordering.** A block's `from=` target is an *implicit* dependency,
  exactly like an explicit `deps=` entry — running (or resolving the
  chain for) any block whose `env=` references a `from=`-declared
  variable automatically runs that variable's source block first. Unlike
  `deps=`, this edge is never skipped by `--no-deps`/the web UI's plain
  "run" (as opposed to "run chain") button: a `deps=` dependency might
  already have fresh cached output, a legitimate reason to skip rerunning
  it, but a `from=`-declared variable has no value at all until its
  source runs, so skipping that edge is never a meaningful choice.
- **Never user-suppliable.** `--set`/a submitted web form/the process
  environment/the on-disk cache can never resolve a `from`-declared
  variable, even if they name it — only an actual run of its `from=`
  block can, so a stale or hand-typed value can never impersonate a
  computed one. Consequently `meshfox configure` and the web UI's pre-run
  vars form never offer a `from`-declared variable as a field.

### Dynamic `default`/`choices` (`default_var=`/`choices_var=`)

`default_var=`/`choices_var=` (see above) let one variable's `default`/
`choices` come from *another* declared variable's resolved value instead
of a literal string — and since that other variable can itself be
`from=`-computed, this is how a `select`'s options (or a text field's
suggested default) end up actually coming from running a script:

    <!-- meshfox:var name="REGIONS_LIST" from="aws/list-regions" -->
    <!-- meshfox:var name="REGION" type="select" choices_var="REGIONS_LIST" -->

    ```bash name="list-regions"
    aws ec2 describe-regions --query 'Regions[].RegionName' --output text | tr '\t' ',' >> "$MESHFOX_VARS_OUT"
    ```

    ```bash name="deploy" env="$REGION"
    echo "deploying to $REGION"
    ```

- **Implicit ordering, transitively.** `REGION` here doesn't declare
  `from=` itself, but resolving it requires `REGIONS_LIST` to already be
  resolved — so a block referencing `REGION` via `env=` gets an implicit
  dependency on `aws/list-regions` (via `REGIONS_LIST`'s own `from=`) the
  same way it would if it referenced `REGIONS_LIST` directly. This chains
  arbitrarily deep: a `default_var`/`choices_var` reference is followed
  transitively, in both dependency-ordering and "which variables does this
  block actually need resolved" scoping, wherever either matters.
- **Substitution, not delegation.** Once `REGIONS_LIST` resolves, its
  value becomes `REGION`'s own effective `choices` (split the same
  comma-separated way a literal `choices=` is) for exactly this
  resolution — `REGION` still goes through its own full override/env/
  cache/default/prompt chain afterward, using that substituted value
  where a literal `choices=`/`default=` would otherwise sit. If the
  reference isn't resolvable yet (rare in practice, since the expected
  `default_var`/`choices_var` target is `from=`-computed and therefore
  already guaranteed resolved by the ordering above), the referencing
  variable is simply deferred rather than shown with stale or empty
  choices.
- Mutually exclusive with the corresponding literal attribute
  (`default`/`choices`) and with `from=` — see each attribute's own
  entry above. `meshfox validate` catches a `default_var`/`choices_var`
  naming a variable nothing declares, and a reference cycle.

### CLI

- `meshfox configure [canvas]` — the one place that *does* walk every
  declared non-secret, non-session variable in the whole document,
  regardless of which (if any) block currently references it — showing
  its
  currently-resolved value (cache / env / default) as the prompt's own
  default, and writing whatever you answer (even if unchanged) back to
  the cache. The explicit "set these up now" step, the same role
  `ccmake`/`cmake -L` plays for a CMake cache; nothing else in meshfox
  requires running it. Requires a terminal; refuses to run (rather than
  silently do nothing or apply bare defaults) if stdin isn't one. Secret
  variables are never shown here — asking for one that's never cached
  and immediately discarded again wouldn't do anything useful.
- `meshfox run` only ever resolves/prompts for a variable that some block
  in the requested chain actually references (no separate configure step
  required, and no prompt at all for a block whose `env=` is empty or
  already fully resolved) — but it does so for the *whole* resolved chain
  up front, before running any of it, not block by block as each one's
  turn comes up. Without this, a variable only a block near the tail of a
  long chain references (e.g. a database password only the final `load`
  step needs) would only be asked for after everything ahead of it had
  already run — the same "resolve the whole chain's variables before
  starting" the web UI's own pre-run form (`GET /api/vars`) already does.
  A `from=`-computed variable is the one exception: nothing has run yet at
  preflight time, so it can't be resolved that early — it's still checked
  right before the block that needs it runs, once its source block has
  actually had the chance to produce it. `--set NAME=value` (repeatable)
  supplies overrides on the command line, the non-interactive equivalent
  of answering a prompt — the same flag CI would use in place of a TTY,
  which `run` otherwise requires whenever some referenced variable is
  still missing after `--set`/env/cache/default. A `--set` value is saved
  to the cache regardless of whether anything in the current invocation
  actually references it, same as `cmake -D` always updating
  `CMakeCache.txt`.

## Options

Document-wide settings that flip a default behavior for the whole canvas —
distinct from `meshfox:var` (a value asked for from whoever runs the
document, and — unlike an option — declarable per-node too, see above) in
that an option has no prompt and no value: it's either declared or it
isn't. Declared as `<!-- meshfox:option name="..." -->` comments, **only
inside the root node's own body** — an option is always document-wide,
never per-node, so unlike `meshfox:var` it has no node-scoped form at all.
A `meshfox:option` found in any other node, one missing `name`, or the
same `name` declared twice, is a `meshfox validate` error.

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

## Tag colors

A node's `color=` (a JSON-Canvas preset `"1"`-`"6"` or a literal
`#rrggbb` hex string) is normally set per node. `meshfox:tag-color`
declares a document-wide default instead: any node carrying a given tag,
with no `color=` of its own, picks up that tag's color automatically.
Same placement restriction as `meshfox:option` (root-only, unlike
`meshfox:var`, which also allows a node-scoped form — see "Variables"
above) — and the same reasoning: the default applies to the whole
document, not one node.

    <!-- meshfox:tag-color tag="bug" color="1" -->
    <!-- meshfox:tag-color tag="feature" color="4" -->

One declaration per tag rather than one comment listing every tag — a
tag name may contain spaces or other characters a bare `key="value"`
token can't safely stand in for, so `tag=` and `color=` each get their
own quoted value.

Precedence, checked in this order:

1. The node's own explicit `color=`, if it has one — always wins.
2. Otherwise, the color declared for the first of the node's own `tags`
   (in the order written on that node) that has one.
3. Otherwise, no color — same as today.

A `meshfox:tag-color` missing `tag=` or `color=`, or the same `tag`
declared twice, is a `meshfox validate` error — every other reader
(`run`/`view`/`tui`, the server) falls back to no tag-derived colors at
all rather than breaking, same best-effort split `meshfox:option` above
has between "parses enough to view" and "fully valid".

## Cached output

Running a `cache`d block writes/updates a fenced block immediately after the
source, wrapped in markers so re-runs replace just that region:

    ```bash name="build" cache
    cargo build --workspace
    ```
    <!-- meshfox:output name="build" hash="a1b2c3d4" -->
    ```text
    exit code: 0 · 4.2s
    ...
    ```
    <!-- /meshfox:output -->

The header line inside the `text` fence carries the exit code alongside how
long the block's own process actually ran (`meshfox_core::format_duration_ms`
— `"842ms"`/`"2.3s"`/`"1m 05s"`), so a re-opened canvas still shows the last
run's duration, not just whether it succeeded. The web UI shows the same
figure live, ticking up in real time while a block is still running (like a
Livebook cell) rather than only once it's done.

The marker's own `hash=` is a short fingerprint
(`meshfox_core::fence::fingerprint`) of everything about the fence that
actually changes what running it does — its code, `lang`, `interpreter=`, and
its `env=`/`deps=` *references* (by name, not a resolved value) — not a
security hash, just a cheap way to tell "this output is still current" from
"the fence changed since this ran": every reader (the web UI, the TUI)
recomputes the fence's own live fingerprint and compares it against this
stored one, showing the cached output as **stale** (still there, never
silently discarded) whenever they differ, until the block is actually
re-run. A marker written before this field existed has no `hash=` at all —
treated the same as stale, since there's nothing to compare against.

Output (live or cached) that contains ANSI SGR color/style escape codes
renders in color in the web UI, both while a block is still streaming and
for a previously-cached block's saved output. Commands run without a
pseudo-terminal, so a tool that auto-detects "not a real terminal" and
disables its own color output (`cargo`, `git`, plain `ls`) prints plain
text here unless it's told to force color (`--color=always`, or a script
emitting raw escape codes itself).

## Comments

    <!-- meshfox:comment -->Any text<!-- /meshfox:comment -->

`meshfox:comment`/`/meshfox:comment` are, by themselves, nothing but two
ordinary HTML comments — invisible to any Markdown renderer, meshfox
included. The text sitting *between* them, though, is completely ordinary
Markdown as far as a plain renderer (GitHub, a text editor's preview, ...)
is concerned, so it renders there like any other paragraph — but meshfox's
own tooling (the web UI, the TUI, `static`/`pdf` export, `meshfox run`'s
prompts) recognizes the marker pair and drops the whole region, markers and
text alike, before a node's body ever reaches any of them. A node's raw
Markdown on disk always keeps the region intact either way; only what
meshfox itself *shows* strips it.

That makes it the place to put context meant only for someone reading the
raw file outside meshfox — most commonly a short "this is a meshfox
document" note, since meshfox's own UI already makes that obvious just by
existing. `meshfox create` (and `meshfox view --create`) writes exactly
that as the new file's root body:

    <!-- meshfox:canvas -->
    # My Project

    <!-- meshfox:comment -->
    > This is a [meshfox](https://meshfox.orofarne.net/) document — open it
    > with `meshfox view` (or `meshfox tui`) for the interactive canvas.
    > This note is only visible here, in a plain Markdown viewer.
    <!-- /meshfox:comment -->

A `file`/`link`/`include` node's body rule ("exactly one Markdown link") is
checked *after* stripping — a comment-wrapped blurb alongside the link
doesn't count against it. Fence-aware, same as heading/node-comment
scanning elsewhere in this spec: a marker written literally inside a code
fence (e.g. showing someone this exact syntax) is left alone rather than
treated as a real region.

## Formal grammar

Reference EBNF for every `meshfox:*` construct described above, collected in
one place. This is documentation only — kept in sync by hand with the actual
parsers (`crates/core/src/{mdcanvas,vars,options,tag_colors,fence,attrs,comment,output}.rs`
and the mirrored bits of the web/TS side), not generated from either one or
used to generate either one. The syntax is simple and stable enough
(single-line constructs, no nesting) that a parser generator would only buy
correctness on the Rust side, while the web/TS side still needs its own
hand-written reader regardless — the same reasoning already applied to
*future* extensions. Treat a mismatch between this grammar and the code as a
bug, usually in this grammar rather than the code.

Notation: `::=` defines, `|` alternation, `[x]` optional, `{x}` zero-or-more,
`'x'` a literal, `<x>` a prose-described terminal.

### Lexical building blocks

Shared by every construct's attribute list (`crates/core/src/attrs.rs`):

    attr-list   ::= { ws attr }
    attr        ::= key '=' value | key
    key         ::= key-char { key-char }
    key-char    ::= <any character except whitespace, '=', '"'>
    value       ::= '"' { <any character except '"'> } '"' | bare-value
    bare-value  ::= <one or more characters, none of them whitespace>
    ws          ::= <one or more whitespace characters>

A bare `key` with no `=value` is a flag, equivalent to `key="true"`. An
unquoted `value` can't itself contain whitespace — there's no escape for
that, only wrapping it in `"..."` instead. Attribute order is never
significant; the same key written twice keeps whichever occurrence the
tokenizer's map-insert resolves to last (last write wins) — not itself a
parse error, though a specific construct's own semantic rules below may
still reject the result.

### Markers

Every `meshfox:*` construct is an HTML comment, matched line-by-line, and
**fence-aware**: a marker written literally inside a code fence (a
documentation example, cached output that happens to contain one, ...) is
never treated as a real one. That scanning rule is a document-structure
property, not part of any single line's own grammar, so it's stated once
here instead of being repeated per construct below.

    node-marker      ::= '<!--' ws 'meshfox:node' attr-list ws '-->'
    edge-marker      ::= '<!--' ws 'meshfox:edge' attr-list ws '-->'
    canvas-marker    ::= '<!--' ws 'meshfox:canvas' ws '-->'
    var-marker       ::= '<!--' ws 'meshfox:var' attr-list ws '-->'
    option-marker    ::= '<!--' ws 'meshfox:option' attr-list ws '-->'
    tag-color-marker ::= '<!--' ws 'meshfox:tag-color' attr-list ws '-->'
    output-open      ::= '<!--' ws 'meshfox:output' attr-list ws '-->'
    output-close     ::= '<!--' ws '/meshfox:output' ws '-->'
    comment-open     ::= '<!--' ws 'meshfox:comment' ws '-->'
    comment-close    ::= '<!--' ws '/meshfox:comment' ws '-->'

`ws` around the tag name/`-->` above may match zero characters in practice
(the parser trims, it doesn't require padding) — written as `ws` rather than
`[ws]` only to reuse the same rule name as `attr-list`.

### Attribute vocabularies

Each construct restricts `attr-list` (above) to its own known keys — the
vocabulary `meshfox validate`'s `unknown_*_attr` checks enforce, though every
*other* consumer (`run`/`view`/`tui`, the server) keeps silently accepting an
unrecognized key, for forward/backward compatibility across format versions
(see "Options" above).

    node-attr      ::= 'id' | 'type' | 'x' | 'y' | 'w' | 'h' | 'color'
                     | 'tags' | 'parent' | 'fold' | 'edgeLabel' | 'display'
                     | 'lang' | 'interpreter' | 'preview'
    edge-attr      ::= 'from' | 'label' | 'color' | 'style' | 'arrowStart'
                     | 'arrowEnd' | 'tags'
    var-attr       ::= 'name' | 'type' | 'prompt' | 'default' | 'default_var'
                     | 'choices' | 'choices_var' | 'secret' | 'required'
                     | 'session' | 'from'
    option-attr    ::= 'name'
    tag-color-attr ::= 'tag' | 'color'
    output-attr    ::= 'name'

`display`/`lang`/`interpreter`/`preview` only mean something when `type` is
`file`/`link` (see "Node types"); `meshfox validate` enforces that
cross-attribute constraint separately — this grammar only fixes the set of
keys a line may use, not which combinations of them make sense together
(same for every other "mutually exclusive with..." rule described in prose
above, e.g. `default`/`default_var`/`from` on a `meshfox:var`).

### Fence info strings

A code fence's info string (the text right after the opening ` ``` `) uses
the same `attr-list` grammar, with the fence's language as an unnamed
leading token instead of a `key=value` pair (`crates/core/src/fence.rs`):

    fence-info      ::= lang [ ws attr-list ]
    lang            ::= bare-value

    runnable-attr   ::= 'name' | 'cache' | 'default' | 'deps' | 'env' | 'tty'
                     | 'autoclose' | 'interpreter'
    constraint-attr ::= 'constraint' | 'name'

A runnable fence additionally requires `lang` to be `bash` or `sh`, *or* its
own `interpreter=` attribute set (see "Runnable code fences"); a constraint
fence requires `lang = 'starlark'`
*and* the bare `constraint` flag (see "Constraint fences") — again,
cross-cutting rules enforced by the fence scanner/`meshfox validate`, not
expressible in `fence-info` alone.

### Values with their own inner structure

A handful of attribute *values* are themselves small comma-separated
grammars, layered on top of `value` (above) rather than on `attr-list`
itself:

    tag-list     ::= tag { ',' tag }
    tag          ::= <one or more characters, none of them ',' or '"'>

    deps-list    ::= deps-entry { ',' deps-entry }
    deps-entry   ::= [ node-id '/' ] block-name

    env-list     ::= env-entry { ',' env-entry }
    env-entry    ::= [ '$' ] var-name [ '=' [ '$' ] var-name ]

    choices-list ::= choice { ',' choice }
    choice       ::= <one or more characters, none of them ','>

`tags=` (on a node or edge), `deps=`, `env=`, and `choices=` (plus
`choices_var=`'s resolved value, split the same way) each use one of these
— see each attribute's own entry above for what the pieces mean.

Ordinary Markdown (a node's own body) and Starlark (a constraint fence's own
script) are each a complete grammar of their own — CommonMark and Starlark
respectively — and out of scope here.

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

`list`/`view`/`validate`/`check` (and every other subcommand with a
positional slot free for it — `configure`/`create`/`tui`/`static`/`pdf`)
take the canvas path either as an optional positional argument or via
`--canvas` (the two are mutually exclusive — pick one). `run` and `node
<op>`, whose own positional slot is taken by other arguments, accept only
`--canvas`. Any command can also take it as a single leading argument
before the subcommand itself — `meshfox path/to.canvas.md validate` —
recognized by its `.md` suffix, since node ids never have one; that leading
form is spliced into whichever of the two shapes the subcommand that
follows actually expects, before it overrides the subcommand's own
path/`--canvas` if both are given. Omit it entirely and any of them
auto-discover the single `*.canvas.md` (or marked `*.md`) file in the
current directory — except `create`, which always requires an explicit
path, since there's nothing to auto-discover for a file that doesn't exist
yet.
