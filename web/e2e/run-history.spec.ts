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

test("a tty block shows its last run's status and a history entry, with no stored output", async ({ page }) => {
  const shell = node(page, "shell");
  await expect(shell.locator(".mesh-node-failed-badge")).toHaveCount(0);

  await shell.getByRole("button", { name: "run shell" }).click();
  await expect(page.locator(".mesh-tty-panel")).toContainText("exited (3)");
  await page.locator(".mesh-tty-head-actions button", { hasText: "✕" }).click();
  await expect(shell.locator(".mesh-node-failed-badge")).toBeVisible();
  // The outcome is also written under the block (a terminal session has no
  // output to show, but a successful run would otherwise leave no trace).
  await expect(shell.locator(".mesh-tty-status")).toContainText("exit 3");

  await shell.getByRole("button", { name: "Run history" }).click();
  const rows = dialog(page).getByRole("option");
  await expect(rows).toHaveCount(1);
  await expect(rows.first()).toContainText("exit 3");
  await expect(rows.first()).not.toContainText("stale");
  await expect(dialog(page).locator(".run-history-detail")).toContainText("a tty session's output isn't stored");
  await page.keyboard.press("Escape");

  // A fresh tab learns the status from the server, not from this one.
  await page.reload();
  await page.waitForSelector(".mesh-node");
  await clickFitViewAndWait(page);
  await expect(node(page, "shell").locator(".mesh-node-failed-badge")).toBeVisible();
  await expect(node(page, "shell").locator(".mesh-tty-status")).toContainText("exit 3");
});

test("a tty run made in another tab shows up here without a reload", async ({ page, context }) => {
  // `page` stays open and untouched; a second tab (standing in for the TUI or
  // a terminal session) runs the block.
  await expect(node(page, "watched").locator(".mesh-node-failed-badge")).toHaveCount(0);

  const other = await context.newPage();
  await other.goto("/");
  await other.waitForSelector(".mesh-node");
  await clickFitViewAndWait(other);
  await node(other, "watched").getByRole("button", { name: "run watched" }).click();
  await expect(other.locator(".mesh-tty-panel")).toContainText("exited (4)");

  await expect(node(page, "watched").locator(".mesh-node-failed-badge")).toBeVisible();
  await other.close();
});

test("a dependency run by another tab's chain shows up here too, not just the chain's target", async ({ page, context }) => {
  await expect(node(page, "dep-source").locator(".mesh-code-output")).toHaveCount(0);

  const other = await context.newPage();
  await other.goto("/");
  await other.waitForSelector(".mesh-node");
  await clickFitViewAndWait(other);
  await node(other, "chained").getByRole("button", { name: "⛓ run chain: chained" }).click();
  await expect(node(other, "chained").locator(".mesh-code-output")).toContainText("chained-ran");

  // `run-started` is only broadcast for the chain's target; the dependency
  // arrives via `runs-changed`.
  await expect(node(page, "dep-source").locator(".mesh-code-output")).toContainText("dep-ran-elsewhere");
  await expect(node(page, "chained").locator(".mesh-code-output")).toContainText("chained-ran");
  await other.close();
});
