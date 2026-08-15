<!-- meshfox:canvas -->
# Root
<!-- meshfox:node id="root" -->
<!-- meshfox:option name="unfold" -->

Fixture canvas for the Playwright edge-routing suite
(`web/e2e/edge-routing.spec.ts`), which checks structural (plain
parent→child) edges keep attaching to each node's original Left/Right
"default" handles — not one of MeshNode.tsx's newer routing-only
top/bottom handles meant only for a `meshfox:edge` extra edge stacked
mostly vertically with its other endpoint. Declares `unfold` (see
SPEC.md's "Options") so every node below renders at its normal
auto-laid-out size rather than starting folded to a compact title-only
row, matching this suite's own "does the rendered edge look right"
assertions.

`root` (level 1) is `child-a`/`child-b`'s source — its own source handle
sits on the Left (see MeshNode.tsx's own doc comment: a root-level
node's children sit almost directly below it, so exiting left gives a
near-straight path down instead of an ugly backward loop). `child-a`
(level 2) is `grandchild`'s source instead, from the Right — every
level below root indents its children rightward, where exiting right is
the natural direction. Between them, `root->child-a`/`root->child-b`
and `child-a->grandchild` cover both of `MeshNode`'s source-handle
branches.

## Child A
<!-- meshfox:node id="child-a" -->

### Grandchild
<!-- meshfox:node id="grandchild" -->

## Child B
<!-- meshfox:node id="child-b" -->
