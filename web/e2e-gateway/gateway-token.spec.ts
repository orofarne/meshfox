import { test, expect } from "@playwright/test";
import { TOKEN } from "./constants";
import { problems, slugOf } from "./helpers";

// The same gateway with MESHFOX_TOKEN set: the browser has to sign in, and
// the session cookie it gets must carry the page, its API calls and the
// watch WebSocket, while a wrong token gets nothing.

const auth = { Authorization: `Bearer ${TOKEN}` };

test("without a session every address asks for the token and serves nothing", async ({ page }) => {
  await page.goto("/");
  await expect(page.getByPlaceholder("Access token")).toBeVisible();
  expect((await page.request.get("/_/index.json")).status()).toBe(401);
  const slug = await slugOf(page, "main.canvas.md", auth);
  // A canvas address shows the form too, and posts back to it relatively.
  await page.goto(`/c/${slug}/`);
  await expect(page.getByPlaceholder("Access token")).toBeVisible();
  expect((await page.request.get(`/c/${slug}/api/canvas`)).status()).toBe(401);
});

test("a wrong token is refused and shows why", async ({ page }) => {
  await page.goto("/");
  await page.getByPlaceholder("Access token").fill("not-the-token");
  await page.getByRole("button", { name: "Sign in" }).click();
  await expect(page.getByText("That token was not accepted")).toBeVisible();
  expect((await page.context().cookies()).some((c) => c.name === "mfx_session")).toBe(false);
});

test("signing in from a canvas address returns to that canvas, socket included", async ({ page }) => {
  const bad = problems(page);
  const slug = await slugOf(page, "main.canvas.md", auth);
  const sockets: string[] = [];
  page.on("websocket", (ws) => sockets.push(ws.url()));

  await page.goto(`/c/${slug}/`);
  await page.getByPlaceholder("Access token").fill(TOKEN);
  await page.getByRole("button", { name: "Sign in" }).click();
  await expect(page).toHaveURL(new RegExp(`/c/${slug}/$`));
  await page.waitForSelector(".mesh-node");

  const cookie = (await page.context().cookies()).find((c) => c.name === "mfx_session");
  expect(cookie?.httpOnly).toBe(true);
  expect(cookie?.sameSite).toBe("Strict");
  expect(cookie?.value).not.toContain(TOKEN);

  // The watch socket authenticates with the cookie alone.
  await expect.poll(() => sockets.length).toBeGreaterThan(0);
  // The 401 that preceded sign-in is expected; nothing after it may fail.
  expect(bad.filter((b) => !b.startsWith("401 "))).toEqual([]);

  // The index works with the same cookie.
  await page.goto("/");
  await expect(page.getByRole("link", { name: "main.canvas.md" }).first()).toBeVisible();
});
