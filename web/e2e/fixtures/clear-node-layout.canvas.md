<!-- meshfox:canvas -->
# Clear Node Layout Fixture
<!-- meshfox:node id="root" -->
<!-- meshfox:option name="unfold" -->

Fixture canvas for the Playwright ↺ "reset to auto-layout" suite
(`web/e2e/clear-node-layout.spec.ts` — TODO.canvas.md: "Способ удалить
координаты для конкретной ноды"). `positioned` carries a real, authored
`x`/`y`/`w`/`h` plus a color/tag (to confirm clearing the position leaves
everything else untouched); `auto` has none, so it never gets the ↺
button in the first place. Declares `unfold` so every test starts from
"nothing folded".

## Positioned
<!-- meshfox:node id="positioned" x=200 y=100 w=240 h=120 color="2" tags="keep-me" -->

Has a real, authored position and size.

## Auto
<!-- meshfox:node id="auto" -->

Auto-placed — no position to clear, so no ↺ button.
