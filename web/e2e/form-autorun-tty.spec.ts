import { test, expect, type Locator, type Page } from "@playwright/test";
import { clickFitViewAndWait } from "./helpers";

// Drives web/e2e/fixtures/form-autorun-tty.canvas.md — a `form`'s Send
// reaching a `tty` block that carries `autorun` (see SPEC.md's "Form
// fences" and "Interactive (`tty`) blocks"): the server starts the
// terminal on its own, this tab attaches to it without a click (the watch
// socket's `"tty-started"`), and Send is disabled for as long as that
// foreground run lasts — the server would refuse it (`409`) anyway.

// One server, shared by both browsers' projects: Firefox is skipped so
// the two never race each other's session.
test.skip(({ browserName }) => browserName === "firefox", "single shared server — Chrome only");

function block(page: Page, nodeId: string, blockName: string): Locator {
  return page.locator(`#mesh-block-${nodeId}\\:\\:${blockName}`);
}

test("Send starts the autorun terminal, opens it, and stays disabled until it exits", async ({ page }) => {
  await page.goto("/");
  await page.waitForSelector(".mesh-node");
  await clickFitViewAndWait(page);

  const form = block(page, "session", "session-form");
  const send = form.getByRole("button", { name: "Open" });
  await expect(send).toBeEnabled();

  await form.getByLabel("Name").fill("Ada");
  await send.click();

  // Nobody clicked run on the `tty` block — the terminal opens by itself.
  const panel = page.locator(".mesh-tty-panel");
  await expect(panel).toBeVisible();
  await expect(panel).toContainText("hello Ada");

  // A foreground run is in progress: Send is greyed out.
  await expect(send).toBeDisabled();

  // The program ends on its own; the form is usable again.
  await expect(panel).toContainText("bye");
  await expect(send).toBeEnabled({ timeout: 20_000 });
  await expect(panel.locator(".mesh-tty-head-status")).toHaveText("✓ Completed · exit 0");
  await expect(panel.locator(".mesh-tty-finished")).toContainText("Session finished");
  await panel.getByTitle("Minimize terminal", { exact: true }).click();
  await expect(page.locator(".mesh-tty-pill")).toContainText("✓ Completed · exit 0");
  await page.locator(".mesh-tty-pill").click();
  await panel.getByRole("button", { name: "Close", exact: true }).click();
  await expect(panel).toHaveCount(0);
});
