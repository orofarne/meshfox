import { test, expect, type Locator, type Page } from "@playwright/test";
import { clickFitViewAndWait, disableDefaultFold } from "./helpers";

// Drives web/e2e/fixtures/deps.canvas.md through the real UI, in the
// default read-only mode (never clicks "Edit"), so nothing here ever
// writes back into the fixture file on disk.

function block(page: Page, nodeId: string, blockName: string): Locator {
  // `#mesh-block-<node>::<block>` — the `:` needs CSS-escaping in a
  // selector string, see web/src/deps.ts's `blockDomId`.
  return page.locator(`#mesh-block-${nodeId}\\:\\:${blockName}`);
}

test.beforeEach(async ({ page }) => {
  // This suite is about deps/run-chain UI, not fold — every node here
  // needs to be visible/expanded regardless of App.tsx's own
  // fold-everything-but-root-by-default behavior.
  await disableDefaultFold(page, "root");
  await page.goto("/");
  await page.waitForSelector(".mesh-node");
  // The app opens centered on the root node at a fixed zoom (see App.tsx's
  // `INITIAL_ZOOM`), not fitted to content — this fixture's later nodes
  // (release-node, slow-node, ...) end up off screen otherwise, which isn't
  // just invisible to a human: Firefox's driver refuses to click a target
  // that isn't genuinely within the viewport (see helpers.ts for the
  // Chromium-vs-Firefox difference this papers over). Fitting the view
  // gets every node on screen for every test in this file.
  await clickFitViewAndWait(page);
});

test("a block without deps gets a plain run button", async ({ page }) => {
  // `.locator("button")` alone would also match the block's own fold
  // toggle (see MeshNode.tsx's `RunnableCodeBlock`) — filtered to the run
  // button specifically by its "run …" text.
  const btn = block(page, "build-node", "build").locator("button", { hasText: "run" });
  await expect(btn).toHaveText("run build");
  await expect(btn).not.toHaveClass(/mesh-run-chain/);
  await expect(block(page, "build-node", "build").locator(".mesh-code-deps")).toHaveCount(0);
});

test("a block with deps gets the distinct chain-run button and an after: badge", async ({ page }) => {
  const testBlock = block(page, "build-node", "test");
  await expect(testBlock.locator("button.mesh-run-chain")).toHaveText("⛓ run chain: test");
  await expect(testBlock.locator(".mesh-code-deps")).toContainText("build");
});

test("running a chained block runs its whole dependency chain in order", async ({ page }) => {
  // release depends on build-node/test and deploy-node/deploy, which in
  // turn depend on build-node/build — running it should transitively run
  // all four, each showing its own (unsaved) output.
  await block(page, "release-node", "release").locator("button.mesh-run-chain").click();

  await expect(block(page, "build-node", "build").locator(".mesh-code-output")).toContainText("building");
  await expect(block(page, "build-node", "test").locator(".mesh-code-output")).toContainText("testing");
  await expect(block(page, "deploy-node", "deploy").locator(".mesh-code-output")).toContainText("deploying");
  await expect(block(page, "release-node", "release").locator(".mesh-code-output")).toContainText("releasing");

  // Read-only mode: every output is shown but never persisted.
  await expect(page.locator(".mesh-code-output-transient")).toHaveCount(4);
});

test("running a chain executes an uncached block too, shown transiently like any other step", async ({ page }) => {
  await block(page, "deploy-node", "verify").locator("button.mesh-run-chain").click();
  await expect(block(page, "deploy-node", "deploy").locator(".mesh-code-output")).toContainText("deploying");
  // `cache` only controls whether a step's output gets written back into
  // the file — the UI still shows *every* step's output while it's fresh,
  // marked "not saved" instead of omitted, whether or not that block has
  // `cache` set.
  const verifyOutput = block(page, "deploy-node", "verify").locator(".mesh-code-output");
  await expect(verifyOutput).toContainText("verifying");
  await expect(verifyOutput.locator(".mesh-code-output-transient")).toBeVisible();
});

test("the multiple-deps badge lists every dependency and each is a separate link", async ({ page }) => {
  const links = block(page, "release-node", "release").locator(".mesh-dep-link");
  await expect(links).toHaveCount(2);
  await expect(links.nth(0)).toHaveText("build-node/test");
  await expect(links.nth(1)).toHaveText("deploy-node/deploy");
});

test("clicking an after: link scrolls to and briefly highlights the dependency block", async ({ page }) => {
  const link = block(page, "release-node", "release").locator(".mesh-dep-link", {
    hasText: "deploy-node/deploy",
  });
  await link.click();
  await expect(block(page, "deploy-node", "deploy")).toHaveClass(/mesh-code-block-flash/);
  // The flash is transient — it should clear itself without user action.
  await expect(block(page, "deploy-node", "deploy")).not.toHaveClass(/mesh-code-block-flash/, {
    timeout: 3_000,
  });
});

test("a lone unnamed fence is runnable, implicitly named after its node", async ({ page }) => {
  // block(page, "implicit-node", "implicit-node") — the fence has no
  // `name=`, so its effective (and DOM-id) name is the node's own id.
  const block_ = block(page, "implicit-node", "implicit-node");
  const btn = block_.locator("button", { hasText: "run" });
  await expect(btn).toHaveText("run implicit-node");
  await expect(btn).not.toHaveClass(/mesh-run-chain/);

  await btn.click();
  await expect(block_.locator(".mesh-code-output")).toContainText("implicit block ran");
});

test("output streams in as it happens, not all at once at the end", async ({ page }) => {
  const slow = block(page, "slow-node", "slow");
  await slow.locator("button", { hasText: "run" }).click();

  await expect(slow.locator(".mesh-code-output")).toContainText("tick 1", { timeout: 3_000 });
  // `slow` takes ~5s end to end (five 1s ticks) — right after the first
  // tick lands, the last line genuinely can't be there yet. If output only
  // ever appeared as one lump at the end, this would already contain it.
  await expect(slow.locator(".mesh-code-output")).not.toContainText("finished");
  await expect(slow.locator('[data-exit="running"]')).toBeVisible();

  await expect(slow.locator(".mesh-code-output")).toContainText("finished", { timeout: 8_000 });
  await expect(slow.locator('[data-exit="ok"]')).toBeVisible();
});

test("Kill stops a running block, and the rest of its chain never starts", async ({ page }) => {
  const slow = block(page, "slow-node", "slow");
  const afterSlow = block(page, "slow-node", "after-slow");

  // Running after-slow pulls in its dependency, `slow`, first.
  await afterSlow.locator("button.mesh-run-chain").click();
  await expect(slow.locator(".mesh-kill-button")).toBeVisible({ timeout: 3_000 });
  await expect(slow.locator(".mesh-code-output")).toContainText("tick 1");

  await slow.locator(".mesh-kill-button").click();

  await expect(slow.locator('[data-exit="killed"]')).toBeVisible({ timeout: 5_000 });
  await expect(slow.locator(".mesh-kill-button")).toHaveCount(0);
  // Killing stops the whole chain — after-slow (queued behind `slow`)
  // never gets a chance to run at all, so it settles into `blocked` rather
  // than sitting stuck on "queued…" forever (see MeshNode.tsx's
  // `LiveBlockState.status` doc comment).
  await expect(afterSlow.locator('[data-exit="blocked"]')).toBeVisible({ timeout: 5_000 });

  // The process is really gone, not just abandoned client-side — running
  // `slow` again should behave like a completely fresh run, not one still
  // tangled up with the killed one.
  await slow.locator("button", { hasText: "run" }).click();
  await expect(slow.locator(".mesh-code-output")).toContainText("tick 1", { timeout: 3_000 });
  await expect(slow.locator(".mesh-code-output")).toContainText("finished", { timeout: 8_000 });
});

test("a failing step in a chain marks what comes after it blocked, not stuck queued", async ({ page }) => {
  const fail = block(page, "failing-node", "fail");
  const afterFail = block(page, "failing-node", "after-fail");

  await afterFail.locator("button.mesh-run-chain").click();

  await expect(fail.locator('[data-exit="fail"]')).toBeVisible({ timeout: 5_000 });
  // The server stops the chain the moment `fail` exits non-zero — `after-fail`
  // never gets a `step-start`/`step-skipped` at all, so it must settle into
  // `blocked` on its own once the stream ends rather than sitting on
  // "queued…" forever (see App.tsx's `executeRun`/`blockStuckQueued`).
  await expect(afterFail.locator('[data-exit="blocked"]')).toBeVisible({ timeout: 5_000 });
  await expect(afterFail.locator(".mesh-code-output")).toContainText("a dependency in its chain failed");

  // Not stuck disabled either — retrying is just clicking run again.
  await expect(afterFail.locator("button", { hasText: "run" })).toBeEnabled();
});
