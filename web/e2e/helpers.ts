import { expect, type Page } from "@playwright/test";

// Shared across the e2e suite's spec files.

export function viewportTransform(page: Page): Promise<string> {
  return page.locator(".react-flow__viewport").evaluate((e) => (e as HTMLElement).style.transform);
}

/** Clicks the Controls panel's own "fit view" button — a real, ordinary
 * user action — and waits for it to actually take effect before
 * proceeding. Needed because the app opens centered on the root node at a
 * fixed zoom (see App.tsx's `INITIAL_ZOOM`), not fitted to content, so a
 * fixture with nodes spread beyond that fixed view otherwise leaves some
 * of them off screen — which isn't just invisible to a human, it can
 * break Playwright interactions outright: Chromium's CDP-based dispatch
 * can click an off-screen coordinate anyway, but Firefox's driver holds
 * clicks to genuinely in-viewport targets, so a node still on screen only
 * because of Chromium's leniency fails there with "element intercepts
 * pointer events" (a wrong element ends up at that screen position).
 *
 * `fitView()` isn't synchronous (it resolves a promise / can animate), so
 * reading the viewport transform immediately after the click resolves can
 * still catch the pre-click value; waiting for it to differ from whatever
 * it was beforehand avoids that race. */
export async function clickFitViewAndWait(page: Page) {
  const before = await viewportTransform(page);
  await page.locator(".react-flow__controls-fitview").click();
  await expect.poll(() => viewportTransform(page)).not.toBe(before);
}
