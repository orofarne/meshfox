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

test("only a positioned node shows the ↺ button", async ({ page }) => {
  await selectNode(node(page, "positioned"));
  await expect(clearLayoutButton(page)).toHaveCount(1);

  await selectNode(node(page, "auto"));
  await expect(clearLayoutButton(page)).toHaveCount(0);
});

test("clicking ↺ clears the node's authored position/size and preserves everything else", async ({ page }) => {
  const before = await fetchRaw(page);
  expect(before).toContain('x=200');
  expect(before).toContain('color="2"');

  await selectNode(node(page, "positioned"));
  await clearLayoutButton(page).click();

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
