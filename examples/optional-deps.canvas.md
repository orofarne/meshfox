<!-- meshfox:canvas -->
# Optional Dependencies
<!-- meshfox:node id="root" -->

A dependency can be switched on and off by a variable. The condition is written
on the **edge** — `deps="fix?USE_AGENT"` — not on the block it points at, so
`fix` stays an ordinary block that any other block may still depend on
unconditionally, or run directly — running it ignores every condition that
points *at* it (its own `deps=` conditions still apply).

| Entry | The edge exists when |
| --- | --- |
| `fix?USE_AGENT` | the `bool` variable `USE_AGENT` is `true` |
| `fix?!USE_AGENT` | it is `false` |
| `fix?PROVIDER=claude` | `PROVIDER` equals `claude` (any variable type) |
| `fix?!PROVIDER=claude` | `PROVIDER` is anything else |
| `fix?USE_AGENT!` | like the first, plus the usual sync `!` |

An edge whose condition is false is simply not there: the dependency is not run
for this block and does not count towards its freshness, so flipping the value
makes the block stale. If the variable is computed (`from=`), its source runs
first and the edge is decided afterwards.

The two variables below are the whole "configuration" of this canvas. Neither
block asks for them through `env=` — a condition is enough for `meshfox` to
resolve the variable (and to ask for it, when it has no default) as soon as a
chain reaches the edge.

<!-- meshfox:var name="USE_AGENT" type="bool" default="false" prompt="Let an agent repair the file first?" -->
<!-- meshfox:var name="PROVIDER" type="select" choices="claude,codex" default="claude" prompt="Which agent?" -->

## Convert
<!-- meshfox:node id="convert" -->

`convert` always runs. The repair step in front of it is optional:

```bash name="convert" default deps="repair/fix?USE_AGENT"
echo "converting the (possibly repaired) input"
```

Try it from the repository root:

```text
meshfox examples/optional-deps.canvas.md run convert                       # only "convert"
meshfox examples/optional-deps.canvas.md run --set USE_AGENT=true convert  # "fix" first
meshfox examples/optional-deps.canvas.md run repair fix                    # "fix" regardless of USE_AGENT
```

### Files and variables follow the same edge

A condition on a `deps=` entry covers everything that makes this block depend
on the target, not only the explicit edge. If `fix` declared
`outputs="fixed.md"` and `convert` declared `inputs="fixed.md"`, the producer
would normally be pulled in automatically; with `deps="fix?USE_AGENT"` it is
pulled in only while `USE_AGENT` is `true`, and while it is `false` the
`inputs="fixed.md"` declaration is ignored altogether (the file need not exist).
The same goes for a variable computed by `fix` (`meshfox:var from="fix"`) that
`convert` takes through `env=`: it is simply absent while the edge is off, so
branch on the flag, not on whether the file or variable happens to exist.

## Repair
<!-- meshfox:node id="repair" -->

In a real canvas `fix` would be `interpreter="@agent"` (add `tty` for an
interactive session) — see the agent recipe in the README. Here it only prints
what it would do, so the example runs anywhere. Its own dependencies pick the
provider's set-up step with a comparison, so only the chosen one runs:

```bash name="claude-setup"
echo "checking that the claude CLI is configured"
```

```bash name="codex-setup"
echo "checking that the codex CLI is configured"
```

```bash name="fix" env="PROVIDER" deps="claude-setup?PROVIDER=claude,codex-setup?PROVIDER=codex"
echo "asking $PROVIDER to repair the input"
```
