import { test, expect } from "@playwright/test";
import { slugOf } from "../e2e-gateway/helpers";
import { expectNever } from "../e2e/helpers";
import fs from "node:fs";

for (const expanded of [false, true]) {
  test(`synthetic, ${expanded ? "expanded" : "default folds"}: a 560-node canvas renders and survives reload`, async ({ page }, testInfo) => {
    const errors: string[] = [];
    page.on("pageerror", error => errors.push(error.stack ?? error.message));
    page.on("console", message => {
      if (message.type() === "error") errors.push(message.text());
    });
    try {
      if (expanded) {
        await page.addInitScript(rootId => localStorage.setItem(`meshfox-folded:${rootId}`, "[]"), "root");
      }
      const prefix = testInfo.project.name.endsWith("-direct")
        ? "/"
        : `/c/${await slugOf(page, "synthetic.canvas.md")}/`;
      for (let load = 0; load < 2; load++) {
        if (load === 0) await page.goto(prefix);
        else await page.reload();
        await expect.poll(() => errors, { timeout: 1000 }).toEqual([]);
        // The initial default-folded view must have visible, measured nodes,
        // not just a toolbar and an empty React Flow container.
        try {
          await expect(page.locator(".mesh-node").first()).toBeVisible({ timeout: 30_000 });
        } catch (error) {
          expect(errors, "browser errors before nodes became visible").toEqual([]);
          throw error;
        }
        await expect(page.locator(".react-flow__node")).toHaveCount(expanded ? 560 : 49);
        if (expanded && load === 0) {
          const toggle = page.locator('.react-flow__node[data-id="root"] .mesh-node-fold-toggle');
          await toggle.click();
          await expect(page.locator(".react-flow__node")).toHaveCount(1);
          await toggle.click();
          await expect(page.locator(".react-flow__node")).toHaveCount(560);
        }
        await expect.poll(async () => ({
          errors,
          measured: await page.locator('.react-flow__node').evaluateAll(nodes => nodes.every(node => {
            const box = node.getBoundingClientRect();
            return getComputedStyle(node).visibility !== "hidden" && box.width > 0 && box.height > 0;
          })),
        }), { timeout: 30_000 }).toEqual({ errors: [], measured: true });
        await expect(page.locator(".react-flow__edge-path")).toHaveCount(expanded ? 559 : 48);
        await expectNever(10_000, () => errors.length > 0, "large canvas raised a browser error");
        await expect(page.locator(".mesh-node").first()).toBeVisible();
      }
      expect(errors).toEqual([]);
    } finally {
      const errorsPath = testInfo.outputPath("browser-errors.json");
      fs.writeFileSync(errorsPath, JSON.stringify(errors, null, 2));
      await testInfo.attach("browser-errors", { path: errorsPath, contentType: "application/json" });
    }
  });
}
