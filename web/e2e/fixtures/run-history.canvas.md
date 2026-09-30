<!-- meshfox:canvas -->
# Run History Fixture
<!-- meshfox:node id="root" -->
<!-- meshfox:option name="unfold" -->

Fixture canvas for the Playwright run-history suite
(`web/e2e/run-history.spec.ts`) — checks the block header's ◷ button lists a
block's earlier runs (kept by the server's `run_ledger`), shows a run's
stored output on click, and marks every run stale once the block's own code
changes. Declares `unfold` so every node renders fully expanded.

## Greet
<!-- meshfox:node id="greet" -->

```bash name="greet"
echo first-run
```
