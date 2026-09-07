#!/usr/bin/env bash
# meshfox's built-in `@agent` interpreter. Reached the same way any real
# `interpreter=` script is: $1 is the fence's own body (a prompt, here),
# already written to a real file by meshfox before this script ever runs —
# see meshfox_core::exec::split_interpreter's own doc comment for that
# shebang-style convention, which `@agent` deliberately doesn't deviate
# from at all.
#
# Which agent CLI to call comes from `interpreters.agent.provider` in
# `.meshfox/config.toml` (local) / `~/.meshfox/config.toml` (global) — see
# meshfox_core::config — exported by the caller as
# $MESHFOX_CONFIG_INTERPRETERS_AGENT_PROVIDER, same as every other
# `MESHFOX_CONFIG_*` flattened key. Unset (no config at all) defaults to
# "claude".
#
# `[ -t 1 ]` tells the two run shapes apart without meshfox itself having
# to know or care which one it's in — a `tty` block hands this script a
# real inherited terminal (`crates/cli/src/main.rs`'s `run_tty_block`, the
# TUI's own copy, `pty_exec`'s real pty), while a plain (non-`tty`) block
# always has its stdout piped for capture (`stream_exec::spawn_interpreter`)
# — the same "is my output a real terminal" check countless other CLI
# tools already use for color/interactivity decisions:
#   - a real terminal -> a genuine interactive `claude`/`codex` session,
#     full tool access, exactly as if run by hand in this shell. Mark the
#     fence `tty` to get this.
#   - piped/captured -> the restricted, one-shot `-p`/`exec` answer below,
#     safe to run unattended (`cache`, CI, the web UI's captured output).
set -euo pipefail

provider="${MESHFOX_CONFIG_INTERPRETERS_AGENT_PROVIDER:-claude}"

# Substitutes $NAME / ${NAME} references to this fence's own declared
# env= vars into `$1` — meshfox exports MESHFOX_ENV_NAMES as the
# comma-separated *local* names from the block's own `env=` list (see
# crate::fence::EnvRef), so this only ever touches names the fence itself
# opted into, never $PATH/$HOME/ambient $MESHFOX_CONFIG_* by accident.
# Word-boundary aware for the bare $NAME form — same "whole token, not a
# prefix of a longer identifier" rule crate::exec::interpreter_var_refs
# already applies to interpreter= (`$TOPIC` matches, `$TOPICS` doesn't;
# `${TOPIC}` is unambiguous either way). Pure bash string ops, no eval —
# this never executes anything the prompt text itself contains, unlike a
# naive `eval echo "$prompt"` would.
#
# A literal `$` a prompt author doesn't want treated as a reference at all
# (talking about a real `$TOPIC` env var in prose, a price like `$5`, ...)
# is escaped by doubling it: `$$TOPIC` -> literal `$TOPIC`, never
# substituted, regardless of whether TOPIC is even a declared name.
interpolate() {
  local text=$1 name value out rest idx tail next
  local esc=$'\x01'
  text=${text//\$\$/$esc}
  IFS=',' read -ra names <<< "${MESHFOX_ENV_NAMES:-}"
  for name in "${names[@]}"; do
    [ -n "$name" ] || continue
    value=${!name-}
    text=${text//\$\{$name\}/$value}
    out="" rest="$text"
    while true; do
      idx=${rest%%\$"$name"*}
      if [ "$idx" = "$rest" ]; then
        out+="$rest"
        break
      fi
      tail=${rest#"$idx"\$"$name"}
      next=${tail:0:1}
      if [[ -n "$next" && "$next" =~ [A-Za-z0-9_] ]]; then
        out+="$idx\$$name$next"
        rest=${tail:1}
      else
        out+="$idx$value"
        rest=$tail
      fi
    done
    text="$out"
  done
  text=${text//$esc/\$}
  printf '%s' "$text"
}

if [ -t 1 ]; then
  prompt="$(interpolate "$(cat "$1")")"
  case "$provider" in
    claude)
      # A real interactive session — full tool access, same as running
      # `claude` by hand in this shell, seeded with the fence's own body
      # as the first message instead of an empty prompt.
      exec claude "$prompt"
      ;;
    codex)
      # codex's own bare interactive entry point — unverified against a
      # real `codex` binary while this skeleton was built (it wasn't
      # installed in the environment that wrote this script).
      exec codex "$prompt"
      ;;
    *)
      echo "@agent: unknown interpreters.agent.provider '$provider' (expected claude or codex)" >&2
      exit 1
      ;;
  esac
fi

prompt="$(interpolate "$(cat "$1")")"
case "$provider" in
  claude)
    # Conservative flags for a *builtin default* — a block whose whole
    # point is a one-shot answer, not a coding agent with real tool
    # access. A canvas author who wants the latter should drop down to a
    # hand-written `interpreter=` script instead (see
    # examples/agent-prompt.canvas.md) rather than expecting `@agent` to
    # cover every shape of "call an agent".
    exec claude -p \
      --restricted \
      --permission-prompts none \
      --no-session-persistence \
      -- "$prompt"
    ;;
  codex)
    # codex exec's own non-interactive flag surface, mirroring the same
    # restricted/one-shot intent as the `claude` branch above — unverified
    # against a real `codex` binary while this skeleton was built (it
    # wasn't installed in the environment that wrote this script), so
    # treat this branch as a starting point to check against the real CLI
    # before relying on it.
    exec codex exec -- "$prompt"
    ;;
  *)
    echo "@agent: unknown interpreters.agent.provider '$provider' (expected claude or codex)" >&2
    exit 1
    ;;
esac
