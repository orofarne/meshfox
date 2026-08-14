import { test, expect, type Page } from "@playwright/test";
import { clickFitViewAndWait, disableDefaultFold } from "./helpers";

// Drives web/e2e/fixtures/settings.canvas.md: one node per NodeSettings-
// relevant type/field combination (see the fixture's own root body for the
// full rationale). Every node here gets the exact same treatment — open its
// settings modal, click "ok" without touching a single field — and the raw
// file (`GET /api/canvas/raw`) must come out byte-for-byte identical every
// time.
//
// This suite exists because "ok" resending fields nobody touched has twice
// silently rewritten the file out from under a no-op click: once by always
// including `color` (turning an absent color into a stored `color=""`), and
// once by always including the *resolved* node type — which, for an
// `include` node, is never actually `"include"` (the server always resolves
// one into a `group`/`text` node before it reaches the client; see
// `crates/core/src/include.rs`), so resending it silently overwrote the raw
// `type="include"` with whatever it happened to resolve to, destroying the
// include outright. `NodeSettings.tsx`'s `buildPatch` now diffs against the
// node's original values and sends only what actually changed — this suite
// is the regression test for that, across every node type the settings
// modal can show.

async function openSettings(page: Page, nodeId: string) {
  const node = page.locator(`.react-flow__node[data-id="${nodeId}"]`);
  // A `group` node's own box spans its children, so hovering the node's
  // full bounding box (rather than just its title bar) can land on a
  // child's DOM instead and never reveal this node's own actions — see
  // `.mesh-node-title`/`.mesh-node-title-actions` in MeshNode.tsx.
  await node.locator(".mesh-node-title").hover();
  await node.locator('button[title*="settings" i]').click();
  await expect(page.locator(".node-settings-modal")).toBeVisible();
}

async function clickOk(page: Page) {
  await page.locator(".vars-modal-actions button", { hasText: "ok" }).click();
  await expect(page.locator(".node-settings-modal")).toHaveCount(0);
}

function fetchRaw(page: Page): Promise<string> {
  return page.evaluate(() => fetch("/api/canvas/raw").then((r) => r.text()));
}

test.beforeEach(async ({ page }) => {
  // This fixture nests real nodes (group-node -> group-child,
  // include-canvas -> its own spliced children) that App.tsx's
  // fold-everything-with-children-by-default behavior would otherwise
  // hide on first load — this suite is about NodeSettings, not fold, so
  // start from every node visible instead.
  await disableDefaultFold(page, "root");
  await page.goto("/");
  await page.waitForSelector(".mesh-node");
  // Settings are Edit-mode-only (see MeshNode.tsx's gear icon) — every test
  // below needs it on. Fit-view first, same reasoning as every other spec
  // in this suite (see helpers.ts): gets every node genuinely on screen so
  // Firefox's stricter in-viewport click check doesn't fail on the later
  // ones.
  await clickFitViewAndWait(page);
  await page.getByRole("button", { name: "Edit" }).click();
});

// One entry per NodeSettings-relevant combination in the fixture:
// - a bare text node (nothing optional set at all)
// - a text node with every optional field set at once (color, tags, an
//   extra incoming edge)
// - a `file` node, plain link display
// - a `file` node, code-preview display + a language hint + an interpreter
// - a `link` node
// - an `include` node whose target is plain Markdown (resolves to `text`)
// - an `include` node whose target is itself a canvas (resolves to `group`)
// - a `group` node, and one of its structural children
const NODE_IDS = [
  "root",
  "text-plain",
  "text-styled",
  "file-link",
  "file-code",
  "link-node",
  "include-text",
  "include-canvas",
  "group-node",
  "group-child",
];

for (const nodeId of NODE_IDS) {
  test(`"ok" with no edits leaves the file unchanged — ${nodeId}`, async ({ page }) => {
    const before = await fetchRaw(page);

    await openSettings(page, nodeId);
    await clickOk(page);

    expect(await fetchRaw(page)).toBe(before);
  });
}
