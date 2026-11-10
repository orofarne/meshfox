<!-- meshfox:canvas -->
# How-to
<!-- meshfox:node id="how-to" -->

Part of the [meshfox documentation](./README.md).

A few common recipes, each pointing at a real, running example elsewhere in
this repo rather than repeating it.

## Structured docs instead of a Makefile
<!-- meshfox:node id="structured-docs-instead-of-a-makefile" -->

This very README is the worked example: every `##` section is a node (see
"Concept" in [README.md](./README.md)), and its runnable fences — [CLI.canvas.md](./CLI.canvas.md) runs real CLI
invocations, [DEVELOPMENT.canvas.md](./DEVELOPMENT.canvas.md) runs the real build/test/lint commands — replace
what would otherwise be a Makefile's targets. `meshfox validate`/`meshfox
check` double as the pre-commit/CI gate ("Full check" in [DEVELOPMENT.canvas.md](./DEVELOPMENT.canvas.md)),
and `meshfox list` prints every runnable block as a tree instead of grepping
the file for what's runnable.

## A canvas's own Python environment
<!-- meshfox:node id="a-canvas-s-own-python-environment" -->

[examples/python-venv.canvas.md](./examples/python-venv.canvas.md) — a
canvas-local `.meshfox/<canvas filename>.venv/`, created once and reported as a computed
`meshfox:var` (`PYTHON`) every other Python fence references via
`interpreter="$PYTHON -u"`, so nothing hardcodes a path or depends on
whatever Python happens to be on `$PATH`. The venv/install step itself is
meshfox's own built-in `interpreter="@python_venv"`: a `text` fence supplies
`requirements.txt`, while a `toml` fence supplies `pyproject.toml`, with no
shell script of your own to keep in sync. `PYTHON` points at a small launcher,
`<venv>/bin/python3-mf`, which is the venv's Python with the block's working
directory added to `PYTHONPATH` — so a block can `import` a module that sits
next to its canvas without a `sys.path.insert(0, os.getcwd())` of its own.

For independent environments in the same canvas, set `name=` and `var=`
inside the interpreter specification. Both are optional: `name=` selects a
subdirectory of the canvas's usual venv, and `var=` changes the exported
variable name (default `PYTHON`). The same name shares an environment;
different names isolate installed packages. For example:

````markdown
<!-- meshfox:var name="XLS_PYTHON" from="xls-setup" -->

```text name="xls-setup" interpreter="@python_venv name=xls var=XLS_PYTHON" cache
python-calamine
```

```python name="read-xls" interpreter="$XLS_PYTHON -u"
from python_calamine import CalamineWorkbook
```
````

Names must start with an ASCII letter or underscore, followed by ASCII
letters, digits, underscores or hyphens. Output variable names follow the
same rules without hyphens. `@node_env` accepts the same options, with
`NODE` as its default output variable.

Node has the same shape: [examples/node-env.canvas.md](./examples/node-env.canvas.md)
uses `interpreter="@node_env"` — a `text` fence of npm packages (or a `json`
fence holding a `package.json`) is installed into a canvas-local
`.meshfox/<canvas filename>.node/` (only when the list changed), and the
computed `NODE` variable is a launcher that makes those packages visible to
both `require` and `import`. Blocks then say `interpreter="$NODE"`.

### Example
<!-- meshfox:node id="example" type="file" -->

[examples/python-venv.canvas.md](./examples/python-venv.canvas.md)

### Example: Node environment
<!-- meshfox:node id="example-node-env" type="file" -->

[examples/node-env.canvas.md](./examples/node-env.canvas.md)

## Rendering rich output (a pandas DataFrame preview)
<!-- meshfox:node id="rendering-rich-output-a-pandas-dataframe-preview" -->

[examples/pandas-dataframe.canvas.md](./examples/pandas-dataframe.canvas.md)
— the `output="markdown"` fence attribute (SPEC.md's "Runnable code
fences"/"Cached output") splices a `cache`d block's captured stdout into the
canvas as real Markdown instead of the default passive `text` fence, so a
command that already prints Markdown worth rendering — here, a `pandas`
`DataFrame` via `df.to_markdown()` — shows up as an actual table rather than
a wall of pipe characters. Builds on the same venv pattern as "A canvas's own
Python environment" above.

### Example
<!-- meshfox:node id="example-7" type="file" -->

[examples/pandas-dataframe.canvas.md](./examples/pandas-dataframe.canvas.md)

## Running a block under a different language/tool
<!-- meshfox:node id="running-a-block-under-a-different-language-tool" -->

[examples/interpreters.canvas.md](./examples/interpreters.canvas.md) — every
shape of `interpreter=` side by side: a bare command (`python3`), one with
its own flags (`python3 -u`), any other tool on `$PATH` (`node`), an
interactive `tty` block handing over a real REPL, and the same mechanism on
a `file`-type node instead of a fence.

A `file` node with an `interpreter=` is a full block, not just a shortcut:
[examples/runnable-files.canvas.md](./examples/runnable-files.canvas.md) shows
one with dependencies, cached output, `meshfox:arg` arguments passed both as
environment variables and as `--name=value` flags (`argv="false"` turns the
flags off), `confirm`, and the same run/chain/history controls a fence gets —
see SPEC.md's "Runnable file nodes".

A block's code is written to a temp file whose extension follows the fence
language, because runtimes pick a mode from it (Node reads a `.tmp` file as
CommonJS only, so an ESM `import` needs `.js`): `python` → `.py`,
`javascript`/`js`/`node`/`mjs` → `.js`, `typescript`/`ts` → `.ts`,
`ruby`/`rb` → `.rb`, `perl`/`pl` → `.pl`, `php`, `lua`, `sql`, `go`,
`bash`/`sh`/`zsh` → `.sh`. A language with no entry keeps `.tmp`. A block
that also needs to find files next to the canvas wants the `tangle` flag —
see "Importing a file that sits beside the canvas" below.

[examples/services.canvas.md](./examples/services.canvas.md) covers long-lived
services: graceful Stop/Restart, the YAML `interpreter="@service"` lifecycle
(`start`, `stop`, optional `monitor`, readiness and health checks), run-scoped
`lease` and `defer` cleanup, and `oneshot`/`reuse` for borrowing external resources
without stopping resources started outside meshfox. It also includes a runnable
Docker Compose stack. Ordinary `service` blocks now receive SIGTERM with a
30-second grace period before SIGKILL.

### Example
<!-- meshfox:node id="example-2" type="file" -->

[examples/interpreters.canvas.md](./examples/interpreters.canvas.md)

### Example: runnable files
<!-- meshfox:node id="example-runnable-files" type="file" -->

[examples/runnable-files.canvas.md](./examples/runnable-files.canvas.md)

### Example: services
<!-- meshfox:node id="example-services" type="file" -->

[examples/services.canvas.md](./examples/services.canvas.md)

## Importing a file that sits beside the canvas
<!-- meshfox:node id="importing-a-file-that-sits-beside-the-canvas" -->

[examples/tangle.canvas.md](./examples/tangle.canvas.md) — a runtime looks up
relative imports (Ruby's `require_relative`, Node's `import "./helper.mjs"`),
`package.json` and `tsconfig` next to the *script file*, and by default that
file is a throwaway in the system temp directory. Put the `tangle` flag next
to the block's `interpreter=` and the code instead runs from a hidden file
with a stable name in the block's working directory,
`.meshfox-tangle-<canvas file>__<block>.<ext>`, written only when the code
changes and left in place after the run. Add `.meshfox-tangle-*` to your
`.gitignore`. If the directory can't be written the run fails with a clear
error rather than quietly importing from somewhere else. Leftovers of blocks
that no longer exist are removed when the canvas's worker starts, and
`meshfox prune` clears those of renamed or deleted canvases.
To rebuild an environment of a canvas that still exists (say after the system
Python was upgraded), `meshfox env list` shows them and `meshfox env rm
<canvas> [--name NAME] [--python | --node]` deletes them — then run the setup
block again with `meshfox run --fresh`, since a `cache`d block does not notice
that its directory is gone. `env rm` refuses while a worker is running for the
canvas.

Two side effects of running from the project directory: a file watcher there
(`nodemon`, `vite`, ...) can see the file appear and change (it does not see a
re-run of unchanged code), and the runtime now finds the project's own
configuration — Node reads the nearest `package.json`, so in a
`"type": "module"` project a tangled block is an ES module (no `require`),
where the same block without `tangle` runs as CommonJS from the temp dir.

Bare package imports are a different matter: those are the job of
`@python_venv` / `@node_env` (see "A canvas's own Python environment").

### Example
<!-- meshfox:node id="example-tangle" type="file" -->

[examples/tangle.canvas.md](./examples/tangle.canvas.md)

## Files as a pipeline's inputs and outputs
<!-- meshfox:node id="files-as-a-pipeline-s-inputs-and-outputs" -->

[examples/artifacts.canvas.md](./examples/artifacts.canvas.md) — `outputs=`
and `inputs=` on a fence make files part of the dependency graph: a block
that reads `artifact-demo/source.csv` finds the block that produces it
without any `deps=` between them, and re-runs only when that file's content
changed (delete an output and it is restored). Handy for a data-processing
project where parser code, patches and downloaded CSV/JSON/ZIP files feed
one another.

### Example
<!-- meshfox:node id="example-artifacts" type="file" -->

[examples/artifacts.canvas.md](./examples/artifacts.canvas.md)

## Checking that documentation stays consistent
<!-- meshfox:node id="checking-that-documentation-stays-consistent" -->

[LICENSE.canvas.md](./LICENSE.canvas.md) below is the real, load-bearing
example: its `every-direct-dep-is-documented` constraint fails `meshfox
check` whenever a `Cargo.toml`/`package.json` dependency has crept in with no
matching row in that file's license tables — the same mechanism
"Constraints" in [REFERENCE.canvas.md](./REFERENCE.canvas.md) walks through in isolation
([examples/constraints.canvas.md](./examples/constraints.canvas.md)'s
"Dependency audit" node).

### Example
<!-- meshfox:node id="example-3" type="file" -->

[LICENSE.canvas.md](./LICENSE.canvas.md)

### Example: constraints
<!-- meshfox:node id="example-constraints-doc" type="file" -->

[examples/constraints.canvas.md](./examples/constraints.canvas.md)

## Publishing a canvas as a static site
<!-- meshfox:node id="publishing-a-canvas-as-a-static-site" -->

See "Static export" in [EXPORT.canvas.md](./EXPORT.canvas.md) for the real `meshfox static`
invocation against [examples/hello.canvas.md](./examples/hello.canvas.md)
and [site-template/](./site-template/) as a standalone example. This README's own repo builds and
publishes itself with [site-template-archive/](./site-template-archive/) (`scripts/build-site.sh`, see `.gitignore`'s
`/site-dist` entry) — the live result is
[meshfox.orofarne.net](https://meshfox.orofarne.net/).

### Example
<!-- meshfox:node id="example-4" type="file" -->

[examples/hello.canvas.md](./examples/hello.canvas.md)

## A second brain for an LLM agent
<!-- meshfox:node id="a-second-brain-for-an-llm-agent" -->

[examples/second-brain.canvas.md](./examples/second-brain.canvas.md) — one
memory per node, tagged by type (`user`/`feedback`/`project`/`reference`),
with a constraint fence enforcing that a `feedback`/`project` memory always
carries a `**Why:**` line so a later session can judge an edge case instead
of blindly following the rule. The same schema this repo's own coding-agent
sessions use for their persistent memory, outside the chat window itself.

### Example
<!-- meshfox:node id="example-5" type="file" -->

[examples/second-brain.canvas.md](./examples/second-brain.canvas.md)

## Summarizing a chat as a canvas (Claude skill)
<!-- meshfox:node id="summarizing-a-chat-as-a-canvas" -->

A skill for regular Claude (claude.ai / desktop — not Claude Code): ask it
to "summarize this conversation as a meshfox canvas" and it answers with a
ready `.canvas.md` file — decisions, open questions, risks and action items
as small nodes, with edges for cross-links. Download
[`summary-to-canvas.zip`](https://github.com/orofarne/meshfox/releases/latest/download/summary-to-canvas.zip)
from the latest release and upload it under *Settings → Capabilities →
Skills*, then check what it wrote with `meshfox validate <file>` and open it
with `meshfox view <file>`. The zip is built from
[skills/summary-to-canvas](./skills/summary-to-canvas/) by the release
workflow; that folder's own README has the build block and install steps.

### Example
<!-- meshfox:node id="example-summary-to-canvas" type="file" -->

[skills/summary-to-canvas/README.canvas.md](./skills/summary-to-canvas/README.canvas.md)

## Navigating between canvases
<!-- meshfox:node id="navigating-between-canvases" -->

A `file` node whose target is itself a `.canvas.md` (or a plain `.md`
carrying the `meshfox:canvas` marker) gets special "↗ open" handling
instead of being handed to the OS's default app: the web UI spawns (or
reuses) a `meshfox view` worker for it and opens a new browser tab; the
TUI's `o` spawns a nested `meshfox tui` in the same terminal instead.
[examples/hello.canvas.md](./examples/hello.canvas.md)'s "Links" node has a
live example ("Related Canvas") — opening it takes you to
[examples/vars.canvas.md](./examples/vars.canvas.md).

### Example
<!-- meshfox:node id="example-6" type="file" -->

[examples/hello.canvas.md#related-canvas](./examples/hello.canvas.md#related-canvas)

### Example: related canvas
<!-- meshfox:node id="example-vars" type="file" -->

[examples/vars.canvas.md](./examples/vars.canvas.md)

## Calling an AI agent from a block
<!-- meshfox:node id="calling-an-ai-agent-from-a-block" -->

[examples/agent-prompt.canvas.md](./examples/agent-prompt.canvas.md) —
`interpreter="@agent"` is one of a small, fixed set of built-in macro
interpreters meshfox ships in its own binary (`crates/core/src/builtins/`,
alongside `@python_venv` above) — the fence body becomes a one-shot prompt
to `claude -p`/`codex exec`, or, on a `tty` block, an interactive CLI
session instead (`agent.sh`'s own `[ -t 1 ]` check tells the two apart,
no separate meshfox mechanism needed).
`env=` locals and bound arguments interpolate right into the prompt as
`$NAME`/`${NAME}` — whole-token only (`$TOPIC` matches, `$TOPICS` doesn't),
and only names belonging to this block are substituted; `$$NAME` escapes
to a literal `$NAME` for anything that shouldn't be touched. No
shell-quoting of your own to write for any of it. Substitution is one pass:
inserted variable values are never interpreted as more references.

Like a bash block, `@agent` runs as the current user. Meshfox adds no agent
access restrictions; the agent's own tool, sandbox, approval, and directory
trust settings apply. `tty` changes interactivity, not this access contract.
Dependency results become stale when the builtin script or effective
provider setting changes.

Which provider a `@name` macro actually calls (`claude`, `codex` or `opencode`, for
`@agent`) comes from a small settings file meshfox itself reads —
`interpreters.agent.provider` in `.meshfox/config.toml` next to the canvas,
or `~/.meshfox/config.toml` globally (local wins, key by key) — a fact
about the machine/what's installed, not something a `meshfox:var` should
have to prompt for on every run. Every dotted key in there is exported to
a macro's own process as `MESHFOX_CONFIG_<PATH>` (e.g.
`MESHFOX_CONFIG_INTERPRETERS_AGENT_PROVIDER`), so a hand-written
`interpreter=` script can read the same settings too.

`provider = "opencode"` runs `opencode run -- "$prompt"` (and
`opencode --prompt "$prompt"` on a `tty` block). `provider = "custom"` runs
any command of your own:

```toml
[interpreters.agent]
provider    = "custom"
command     = 'aider --yes --message "$1"'   # plain block (output captured)
tty_command = 'aider "$1"'                   # tty block; defaults to command
```

The command is a shell snippet run as `bash -c "$command" agent "$prompt"`:
the prompt (after `$VAR` interpolation) is `$1` and also
`$MESHFOX_PROMPT`, never spliced into the command text, so quotes in it
can't break or inject anything. `command` is required for `custom`; a
changed command makes the block's earlier runs stale.

### Example
<!-- meshfox:node id="example-8" type="file" -->

[examples/agent-prompt.canvas.md](./examples/agent-prompt.canvas.md)

## Making a dependency optional
<!-- meshfox:node id="making-a-dependency-optional" -->

[examples/optional-deps.canvas.md](./examples/optional-deps.canvas.md) — a
`?` after a `deps=` entry makes that *edge* conditional on a variable:
`deps="fix?USE_AGENT"` runs `fix` first only while the `bool` variable
`USE_AGENT` is `true`; `fix?!USE_AGENT` negates it, and
`fix?PROVIDER=claude` compares any variable with a value (`?!` negates the
comparison too). The condition belongs to the dependency, not to the block it
points at, so another block can still depend on `fix` unconditionally. A
variable named in a condition is resolved like one named in `env=` — it shows
up in `configure` and in the run form only for chains that reach the edge —
and a `from=`-computed one runs its source first. A false condition drops the
dependency from the chain and from the block's freshness, so flipping the value
makes the block stale. This is the way to keep an optional agent step
(`interpreter="@agent"`, with its provider choice, `$VAR` interpolation and
`tty` mode) in the chain instead of wrapping it in a shell script. Conditions
are valid in `deps=` only; see SPEC.md's `deps` entry. The condition covers
the whole dependency: the producer of an `inputs=` file or the `from=` source
of an `env=` variable is not pulled in while it is off, and the matching input
is ignored.

### Example
<!-- meshfox:node id="example-optional-deps" type="file" -->

[examples/optional-deps.canvas.md](./examples/optional-deps.canvas.md)

## Sharing code between blocks, and testing it (`extends=`)
<!-- meshfox:node id="sharing-code-between-blocks" -->

[examples/extends.canvas.md](./examples/extends.canvas.md) — `extends="defs"`
puts the code of another block in front of this one's and runs the joined text.
Keep the functions in one block (the *definitions*), and let every block that
needs them extend it: the step that does the work adds a line that calls them,
the tests add the assertions. There is no module system to set up, so it works
the same in `python`, `bash`, `node` or anything else with an `interpreter=`,
and the tests see everything the definitions define, private names included.

The extended block is only borrowed from — it is not run and is not a
dependency — so the extending block still declares its own `deps=`, `inputs=`
and `outputs=`; the definitions hold nothing but definitions. The extending
block inherits the `interpreter=` (and `env=`) of what it extends, writes its
own `deps`/`inputs`/`outputs`, and goes stale when a definition changes. Entries
may be chained and are included once, in order; a cycle, a language mismatch
or a parent with `deps=`/`outputs=`/`tty` is a `meshfox validate` error. Line
numbers an interpreter reports count the joined text. See SPEC.md's `extends`
entry.

### Example
<!-- meshfox:node id="example-extends" type="file" -->

[examples/extends.canvas.md](./examples/extends.canvas.md)

## Showing a diagram, formula or chart (SVG)
<!-- meshfox:node id="showing-a-diagram-formula-or-chart-svg" -->

[examples/svg.canvas.md](./examples/svg.canvas.md) — an SVG is an ordinary
Markdown image (`![](data:image/svg+xml;base64,…)`), shown the same way in the
web UI, the TUI, `meshfox static` and `meshfox pdf`, with no renderer built
into meshfox: any command that prints an SVG will do (Graphviz, Typst,
Mermaid, PlantUML, Vega, matplotlib, ...). `output="image"` (SPEC.md's "Image
output") takes the block's stdout *as* the SVG and stores it as that image;
`output="markdown"` does the same when a script prints the image line itself.
An SVG follows the light/dark theme through `currentColor` and
`var(--mf-fg, …)`, and one drawn for a white page can be backed per block with
`output-attrs="bg=#fff"` (MARKDOWN.md's "SVG images").

### Example
<!-- meshfox:node id="example-9" type="file" -->

[examples/svg.canvas.md](./examples/svg.canvas.md)

## Browsing a huge table (CSV, Parquet, ...)
<!-- meshfox:node id="browsing-a-huge-table-csv-parquet" -->

[examples/tables.canvas.md](./examples/tables.canvas.md) — a `file` node with
`display="table"` (SPEC.md's "Table previews") shows its target as an
interactive, read-only grid in the web UI and, full screen with `Enter`, in the
terminal viewer: sortable by column, filterable per column (`>10`, `=x`,
`^start`, `~part`, `null`, ...), searchable across every column, with the whole
value of any cell a click away. Only the rows on screen are ever fetched, so a
CSV with a million rows opens as fast as one with twenty — the canvas generates
one to try, and turns it into Parquet (read in place, no import) with `duckdb`
itself. Any format the `duckdb` command-line tool reads works, `.csv.gz`
included.

CSV/TSV previews use strict dialect validation by default. For an export with
mixed CRLF/LF line endings, set `csv-strict="false"` on that particular
`meshfox:node type="file" display="table"` comment. This setting applies
only to that node's preview, does not skip error rows, and does not change
how runnable import blocks read the file.

It needs that `duckdb` executable on `PATH` (or `duckdb_path` under `[tables]`
in the config above): meshfox never installs it, and a table node without it
says so rather than showing anything. The table is read-only — edit the file
with any editor and the node reloads — and `meshfox static`/`meshfox pdf` print
its first 100 rows as a plain grid, failing loudly if a table can't be read.

### Example
<!-- meshfox:node id="example-10" type="file" -->

[examples/tables.canvas.md](./examples/tables.canvas.md)

## Typed variables, forms and file pickers
<!-- meshfox:node id="typed-variables-forms-and-file-pickers" -->

[examples/ffmpeg.canvas.md](./examples/ffmpeg.canvas.md) — a cut-down `ffmpeg`
how-to that uses every variable type at once and one `form` fence to set them:
`type="file"` (an input with `mode="open"`, an output with `mode="save"`,
`ext="mp4,mkv"` as a picker hint), `type="dir"`, `select`, `int`, `bool` and
plain strings. A `dir` or `file` field has a **Browse…** button in the web UI
(`Ctrl-O` in the terminal) that opens a file browser over the *worker's* file
system — where the blocks actually run, which may not be where the UI is — and
answers with an absolute path. SPEC.md's "File variables" and "Path pickers"
have the details.

`type="date"` and `type="datetime"` have their own recipe, "Dates, times and
calendars", just below.

A chosen path is also a *permission*: a `dir` value becomes a trusted location
a `file` node may read below, and a `file` value one trusted file — exactly that
one, never its folder — so `[report]($OUTPUT_DIR/probe.json)` or `[clip]($INPUT)`
previews what you picked, while a canvas's own text still cannot reach outside
its directory by itself.

Needs `ffmpeg` and `ffprobe` on `PATH`; the first block generates a test clip,
so the example works before you pick a video of your own.

### Example
<!-- meshfox:node id="example-11" type="file" -->

[examples/ffmpeg.canvas.md](./examples/ffmpeg.canvas.md)

## Dates, times and calendars
<!-- meshfox:node id="dates-times-and-calendars" -->

[examples/dates.canvas.md](./examples/dates.canvas.md) — two small calculators built on `type="date"` and `type="datetime"` variables: calendar days between two dates, and hours between two moments that each carry their own UTC offset. A `form` sets the values and the `autorun` blocks recalculate on **Apply**.

A `date` is `YYYY-MM-DD`; a `datetime` is RFC3339 *with* an offset (`2026-10-25T14:30:00+03:00`). Both can be typed by hand and picked from a calendar: the browser's native date and date-time controls (the latter beside an editable UTC-offset box) in the web UI, a calendar modal on `Ctrl-O` in the terminal (`[Pick… ^o]` after the field). Because a block always receives the canonical form, plain shell parameter expansion takes it apart — the example does its arithmetic in bash without `date`, which differs between macOS and Linux.

Every value is checked against its type, wherever it comes from: `--set`, prompts and forms are rejected on entry, and a bad value in the environment, the variable cache, shared config or a `from=` block's output fails the run with a message naming the variable and where the value came from (a stale cache entry is fixed with `--set`). SPEC.md's "Date and datetime variables" and "Date pickers" have the details.

### Example
<!-- meshfox:node id="example-15" type="file" -->

[examples/dates.canvas.md](./examples/dates.canvas.md)

## Lists, map and reduce
<!-- meshfox:node id="lists-map-and-reduce" -->

[examples/lists.canvas.md](./examples/lists.canvas.md) — a `type="list"` variable holds an ordered list of strings, and a dependency `deps='square[n=*$NUMBERS]'` runs once per element: a producer lists the work, a parameterized block handles one element, and the consumer reads the whole list to reduce. Several `*` bindings give the cartesian product; an empty list simply removes the dependency, and a list a `from=` block has not produced yet is expanded as soon as that block finishes.

The list itself has no separator. A producer writes it as one `NAME[]=item` line per element (or a JSON array), and every block that reads it chooses the shape in its own `env=`: `env="NUMBERS|space"` for shell words, `env="IDS=NUMBERS|pg"` for a PostgreSQL array, no suffix for JSON. A list with `choices` is a set of checkboxes in a `form` and a numbered menu at the terminal. SPEC.md's "List variables" has the formats and the rules.

### Example
<!-- meshfox:node id="example-lists" type="file" -->

[examples/lists.canvas.md](./examples/lists.canvas.md)

## Calling an application in another canvas
<!-- meshfox:node id="calling-an-application-in-another-canvas" -->

A native `meshfox` fence describes a child invocation as YAML: `canvas`, `run`, and optional `set`, `fresh`, `no-deps`, or `skip-fresh` fields. The caller prepares child input and confirmation before its own chain starts, streams the child's steps and output, and forwards cancellation to that specific child run. The child keeps its own worker, working directory, locks and history.

[examples/canvas-call.canvas.md](./examples/canvas-call.canvas.md) contains a runnable parent, an embedded execution diagram, input, cancellation and failure examples. Its separate [child canvas](./examples/canvas-call-data/child.canvas.md) shows the other side of the call.

### Example
<!-- meshfox:node id="example-12" type="file" -->

[examples/canvas-call.canvas.md](./examples/canvas-call.canvas.md)

### Example: child canvas
<!-- meshfox:node id="example-canvas-call-child" type="file" -->

[examples/canvas-call-data/child.canvas.md](./examples/canvas-call-data/child.canvas.md)

## Computing a Markdown table from document tags
<!-- meshfox:node id="computing-a-markdown-table-from-document-tags" -->

Use a `starlark render` fence to turn the document tree into a live Markdown view, without launching an external interpreter. The **Computed tag summary** node in [examples/constraints.canvas.md](./examples/constraints.canvas.md#computed-tag-summary) counts nodes by tag and prints a table with `md.table()`.

Open the example in WebUI or TUI: the table appears below the collapsed source and updates when tags or nodes change. `md` builders return typed `MarkdownFragment` values, so links and future formatting helpers can compose inside tables without double escaping.

Static-site and PDF exports include the computed table as a snapshot of the document at export time. The exported site does not run Starlark in the browser; regenerate the export to update its results. The live document's source, cached-output regions and history never store the generated output.

### Example
<!-- meshfox:node id="example-13" type="file" -->

[examples/constraints.canvas.md](./examples/constraints.canvas.md)

## Reading a single-node canvas as a document
<!-- meshfox:node id="reading-a-single-node-canvas-as-a-document" -->

A canvas containing only its root node opens as a full-page document in the web UI's view mode: title, scrollable content and the usual runnable blocks, with the toolbar still available. Edit mode keeps the regular canvas. Adding a child restores the canvas in view mode too; folding a larger tree does not activate the document layout. No option is needed, and the root's saved position and size are preserved.

### Example
<!-- meshfox:node id="example-14" type="file" -->

[examples/single-node.canvas.md](./examples/single-node.canvas.md)
