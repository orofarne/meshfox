<!-- meshfox:canvas -->
# Quick Run Fixture
<!-- meshfox:node id="root" -->
<!-- meshfox:option name="unfold" -->

Fixture canvas for the Playwright quick-run suite
(`web/e2e/quick-run.spec.ts`) — checks the title bar's "▷ run" quick-run
button (see MeshNode.tsx) routes a `tty`-flagged default block through
`onRunTty` (opens a real terminal), not `onRun` (which the server
rejects outright for a `tty` block — see `crates/server/src/lib.rs`'s
`run_block`). Declares `unfold` so both nodes below render fully
expanded, matching this suite's own visibility assumptions.

## Monitor
<!-- meshfox:node id="monitor" -->

Its one fence is unnamed, so it implicitly takes this node's own id
(`monitor`) as its block name — the same "sole unnamed fence" rule that
makes it this node's default block (see `core::fence::default_block`),
which is what the title bar's quick-run button targets.

```bash tty
sh
```

## Plain
<!-- meshfox:node id="plain" -->

A node whose default block is an ordinary (non-`tty`) fence, so the
quick-run button here should still go through the plain `onRun` path —
this is the suite's control case, checking the fix didn't flip *every*
quick-run to go through `onRunTty` regardless of the block's own flag.

```bash
echo hello
```
