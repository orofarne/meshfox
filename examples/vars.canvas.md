<!-- meshfox:canvas -->
# Variables Demo
<!-- meshfox:node id="root" x=0 y=0 w=280 h=60 -->

Every `meshfox:var` setting, side by side — see SPEC.md's "Variables"
section for the full writeup. All ten are declared once here, in the
root node's own body:

<!-- meshfox:var name="GREETING" prompt="Greeting?" default="Hello" -->
<!-- meshfox:var name="INSTALL_PATH" prompt="Install prefix?" default="/usr/local/bin" required -->
<!-- meshfox:var name="LOG_LEVEL" type="select" choices="debug,info,warn,error" default="info" -->
<!-- meshfox:var name="REGION" type="select" choices="us-east-1,eu-west-1,ap-southeast-1" default="us-east-1" required -->
<!-- meshfox:var name="VERBOSE" type="bool" default="false" -->
<!-- meshfox:var name="RETRY_COUNT" prompt="Retry count?" type="int" default="3" -->
<!-- meshfox:var name="API_TOKEN" secret -->
<!-- meshfox:var name="RESOURCE_ID" from="computed/create" -->
<!-- meshfox:var name="DEPLOY_CONFIG" prompt="Deploy which config?" type="select" choices="staging,canary,prod" required session -->
<!-- meshfox:var name="REGIONS_LIST" from="dynamic-choices/list-regions" -->
<!-- meshfox:var name="REGION_DYNAMIC" type="select" choices_var="REGIONS_LIST" required session -->

A declaration on its own does nothing — only a block that opts in via its
own `env=` attribute ever resolves or prompts for one, and only for
whatever it actually references. Each node below demonstrates one
setting in isolation by referencing just its own variable(s).

## Plain default
<!-- meshfox:node id="plain-default" x=32 y=120 w=440 h=456 -->

`GREETING` has a `default` and no other flags — it's used silently,
never prompted for, unless something overrides it (`--set`, the process
environment, or an answer already sitting in the cache from a previous
run).

```bash name="greet" env="$GREETING" cache
echo "$GREETING, world"
```
<!-- meshfox:output name="greet" -->
```text
exit code: 0

Hello, world
```
<!-- /meshfox:output -->

## Required confirmation
<!-- meshfox:node id="required-confirm" x=32 y=636 w=440 h=566 -->

`INSTALL_PATH` also has a `default`, but it's `required` — nothing but
an explicit override/the environment/an already-cached answer is allowed
to resolve it silently. The very first time this block runs, it's
prompted for interactively, with `/usr/local/bin` offered as the
prompt's own pre-filled suggestion (confirm with a bare Enter, or type
something else). Whatever's answered is then cached, so every later run
resolves straight from there — this is a one-time confirmation, not a
standing "ask every run".

```bash name="install" env="$INSTALL_PATH" cache
echo "installing to $INSTALL_PATH"
```
<!-- meshfox:output name="install" -->
```text
exit code: 0

installing to /usr/local/bin
```
<!-- /meshfox:output -->

## Select, not required
<!-- meshfox:node id="select-plain" x=32 y=1262 w=440 h=478 -->

`LOG_LEVEL` is `type="select"` with a `default` and no `required` —
`type` only changes how a prompt is *shown* if one ever happens (a menu
of `choices` instead of free text); it doesn't affect whether one
happens. With a `default` present and no `required`, it resolves
silently exactly like `GREETING` above.

```bash name="serve" env="$LOG_LEVEL" cache
echo "log level: $LOG_LEVEL"
```
<!-- meshfox:output name="serve" -->
```text
exit code: 0

log level: info
```
<!-- /meshfox:output -->

## Select, required
<!-- meshfox:node id="select-required" x=32 y=1800 w=440 h=478 -->

`REGION` combines `type="select"` with `required` — the two are
independent axes (`type` is about *how* to ask, `required` is about
*whether* a `default` gets to skip asking). First run shows a menu with
`us-east-1` pre-selected as the suggested answer; any confirmed choice
is cached from then on.

```bash name="deploy" env="$REGION" cache
echo "deploying to $REGION"
```
<!-- meshfox:output name="deploy" -->
```text
exit code: 0

deploying to us-east-1
```
<!-- /meshfox:output -->

## Bool
<!-- meshfox:node id="bool-flag" x=32 y=2338 w=440 h=434 -->

`VERBOSE` is `type="bool"` with a `default` — a plain (non-`required`)
boolean, so it resolves silently too; a prompt for it (had it been
`required`, or had nothing else supplied a value) would ask `y`/`n`
rather than free text.

```bash name="build" env="$VERBOSE" cache
echo "verbose: $VERBOSE"
```
<!-- meshfox:output name="build" -->
```text
exit code: 0

verbose: false
```
<!-- /meshfox:output -->

## Int
<!-- meshfox:node id="int-count" x=32 y=2832 w=440 h=566 -->

`RETRY_COUNT` is `type="int"` with a `default` — same "resolves silently,
`type` only shapes how a prompt would look" story as `LOG_LEVEL`/
`VERBOSE` above, except an `int` prompt/form field is validated: only
something that actually parses as a whole number is ever accepted (a
terminal prompt keeps re-asking, the web form blocks submit, the TUI's
own field only lets you type digits and a leading `+`/`-` in the first
place) — see `meshfox_core::vars::validate_value`. Unlike `type`'s other
values, this is real enforcement, not just a hint for how to ask.

```bash name="retry" env="$RETRY_COUNT" cache
echo "retrying up to $RETRY_COUNT time(s)"
```
<!-- meshfox:output name="retry" -->
```text
exit code: 0

retrying up to 3 time(s)
```
<!-- /meshfox:output -->

## Secret
<!-- meshfox:node id="secret-token" x=32 y=3458 w=440 h=588 -->

`API_TOKEN` is `secret` — no `default`, never read from or written to
the on-disk cache, never pre-filled anywhere. The only way to supply one
without an interactive prompt is `--set`/the process environment; asked
for fresh every single time some block's `env=` needs it, masked as it's
typed. `secret` and `required` are independent flags, but in practice a
`secret` variable is already "confirmed fresh" on every run regardless —
it can never be silently reused from a cache the way a plain `required`
variable's *second* run can.

Deliberately without `cache` here — caching would freeze a secret's
value into this very file, defeating the point of it never being
persisted anywhere.

```bash name="call-api" env="$API_TOKEN"
echo "calling the API with a token ${#API_TOKEN} characters long"
```

## Computed
<!-- meshfox:node id="computed" x=32 y=4106 w=440 h=520 -->

`RESOURCE_ID` has `from="computed/create"` instead of a `default` —
its value comes from actually running the `create` block below and
reading back what it wrote to `$MESHFOX_VARS_OUT` (a `NAME=value`-per-line
file meshfox hands every `from=` source block a fresh path to), not from
any override/environment/cache/prompt. Running `use-it` automatically
runs `create` first — `from=` is an implicit dependency, the same as an
explicit `deps=` entry — and fails the whole run if `create` exits
nonzero or never writes `RESOURCE_ID`.

```bash name="create"
echo "RESOURCE_ID=resource-42" >> "$MESHFOX_VARS_OUT"
```

```bash name="use-it" env="$RESOURCE_ID" cache
echo "using $RESOURCE_ID"
```
<!-- meshfox:output name="use-it" -->
```text
exit code: 0

using resource-42
```
<!-- /meshfox:output -->

## Session
<!-- meshfox:node id="session-demo" x=32 y=4686 w=440 h=478 -->

`DEPLOY_CONFIG` is `type="select"` with `required session` — unlike a
plain `required` variable (whose confirmation is cached and reused
forever after), `session` means the answer is never written to the
on-disk cache at all, so every separate `meshfox run` invocation asks
again. Referenced by more than one block in the *same* invocation, it's
still only ever prompted for once — `session` only skips the cache, not
the same once-per-invocation reuse every other variable already gets.
Deliberately without `cache` here, same reason `secret-token` above has
none: this needs a fresh interactive answer (or `--set`) every run, so
there's no single "settled" output to freeze into the file.

```bash name="deploy" env="$DEPLOY_CONFIG"
echo "deploying to $DEPLOY_CONFIG"
```

## Dynamic choices
<!-- meshfox:node id="dynamic-choices" x=32 y=5188 w=440 h=520 -->

`REGION_DYNAMIC` has `choices_var="REGIONS_LIST"` instead of a literal
`choices=` — and `REGIONS_LIST` is itself `from=`-computed by
`list-regions` below, so `REGION_DYNAMIC`'s own options ultimately come
from actually running a script, not a hardcoded list. Running `use-region`
automatically runs `list-regions` first (the same implicit-dependency
mechanism `from=` always gets, followed transitively through
`choices_var`) — `REGION_DYNAMIC` itself still needs an interactive
answer (or `--set`) once its choices are known, so this is also without
`cache`.

```bash name="list-regions"
echo "REGIONS_LIST=us-east-1,eu-west-1,ap-southeast-1" >> "$MESHFOX_VARS_OUT"
```

```bash name="use-region" env="$REGION_DYNAMIC"
echo "using $REGION_DYNAMIC"
```

## Combined
<!-- meshfox:node id="combined" x=32 y=5766 w=440 h=610 -->

`env=` takes a comma-separated list — a single block can reference
several declared variables at once, mixing plain, `required`, `select`,
`bool`, and `int` ones freely. Each is still resolved independently (its
own override/environment/cache/`default`, `required` skipping that last
step, `int` validated on the way in, same as anywhere else) — `env=`
just collects whichever of them this one block actually needs into its
process environment together.

```bash name="provision" env="$GREETING,$INSTALL_PATH,$LOG_LEVEL,$VERBOSE,$RETRY_COUNT" cache
echo "$GREETING! installing to $INSTALL_PATH (log level $LOG_LEVEL, verbose=$VERBOSE, retries=$RETRY_COUNT)"
```
<!-- meshfox:output name="provision" -->
```text
exit code: 0

Hello! installing to /usr/local/bin (log level info, verbose=false, retries=3)
```
<!-- /meshfox:output -->

