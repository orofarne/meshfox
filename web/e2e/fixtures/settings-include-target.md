# Included Doc

Plain-Markdown include target for `web/e2e/settings.spec.ts` — no
`meshfox:canvas` marker, so `include::resolve` splices this in as this
include node's own body (`text` type) rather than as real child nodes.
