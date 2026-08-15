import { test, expect, type Page } from "@playwright/test";
import { clickFitViewAndWait } from "./helpers";

// Drives web/e2e/fixtures/edge-routing.canvas.md — root plus two direct
// children (one with a grandchild of its own), no `meshfox:edge` extras at
// all. Every edge here is a plain structural (parent→child) "tree" edge —
// the ones this suite calls "the main arrows".
//
// This exists because of a real regression: MeshNode.tsx grew a second
// pair of handles (top/bottom, routing-only, meant for a `meshfox:edge`
// extra edge whose other endpoint sits mostly above/below it rather than
// side by side — see App.tsx's edge-building effect) without giving the
// *original* Left/Right pair explicit `id`s. React Flow's own handle
// lookup for an edge with no `sourceHandle`/`targetHandle` set doesn't
// mean "the handle with no id" — once a node has more than one handle of a
// type, it just grabs the first one it finds in render order
// (`getHandle` in `@xyflow/system`), which silently became one of the new
// top/bottom ones instead. Every structural edge's source point moved as
// a result, changing the whole tree's look — the exact thing this suite
// guards against by asserting each edge's rendered endpoints land on its
// node's `*-default` handle specifically, by id, not merely "somewhere on
// the node".

/** The rendered path's actual start/end points, converted from the SVG's
 * own local coordinate space (what its `d` attribute is written in, same
 * space React Flow computes `sourceX`/`targetX` etc. in) into screen
 * pixels via the path's `getScreenCTM()` — directly comparable to a
 * `getBoundingClientRect()`-based point without hand-rolling the pan/zoom
 * transform math ourselves. Distance (not exact equality) to each
 * expected handle's own center, since a handle is a small circle with
 * real width/height, not a point. */
async function edgeEndpointDistances(
  page: Page,
  edgeId: string,
  sourceNodeId: string,
  sourceHandleId: string,
  targetNodeId: string,
  targetHandleId: string,
) {
  return page.evaluate(
    ({ edgeId, sourceNodeId, sourceHandleId, targetNodeId, targetHandleId }) => {
      const path = document.querySelector<SVGPathElement>(
        `.react-flow__edge[data-id="${CSS.escape(edgeId)}"] path.react-flow__edge-path`,
      );
      if (!path) throw new Error(`no rendered path for edge ${edgeId}`);
      const d = path.getAttribute("d") ?? "";
      const nums = (d.match(/-?\d+(?:\.\d+)?/g) ?? []).map(Number);
      if (nums.length < 4) throw new Error(`unparseable path d="${d}" for edge ${edgeId}`);
      const svg = path.ownerSVGElement!;
      const ctm = path.getScreenCTM()!;
      const toScreen = (x: number, y: number) => {
        const p = svg.createSVGPoint();
        p.x = x;
        p.y = y;
        const s = p.matrixTransform(ctm);
        return { x: s.x, y: s.y };
      };
      const start = toScreen(nums[0], nums[1]);
      const end = toScreen(nums[nums.length - 2], nums[nums.length - 1]);
      const handleCenter = (nodeId: string, handleId: string) => {
        const node = document.querySelector(`.react-flow__node[data-id="${CSS.escape(nodeId)}"]`);
        const handle = node?.querySelector(`[data-handleid="${CSS.escape(handleId)}"]`);
        if (!handle) throw new Error(`no handle "${handleId}" on node ${nodeId}`);
        const r = handle.getBoundingClientRect();
        return { x: r.x + r.width / 2, y: r.y + r.height / 2 };
      };
      const dist = (a: { x: number; y: number }, b: { x: number; y: number }) => Math.hypot(a.x - b.x, a.y - b.y);
      return {
        startToSourceHandle: dist(start, handleCenter(sourceNodeId, sourceHandleId)),
        endToTargetHandle: dist(end, handleCenter(targetNodeId, targetHandleId)),
      };
    },
    { edgeId, sourceNodeId, sourceHandleId, targetNodeId, targetHandleId },
  );
}

test.beforeEach(async ({ page }) => {
  await page.goto("/");
  await page.waitForSelector(".mesh-node");
  await clickFitViewAndWait(page);
});

test("root's own edges (level 1) exit from its Left handle, not the routing-only top/bottom pair", async ({
  page,
}) => {
  for (const [edgeId, targetId] of [
    ["root->child-a", "child-a"],
    ["root->child-b", "child-b"],
  ] as const) {
    const { startToSourceHandle, endToTargetHandle } = await edgeEndpointDistances(
      page,
      edgeId,
      "root",
      "source-default",
      targetId,
      "target-default",
    );
    expect(startToSourceHandle, `${edgeId} start vs. root's source-default handle`).toBeLessThan(4);
    expect(endToTargetHandle, `${edgeId} end vs. ${targetId}'s target-default handle`).toBeLessThan(4);
  }
});

test("a deeper node's own edges (level ≥ 2) exit from its Right handle", async ({ page }) => {
  const { startToSourceHandle, endToTargetHandle } = await edgeEndpointDistances(
    page,
    "child-a->grandchild",
    "child-a",
    "source-default",
    "grandchild",
    "target-default",
  );
  expect(startToSourceHandle, "child-a->grandchild start vs. child-a's source-default handle").toBeLessThan(4);
  expect(endToTargetHandle, "child-a->grandchild end vs. grandchild's target-default handle").toBeLessThan(4);
});

test("a structural edge never lands on one of the routing-only top/bottom handles instead", async ({ page }) => {
  // Same check as above, from the opposite direction: the routing-only
  // handles exist (invisible, `opacity: 0` — see index.css's
  // `.mesh-handle-routing`) but a plain structural edge should never
  // measure as attached to one of them.
  for (const [edgeId, sourceId, targetId] of [
    ["root->child-a", "root", "child-a"],
    ["root->child-b", "root", "child-b"],
    ["child-a->grandchild", "child-a", "grandchild"],
  ] as const) {
    for (const sourceHandleId of ["source-top", "source-bottom"]) {
      const { startToSourceHandle } = await edgeEndpointDistances(
        page,
        edgeId,
        sourceId,
        sourceHandleId,
        targetId,
        "target-default",
      );
      expect(startToSourceHandle, `${edgeId} start vs. ${sourceId}'s ${sourceHandleId} handle`).toBeGreaterThan(10);
    }
    for (const targetHandleId of ["target-top", "target-bottom"]) {
      const { endToTargetHandle } = await edgeEndpointDistances(
        page,
        edgeId,
        sourceId,
        "source-default",
        targetId,
        targetHandleId,
      );
      expect(endToTargetHandle, `${edgeId} end vs. ${targetId}'s ${targetHandleId} handle`).toBeGreaterThan(10);
    }
  }
});
