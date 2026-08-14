import { test, expect, type Page } from "@playwright/test";
import { clickFitViewAndWait } from "./helpers";

// Drives web/e2e/fixtures/quick-run.canvas.md — checks the title bar's
// "▷ run" quick-run button (see MeshNode.tsx) routes a `tty`-flagged
// default block through `onRunTty` (opens a TtyPanel), not `onRun`
// (which the server rejects for a `tty` block over `/api/run`).

function node(page: Page, id: string) {
  return page.locator(`.react-flow__node[data-id="${id}"]`);
}

test.beforeEach(async ({ page }) => {
  await page.goto("/");
  await page.waitForSelector(".mesh-node");
  await clickFitViewAndWait(page);
});

test("quick-run on a tty default block opens a terminal, not an error", async ({ page }) => {
  await node(page, "monitor").locator(".mesh-node-quick-run-icon").click();

  await expect(page.locator(".mesh-tty-panel")).toBeVisible();
  await expect(page.locator(".error")).toHaveCount(0);

  await page.locator(".mesh-tty-head-actions button", { hasText: "✕" }).click();
});

test("quick-run on a plain (non-tty) default block still streams output the normal way", async ({ page }) => {
  await node(page, "plain").locator(".mesh-node-quick-run-icon").click();

  await expect(node(page, "plain").locator(".mesh-code-block")).toContainText("hello");
  await expect(page.locator(".mesh-tty-panel")).toHaveCount(0);
  await expect(page.locator(".error")).toHaveCount(0);
});
