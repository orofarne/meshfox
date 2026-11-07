import { test, expect, type Locator, type Page } from "@playwright/test";
import { clickFitViewAndWait } from "./helpers";

// Drives web/e2e/fixtures/choices.canvas.md: a `select` fed by a computed
// list is a filterable list (the browser counterpart of the TUI picker), and a
// field whose list could not be computed says so and why.

function block(page: Page, nodeId: string, blockName: string): Locator {
  return page.locator(`#mesh-block-${nodeId}\\:\\:${blockName}`);
}

test.beforeEach(async ({ page }) => {
  await page.goto("/");
  await page.waitForSelector(".mesh-node");
  await clickFitViewAndWait(page);
});

test("a select filters its choices as you type and takes the same keys as the TUI picker", async ({ page }) => {
  await block(page, "fonts", "use-font").locator("button", { hasText: "run" }).click();
  const modal = page.locator(".vars-modal");
  const select = modal.getByRole("combobox", { name: "Font?", exact: true });
  await select.click();
  const filter = modal.getByRole("textbox", { name: "Filter Font?" });
  const options = modal.getByRole("option");
  await expect(options).toHaveCount(16);

  // Case-insensitive substring, like `meshfox_core::filter_choices`.
  await filter.fill("MONO");
  await expect(options).toHaveText(["DejaVu Sans Mono", "Noto Mono"]);
  await filter.fill("nothing like this");
  await expect(modal.getByText("Nothing matches")).toBeVisible();

  // Ctrl-U clears the text, then Esc: the first press clears, the second
  // closes the list — and never the dialog around it.
  await filter.press("Control+u");
  await expect(options).toHaveCount(16);
  await filter.fill("noto");
  await filter.press("Escape");
  await expect(filter).toHaveValue("");
  await expect(options).toHaveCount(16);
  await filter.press("Escape");
  await expect(filter).toHaveCount(0);
  await expect(modal).toBeVisible();

  // Arrow keys move (wrapping), Enter picks without submitting the form.
  await select.click();
  await filter.fill("noto");
  await filter.press("ArrowDown");
  await filter.press("ArrowDown");
  await filter.press("ArrowDown");
  await expect(modal.getByRole("option", { selected: true })).toHaveText("Noto Mono");
  await filter.press("ArrowUp");
  await filter.press("Enter");
  await expect(modal).toBeVisible();
  await expect(select).toContainText("Noto Serif");

  await modal.locator("button", { hasText: "run" }).click();
  await expect(modal).toHaveCount(0);
  await expect(block(page, "fonts", "use-font").locator(".mesh-code-output")).toContainText("font is Noto Serif");
});

test("a select whose choices could not be computed says why instead of listing nothing", async ({ page }) => {
  await block(page, "broken", "use-broken").locator("button", { hasText: "run" }).click();
  const modal = page.locator(".vars-modal");
  await expect(modal).toBeVisible();

  const select = modal.getByRole("combobox", { name: "Broken font?", exact: true });
  await expect(select).toBeDisabled();
  const note = modal.getByRole("note");
  await expect(note).toContainText("BROKEN_LIST");
  await expect(note).toContainText("list-broken");
  await expect(note).toContainText("exited with status 3");
  await expect(note).toContainText("no fontconfig here");

  // Nothing can be answered, so the run does not go on.
  await modal.locator("button", { hasText: "run" }).click();
  await expect(modal).toBeVisible();
  await expect(modal.locator(".vars-modal-error")).toContainText("BROKEN_LIST");
  await modal.getByRole("button", { name: "cancel", exact: true }).click();
  await expect(block(page, "broken", "use-broken").locator(".mesh-code-output")).toHaveCount(0);
});

test("a form field whose choices could not be computed says why, too", async ({ page }) => {
  const form = block(page, "broken", "pick-broken");
  await expect(form.getByRole("combobox", { name: "Broken font?", exact: true })).toBeDisabled();
  await expect(form.getByRole("note")).toContainText("exited with status 3");
});

test("the configure dialog explains a pending select and does not overwrite it when saved", async ({ page }) => {
  await page.getByRole("button", { name: "⚙ configure" }).click();
  const modal = page.locator(".vars-modal");
  await expect(modal).toBeVisible();
  await expect(modal.getByRole("combobox", { name: "Configured font?", exact: true })).toBeDisabled();
  await expect(modal.getByRole("note")).toContainText("exited with status 3");

  // Saving the other fields must go through: the pending one is left out
  // of the request instead of being saved empty.
  const request = page.waitForRequest((r) => r.url().endsWith("/api/vars/configure") && r.method() === "POST");
  await modal.getByRole("button", { name: "save", exact: true }).click();
  expect(JSON.parse((await request).postData() ?? "{}").vars).not.toHaveProperty("CFG_FONT");
  await expect(modal).toHaveCount(0);
});


test("the configure button shows a spinner and ignores clicks while its listing loads", async ({ page }) => {
  // The listing runs the sources of computed choices first, so it can be slow.
  let release!: () => void;
  const gate = new Promise<void>((resolve) => (release = resolve));
  await page.route("**/api/vars/configure", async (route) => {
    if (route.request().method() === "GET") await gate;
    await route.continue();
  });

  const button = page.getByRole("button", { name: /configur/ });
  await button.click();
  await expect(button).toBeDisabled();
  await expect(button).toHaveText(/configuring…/);
  await expect(button).toHaveAttribute("aria-busy", "true");
  await expect(page.locator(".vars-modal")).toHaveCount(0);

  release();
  await expect(page.locator(".vars-modal")).toBeVisible();
  await expect(button).toBeEnabled();
  await expect(button).toHaveText("⚙ configure");
  await page.locator(".vars-modal").getByRole("button", { name: "cancel", exact: true }).click();
});
