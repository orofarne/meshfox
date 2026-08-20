<!-- meshfox:canvas -->
# E2E Fixture
<!-- meshfox:node id="root" -->

Fixture canvas for the Playwright e2e suite (`web/e2e/`). Deliberately
deterministic — no `date`/timestamps — and separate from
`examples/hello.canvas.md`, so test stability never depends on the
documentation example's own content. Covers: a plain block, a same-node
`deps` chain, a cross-node `deps` chain, an uncached block inside a chain,
a block with multiple `deps`, a slow block for exercising live streaming
and Kill, and an implicitly-named (no `name=`) lone block.

Every block that's ever a pulled-in *dependency* (not the directly-clicked
target — that one always runs for real, see `run_block`'s own doc comment)
of a chain whose assertions here depend on its literal "just ran" output
(`"building"`, `"tick 1"`, ...) is flagged `always`. Without it, the
in-memory "already ran this session, unchanged" skip (see
`AppState::session_runs`) kicks in the *second* time a chain pulls in that
same unchanged block — which happens constantly here: every `<browser>-deps`
project (`chrome-deps`, `firefox-deps`, ...) shares one `meshfox view`
server/session for this whole file (`deps` isn't one of the projects with
its own dedicated port in `playwright.config.ts`), so the file effectively
runs once per browser against the same session, on top of more than one
test in this file also pulling in the same block. `always` is exactly the
escape hatch `crates/core/src/fence.rs` documents for this: a step whose
side effect isn't captured by "looks unchanged".

## Build Node
<!-- meshfox:node id="build-node" -->

```bash name="build" cache always
echo "building"
```

```bash name="test" cache always deps="build"
echo "testing"
```

## Deploy Node
<!-- meshfox:node id="deploy-node" -->

```bash name="deploy" cache always deps="build-node/test"
echo "deploying"
```

```bash name="verify" deps="deploy"
echo "verifying"
```

## Release Node
<!-- meshfox:node id="release-node" -->

```bash name="release" cache deps="build-node/test,deploy-node/deploy"
echo "releasing"
```

## Slow Node
<!-- meshfox:node id="slow-node" -->

`slow` ticks once a second so a test can observe output arriving live
rather than all at once, and has enough headroom to click Kill mid-run.
`after-slow` depends on it, so a test can confirm a kill stops the rest of
the chain too.

```bash name="slow" always
for i in 1 2 3 4 5; do echo "tick $i"; sleep 1; done
echo "finished"
```

```bash name="after-slow" deps="slow"
echo "after-slow ran"
```

## Implicit Node
<!-- meshfox:node id="implicit-node" -->

Its sole fence has no `name=` at all — implicitly runnable as
`implicit-node`, same as if it had `name="implicit-node"` explicitly.

```bash cache
echo "implicit block ran"
```
