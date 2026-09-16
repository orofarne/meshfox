<!-- meshfox:canvas -->
# Drag Reorder Fixture
<!-- meshfox:node id="root" -->
<!-- meshfox:option name="unfold" -->

Fixture canvas for the Playwright "drag a node among untouched siblings"
suite (`web/e2e/drag-reorder.spec.ts`, firefox project) — `PutCanvasRequest::layout_hints`'s
own regression coverage. `alpha`/`beta`/`gamma` are all auto-placed (no
`x`/`y`), stacked top-to-bottom in that document order — the only case
`layout_hints` matters for: dragging one of them to a real position should
slot it in among the *other two* by where it visually lands, not always
sort it first (any real number used to beat every unpositioned sibling's
implicit `f64::INFINITY`). Declares `unfold` so every test starts from
"nothing folded".

## Alpha
<!-- meshfox:node id="alpha" -->

First auto-placed child.

## Beta
<!-- meshfox:node id="beta" -->

Second auto-placed child.

## Gamma
<!-- meshfox:node id="gamma" -->

Third auto-placed child.
