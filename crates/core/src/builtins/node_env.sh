#!/usr/bin/env bash
# meshfox's built-in `@node_env` interpreter. $1 is the fence's own body,
# already written to a real file by meshfox. MESHFOX_BLOCK_LANG selects what
# it is: a `json` fence is a package.json, a `text` fence is a list of npm
# package specs, one per line (`pele`, `left-pad@1.3.0`, `@scope/pkg@^2`;
# blank lines and `#` comments are skipped).
#
# Creates a node environment (idempotent) at $MESHFOX_NODE_DIR — meshfox_core
# sets this to `.meshfox/<canvas filename>.node`, colocated with (and unique
# per) the canvas that referenced this block, like @python_venv's venv. Falls
# back to a plain `.node-env` in the block's own cwd when unset. Runs
# `npm install` there, but only when the declared packages changed since the
# last successful install (the input's hash is kept next to it).
#
# Reports a launcher for node as the computed NODE variable via
# $MESHFOX_VARS_OUT — the same `from=` contract SPEC.md's "Computed variables"
# describes, so a downstream block just references `interpreter="$NODE"`.
#
# The launcher (<dir>/node) runs the `node` on PATH with this environment's
# node_modules made visible to the code: NODE_PATH for CommonJS `require`, and
# an `--import` resolve hook for ESM `import` (which ignores NODE_PATH) that
# retries a bare specifier Node could not find from the script's own location
# against <dir>/node_modules. meshfox runs a block's code from a file whose
# directory has no node_modules of its own, so without this `import "pele"`
# would not resolve. Relative imports (`./helper.js`) are not affected — those
# are what the `tangle` flag is for. Needs Node 20.6 or newer.
set -euo pipefail

case "${MESHFOX_BLOCK_LANG:-text}" in
  json|text) ;;
  *) echo "@node_env: expected a json or text block, got ${MESHFOX_BLOCK_LANG}" >&2; exit 2 ;;
esac

command -v npm >/dev/null || { echo "@node_env: npm is not installed" >&2; exit 1; }
command -v node >/dev/null || { echo "@node_env: node is not installed" >&2; exit 1; }

node_dir="${MESHFOX_NODE_DIR:-.node-env}"
mkdir -p "$node_dir"
node_dir="$(cd "$node_dir" && pwd)"

if command -v sha256sum >/dev/null; then
  input_hash="$(sha256sum < "$1" | cut -d' ' -f1)"
else
  input_hash="$(shasum -a 256 < "$1" | cut -d' ' -f1)"
fi
stamp="$node_dir/.meshfox-input.sha256"

if [ -d "$node_dir/node_modules" ] && [ -f "$stamp" ] && [ "$(cat "$stamp")" = "$input_hash" ]; then
  echo "node_modules up to date"
else
  if [ "${MESHFOX_BLOCK_LANG:-text}" = json ]; then
    cp "$1" "$node_dir/package.json"
    (cd "$node_dir" && npm install --no-audit --no-fund)
  else
    specs=()
    while IFS= read -r line || [ -n "$line" ]; do
      line="${line%%#*}"
      line="$(printf '%s' "$line" | sed -e 's/^[[:space:]]*//' -e 's/[[:space:]]*$//')"
      [ -n "$line" ] && specs+=("$line")
    done < "$1"
    [ -f "$node_dir/package.json" ] || printf '{"private": true}\n' > "$node_dir/package.json"
    if [ "${#specs[@]}" -gt 0 ]; then
      (cd "$node_dir" && npm install --no-audit --no-fund --save "${specs[@]}")
    fi
  fi
  mkdir -p "$node_dir/node_modules"
  printf '%s' "$input_hash" > "$stamp"
fi

# The ESM resolve hook. `registerHooks` (Node 22.15+/23.5+) is the supported
# API; `register` is the older one, which newer Node deprecates with a warning.
cat > "$node_dir/hooks.mjs.$$" <<'HOOKS'
const anchor = new URL("./anchor.mjs", import.meta.url).href;
export async function resolve(specifier, context, nextResolve) {
  try {
    return await nextResolve(specifier, context);
  } catch (e) {
    if (e.code !== "ERR_MODULE_NOT_FOUND" || /^([./]|file:|node:)/.test(specifier)) throw e;
    return nextResolve(specifier, { ...context, parentURL: anchor });
  }
}
HOOKS
cat > "$node_dir/register.mjs.$$" <<'REGISTER'
import { registerHooks, register } from "node:module";
const anchor = new URL("./anchor.mjs", import.meta.url).href;
if (registerHooks) {
  registerHooks({
    resolve(specifier, context, nextResolve) {
      try {
        return nextResolve(specifier, context);
      } catch (e) {
        if (e.code !== "ERR_MODULE_NOT_FOUND" || /^([./]|file:|node:)/.test(specifier)) throw e;
        return nextResolve(specifier, { ...context, parentURL: anchor });
      }
    },
  });
} else {
  register("./hooks.mjs", import.meta.url);
}
REGISTER
: > "$node_dir/anchor.mjs.$$"
cat > "$node_dir/node.$$" <<LAUNCHER
#!/usr/bin/env bash
export NODE_PATH="$node_dir/node_modules\${NODE_PATH:+:\$NODE_PATH}"
exec node --import "$node_dir/register.mjs" "\$@"
LAUNCHER
chmod +x "$node_dir/node.$$"
for f in hooks.mjs register.mjs anchor.mjs node; do
  mv -f "$node_dir/$f.$$" "$node_dir/$f"
done

echo "NODE=$node_dir/node" >> "$MESHFOX_VARS_OUT"
echo "node env ready: $node_dir/node"
