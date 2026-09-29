#!/usr/bin/env bash
# meshfox's built-in `@python_venv` interpreter. $1 is the fence's own
# body, already written to a real file by meshfox. MESHFOX_BLOCK_LANG
# selects requirements.txt (`text`) or pyproject.toml (`toml`).
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
# just references `interpreter="$PYTHON -u"` (or `env="$PYTHON"`).
set -euo pipefail

case "${MESHFOX_BLOCK_LANG:-text}" in
  text|toml) ;;
  *) echo "@python_venv: expected a text or toml block, got ${MESHFOX_BLOCK_LANG}" >&2; exit 2 ;;
esac

venv_dir="${MESHFOX_VENV_DIR:-.venv}"
mkdir -p "$venv_dir"
venv_dir="$(cd "$venv_dir" && pwd)"
[ -x "$venv_dir/bin/python3" ] || python3 -m venv "$venv_dir"
if [ "${MESHFOX_BLOCK_LANG:-text}" = text ]; then
  "$venv_dir/bin/python3" -m pip install --disable-pip-version-check -r "$1"
else
  project_dir="$(mktemp -d)"
  trap 'rm -rf "$project_dir"' EXIT
  cp "$1" "$project_dir/pyproject.toml"
  "$venv_dir/bin/python3" -m pip install --disable-pip-version-check "$project_dir"
fi
echo "PYTHON=$venv_dir/bin/python3" >> "$MESHFOX_VARS_OUT"
echo "venv ready: $venv_dir/bin/python3"
