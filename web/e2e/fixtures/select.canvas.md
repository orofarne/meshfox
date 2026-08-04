<!-- meshfox:canvas -->
# Select Fixture
<!-- meshfox:node id="root" -->

Fixture canvas for the Playwright text-selection suite
(`web/e2e/select.spec.ts`). Two children: one with a title and a body (the
normal title layout, `.mesh-node-title` + `.mesh-node-title-text`), one
with a title and no body at all (the centered, title-only layout,
`.mesh-node-title-centered` — see MeshNode.tsx's `isTitleOnly`). Both are
read-only, default (non-Edit) nodes, matching how a user actually views a
canvas day to day.

## Title And Body Node
<!-- meshfox:node id="title-body-node" -->

Some body text that should also remain selectable, matching the node
body's own drag-to-select fix from before.

## Title Only Node
<!-- meshfox:node id="title-only-node" -->
