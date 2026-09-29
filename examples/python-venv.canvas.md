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

