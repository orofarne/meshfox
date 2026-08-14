<!-- meshfox:canvas -->
# Fold Fixture
<!-- meshfox:node id="root" -->
<!-- meshfox:option name="unfold" -->

Fixture canvas for the Playwright fold/unfold suite (`web/e2e/fold.spec.ts`).
`parent-node` has two children (`child-one`, `child-two`); `sibling-node`
follows it with no body of its own. None of the three have an authored
`x`/`y`, so the client's own auto-layout places them — this is what lets
folding `parent-node` visibly move `sibling-node` up to fill the gap its
hidden children used to occupy. Declares `unfold` (see SPEC.md's
"Options") so every test here starts from "nothing folded" and drives the
fold toggle explicitly, rather than starting from the client's own
folded-by-default state.

## Parent Node
<!-- meshfox:node id="parent-node" -->

A node with children — folding this hides both of them and shrinks this
node itself to a title-only row.

### Child One
<!-- meshfox:node id="child-one" -->

First child, hidden while `parent-node` is folded.

### Child Two
<!-- meshfox:node id="child-two" -->

Second child, hidden while `parent-node` is folded.

## Sibling Node
<!-- meshfox:node id="sibling-node" -->

Auto-placed sibling of Parent Node — used to check that folding Parent
Node lets this node reflow upward into the space it vacates.
