<!-- meshfox:canvas -->
# Python Venv Demo
<!-- meshfox:node id="root" -->

A project-local virtualenv for Python, following the same `meshfox:var` `from=`
pattern SPEC.md's "Computed variables" describes: the `venv-setup` block below
creates `.venv/` (idempotent — a no-op once it already exists) and reports its
own interpreter path as the computed `PYTHON` variable; every other
Python-running fence in this document references it via `interpreter="$PYTHON
-u"` rather than a hardcoded `.venv/bin/python3` path or whatever Python
happens to be on `$PATH` — useful since a system Python is often externally
managed (Homebrew, PEP 668) and refuses a bare `pip install`.

The other trick this canvas demonstrates: requirements aren't a separate
checked-in `requirements.txt` a shell block reads — they're the body of their
own runnable fence, installed directly via `interpreter="$PYTHON -m pip
install -r"`. `interpreter=` writes a fence's body to a real temp file and
runs it as `interpreter target-tmpfile` (SPEC.md's "Runnable code fences"),
not piped to stdin, so pip's own `-r <file>` flag gets exactly that tmpfile —
the plain requirements list is simultaneously the human-readable doc and
pip's real install input, nothing to keep in sync by hand (the same idiom
`interpreter="psql -f"` uses elsewhere for a `.sql` fence body).

<!-- meshfox:var name="PYTHON" from="environment/venv-setup" -->

## Environment setup
<!-- meshfox:node id="environment" -->

Creates `.venv/` the first time anything below needs it — skipped on a later
run in the same session once this block's own code hasn't changed (the usual
session-freshness skip), so it doesn't re-create the venv every time. Reports
the venv's own `python3` as the computed `PYTHON` variable via
`$MESHFOX_VARS_OUT` (SPEC.md's "Computed variables").

```bash name="venv-setup" cache
set -euo pipefail
[ -x .venv/bin/python3 ] || python3 -m venv .venv
echo "PYTHON=$(pwd)/.venv/bin/python3" >> "$MESHFOX_VARS_OUT"
echo "venv ready: .venv/bin/python3"
```
<!-- meshfox:output name="venv-setup" hash="0aca2e76" -->
```text
exit code: 0 · 2.0s

venv ready: .venv/bin/python3
```
<!-- /meshfox:output -->

## Requirements
<!-- meshfox:node id="requirements" -->

The requirements list itself — not a separate `requirements.txt` (see the
root node's note above). Running this block installs it into `.venv` for
real.

```text name="install-requirements" deps="environment/venv-setup" cache interpreter="$PYTHON -m pip install --disable-pip-version-check -r"
tabulate==0.9.0
```
<!-- meshfox:output name="install-requirements" hash="3b42f795" -->
```text
exit code: 0 · 545ms

Collecting tabulate==0.9.0 (from -r /var/folders/y2/qq2wc6hd75b06jsjmcvpbmn80000gn/T/meshfox-68786-81f0c716-1941-4106-9e6b-8830f5abc3b6.tmp (line 1))
  Using cached tabulate-0.9.0-py3-none-any.whl.metadata (34 kB)
Using cached tabulate-0.9.0-py3-none-any.whl (35 kB)
Installing collected packages: tabulate
Successfully installed tabulate-0.9.0
```
<!-- /meshfox:output -->

## Demo
<!-- meshfox:node id="demo" -->

Imports the package installed above and prints a small table with it — proof
the venv/requirements chain actually ran, not just that the files exist.

```python name="demo" interpreter="$PYTHON -u" cache deps="requirements/install-requirements"
from tabulate import tabulate

print(tabulate([["meshfox", "canvas"], ["venv", "demo"]], headers=["a", "b"]))
```
<!-- meshfox:output name="demo" hash="f49b1746" -->
```text
exit code: 0 · 53ms

a        b
-------  ------
meshfox  canvas
venv     demo
```
<!-- /meshfox:output -->

