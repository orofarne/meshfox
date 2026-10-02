import { test, expect, type Page } from "@playwright/test";

// A browser WebSocket never shows a script ping frames and never times out on
// silence: a worker that hangs with its socket open raises no event at all.
// The worker therefore sends `{"type":"heartbeat"}` as an ordinary message and
// the page runs its own silence clock (`watchSilence` in src/api.ts). Both
// halves are driven here against a scripted `/api/watch` — a real worker
// cannot be made to hang from inside the page. Limit shrunk to 2 s through
// the `meshfox.wsSilenceMs` test hook.

test.skip(({ browserName }) => browserName !== "chromium", "routeWebSocket scripting is only driven in Chromium here");

async function shrinkSilenceLimit(page: Page) {
  await page.addInitScript(() => localStorage.setItem("meshfox.wsSilenceMs", "2000"));
}

test("a watch socket that goes silent is dropped and re-established", async ({ page }) => {
  await shrinkSilenceLimit(page);
  let connections = 0;
  await page.routeWebSocket(/\/api\/watch/, (ws) => {
    connections += 1;
    // Connected, then nothing at all: a hung worker with the socket open.
    ws.send(JSON.stringify({ type: "connected", resync: false }));
  });
  await page.goto("/");
  await expect.poll(() => connections, { timeout: 15_000 }).toBeGreaterThanOrEqual(2);
});

test("a quiet watch socket that keeps sending heartbeats is left alone", async ({ page }) => {
  await shrinkSilenceLimit(page);
  let connections = 0;
  await page.routeWebSocket(/\/api\/watch/, (ws) => {
    connections += 1;
    ws.send(JSON.stringify({ type: "connected", resync: false }));
    // Quiet for much longer than the 2 s limit, but alive.
    const timer = setInterval(() => ws.send(JSON.stringify({ type: "heartbeat" })), 500);
    ws.onClose(() => clearInterval(timer));
  });
  await page.goto("/");
  await expect.poll(() => connections).toBe(1);
  await page.waitForTimeout(6_000);
  expect(connections).toBe(1);
});
