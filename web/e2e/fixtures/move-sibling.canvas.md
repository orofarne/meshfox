<!-- meshfox:canvas -->
# Move Sibling Fixture
<!-- meshfox:node id="root" -->
<!-- meshfox:option name="unfold" -->

Fixture canvas for the Playwright `↑`/`↓` sibling-reorder suite
(`web/e2e/move-sibling.spec.ts`). `alpha`/`beta`/`gamma` have no authored
`x`/`y` (auto-placed — the only case that gets ↑/↓ buttons at all), with
`positioned` (a real `x`/`y`, no buttons of its own) sandwiched in the
middle to exercise moving relative to a positioned neighbor too. Declares
`unfold` (see SPEC.md's "Options") so every test here starts from
"nothing folded".

## Alpha
<!-- meshfox:node id="alpha" -->

First auto-placed child — the topmost sibling overall, so it never gets
an up-button.

## Positioned
<!-- meshfox:node id="positioned" x=700 y=0 w=200 h=100 -->

Has a real position — never shows ↑/↓ buttons itself, but still a valid
target for an auto-placed neighbor's own move.

## Beta
<!-- meshfox:node id="beta" -->

Second auto-placed child.

## Gamma
<!-- meshfox:node id="gamma" -->

Last auto-placed child — the bottommost sibling overall, so it never gets
a down-button.
