<!-- meshfox:canvas -->
# Clear Node Layout Fixture
<!-- meshfox:node id="root" -->
<!-- meshfox:option name="unfold" -->

Fixture canvas for the Playwright ↺ "reset to auto-layout" suite
(`web/e2e/clear-node-layout.spec.ts` — TODO.canvas.md: "Способ удалить
координаты для конкретной ноды"). `positioned` carries a real, authored
`x`/`y`/`w`/`h` plus a color/tag (to confirm clearing the position leaves
everything else untouched); `auto` has none, so it never gets the ↺
button in the first place; `sized-only` carries a real `w`/`h` but no
`x`/`y` at all (dragging only a `NodeResizer` corner handle never touches
position) — the ↺ button (and the filled-corner indicator) key off *any*
of the four being authored, not position specifically, so this still
counts as "has something to reset". Declares `unfold` so every test
starts from "nothing folded".

## Positioned
<!-- meshfox:node id="positioned" x=200 y=100 w=240 h=120 color="2" tags="keep-me" -->

Has a real, authored position and size.

## Auto
<!-- meshfox:node id="auto" -->

Auto-placed — no position to clear, so no ↺ button.

## Sized Only
<!-- meshfox:node id="sized-only" w=180 h=90 -->

Has a real, authored width/height, but no x/y at all.
