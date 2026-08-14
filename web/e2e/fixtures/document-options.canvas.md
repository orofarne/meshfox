<!-- meshfox:canvas -->
# Document Options Fixture
<!-- meshfox:node id="root" -->
<!-- meshfox:option name="custom-future-option" -->

Fixture canvas for the Playwright document-options suite
(`web/e2e/document-options.spec.ts`), which drives the toolbar's "options"
modal (`DocumentOptions.tsx`). Declares one option this version of
meshfox doesn't recognize (`custom-future-option`), to check the modal
shows it read-only and carries it through untouched, alongside the
`unfold` checkbox it does recognize. Doesn't declare `unfold` itself —
the suite adds/removes it through the UI.

One of this suite's tests actually saves through the modal, genuinely
rewriting this file's `meshfox:option` lines in place — harmless in CI (a
fresh checkout every time); `git checkout` it back afterward if running
locally leaves it modified and that matters to you.

## Child
<!-- meshfox:node id="child" -->

A child node — not exercised directly by this suite (App.tsx's
fold-restore effect only ever applies a document's declared default the
very first time a given root id is seen with nothing in localStorage yet,
so `unfold`'s effect on fold state can't be observed live within one
already-loaded session; see fold.spec.ts/keyboard-nav.spec.ts for that
coverage instead). Kept only so the fixture isn't a single bare root.
