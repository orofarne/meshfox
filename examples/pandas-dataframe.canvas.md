<!-- meshfox:canvas -->
# Pandas DataFrame Preview

Renders a `pandas` `DataFrame` as an actual Markdown table in the canvas,
instead of the usual preformatted-text output — via the `output="markdown"`
fence attribute (SPEC.md's "Runnable code fences"/"Cached output"): a
`cache`d block's captured stdout is normally wrapped in a passive ` ```text `
fence and shown verbatim; with `output="markdown"` it's spliced in as real
Markdown instead, so `df.to_markdown()` (needs the `tabulate` package —
`pandas` shells out to it rather than implementing Markdown rendering
itself) turns into a real, rendered table on the next run rather than a wall
of pipe characters.

Reuses [examples/python-venv.canvas.md](./python-venv.canvas.md)'s own
pattern for the venv itself, the same `meshfox:var` `from=` idiom SPEC.md's
"Computed variables" describes: `venv-setup` below creates `.venv/`
(idempotent — a no-op once it already exists), installs this demo's two
dependencies into it, and reports its own interpreter path as the computed
`PYTHON` variable. A second, independent computed variable, `TMP_DIR`,
comes from `tmp-dir-setup` — a fresh scratch directory `generate-csv` writes
a random CSV into and `demo` reads it back from. `demo` below references
both (`interpreter="$PYTHON -u"`, `env="TMP_DIR"`) with no `deps=` of its
own for either — both source blocks are already its implicit dependencies
via those two `from=` declarations. What *does* need an explicit `deps=` is
the ordering between `generate-csv` and `demo` themselves: they only agree
on a directory (`$TMP_DIR`) and a filename, not a computed variable, so
nothing implicit forces `generate-csv` to run first.

<!-- meshfox:var name="PYTHON" from="environment-setup/venv-setup" -->
<!-- meshfox:var name="TMP_DIR" from="environment-setup/tmp-dir-setup" -->

## Environment setup
<!-- meshfox:node id="environment-setup" -->

Creates `.venv/` the first time anything below needs it and installs this
demo's requirements into it — both idempotent, so re-running this block on
every use (no `cache` here, unlike `demo` below) costs at most a quick "already
satisfied" pip check, not a real reinstall. Reports the venv's own `python3`
as the computed `PYTHON` variable via `$MESHFOX_VARS_OUT` (SPEC.md's
"Computed variables"), so anything that needs the venv ready just references
`$PYTHON` (see the root node's note above) rather than also declaring an
explicit `deps=` on this block.

`pandas` for the `DataFrame` itself, `tabulate` for `DataFrame.to_markdown()`
(pandas shells out to it rather than implementing Markdown rendering
itself). The requirements list itself is right there in the heredoc below,
plain and readable — `pip install -r -` can't read it from stdin directly
(pip only ever tries to open `-r`'s argument as a real file, stdin or not:
`Could not open requirements file: [Errno 2] No such file or directory:
'-'`), but bash `<(...)` process substitution hands it a real (if
ephemeral) path instead, so this stays one file with nothing to keep in
sync by hand.

```bash name="venv-setup"
set -euo pipefail
[ -x .venv/bin/python3 ] || python3 -m venv .venv
.venv/bin/python3 -m pip install --disable-pip-version-check -r <(cat <<'REQUIREMENTS'
pandas==2.2.3
tabulate==0.9.0
REQUIREMENTS
)
echo "PYTHON=$(pwd)/.venv/bin/python3" >> "$MESHFOX_VARS_OUT"
echo "venv ready: .venv/bin/python3"
```

A second, unrelated setup step: a fresh scratch directory for `generate-csv`
and `demo` below to share, reported as the computed `TMP_DIR` variable the
same way `PYTHON` above is. No `cache` here either — a new directory every
run, same reasoning as `generate-csv`'s own randomness.

```bash name="tmp-dir-setup"
set -euo pipefail
TMP_DIR="$(mktemp -d -t meshfox-demo-XXXXXX)"
echo "TMP_DIR=$TMP_DIR" >> "$MESHFOX_VARS_OUT"
echo "tmp dir ready: $TMP_DIR"
```

## Generate data
<!-- meshfox:node id="generate-data" -->

Generates a small random CSV with plain bash — no Python needed yet, so this
runs independently of `venv-setup` above (it only needs `tmp-dir-setup`'s
`$TMP_DIR`, via `env=`, which is this fence's one implicit dependency).
`$RANDOM` (bash's own built-in, 0-32767) fills in the numbers. The file
itself lives at a fixed name (`data.csv`) inside that run's own scratch
`$TMP_DIR` — unlike `$TMP_DIR` itself, this path isn't a computed variable:
`demo` below just knows the filename and reconstructs the same path from its
own `$TMP_DIR`, so the two blocks need an explicit `deps=` to order them
(see `demo`'s own note). No `cache` here either: re-running it hands `demo`
a fresh random dataset each time, which is rather the point of a "random
CSV" demo.

```bash name="generate-csv" env="TMP_DIR"
set -euo pipefail
CSV="$TMP_DIR/data.csv"
cities=(Berlin Tokyo Lima Nairobi Oslo Wellington)
{
  echo "city,temp_c,humidity_pct"
  for city in "${cities[@]}"; do
    printf '%s,%d,%d\n' "$city" "$(( RANDOM % 40 - 5 ))" "$(( RANDOM % 60 + 20 ))"
  done
} > "$CSV"
echo "generated: $CSV"
```

## Demo
<!-- meshfox:node id="demo" -->

Reads the CSV generated above with `pandas.read_csv` and prints it via
`.to_markdown()`. The `output="markdown"` attribute on the fence below is
what turns that printed pipe-table into an actually-rendered table in the
canvas on the next run, instead of a passive `text` block — the only
difference from an ordinary `cache`d fence.

The script also logs a line to stderr (`sys.stderr`) — with `output="markdown"`,
stderr is captured separately from stdout and shown as its own plain-text
block, *before* the rendered table, regardless of where in the script it was
actually printed (`core::output::render_output_block_markdown`): stdout is
the only half meant to be parsed as Markdown, so stray warnings/log lines
never end up mixed into the table.

This fence mixes both ways a meshfox block can depend on another: `interpreter=`
and `env=` each pull in a `from=`-computed variable (`$PYTHON`, `$TMP_DIR`),
making their source blocks (`venv-setup`, `tmp-dir-setup`) implicit
dependencies — no `deps=` needed for either, per SPEC.md's "Computed
variables". But `generate-csv` doesn't compute a variable this fence
consumes — it just leaves `data.csv` sitting in `$TMP_DIR` under a name both
sides happen to agree on — so ordering it before this fence needs an actual
explicit `deps="generate-data/generate-csv"`.

```python name="demo" cache interpreter="$PYTHON -u" env="TMP_DIR" deps="generate-data/generate-csv" output="markdown"
import os
import sys

import pandas as pd

path = os.path.join(os.environ["TMP_DIR"], "data.csv")
df = pd.read_csv(path)
print(f"loaded {len(df)} rows from {path}", file=sys.stderr)
print(df.to_markdown(index=False))
```
<!-- meshfox:output name="demo" hash="9082ac1e" -->

```text
loaded 6 rows from /var/folders/y2/qq2wc6hd75b06jsjmcvpbmn80000gn/T/meshfox-demo-XXXXXX.Q9Fra105IT/data.csv
```

| city       |   temp_c |   humidity_pct |
|:-----------|---------:|---------------:|
| Berlin     |       32 |             20 |
| Tokyo      |        6 |             38 |
| Lima       |       30 |             23 |
| Nairobi    |       -1 |             77 |
| Oslo       |       33 |             77 |
| Wellington |       13 |             63 |

<!-- /meshfox:output -->

