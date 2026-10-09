import { test, expect, type Page } from "@playwright/test";
import { clickFitViewAndWait, disableDefaultFold } from "./helpers";

// Real, auto-sized members use group-relative coordinates. Sample the SVG
// curve in screen space so incorrect parent offsets, rounded corners and
// a fallback bezier disappearing behind cards are all covered.
async function inspectLink(page: Page) {
  return page.evaluate(() => {
    const path = document.querySelector<SVGPathElement>(
      '.react-flow__edge[data-id="node2->node10:extra"] .react-flow__edge-path',
    );
    if (!path) throw new Error("node2 -> node10 is missing");
    const matrix = path.getScreenCTM()!;
    const length = path.getTotalLength();
    const screenPoint = (distance: number) => {
      const point = path.getPointAtLength(distance);
      return new DOMPoint(point.x, point.y).matrixTransform(matrix);
    };
    const cards = Array.from({ length: 12 }, (_, i) => {
      const id = `node${i + 1}`;
      const node = document.querySelector<HTMLElement>(`.react-flow__node[data-id="${id}"]`)!;
      return { id, node, box: node.getBoundingClientRect() };
    });
    const crossings: string[] = [];
    const hidden: string[] = [];
    // At most one screen pixel between samples, including both endpoints.
    const steps = Math.ceil(length * Math.hypot(matrix.a, matrix.b));
    for (let i = 0; i <= steps; i++) {
      const point = screenPoint(length * i / steps);
      for (const { id, box } of cards) {
        if (point.x > box.left + 1 && point.x < box.right - 1 &&
            point.y > box.top + 1 && point.y < box.bottom - 1) crossings.push(id);
      }
      // The group's background must not paint over the link either.
      // React Flow's interaction path shares the edge's stacking position.
      if (i > 0 && i < steps && point.x > 0 && point.x < innerWidth && point.y > 0 && point.y < innerHeight) {
        const hits = document.elementsFromPoint(point.x, point.y);
        const edgeIndex = hits.findIndex(el => el.closest('.react-flow__edge[data-id="node2->node10:extra"]'));
        const cardIndex = hits.findIndex(el => el.closest('.mesh-node'));
        if (edgeIndex < 0 || (cardIndex >= 0 && cardIndex < edgeIndex)) hidden.push(`${Math.round(point.x)},${Math.round(point.y)}`);
      }
    }
    const endpointDistance = (id: string, handle: string, distance: number) => {
      const node = cards.find(card => card.id === id)!.node;
      const box = node.querySelector(`[data-handleid="${handle}"]`)!.getBoundingClientRect();
      const point = screenPoint(distance);
      return Math.hypot(point.x - box.x - box.width / 2, point.y - box.y - box.height / 2);
    };
    const end = screenPoint(length);
    const start = screenPoint(0);
    return {
      crossings: [...new Set(crossings)], hidden: hidden.slice(0, 5),
      sourceDistance: endpointDistance("node2", "source-bottom", 0),
      targetDistance: endpointDistance("node10", "target-top", length),
      marker: path.getAttribute("marker-end"),
      endpointsInView: [start, end].every(p => p.x > 0 && p.x < innerWidth && p.y > 0 && p.y < innerHeight),
    };
  });
}

for (const folded of [false, true]) {
  test(`node2 -> node10 remains visible around 12 ${folded ? "folded" : "expanded"} group members`, async ({ page }) => {
    await disableDefaultFold(page, "root");
    await page.goto("/");
    await expect(page.locator('.react-flow__node[data-id="g1"]')).toBeVisible();
    await expect(page.locator('.react-flow__node[data-id^="node"]')).toHaveCount(12);
    await clickFitViewAndWait(page);
    if (folded) {
      for (let i = 1; i <= 12; i++) {
        await page.locator(`.react-flow__node[data-id="node${i}"] .mesh-node-fold-toggle`).click();
      }
      await clickFitViewAndWait(page);
    }
    await expect.poll(async () => {
      const link = await inspectLink(page);
      return { ...link, sourceDistance: link.sourceDistance < 4, targetDistance: link.targetDistance < 4,
        marker: !!link.marker && link.marker !== "none" };
    }).toEqual({ crossings: [], hidden: [], sourceDistance: true, targetDistance: true, marker: true, endpointsInView: true });
  });
}
