<!-- meshfox:canvas -->
# Search Pan Fixture
<!-- meshfox:node id="root" -->
<!-- meshfox:option name="unfold" -->

Fixture canvas for the Playwright search-pan suite
(`web/e2e/search-pan.spec.ts`) — isolated from search.canvas.md/
search.spec.ts because `huge-node`'s own height (deliberately well past
the browser viewport, to exercise `ensureRangeVisible`'s canvas-pan
fallback — see MeshNode.tsx) also wrecks `clickFitViewAndWait`'s
fit-to-everything zoom for every *other* node sharing a canvas with it —
confirmed directly when it lived alongside search.canvas.md's own nodes:
it zoomed the whole rest of that canvas down to a sliver under the
toolbar and broke every test there, not just its own. This suite never
calls fitView at all — its whole point is the app's own *default* initial
view (centered on root at a fixed zoom, not fitted to content — see
playwright.config.ts's own `VIEWPORT` comment), which already leaves
`huge-node`'s own matched text well outside the visible viewport to begin
with.

## Huge Node
<!-- meshfox:node id="huge-node" -->

A direct child of `root` (depth 1) — autolayout.ts's `max-height` cap
only ever applies at depth ≥2, so this node has no internal scrollbar at
all regardless of how tall its content grows; there's nothing here for
`.mesh-node-body`'s own `overflow: auto` to actually do. The only way to
bring a match buried this far down into view is panning the canvas
itself.

Line 1 of filler text in an uncapped depth-1 node, nothing interesting here.
Line 2 of filler text in an uncapped depth-1 node, nothing interesting here.
Line 3 of filler text in an uncapped depth-1 node, nothing interesting here.
Line 4 of filler text in an uncapped depth-1 node, nothing interesting here.
Line 5 of filler text in an uncapped depth-1 node, nothing interesting here.
Line 6 of filler text in an uncapped depth-1 node, nothing interesting here.
Line 7 of filler text in an uncapped depth-1 node, nothing interesting here.
Line 8 of filler text in an uncapped depth-1 node, nothing interesting here.
Line 9 of filler text in an uncapped depth-1 node, nothing interesting here.
Line 10 of filler text in an uncapped depth-1 node, nothing interesting here.
Line 11 of filler text in an uncapped depth-1 node, nothing interesting here.
Line 12 of filler text in an uncapped depth-1 node, nothing interesting here.
Line 13 of filler text in an uncapped depth-1 node, nothing interesting here.
Line 14 of filler text in an uncapped depth-1 node, nothing interesting here.
Line 15 of filler text in an uncapped depth-1 node, nothing interesting here.
Line 16 of filler text in an uncapped depth-1 node, nothing interesting here.
Line 17 of filler text in an uncapped depth-1 node, nothing interesting here.
Line 18 of filler text in an uncapped depth-1 node, nothing interesting here.
Line 19 of filler text in an uncapped depth-1 node, nothing interesting here.
Line 20 of filler text in an uncapped depth-1 node, nothing interesting here.
Line 21 of filler text in an uncapped depth-1 node, nothing interesting here.
Line 22 of filler text in an uncapped depth-1 node, nothing interesting here.
Line 23 of filler text in an uncapped depth-1 node, nothing interesting here.
Line 24 of filler text in an uncapped depth-1 node, nothing interesting here.
Line 25 of filler text in an uncapped depth-1 node, nothing interesting here.
Line 26 of filler text in an uncapped depth-1 node, nothing interesting here.
Line 27 of filler text in an uncapped depth-1 node, nothing interesting here.
Line 28 of filler text in an uncapped depth-1 node, nothing interesting here.
Line 29 of filler text in an uncapped depth-1 node, nothing interesting here.
Line 30 of filler text in an uncapped depth-1 node, nothing interesting here.
Line 31 of filler text in an uncapped depth-1 node, nothing interesting here.
Line 32 of filler text in an uncapped depth-1 node, nothing interesting here.
Line 33 of filler text in an uncapped depth-1 node, nothing interesting here.
Line 34 of filler text in an uncapped depth-1 node, nothing interesting here.
Line 35 of filler text in an uncapped depth-1 node, nothing interesting here.
Line 36 of filler text in an uncapped depth-1 node, nothing interesting here.
Line 37 of filler text in an uncapped depth-1 node, nothing interesting here.
Line 38 of filler text in an uncapped depth-1 node, nothing interesting here.
Line 39 of filler text in an uncapped depth-1 node, nothing interesting here.
Line 40 of filler text in an uncapped depth-1 node, nothing interesting here.
Line 41 of filler text in an uncapped depth-1 node, nothing interesting here.
Line 42 of filler text in an uncapped depth-1 node, nothing interesting here.
Line 43 of filler text in an uncapped depth-1 node, nothing interesting here.
Line 44 of filler text in an uncapped depth-1 node, nothing interesting here.
Line 45 of filler text in an uncapped depth-1 node, nothing interesting here.
Line 46 of filler text in an uncapped depth-1 node, nothing interesting here.
Line 47 of filler text in an uncapped depth-1 node, nothing interesting here.
Line 48 of filler text in an uncapped depth-1 node, nothing interesting here.
Line 49 of filler text in an uncapped depth-1 node, nothing interesting here.
Line 50 of filler text in an uncapped depth-1 node, nothing interesting here.
Line 51 of filler text in an uncapped depth-1 node, nothing interesting here.
Line 52 of filler text in an uncapped depth-1 node, nothing interesting here.
Line 53 of filler text in an uncapped depth-1 node, nothing interesting here.
Line 54 of filler text in an uncapped depth-1 node, nothing interesting here.
Line 55 of filler text in an uncapped depth-1 node, nothing interesting here.
Line 56 of filler text in an uncapped depth-1 node, nothing interesting here.
Line 57 of filler text in an uncapped depth-1 node, nothing interesting here.
Line 58 of filler text in an uncapped depth-1 node, nothing interesting here.
Line 59 of filler text in an uncapped depth-1 node, nothing interesting here.
Line 60 of filler text in an uncapped depth-1 node, nothing interesting here.
Line 61 of filler text in an uncapped depth-1 node, nothing interesting here.
Line 62 of filler text in an uncapped depth-1 node, nothing interesting here.
Line 63 of filler text in an uncapped depth-1 node, nothing interesting here.
Line 64 of filler text in an uncapped depth-1 node, nothing interesting here.
Line 65 of filler text in an uncapped depth-1 node, nothing interesting here.
Line 66 of filler text in an uncapped depth-1 node, nothing interesting here.
Line 67 of filler text in an uncapped depth-1 node, nothing interesting here.
Line 68 of filler text in an uncapped depth-1 node, nothing interesting here.
Line 69 of filler text in an uncapped depth-1 node, nothing interesting here.
Line 70 of filler text in an uncapped depth-1 node, nothing interesting here.
Line 71 of filler text in an uncapped depth-1 node, nothing interesting here.
Line 72 of filler text in an uncapped depth-1 node, nothing interesting here.
Line 73 of filler text in an uncapped depth-1 node, nothing interesting here.
Line 74 of filler text in an uncapped depth-1 node, nothing interesting here.
Line 75 of filler text in an uncapped depth-1 node, nothing interesting here.
Line 76 of filler text in an uncapped depth-1 node, nothing interesting here.
Line 77 of filler text in an uncapped depth-1 node, nothing interesting here.
Line 78 of filler text in an uncapped depth-1 node, nothing interesting here.
Line 79 of filler text in an uncapped depth-1 node, nothing interesting here.
Line 80 of filler text in an uncapped depth-1 node, nothing interesting here.
Line 81 of filler text in an uncapped depth-1 node, nothing interesting here.
Line 82 of filler text in an uncapped depth-1 node, nothing interesting here.
Line 83 of filler text in an uncapped depth-1 node, nothing interesting here.
Line 84 of filler text in an uncapped depth-1 node, nothing interesting here.
Line 85 of filler text in an uncapped depth-1 node, nothing interesting here.
Line 86 of filler text in an uncapped depth-1 node, nothing interesting here.
Line 87 of filler text in an uncapped depth-1 node, nothing interesting here.
Line 88 of filler text in an uncapped depth-1 node, nothing interesting here.
Line 89 of filler text in an uncapped depth-1 node, nothing interesting here.
Line 90 of filler text in an uncapped depth-1 node, nothing interesting here.

Here it is: zzzhugematch, buried near the bottom of a very tall node.
