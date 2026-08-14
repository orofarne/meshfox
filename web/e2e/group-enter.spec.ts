import { test, expect, type Page, type Locator } from "@playwright/test";

// Drives web/e2e/fixtures/group-enter.canvas.md — a group's expand button
// (see MeshNode.tsx) opens a mini sub-canvas of its own direct members
// (NodeExpandPanel.tsx), scoped to exactly that group's subtree.

function fetchRaw(page: Page): Promise<string> {
  return page.evaluate(() => fetch("/api/canvas/raw").then((r) => r.text()));
}

function nodeMeta(raw: string, id: string): { x: number; y: number } | null {
  const re = new RegExp(`id="${id}"[^>]*x=(-?\\d+(?:\\.\\d+)?)\\s+y=(-?\\d+(?:\\.\\d+)?)`);
  const m = raw.match(re);
  return m ? { x: Number(m[1]), y: Number(m[2]) } : null;
}

async function dragNodeTitle(page: Page, node: Locator, dx: number, dy: number) {
  const titleText = node.locator(".mesh-node-title-text");
  const box = await titleText.boundingBox();
  if (!box) throw new Error("node title text has no box");
  const startX = box.x + Math.min(10, box.width / 2);
  const startY = box.y + box.height / 2;
  await page.mouse.move(startX, startY);
  await page.mouse.down();
  await page.mouse.move(startX + dx, startY + dy, { steps: 10 });
  await page.mouse.up();
}

test.beforeEach(async ({ page }) => {
  await page.goto("/");
  await page.waitForSelector(".mesh-node");
});

test("a group's expand button opens a mini sub-canvas of exactly its own members", async ({ page }) => {
  const frame = page.locator('.react-flow__node[data-id="frame"]');
  await frame.locator(".mesh-node-expand-icon").click();

  const panel = page.locator(".mesh-expand-panel-group");
  await expect(panel).toBeVisible();
  await expect(panel.locator('.react-flow__node[data-id="member-one"]')).toBeVisible();
  await expect(panel.locator('.react-flow__node[data-id="member-two"]')).toBeVisible();
  // Neither the group itself nor an unrelated sibling should appear inside
  // the mini canvas — it's scoped to exactly this group's own members.
  await expect(panel.locator('.react-flow__node[data-id="frame"]')).toHaveCount(0);
  await expect(panel.locator('.react-flow__node[data-id="outsider"]')).toHaveCount(0);
});

test("closing the panel via the backdrop hides it again", async ({ page }) => {
  const frame = page.locator('.react-flow__node[data-id="frame"]');
  await frame.locator(".mesh-node-expand-icon").click();
  await expect(page.locator(".mesh-expand-panel-group")).toBeVisible();

  await page.locator(".mesh-expand-backdrop").click({ position: { x: 5, y: 5 } });

  await expect(page.locator(".mesh-expand-panel-group")).toHaveCount(0);
});

test("dragging a member inside the mini canvas persists the same way as on the main canvas", async ({ page }) => {
  await page.getByRole("button", { name: "Edit" }).click();
  const frame = page.locator('.react-flow__node[data-id="frame"]');
  // Edit mode's extra per-node action buttons (resize handles included)
  // push member-one's own title bar close enough to frame's own that a
  // coordinate-based click (even `force: true`, which only skips
  // Playwright's own actionability checks, not the browser's native
  // hit-testing at that screen position) can land on the member instead —
  // invoke the DOM `click()` method directly on the right element, no
  // screen coordinates involved.
  await frame.locator(".mesh-node-expand-icon").evaluate((el: HTMLElement) => el.click());

  const panel = page.locator(".mesh-expand-panel-group");
  const memberOne = panel.locator('.react-flow__node[data-id="member-one"]');
  await expect(memberOne).toBeVisible();

  const before = nodeMeta(await fetchRaw(page), "member-one");
  const memberTwoBefore = nodeMeta(await fetchRaw(page), "member-two");
  expect(before).not.toBeNull();

  await dragNodeTitle(page, memberOne, 50, 30);

  await expect
    .poll(async () => nodeMeta(await fetchRaw(page), "member-one"), { timeout: 10_000 })
    .not.toEqual(before);
  // The other member, untouched, keeps its own position exactly.
  expect(nodeMeta(await fetchRaw(page), "member-two")).toEqual(memberTwoBefore);
});
