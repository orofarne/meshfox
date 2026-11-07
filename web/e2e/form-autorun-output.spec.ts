import { test, expect, type Locator, type Page } from "@playwright/test";
import { clickFitViewAndWait } from "./helpers";

// Drives web/e2e/fixtures/form-autorun-output.canvas.md — end-to-end
// coverage for the `form`/`autorun`/`output="markdown"` combination (see
// SPEC.md's "Form fences" and "Runnable code fences"): filling in a
// form's own fields and clicking Send commits their values and
// automatically reruns the `autorun` block that references them, with no
// manual run and no reload — its freshly printed markdown table replaces
// the old one in place.

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

test("filling in the form and clicking Send autoruns the table with the submitted values", async ({
  page,
}) => {
  const form = block(page, "greeting", "greeting-form");
  const table = block(page, "greeting", "table");
  const output = table.locator(".mesh-code-output");

  // Deliberately doesn't assert "nothing has run yet" here — a retried test
  // (or a reused server) may already have left real output behind, which
  // `App.tsx`'s reconciliation effect (`reconciledActiveRunsRef`)
  // correctly shows on a fresh page load too. What this test covers is that
  // submitting replaces whatever was there (leftover or not) with these
  // specific values, not that the slate starts blank.

  await form.getByLabel("Name").fill("Ada");
  await form.getByLabel("City").fill("Paris");
  await form.getByRole("button", { name: "Apply" }).click();

  await expect(output).toBeVisible();
  await expect(output.locator("table")).toBeVisible();
  await expect(output).toContainText("Ada");
  await expect(output).toContainText("Paris");

  // Submitting again, with different values, reruns the same block again
  // (not just once on the very first Send) and the table reflects the
  // new values, not a mix of old and new.
  await form.getByLabel("Name").fill("Grace");
  await form.getByLabel("City").fill("Boston");
  await form.getByRole("button", { name: "Apply" }).click();

  await expect(output).toContainText("Grace");
  await expect(output).toContainText("Boston");
  await expect(output).not.toContainText("Ada");
  await expect(output).not.toContainText("Paris");
});

test("the table's own source can still be expanded independently — its output never depends on that", async ({
  page,
}) => {
  const form = block(page, "greeting", "greeting-form");
  const table = block(page, "greeting", "table");

  await form.getByLabel("Name").fill("Ada");
  await form.getByLabel("City").fill("Paris");
  await form.getByRole("button", { name: "Apply" }).click();
  await expect(table.locator(".mesh-code-output")).toBeVisible();

  // Folding the block's own source away (the dedicated source-only
  // toggle, `.mesh-code-source-toggle` — distinct from the whole-block
  // fold triangle, which collapses code *and* output together) must
  // never also hide the output it just produced — SPEC.md's "Runnable
  // code fences" `fold` attribute exists precisely to decouple the two.
  await expect(table.locator(".mesh-code-block-source")).toBeVisible();
  await table.getByRole("button", { name: "‹/›" }).click();
  await expect(table.locator(".mesh-code-block-source")).toBeHidden();
  await expect(table.locator(".mesh-code-output")).toBeVisible();
  await expect(table.locator(".mesh-code-output")).toContainText("Ada");
});

test("a form with `reset` clears its plain fields after a successful Send but keeps `keep` ones", async ({
  page,
}) => {
  const form = block(page, "resetting", "reset-form");
  const note = block(page, "resetting", "note");

  await form.getByLabel("Item").fill("milk");
  await form.getByLabel("Shop").fill("market");
  await form.getByRole("button", { name: "Add" }).click();

  await expect(note.locator(".mesh-code-output")).toContainText("added milk at market");
  await expect(form.getByLabel("Item")).toHaveValue("");
  await expect(form.getByLabel("Shop")).toHaveValue("market");
});

test("the Reset button puts every field back to its default, `keep` ones included, and runs nothing", async ({
  page,
}) => {
  const form = block(page, "resetting", "reset-form");
  const note = block(page, "resetting", "note");

  await form.getByLabel("Item").fill("bread");
  await form.getByLabel("Shop").fill("bakery");
  await form.getByRole("button", { name: "Add" }).click();
  await expect(note.locator(".mesh-code-output")).toContainText("added bread at bakery");

  await form.getByLabel("Item").fill("typed but not sent");
  await form.getByRole("button", { name: "Reset" }).click();

  await expect(form.getByLabel("Item")).toHaveValue("");
  await expect(form.getByLabel("Shop")).toHaveValue("corner");
  // Reset starts nothing: the last output is still the last Send's.
  await expect(note.locator(".mesh-code-output")).toContainText("added bread at bakery");
});
