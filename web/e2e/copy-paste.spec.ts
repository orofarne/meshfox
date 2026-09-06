import { test, expect, type Locator, type Page } from "@playwright/test";
import { selectNode, toolbarButton } from "./helpers";

// Drives web/e2e/fixtures/copy-paste.canvas.md (TODO.canvas.md: "VSCode:
// вставка текста (Cmd+V и контекстное меню) в редактор ноды не работает") —
// real, OS-trusted keyboard/mouse copy and paste against the two Monaco
// editors that share `MONACO_OPTIONS` (NodeTextEditor's node-body editor,
// CanvasSourceEditor's whole-document editor), unlike image-paste.spec.ts's
// synthetic `dispatchEvent(new ClipboardEvent(...))` approach (which
// deliberately never touches the real OS clipboard, and so never exercises
// Monaco's own EditContext-vs-classic-textarea code paths this suite cares
// about — see MONACO_OPTIONS' own `contextmenu` comment in
// NodeTextEditor.tsx).
//
// Two automation-specific gotchas shaped every test below, confirmed
// directly (a throwaway script against a bare fixture, repeated until each
// step was isolated):
//
// 1. Selecting text via the *keyboard* (`ControlOrMeta+A`, or even a plain
//    `Shift+End`) against Monaco 0.53's Chromium-only `EditContext` input
//    surface (`.native-edit-context`) leaves it permanently unresponsive to
//    every further Playwright-dispatched key press — not just the
//    following one, and not fixed by re-focusing it — while a *mouse*
//    selection (a triple-click) behaves normally afterward. Whether this is
//    a genuine EditContext-vs-CDP incompatibility or something narrower,
//    it's specific to synthetic (CDP) input: this is exactly the same
//    surface a real trusted Cmd+V already lands on fine elsewhere in this
//    file. Every selection below is a triple-click, never a keyboard
//    shortcut, to sidestep it rather than fight it.
// 2. A genuinely trusted `ControlOrMeta+KeyC` against that same surface
//    causes the *identical* stuck-after-selection symptom on its own, mouse
//    selection or not — so no test here ever types into an editor again
//    after copying from it. Each test copies from one editor and pastes
//    into a *different* one instead.
//
// Because these tests use the machine's real, global clipboard (there's no
// way to trigger a genuinely *trusted* paste otherwise — Playwright's
// `context.grantPermissions(["clipboard-read"/"clipboard-write"])` only
// wires up the *scripted* `navigator.clipboard` API, not real Cmd/Ctrl+C/V),
// every test also does its own copy immediately before its own paste rather
// than relying on clipboard content left by an earlier step or another
// test/project — this file's own tests already run serially against each
// other (`fullyParallel: false` in playwright.config.ts), but a *different*
// project (e.g. the Firefox variant of this same suite) can still be
// executing concurrently in another worker and racing the same real OS
// clipboard. Each marker string below is unique per test, so cross-talk
// from that race would surface as a clear assertion failure (wrong/missing
// text) rather than a silent false pass.

function bodyEditor(page: Page): Locator {
  return page.locator(".mesh-text-editor-source .monaco-editor");
}

function wholeDocEditor(page: Page): Locator {
  return page.locator(".mesh-source-editor-body .monaco-editor");
}

async function openRootBodyEditor(page: Page) {
  const root = page.locator('.react-flow__node[data-id="root"]');
  await selectNode(root);
  await toolbarButton(page, "Edit this node's Markdown text").click();
}

/** Types `text` (via `insertText`, not `keyboard.type` — confirmed directly
 * that per-character `type()` can drop characters against this same
 * EditContext surface under fast synthetic dispatch) at the end of
 * `editor`'s content, then selects just that appended text with a
 * triple-click (see this file's own top comment for why not a keyboard
 * shortcut) and copies it with a real, trusted `ControlOrMeta+C`. Leaves
 * `editor` itself alone afterward — per this file's top comment, typing
 * into it again after this would be broken until a reload. */
async function appendSelectAndCopy(page: Page, editor: Locator, text: string) {
  await editor.locator(".view-lines").click();
  await page.keyboard.press("ControlOrMeta+End");
  await page.keyboard.insertText(`\n${text}`);
  await editor.locator(".view-line", { hasText: text }).click({ clickCount: 3 });
  await page.keyboard.press("ControlOrMeta+KeyC");
}

test.beforeEach(async ({ page }) => {
  // Every test here leaves the Source editor via Cancel with unsaved edits
  // still in it (see `appendSelectAndCopy`'s own doc comment) —
  // `CanvasSourceEditor.confirmDiscardIfDirty` raises a native
  // `window.confirm("Discard unsaved source changes?")` for that, which
  // Playwright auto-dismisses (i.e. answers "no") unless a handler here
  // accepts it instead.
  page.on("dialog", (dialog) => dialog.accept());
  await page.goto("/");
  await page.waitForSelector(".mesh-node");
  await page.getByRole("button", { name: "Edit" }).click();
});

test("keyboard copy in the whole-document source editor and keyboard paste in the node body editor round-trip the real clipboard", async ({
  page,
  browserName,
}) => {
  const marker = `COPY_PASTE_ROUNDTRIP_${browserName}`;

  // Copy source: the whole-document Source editor, left un-Saved — so the
  // marker text typed into it here never reaches the fixture file, and
  // can't show up as a false positive on the paste side below (which reads
  // the file's own content via the node body editor).
  await page.getByRole("button", { name: "Source" }).click();
  const wholeDoc = wholeDocEditor(page);
  await expect(wholeDoc).toBeVisible();
  await appendSelectAndCopy(page, wholeDoc, marker);
  await page.locator(".mesh-source-editor-actions button", { hasText: "Cancel" }).click();

  // Paste target: the (empty) node body editor.
  await openRootBodyEditor(page);
  const body = bodyEditor(page);
  await expect(body).toBeVisible();
  await body.locator(".view-lines").click();
  await page.keyboard.press("ControlOrMeta+KeyV");

  await expect(body).toContainText(marker);
});

test("Monaco's own broken 'Paste' menu item never appears, on any browser", async ({ page }) => {
  // An earlier version of this fix (see MONACO_OPTIONS' own `contextmenu`
  // comment in NodeTextEditor.tsx) left Monaco's own context menu enabled
  // on Firefox specifically, on the theory that its classic `<textarea>`
  // input surface (unlike Chromium's `EditContext`-based one) didn't share
  // the same `execCommand("paste")` breakage. Confirmed by hand afterward:
  // a real right-click → Paste through Firefox's own rendered menu item
  // doesn't actually insert anything there either — so `contextmenu` is
  // now unconditionally `false`, and this is a same-for-every-browser
  // regression guard rather than a per-browser one: if it were ever flipped
  // back to `true` anywhere, Monaco's own broken "Paste" item would
  // reappear here. Not a check that the browser's/Electron's own native
  // menu's Paste works — Playwright can't reach a real OS/Electron context
  // menu at all, it renders outside the page's DOM.
  await openRootBodyEditor(page);
  const body = bodyEditor(page);
  await expect(body).toBeVisible();
  await body.locator(".view-lines").click({ button: "right" });
  await page.waitForTimeout(200);
  await expect(page.getByRole("menuitem", { name: "Paste" })).toHaveCount(0);
  await page.keyboard.press("Escape");
});
