import { test, expect, type Page, type Locator } from "@playwright/test";
import { selectNode, toolbarButton } from "./helpers";

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

async function nodeBox(page: Page, id: string) {
  const result = await page.locator(`.react-flow__node[data-id="${id}"]`).boundingBox();
  if (!result) throw new Error(`${id} has no box`);
  return result;
}

test("a group's visible frame keeps its intended padding around positioned members", async ({ page }) => {
  await expect.poll(async () => {
    const frame = await nodeBox(page, "frame");
    const first = await nodeBox(page, "member-two");
    const last = await nodeBox(page, "member-one");
    const left = first.x - frame.x;
    const right = frame.x + frame.width - (last.x + last.width);
    const top = first.y - frame.y;
    const bottom = frame.y + frame.height - (last.y + last.height);
    return left > 10 && bottom > 10 && Math.abs(left - right) < 3 && Math.abs(top - 2 * bottom) < 3;
  }).toBe(true);
});

test("an auto group with a member at x=0/y=0 stays padded and clear of its siblings", async ({ page }) => {
  await expect.poll(async () => {
    const before = await nodeBox(page, "outsider");
    const frame = await nodeBox(page, "auto-frame");
    const member = await nodeBox(page, "at-origin");
    const after = await nodeBox(page, "after-auto-frame");
    const left = member.x - frame.x;
    const top = member.y - frame.y;
    const right = frame.x + frame.width - member.x - member.width;
    const bottom = frame.y + frame.height - member.y - member.height;
    return (
      frame.y >= before.y + before.height &&
      after.y >= frame.y + frame.height &&
      left > 10 &&
      Math.abs(left - right) < 3 &&
      Math.abs(top - 2 * bottom) < 3
    );
  }).toBe(true);
});

test("unfolding an auto group reflows its following sibling below the full frame", async ({ page }) => {
  const frame = page.locator('.react-flow__node[data-id="auto-frame"]');
  const toggle = frame.locator(".mesh-node-fold-toggle");
  await toggle.click();
  await expect(page.locator('.react-flow__node[data-id="at-origin"]')).toHaveCount(0);
  const foldedAfter = await nodeBox(page, "after-auto-frame");

  await toggle.click();
  await expect(page.locator('.react-flow__node[data-id="at-origin"]')).toBeVisible();
  await expect.poll(async () => {
    const fullFrame = await nodeBox(page, "auto-frame");
    const after = await nodeBox(page, "after-auto-frame");
    return after.y > foldedAfter.y && after.y >= fullFrame.y + fullFrame.height;
  }).toBe(true);
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
  // The expand icon now lives in a floating `NodeToolbar`, shown only once
  // `frame` is selected and portaled out of its own DOM subtree entirely
  // (see helpers.ts's own doc comments on `selectNode`/`toolbarButton`) —
  // select it first, then reach the toolbar globally rather than scoped
  // under `frame`.
  await selectNode(frame);
  await toolbarButton(page, "Open this group's members").click();

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
