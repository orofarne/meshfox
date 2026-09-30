import { test, expect, type Page } from "@playwright/test";
import { clickFitViewAndWait } from "./helpers";

// Drives web/e2e/fixtures/run-history.canvas.md — the block header's clock
// button opens a modal (RunHistoryDialog.tsx) listing the finished runs the
// server keeps for a block, showing a run's stored output, and flagging every
// run stale once the block's code changes; and a reloaded tab gets the last
// run's output back.
//
// Chromium only: the server (and its session database) is shared between
// browser projects, and this suite rewrites the block's code, so a second
// browser would start from the first one's leftovers. Tests run in order.

function node(page: Page, id: string) {
  return page.locator(`.react-flow__node[data-id="${id}"]`);
}

function dialog(page: Page) {
  return page.getByRole("dialog", { name: /Run history/ });
}

test.beforeEach(async ({ page, browserName }) => {
  test.skip(browserName !== "chromium", "shares a stateful server between browsers");
  await page.goto("/");
  await page.waitForSelector(".mesh-node");
  await clickFitViewAndWait(page);
});

test.describe.configure({ mode: "serial" });

test("a finished run shows up in the history modal with its output", async ({ page }) => {
  const greet = node(page, "greet");
  await greet.getByRole("button", { name: "run greet" }).click();
  await expect(greet.locator(".mesh-code-output")).toContainText("first-run");

  await greet.getByRole("button", { name: "Run history" }).click();
  await expect(dialog(page)).toBeVisible();
  const rows = dialog(page).getByRole("option");
  await expect(rows).toHaveCount(1);
  await expect(rows.first()).toContainText("exit 0");
  await expect(rows.first()).not.toContainText("stale");
  // The newest run is selected on open, so its output is already showing.
  await expect(dialog(page).locator(".run-history-detail")).toContainText("first-run");

  await page.keyboard.press("Escape");
  await expect(dialog(page)).toHaveCount(0);

  // A second run shows up as a second row.
  await greet.getByRole("button", { name: "run greet" }).click();
  await expect(greet.locator(".mesh-code-output")).toContainText("first-run");
  await greet.getByRole("button", { name: "Run history" }).click();
  await expect(dialog(page).getByRole("option")).toHaveCount(2);
});

test("a reloaded tab gets the last run's output back", async ({ page }) => {
  // The previous test's runs are still on the server; a fresh page starts
  // with no client-side run state at all.
  await expect(node(page, "greet").locator(".mesh-code-output")).toContainText("first-run");
});

test("editing the block marks its earlier runs stale", async ({ page }) => {
  const greet = node(page, "greet");
  const res = await page.request.patch("/api/nodes/greet", {
    data: { text: "```bash name=\"greet\"\necho changed\n```\n" },
  });
  expect(res.ok()).toBeTruthy();
  await page.reload();
  await page.waitForSelector(".mesh-node");
  await clickFitViewAndWait(page);

  // The edited block's old output isn't offered as its latest result...
  await expect(greet.locator(".mesh-code-output")).toHaveCount(0);
  // ...but the runs are still in the history, flagged.
  await greet.getByRole("button", { name: "Run history" }).click();
  const rows = dialog(page).getByRole("option");
  await expect(rows.first()).toBeVisible();
  const total = await rows.count();
  await expect(dialog(page).locator(".run-history-row .run-history-stale")).toHaveCount(total);
});
