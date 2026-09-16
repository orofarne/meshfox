import { test, expect, type Page } from "@playwright/test";
import { clickFitViewAndWait } from "./helpers";

// Drives web/e2e/fixtures/drag-reorder.canvas.md — `PutCanvasRequest::
// layout_hints`'s own end-to-end coverage: dragging one auto-placed
// sibling to a real position should slot it in among the *other*
// auto-placed siblings by where it visually lands, not always sort it
// first (any real number used to beat every unpositioned sibling's
// implicit `f64::INFINITY` in `mdcanvas::reorder_by_position`).

function fetchRaw(page: Page): Promise<string> {
  return page.evaluate(() => fetch("/api/canvas/raw").then((r) => r.text()));
}

function pos(raw: string, id: string): number {
  const at = raw.indexOf(`id="${id}"`);
  if (at < 0) throw new Error(`${id} not found in: ${raw}`);
  return at;
}

// Same drag mechanics as group-drag.spec.ts's own `dragNodeTitle`, just
// dragging to an absolute screen position instead of by a relative delta —
// this suite cares about *where on screen* the node ends up relative to
// its still-untouched siblings, not by how much it moved.
async function dragNodeTitleTo(page: Page, nodeId: string, targetX: number, targetY: number) {
  const titleText = page.locator(`.react-flow__node[data-id="${nodeId}"] .mesh-node-title-text`);
  const box = await titleText.boundingBox();
  if (!box) throw new Error(`${nodeId} title text has no box`);
  const startX = box.x + Math.min(10, box.width / 2);
  const startY = box.y + box.height / 2;
  await page.mouse.move(startX, startY);
  await page.mouse.down();
  await page.mouse.move(targetX, targetY, { steps: 10 });
  await page.mouse.up();
}

test.beforeEach(async ({ page }) => {
  await page.goto("/");
  await page.waitForSelector(".mesh-node");
  await clickFitViewAndWait(page);
  await page.getByRole("button", { name: "Edit" }).click();
});

test("dragging the last of three auto-placed siblings between the other two reorders it there, not to the front", async ({
  page,
}) => {
  const before = await fetchRaw(page);
  expect(pos(before, "alpha")).toBeLessThan(pos(before, "beta"));
  expect(pos(before, "beta")).toBeLessThan(pos(before, "gamma"));

  const alphaBox = await page.locator('.react-flow__node[data-id="alpha"] .mesh-node-title-text').boundingBox();
  const betaBox = await page.locator('.react-flow__node[data-id="beta"] .mesh-node-title-text').boundingBox();
  if (!alphaBox || !betaBox) throw new Error("alpha/beta title text has no box");
  // Roughly halfway down between alpha's and beta's own title rows —
  // squarely between the two on screen, regardless of exact spacing.
  const targetX = betaBox.x + betaBox.width / 2;
  const targetY = (alphaBox.y + betaBox.y + betaBox.height) / 2;

  await dragNodeTitleTo(page, "gamma", targetX, targetY);

  // Not "gamma always first" (the old, surprising behavior — any real y/x
  // used to beat both unpositioned siblings' implicit `f64::INFINITY`
  // regardless of where it was actually dropped) and not "unchanged either"
  // — specifically *between* alpha and beta, matching where it visually
  // landed.
  await expect
    .poll(
      async () => {
        const raw = await fetchRaw(page);
        return pos(raw, "alpha") < pos(raw, "gamma") && pos(raw, "gamma") < pos(raw, "beta");
      },
      { timeout: 10_000 },
    )
    .toBe(true);

  // `alpha`/`beta` themselves stay auto-placed — the hint that made this
  // possible is never persisted as a real x/y on either of them.
  const after = await fetchRaw(page);
  for (const id of ["alpha", "beta"]) {
    const line = after.slice(pos(after, id), after.indexOf("-->", pos(after, id)));
    expect(line).not.toContain(" x=");
    expect(line).not.toContain(" y=");
  }
});
