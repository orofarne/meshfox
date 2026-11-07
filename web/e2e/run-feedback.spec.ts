import { test, expect } from "@playwright/test";

const taskSelector = '[id="mesh-block-root::long-task"]';

test("run acknowledges the click while arguments are being prepared", async ({ page }) => {
  await page.goto("/");
  const task = page.locator(taskSelector);
  let release!: () => void;
  const held = new Promise<void>(resolve => { release = resolve; });
  let requests = 0;
  await page.route("**/api/args", async route => {
    requests++;
    await held;
    await route.continue();
  });
  try {
    await task.getByRole("button", { name: "run long-task", exact: true }).click();
    await expect.poll(() => requests).toBe(1);
    await expect(task.locator('button[aria-busy="true"]')).toBeDisabled({ timeout: 1500 });
    await expect(task.locator('button[aria-busy="true"] .mesh-node-running-spinner')).toBeVisible();
    // A second DOM click must not enqueue another preparation request.
    await task.locator('button[aria-busy="true"]').evaluate((button: HTMLButtonElement) => button.click());
    expect(requests).toBe(1);
  } finally { release(); }
  await expect(task.getByRole("button", { name: "running long-task…", exact: true })).toBeDisabled();
  await expect(task.locator(".mesh-code-output")).toContainText("done", { timeout: 12_000 });
});

test("completed output never disappears when the run registry catches up", async ({ page }) => {
  await page.goto("/");
  const task = page.locator(taskSelector);
  await task.getByRole("button", { name: "run long-task", exact: true }).click();
  await expect(task.getByRole("button", { name: "running long-task…", exact: true })).toBeDisabled();
  await expect(task.locator(".mesh-code-output")).toContainText("done", { timeout: 12_000 });
  await expect(task.getByRole("button", { name: "run long-task", exact: true })).toBeEnabled();
  await page.evaluate(selector => {
    const task = document.querySelector(selector)!;
    const state = { missing: false };
    new MutationObserver(() => {
      if (!task.querySelector('.mesh-code-output')?.textContent?.includes('done')) state.missing = true;
    }).observe(task, { subtree: true, childList: true, characterData: true });
    Object.assign(window, { retainedOutput: state });
  }, taskSelector);
  // Cross the foreground stream's ten-second reconciliation grace period.
  // Real worker + real polling; an eventual assertion alone misses the flash.
  const observeUntil = Date.now() + 12_000;
  await expect.poll(async () => {
    await page.waitForResponse(response => response.url().endsWith('/api/runs'));
    return Date.now() >= observeUntil;
  }, { timeout: 17_000, intervals: [1000] }).toBe(true);
  expect(await page.evaluate(() => (window as unknown as { retainedOutput: { missing: boolean } }).retainedOutput.missing)).toBe(false);
});

test("a failed preparation releases the run button", async ({ page }) => {
  await page.goto("/");
  const task = page.locator(taskSelector);
  let release!: () => void;
  const held = new Promise<void>(resolve => { release = resolve; });
  await page.route("**/api/args", async route => {
    await held;
    await route.fulfill({ status: 500, body: "argument preparation failed" });
  });
  try {
    await task.getByRole("button", { name: "run long-task", exact: true }).click();
    await expect(task.locator('button[aria-busy="true"]')).toBeDisabled();
  } finally { release(); }
  await expect(page.getByText("Error: argument preparation failed", { exact: true })).toBeVisible();
  await expect(task.getByRole("button", { name: "run long-task", exact: true })).toBeEnabled();
  await expect(task.locator('button[aria-busy="true"]')).toHaveCount(0);
});
