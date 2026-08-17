import { test, expect, type Locator, type Page } from "@playwright/test";
import { clickFitViewAndWait } from "./helpers";

// Drives web/e2e/fixtures/vars-form.canvas.md — regression coverage for a
// bug where the "Configure variables" modal's <select> rendered with zero
// options for a `choices_var`-declared variable whose reference chain
// reaches a `from=`-computed one: GET /api/vars never executes anything,
// so the choices could never be known ahead of a real run. See
// crates/server/src/lib.rs's `materialize_choices_and_defaults`.

function block(page: Page, nodeId: string, blockName: string): Locator {
  // `#mesh-block-<node>::<block>` — the `:` needs CSS-escaping in a
  // selector string, see web/src/deps.ts's `blockDomId`.
  return page.locator(`#mesh-block-${nodeId}\\:\\:${blockName}`);
}

test.beforeEach(async ({ page }) => {
  await page.goto("/");
  await page.waitForSelector(".mesh-node");
  await clickFitViewAndWait(page);
});

test("the vars modal offers a choices_var-declared select's real, from=-computed options", async ({ page }) => {
  await block(page, "dynamic", "use-region").locator("button", { hasText: "run" }).click();

  const modal = page.locator(".vars-modal");
  await expect(modal).toBeVisible();
  const select = modal.locator("select");
  // The reported bug: this rendered with zero <option>s, since the
  // choices_var chain through REGIONS_LIST (from=-computed) could never
  // resolve during a mere status check.
  await expect(select.locator("option")).toHaveCount(2);
  await expect(select.locator("option")).toHaveText(["us-east-1", "eu-west-1"]);

  await select.selectOption("eu-west-1");
  await modal.locator("button", { hasText: "run" }).click();

  await expect(modal).toHaveCount(0);
  await expect(block(page, "dynamic", "use-region").locator(".mesh-code-output")).toContainText(
    "using eu-west-1",
  );
});
