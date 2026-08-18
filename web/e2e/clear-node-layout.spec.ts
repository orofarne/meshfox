import { test, expect, type Page } from "@playwright/test";
import { clickFitViewAndWait } from "./helpers";

// Drives web/e2e/fixtures/clear-node-layout.canvas.md — the web UI's ↺
// "reset to auto-layout" button, a positioned node's way to drop its own
// authored x/y/w/h and go back to auto-placement without touching the rest
// of the document (TODO.canvas.md: "Способ удалить координаты для
// конкретной ноды").

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

test("only a positioned node shows the ↺ button", async ({ page }) => {
  await node(page, "positioned").locator(".mesh-node-title").hover();
  await expect(node(page, "positioned").locator(".mesh-node-clear-layout")).toHaveCount(1);

  await node(page, "auto").locator(".mesh-node-title").hover();
  await expect(node(page, "auto").locator(".mesh-node-clear-layout")).toHaveCount(0);
});

test("clicking ↺ clears the node's authored position/size and preserves everything else", async ({ page }) => {
  const before = await fetchRaw(page);
  expect(before).toContain('x=200');
  expect(before).toContain('color="2"');

  await node(page, "positioned").locator(".mesh-node-title").hover();
  await node(page, "positioned").locator(".mesh-node-clear-layout").click();

  await expect
    .poll(() => fetchRaw(page).then((r) => !r.includes("x=200") && !r.includes("y=100")))
    .toBe(true);
  const raw = await fetchRaw(page);
  expect(raw).toContain('color="2"');
  expect(raw).toContain('tags="keep-me"');
  // The node itself is still there, now auto-placed — the client's own
  // layout gives it a box even with no authored position.
  await expect(node(page, "positioned")).toBeVisible();

  await page.evaluate(
    (raw) => fetch("/api/canvas/raw", { method: "PUT", headers: { "content-type": "text/plain" }, body: raw }),
    before,
  );
  expect(await fetchRaw(page)).toBe(before);
});
