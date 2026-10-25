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

test("the vars modal browses for a type=file variable and runs with the chosen path", async ({ page }) => {
  await block(page, "clips", "use-clip").locator("button", { hasText: "run" }).click();

  const modal = page.locator(".vars-modal:not(.path-picker)");
  await expect(modal).toBeVisible();
  await modal.locator(".path-field-browse").click();

  const picker = page.locator(".path-picker");
  await expect(picker).toBeVisible();
  await picker.locator(".path-picker-entry", { hasText: "path-picker-data" }).click();
  await picker.locator(".path-picker-entry", { hasText: "footage" }).click();
  await picker.locator(".path-picker-entry", { hasText: "clip.mp4" }).click();
  await picker.getByRole("button", { name: "choose", exact: true }).click();
  await expect(picker).toHaveCount(0);

  // The modal is still open, holding the picked absolute path.
  await expect(modal.locator(".path-field input")).toHaveValue(/path-picker-data\/footage\/clip\.mp4$/);
  await modal.locator("button", { hasText: "run" }).click();

  await expect(modal).toHaveCount(0);
  await expect(block(page, "clips", "use-clip").locator(".mesh-code-output")).toContainText(
    /clip is .*path-picker-data\/footage\/clip\.mp4/,
  );
});

test("the vars modal takes a date and a datetime with an offset and runs with canonical values", async ({ page }) => {
  await block(page, "when", "use-when").locator("button", { hasText: "run" }).click();

  const modal = page.locator(".vars-modal");
  await expect(modal).toBeVisible();
  const day = modal.locator('input[type="date"]');
  const at = modal.locator('input[type="datetime-local"]');
  const offset = modal.locator(".date-field-offset");
  await expect(day).toBeVisible();
  await expect(at).toBeVisible();
  // Before anything is picked the offset box already holds the browser's own.
  await expect(offset).toHaveValue(/^(Z|[+-]\d{2}:\d{2})$/);

  await day.fill("2026-10-25");
  await at.fill("2026-10-25T14:30:15");
  await offset.fill("+03:00");
  await modal.locator("button", { hasText: "run" }).click();

  await expect(modal).toHaveCount(0);
  await expect(block(page, "when", "use-when").locator(".mesh-code-output")).toContainText(
    "when 2026-10-25 2026-10-25T14:30:15+03:00",
  );
});

test("the vars modal refuses a datetime with a malformed offset and keeps itself open", async ({ page }) => {
  await block(page, "when", "use-when").locator("button", { hasText: "run" }).click();

  const modal = page.locator(".vars-modal");
  await modal.locator('input[type="date"]').fill("2026-10-25");
  await modal.locator('input[type="datetime-local"]').fill("2026-10-25T14:30");
  await modal.locator(".date-field-offset").fill("3pm");
  await modal.locator("button", { hasText: "run" }).click();

  await expect(modal).toBeVisible();
  await expect(modal.locator(".vars-modal-error")).toContainText("needs a date and time with an offset");
});

test("a file node's code preview re-reads its target after a run writes it", async ({ page }) => {
  const preview = page.locator('.react-flow__node[data-id="report-file"]');
  const text = async () => (await preview.innerText()).replace(/\s+/g, " ");

  await block(page, "report", "write-report").locator("button", { hasText: "run" }).click();
  await expect.poll(text).toMatch(/report-\d+/);
  const first = (await text()).match(/report-\d+/)![0];

  await block(page, "report", "write-report").locator("button", { hasText: "run" }).click();
  // A new run writes a new token; the preview must pick it up without a reload.
  await expect.poll(async () => (await text()).match(/report-\d+/)?.[0]).not.toBe(first);
});
