import { test, expect } from "@playwright/test";

// Drives web/e2e/fixtures/search-pan.canvas.md — its own fixture/port (see
// playwright.config.ts), split out from search.spec.ts because `huge-node`'s
// deliberately-over-tall body wrecks `clickFitViewAndWait`'s fit-to-everything
// zoom for every other node sharing a canvas with it (see the fixture's own
// doc comment). No fitView here at all — the point of this suite is the
// app's own *default* initial view (centered on root, not fitted to
// content), which already leaves `huge-node`'s own match outside the
// visible viewport before search ever touches anything.

test("a match buried in an uncapped, over-tall node pans the canvas — no internal scroll possible there at all", async ({
  page,
}) => {
  await page.goto("/");
  await page.waitForSelector(".mesh-node");

  // `huge-node` is depth 1 — autolayout.ts's `max-height` cap only ever
  // applies at depth ≥2 (see the fixture's own doc comment), so its body
  // never gets an internal scrollbar regardless of how tall its content
  // is: `ensureRangeVisible`'s ancestor-scroll walk finds nothing to
  // scroll here and has to fall all the way through to panning the
  // canvas — the genuinely different case from search.canvas.md's own
  // `long-body-node`.
  const body = page.locator('.react-flow__node[data-id="huge-node"] .mesh-node-body');
  await expect.poll(() => body.evaluate((el) => el.scrollHeight === el.clientHeight)).toBe(true);

  const viewportBefore = await page.locator(".react-flow__viewport").evaluate((el) => el.style.transform);

  await page.keyboard.press("/");
  await page.waitForSelector(".search-bar input");
  await page.fill(".search-bar input", "zzzhugematch");
  await expect(page.locator(".search-bar-count")).toHaveText("1/1");
  await page.locator(".search-bar input").press("Enter");

  await expect
    .poll(() => page.locator(".react-flow__viewport").evaluate((el) => el.style.transform))
    .not.toBe(viewportBefore);
  // Confirms the pan actually landed the match on-screen, not just that
  // *some* pan happened — `ensureRangeVisible` targets the range's own
  // position, so this is the real end-to-end assertion.
  await expect
    .poll(() =>
      page.evaluate(() => {
        const h = CSS.highlights.get("mesh-search-match");
        if (!h) return false;
        for (const r of h) {
          const rect = (r as Range).getBoundingClientRect();
          if (rect.top >= 0 && rect.left >= 0 && rect.bottom <= window.innerHeight && rect.right <= window.innerWidth) {
            return true;
          }
        }
        return false;
      }),
    )
    .toBe(true);
  // And that it didn't do it by scrolling the (non-existent) internal
  // overflow instead — there's nothing there to scroll.
  expect(await body.evaluate((el) => el.scrollTop)).toBe(0);
});
