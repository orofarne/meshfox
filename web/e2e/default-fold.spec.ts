import { test, expect, type Page } from "@playwright/test";
import { clickFitViewAndWait } from "./helpers";

// Drives web/e2e/fixtures/default-fold.canvas.md, which deliberately
// doesn't declare `unfold` — checks App.tsx's `resolveDefaultFold` picks
// the right default for each of its node shapes, and that a title-only
// node only gets a fold toggle (and only folds by default) when it has
// children of its own to hide (see MeshNode.tsx's `FoldToggle` render
// site, App.tsx's `canFold`).

function node(page: Page, id: string) {
  return page.locator(`.react-flow__node[data-id="${id}"]`);
}

test.beforeEach(async ({ page }) => {
  await page.goto("/");
  await page.waitForSelector(".mesh-node");
  await clickFitViewAndWait(page);
});

test("a plain leaf with a real body folds by default", async ({ page }) => {
  await expect(node(page, "plain-leaf").locator(".mesh-node")).toHaveAttribute("data-folded", "true");
  await expect(node(page, "plain-leaf").locator(".mesh-node-body")).toHaveCount(0);
});

test("a childless title-only (empty-bodied) node never folds and gets no fold toggle", async ({ page }) => {
  const titleOnly = node(page, "title-only").locator(".mesh-node");
  await expect(titleOnly).toHaveAttribute("data-folded", "false");
  await expect(node(page, "title-only").locator(".mesh-node-fold-toggle")).toHaveCount(0);
});

test("a title-only node with a child still folds by default (hiding the child, not its own row)", async ({
  page,
}) => {
  const parent = node(page, "title-only-with-child").locator(".mesh-node");
  await expect(parent).toHaveAttribute("data-folded", "true");
  await expect(node(page, "title-only-with-child").locator(".mesh-node-fold-toggle")).toBeVisible();
  await expect(node(page, "nested-under-title-only")).toHaveCount(0);

  await node(page, "title-only-with-child").locator(".mesh-node-fold-toggle").click();
  await expect(parent).toHaveAttribute("data-folded", "false");
  await expect(node(page, "nested-under-title-only")).toBeVisible();
});

test("a node with an authored size does not fold by default", async ({ page }) => {
  const sized = node(page, "sized-node").locator(".mesh-node");
  await expect(sized).toHaveAttribute("data-folded", "false");
  await expect(node(page, "sized-node").locator(".mesh-node-body")).toBeVisible();
});

test("root itself never folds by default", async ({ page }) => {
  await expect(node(page, "root").locator(".mesh-node")).toHaveAttribute("data-folded", "false");
});
