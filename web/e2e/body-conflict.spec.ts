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

  // Lands while the editor holds an unsaved local draft.
  await replaceBodyElsewhere(page, "theirs");

  await expect(page.locator(".mesh-text-editor-conflict")).toBeVisible();
  // Well past the debounce: nothing may have been written over "theirs".
  await page.waitForTimeout(1500);
  expect((await serverBody(page)).text).toBe("theirs");
  await expect(lines).toContainText("body a MINE");

  await page.locator(".mesh-text-editor-conflict button", { hasText: "compare" }).click();
  await expect(page.locator(".mesh-text-editor-conflict-theirs")).toContainText("theirs");
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

// Explicit saves and ordering regressions. All writes use the real worker;
// routes only control response timing or simulate a failed connection.
test("title and body remain local until Apply; Cancel discards subsequent changes", async ({ page }) => {
  const lines = await openEditor(page);
  await typeAtEnd(page, lines, " DRAFT");
  const title = page.locator(".mesh-text-editor-title-input");
  await title.fill("Local title");
  await title.blur();
  await page.waitForTimeout(1000);
  expect((await serverBody(page)).text).toBe("body a");
  const initial = await (await page.request.get("/api/canvas")).json();
  expect(initial.nodes.find((n: { id: string }) => n.id === "a").title).not.toBe("Local title");
  await page.getByRole("button", { name: "Apply", exact: true }).click();
  await expect(page.getByRole("button", { name: "Apply", exact: true })).toBeDisabled();
  const applied = await (await page.request.get("/api/canvas")).json();
  expect(applied.nodes.find((n: { id: string }) => n.id === "a")).toMatchObject({ title: "Local title", text: "body a DRAFT" });
  await typeAtEnd(page, lines, " CANCELLED");
  await title.fill("Cancelled title");
  await page.getByRole("button", { name: "Cancel", exact: true }).click();
  await expect(page.locator(".mesh-text-editor")).toHaveCount(0);
  expect((await serverBody(page)).text).toBe("body a DRAFT");
});

test("Apply preserves the cursor and local whitespace; Ctrl/Cmd+S applies without closing", async ({ page }) => {
  const lines = await openEditor(page);
  await lines.click();
  await page.keyboard.press("End");
  await page.keyboard.press("Enter");
  await expect(lines.locator(".view-line")).toHaveCount(2);
  await page.keyboard.press("ArrowUp");
  await page.keyboard.press("Home");
  await page.keyboard.type("X ", { delay: 80 });
  await expect(lines).toContainText("X body a");
  await page.keyboard.press("ControlOrMeta+s");
  await expect.poll(async () => (await serverBody(page)).text).toBe("X body a");
  await expect(page.getByRole("button", { name: "Apply", exact: true })).toBeDisabled();
  // The server trimmed the trailing newline; the local model keeps it.
  await expect(lines.locator(".view-line")).toHaveCount(2);
  await page.keyboard.type("Y", { delay: 80 });
  await expect(lines).toContainText("X Ybody a");
  await expect(page.locator(".mesh-text-editor-conflict")).toHaveCount(0);
  await page.getByRole("button", { name: "Save & close", exact: true }).click();
  await expect(page.locator(".mesh-text-editor")).toHaveCount(0);
  expect((await serverBody(page)).text).toBe("X Ybody a");
});

test("typing while Apply awaits its response stays unsaved and never self-conflicts", async ({ page }) => {
  const lines = await openEditor(page);
  await typeAtEnd(page, lines, " FIRST");
  let release!: () => void;
  const held = new Promise<void>((resolve) => { release = resolve; });
  let arrived!: () => void;
  const pending = new Promise<void>((resolve) => { arrived = resolve; });
  await page.route("**/api/nodes/a", async (route) => {
    const response = await route.fetch();
    arrived();
    await held;
    await route.fulfill({ response });
  });
  await page.getByRole("button", { name: "Apply", exact: true }).click();
  await pending;
  await typeAtEnd(page, lines, " SECOND");
  release();
  await expect(page.getByRole("button", { name: "Apply", exact: true })).toBeEnabled();
  await expect(lines).toContainText("body a FIRST SECOND");
  expect((await serverBody(page)).text).toBe("body a FIRST");
  await expect(page.locator(".mesh-text-editor-conflict")).toHaveCount(0);
  await page.unroute("**/api/nodes/a");
  await page.getByRole("button", { name: "Save & close", exact: true }).click();
  await expect(page.locator(".mesh-text-editor")).toHaveCount(0);
  expect((await serverBody(page)).text).toBe("body a FIRST SECOND");
});

test("an older GET arriving after Apply cannot roll back the canvas or editor", async ({ page }) => {
  const lines = await openEditor(page);
  let release!: () => void;
  const held = new Promise<void>((resolve) => { release = resolve; });
  let arrived!: () => void;
  const pending = new Promise<void>((resolve) => { arrived = resolve; });
  let first = true;
  await page.route("**/api/canvas", async (route) => {
    if (!first) { await route.continue(); return; }
    first = false;
    const response = await route.fetch();
    arrived();
    await held;
    await route.fulfill({ response });
  });
  // An unrelated operation starts a reload holding the old body of a.
  const changed = await page.request.patch("/api/nodes/root", { data: { title: `Other ${Date.now()}` } });
  expect(changed.ok()).toBe(true);
  await pending;
  await typeAtEnd(page, lines, " NEW");
  await page.getByRole("button", { name: "Apply", exact: true }).click();
  await expect(page.getByRole("button", { name: "Apply", exact: true })).toBeDisabled();
  release();
  await page.waitForTimeout(500);
  await expect(lines).toContainText("body a NEW");
  await expect(page.locator('.react-flow__node[data-id="a"] .mesh-node-body')).toContainText("body a NEW");
  await expect(page.locator(".mesh-text-editor-conflict")).toHaveCount(0);
  expect((await serverBody(page)).text).toBe("body a NEW");
});

test("failed Save & close keeps the entire draft open", async ({ page }) => {
  const lines = await openEditor(page);
  await typeAtEnd(page, lines, " RETRY");
  await page.locator(".mesh-text-editor-title-input").fill("Retry title");
  await page.route("**/api/nodes/a", (route) => route.fulfill({ status: 500, body: "simulated failure" }));
  await page.getByRole("button", { name: "Save & close", exact: true }).click();
  await expect(page.locator(".mesh-text-editor-error")).toBeVisible();
  await expect(lines).toContainText("body a RETRY");
  expect((await serverBody(page)).text).toBe("body a");
  await page.unroute("**/api/nodes/a");
  await page.getByRole("button", { name: "Save & close", exact: true }).click();
  await expect(page.locator(".mesh-text-editor")).toHaveCount(0);
  expect((await serverBody(page)).text).toBe("body a RETRY");
});

test("adjacent ordinary fences have a visible gap in the editor preview", async ({ page }) => {
  await replaceBodyElsewhere(page, '```\necho "# Hello!"\n```\n\n```\necho "# Hello!"\n```');
  await openEditor(page);
  const blocks = page.locator(".mesh-text-editor-preview .mesh-node-body > .mesh-code-block-source");
  await expect(blocks).toHaveCount(2);
  const first = await blocks.nth(0).boundingBox();
  const second = await blocks.nth(1).boundingBox();
  expect(second!.y - (first!.y + first!.height)).toBeGreaterThan(4);
});

test("slug suggestion appears even for a manually assigned ID", async ({ page }) => {
  await openEditor(page);
  await page.locator(".mesh-text-editor-header button").click();
  const settings = page.locator(".node-settings-modal");
  await settings.locator('.vars-modal-field', { hasText: "Title" }).locator("input").fill("Test Node");
  await settings.locator('.vars-modal-field', { hasText: "ID" }).locator("input").fill("random123");
  await expect(settings.locator(".node-settings-id-hint")).toContainText('test-node');
  await settings.locator(".node-settings-id-hint button").click();
  await expect(settings.locator('.vars-modal-field', { hasText: "ID" }).locator("input")).toHaveValue("test-node");
  await expect(settings.locator(".node-settings-id-hint")).toHaveCount(0);
});


test("backdrop close offers save, discard, or continued editing", async ({ page }) => {
  const lines = await openEditor(page);
  await typeAtEnd(page, lines, " DRAFT");
  await page.locator(".mesh-text-editor-backdrop").click({ position: { x: 5, y: 5 } });
  const prompt = page.getByRole("alertdialog", { name: "Unsaved changes" });
  await expect(prompt).toBeVisible();
  await prompt.getByRole("button", { name: "Keep editing" }).click();
  await expect(prompt).toHaveCount(0);
  await expect(lines).toContainText("body a DRAFT");
  await page.locator(".mesh-text-editor-backdrop").click({ position: { x: 5, y: 5 } });
  await prompt.getByRole("button", { name: "Save & close", exact: true }).click();
  await expect(page.locator(".mesh-text-editor")).toHaveCount(0);
  expect((await serverBody(page)).text).toBe("body a DRAFT");
});

test("a title changed elsewhere conflicts with the local title/body draft", async ({ page }) => {
  const lines = await openEditor(page);
  await typeAtEnd(page, lines, " MINE");
  await page.locator(".mesh-text-editor-title-input").fill("My title");
  const changed = await page.request.patch("/api/nodes/a", { data: { title: "Their title" } });
  expect(changed.ok()).toBe(true);
  await expect(page.locator(".mesh-text-editor-conflict")).toBeVisible();
  expect((await serverBody(page)).text).toBe("body a");
  await page.getByRole("button", { name: "keep mine", exact: true }).click();
  await expect(page.locator(".mesh-text-editor-conflict")).toHaveCount(0);
  await expect.poll(async () => (await serverBody(page)).text).toBe("body a MINE");
  const canvas = await (await page.request.get("/api/canvas")).json();
  expect(canvas.nodes.find((n: { id: string }) => n.id === "a").title).toBe("My title");
});
