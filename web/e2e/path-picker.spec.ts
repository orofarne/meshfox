import { test, expect, type Locator, type Page } from "@playwright/test";
import { clickFitViewAndWait } from "./helpers";

// Drives web/e2e/fixtures/path-picker.canvas.md — the "Browse…" file/
// directory chooser of `type="file"`/`type="dir"` form fields. It browses
// the *worker's* file system through `GET /api/fs/list`; the tree it walks
// is web/e2e/fixtures/path-picker-data/.

function field(page: Page, label: string): Locator {
  return page.locator(".mesh-form-block .vars-modal-field", { hasText: label });
}

function picker(page: Page): Locator {
  return page.locator(".path-picker");
}

function entry(page: Page, name: string): Locator {
  return picker(page).locator(".path-picker-entry", { hasText: name });
}

test.beforeEach(async ({ page }) => {
  await page.goto("/");
  await page.waitForSelector(".mesh-form-block .path-field");
  await clickFitViewAndWait(page);
});

test("an open-mode file field picks an existing file, hints by extension, and hides dotfiles", async ({ page }) => {
  await field(page, "Input video").locator(".path-field-browse").click();
  await expect(picker(page)).toBeVisible();

  await entry(page, "path-picker-data").click();
  await entry(page, "footage").click();

  // `ext="mp4,mkv"` narrows the files; the directory list is untouched.
  await expect(entry(page, "clip.mp4")).toBeVisible();
  await expect(entry(page, "interview.mkv")).toBeVisible();
  await expect(entry(page, "notes.txt")).toHaveCount(0);
  await expect(entry(page, ".hidden.mp4")).toHaveCount(0);

  // The hint is switchable, never a restriction.
  await picker(page).getByLabel(/only/).uncheck();
  await expect(entry(page, "notes.txt")).toBeVisible();
  await picker(page).getByLabel("hidden").check();
  await expect(entry(page, ".hidden.mp4")).toBeVisible();

  await entry(page, "clip.mp4").click();
  await picker(page).getByRole("button", { name: "choose", exact: true }).click();
  await expect(picker(page)).toHaveCount(0);
  await expect(field(page, "Input video").locator("input")).toHaveValue(/path-picker-data\/footage\/clip\.mp4$/);
});

test("a save-mode file field takes a new name in a chosen directory", async ({ page }) => {
  await field(page, "Output file").locator(".path-field-browse").click();
  await entry(page, "path-picker-data").click();
  await entry(page, "exports").click();

  await picker(page).getByLabel("File name").fill("result.mp4");
  await picker(page).getByRole("button", { name: "choose", exact: true }).click();
  await expect(field(page, "Output file").locator("input")).toHaveValue(/path-picker-data\/exports\/result\.mp4$/);
});

test("a dir field chooses the directory it is browsing", async ({ page }) => {
  await field(page, "Work directory").locator(".path-field-browse").click();
  await entry(page, "path-picker-data").click();
  await entry(page, "exports").click();
  // Files are not listed for a directory picker.
  await expect(picker(page).locator(".path-picker-entry:not(.is-dir)")).toHaveCount(0);

  await picker(page).getByRole("button", { name: "choose this folder" }).click();
  await expect(field(page, "Work directory").locator("input")).toHaveValue(/path-picker-data\/exports$/);
});

test("the picker starts at the field's current value and Escape closes it without a change", async ({ page }) => {
  const input = field(page, "Input video").locator("input");
  await input.fill("path-picker-data/footage/interview.mkv");
  await field(page, "Input video").locator(".path-field-browse").click();

  // A file value opens its directory with the file pre-selected.
  await expect(entry(page, "interview.mkv")).toHaveClass(/is-picked/);
  await page.keyboard.press("Escape");
  await expect(picker(page)).toHaveCount(0);
  await expect(input).toHaveValue("path-picker-data/footage/interview.mkv");
});
