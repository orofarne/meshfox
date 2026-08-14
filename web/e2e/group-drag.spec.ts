import { test, expect, type Page } from "@playwright/test";
import { clickFitViewAndWait, disableDefaultFold } from "./helpers";

// Drives web/e2e/fixtures/group-drag.canvas.md — a group with a real
// anchor (`frame`) and two real, group-relative members. Group-member
// coordinates are relative to their group's own anchor (see SPEC.md and
// App.tsx's `positionFor`), so dragging a single member should only ever
// rewrite that member's own stored x/y, and dragging the group itself
// (via its title bar, React Flow's native `parentId` nesting) should only
// ever rewrite the group's own anchor — the members' own relative offsets
// stay exactly as authored either way.

function fetchRaw(page: Page): Promise<string> {
  return page.evaluate(() => fetch("/api/canvas/raw").then((r) => r.text()));
}

function nodeMeta(raw: string, id: string): { x: number; y: number } | null {
  const re = new RegExp(`id="${id}"[^>]*x=(-?\\d+(?:\\.\\d+)?)\\s+y=(-?\\d+(?:\\.\\d+)?)`);
  const m = raw.match(re);
  return m ? { x: Number(m[1]), y: Number(m[2]) } : null;
}

async function dragNodeTitle(page: Page, nodeId: string, dx: number, dy: number) {
  // Dragging from the title *text* specifically, not the title bar's full
  // bounding box: in Edit mode the title bar also holds several action
  // buttons (expand/edit/settings/delete, all `nodrag`) that can occupy
  // enough width to land a center-of-box click on one of them instead of
  // plain title background, silently swallowing the drag.
  const titleText = page.locator(`.react-flow__node[data-id="${nodeId}"] .mesh-node-title-text`);
  const box = await titleText.boundingBox();
  if (!box) throw new Error(`${nodeId} title text has no box`);
  const startX = box.x + Math.min(10, box.width / 2);
  const startY = box.y + box.height / 2;
  await page.mouse.move(startX, startY);
  await page.mouse.down();
  await page.mouse.move(startX + dx, startY + dy, { steps: 10 });
  await page.mouse.up();
}

test.beforeEach(async ({ page }) => {
  // `frame` has real children (member-one/two) — App.tsx's
  // fold-everything-with-children-by-default would otherwise hide them on
  // first load, but this suite's own tests need them visible to drag.
  await disableDefaultFold(page, "root");
  await page.goto("/");
  await page.waitForSelector(".mesh-node");
  // This suite's whole point is drag-and-persist (see the fixture's own
  // doc comment): every successful run leaves `frame`/its members further
  // from wherever they started, and the app opens centered on root at a
  // fixed zoom, not fitted to content — without this, enough accumulated
  // drift across repeated local runs eventually pushes a member outside
  // the default viewport, and a drag on something not actually on screen
  // silently fails or lands on the wrong element.
  await clickFitViewAndWait(page);
  await page.getByRole("button", { name: "Edit" }).click();
});

test("dragging a single member only rewrites that member's own position", async ({ page }) => {
  // Deliberately not asserting a specific starting value: this suite's
  // whole point is drag-and-persist (see the fixture's own doc comment),
  // so re-running it (or sharing this same server/file across the
  // chrome/firefox projects in one `playwright test` invocation) leaves
  // the file genuinely mutated from whatever the previous run left it at
  // — the property under test is "did *this* drag change only what it
  // should", not any particular absolute number.
  const before = nodeMeta(await fetchRaw(page), "member-one");
  expect(before).not.toBeNull();
  const frameBefore = nodeMeta(await fetchRaw(page), "frame");
  const memberTwoBefore = nodeMeta(await fetchRaw(page), "member-two");

  await dragNodeTitle(page, "member-one", 60, 40);

  // Not an exact expected value: the drag's screen-pixel delta doesn't
  // necessarily map 1:1 to canvas pixels (zoom/devicePixelRatio) — the
  // property under test is *which* node's position moved, not by exactly
  // how much.
  await expect
    .poll(async () => nodeMeta(await fetchRaw(page), "member-one"), { timeout: 10_000 })
    .not.toEqual(before);
  expect(nodeMeta(await fetchRaw(page), "frame")).toEqual(frameBefore);
  expect(nodeMeta(await fetchRaw(page), "member-two")).toEqual(memberTwoBefore);
});

test("dragging the group's title bar only rewrites the group's own anchor", async ({ page }) => {
  const memberOneBefore = nodeMeta(await fetchRaw(page), "member-one");
  const memberTwoBefore = nodeMeta(await fetchRaw(page), "member-two");
  const frameBefore = nodeMeta(await fetchRaw(page), "frame");
  expect(frameBefore).not.toBeNull();

  await dragNodeTitle(page, "frame", 100, 50);

  await expect
    .poll(async () => nodeMeta(await fetchRaw(page), "frame"), { timeout: 10_000 })
    .not.toEqual(frameBefore);
  // The members' own relative offsets are untouched — React Flow moved
  // them visually along with the group natively (`parentId`), with
  // nothing of their own to persist.
  expect(nodeMeta(await fetchRaw(page), "member-one")).toEqual(memberOneBefore);
  expect(nodeMeta(await fetchRaw(page), "member-two")).toEqual(memberTwoBefore);
});
