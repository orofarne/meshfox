import { test, expect, type Page } from "@playwright/test";
import { clickFitViewAndWait, viewportTransform } from "./helpers";

// Drives web/e2e/fixtures/keyboard-nav.canvas.md, in the default read-only
// mode except for the one guard test that needs Edit mode — j/k/h/l/Enter
// canvas navigation is available either way (see App.tsx's keydown
// effect), modeled on the TUI's own key scheme (crates/cli/src/tui/app.rs).

function node(page: Page, id: string) {
  return page.locator(`.react-flow__node[data-id="${id}"]`);
}

function isFocused(page: Page, id: string) {
  return expect(node(page, id).locator(".mesh-node")).toHaveAttribute("data-focused", "true");
}

test.beforeEach(async ({ page }) => {
  await page.goto("/");
  await page.waitForSelector(".mesh-node");
  await clickFitViewAndWait(page);
});

test("j moves focus down through the document in order, k moves back up", async ({ page }) => {
  const order = ["root", "section-one", "child-a", "child-b", "section-two"];
  for (const id of order) {
    await page.keyboard.press("j");
    await isFocused(page, id);
  }
  for (const id of [...order].reverse().slice(1)) {
    await page.keyboard.press("k");
    await isFocused(page, id);
  }
});

test("h folds a focused node with children, then jumps focus to its parent", async ({ page }) => {
  await page.keyboard.press("j"); // root
  await page.keyboard.press("j"); // section-one
  await isFocused(page, "section-one");

  await page.keyboard.press("h");
  await expect(node(page, "child-a")).toHaveCount(0);
  await expect(node(page, "section-one").locator(".mesh-node")).toHaveAttribute("data-folded", "true");
  // section-one is still focused — folding it doesn't move focus by itself.
  await isFocused(page, "section-one");

  await page.keyboard.press("h");
  await isFocused(page, "root");
});

test("h folds a childless focused node too (there's still its own body to hide), then jumps focus to its parent", async ({ page }) => {
  for (let i = 0; i < 5; i++) await page.keyboard.press("j"); // (unfocused) -> root -> ... -> section-two
  await isFocused(page, "section-two");

  await page.keyboard.press("h");
  await expect(node(page, "section-two").locator(".mesh-node-body")).toHaveCount(0);
  await expect(node(page, "section-two").locator(".mesh-node")).toHaveAttribute("data-folded", "true");
  await isFocused(page, "section-two");

  await page.keyboard.press("h");
  await isFocused(page, "root");
});

test("h on a childless title-only node jumps straight to its parent (nothing to fold)", async ({ page }) => {
  for (let i = 0; i < 6; i++) await page.keyboard.press("j"); // ... -> title-only-leaf
  await isFocused(page, "title-only-leaf");

  await page.keyboard.press("h");
  await isFocused(page, "root");
});

test("h on a title-only node with a child still folds it (hiding the child, not its own row)", async ({ page }) => {
  for (let i = 0; i < 7; i++) await page.keyboard.press("j"); // ... -> title-only-parent
  await isFocused(page, "title-only-parent");

  await page.keyboard.press("h");
  await expect(node(page, "title-only-child")).toHaveCount(0);
  await expect(node(page, "title-only-parent").locator(".mesh-node")).toHaveAttribute("data-folded", "true");
  await isFocused(page, "title-only-parent");

  await page.keyboard.press("h");
  await isFocused(page, "root");
});

test("l unfolds a folded node without moving focus", async ({ page }) => {
  await page.keyboard.press("j"); // root
  await page.keyboard.press("j"); // section-one
  await page.keyboard.press("h"); // fold
  await expect(node(page, "child-a")).toHaveCount(0);

  await page.keyboard.press("l");
  await expect(node(page, "child-a")).toBeVisible();
  await isFocused(page, "section-one");
});

test("Enter toggles fold on the focused node", async ({ page }) => {
  await page.keyboard.press("j"); // root
  await page.keyboard.press("j"); // section-one

  await page.keyboard.press("Enter");
  await expect(node(page, "child-a")).toHaveCount(0);

  await page.keyboard.press("Enter");
  await expect(node(page, "child-a")).toBeVisible();
});

test("moving focus pans the camera to keep the focused node in view", async ({ page }) => {
  const before = await viewportTransform(page);

  for (let i = 0; i < 4; i++) await page.keyboard.press("j"); // root -> ... -> child-b

  await isFocused(page, "child-b");
  await expect.poll(() => viewportTransform(page)).not.toBe(before);
});

test("keys are ignored while the source editor is open", async ({ page }) => {
  await page.locator("button", { hasText: "Edit" }).click();
  await page.locator("button", { hasText: "Source" }).click();
  await expect(page.locator(".monaco-editor")).toBeVisible();

  await page.keyboard.press("j");

  await expect(page.locator('.mesh-node[data-focused="true"]')).toHaveCount(0);
});

// Regression test: unlike Source mode above (guarded by its own
// `sourceMode` flag), a node's own body editor (NodeTextEditor) has no
// equivalent App.tsx state — `isEditableTarget` is the *only* thing
// standing between typing here and j/k/h/l firing as canvas navigation,
// and it used to miss Monaco's EditContext-API input surface entirely
// (see App.tsx's own comment on `isEditableTarget`).
test("keys are ignored while a node's own body editor is open, and still reach the editor", async ({ page }) => {
  await page.locator("button", { hasText: "Edit" }).click();
  const root = page.locator('.react-flow__node[data-id="root"]');
  await root.locator(".mesh-node-title").hover();
  await root.locator('button[title="Edit this node\'s Markdown text"]').click();

  const source = page.locator(".mesh-text-editor-source .monaco-editor");
  await expect(source).toBeVisible();
  await source.locator(".view-lines").click();

  await page.keyboard.type("jkhl");

  await expect(page.locator('.mesh-node[data-focused="true"]')).toHaveCount(0);
  await expect(source).toContainText("jkhl");
});
