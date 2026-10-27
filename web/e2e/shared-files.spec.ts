import { test, expect } from "@playwright/test";
import fs from "node:fs";
import path from "node:path";

test("parent config permits ordinary Markdown images, includes and links", async ({ page, request }) => {
  await page.goto("/");
  for (const name of ["SCD41", "Included"]) {
    const image = page.getByRole("img", { name, exact: true });
    await expect(image).toBeVisible();
    await expect.poll(() => image.evaluate((el: HTMLImageElement) => el.naturalWidth)).toBe(32);
  }
  const link = page.getByRole("link", { name: "datasheet", exact: true });
  const href = await link.getAttribute("href");
  expect(href).toContain("api/asset?");
  const response = await request.get(href!);
  expect(response.status()).toBe(200);
  expect(await response.text()).toBe("shared datasheet");
  const config = path.join(process.env.MESHFOX_SHARED_FILES_FIXTURE!, ".meshfox/config.toml");
  fs.writeFileSync(config, "[files]\nallow = []\n");
  expect((await request.get(href!)).status()).toBe(403);
});
