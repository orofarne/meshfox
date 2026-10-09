import { test, expect, type Locator, type Page } from "@playwright/test";
import { clickFitViewAndWait } from "./helpers";

// Drives web/e2e/fixtures/reload-live-run.canvas.md — a long-running
// block's live output is purely client-side React state
// (`MeshNodeData.liveBlocks`), lost the instant a tab reloads. Checks
// App.tsx's own reconciliation effect (`reconciledActiveRunsRef`: right
// after the canvas first loads, `GET /api/runs` for every block this
// server process still knows about, then `GET /api/run/subscribe` to
// replay its buffered backlog and resume the live tail) actually
// restores *both* halves — lines already printed before the reload, and
// ones the process goes on to print after it — rather than losing the
// first (nothing shown at all until the next manual run) or getting
// stuck on the second (the backlog shows, but live output never resumes).

function block(page: Page, nodeId: string, blockName: string): Locator {
  // `#mesh-block-<node>::<block>` — the `:` needs CSS-escaping in a
  // selector string, see web/src/deps.ts's `blockDomId`.
  return page.locator(`#mesh-block-${nodeId}\\:\\:${blockName}`);
}

test.beforeEach(async ({ page }) => {
  await page.goto("/");
  await page.waitForSelector(".mesh-node");
  await clickFitViewAndWait(page);
});

test("reloading mid-run keeps the already-printed output and resumes the live tail", async ({
  page,
}) => {
  const task = block(page, "root", "long-task");
  const output = task.locator(".mesh-code-output");

  await task.getByRole("button", { name: "run long-task" }).click();
  await expect(output).toContainText("line 1");
  // Still well before the script's own ~6s run — reloading now is the
  // point: the process is still genuinely going on the server, this tab
  // is about to lose every bit of client-side state that says so.
  await expect(output).not.toContainText("line 6");

  await page.reload();
  await page.waitForSelector(".mesh-node");
  await clickFitViewAndWait(page);

  const taskAfterReload = block(page, "root", "long-task");
  const outputAfterReload = taskAfterReload.locator(".mesh-code-output");

  // The backlog: whatever had already printed before the reload is back,
  // without needing a fresh run.
  await expect(outputAfterReload).toContainText("line 1");
  // The run button itself reflects the real, still-running server state
  // (reconciled from `GET /api/runs`), not "idle" just because this tab
  // is new.
  await expect(taskAfterReload.getByRole("button", { name: "running long-task…" })).toBeVisible();

  // The live tail: lines the process prints *after* the reload still
  // arrive, proving the reconnect resumed streaming rather than leaving
  // this tab stuck on the backlog alone.
  await expect(outputAfterReload).toContainText("line 6", { timeout: 10_000 });
  await expect(outputAfterReload).toContainText("done");
  await expect(taskAfterReload.getByRole("button", { name: "run long-task" })).toBeVisible();
});

test("editing a node preserves completed output without a replay or height jump", async ({ page }) => {
  const task = block(page, "root", "long-task");
  await task.getByRole("button", { name: "run long-task" }).click();
  await expect(task.getByRole("button", { name: "running long-task…" })).toBeVisible();
  await expect(task.locator(".mesh-code-output")).toContainText("done", { timeout: 10_000 });
  await expect(task.getByRole("button", { name: "run long-task" })).toBeVisible();
  await page.getByRole("button", { name: "Edit", exact: true }).click();

  // Observe every DOM mutation and frame, so an eventual assertion cannot
  // hide the old bug: output disappears, then /api/runs replays it later.
  await page.evaluate(() => {
    const node = document.querySelector('.react-flow__node[data-id="root"]')!;
    const height = node.getBoundingClientRect().height;
    const samples: { missing: boolean; height: number }[] = [];
    let observing = true;
    const sample = () => samples.push({
      missing: !node.querySelector('.mesh-code-output')?.textContent?.includes('done'),
      height: node.getBoundingClientRect().height,
    });
    const observer = new MutationObserver(sample);
    observer.observe(node, { childList: true, subtree: true, characterData: true });
    const frame = () => { if (observing) { sample(); requestAnimationFrame(frame); } };
    requestAnimationFrame(frame);
    Object.assign(window, { outputObservation: {
      finish: () => { observing = false; observer.disconnect(); sample(); return { height, samples }; },
    } });
  });
  let replays = 0;
  await page.route('**/api/run/subscribe**', route => { replays++; return route.abort(); });

  // Use the real inline title editor, which PATCHes the node and receives
  // a fresh versioned canvas through both its response and the watch feed.
  const title = page.locator('.react-flow__node[data-id="root"] .mesh-node-title');
  await title.locator('.mesh-node-title-text').dblclick();
  const input = page.locator('.mesh-node-title-edit-input');
  const changedTitle = `Edited output fixture ${Date.now()}`;
  await input.fill(changedTitle);
  const saved = page.waitForResponse(response => response.url().endsWith('/api/nodes/root') && response.request().method() === 'PATCH');
  await input.press('Enter');
  expect((await saved).ok()).toBe(true);
  await expect(title).toContainText(changedTitle);
  // Span a full registry poll (3s), including the delayed replay that
  // previously masked the temporary disappearance.
  await page.waitForTimeout(3500);
  const observation = await page.evaluate(() => (window as unknown as {
    outputObservation: { finish: () => { height: number; samples: { missing: boolean; height: number }[] } };
  }).outputObservation.finish());
  expect(observation.samples.length).toBeGreaterThan(0);
  expect(observation.samples.some(sample => sample.missing)).toBe(false);
  expect(Math.min(...observation.samples.map(sample => sample.height))).toBeGreaterThanOrEqual(observation.height - 1);
  expect(replays).toBe(0);
});
