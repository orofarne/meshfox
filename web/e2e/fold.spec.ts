import { test, expect, type Page } from "@playwright/test";
import { clickFitViewAndWait } from "./helpers";

// Drives web/e2e/fixtures/fold.canvas.md, in the default read-only mode
// (never clicks "Edit") — folding is a view-only preference (see App.tsx's
// `foldedNodeIds`), available whether or not the canvas is being edited.

function node(page: Page, id: string) {
  return page.locator(`.react-flow__node[data-id="${id}"]`);
}

test.beforeEach(async ({ page }) => {
  await page.goto("/");
  await page.waitForSelector(".mesh-node");
  await clickFitViewAndWait(page);
});

test("folding a subtree hides its descendants and shows a compact header", async ({ page }) => {
  await expect(node(page, "child-one")).toBeVisible();
  await expect(node(page, "child-two")).toBeVisible();

  await node(page, "parent-node").locator(".mesh-node-fold-toggle").click();

  await expect(node(page, "child-one")).toHaveCount(0);
  await expect(node(page, "child-two")).toHaveCount(0);
  await expect(node(page, "parent-node").locator(".mesh-node")).toHaveAttribute("data-folded", "true");
  await expect(node(page, "parent-node").locator(".mesh-node-body")).toHaveCount(0);
});

test("unfolding restores the hidden descendants", async ({ page }) => {
  const toggle = node(page, "parent-node").locator(".mesh-node-fold-toggle");
  await toggle.click();
  await expect(node(page, "child-one")).toHaveCount(0);

  await toggle.click();

  await expect(node(page, "child-one")).toBeVisible();
  await expect(node(page, "child-two")).toBeVisible();
  await expect(node(page, "parent-node").locator(".mesh-node")).toHaveAttribute("data-folded", "false");
});

test("fold state survives a reload", async ({ page }) => {
  await node(page, "parent-node").locator(".mesh-node-fold-toggle").click();
  await expect(node(page, "child-one")).toHaveCount(0);

  await page.reload();
  await page.waitForSelector(".mesh-node");

  await expect(node(page, "child-one")).toHaveCount(0);
  await expect(node(page, "parent-node").locator(".mesh-node")).toHaveAttribute("data-folded", "true");
});

test("a childless node can be folded too, hiding its own body", async ({ page }) => {
  // `sibling-node` has no children — folding it has no subtree to hide,
  // but its own (possibly large) body still collapses to a title-only
  // row, same as a node with children. See MeshNode.tsx's `FoldToggle`:
  // it's rendered unconditionally, on every node.
  await expect(node(page, "sibling-node").locator(".mesh-node-body")).toBeVisible();

  await node(page, "sibling-node").locator(".mesh-node-fold-toggle").click();

  await expect(node(page, "sibling-node").locator(".mesh-node-body")).toHaveCount(0);
  await expect(node(page, "sibling-node").locator(".mesh-node")).toHaveAttribute("data-folded", "true");
});

test("clicking the title of a folded node unfolds it too, not just the toggle", async ({ page }) => {
  await node(page, "parent-node").locator(".mesh-node-fold-toggle").click();
  await expect(node(page, "child-one")).toHaveCount(0);

  await node(page, "parent-node").locator(".mesh-node-title-text").click();

  await expect(node(page, "child-one")).toBeVisible();
  await expect(node(page, "parent-node").locator(".mesh-node")).toHaveAttribute("data-folded", "false");
});

test("clicking the title of an unfolded node folds it too, not just the toggle", async ({ page }) => {
  await expect(node(page, "parent-node").locator(".mesh-node")).toHaveAttribute("data-folded", "false");

  await node(page, "parent-node").locator(".mesh-node-title-text").click();

  await expect(node(page, "parent-node").locator(".mesh-node")).toHaveAttribute("data-folded", "true");
  await expect(node(page, "child-one")).toHaveCount(0);
});

test("in Edit mode, clicking the title does nothing — only the fold toggle button works", async ({ page }) => {
  // Edit mode makes every node draggable (`nodesDraggable`, see
  // App.tsx's `<ReactFlow>`) — React Flow starts its own node-drag
  // gesture on mousedown anywhere on a node *except* an element (or
  // ancestor) carrying its `nodrag` class. `.mesh-node-title-text` only
  // ever carries `nopan` (blocks canvas panning, relevant in read-only
  // mode too — see the tests above), not `nodrag`, so in Edit mode a
  // click there starts a node-drag instead of ever reaching this
  // component's own `onClick` — unlike `.mesh-node-fold-toggle`, which
  // does carry `nodrag` and keeps working. Deliberate, not an oversight:
  // Edit mode's title needs to stay draggable (grabbing it is how a node
  // gets repositioned), so only the dedicated toggle button folds there.
  await page.getByRole("button", { name: "Edit" }).click();
  await expect(node(page, "parent-node").locator(".mesh-node")).toHaveAttribute("data-folded", "false");

  await node(page, "parent-node").locator(".mesh-node-title-text").click();
  await expect(node(page, "parent-node").locator(".mesh-node")).toHaveAttribute("data-folded", "false");
  await expect(node(page, "child-one")).toBeVisible();

  await node(page, "parent-node").locator(".mesh-node-fold-toggle").click();
  await expect(node(page, "parent-node").locator(".mesh-node")).toHaveAttribute("data-folded", "true");
  await expect(node(page, "child-one")).toHaveCount(0);

  // And back the other way — the toggle unfolds it again, but a title
  // click on the now-folded node still doesn't.
  await node(page, "parent-node").locator(".mesh-node-title-text").click();
  await expect(node(page, "parent-node").locator(".mesh-node")).toHaveAttribute("data-folded", "true");

  await node(page, "parent-node").locator(".mesh-node-fold-toggle").click();
  await expect(node(page, "parent-node").locator(".mesh-node")).toHaveAttribute("data-folded", "false");
});

test("dragging across the title text to select it doesn't fold the node", async ({ page }) => {
  await expect(node(page, "parent-node").locator(".mesh-node")).toHaveAttribute("data-folded", "false");
  const title = node(page, "parent-node").locator(".mesh-node-title-text");
  const box = await title.boundingBox();
  if (!box) throw new Error("title has no box");

  await page.mouse.move(box.x + 2, box.y + box.height / 2);
  await page.mouse.down();
  await page.mouse.move(box.x + box.width - 2, box.y + box.height / 2, { steps: 10 });
  await page.mouse.up();

  await expect
    .poll(() => page.evaluate(() => window.getSelection()?.toString().length ?? 0))
    .toBeGreaterThan(0);
  await expect(node(page, "parent-node").locator(".mesh-node")).toHaveAttribute("data-folded", "false");
});

test("an auto-placed sibling reflows upward when a preceding subtree folds", async ({ page }) => {
  const before = await node(page, "sibling-node").boundingBox();
  if (!before) throw new Error("sibling-node has no box");

  await node(page, "parent-node").locator(".mesh-node-fold-toggle").click();

  await expect
    .poll(async () => (await node(page, "sibling-node").boundingBox())?.y ?? null)
    .toBeLessThan(before.y);
});

// TODO.canvas.md: "Тэги в заголовке" — `parent-node` carries tags (see the
// fixture's own doc comment). Autolayout gives every folded node a fixed
// box height (`FOLDED_HEIGHT`) regardless of its real content, which only
// holds if a folded node's real rendered height never exceeds that — but
// tags used to render as their own row below the title, unconditionally
// (nothing gated them on fold state, unlike the body), so a folded, tagged
// node's real height silently grew past its allotted slot and overlapped
// `sibling-node`, auto-placed right after it on the assumption that slot
// was accurate.
test("a folded node's tags don't grow it past its folded slot and overlap the next sibling", async ({ page }) => {
  await expect(node(page, "parent-node").locator(".mesh-tag-chip")).toHaveCount(2);

  await node(page, "parent-node").locator(".mesh-node-fold-toggle").click();

  const parentBox = await node(page, "parent-node").boundingBox();
  const siblingBox = await node(page, "sibling-node").boundingBox();
  if (!parentBox || !siblingBox) throw new Error("parent-node/sibling-node has no box");

  expect(siblingBox.y).toBeGreaterThanOrEqual(parentBox.y + parentBox.height);
});

function fetchRaw(page: Page): Promise<string> {
  return page.evaluate(() => fetch("/api/canvas/raw").then((r) => r.text()));
}

// TODO.canvas.md: "Проблема с разворачиванием ноды при редактировании" — in
// Edit mode, fold an auto-placed node, create an unrelated node elsewhere
// (any full canvas reload does), then unfold the first one again: its own
// fold toggle flips back and its body reappears in the DOM, but the node's
// box itself stayed clamped to its folded height, as if still collapsed.
//
// Root cause: `sibling-node` is auto-placed (`data.suggested`), so its own
// height is meant to stay `undefined` while unfolded (auto-measured from
// content) and only ever `FOLDED_HEIGHT` while folded. The canvas-reload
// effect in App.tsx sets that explicit `FOLDED_HEIGHT` on it correctly
// while folded — but the *other* effect (the one reacting to a plain
// fold/unfold, not a full reload) used to just carry `n.height` through
// unchanged for a non-group suggested node, on the assumption it was
// already `undefined`. Fold, then a reload while folded, then unfold:
// that assumption breaks, and the stale explicit `FOLDED_HEIGHT` from the
// reload never gets cleared back to `undefined`.
test("a folded auto-placed node restores its real height after an unrelated canvas reload", async ({ page }) => {
  const before = await fetchRaw(page);
  await page.getByRole("button", { name: "Edit" }).click();

  const unfoldedHeight = (await node(page, "sibling-node").boundingBox())?.height;
  if (!unfoldedHeight) throw new Error("sibling-node has no box");

  await node(page, "sibling-node").locator(".mesh-node-fold-toggle").click();

  // Any full canvas reload while `sibling-node` is folded reproduces this —
  // creating an unrelated node under `root` is the simplest one available
  // from the UI itself.
  await page.locator(".mesh-node-add-child").first().click();
  await page.locator(".vars-modal-actions button", { hasText: "ok" }).click();
  await expect(page.locator(".node-settings-modal")).toHaveCount(0);

  await node(page, "sibling-node").locator(".mesh-node-fold-toggle").click();

  await expect
    .poll(async () => (await node(page, "sibling-node").boundingBox())?.height ?? null)
    .toBeGreaterThan(unfoldedHeight - 5);

  await page.evaluate(
    (raw) => fetch("/api/canvas/raw", { method: "PUT", headers: { "content-type": "text/plain" }, body: raw }),
    before,
  );
  expect(await fetchRaw(page)).toBe(before);
});
