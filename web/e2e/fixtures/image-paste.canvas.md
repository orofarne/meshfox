<!-- meshfox:canvas -->
# Image Paste Fixture
<!-- meshfox:node id="root" -->
<!-- meshfox:option name="unfold" -->

Fixture canvas for the Playwright base64-image-paste suite
(`web/e2e/image-paste.spec.ts` — TODO.canvas.md: "Base64 image"). One bare
root is enough: the paste extension is shared between the node body editor
(NodeTextEditor) and the whole-document source editor (CanvasSourceEditor),
and this fixture's root is a target for both.
