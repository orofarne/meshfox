<!-- meshfox:canvas -->
# Python Venv Demo
<!-- meshfox:node id="root" -->

A canvas-local virtualenv for Python, following the same `meshfox:var` `from=`
pattern SPEC.md's "Computed variables" describes: the `venv-setup` block below
creates `.meshfox/python-venv.canvas.md.venv/` (idempotently), installs this
demo's one dependency into it, and reports its own interpreter path as the
computed `PYTHON` variable. Every other Python-running fence in this document
references it via `interpreter="$PYTHON -u"` rather than a hardcoded
virtualenv path or whatever Python happens to be on `$PATH` —
useful since a system Python is often externally managed (Homebrew, PEP 668)
and refuses a bare `pip install`.

That `interpreter=` reference is also the *only* ordering this canvas needs:
SPEC.md's "Computed variables" makes a `from=`-declared variable's source
block an implicit dependency of anything that resolves it, exactly like an
explicit `deps=` — so `demo` below needs no `deps=` of its own at all, even
though it can't actually run before `venv-setup` has both created the venv
and installed everything into it.

<!-- meshfox:var name="PYTHON" from="environment/venv-setup" -->

## Environment setup
<!-- meshfox:node id="environment" -->

`interpreter="@python_venv"` is meshfox's own built-in interpreter for
exactly this — a small, fixed set of macro scripts meshfox carries with it.
A `text` fence is read as `requirements.txt`; the builtin creates
its canvas-local virtualenv (idempotent once created), installs the
requirements, and reports the venv's own `python3` as the computed `PYTHON`
variable via `$MESHFOX_VARS_OUT` (SPEC.md's "Computed variables") — the same shell script this node used to
spell out by hand, now shipped inside meshfox itself. Anything that needs
the venv ready just references `$PYTHON` (see the root node's note above)
rather than also declaring an explicit `deps=` on this block. Skipped on a
later run in the same session once its own code hasn't changed (the usual
session-freshness skip).

```text name="venv-setup" interpreter="@python_venv" cache
tabulate==0.9.0
```
<!-- meshfox:output name="venv-setup" hash="4f84ef89" -->
```text
exit code: 0 · 261ms

Requirement already satisfied: tabulate==0.9.0 in ./.meshfox/python-venv.canvas.md.venv/lib/python3.14/site-packages (from -r /var/folders/y2/qq2wc6hd75b06jsjmcvpbmn80000gn/T/meshfox-54304-5a44f457-3d19-49f5-9a5d-90b21f8294c5.tmp (line 1)) (0.9.0)
venv ready: /Users/orofarne/sources/meshfox/examples/.meshfox/python-venv.canvas.md.venv/bin/python3
```
<!-- /meshfox:output -->

The same interpreter also accepts a `toml` fence as `pyproject.toml`. This
alternative installs a temporary project into the same canvas-local venv;
use either setup block for a given project. A build backend and project
metadata are required because pip installs the project directory.

```toml name="venv-pyproject" interpreter="@python_venv" cache
[build-system]
requires = ["setuptools>=61"]
build-backend = "setuptools.build_meta"

[project]
name = "meshfox-venv-example"
version = "0.1.0"
dependencies = ["tabulate==0.9.0"]

[tool.setuptools]
py-modules = []
```

## Demo
<!-- meshfox:node id="demo" -->

Imports the package installed above and prints a small table with it — proof
the venv/install chain actually ran, not just that the files exist.

```python name="demo" interpreter="$PYTHON -u" cache
from tabulate import tabulate

print(tabulate([["meshfox", "canvas"], ["venv", "demo"]], headers=["a", "b"]))
```
<!-- meshfox:output name="demo" hash="b23e8b55" -->
```text
exit code: 0 · 50ms

a        b
-------  ------
meshfox  canvas
venv     demo
```
<!-- /meshfox:output -->

## A module next to the canvas
<!-- meshfox:node id="local-module" -->

`$PYTHON` isn't the venv's bare `python3`: `interpreter="@python_venv"` reports
a small launcher, `.meshfox/python-venv.canvas.md.venv/bin/python3-mf`, which
is the venv's Python with the block's working directory added to
`PYTHONPATH`. meshfox runs a block's code from a file that doesn't live in that
directory, and Python looks next to the *script*, not the cwd — so without the
launcher a block can't import a module sitting beside its canvas. With it, the
block below imports [venv_demo_helper.py](./venv_demo_helper.py) (which itself
uses the venv's `tabulate`) with no `sys.path.insert(0, os.getcwd())`. It
prints a Markdown table, and `output="markdown"` (see
[pandas-dataframe.canvas.md](./pandas-dataframe.canvas.md)) shows it as a
rendered table instead of a block of pipe characters.

```python name="local-module" interpreter="$PYTHON -u" cache output="markdown"
import sys

import venv_demo_helper

print(venv_demo_helper.render([
    ["venv_demo_helper", "next to the canvas (PYTHONPATH from the launcher)"],
    ["tabulate", "the canvas's own virtualenv"],
]))
print()
print(f"Python: `{sys.executable}`")
```
<!-- meshfox:output name="local-module" hash="ad6bb00f" -->

| module           | found via                                         |
|------------------|---------------------------------------------------|
| venv_demo_helper | next to the canvas (PYTHONPATH from the launcher) |
| tabulate         | the canvas's own virtualenv                       |

Python: `/Users/orofarne/sources/meshfox/examples/.meshfox/python-venv.canvas.md.venv/bin/python3`

<!-- /meshfox:output -->

## An independent XLS environment
<!-- meshfox:node id="an-independent-xls-environment" -->

The default environment above exports `PYTHON`. A second setup can install
spreadsheet dependencies independently, without changing that variable or
adding packages to the default environment. `name=xls` selects
`.meshfox/python-venv.canvas.md.venv/.named/xls/`; `var=XLS_PYTHON` selects
the output key. Both options are optional and independent: changing only
`var=` still uses the default environment. Reusing the same `name=` shares
an environment between setup blocks.

<!-- meshfox:var name="XLS_PYTHON" from="xls-setup" -->

```text name="xls-setup" interpreter="@python_venv name=xls var=XLS_PYTHON" cache
python-calamine
```

This consumer automatically runs `xls-setup` through the computed variable.
It imports the XLS reader and prints its separate environment path; no
spreadsheet fixture is needed. To read a real workbook, use
`CalamineWorkbook.from_path("report.xls")`.

```python name="xls-demo" interpreter="$XLS_PYTHON -u" cache
import sys
from pathlib import Path

from python_calamine import CalamineWorkbook

assert Path(sys.prefix).name == "xls"
assert Path(sys.prefix).parent.name == ".named"
print(f"XLS environment: {sys.prefix}")
print(f"Reader ready: {CalamineWorkbook.__name__}")
```

The original consumers keep using `interpreter="$PYTHON -u"`. The two
interpreters now point to distinct environments with separate installed
packages. `@node_env` supports the same `name=` and `var=` options, with
`NODE` as the default output key.

