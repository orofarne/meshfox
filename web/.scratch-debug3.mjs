import { chromium } from "playwright";

const BASE = "http://127.0.0.1:4599";
const browser = await chromium.launch();
const page = await browser.newPage();
page.on("console", (m) => console.log("[console]", m.type(), m.text()));
page.on("pageerror", (e) => console.log("[pageerror]", e.message));

await page.goto(BASE);
await page.waitForSelector(".mesh-node");
await page.getByRole("button", { name: /edit/i }).click();
await page.waitForSelector(".mesh-node-id");

// Hover the root node to reveal its NodeToolbar "+" button, then click it.
const root = page.getByTestId("rf__node-root");
await root.hover();
await page.waitForSelector(".mesh-node-add-child", { state: "visible" });
await page.locator(".mesh-node-add-child").click();

await page.waitForSelector(".node-settings-modal");
const idInput = page.locator(".node-settings-modal label:has-text('ID') input");
const titleInput = page.locator(".node-settings-modal label:has-text('Title') input");
console.log("initial id:", await idInput.inputValue());
console.log("initial title:", await titleInput.inputValue());

await titleInput.fill("");
await titleInput.type("Привет Мир", { delay: 20 });
await page.waitForTimeout(200);

const hintCount = await page.locator(".node-settings-id-hint").count();
console.log("hint count after editing title:", hintCount);
if (hintCount) console.log("hint text:", await page.locator(".node-settings-id-hint").textContent());
console.log("id value now:", await idInput.inputValue());

await browser.close();
