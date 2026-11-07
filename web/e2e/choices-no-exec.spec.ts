import { test, expect } from "@playwright/test";

// The same fixture as choices.spec.ts, served by `meshfox view --no-exec`:
// a `choices_var` source is never run there, so a `select` that depends on
// one has no choices — and the form must say that, not show an empty list.

// A `--no-exec` page shows no form to fill in (a Send would run blocks), so the
// explanation is checked where it comes from: the same endpoints the forms,
// the TUI and the CLI read.
test("with --no-exec a form field says its list is not computed because running is switched off", async ({ page }) => {
  const res = await page.request.get("/api/form/fields?nodeId=broken&block=pick-broken");
  expect(res.ok()).toBeTruthy();
  const { fields } = await res.json();
  expect(fields[0].choices ?? []).toEqual([]);
  expect(fields[0].choicesPending.var).toBe("BROKEN_LIST");
  expect(fields[0].choicesPending.reason).toContain("--no-exec");
});

test("with --no-exec the configure listing reports the same pending choices and reason", async ({ page }) => {
  const vars: { name: string; choices?: string[]; choicesPending?: { var: string; reason?: string } }[] = await (
    await page.request.get("/api/vars/configure")
  ).json();
  const font = vars.find((v) => v.name === "CFG_FONT");
  expect(font?.choices ?? []).toEqual([]);
  expect(font?.choicesPending?.var).toBe("BROKEN_LIST");
  expect(font?.choicesPending?.reason).toContain("--no-exec");
});
