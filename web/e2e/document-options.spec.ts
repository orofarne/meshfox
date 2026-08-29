import { test, expect, type Page } from "@playwright/test";

// Drives web/e2e/fixtures/document-options.canvas.md — checks the
// toolbar's "options" modal (DocumentOptions.tsx) round-trips
// document-wide `meshfox:option` declarations through `PUT /api/options`
// (see SPEC.md's "Options"). Doesn't test `unfold`'s actual effect on
// fold state — see this suite's fixture comment for why that can't be
// observed live within one already-loaded session.
//
// The "⚙ options" button only shows in Edit mode (same as Auto-layout,
// Source), so every test here enters it first.

function optionsButton(page: Page) {
  return page.getByRole("button", { name: "⚙ options" });
}

function unfoldCheckbox(page: Page) {
  return page.getByLabel("Expand everything by default");
}

test.beforeEach(async ({ page }) => {
  await page.goto("/");
  await page.waitForSelector(".mesh-node");
  await page.getByRole("button", { name: "Edit" }).click();
});

test("shows the current declared options, including one this version doesn't recognize", async ({ page }) => {
  await optionsButton(page).click();
  const modal = page.locator(".vars-modal");
  await expect(modal).toBeVisible();
  await expect(unfoldCheckbox(page)).not.toBeChecked();
  await expect(modal).toContainText("custom-future-option");
});

test("checking unfold and saving declares it, and it round-trips on reopen", async ({ page }) => {
  await optionsButton(page).click();
  await unfoldCheckbox(page).check();
  await page.getByRole("button", { name: "save" }).click();
  await expect(page.locator(".vars-modal")).toHaveCount(0);

  await optionsButton(page).click();
  await expect(unfoldCheckbox(page)).toBeChecked();

  // undo, so this test doesn't leave the fixture file modified for
  // whichever test runs next in this same worker
  await unfoldCheckbox(page).uncheck();
  await page.getByRole("button", { name: "save" }).click();
});

test("cancel discards the change", async ({ page }) => {
  await optionsButton(page).click();
  await unfoldCheckbox(page).check();
  await page.getByRole("button", { name: "cancel" }).click();

  await optionsButton(page).click();
  await expect(unfoldCheckbox(page)).not.toBeChecked();
});

test("unchecking a previously-saved option removes its declaration", async ({ page }) => {
  await optionsButton(page).click();
  await unfoldCheckbox(page).check();
  await page.getByRole("button", { name: "save" }).click();

  await optionsButton(page).click();
  await unfoldCheckbox(page).uncheck();
  await page.getByRole("button", { name: "save" }).click();

  await optionsButton(page).click();
  await expect(unfoldCheckbox(page)).not.toBeChecked();
});
