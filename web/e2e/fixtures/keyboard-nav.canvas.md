<!-- meshfox:canvas -->
# Keyboard Nav Fixture
<!-- meshfox:node id="root" -->
<!-- meshfox:option name="unfold" -->

Fixture canvas for the Playwright keyboard-navigation suite
(`web/e2e/keyboard-nav.spec.ts`). `section-one` has two children
(`child-a`, `child-b`); `section-two` is a plain sibling that follows it.
None of the four have an authored `x`/`y`, so document order (the order
j/k should walk) is root, section-one, child-a, child-b, section-two.
Declares `unfold` (see SPEC.md's "Options") so every test here starts
from "nothing folded" and drives fold state explicitly via h/l/Enter,
rather than starting from the client's own folded-by-default state.
`title-only-leaf`/`title-only-parent`/`title-only-child` (appended last,
after every test above's own order assumptions) cover h/Enter's own
foldability guard (see App.tsx's `canFold`) for an empty-bodied node —
one with no children (`title-only-leaf`, never foldable) and one with
(`title-only-parent`, foldable purely for `title-only-child`'s sake).

## Section One
<!-- meshfox:node id="section-one" -->

Has two children below it — the h/l/Enter fold tests target this node.

### Child A
<!-- meshfox:node id="child-a" -->

First child.

### Child B
<!-- meshfox:node id="child-b" -->

Second child.

## Section Two
<!-- meshfox:node id="section-two" -->

A plain sibling with a body of its own, so Edit mode's "✏" button has
something to open for the key-suppression guard test.

## Title Only Leaf
<!-- meshfox:node id="title-only-leaf" -->

## Title Only Parent
<!-- meshfox:node id="title-only-parent" -->

### Title Only Child
<!-- meshfox:node id="title-only-child" -->

Child of an empty-bodied node — its parent's own row never changes when
folded (nothing was ever shown there), but this one still gets hidden.
