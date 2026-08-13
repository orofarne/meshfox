<!-- meshfox:canvas -->
# Included Canvas
<!-- meshfox:node id="included-root" -->

Canvas include target for `web/e2e/settings.spec.ts` — has its own
`meshfox:canvas` marker, so `include::resolve` splices this in as real
namespaced child nodes and turns the including node into a `group`.

## Included Child
<!-- meshfox:node id="included-child" -->

A child of the include target's own root, spliced in under it.
