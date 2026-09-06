<!-- meshfox:canvas -->
# root
<!-- meshfox:node id="root" -->
<!-- meshfox:option name="unfold" -->

Fixture canvas for `editors/vscode/e2e/paste.spec.ts` — one bare root is
enough: every test targets this same node's own body editor, title field,
or Settings modal. `helpers.ts` copies this file into a fresh temp
workspace per test (never opens it in place), same reasoning
`web/playwright.config.ts` documents for its own fixtures — several tests
here genuinely auto-save into the node body (`NodeTextEditor`'s own
auto-save on close), and a checked-in fixture would otherwise pick up real
drift on every local run.
