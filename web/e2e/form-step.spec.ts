import { test, expect, type Locator, type Page } from "@playwright/test";
import { clickFitViewAndWait } from "./helpers";

// Drives web/e2e/fixtures/form-step.canvas.md — a `form` fence as a step of
// a run chain (see SPEC.md's "Form fences"): the run parks on it, the form
// shows that a run is waiting (and stays sendable although a run is in
// progress), Send continues the run, Cancel fails it.

// One server, shared by both browsers' projects: Firefox is skipped so the
// two never race each other's parked run.
test.skip(({ browserName }) => browserName === "firefox", "single shared server — Chrome only");

function block(page: Page, nodeId: string, blockName: string): Locator {
  return page.locator(`#mesh-block-${nodeId}\\:\\:${blockName}`);
}

test.beforeEach(async ({ page }) => {
  await page.goto("/");
  await page.waitForSelector(".mesh-node");
  await clickFitViewAndWait(page);
});

test("Cancel fails the waiting run", async ({ page }) => {
  const form = block(page, "flow", "review");
  const finish = block(page, "flow", "finish");

  await finish.locator(".mesh-run-chain").click();
  await expect(form.getByText("A run is waiting for this form.")).toBeVisible();

  await form.getByRole("button", { name: "Stop the run" }).click();

  await expect(form.getByText("A run is waiting for this form.")).toHaveCount(0);
  await expect(page.getByText(/was cancelled/).first()).toBeVisible();
  await expect(finish.locator(".mesh-code-output").filter({ hasText: "result-is-yes" })).toHaveCount(0);
});

test("a run parks on the form step and Send continues it", async ({ page }) => {
  const form = block(page, "flow", "review");
  const finish = block(page, "flow", "finish");

  await finish.locator(".mesh-run-chain").click();

  await expect(form.getByText("A run is waiting for this form.")).toBeVisible();
  await expect(form.getByText("Check the file, then continue")).toBeVisible();
  // The run is in progress, yet this form is the one it waits for.
  const send = form.getByRole("button", { name: "Continue" });
  await expect(send).toBeEnabled();
  // `finish` has not run yet (its source also contains "result-is-", so look
  // at the output element only).
  await expect(finish.locator(".mesh-code-output").filter({ hasText: "result-is-yes" })).toHaveCount(0);

  await form.getByLabel("Answer").fill("yes");
  await send.click();

  await expect(finish.locator(".mesh-code-output")).toContainText("result-is-yes");
  await expect(form.getByText("A run is waiting for this form.")).toHaveCount(0);
});
