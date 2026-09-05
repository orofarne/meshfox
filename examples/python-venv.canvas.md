<!-- meshfox:canvas -->
# Python Venv Demo
<!-- meshfox:node id="root" -->

A project-local virtualenv for Python, following the same `meshfox:var` `from=`
pattern SPEC.md's "Computed variables" describes: the `venv-setup` block below
creates `.venv/` (idempotent — a no-op once it already exists), installs this
demo's one dependency into it, and reports its own interpreter path as the
computed `PYTHON` variable. Every other Python-running fence in this document
references it via `interpreter="$PYTHON -u"` rather than a hardcoded
`.venv/bin/python3` path or whatever Python happens to be on `$PATH` —
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

Creates `.venv/` the first time anything below needs it (idempotent — a
no-op once it already exists), installs this demo's requirements into it,
and reports the venv's own `python3` as the computed `PYTHON` variable via
`$MESHFOX_VARS_OUT` (SPEC.md's "Computed variables") — all in one block, so
anything that needs the venv ready just references `$PYTHON` (see the root
node's note above) rather than also declaring an explicit `deps=` on this
block. Skipped on a later run in the same session once its own code hasn't
changed (the usual session-freshness skip).

The requirements list itself is right there in the heredoc below, plain and
readable — `pip install -r -` can't read it from stdin directly (pip only
ever tries to open `-r`'s argument as a real file, stdin or not: `Could not
open requirements file: [Errno 2] No such file or directory: '-'`), but bash
`<(...)` process substitution hands it a real (if ephemeral) path instead,
so this stays one file with nothing to keep in sync by hand.

```bash name="venv-setup" cache
set -euo pipefail
[ -x .venv/bin/python3 ] || python3 -m venv .venv
.venv/bin/python3 -m pip install --disable-pip-version-check -r <(cat <<'REQUIREMENTS'
tabulate==0.9.0
REQUIREMENTS
)
echo "PYTHON=$(pwd)/.venv/bin/python3" >> "$MESHFOX_VARS_OUT"
echo "venv ready: .venv/bin/python3"
```
<!-- meshfox:output name="venv-setup" hash="eaefb65e" -->
```text
exit code: 0 · 2.4s

Collecting tabulate==0.9.0 (from -r /dev/fd/63 (line 1))
  Using cached tabulate-0.9.0-py3-none-any.whl.metadata (34 kB)
Using cached tabulate-0.9.0-py3-none-any.whl (35 kB)
Installing collected packages: tabulate
Successfully installed tabulate-0.9.0
venv ready: .venv/bin/python3
```
<!-- /meshfox:output -->

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
exit code: 0 · 35ms

a        b
-------  ------
meshfox  canvas
venv     demo
```
<!-- /meshfox:output -->

