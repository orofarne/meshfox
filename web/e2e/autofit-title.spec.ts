import { test, expect } from "@playwright/test";
import { clickFitViewAndWait } from "./helpers";

// Drives web/e2e/fixtures/autofit-title.canvas.md — a header-only node's
// own title shrinking to fit an explicit, authored `w`/`h` (MeshNode.tsx's
// `useAutoFitTitleFontSize`; TODO.canvas.md: "Автоскейл текста, если у
// ноды есть фиксированный размер").

function titleCenteredOf(page: import("@playwright/test").Page, id: string) {
  return page.locator(`.react-flow__node[data-id="${id}"] .mesh-node-title-centered`);
}

test.beforeEach(async ({ page }) => {
  await page.goto("/");
  await page.waitForSelector(".mesh-node");
  await clickFitViewAndWait(page);
});

test("a header-only node with a fixed size shrinks its title to fit, without overflowing", async ({ page }) => {
  const el = titleCenteredOf(page, "fixed-long-title");
  await expect(el).toBeVisible();

  await expect
    .poll(async () => el.evaluate((n) => n.scrollHeight - n.clientHeight))
    .toBeLessThanOrEqual(1);

  const fontPx = await el.evaluate((n) => parseFloat(getComputedStyle(n).fontSize));
  expect(fontPx).toBeLessThan(16);
  expect(fontPx).toBeGreaterThanOrEqual(10); // MIN_AUTOFIT_TITLE_PX — never shrinks past this
});

test("a header-only node whose title already fits at the default size isn't shrunk", async ({ page }) => {
  const el = titleCenteredOf(page, "fixed-short-title");
  await expect(el).toBeVisible();

  const fontPx = await el.evaluate((n) => parseFloat(getComputedStyle(n).fontSize));
  expect(fontPx).toBe(16); // the CSS default (`.mesh-node-title-centered`'s `1rem`), untouched
});

test("a header-only node without an authored size grows instead of shrinking", async ({ page }) => {
  const node = page.locator('.react-flow__node[data-id="auto-long-title"]');
  const el = titleCenteredOf(page, "auto-long-title");
  await expect(el).toBeVisible();

  const fontPx = await el.evaluate((n) => parseFloat(getComputedStyle(n).fontSize));
  expect(fontPx).toBe(16); // never shrunk — nothing here is a fixed box to fit

  const box = await node.boundingBox();
  // Well past `fixed-long-title`'s authored 70px — its box grew to fit the
  // same long title instead of shrinking text into a small one.
  expect(box?.height ?? 0).toBeGreaterThan(70);
});
