import { expect, type Locator, type Page } from "@playwright/test";

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

/** Neutralizes App.tsx's "fold everything with children except root by
 * default" behavior for a suite that isn't itself about fold/collapse —
 * pre-seeds localStorage (via `addInitScript`, so it's in place before the
 * app's own restore effect ever runs) with an explicit *empty* folded set
 * for `rootId`, which the app reads as "already has a saved (empty) state"
 * and skips computing the default entirely. Call before `page.goto`. Only
 * needed for a fixture with a non-root node that has children of its own
 * (a flat fixture has nothing the default would ever fold). */
export async function disableDefaultFold(page: Page, rootId: string) {
  await page.addInitScript((key) => localStorage.setItem(key, "[]"), `meshfox-folded:${rootId}`);
}

/** Selects `node` (a `.react-flow__node[data-id="..."]` locator) by
 * clicking its own title text — needed before reaching any of that
 * node's own action buttons (edit/settings/delete/move/reset-layout):
 * they now live in a floating `NodeToolbar` above the node (`.mesh-node-
 * toolbar`, MeshNode.tsx) instead of inline in its title bar, shown only
 * once the node is actually *selected*, not merely hovered, the way the
 * old inline buttons worked. A plain click on the title text is a
 * documented no-op in Edit mode as far as fold-toggling goes (see
 * MeshNode.tsx's `handleTitleClick`'s own `if (data.editMode) return`)
 * but still bubbles up to React Flow's own default click-to-select
 * handling — unlike clicking the fold toggle button itself, or any other
 * icon, both of which handle the click themselves. Works the same way
 * whether `node` has a real body (`.mesh-node-title-text`) or is a
 * header-only, centered-layout node (`.mesh-node-title-centered-text`);
 * exactly one of the two ever exists for a given node, never both. */
export async function selectNode(node: Locator) {
  await node.locator(".mesh-node-title-text, .mesh-node-title-centered-text").first().click();
}

/** The floating `NodeToolbar` button whose `title` contains `titleSubstring`
 * (case-insensitive) — global, not scoped under a specific node's own
 * locator, since `NodeToolbar` portals its content out of the node's own
 * DOM subtree entirely (into React Flow's shared top-level rendering
 * layer — see MeshNode.tsx's own comment on why) and only ever renders
 * for whichever single node is currently selected, so at most one `.mesh-
 * node-toolbar` exists in the DOM at any one time regardless of how many
 * nodes the canvas has. Call `selectNode` on the target node first. */
export function toolbarButton(page: Page, titleSubstring: string): Locator {
  return page.locator(`.mesh-node-toolbar button[title*="${titleSubstring}" i]`);
}
