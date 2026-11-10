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
# Installs the requirements into it, and reports a launcher for the venv's
# python3 as the computed PYTHON variable via $MESHFOX_VARS_OUT — the same
# `from=` contract SPEC.md's "Computed variables" describes, so a downstream
# block just references `interpreter="$PYTHON -u"` (or `env="$PYTHON"`).
#
# Which Python builds a *new* venv (the interpreter an existing venv runs is
# whatever it was built with):
#   1. `[interpreters.python_venv] python = "python3.12"` in config.toml —
#      exported here as $MESHFOX_CONFIG_INTERPRETERS_PYTHON_VENV_PYTHON; a
#      command name or a path.
#   2. For a `toml` block, the pyproject's own `requires-python`: `python3`
#      if it satisfies the spec, else the newest `python3.N` that does.
#   3. Otherwise the `python3` first on $PATH.
# When (1) or (2) applies and an existing venv built by this script no longer
# matches it (another minor version, or outside `requires-python`), the venv is
# emptied and rebuilt with the chosen Python. Case (3) never rebuilds: with no
# stated requirement, a venv keeps running whatever Python made it.
#
# The launcher (<venv>/bin/python3-mf) is the venv's python3 with the block's
# working directory prepended to PYTHONPATH. meshfox runs a block's code from
# a file that does not live in that directory, and Python puts the script's
# directory on sys.path, not the cwd — so without this a block can't import a
# module sitting next to its canvas. It is rewritten on every run (cheap,
# idempotent), so a venv made before it existed gets it on the next setup.
set -euo pipefail

case "${MESHFOX_BLOCK_LANG:-text}" in
  text|toml) ;;
  *) echo "@python_venv: expected a text or toml block, got ${MESHFOX_BLOCK_LANG}" >&2; exit 2 ;;
esac

venv_dir="${MESHFOX_VENV_DIR:-.venv${MESHFOX_ENVIRONMENT_NAME:+/.named/$MESHFOX_ENVIRONMENT_NAME}}"
mkdir -p "$venv_dir"
venv_dir="$(cd "$venv_dir" && pwd)"

# py_satisfies PYTHON SPEC: exit 0 if PYTHON's version meets the PEP 440
# specifier SPEC, 1 if it does not, 3 if SPEC uses syntax we don't evaluate
# (only >= <= > < == != ~= and `==X.Y.*` are understood).
read -r -d '' PY_SATISFIES <<'PY' || true
import re, sys
cur = tuple(sys.version_info[:3])
pad = lambda t: tuple(t) + (0,) * (3 - len(t))
for clause in sys.argv[1].split(","):
    clause = clause.strip()
    if not clause:
        continue
    m = re.match(r"^(>=|<=|==|!=|~=|>|<)\s*(\d+(?:\.\d+)*)(\.\*)?$", clause)
    if not m:
        sys.exit(3)
    op, ver, star = m.groups()
    v = tuple(int(x) for x in ver.split("."))
    if star and op not in ("==", "!="):
        sys.exit(3)
    if star:
        hit = cur[:len(v)] == v
        ok = hit if op == "==" else not hit
    elif op == "~=":
        if len(v) < 2:
            sys.exit(3)
        ok = pad(v) <= cur and cur[:len(v) - 1] == v[:-1]
    else:
        w = pad(v)
        ok = {">=": cur >= w, "<=": cur <= w, "==": cur == w,
              "!=": cur != w, ">": cur > w, "<": cur < w}[op]
    if not ok:
        sys.exit(1)
PY
PY_MINOR='import sys; print("%d.%d" % sys.version_info[:2])'
py_satisfies() { "$1" -c "$PY_SATISFIES" "$2"; }
py_minor() { "$1" -c "$PY_MINOR" 2>/dev/null || true; }

requires=""
if [[ "${MESHFOX_BLOCK_LANG:-text}" = toml ]]; then
  requires="$(sed -n -E "s/^[[:space:]]*requires-python[[:space:]]*=[[:space:]]*[\"']([^\"']*)[\"'].*/\1/p" "$1" | head -n 1 || true)"
fi

base_python=""
explicit=""
configured="${MESHFOX_CONFIG_INTERPRETERS_PYTHON_VENV_PYTHON:-}"
if [[ -n "$configured" ]]; then
  base_python="$(command -v "$configured" || true)"
  if [[ -z "$base_python" ]]; then
    echo "@python_venv: interpreters.python_venv.python = '$configured' was not found" >&2
    exit 2
  fi
  explicit=config
elif [[ -n "$requires" ]]; then
  seen=""
  for name in python3 python3.16 python3.15 python3.14 python3.13 python3.12 python3.11 python3.10 python3.9; do
    candidate="$(command -v "$name" 2>/dev/null || true)"
    [[ -n "$candidate" ]] || continue
    minor="$(py_minor "$candidate")"
    [[ " $seen " != *" $minor "* ]] || continue
    rc=0
    py_satisfies "$candidate" "$requires" || rc=$?
    if [[ $rc -eq 0 ]]; then
      base_python="$candidate"
      break
    elif [[ $rc -eq 3 ]]; then
      echo "@python_venv: not evaluating requires-python \"$requires\"; using python3" >&2
      requires=""
      break
    fi
    seen="$seen $minor"
  done
  if [[ -n "$requires" && -z "$base_python" ]]; then
    echo "@python_venv: requires-python \"$requires\" but no matching python found on PATH (found:${seen:- none}); set interpreters.python_venv.python in config.toml" >&2
    exit 2
  fi
  [[ -z "$requires" ]] || explicit=requires
fi
[[ -n "$base_python" ]] || base_python=python3

# An existing venv that contradicts an explicit requirement is rebuilt — but
# only one this script made (it leaves python3-mf, and .meshfox-venv from the
# moment of creation), never a virtualenv that merely sits at the fallback
# `.venv`. The `.named` subdirectory holds other named venvs; leave it alone.
venv_python="$venv_dir/bin/python3"
if [[ -x "$venv_python" && -n "$explicit" ]]; then
  mismatch=""
  if [[ "$explicit" = config ]]; then
    have="$(py_minor "$venv_python")"
    want="$(py_minor "$base_python")"
    [[ "$have" = "$want" ]] || mismatch="python ${have:-?} -> $want"
  else
    rc=0
    py_satisfies "$venv_python" "$requires" || rc=$?
    [[ $rc -ne 1 ]] || mismatch="python $(py_minor "$venv_python") does not satisfy \"$requires\""
  fi
  if [[ -n "$mismatch" ]]; then
    if [[ ! -e "$venv_dir/.meshfox-venv" && ! -e "$venv_dir/bin/python3-mf" ]]; then
      echo "@python_venv: $venv_dir ($mismatch) was not created by meshfox; remove it to rebuild" >&2
      exit 2
    fi
    echo "@python_venv: rebuilding $venv_dir ($mismatch)" >&2
    find "$venv_dir" -mindepth 1 -maxdepth 1 ! -name .named -exec rm -rf {} +
  fi
fi
if [[ ! -x "$venv_python" ]]; then
  "$base_python" -m venv "$venv_dir"
  : > "$venv_dir/.meshfox-venv"
fi
if [[ "${MESHFOX_BLOCK_LANG:-text}" = text ]]; then
  "$venv_dir/bin/python3" -m pip install --disable-pip-version-check -r "$1"
else
  project_dir="$(mktemp -d)"
  trap 'rm -rf "$project_dir"' EXIT
  cp "$1" "$project_dir/pyproject.toml"
  "$venv_dir/bin/python3" -m pip install --disable-pip-version-check "$project_dir"
fi
launcher="$venv_dir/bin/python3-mf"
cat > "$launcher.$$" <<LAUNCHER
#!/usr/bin/env bash
export PYTHONPATH="\$PWD\${PYTHONPATH:+:\$PYTHONPATH}"
exec "$venv_dir/bin/python3" "\$@"
LAUNCHER
chmod +x "$launcher.$$"
mv -f "$launcher.$$" "$launcher"
echo "${MESHFOX_ENVIRONMENT_VAR:-PYTHON}=$launcher" >> "$MESHFOX_VARS_OUT"
echo "venv ready: $launcher"
