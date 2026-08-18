import { test, expect, type Page } from "@playwright/test";
import { clickFitViewAndWait } from "./helpers";

// Drives web/e2e/fixtures/move-sibling.canvas.md — the web UI's `↑`/`↓`
// sibling-reorder buttons, an auto-placed node's only lever for changing
// its own document/heading order (see TODO.canvas.md: "Механизм изменения
// порядка нод при авторазмещении"). A positioned node never gets the
// buttons — see `positioned-buttons-hidden` below — since it already gets
// a new order for free by being dragged (`reorder_by_position`).

function node(page: Page, id: string) {
  return page.locator(`.react-flow__node[data-id="${id}"]`);
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
  await node(page, "positioned").locator(".mesh-node-title").hover();
  await expect(node(page, "positioned").locator(".mesh-node-move-up")).toHaveCount(0);
  await expect(node(page, "positioned").locator(".mesh-node-move-down")).toHaveCount(0);
});

test("the topmost auto-placed sibling has no ↑ button, the bottommost has no ↓ button", async ({ page }) => {
  await node(page, "alpha").locator(".mesh-node-title").hover();
  await expect(node(page, "alpha").locator(".mesh-node-move-up")).toHaveCount(0);
  await expect(node(page, "alpha").locator(".mesh-node-move-down")).toHaveCount(1);

  await node(page, "gamma").locator(".mesh-node-title").hover();
  await expect(node(page, "gamma").locator(".mesh-node-move-down")).toHaveCount(0);
  await expect(node(page, "gamma").locator(".mesh-node-move-up")).toHaveCount(1);
});

test("clicking ↓ moves a node after its next sibling, even a positioned one", async ({ page }) => {
  const before = await fetchRaw(page);
  expect(before.indexOf('id="alpha"')).toBeLessThan(before.indexOf('id="positioned"'));

  await node(page, "alpha").locator(".mesh-node-title").hover();
  await node(page, "alpha").locator(".mesh-node-move-down").click();

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

  await node(page, "gamma").locator(".mesh-node-title").hover();
  await node(page, "gamma").locator(".mesh-node-move-up").click();

  await expect
    .poll(() => fetchRaw(page).then((r) => r.indexOf('id="gamma"') < r.indexOf('id="beta"')))
    .toBe(true);

  await page.evaluate(
    (raw) => fetch("/api/canvas/raw", { method: "PUT", headers: { "content-type": "text/plain" }, body: raw }),
    before,
  );
  expect(await fetchRaw(page)).toBe(before);
});
