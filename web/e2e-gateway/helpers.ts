import type { Page } from "@playwright/test";

/** The slug the gateway gave a canvas, as its own index says. */
export async function slugOf(page: Page, file = "main.canvas.md", headers?: Record<string, string>): Promise<string> {
  const res = await page.request.get("/_/index.json", { headers });
  const index = await res.json();
  const entry = index.canvases.find((c: { path: string }) => c.path === file);
  if (!entry) throw new Error(`${file} is not in ${JSON.stringify(index)}`);
  return entry.slug;
}

/** Collects everything that went wrong on `page` from now on: error
 * responses, failed requests (navigations the page itself cancelled aside)
 * and uncaught exceptions. */
export function problems(page: Page): string[] {
  const bad: string[] = [];
  page.on("response", (r) => {
    if (r.status() >= 400) bad.push(`${r.status()} ${r.url()}`);
  });
  page.on("requestfailed", (r) => {
    if (r.failure()?.errorText !== "net::ERR_ABORTED") bad.push(`FAILED ${r.url()} ${r.failure()?.errorText}`);
  });
  page.on("pageerror", (e) => bad.push(`pageerror ${e.message}`));
  return bad;
}
