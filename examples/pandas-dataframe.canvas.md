<!-- meshfox:canvas -->
# Pandas DataFrame Preview

Renders a `pandas` `DataFrame` as an actual Markdown table in the canvas,
instead of the usual preformatted-text output — via the `output="markdown"`
fence attribute (SPEC.md's "Runnable code fences"/"Cached output"): a
`cache`d block's captured stdout is normally wrapped in a passive ` ```text `
fence and shown verbatim; with `output="markdown"` it's spliced in as real
Markdown instead, so `df.to_markdown()` (needs the `tabulate` package) turns
into a real, rendered table on the next run rather than a wall of pipe
characters.

Reuses [examples/python-venv.canvas.md](./python-venv.canvas.md)'s own
pattern for the venv itself: `environment-setup/venv-setup` below creates a
project-local `.venv/` once and reports its interpreter path as the computed
`PYTHON` variable, and every Python fence here references it via
`interpreter="$PYTHON -u"` rather than a hardcoded path or whatever `python3`
happens to be on `$PATH`.

<!-- meshfox:var name="PYTHON" from="environment-setup/venv-setup" -->

## Environment setup
<!-- meshfox:node id="environment-setup" -->

Creates `.venv/` the first time anything below needs it — skipped on a later
run in the same session once this block's own code hasn't changed (the usual
session-freshness skip). Reports the venv's own `python3` as the computed
`PYTHON` variable via `$MESHFOX_VARS_OUT` (SPEC.md's "Computed variables").

```bash name="venv-setup"
set -euo pipefail
[ -x .venv/bin/python3 ] || python3 -m venv .venv
echo "PYTHON=$(pwd)/.venv/bin/python3" >> "$MESHFOX_VARS_OUT"
echo "venv ready: .venv/bin/python3"
```

## Requirements
<!-- meshfox:node id="requirements" -->

`pandas` for the `DataFrame` itself, `tabulate` for `DataFrame.to_markdown()`
(pandas shells out to it rather than implementing Markdown rendering itself).
The requirements list is the body of its own runnable fence — installed
directly via `interpreter="$PYTHON -m pip install -r"` — rather than a
separate checked-in `requirements.txt`, same idiom
[examples/python-venv.canvas.md](./python-venv.canvas.md) uses. Deliberately
without `cache`: pandas's own build log (no prebuilt wheel yet for every
Python version) is noisy and not worth freezing into this file, same
reasoning as README's own "Release build"/"Install" steps.

```text name="install-requirements" deps="environment-setup/venv-setup" interpreter="$PYTHON -m pip install --disable-pip-version-check -r"
pandas==2.2.3
tabulate==0.9.0
```

## Demo
<!-- meshfox:node id="demo" -->

Builds a small `DataFrame` and prints it via `.to_markdown()`. The
`output="markdown"` attribute on the fence below is what turns that printed
pipe-table into an actually-rendered table in the canvas on the next run,
instead of a passive `text` block — the only difference from an ordinary
`cache`d fence.

```python name="demo" interpreter="$PYTHON -u" cache deps="requirements/install-requirements" output="markdown"
import pandas as pd

df = pd.DataFrame(
    {
        "package": ["meshfox", "pandas", "tabulate"],
        "kind": ["canvas", "dataframe", "renderer"],
        "role": ["host document", "the data", "df.to_markdown()"],
    }
)
print(df.to_markdown(index=False))
```
<!-- meshfox:output name="demo" hash="64b91ace" -->

| package   | kind      | role             |
|:----------|:----------|:-----------------|
| meshfox   | canvas    | host document    |
| pandas    | dataframe | the data         |
| tabulate  | renderer  | df.to_markdown() |

<!-- /meshfox:output -->

