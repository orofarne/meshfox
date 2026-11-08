import { test, expect, type Locator, type Page } from "@playwright/test";
import { clickFitViewAndWait } from "./helpers";

// Drives web/e2e/fixtures/list-form.canvas.md — `type="list"` fields in a
// `form` (SPEC.md, "List variables"): checkboxes for a list with `choices`, a
// textarea (one element per line) for one without. Send commits the stored
// JSON array and the `autorun` block reads it in the format it asked for.

// One server, shared by both browsers' projects: Firefox is skipped.
test.skip(({ browserName }) => browserName === "firefox", "single shared server — Chrome only");

function block(page: Page, nodeId: string, blockName: string): Locator {
  return page.locator(`#mesh-block-${nodeId}\\:\\:${blockName}`);
}

test("checkboxes and a lines textarea fill list variables", async ({ page }) => {
  await page.goto("/");
  await page.waitForSelector(".mesh-node");
  await clickFitViewAndWait(page);

  const form = block(page, "lists", "pick");
  const show = block(page, "lists", "show");

  const regions = form.getByRole("group", { name: "Regions" });
  // The declared default is checked.
  const choice = (name: string) => regions.getByRole("checkbox", { name, exact: true });
  await expect(choice("eu")).toBeChecked();
  await expect(choice("us")).not.toBeChecked();

  await choice("us").check();
  await form.getByRole("textbox", { name: "Tags" }).fill("alpha\nAcme, Inc.\n\nbeta");
  await form.getByRole("button", { name: "Apply" }).click();

  // Checkbox order follows the declared choices; blank lines are dropped; the
  // block asked for `comma` and `space`, so it gets each in its own shape.
  const output = show.locator(".mesh-code-output");
  await expect(output).toContainText("regions=us,eu");
  await expect(output).toContainText("tags=alpha 'Acme, Inc.' beta");

  // Unchecking everything is an empty list, not a missing value.
  await choice("us").uncheck();
  await choice("eu").uncheck();
  await form.getByRole("button", { name: "Apply" }).click();
  await expect(output).toContainText("regions=");
  await expect(output).not.toContainText("regions=eu");
});
