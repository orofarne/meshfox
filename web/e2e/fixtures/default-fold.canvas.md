<!-- meshfox:canvas -->
# Default Fold Fixture
<!-- meshfox:node id="root" -->

Fixture canvas for the Playwright default-fold suite
(`web/e2e/default-fold.spec.ts`) — deliberately does *not* declare the
`unfold` option (unlike this suite's siblings, e.g. `fold.canvas.md`), so
App.tsx's own document-level default (`resolveDefaultFold`) applies
exactly as authored, not neutralized for the test's own convenience.

## Plain Leaf
<!-- meshfox:node id="plain-leaf" -->

A node with a real body and no children of its own — folds by default,
same as any other non-root, non-title-only, unsized node (see
`resolveDefaultFold`'s own doc comment).

## Title Only
<!-- meshfox:node id="title-only" -->

## Sized Node
<!-- meshfox:node id="sized-node" w=320 h=160 -->

An authored `w`/`h` — should *not* fold by default even though it
otherwise would (a plain leaf, same shape as "Plain Leaf" above): an
explicit size is a deliberate "show this much of it" the file already
made, which folding it away on open would silently override.

## Title Only With Child
<!-- meshfox:node id="title-only-with-child" -->

### Nested Under Title Only
<!-- meshfox:node id="nested-under-title-only" -->

Unlike "Title Only" above, its parent has a child of its own — folding
the parent doesn't change *its own* row (still just this same compact
title, nothing was ever shown there) but does hide this node, so the
parent still gets a fold toggle and still folds by default.
