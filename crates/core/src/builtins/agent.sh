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
#   - piped/captured -> the one-shot `-p`/`exec` invocation below,
#     using the agent's own access policy (`cache`, CI, captured output).
set -euo pipefail

provider="${MESHFOX_CONFIG_INTERPRETERS_AGENT_PROVIDER:-claude}"

# Substitutes $NAME / ${NAME} references to this fence's own env= locals
# and bound arguments into `$1`. MESHFOX_ENV_NAMES is the comma-separated
# list of these names; invocation-owned context such as MESHFOX_VARS_OUT
# is excluded. Ambient $PATH/$HOME/$MESHFOX_CONFIG_* is never substituted
# unless explicitly declared as an ordinary local by the block.
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
  local rest=$1 out="" prefix token name
  local declared=",${MESHFOX_ENV_NAMES:-},"
  # Consume only the original text. Inserted values are never rescanned.
  while [[ "$rest" == *'$'* ]]; do
    prefix=${rest%%\$*}
    out+="$prefix"
    rest=${rest#"$prefix"}
    if [[ "$rest" == '$$'* ]]; then
      out+='$'
      rest=${rest:2}
      continue
    fi
    if [[ "$rest" =~ ^\$\{([A-Za-z_][A-Za-z0-9_]*)\} ]] ||
       [[ "$rest" =~ ^\$([A-Za-z_][A-Za-z0-9_]*) ]]; then
      token=${BASH_REMATCH[0]} name=${BASH_REMATCH[1]}
      case "$declared" in
        *",$name,"*) out+="${!name-}" ;;
        *) out+="$token" ;;
      esac
      rest=${rest:${#token}}
    else
      out+='$'
      rest=${rest:1}
    fi
  done
  printf '%s' "$out$rest"
}

unknown_provider() {
  echo "@agent: unknown interpreters.agent.provider '$provider' (expected claude, codex, opencode or custom)" >&2
  exit 1
}

# provider = "custom": a free-form shell command from the config. The prompt
# is never spliced into the command text — it arrives as "$1" (and
# $MESHFOX_PROMPT), so quoting inside the prompt can't break or inject.
run_custom() {
  local cmd=$1 key=$2
  if [[ -z "$cmd" ]]; then
    echo "@agent: provider 'custom' needs interpreters.agent.$key in .meshfox/config.toml" >&2
    exit 1
  fi
  export MESHFOX_PROMPT="$prompt"
  exec bash -c "$cmd" agent "$prompt"
}

prompt="$(interpolate "$(cat "$1")")"

if [[ -t 1 ]]; then
  case "$provider" in
    claude)
      # A real interactive session — full tool access, same as running
      # `claude` by hand in this shell, seeded with the fence's own body
      # as the first message instead of an empty prompt.
      exec claude "$prompt"
      ;;
    codex)
      # Interactive CLI, seeded with the prompt; the agent's policy applies.
      exec codex "$prompt"
      ;;
    opencode)
      # The TUI, seeded through its own --prompt flag.
      exec opencode --prompt "$prompt"
      ;;
    custom)
      run_custom "${MESHFOX_CONFIG_INTERPRETERS_AGENT_TTY_COMMAND:-${MESHFOX_CONFIG_INTERPRETERS_AGENT_COMMAND:-}}" tty_command
      ;;
    *)
      unknown_provider
      ;;
  esac
fi

case "$provider" in
  claude)
    # Meshfox adds no access restrictions; the agent's own policy applies.
    exec claude -p -- "$prompt"
    ;;
  codex)
    # Keep Codex's own trust, sandbox and approval settings.
    exec codex exec -- "$prompt"
    ;;
  opencode)
    # One-shot run; opencode's own permission settings apply.
    exec opencode run -- "$prompt"
    ;;
  custom)
    run_custom "${MESHFOX_CONFIG_INTERPRETERS_AGENT_COMMAND:-}" command
    ;;
  *)
    unknown_provider
    ;;
esac
