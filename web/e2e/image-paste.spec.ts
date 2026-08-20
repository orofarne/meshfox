import { test, expect, type Locator, type Page } from "@playwright/test";

// Drives web/e2e/fixtures/image-paste.canvas.md (TODO.canvas.md: "Base64
// image") — pasting an image into either of the two CodeMirror editors
// that share `imagePaste.ts` (the node body editor, NodeTextEditor, and
// the whole-document source editor, CanvasSourceEditor) embeds it as
// `![](data:image/...;base64,...)` at the cursor. Also covers the flip
// side of the fix this feature needed: `MeshNode.tsx`'s `urlTransform`
// must actually let a `data:image/` `src` through (react-markdown's own
// default silently blanks any scheme outside `http(s)/irc(s)/mailto/xmpp`)
// — NodeTextEditor's live preview pane (`NodeBodyPreview`) uses the exact
// same rendering path the canvas view does, so checking it there is
// checking the real fix, not a mock of it.

// A 1x1 transparent PNG — same fixture bytes `crates/cli/src/tui/app.rs`'s
// own `decode_data_url_image` tests use, kept in sync deliberately so a
// screenshot of "does meshfox handle a real minimal PNG" is answered
// identically on both sides.
const ONE_PIXEL_PNG_BASE64 =
  "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=";

// Every test in this file is Firefox-skipped (see each test's own
// `test.skip`) — confirmed directly (a standalone script dispatching a
// synthetic `paste` `ClipboardEvent` built exactly like `pasteImage` below,
// against a bare page, both browsers): Chromium's listener sees
// `event.clipboardData.items` populated with the file just added via
// `dt.items.add(file)`; Firefox's sees an empty `items` list every time,
// even though the `DataTransfer` itself has the file right up until the
// event is dispatched (`dt.files.length === 1` beforehand). Gecko drops a
// synthetic (`isTrusted: false`) `ClipboardEvent`'s file data rather than
// exposing it to script — a security restriction, not a bug in this app:
// `imagePaste.ts`'s handler correctly finds no image item and bails out
// (`return false`) before ever touching the editor, so nothing here is
// otherwise reachable through Firefox's automation surface. A real,
// OS-triggered (trusted) paste in actual Firefox isn't affected by this at
// all — only the synthetic construction this suite relies on for
// automation is.

/** Builds a `File`/`DataTransfer` for `base64` (assumed `image/png`) in
 * the page's own context and dispatches a real `paste` `ClipboardEvent` at
 * `locator` — `page.evaluate` rather than Playwright's own clipboard
 * permissions API (`context.grantPermissions(["clipboard-read"])` +
 * `navigator.clipboard`), since the async Clipboard API needs an actual
 * OS-level clipboard write first; constructing the event directly is both
 * simpler and exactly what a real "paste an image" keystroke ultimately
 * dispatches at the DOM either way. Only actually exercises the app's
 * handler on Chromium — see this file's own top comment. */
async function pasteImage(locator: Locator, base64: string) {
  await locator.click();
  await locator.evaluate((el, base64) => {
    const byteChars = atob(base64);
    const bytes = new Uint8Array(byteChars.length);
    for (let i = 0; i < byteChars.length; i++) bytes[i] = byteChars.charCodeAt(i);
    const file = new File([bytes], "pasted.png", { type: "image/png" });
    const dt = new DataTransfer();
    dt.items.add(file);
    const event = new ClipboardEvent("paste", { bubbles: true, cancelable: true, clipboardData: dt });
    el.dispatchEvent(event);
  }, base64);
}

function fetchRaw(page: Page): Promise<string> {
  return page.evaluate(() => fetch("/api/canvas/raw").then((r) => r.text()));
}

test.beforeEach(async ({ page }) => {
  await page.goto("/");
  await page.waitForSelector(".mesh-node");
  await page.getByRole("button", { name: "Edit" }).click();
});

test("pasting an image into a node's body editor embeds it as base64 and renders it live", async ({
  page,
  browserName,
}) => {
  test.skip(browserName === "firefox", "Gecko drops synthetic paste-event file data — see this file's own top comment.");

  const root = page.locator('.react-flow__node[data-id="root"]');
  await root.locator(".mesh-node-title").hover();
  await root.locator('button[title="Edit this node\'s Markdown text"]').click();

  const source = page.locator(".mesh-text-editor-source .cm-content");
  await expect(source).toBeVisible();
  await pasteImage(source, ONE_PIXEL_PNG_BASE64);

  await expect(source).toContainText(`data:image/png;base64,${ONE_PIXEL_PNG_BASE64}`);
  // The live preview pane renders through the exact same `NodeBodyPreview`/
  // `urlTransform` path the canvas view does — a broken `urlTransform`
  // would blank the `src` here, not just fail to insert the text above.
  await expect(
    page.locator(`.mesh-text-editor-preview img[src="data:image/png;base64,${ONE_PIXEL_PNG_BASE64}"]`),
  ).toBeVisible();

  await page.locator(".mesh-text-editor-actions button", { hasText: "done" }).click();
  await expect.poll(() => fetchRaw(page)).toContain(`data:image/png;base64,${ONE_PIXEL_PNG_BASE64}`);
});

test("pasting an image into the whole-document source editor embeds it as base64", async ({
  page,
  browserName,
}) => {
  test.skip(browserName === "firefox", "Gecko drops synthetic paste-event file data — see this file's own top comment.");

  await page.getByRole("button", { name: "Source" }).click();
  const source = page.locator(".mesh-source-editor-body .cm-content");
  await expect(source).toBeVisible();
  await pasteImage(source, ONE_PIXEL_PNG_BASE64);

  await expect(source).toContainText(`data:image/png;base64,${ONE_PIXEL_PNG_BASE64}`);

  await page.locator(".mesh-source-editor-actions button", { hasText: "Save" }).click();
  await expect.poll(() => fetchRaw(page)).toContain(`data:image/png;base64,${ONE_PIXEL_PNG_BASE64}`);
});

test("a large paste asks for confirmation first, and declining inserts nothing", async ({
  page,
  browserName,
}) => {
  test.skip(browserName === "firefox", "Gecko drops synthetic paste-event file data — see this file's own top comment.");

  // Doesn't need to be a real decodable image — the paste handler only
  // ever measures the resulting base64 *string* length before deciding to
  // ask, never decodes it (that's the renderer's job, and it's on-demand
  // at display time, not on paste).
  const bigBase64 = "A".repeat(1_400_000); // ~1.4MB of base64 text, over the 1MB soft-warn threshold

  page.once("dialog", (dialog) => {
    expect(dialog.message()).toContain("MB");
    void dialog.dismiss();
  });

  const root = page.locator('.react-flow__node[data-id="root"]');
  await root.locator(".mesh-node-title").hover();
  await root.locator('button[title="Edit this node\'s Markdown text"]').click();
  const source = page.locator(".mesh-text-editor-source .cm-content");
  await expect(source).toBeVisible();
  // Compared against the buffer's own content before, not asserted to be
  // literally absent — the two tests above may have already run against
  // this same shared fixture/server and left a real embed in it; what
  // matters here is that *this* paste adds nothing on top.
  const before = await source.textContent();
  await pasteImage(source, bigBase64);

  // Give the confirm()/dismiss round-trip a beat, then confirm nothing
  // changed.
  await page.waitForTimeout(200);
  await expect(source).toHaveText(before ?? "");
});
