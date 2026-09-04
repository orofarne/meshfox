<!-- meshfox:canvas -->
# Autofit Title Fixture
<!-- meshfox:node id="root" -->
<!-- meshfox:option name="unfold" -->

Fixture canvas for the Playwright autofit-title suite
(`web/e2e/autofit-title.spec.ts`) — a header-only node's title shrinking
to fit its own box (`MeshNode.tsx`'s `useAutoFitTitleFontSize`,
TODO.canvas.md: "Автоскейл текста, если у ноды есть фиксированный
размер"), but only when the node has an explicit, authored `w`/`h` — an
autolayout-sized header-only node just grows to fit its title instead,
same as before this existed.

## Short
<!-- meshfox:node id="fixed-short-title" x=52 y=256 w=220 h=120 -->

### New Node
<!-- meshfox:node id="new-node" x=352 y=256 w=228 h=120 -->

Test
Test
Test

## This title is long enough that it would overflow a small fixed box at the default font size
<!-- meshfox:node id="fixed-long-title" x=80 y=400 w=160 h=132 -->

## This title is long enough that it would overflow a small fixed box at the default font size, but this node has no authored size so its box just grows instead
<!-- meshfox:node id="auto-long-title" x=232 y=568 w=136 h=100 -->
