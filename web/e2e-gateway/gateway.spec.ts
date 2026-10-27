import { test, expect } from "@playwright/test";
import { problems, slugOf } from "./helpers";

// Drives `meshfox serve --http` (open access, loopback) in a real browser:
// the index page, a canvas under `/c/<slug>/`, and everything that page does
// that could leave the prefix — assets, lazy Shiki and Monaco chunks, the
// watch socket, runs, links between canvases, files, the path picker.

test("the index lists the canvases with their extension and opens one under its slug", async ({ page }) => {
  const bad = problems(page);
  await page.goto("/");
  const link = page.getByRole("link", { name: "main.canvas.md" }).first();
  await expect(link).toBeVisible();
  // Subdirectories are collapsed: the link is there, just not shown yet.
  await expect(page.locator("a", { hasText: "other.canvas.md" })).toHaveCount(1);
  await page.getByText("sub/", { exact: true }).click();
  await expect(page.getByRole("link", { name: "other.canvas.md" })).toBeVisible();
  // Relative links: the page works behind any outer prefix.
  expect(await link.getAttribute("href")).toMatch(/^c\/main-[a-z2-7]{8}\/$/);

  await link.click();
  await expect(page).toHaveURL(/\/c\/main-[a-z2-7]{8}\/$/);
  await page.waitForSelector(".mesh-node");
  expect(bad).toEqual([]);
});

test("a canvas without the trailing slash is redirected to the canonical address", async ({ page }) => {
  const slug = await slugOf(page);
  await page.goto(`/c/${slug}`);
  await expect(page).toHaveURL(new RegExp(`/c/${slug}/$`));
  await page.waitForSelector(".mesh-node");
});

test("a bad or tampered slug is a 404, not a canvas", async ({ page }) => {
  const slug = await slugOf(page);
  const hash = slug.slice(-8);
  for (const bad of ["nope", "todo-aaaaaaaa", `wrong-${hash}`]) {
    const res = await page.request.get(`/c/${bad}/`);
    expect(res.status(), bad).toBe(404);
  }
});

test("runs, the watch socket and lazy chunks all stay under the prefix", async ({ page }) => {
  const bad = problems(page);
  const sockets: string[] = [];
  const assets: string[] = [];
  page.on("websocket", (ws) => sockets.push(ws.url()));
  page.on("response", (r) => {
    if (/\/assets\//.test(r.url())) assets.push(r.url());
  });
  const slug = await slugOf(page);
  await page.goto(`/c/${slug}/`);
  await page.waitForSelector(".mesh-node");

  // A run goes through the proxy and streams its output back.
  const run = page.locator('.react-flow__node[data-id="run"]');
  await run.getByRole("button", { name: /^(▷|run)/i }).first().click();
  await expect(run).toContainText("hello from the gateway", { timeout: 20_000 });

  // The cpp grammar is a lazy chunk: it has to load from under the prefix.
  const code = page.locator('.react-flow__node[data-id="code"] .mesh-shiki');
  await expect(code.locator("span[style*='--shiki']").first()).toBeVisible({ timeout: 20_000 });

  expect(sockets.length).toBeGreaterThan(0);
  for (const url of sockets) expect(new URL(url).pathname.startsWith(`/c/${slug}/api/`), url).toBe(true);
  for (const url of assets) expect(new URL(url).pathname.startsWith(`/c/${slug}/assets/`), url).toBe(true);
  expect(bad).toEqual([]);
});

test("the source editor (Monaco and its workers) loads under the prefix", async ({ page }) => {
  const bad = problems(page);
  const slug = await slugOf(page);
  await page.goto(`/c/${slug}/`);
  await page.waitForSelector(".mesh-node");
  await page.getByRole("button", { name: "Edit" }).click();
  await page.getByRole("button", { name: "Source" }).click();
  const editor = page.locator(".monaco-editor");
  await expect(editor.first()).toBeVisible({ timeout: 30_000 });
  await expect(editor.first()).toContainText("Gateway fixture", { timeout: 30_000 });
  expect(bad).toEqual([]);
});

test("a link to another canvas opens its own gateway address in a new tab", async ({ page, context }) => {
  const bad = problems(page);
  const slug = await slugOf(page);
  await page.goto(`/c/${slug}/`);
  await page.waitForSelector(".mesh-node");
  const info = await page.evaluate(() => fetch("api/info").then((r) => r.json()));
  expect(info.remote).toBe(true);

  const [popup] = await Promise.all([
    context.waitForEvent("page"),
    page.locator(".mesh-node-open-link", { hasText: "other.canvas.md" }).first().click(),
  ]);
  await popup.waitForLoadState("networkidle");
  await expect(popup).toHaveURL(/\/c\/other-[a-z2-7]{8}\/#child$/);
  await expect(popup.getByText("The other page")).toBeVisible();
  expect(bad).toEqual([]);
});

test("a plain file is a link to the file, with nothing to open on the server", async ({ page }) => {
  const slug = await slugOf(page);
  await page.goto(`/c/${slug}/`);
  await page.waitForSelector(".mesh-node");
  const note = page.locator('.react-flow__node[data-id="note"]');
  await expect(note.locator(".mesh-node-open-link")).toHaveCount(0);
  const href = await note.locator(".mesh-node-target-row a").first().getAttribute("href");
  expect(href).toContain("api/asset");
  const res = await page.request.get(new URL(href!, page.url()).href);
  expect(res.status()).toBe(200);
  expect(await res.text()).toContain("plain note");
  // Nothing opens a folder or a file on the server's machine.
  await expect(page.locator('button[title="Show this file\'s containing folder"]')).toHaveCount(0);
  await expect(note.locator('button[title="Open this file in the default application"]')).toHaveCount(0);
});

test("the path picker browses the served tree and cannot climb out of it", async ({ page }) => {
  const slug = await slugOf(page);
  await page.goto(`/c/${slug}/`);
  await page.waitForSelector(".mesh-form-block .path-field");
  await page.locator(".mesh-form-block .path-field-browse").click();
  const picker = page.locator(".path-picker");
  await expect(picker).toBeVisible();

  const location = picker.getByLabel("Location");
  await expect(picker.locator(".path-picker-entry", { hasText: "data" })).toBeVisible();
  const root = await location.inputValue();
  expect(root).not.toBe("");
  // At the root there is no way up and no home shortcut.
  await expect(picker.getByTitle("Parent directory")).toBeDisabled();
  await expect(picker.getByTitle("Home directory")).toBeDisabled();

  // Typing a path outside brings the picker back to the root.
  for (const outside of ["/", "/etc", ".."]) {
    await location.fill(outside);
    await location.press("Enter");
    await expect(location).toHaveValue(root);
    await expect(picker.getByTitle("Parent directory")).toBeDisabled();
  }

  // Going down works, and from there up stops at the root again.
  await picker.locator(".path-picker-entry", { hasText: "data" }).click();
  await expect(picker.locator(".path-picker-entry", { hasText: "inside.txt" })).toBeVisible();
  await picker.getByTitle("Parent directory").click();
  await expect(location).toHaveValue(root);
});
