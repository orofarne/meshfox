import { test, expect, type Page } from "@playwright/test";
import { clickFitViewAndWait, selectNode } from "./helpers";

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

// The ↺ button now lives in the floating `NodeToolbar` above a *selected*
// node (see helpers.ts's `selectNode`), not inline in its title bar —
// global, not scoped under a specific node's own locator, same reasoning
// as `toolbarButton` there: `NodeToolbar` portals its content out of the
// node's own DOM subtree, and only ever renders one at a time.
function clearLayoutButton(page: Page) {
  return page.locator(".mesh-node-toolbar .mesh-node-clear-layout");
}

test.beforeEach(async ({ page }) => {
  await page.goto("/");
  await page.waitForSelector(".mesh-node");
  await clickFitViewAndWait(page);
  await page.getByRole("button", { name: "Edit" }).click();
});

test("a node with any authored x/y/w/h shows the ↺ button, a fully auto one doesn't", async ({ page }) => {
  await selectNode(node(page, "positioned"));
  await expect(clearLayoutButton(page)).toHaveCount(1);

  // Authored width/height only, no position at all (e.g. only ever
  // resized, never dragged) — still counts as "something to reset".
  await selectNode(node(page, "sized-only"));
  await expect(clearLayoutButton(page)).toHaveCount(1);

  await selectNode(node(page, "auto"));
  await expect(clearLayoutButton(page)).toHaveCount(0);
});

test("clicking ↺ on a size-only node clears its w/h even with no x/y to clear", async ({ page }) => {
  const before = await fetchRaw(page);
  expect(before).toContain('w=180');
  expect(before).toContain('h=90');

  await selectNode(node(page, "sized-only"));
  await clearLayoutButton(page).click();

  await expect.poll(() => fetchRaw(page).then((r) => !r.includes("w=180") && !r.includes("h=90"))).toBe(true);
  await expect(node(page, "sized-only")).toBeVisible();
  // The ↺ button itself disappears too, now that there's nothing left to clear.
  await selectNode(node(page, "sized-only"));
  await expect(clearLayoutButton(page)).toHaveCount(0);

  await page.evaluate(
    (raw) => fetch("/api/canvas/raw", { method: "PUT", headers: { "content-type": "text/plain" }, body: raw }),
    before,
  );
  expect(await fetchRaw(page)).toBe(before);
});

test("clicking ↺ clears the node's authored position/size and preserves everything else", async ({ page }) => {
  const before = await fetchRaw(page);
  expect(before).toContain('x=200');
  expect(before).toContain('y=100');
  expect(before).toContain('w=240');
  expect(before).toContain('h=120');
  expect(before).toContain('color="2"');

  await selectNode(node(page, "positioned"));
  await clearLayoutButton(page).click();

  await expect
    .poll(() => fetchRaw(page).then((r) => !r.includes("x=200") && !r.includes("y=100")))
    .toBe(true);
  const raw = await fetchRaw(page);
  // All four of x/y/w/h are dropped together, not just position — a
  // regression here (e.g. only clearing x/y and leaving a stale w/h
  // behind) would otherwise silently survive since `positioned` still
  // renders fine either way, sized by whatever it was last authored with.
  expect(raw).not.toContain("x=200");
  expect(raw).not.toContain("y=100");
  expect(raw).not.toContain("w=240");
  expect(raw).not.toContain("h=120");
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
