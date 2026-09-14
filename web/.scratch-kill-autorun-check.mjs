import { chromium } from "@playwright/test";

const browser = await chromium.launch();
const page = await browser.newPage();
await page.goto("http://127.0.0.1:4799/");
await page.waitForSelector(".mesh-node");

const form = page.locator('#mesh-block-slot\\:\\:trigger-form');
const block = page.locator('#mesh-block-slot\\:\\:slow-autorun');

await form.getByLabel("Trigger").fill("go");
await form.getByRole("button", { name: "Apply" }).click();

await block.getByRole("button", { name: /running/ }).waitFor({ timeout: 5000 });
console.log("RUNNING shown, kill button present:", await block.locator(".mesh-kill-button").isVisible());

await block.locator(".mesh-kill-button").click();

try {
  await block.getByRole("button", { name: "run slow-autorun" }).waitFor({ timeout: 5000 });
  console.log("RESULT: kill worked — button reverted to idle quickly");
} catch (e) {
  console.log("RESULT: kill did NOT work — still running after 5s", e.message);
}

const outputText = await block.locator(".mesh-code-output").innerText().catch(() => "<no output>");
console.log("final output:", JSON.stringify(outputText));

await browser.close();
