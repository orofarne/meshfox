<!-- meshfox:canvas -->
# Agent Prompt Blocks

Meshfox has no dedicated "AI prompt" block type — and doesn't need one. A
runnable fence just resolves to `program args... code` (see SPEC.md's
"Runnable code fences"): the implicit `bash -c code` when there's no
`interpreter=`, or `interpreter`'s own program with `code` written to a temp
file otherwise. An agent CLI is just another program to point that at — and
`interpreter="@agent"` is meshfox's own built-in interpreter for exactly
that (see `crates/core/src/builtins/agent.sh`, shipped in the binary
itself): the fence body becomes the prompt, with `env=`-declared variables
interpolated right into it. This canvas runs it for real (`cache`d output
below each fence, not typed by hand — same convention as
`interpreters.canvas.md`), plus the one thing `@agent` deliberately doesn't
try to cover — real shell logic beyond variable substitution, where a raw
bash fence is still the answer.

<!-- meshfox:var name="TOPIC" prompt="Topic?" default="git rebase" -->

## One-shot via @agent
<!-- meshfox:node id="inline-heredoc" -->

The common case: `interpreter="@agent"`, fence body is the prompt, done.
Captured (non-`tty`, this fence's own shape) means `agent.sh`'s own `[ -t 1
]` check picks its restricted, one-shot `claude -p`/`codex exec` branch —
see [Interactive session via tty](#interactive-session-via-tty) for the
other one. Which provider it actually calls comes from
`interpreters.agent.provider` in `.meshfox/config.toml` (unset here, so it
defaults to `claude`) — see `meshfox_core::config`.

```text name="ask" interpreter="@agent" cache
Ответь одной короткой фразой на русском: что быстрее — заяц или черепаха?
```
<!-- meshfox:output name="ask" hash="35c32118" -->
```text
exit code: 0 · 3.5s

Заяц.
```
<!-- /meshfox:output -->

## Interpolation via env=
<!-- meshfox:node id="context-injection-via-env" -->

`@agent`'s own script (`crates/core/src/builtins/agent.sh`) interpolates
`$NAME`/`${NAME}` references in the prompt itself — three rules decide
exactly when:

1. **Only names this fence's own `env=` declares are ever candidates.**
   `agent.sh` never sees the whole ambient environment, just
   `MESHFOX_ENV_NAMES` — the comma-separated *local* names from `env=`
   (here, just `TOPIC`). A `$-looking` token whose name isn't in that list
   — `$PATH`, `$HOME`, a typo, anything not declared — is left completely
   alone, exactly as typed.
2. **Only as a whole token.** Same "not a prefix of a longer identifier"
   rule `interpreter="$PYTHON -u"` already follows
   (`crate::exec::interpreter_var_refs`): `$TOPIC` matches, `$TOPICS`
   doesn't — the trailing `S` blocks it, so a name that happens to prefix
   a real English word in the prompt is safe.
3. **`$$NAME` always escapes to a literal `$NAME`**, doubling the `$` —
   same convention Make/CI systems already use for this — regardless of
   whether `NAME` is even declared. This is the actual answer to "how do I
   stop a `$`-looking piece of my own prompt from being touched": for a
   declared name you don't want substituted *this once*, escape it; for
   anything not declared, rule 1 already leaves it alone with nothing
   extra to write.

All three at once, in one prompt, one real run:

```text name="ask" interpreter="@agent" env="$TOPIC" cache
Explain $TOPIC in one sentence. Also, literally repeat the words "dollar
sign dollar sign TOPIC" — no wait, repeat this literally instead: $$TOPIC.
And don't touch this at all, it isn't declared: $UNRELATED.
```
<!-- meshfox:output name="ask" hash="e238a5c2" -->
```text
exit code: 0 · 3.9s

Git rebase replays your branch's commits one by one onto a new base commit, rewriting history to produce a linear sequence instead of a merge.

Literal repeat: $TOPIC

I won't touch $UNRELATED since it isn't declared.
```
<!-- /meshfox:output -->

Need more than variable substitution — real shell logic, calling another
program first, building the prompt piece by piece? That's what a raw
`bash` fence is still for: write the `claude -p` invocation (and its
quoting) out by hand, same as before `@agent` existed.

```bash name="ask-raw" env="$TOPIC" cache
claude -p \
  --model haiku \
  --restricted \
  --permission-prompts none \
  --max-budget-usd 0.05 \
  --no-session-persistence \
  -- "$(cat <<EOF
Объясни в одном коротком предложении на русском, что такое $TOPIC.
EOF
)"
```
<!-- meshfox:output name="ask-raw" hash="b42b6440" -->
```text
exit code: 0 · 3.6s

Git rebase — это команда для перемещения ваших коммитов на новую основу, переписывая историю истории коммитов вместо создания merge-коммита.
```
<!-- /meshfox:output -->

## Interactive session via tty
<!-- meshfox:node id="interactive-session-via-tty" -->

`@agent`'s own script tells the two run shapes apart with the same plain
`[ -t 1 ]` check mentioned above — no separate meshfox mechanism needed. A
plain (non-`tty`) block always has its stdout piped for capture, so it gets
the restricted, one-shot `-p`/`exec` answer [One-shot via
@agent](#one-shot-via-agent) already demonstrates. A `tty` block hands the
process a *real* inherited terminal instead — `agent.sh` sees that and
drops into a genuine, unrestricted interactive `claude`/`codex` session
(full tool access), seeded with this fence's own body as the opening
message, exactly as if you'd typed `claude "..."` by hand in this shell.

Not run here — same as `interpreters.canvas.md`'s own `tty` example, this
needs a real interactive terminal to mean anything — but it's a valid,
runnable block: `meshfox run --canvas examples/agent-prompt.canvas.md
interactive-session-via-tty ask`, from an interactive shell, hands you a
real Claude session.

```text name="ask" interpreter="@agent" tty
Let's look at this repository together — what does it do?
```

