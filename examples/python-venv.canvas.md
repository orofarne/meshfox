<!-- meshfox:canvas -->
# Python Venv Demo
<!-- meshfox:node id="root" -->

A project-local virtualenv for Python, following the same `meshfox:var` `from=`
pattern SPEC.md's "Computed variables" describes: `venv-setup` creates
`.venv/` (idempotent — a no-op once it already exists) and reports its own
`python3` path as `VENV_PYTHON`; `requirements` installs this demo's one
dependency into that venv via `interpreter="$VENV_PYTHON -m pip install ...
-r"`; `python-var` then reports that same interpreter path as the computed
`PYTHON` variable, once `requirements` has actually finished installing
everything. Every other Python-running fence in this document references it
via `interpreter="$PYTHON -u"` rather than a hardcoded `.venv/bin/python3`
path or whatever Python happens to be on `$PATH` — useful since a system
Python is often externally managed (Homebrew, PEP 668) and refuses a bare
`pip install`.

That `interpreter=`/`env=` chain is most of the ordering this canvas needs:
SPEC.md's "Computed variables" makes a `from=`-declared variable's source
block an implicit dependency of anything that resolves it, exactly like an
explicit `deps=` — so `demo` below needs no `deps=` of its own at all, even
though it can't actually run before `python-var` has reported `$PYTHON`.
`python-var` itself still needs one explicit `deps="requirements"` (see
below): it only *consumes* `$VENV_PYTHON`, which is enough to force
`venv-setup` first, but not enough to also wait on `requirements` — that
edge has to be spelled out.

<!-- meshfox:var name="PYTHON" from="environment/python-var" -->

## Environment setup
<!-- meshfox:node id="environment" -->

Split into three steps so the plain `requirements.txt` contents can live in
their own block instead of being wrapped in a bash heredoc.

<!-- meshfox:var name="VENV_PYTHON" from="venv-setup" -->

`venv-setup` creates `.venv/` the first time anything below needs it
(idempotent — a no-op once it already exists) and reports its own
`python3` path as the computed `VENV_PYTHON` variable via
`$MESHFOX_VARS_OUT` (SPEC.md's "Computed variables"). It installs nothing
itself — that's `requirements`, below.

```bash name="venv-setup" cache
set -euo pipefail
[ -x .venv/bin/python3 ] || python3 -m venv .venv
echo "VENV_PYTHON=$(pwd)/.venv/bin/python3" >> "$MESHFOX_VARS_OUT"
echo "venv ready: .venv/bin/python3"
```
<!-- meshfox:output name="venv-setup" hash="b8c6fe46" -->
```text
exit code: 0 · 2.1s

venv ready: .venv/bin/python3
```
<!-- /meshfox:output -->


`requirements`'s body is nothing but the plain contents of a
`requirements.txt` — no bash, no heredoc — because a runnable fence's
`interpreter=` writes its body to a real temp file before invoking the
interpreter on it (SPEC.md's "Runnable code fences"), so `pip install -r
<tmpfile>` gets an actual file, unlike `pip install -r -` (pip only ever
tries to open `-r`'s argument as a real file, stdin or not: `Could not open
requirements file: [Errno 2] No such file or directory: '-'`). Referencing
`$VENV_PYTHON` in `interpreter=` makes `venv-setup` this block's implicit
dependency, the same as any other `from=`-computed variable.

```text name="requirements" interpreter="$VENV_PYTHON -m pip install --disable-pip-version-check -r" cache
tabulate==0.9.0
```
<!-- meshfox:output name="requirements" hash="56d61dd6" -->
```text
exit code: 0 · 492ms

Collecting tabulate==0.9.0 (from -r /var/folders/y2/qq2wc6hd75b06jsjmcvpbmn80000gn/T/meshfox-17929-a22e5d61-c7ce-46bf-81e2-d7351a37779e.tmp (line 1))
  Using cached tabulate-0.9.0-py3-none-any.whl.metadata (34 kB)
Using cached tabulate-0.9.0-py3-none-any.whl (35 kB)
Installing collected packages: tabulate
Successfully installed tabulate-0.9.0
```
<!-- /meshfox:output -->


`python-var` only runs once `requirements` has actually finished installing
everything (explicit `deps=`, since it doesn't otherwise consume anything
`requirements` produces) and then reports that same venv interpreter path —
pulled in via `env="VENV_PYTHON"` — as the computed `PYTHON` variable
everything else in this canvas resolves (see the root node's note above).

```bash name="python-var" deps="requirements" env="VENV_PYTHON"
echo "PYTHON=$VENV_PYTHON" >> "$MESHFOX_VARS_OUT"
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
exit code: 0 · 62ms

a        b
-------  ------
meshfox  canvas
venv     demo
```
<!-- /meshfox:output -->

