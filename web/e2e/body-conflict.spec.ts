import { test, expect, type Page } from "@playwright/test";
import { selectNode, toolbarButton } from "./helpers";

// Drives web/e2e/fixtures/body-conflict.canvas.md — what the node's body
// editor does when the body is replaced by another client while it is open
// (TODO.canvas.md: "Оптимистичная конкурентность при записи файла"). The
// "other client" is a plain `PATCH /api/nodes/a` from the same page's
// request context: the server can't tell it from a second tab, an agent or
// the CLI, which is the point.

test.skip(({ browserName }) => browserName !== "chromium", "Monaco typing is only driven in Chromium here");

async function serverBody(page: Page): Promise<{ text: string; rev: string }> {
  const canvas = await page.evaluate(() => fetch("/api/canvas").then((r) => r.json()));
  const node = canvas.nodes.find((n: { id: string }) => n.id === "a");
  return { text: node.text, rev: node.bodyRev };
}

/** Replaces node a's body the way another client would: against the
 * revision it currently has. */
async function replaceBodyElsewhere(page: Page, text: string) {
  const { rev } = await serverBody(page);
  const res = await page.request.patch("/api/nodes/a", { data: { text, baseRev: rev } });
  expect(res.ok(), await res.text()).toBe(true);
}

async function openEditor(page: Page) {
  await selectNode(page.locator('.react-flow__node[data-id="a"]'));
  await toolbarButton(page, "Edit this node's Markdown text").click();
  const editor = page.locator(".mesh-text-editor-source .monaco-editor");
  await expect(editor).toBeVisible();
  return editor.locator(".view-lines");
}

/** Types at the end of the buffer, slowly — Monaco's EditContext surface can
 * drop keystrokes at Playwright's default rate (see undo-redo.spec.ts). */
async function typeAtEnd(page: Page, lines: ReturnType<Page["locator"]>, text: string) {
  await lines.click();
  await page.keyboard.press("ControlOrMeta+End");
  await page.keyboard.type(text, { delay: 80 });
}

test.beforeEach(async ({ page }) => {
  await page.goto("/");
  await page.waitForSelector(".mesh-node");
  // Every test starts from the same body, whatever the previous one left.
  await replaceBodyElsewhere(page, "body a");
  await page.reload();
  await page.waitForSelector(".mesh-node");
  await page.getByRole("button", { name: "Edit" }).click();
});

test("an editor with no unsaved edits quietly adopts a body changed elsewhere", async ({ page }) => {
  const lines = await openEditor(page);
  await expect(lines).toContainText("body a");

  await replaceBodyElsewhere(page, "changed elsewhere");

  await expect(lines).toContainText("changed elsewhere");
  await expect(page.locator(".mesh-text-editor-conflict")).toHaveCount(0);
});

test("unsaved edits are never written over a body changed elsewhere: the editor asks first", async ({ page }) => {
  const lines = await openEditor(page);
  await typeAtEnd(page, lines, " MINE");
  await expect(lines).toContainText("body a MINE");

  // Lands while the typed text is still waiting out its autosave debounce.
  await replaceBodyElsewhere(page, "theirs");

  await expect(page.locator(".mesh-text-editor-conflict")).toBeVisible();
  // Well past the debounce: nothing may have been written over "theirs".
  await page.waitForTimeout(1500);
  expect((await serverBody(page)).text).toBe("theirs");
  await expect(lines).toContainText("body a MINE");

  await page.locator(".mesh-text-editor-conflict button", { hasText: "compare" }).click();
  await expect(page.locator(".mesh-text-editor-conflict-theirs")).toHaveText("theirs");
});

test("'take theirs' drops the unsaved edits and shows the other body", async ({ page }) => {
  const lines = await openEditor(page);
  await typeAtEnd(page, lines, " MINE");
  await replaceBodyElsewhere(page, "theirs");
  await expect(page.locator(".mesh-text-editor-conflict")).toBeVisible();

  await page.locator(".mesh-text-editor-conflict button", { hasText: "take theirs" }).click();

  await expect(page.locator(".mesh-text-editor-conflict")).toHaveCount(0);
  await expect(lines).toContainText("theirs");
  await expect(lines).not.toContainText("MINE");
  await page.waitForTimeout(1200);
  expect((await serverBody(page)).text).toBe("theirs");
});

test("'keep mine' writes the editor's text over the other body, against its revision", async ({ page }) => {
  const lines = await openEditor(page);
  await typeAtEnd(page, lines, " MINE");
  await replaceBodyElsewhere(page, "theirs");
  await expect(page.locator(".mesh-text-editor-conflict")).toBeVisible();

  await page.locator(".mesh-text-editor-conflict button", { hasText: "keep mine" }).click();

  await expect(page.locator(".mesh-text-editor-conflict")).toHaveCount(0);
  await expect.poll(async () => (await serverBody(page)).text).toBe("body a MINE");
});

// ---- Source mode: the whole-file editor writes against the file's ETag ----

/** Replaces the whole file the way another client would: against the ETag
 * the file currently has. */
async function replaceFileElsewhere(page: Page, edit: (text: string) => string) {
  const read = await page.request.get("/api/canvas/raw");
  const etag = read.headers()["etag"];
  expect(etag, "GET /api/canvas/raw carries an ETag").toBeTruthy();
  const res = await page.request.put("/api/canvas/raw", {
    headers: { "if-match": etag, "content-type": "text/plain" },
    data: edit(await read.text()),
  });
  expect(res.status(), await res.text()).toBe(204);
}

async function fileText(page: Page): Promise<string> {
  return (await page.request.get("/api/canvas/raw")).text();
}

async function openSourceEditor(page: Page) {
  await page.getByRole("button", { name: "Source" }).click();
  const editor = page.locator(".mesh-source-editor-body .monaco-editor");
  await expect(editor).toBeVisible();
  return editor.locator(".view-lines");
}

test("a Source-mode save over a file changed elsewhere is refused: the editor asks first", async ({ page }) => {
  const lines = await openSourceEditor(page);
  await typeAtEnd(page, lines, " MINE");
  await expect(lines).toContainText("MINE");

  await replaceFileElsewhere(page, (t) => t.replace("body a", "body a, theirs"));

  await page.locator(".mesh-source-editor-actions button", { hasText: "Save" }).click();
  await expect(page.locator(".mesh-source-editor-conflict")).toBeVisible();
  const onDisk = await fileText(page);
  expect(onDisk).toContain("body a, theirs");
  expect(onDisk).not.toContain("MINE");

  await page.locator(".mesh-source-editor-conflict button", { hasText: "compare" }).click();
  await expect(page.locator(".mesh-source-editor-conflict-theirs")).toContainText("body a, theirs");

  await page.locator(".mesh-source-editor-conflict button", { hasText: "take theirs" }).click();
  await expect(page.locator(".mesh-source-editor-conflict")).toHaveCount(0);
  await expect(lines).toContainText("theirs");
  await expect(lines).not.toContainText("MINE");
  // Nothing of this editor's was written, and there is nothing left to save.
  expect(await fileText(page)).not.toContain("MINE");
  await expect(page.locator(".mesh-source-editor-actions button", { hasText: "Save" })).toBeDisabled();
});

test("'keep mine' in Source mode writes the editor's text over the file, against its current ETag", async ({ page }) => {
  const lines = await openSourceEditor(page);
  await typeAtEnd(page, lines, " MINE");
  await replaceFileElsewhere(page, (t) => t.replace("body a", "body a, theirs"));
  await page.locator(".mesh-source-editor-actions button", { hasText: "Save" }).click();
  await expect(page.locator(".mesh-source-editor-conflict")).toBeVisible();

  await page.locator(".mesh-source-editor-conflict button", { hasText: "keep mine" }).click();

  // Saved: Source mode closes, and the file now holds this editor's text.
  await expect(page.locator(".mesh-source-editor")).toHaveCount(0);
  const onDisk = await fileText(page);
  expect(onDisk).toContain("MINE");
  expect(onDisk).not.toContain("body a, theirs");
});
