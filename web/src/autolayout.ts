// Client-side auto-layout for whatever's still unpositioned in the canvas.
// This is the only auto-layout meshfox has — there used to be a parallel
// heuristic-based estimate on the Rust side (`crates/core/src/layout.rs`,
// once used by the now-removed `meshfox fmt` command), but it had no way to
// know the browser's actual viewport width or a node's real rendered
// content height, both of which this module leans on directly, so it was
// dropped in favor of this one. Coordinates are only ever set by dragging a
// node in the web UI now; nothing hand-computes and writes them anymore.
//
// Root + its direct children read top-to-bottom in one column, deeper
// nesting branches right of its own parent, siblings stack vertically
// without overlapping, a subtree's full consumed height bubbles up to its
// parent so unrelated branches never collide, a `group`'s box is always the
// bounding box of its resolved members. Sizing:
//   - width is tier-based, not content-estimated: root (tree depth 0) and
//     its direct children (depth 1) all get the same width, a fraction of
//     the viewport; everything deeper gets a narrower fraction, uniformly
//     regardless of how much deeper.
//   - height is never estimated: it comes from React Flow's own
//     `measured.height` (it tracks real rendered size via ResizeObserver
//     for any node this module doesn't hand an explicit `height`), with a
//     small placeholder used only until the first real measurement lands.
//     Depth ≥2 nodes additionally get a CSS `max-height` cap (surfaced here
//     as `LayoutBox.maxHeight`, for the caller to apply as a style
//     override) so one long block can't drag a whole subtree far from its
//     parent — it scrolls internally past the cap instead (the existing
//     `.mesh-node-body { overflow: auto }` already handles that), and
//     `measured.height` reflects the clamped box once rendered, so the cap
//     flows into the stacking math for free.
//
// A node with a real, authored position (`x`/`y`) — or, transitively, a
// real `height` — anchors there instead of at the ideal synthetic spot, so
// this never fights the user's own drag; nothing here is ever written back
// to the file on its own (see App.tsx's `touchedNodeIds`/`handleSaveLayout`).

import type { CanvasDoc, CanvasNode } from "./types";
import { findRoot, subtreeIds, visibleNodeIds } from "./tree";

const H_GAP = 80;
const V_GAP = 25;
const GROUP_PADDING = 40;
const GROUP_TITLE_SPACE = 40;
/** A folded node's own box height, regardless of its real/measured content
 * height — small enough to read as "just a title row" (comparable to
 * `GROUP_TITLE_SPACE`), and forced into the stacking math below so a
 * folded node's later siblings actually reflow up into the space its
 * hidden subtree used to occupy, instead of leaving a gap the size of its
 * un-folded content. */
export const FOLDED_HEIGHT = 44;
/** Direct children of root get only a small nudge right of it, reading
 * top-to-bottom like a document's title followed by its headings — mirrors
 * `layout.rs`'s `ROOT_CHILD_INDENT`. */
const ROOT_CHILD_INDENT = 32;

/** Root (tree depth 0) and its direct children (depth 1) share this
 * fraction of the viewport's width. */
const WIDTH_RATIO_SHALLOW = 0.6;
/** Everything deeper (depth ≥2) gets this fraction instead, uniformly
 * regardless of how much deeper. */
const WIDTH_RATIO_DEEP = 0.55;
/** A depth-≥2 node's content-driven height is capped here (see the module
 * doc comment) — root/depth-1 nodes are never capped. */
const MAX_HEIGHT_DEEP = 480;
/** Used only for a node that hasn't been measured yet (its very first
 * render) — self-corrects the moment a real `measured.height` lands. */
const UNMEASURED_PLACEHOLDER_HEIGHT = 100;

export interface LayoutBox {
  x: number;
  y: number;
  width: number;
  height: number;
  /** A group's coordinate origin. Its visible frame can start before this
   * point to leave room for padding and the title. */
  anchorX?: number;
  anchorY?: number;
  /** Set only for an auto-placed (no real `height`) node at depth ≥2 — the
   * caller should apply this as a CSS `max-height` so the box still grows
   * with content up to the cap rather than always rendering at it. */
  maxHeight?: number;
}

export interface AutoLayoutInput {
  canvas: CanvasDoc;
  /** Typically `window.innerWidth`, read once per canvas load — see
   * App.tsx; this module has no opinion on when the caller re-invokes it. */
  viewportWidth: number;
  /** React Flow's own `node.measured.height` for a node that's been
   * rendered at least once, if any. */
  measuredHeight: (nodeId: string) => number | undefined;
  /** Node ids whose subtree is currently folded (see App.tsx's fold
   * feature) — a view-only concern, never written to the file. Every
   * descendant of a folded node is excluded from this layout pass
   * entirely, so an auto-placed sibling that comes after a folded subtree
   * flows up to fill the gap; the folded node itself still gets a box
   * (see `FOLDED_HEIGHT`), just a compact one. */
  foldedNodeIds?: ReadonlySet<string>;
}

function directChildren(canvas: CanvasDoc, id: string): CanvasNode[] {
  return canvas.nodes.filter((n) => n.parent === id);
}

function widthForDepth(depth: number, viewportWidth: number): number {
  return depth <= 1 ? viewportWidth * WIDTH_RATIO_SHALLOW : viewportWidth * WIDTH_RATIO_DEEP;
}

/** A fresh layout for every currently-visible node in `canvas` (excluding
 * any folded node's descendants), keyed by node id — see the module doc
 * comment for the shape. */
export function computeAutoLayout({
  canvas,
  viewportWidth,
  measuredHeight,
  foldedNodeIds,
}: AutoLayoutInput): Map<string, LayoutBox> {
  const boxes = new Map<string, LayoutBox>();
  const folded = foldedNodeIds ?? new Set<string>();
  const visible = new Set(visibleNodeIds(canvas, folded));
  const view: CanvasDoc = { ...canvas, nodes: canvas.nodes.filter((n) => visible.has(n.id)) };
  const root = findRoot(view);
  if (!root) return boxes;

  // Real height/measured height/placeholder, in that order — clamped to
  // `cap` (depth ≥2 only) only when it isn't a real, authored height: the
  // user's own explicit size is never second-guessed. A folded node is the
  // one exception: its own compact box always wins, even over a real
  // authored height, since folding is a display-only override.
  function sizeFor(node: CanvasNode, depth: number): { width: number; height: number; maxHeight?: number } {
    const width = node.width ?? widthForDepth(depth, viewportWidth);
    if (folded.has(node.id)) {
      return { width, height: FOLDED_HEIGHT };
    }
    if (node.height !== undefined) {
      return { width, height: node.height };
    }
    const measured = measuredHeight(node.id) ?? UNMEASURED_PLACEHOLDER_HEIGHT;
    if (depth < 2) {
      return { width, height: measured };
    }
    return { width, height: Math.min(measured, MAX_HEIGHT_DEEP), maxHeight: MAX_HEIGHT_DEEP };
  }

  const rootSize = sizeFor(root, 0);
  boxes.set(root.id, { x: 0, y: 0, ...rootSize });

  let yCursor = rootSize.height;
  for (const section of directChildren(view, root.id)) {
    yCursor += V_GAP;
    // `null`: root's own direct children are never inside a group (root
    // has no parent, so nothing above it could be one either).
    yCursor += placeRightward(view, section, ROOT_CHILD_INDENT, yCursor, 1, null, sizeFor, boxes, folded);
  }

  // `placeRightward` resolves group frames as it unwinds so each caller can
  // reserve the frame's actual height before placing the next sibling. The
  // root is normally a text node; handle a group root as well.
  if (root.type === "group") layoutGroupBox(view, root, boxes);
  return boxes;
}

/** Lays out `node` and its full subtree at column `x`/depth `depth`: `node`
 * itself goes at `x`, its children recurse one column further right at
 * `depth + 1`, stacking vertically among themselves. Returns the total
 * vertical space the subtree consumed, so a caller placing multiple
 * siblings in the same column can advance its own cursor without
 * overlapping this one — mirrors `layout.rs`'s `place_rightward`.
 *
 * `groupOrigin` is non-null exactly when `node`'s own structural parent is
 * a `group` — its own just-resolved anchor, real or synthetic (always
 * concrete by the time it's passed down). A group member's own real x/y is
 * relative to that anchor, not the whole document (see SPEC.md and
 * `layout.rs`'s own `place_rightward`, this pass's Rust equivalent) — null
 * for anything not a direct group child leaves a real position exactly as
 * authored, same as before this existed. */
function placeRightward(
  canvas: CanvasDoc,
  node: CanvasNode,
  x: number,
  y: number,
  depth: number,
  groupOrigin: { x: number; y: number } | null,
  sizeFor: (node: CanvasNode, depth: number) => { width: number; height: number; maxHeight?: number },
  boxes: Map<string, LayoutBox>,
  folded: ReadonlySet<string>,
): number {
  const size = sizeFor(node, depth);
  const [nx, ny] =
    node.x !== undefined && node.y !== undefined
      ? groupOrigin
        ? [groupOrigin.x + node.x, groupOrigin.y + node.y]
        : [node.x, node.y]
      : [x, y];
  // Every *direct* child of a `group` gets this node's own just-resolved
  // (nx, ny) as its own `groupOrigin` — nested one level deeper (a group
  // member's own child) reverts to plain absolute coordinates, matching
  // `web/src/tree.ts`'s `deriveEdges`, which only suppresses the
  // structural nesting line one level down too. A nested group's own
  // members still compose correctly: the inner group's own anchor (itself
  // resolved via the *outer* group's `groupOrigin`) becomes `groupOrigin`
  // for the inner group's own children in turn.
  const childGroupOrigin = node.type === "group" ? { x: nx, y: ny } : null;
  const children = directChildren(canvas, node.id);

  if (children.length === 0) {
    boxes.set(node.id, { x: nx, y: ny, ...size });
    return size.height;
  }

  // A group's frame wraps its members with padding and a title row. Start
  // auto-placed members after that reserved space. Positioned members can
  // start elsewhere, so measure the actual frame before placing siblings.
  const isGroup = node.type === "group";
  const topReserve = isGroup ? GROUP_PADDING + GROUP_TITLE_SPACE : 0;
  const bottomReserve = isGroup ? GROUP_PADDING : 0;

  // A group's own frame directly wraps its members — its
  // children indent only `GROUP_PADDING` from that same left edge, not the
  // normal `size.width + H_GAP` column-jump every other parent/child pair
  // gets (each rendered as its own separate box, side by side, one tier
  // deeper). Using the normal jump here still positioned every member
  // correctly (the frame's bounding box just follows wherever they
  // landed) — it just left a wide dead gap between the frame's own
  // (correctly self-aligned) left edge and the far-off column its members
  // actually rendered in, since nothing here was previously reserving the
  // *frame*'s edge as the actual left edge to indent from.
  const childX = isGroup ? nx + GROUP_PADDING : nx + size.width + H_GAP;
  let cursor = ny + topReserve;
  let span = 0;
  children.forEach((child, i) => {
    if (i > 0) {
      cursor += V_GAP;
      span += V_GAP;
    }
    const childH = placeRightward(canvas, child, childX, cursor, depth + 1, childGroupOrigin, sizeFor, boxes, folded);
    cursor += childH;
    span += childH;
  });
  // A group's own `size.height` is deliberately left out of its `consumed`
  // — that figure is `measuredHeight(node.id)`, last render's real DOM
  // height of whatever `layoutGroupBox` set this same box to on the
  // *previous* pass, so feeding it back into this pass's own layout math
  // never converges (confirmed directly: an unfolded group's height blew
  // up to several thousand px within a couple of reflow passes before this
  // was excluded). `topReserve`/`bottomReserve` stand in for it here
  // instead, covering the frame's own overhang on both ends of the stack.
  const consumed = isGroup ? topReserve + span + bottomReserve : Math.max(span, size.height);
  // Only recenter a synthetic y against the children's stacked span — a
  // real y is the user's own, never shifted to "look centered". Skipped
  // for a group entirely (`nodeY = ny`, no centering): whenever this
  // branch runs at all, the group has ≥1 visible child, which means
  // `layoutGroupBox` is about to override this box wholesale from its
  // members' own boxes anyway (see that function) — nothing stored here
  // survives, so centering against a stale `size.height` would just be
  // reintroducing that same feedback risk for a value nothing downstream
  // even reads.
  const nodeY = node.y !== undefined || isGroup ? ny : ny + (consumed - size.height) / 2;
  boxes.set(node.id, { x: nx, y: nodeY, ...size });
  if (!isGroup) return consumed;

  let frame = layoutGroupBox(canvas, node, boxes);
  if (!frame) return consumed;
  // For an auto-placed group, the caller's (x, y) is the *frame* slot.
  // Explicit member coordinates such as x=0/y=0 can otherwise pull the
  // frame left/up out of that slot, even though the children were initially
  // laid out with padding in mind. Move the whole subtree together, keeping
  // the children's coordinates relative to the logical group anchor.
  if (node.x === undefined || node.y === undefined) {
    const dx = x - frame.x;
    const dy = y - frame.y;
    for (const id of [node.id, ...subtreeIds(canvas, node.id)]) {
      const box = boxes.get(id);
      if (!box) continue;
      boxes.set(id, {
        ...box,
        x: box.x + dx,
        y: box.y + dy,
        anchorX: box.anchorX === undefined ? undefined : box.anchorX + dx,
        anchorY: box.anchorY === undefined ? undefined : box.anchorY + dy,
      });
    }
    frame = boxes.get(node.id)!;
  }
  return Math.max(0, frame.y + frame.height - y);
}

/** Wraps a group's full subtree in a frame, preserving its logical anchor.
 * Descendant groups have already been resolved by the recursive placement. */
function layoutGroupBox(
  canvas: CanvasDoc,
  group: CanvasNode,
  boxes: Map<string, LayoutBox>,
): LayoutBox | undefined {
  // Member boxes are absolute, including authored coordinates resolved
  // through the group's logical anchor. That anchor is kept separately:
  // the frame can extend above or left of it when a member has x=0/y=0.
  const memberBoxes = subtreeIds(canvas, group.id)
    .map((id) => boxes.get(id))
    .filter((b): b is LayoutBox => b !== undefined);
  if (memberBoxes.length === 0) return undefined;
  const minX = Math.min(...memberBoxes.map((b) => b.x));
  const minY = Math.min(...memberBoxes.map((b) => b.y));
  const maxX = Math.max(...memberBoxes.map((b) => b.x + b.width));
  const maxY = Math.max(...memberBoxes.map((b) => b.y + b.height));
  const origin = boxes.get(group.id);
  const frame: LayoutBox = {
    x: minX - GROUP_PADDING,
    y: minY - GROUP_PADDING - GROUP_TITLE_SPACE,
    width: maxX - minX + GROUP_PADDING * 2,
    height: maxY - minY + GROUP_PADDING * 2 + GROUP_TITLE_SPACE,
    anchorX: origin?.anchorX ?? origin?.x,
    anchorY: origin?.anchorY ?? origin?.y,
  };
  boxes.set(group.id, frame);
  return frame;
}
