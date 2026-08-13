<!-- meshfox:canvas -->
# Settings Fixture
<!-- meshfox:node id="root" -->

Fixture canvas for the Playwright node-settings suite (`web/e2e/settings.spec.ts`)
— one node per `NodeSettings`-relevant type/field combination. Every test there
opens a node's settings modal and clicks "ok" without changing a single field,
then asserts the raw file (`GET /api/canvas/raw`) is byte-for-byte unchanged —
a no-op "ok" must never rewrite the file, whatever the node's type or which of
its optional fields happen to be set.

## Plain Text
<!-- meshfox:node id="text-plain" -->

A bare text node: default type, no color, no tags, no extra edges — the
simplest possible settings combination.

## Styled Text
<!-- meshfox:node id="text-styled" color="3" tags="alpha, beta" -->
<!-- meshfox:edge from="text-plain" -->

A text node with every optional field `NodeSettings` can set on a plain node
at once: a color preset, tags, and an extra incoming edge.

## File (link display)
<!-- meshfox:node id="file-link" type="file" -->

[settings-file-target.txt](./settings-file-target.txt)

## File (code preview)
<!-- meshfox:node id="file-code" type="file" display="code" lang="text" interpreter="cat" -->

[settings-file-target.txt](./settings-file-target.txt)

## Link
<!-- meshfox:node id="link-node" type="link" -->

[example](https://example.com)

## Include (plain Markdown target)
<!-- meshfox:node id="include-text" type="include" -->

[included](./settings-include-target.md)

## Include (canvas target)
<!-- meshfox:node id="include-canvas" type="include" -->

[included](./settings-include-target.canvas.md)

## Group
<!-- meshfox:node id="group-node" type="group" -->

### Group Child
<!-- meshfox:node id="group-child" -->

A child of the group above — a `group` node's own body must stay empty;
this exists just so the group has a real member and a real derived box.
