import { test, expect, type Page } from "@playwright/test";
import { clickFitViewAndWait } from "./helpers";

// Drives web/e2e/fixtures/fold.canvas.md, in the default read-only mode
// (never clicks "Edit") — folding is a view-only preference (see App.tsx's
// `foldedNodeIds`), available whether or not the canvas is being edited.

function node(page: Page, id: string) {
  return page.locator(`.react-flow__node[data-id="${id}"]`);
}

test.beforeEach(async ({ page }) => {
  await page.goto("/");
  await page.waitForSelector(".mesh-node");
  await clickFitViewAndWait(page);
});

test("folding a subtree hides its descendants and shows a compact header", async ({ page }) => {
  await expect(node(page, "child-one")).toBeVisible();
  await expect(node(page, "child-two")).toBeVisible();

  await node(page, "parent-node").locator(".mesh-node-fold-toggle").click();

  await expect(node(page, "child-one")).toHaveCount(0);
  await expect(node(page, "child-two")).toHaveCount(0);
  await expect(node(page, "parent-node").locator(".mesh-node")).toHaveAttribute("data-folded", "true");
  await expect(node(page, "parent-node").locator(".mesh-node-body")).toHaveCount(0);
});

test("unfolding restores the hidden descendants", async ({ page }) => {
  const toggle = node(page, "parent-node").locator(".mesh-node-fold-toggle");
  await toggle.click();
  await expect(node(page, "child-one")).toHaveCount(0);

  await toggle.click();

  await expect(node(page, "child-one")).toBeVisible();
  await expect(node(page, "child-two")).toBeVisible();
  await expect(node(page, "parent-node").locator(".mesh-node")).toHaveAttribute("data-folded", "false");
});

test("fold state survives a reload", async ({ page }) => {
  await node(page, "parent-node").locator(".mesh-node-fold-toggle").click();
  await expect(node(page, "child-one")).toHaveCount(0);

  await page.reload();
  await page.waitForSelector(".mesh-node");

  await expect(node(page, "child-one")).toHaveCount(0);
  await expect(node(page, "parent-node").locator(".mesh-node")).toHaveAttribute("data-folded", "true");
});

test("a childless node can be folded too, hiding its own body", async ({ page }) => {
  // `sibling-node` has no children — folding it has no subtree to hide,
  // but its own (possibly large) body still collapses to a title-only
  // row, same as a node with children. See MeshNode.tsx's `FoldToggle`:
  // it's rendered unconditionally, on every node.
  await expect(node(page, "sibling-node").locator(".mesh-node-body")).toBeVisible();

  await node(page, "sibling-node").locator(".mesh-node-fold-toggle").click();

  await expect(node(page, "sibling-node").locator(".mesh-node-body")).toHaveCount(0);
  await expect(node(page, "sibling-node").locator(".mesh-node")).toHaveAttribute("data-folded", "true");
});

test("clicking the title of a folded node unfolds it too, not just the toggle", async ({ page }) => {
  await node(page, "parent-node").locator(".mesh-node-fold-toggle").click();
  await expect(node(page, "child-one")).toHaveCount(0);

  await node(page, "parent-node").locator(".mesh-node-title-text").click();

  await expect(node(page, "child-one")).toBeVisible();
  await expect(node(page, "parent-node").locator(".mesh-node")).toHaveAttribute("data-folded", "false");
});

test("clicking the title of an already-unfolded node does nothing (doesn't fold it)", async ({ page }) => {
  await expect(node(page, "parent-node").locator(".mesh-node")).toHaveAttribute("data-folded", "false");

  await node(page, "parent-node").locator(".mesh-node-title-text").click();

  await expect(node(page, "parent-node").locator(".mesh-node")).toHaveAttribute("data-folded", "false");
  await expect(node(page, "child-one")).toBeVisible();
});

test("an auto-placed sibling reflows upward when a preceding subtree folds", async ({ page }) => {
  const before = await node(page, "sibling-node").boundingBox();
  if (!before) throw new Error("sibling-node has no box");

  await node(page, "parent-node").locator(".mesh-node-fold-toggle").click();

  await expect
    .poll(async () => (await node(page, "sibling-node").boundingBox())?.y ?? null)
    .toBeLessThan(before.y);
});
