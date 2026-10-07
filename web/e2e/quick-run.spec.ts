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

test("quick-run on a default block with deps runs its dependency chain first, not just the block itself", async ({ page }) => {
  await node(page, "chained").locator(".mesh-node-quick-run-icon").click();

  await expect(node(page, "dep-source").locator(".mesh-code-block")).toContainText("dep-ran");
  await expect(node(page, "chained").locator(".mesh-code-block")).toContainText("chained-ran");
  await expect(page.locator(".error")).toHaveCount(0);
});

test("confirm gates dependencies, cancellation and every repeated run", async ({ page }, testInfo) => {
  const target = node(page, "confirm-consumer");
  const modal = page.getByRole("dialog", { name: "Confirm run" });
  const nativeDialogs: string[] = [];
  page.on("dialog", async dialog => { nativeDialogs.push(dialog.message()); await dialog.dismiss(); });
  await target.locator(".mesh-node-quick-run-icon").click();
  await expect(modal).toBeVisible();
  await expect(modal).toContainText("confirm-cleanup/confirm-cleanup");
  await expect(modal.getByRole("button", { name: "cancel", exact: true })).toBeFocused();
  await expect(target.locator(".mesh-code-output")).toHaveCount(0);
  await page.screenshot({ path: testInfo.outputPath("confirm-run.png") });
  await page.keyboard.press("Enter");
  await expect(modal).toHaveCount(0);
  await expect(target.locator(".mesh-code-output")).toHaveCount(0);
  await target.locator(".mesh-node-quick-run-icon").click();
  await expect(modal).toBeVisible();
  await modal.getByRole("button", { name: "run", exact: true }).click();
  await expect(modal).toHaveCount(0);
  await expect(target.locator(".mesh-code-output")).toContainText("confirm-consumer-ran");
  await page.locator(".console-panel-close").click();
  await target.locator(".mesh-node-quick-run-icon").click();
  await expect(modal).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(modal).toHaveCount(0);
  await expect(page.locator(".error")).toHaveCount(0);
  expect(nativeDialogs).toEqual([]);
});

test("confirm tty waits for approval before opening a terminal", async ({ page }) => {
  const modal = page.getByRole("dialog", { name: "Confirm run" });
  const button = node(page, "confirm-terminal").locator(".mesh-node-quick-run-icon");
  await button.click();
  await expect(modal).toBeVisible();
  await expect(modal).toContainText("confirm-terminal/confirm-terminal");
  await expect(page.locator(".mesh-tty-panel")).toHaveCount(0);
  await modal.getByRole("button", { name: "cancel", exact: true }).click();
  await expect(modal).toHaveCount(0);
  await button.click();
  await modal.getByRole("button", { name: "run", exact: true }).click();
  await expect(page.locator(".mesh-tty-panel")).toBeVisible();
  await expect(page.locator(".mesh-tty-panel")).toContainText("confirm-terminal-ran");
  await page.locator(".mesh-tty-head-actions button", { hasText: "✕" }).click();
});

test("confirmation traps focus and backdrop cancellation does not run a block", async ({ page }) => {
  const launches: string[] = [];
  page.on("websocket", socket => {
    if (new URL(socket.url()).pathname === "/api/run") launches.push(socket.url());
  });
  const target = node(page, "confirm-cleanup");
  const modal = page.getByRole("dialog", { name: "Confirm run" });
  await target.locator(".mesh-node-quick-run-icon").click();
  await expect(modal).toBeVisible();
  await page.keyboard.press("Shift+Tab");
  await expect(modal.getByRole("button", { name: "run", exact: true })).toBeFocused();
  await page.keyboard.press("Tab");
  await expect(modal.getByRole("button", { name: "cancel", exact: true })).toBeFocused();
  await page.locator(".vars-modal-backdrop").click({ position: { x: 5, y: 5 } });
  await expect(modal).toHaveCount(0);
  await expect(target.locator(".mesh-node-quick-run-icon")).toBeFocused();
  expect(launches).toEqual([]);
});
