<!-- meshfox:canvas -->
# Search Fixture
<!-- meshfox:node id="root" -->
<!-- meshfox:option name="unfold" -->

Fixture canvas for the Playwright search suite (`web/e2e/search.spec.ts`).
`branch-alpha` and `branch-beta` each have one child whose body contains
the search suite's query term — two matches in two separate,
independently foldable subtrees, which is what lets the suite check that
stepping from one match to the other folds the no-longer-needed branch
back instead of leaving every previously-visited branch pinned open.
`unrelated-node` never matches. The query term itself is deliberately not
spelled out anywhere in *this* body text (unlike the rest of this
sentence, which would otherwise itself become an unwanted third match —
search scans every node's title/body, this root included) — see
`leaf-alpha`/`leaf-beta` for the actual string
(`search.spec.ts`'s own `QUERY` constant). Declares `unfold` (see
SPEC.md's "Options") so every test starts from "nothing folded" and each
spec folds `branch-alpha`/`branch-beta` explicitly in its own
`beforeEach`, rather than relying on the client's own folded-by-default
state.

## Branch Alpha
<!-- meshfox:node id="branch-alpha" -->

First branch, folded to start.

### Needle One
<!-- meshfox:node id="leaf-alpha" -->

Contains zzzneedle, the search suite's query term.

## Branch Beta
<!-- meshfox:node id="branch-beta" -->

Second branch, folded to start.

### Needle Two
<!-- meshfox:node id="leaf-beta" -->

Contains zzzneedle too.

## Unrelated Node
<!-- meshfox:node id="unrelated-node" -->

Neither this title nor this body contains the query term.

## Self Folded Node
<!-- meshfox:node id="self-folded-node" -->

Contains zzzowntext, a second, unrelated query term matching only this
node — a direct child of `root`, so no *ancestor* fold ever hides it, only
its own. Covers `revealAndFocus` unfolding the matched node itself, not
just whatever ancestor stands between it and `root` (that half is already
covered by `branch-alpha`/`branch-beta`/`leaf-alpha`/`leaf-beta` above).

## Long Branch
<!-- meshfox:node id="long-branch" -->

### Long Body Node
<!-- meshfox:node id="long-body-node" -->

Tall enough to exceed autolayout.ts's depth-≥ 2 `MAX_HEIGHT_DEEP` cap
(480px), so its own `.mesh-node-body` genuinely overflows and needs an
internal scroll to bring a match this far down into view — covers
`useSearchHighlight`'s scroll-into-view half, not just the highlight
itself (already covered by the nodes above). The match sits with plenty
of filler both before *and* after it, not right at the very end of the
content — `ensureRangeVisible`'s centering math clamps against the real
scroll bounds like any scroll does, so a match too close to the end can
land a few px short of dead-center (still visible, just not exactly
centered) — deliberately not what this fixture is testing.

Line 1 of filler text, nothing interesting here.
Line 2 of filler text, nothing interesting here.
Line 3 of filler text, nothing interesting here.
Line 4 of filler text, nothing interesting here.
Line 5 of filler text, nothing interesting here.
Line 6 of filler text, nothing interesting here.
Line 7 of filler text, nothing interesting here.
Line 8 of filler text, nothing interesting here.
Line 9 of filler text, nothing interesting here.
Line 10 of filler text, nothing interesting here.
Line 11 of filler text, nothing interesting here.
Line 12 of filler text, nothing interesting here.
Line 13 of filler text, nothing interesting here.
Line 14 of filler text, nothing interesting here.
Line 15 of filler text, nothing interesting here.
Line 16 of filler text, nothing interesting here.
Line 17 of filler text, nothing interesting here.
Line 18 of filler text, nothing interesting here.
Line 19 of filler text, nothing interesting here.
Line 20 of filler text, nothing interesting here.
Line 21 of filler text, nothing interesting here.
Line 22 of filler text, nothing interesting here.
Line 23 of filler text, nothing interesting here.
Line 24 of filler text, nothing interesting here.
Line 25 of filler text, nothing interesting here.
Line 26 of filler text, nothing interesting here.
Line 27 of filler text, nothing interesting here.
Line 28 of filler text, nothing interesting here.
Line 29 of filler text, nothing interesting here.
Line 30 of filler text, nothing interesting here.
Line 31 of filler text, nothing interesting here.
Line 32 of filler text, nothing interesting here.
Line 33 of filler text, nothing interesting here.
Line 34 of filler text, nothing interesting here.
Line 35 of filler text, nothing interesting here.
Line 36 of filler text, nothing interesting here.
Line 37 of filler text, nothing interesting here.
Line 38 of filler text, nothing interesting here.
Line 39 of filler text, nothing interesting here.
Line 40 of filler text, nothing interesting here.

Here it is: zzzlongmatch, buried near the bottom of a long body.

Line 41 of filler text, nothing interesting here.
Line 42 of filler text, nothing interesting here.
Line 43 of filler text, nothing interesting here.
Line 44 of filler text, nothing interesting here.
Line 45 of filler text, nothing interesting here.
Line 46 of filler text, nothing interesting here.
Line 47 of filler text, nothing interesting here.
Line 48 of filler text, nothing interesting here.
Line 49 of filler text, nothing interesting here.
Line 50 of filler text, nothing interesting here.
Line 51 of filler text, nothing interesting here.
Line 52 of filler text, nothing interesting here.
Line 53 of filler text, nothing interesting here.
Line 54 of filler text, nothing interesting here.
Line 55 of filler text, nothing interesting here.
Line 56 of filler text, nothing interesting here.
Line 57 of filler text, nothing interesting here.
Line 58 of filler text, nothing interesting here.
Line 59 of filler text, nothing interesting here.
Line 60 of filler text, nothing interesting here.
Line 61 of filler text, nothing interesting here.
Line 62 of filler text, nothing interesting here.
Line 63 of filler text, nothing interesting here.
Line 64 of filler text, nothing interesting here.
Line 65 of filler text, nothing interesting here.
Line 66 of filler text, nothing interesting here.
Line 67 of filler text, nothing interesting here.
Line 68 of filler text, nothing interesting here.
Line 69 of filler text, nothing interesting here.
Line 70 of filler text, nothing interesting here.
Line 71 of filler text, nothing interesting here.
Line 72 of filler text, nothing interesting here.
Line 73 of filler text, nothing interesting here.
Line 74 of filler text, nothing interesting here.
Line 75 of filler text, nothing interesting here.
Line 76 of filler text, nothing interesting here.
Line 77 of filler text, nothing interesting here.
Line 78 of filler text, nothing interesting here.
Line 79 of filler text, nothing interesting here.
Line 80 of filler text, nothing interesting here.
Line 81 of filler text, nothing interesting here.
Line 82 of filler text, nothing interesting here.
Line 83 of filler text, nothing interesting here.
Line 84 of filler text, nothing interesting here.
Line 85 of filler text, nothing interesting here.

## Multi Match Node
<!-- meshfox:node id="multi-match-node" -->

The query appears three times in this one node's own body — covers
`searchMatches` counting/stepping through individual *occurrences* within
a single node, not just once per matching node (see App.tsx's own doc
comment on `searchMatches`). Stepping between them never touches fold
state at all (nothing here is ever folded to begin with), only which
occurrence `useSearchHighlight` treats as current.

Filler paragraph 1 with zzzmulti buried inside it somewhere.

Filler paragraph 2 with zzzmulti buried inside it somewhere.

Filler paragraph 3 with zzzmulti buried inside it somewhere.


## Wide Output Node
<!-- meshfox:node id="wide-output-node" -->

A cached run output containing one very long, unwrapped (`white-space:
pre`) line, with the query term positioned far enough right that it's
scrolled out of `<pre>`'s own `overflow: auto` viewport by default —
covers `ensureRangeVisible`'s *horizontal* ancestor-scroll adjustment
(`scrollLeft`), not just the vertical one `long-body-node` above already
covers. `getBoundingClientRect()` reports a range's geometric position
regardless of what clips it away, so skipping the horizontal half doesn't
just leave the match unscrolled — it poisons the window-visibility check
and canvas-pan target with an x coordinate that was never the match's
real on-screen position (confirmed directly against a real canvas: this
exact gap landed the camera on empty space).

```bash name="wide-output" cache
printf '%s'
```
<!-- meshfox:output name="wide-output" hash="0" -->
```text
exit code: 0 · 0ms

prefix xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx zzzwidematch xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx suffix
```
<!-- /meshfox:output -->
