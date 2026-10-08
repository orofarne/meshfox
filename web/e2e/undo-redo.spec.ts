import { test, expect, type Page } from "@playwright/test";
import { selectNode, toolbarButton } from "./helpers";

// Drives web/e2e/fixtures/undo-redo.canvas.md — the toolbar's ↶ undo/↷ redo
// buttons and their Cmd/Ctrl-Z / Cmd/Ctrl-Shift-Z shortcuts
// (TODO.canvas.md: "Undo для правок канваса", layer 6 — "webui: шоткаты +
// кнопки"), plus the one behavior that matters most for that layer: the
// shortcut must defer to Monaco's own native undo while focus is inside a
// node's body editor, not fire the document-level `/api/undo` underneath it
// (see App.tsx's `isEditableTarget`).

function undoButton(page: Page) {
  return page.getByRole("button", { name: "↶ undo" });
}

function redoButton(page: Page) {
  return page.getByRole("button", { name: "↷ redo" });
}

async function nodeAText(page: Page): Promise<string> {
  const canvas = await page.evaluate(() => fetch("/api/canvas").then((r) => r.json()));
  return canvas.nodes.find((n: { id: string }) => n.id === "a").text;
}

// Opens node "a"'s inline body editor, appends " EDITED" to its text, and
// saves via Save & close, which waits for the write before closing.
async function editNodeABody(page: Page) {
  const node = page.locator('.react-flow__node[data-id="a"]');
  await selectNode(node);
  await toolbarButton(page, "Edit this node's Markdown text").click();
  const editor = page.locator(".mesh-text-editor-source .monaco-editor");
  await expect(editor).toBeVisible();
  const lines = editor.locator(".view-lines");
  await lines.click();
  await page.keyboard.press("ControlOrMeta+End");
  // Monaco 0.53's EditContext input surface can drop or reorder keystrokes
  // typed at Playwright's default (near-zero-delay) CDP dispatch rate —
  // confirmed directly (a throwaway script against this same fixture,
  // repeated until isolated): plain `.type(" EDITED")` right after a
  // Ctrl/Cmd-End lands as "TED", "TED EDID", or similar mangled/reordered
  // output more often than not. A per-key delay avoids it; the trailing
  // `toContainText` still double-checks the exact final string landed
  // before Save & close commits whatever's actually there.
  await page.keyboard.type(" EDITED", { delay: 80 });
  await expect(lines).toContainText("body a EDITED");
  await page.locator(".mesh-text-editor-actions button", { hasText: "Save & close" }).click();
  await expect(page.locator(".mesh-text-editor")).toHaveCount(0);
}

test.beforeEach(async ({ page }) => {
  await page.goto("/");
  await page.waitForSelector(".mesh-node");
  await page.getByRole("button", { name: "Edit" }).click();
});

test("undo/redo buttons start disabled and enable as history accumulates", async ({ page }) => {
  const before = await nodeAText(page);
  await expect(undoButton(page)).toBeDisabled();
  await expect(redoButton(page)).toBeDisabled();

  await editNodeABody(page);

  await expect(undoButton(page)).toBeEnabled();
  await expect(redoButton(page)).toBeDisabled();

  // Leave the fixture (and its undo history) exactly as this suite found
  // it — every other test in this file assumes it's starting from a
  // fully-undone log, same as this one did.
  await undoButton(page).click();
  await expect.poll(() => nodeAText(page)).toBe(before);
  await expect(undoButton(page)).toBeDisabled();
});

test("clicking undo reverts the last edit; clicking redo reapplies it", async ({ page }) => {
  const before = await nodeAText(page);
  await editNodeABody(page);
  const edited = await nodeAText(page);
  expect(edited).not.toBe(before);

  await undoButton(page).click();
  await expect.poll(() => nodeAText(page)).toBe(before);
  await expect(undoButton(page)).toBeDisabled();
  await expect(redoButton(page)).toBeEnabled();

  await redoButton(page).click();
  await expect.poll(() => nodeAText(page)).toBe(edited);
  await expect(undoButton(page)).toBeEnabled();
  await expect(redoButton(page)).toBeDisabled();

  // Leave the fixture as this suite found it.
  await undoButton(page).click();
  await expect.poll(() => nodeAText(page)).toBe(before);
});

test("Cmd/Ctrl-Z and Cmd/Ctrl-Shift-Z drive undo/redo when focus is on the canvas", async ({ page }) => {
  const before = await nodeAText(page);
  await editNodeABody(page);
  const edited = await nodeAText(page);

  // Away from any input/editor, same as a real user's focus most of the
  // time — `<body>` itself, reached by blurring whatever `editNodeABody`
  // last focused.
  await page.locator("body").click({ position: { x: 5, y: 5 } });

  await page.keyboard.press("ControlOrMeta+z");
  await expect.poll(() => nodeAText(page)).toBe(before);
  await expect(undoButton(page)).toBeDisabled();

  await page.keyboard.press("ControlOrMeta+Shift+z");
  await expect.poll(() => nodeAText(page)).toBe(edited);
  await expect(redoButton(page)).toBeDisabled();

  // Leave the fixture as this suite found it.
  await page.keyboard.press("ControlOrMeta+z");
  await expect.poll(() => nodeAText(page)).toBe(before);
});

test("Cmd/Ctrl-Z is swallowed by the node body editor instead of triggering document undo", async ({ page }) => {
  const before = await nodeAText(page);
  // Establish real undo history first — if the app-level shortcut *did*
  // fire despite focus being in the editor, this is what it would revert,
  // making the regression this test guards against actually observable.
  await editNodeABody(page);
  await expect(undoButton(page)).toBeEnabled();

  let undoRequests = 0;
  await page.route("**/api/undo", (route) => {
    undoRequests++;
    route.continue();
  });

  const node = page.locator('.react-flow__node[data-id="a"]');
  await selectNode(node);
  await toolbarButton(page, "Edit this node's Markdown text").click();
  const editor = page.locator(".mesh-text-editor-source .monaco-editor");
  const lines = editor.locator(".view-lines");
  await lines.click();
  await page.keyboard.press("ControlOrMeta+End");
  await page.keyboard.type("x", { delay: 80 });
  await expect(lines).toContainText("body a EDITEDx");
  await page.keyboard.press("ControlOrMeta+z");

  // The app's own `/api/undo` must never have been reached — Monaco's own
  // native undo (or lack of any effect at all, since a genuinely trusted
  // Ctrl/Cmd-Z into Monaco's EditContext surface isn't this suite's
  // concern) is a separate matter from whether `isEditableTarget` actually
  // gated the document-level shortcut, which is all this asserts.
  expect(undoRequests).toBe(0);

  // Save & close commits whatever the editor currently holds. What
  // that is depends on the browser, which is not this test's concern:
  // Chrome's Monaco ignores the synthetic Ctrl/Cmd-Z and keeps the typed
  // "x", so Save & close is a second real edit on top of the first; Firefox's
  // Monaco natively undoes the "x", so Save & close commits text identical to
  // what's already saved and adds no history entry at all. Undo only as
  // many steps as were really recorded, then check the fixture is restored.
  await page.locator(".mesh-text-editor-actions button", { hasText: "Save & close" }).click();
  await expect(page.locator(".mesh-text-editor")).toHaveCount(0);
  if ((await nodeAText(page)) !== "body a EDITED") {
    await undoButton(page).click();
    await expect.poll(() => nodeAText(page)).toBe("body a EDITED");
  }
  await undoButton(page).click();
  await expect.poll(() => nodeAText(page)).toBe(before);
});

test("history panel's 'Start of history' row reverts the oldest step, which no real row can", async ({ page }) => {
  const before = await nodeAText(page);
  await editNodeABody(page);
  expect(await nodeAText(page)).not.toBe(before);

  await page.getByRole("button", { name: "🕘 history" }).click();
  // Every real row jumps to the state right *after* its own step, so
  // only this synthetic bottom row (goto seq 0) can undo the log's oldest step.
  await page.locator(".history-panel-row", { hasText: /Start of history/ }).click();

  await expect.poll(() => nodeAText(page)).toBe(before);
  await expect(undoButton(page)).toBeDisabled();
  await expect(redoButton(page)).toBeEnabled();
});
