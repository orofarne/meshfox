import { test, expect, type Page } from "@playwright/test";
import { clickFitViewAndWait, selectNode } from "./helpers";

// Drives web/e2e/fixtures/move-sibling.canvas.md — the web UI's `↑`/`↓`
// sibling-reorder buttons, an auto-placed node's only lever for changing
// its own document/heading order (see TODO.canvas.md: "Механизм изменения
// порядка нод при авторазмещении"). A positioned node never gets the
// buttons — see `positioned-buttons-hidden` below — since it already gets
// a new order for free by being dragged (`reorder_by_position`).

function node(page: Page, id: string) {
  return page.locator(`.react-flow__node[data-id="${id}"]`);
}

// The `↑`/`↓` buttons now live in the floating `NodeToolbar` above a
// *selected* node (see helpers.ts's `selectNode`), not inline in its
// title bar — global, not scoped under a specific node's own locator,
// for the same reason `toolbarButton` there isn't either: `NodeToolbar`
// portals its content out of the node's own DOM subtree, and only ever
// renders one at a time (whichever node is currently selected).
function moveUpButton(page: Page) {
  return page.locator(".mesh-node-toolbar .mesh-node-move-up");
}
function moveDownButton(page: Page) {
  return page.locator(".mesh-node-toolbar .mesh-node-move-down");
}

function fetchRaw(page: Page): Promise<string> {
  return page.evaluate(() => fetch("/api/canvas/raw").then((r) => r.text()));
}

test.beforeEach(async ({ page }) => {
  await page.goto("/");
  await page.waitForSelector(".mesh-node");
  await clickFitViewAndWait(page);
  await page.getByRole("button", { name: "Edit" }).click();
});

test("a positioned node never shows the ↑/↓ buttons, even hovered in edit mode", async ({ page }) => {
  await selectNode(node(page, "positioned"));
  await expect(moveUpButton(page)).toHaveCount(0);
  await expect(moveDownButton(page)).toHaveCount(0);
});

test("the topmost auto-placed sibling has no ↑ button, the bottommost has no ↓ button", async ({ page }) => {
  await selectNode(node(page, "alpha"));
  await expect(moveUpButton(page)).toHaveCount(0);
  await expect(moveDownButton(page)).toHaveCount(1);

  await selectNode(node(page, "gamma"));
  await expect(moveDownButton(page)).toHaveCount(0);
  await expect(moveUpButton(page)).toHaveCount(1);
});

test("clicking ↓ moves a node after its next sibling, even a positioned one", async ({ page }) => {
  const before = await fetchRaw(page);
  expect(before.indexOf('id="alpha"')).toBeLessThan(before.indexOf('id="positioned"'));

  await selectNode(node(page, "alpha"));
  await moveDownButton(page).click();

  await expect.poll(() => fetchRaw(page).then((r) => r.indexOf('id="alpha"') > r.indexOf('id="positioned"'))).toBe(
    true,
  );

  await page.evaluate(
    (raw) => fetch("/api/canvas/raw", { method: "PUT", headers: { "content-type": "text/plain" }, body: raw }),
    before,
  );
  expect(await fetchRaw(page)).toBe(before);
});

test("clicking ↑ moves a node before its previous sibling", async ({ page }) => {
  const before = await fetchRaw(page);
  expect(before.indexOf('id="beta"')).toBeLessThan(before.indexOf('id="gamma"'));

  await selectNode(node(page, "gamma"));
  await moveUpButton(page).click();

  await expect
    .poll(() => fetchRaw(page).then((r) => r.indexOf('id="gamma"') < r.indexOf('id="beta"')))
    .toBe(true);

  await page.evaluate(
    (raw) => fetch("/api/canvas/raw", { method: "PUT", headers: { "content-type": "text/plain" }, body: raw }),
    before,
  );
  expect(await fetchRaw(page)).toBe(before);
});
