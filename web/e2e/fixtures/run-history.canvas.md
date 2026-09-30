<!-- meshfox:canvas -->
# Run History Fixture
<!-- meshfox:node id="root" -->
<!-- meshfox:option name="unfold" -->

Fixture canvas for the Playwright run-history suite
(`web/e2e/run-history.spec.ts`) — checks the block header's ◷ button lists a
block's earlier runs (kept by the server's `run_ledger`), shows a run's
stored output on click, and marks every run stale once the block's own code
changes; a `tty` block (`shell`, exits 3) gets a status badge and a history
entry too, without stored output. Declares `unfold` so every node renders fully expanded.

## Greet
<!-- meshfox:node id="greet" -->

```bash name="greet"
echo first-run
```

## Shell
<!-- meshfox:node id="shell" -->

```bash name="shell" tty
echo from-tty; exit 3
```

## Watched
<!-- meshfox:node id="watched" -->

```bash name="watched" tty
echo watched-tty; exit 4
```

## Dependency
<!-- meshfox:node id="dep-source" -->

```bash name="setup"
echo dep-ran-elsewhere
```

## Chained
<!-- meshfox:node id="chained" -->

```bash name="chained" deps="dep-source/setup"
echo chained-ran
```
