#!/usr/bin/env bash
# meshfox's built-in `@python_venv` interpreter. $1 is the fence's own
# body — a requirements.txt-shaped list of packages — already written to a
# real file by meshfox (same convention `@agent` follows, see its own
# comment). Because it's a real file rather than inline text, this can
# `pip install -r "$1"` directly; the process-substitution trick
# examples/python-venv.canvas.md's hand-written `venv-setup` block needs
# (`pip install -r <(cat <<'REQUIREMENTS' ...)`) is only there because that
# example keeps the requirements list inline in the same heredoc as the
# rest of its shell code — not a limitation of pip itself.
#
# Creates the venv (idempotent) at $MESHFOX_VENV_DIR — meshfox_core sets
# this to `.meshfox/<canvas filename>.venv`, colocated with (and unique
# per) the canvas that referenced this block, so two unrelated canvases
# sharing a directory never silently share one venv's installed packages.
# Falls back to a plain `.venv` in the block's own cwd when unset (a
# caller that doesn't know its own canvas path — see
# meshfox_core::builtin_interpreter::resolve_with_env's own doc comment).
#
# Installs the requirements into it, and reports the venv's own python3 as
# the computed PYTHON variable via $MESHFOX_VARS_OUT — the same `from=`
# contract SPEC.md's "Computed variables" describes, so a downstream block
# just references `interpreter="$PYTHON -u"` (or `env="$PYTHON"`), same as
# the hand-written example already does today.
set -euo pipefail

venv_dir="${MESHFOX_VENV_DIR:-.venv}"
mkdir -p "$venv_dir"
venv_dir="$(cd "$venv_dir" && pwd)"
[ -x "$venv_dir/bin/python3" ] || python3 -m venv "$venv_dir"
"$venv_dir/bin/python3" -m pip install --disable-pip-version-check -r "$1"
echo "PYTHON=$venv_dir/bin/python3" >> "$MESHFOX_VARS_OUT"
echo "venv ready: $venv_dir/bin/python3"
