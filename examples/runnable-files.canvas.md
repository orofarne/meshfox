<!-- meshfox:canvas -->
# Runnable Files Demo
<!-- meshfox:node id="root" -->

A `file` node with an `interpreter=` is a runnable block whose code lives in
a file instead of a fence: `interpreter target` runs the file, and the node
behaves like a fence in every other way — `deps=`, `env=`, `cache`,
`confirm`, `meshfox:arg` arguments, run history, `run -d`. See SPEC.md's
"Node types" (the `file` entry) for the reference.

The block is always named after its node, so `meshfox run <node>` addresses
it without a trailing block name. Cached output below is from actual
`meshfox run` invocations (for example `meshfox
examples/runnable-files.canvas.md run hello`), not typed by hand.

## Hello
<!-- meshfox:node id="hello" type="file" interpreter="bash" cache="true" -->

[hello.sh](./runnable-files-data/hello.sh)

Plain case. The script runs in the canvas's own directory, not the
script's: `cwd` below is `examples`, while the script lives one level
deeper. The path in the link is resolved relative to the canvas and may
not leave its tree (or the directories of `dir`/`file` variables).

<!-- meshfox:output name="hello" hash="4f565502" -->
```text
exit code: 0 · 16ms

hello from hello.sh
cwd: examples
```
<!-- /meshfox:output -->

## Shown as code
<!-- meshfox:node id="shown" type="file" display="code" lang="python" interpreter="python3" -->

[extract.py](./runnable-files-data/extract.py)

`display="code"` previews the script itself, so the node reads like a fence
that happens to be backed by a file — the preview is read fresh from disk,
and the block's fingerprint follows the file's content: edit the script and
its cached output (and "already ran this session" freshness) goes stale.
This node only previews and runs; the `Arguments` node below declares
arguments for the same script.

## Dependencies
<!-- meshfox:node id="pipeline" -->

Any block can depend on a file node's block as `node-id/node-id`, and a file
node can carry its own `deps=` on its `meshfox:node` line — the same
attributes a fence puts in its info string (`deps`, `env`, `cache`,
`confirm`, `default`, `lease`, `defer`, ...), minus `name` (always the node
id), `interpreter` (its own attribute) and `fold`. They are only accepted on
a file node that has an `interpreter`; `meshfox validate` rejects them
anywhere else.

```bash name="all" deps="prepare/prepare,hello/hello"
echo "pipeline done"
```

### Prepare
<!-- meshfox:node id="prepare" type="file" interpreter="bash" -->

[prepare.sh](./runnable-files-data/prepare.sh)

## Arguments
<!-- meshfox:node id="arguments" -->

A file node declares arguments exactly like a fence — `meshfox:arg` lines —
but they sit immediately above the *link* (nothing but blank lines between,
the same rule as above a fence). Values reach the script two ways: as
environment variables (always, like fence arguments) and as `--name=value`
flags after the script path, in declaration order. `python3 extract.py
--lang=hy --pages=3` below also has `lang` and `pages` in its environment.

### Extract
<!-- meshfox:node id="extract" type="file" interpreter="python3" -->

<!-- meshfox:arg name="lang" type="select" choices="en,hy" default="en" -->
<!-- meshfox:arg name="pages" type="int" default="0" -->
[extract.py](./runnable-files-data/extract.py)

Launched as an application: `meshfox examples/runnable-files.canvas.md run
arguments extract 'extract[lang=hy,pages=3]'`, from a manual form in the web
UI/TUI, or as a dependency (`deps='extract/extract[lang=hy]'`). A block with
arguments cannot use `cache` (cached output is kept per block, not per
application), for file nodes and fences alike — `meshfox validate` says so.
That run prints `env:  lang=hy pages=3` and `argv: lang=hy pages=3` (copied
from the terminal).

### Environment only
<!-- meshfox:node id="report" type="file" interpreter="bash" argv="false" -->

<!-- meshfox:arg name="lang" type="select" choices="en,hy" default="en" -->
[report.sh](./runnable-files-data/report.sh)

`argv="false"` keeps the flags off the command line for a script that does
not parse them (an `argparse` script would reject unknown flags). The
arguments are still in the environment, so `meshfox ... run arguments report
'report[lang=hy]'` prints `report for lang=hy (argv: 0 extra arguments)`.
Only blocks with an `interpreter` get flags; a plain `bash` fence has
no script path to append them to and receives the environment only.

## Confirmation
<!-- meshfox:node id="reset" type="file" interpreter="bash" confirm="true" -->

[reset.sh](./runnable-files-data/reset.sh)

`confirm` works as on a fence: every invocation, including as someone else's
dependency, needs explicit approval — `meshfox ... run reset --confirm`, the
confirmation dialog in the web UI, `y` in the TUI.

## Not supported
<!-- meshfox:node id="limits" -->

- `tty`, `service` and `tangle` on a file node — the script is run directly
  from its file; `meshfox validate` rejects them.
- `cache` together with arguments (see `Extract`).
- More than one block per file node, or other fences in its body: the body
  is the link, an optional plain-prose caption, and — written by meshfox —
  one trailing cached-output region.
