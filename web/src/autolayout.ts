// Client-side auto-layout for whatever's still unpositioned in the canvas —
// crates/core/src/layout.rs's counterpart used to fill this role over the
// API (`suggestedX`/etc.), but that heuristic-based estimate has no way to
// know the browser's actual viewport width or a node's real rendered
// content height, both of which this module leans on directly. `layout.rs`
// itself is untouched and still backs `meshfox fmt` — the two are
// deliberately independent now, not required to agree pixel-for-pixel.
//
// Same overall shape as `layout.rs::compute` (root + its direct children
// read top-to-bottom in one column, deeper nesting branches right of its
// own parent, siblings stack vertically without overlapping, a subtree's
// full consumed height bubbles up to its parent so unrelated branches never
// collide, a `group`'s box is always the bounding box of its resolved
// members) — but swaps in real inputs for the two things `layout.rs` could
// only estimate:
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
// real `height` — anchors there instead of at the ideal synthetic spot,
// same "don't fight the user's own drag" rule `layout.rs` has; nothing here
// is ever written back to the file on its own (see App.tsx's
// `touchedNodeIds`/`handleSaveLayout`).

import type { CanvasDoc, CanvasNode } from "./types";
import { findRoot, subtreeIds } from "./tree";

const H_GAP = 80;
const V_GAP = 60;
const GROUP_PADDING = 40;
const GROUP_TITLE_SPACE = 40;
/** Direct children of root get only a small nudge right of it, reading
 * top-to-bottom like a document's title followed by its headings — mirrors
 * `layout.rs`'s `ROOT_CHILD_INDENT`. */
const ROOT_CHILD_INDENT = 32;

/** Root (tree depth 0) and its direct children (depth 1) share this
 * fraction of the viewport's width. */
const WIDTH_RATIO_SHALLOW = 0.6;
/** Everything deeper (depth ≥2) gets this fraction instead, uniformly
 * regardless of how much deeper. */
const WIDTH_RATIO_DEEP = 0.4;
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
}

function directChildren(canvas: CanvasDoc, id: string): CanvasNode[] {
  return canvas.nodes.filter((n) => n.parent === id);
}

function treeDepth(canvas: CanvasDoc, id: string): number {
  const byId = new Map(canvas.nodes.map((n) => [n.id, n]));
  let depth = 0;
  let cur = byId.get(id);
  while (cur?.parent) {
    depth++;
    cur = byId.get(cur.parent);
  }
  return depth;
}

function widthForDepth(depth: number, viewportWidth: number): number {
  return depth <= 1 ? viewportWidth * WIDTH_RATIO_SHALLOW : viewportWidth * WIDTH_RATIO_DEEP;
}

/** A fresh layout for every node in `canvas`, keyed by node id — see the
 * module doc comment for the shape. */
export function computeAutoLayout({ canvas, viewportWidth, measuredHeight }: AutoLayoutInput): Map<string, LayoutBox> {
  const boxes = new Map<string, LayoutBox>();
  const root = findRoot(canvas);
  if (!root) return boxes;

  // Real height/measured height/placeholder, in that order — clamped to
  // `cap` (depth ≥2 only) only when it isn't a real, authored height: the
  // user's own explicit size is never second-guessed.
  function sizeFor(node: CanvasNode, depth: number): { width: number; height: number; maxHeight?: number } {
    const width = node.width ?? widthForDepth(depth, viewportWidth);
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
  for (const section of directChildren(canvas, root.id)) {
    yCursor += V_GAP;
    yCursor += placeRightward(canvas, section, ROOT_CHILD_INDENT, yCursor, 1, sizeFor, boxes);
  }

  layoutGroups(canvas, boxes, sizeFor);
  return boxes;
}

/** Lays out `node` and its full subtree at column `x`/depth `depth`: `node`
 * itself goes at `x`, its children recurse one column further right at
 * `depth + 1`, stacking vertically among themselves. Returns the total
 * vertical space the subtree consumed, so a caller placing multiple
 * siblings in the same column can advance its own cursor without
 * overlapping this one — mirrors `layout.rs`'s `place_rightward`. */
function placeRightward(
  canvas: CanvasDoc,
  node: CanvasNode,
  x: number,
  y: number,
  depth: number,
  sizeFor: (node: CanvasNode, depth: number) => { width: number; height: number; maxHeight?: number },
  boxes: Map<string, LayoutBox>,
): number {
  const size = sizeFor(node, depth);
  const [nx, ny] = node.x !== undefined && node.y !== undefined ? [node.x, node.y] : [x, y];
  const children = directChildren(canvas, node.id);

  if (children.length === 0) {
    boxes.set(node.id, { x: nx, y: ny, ...size });
    return size.height;
  }

  const childX = nx + size.width + H_GAP;
  let cursor = ny;
  let span = 0;
  children.forEach((child, i) => {
    if (i > 0) {
      cursor += V_GAP;
      span += V_GAP;
    }
    const childH = placeRightward(canvas, child, childX, cursor, depth + 1, sizeFor, boxes);
    cursor += childH;
    span += childH;
  });
  const consumed = Math.max(span, size.height);
  // Only recenter a synthetic y against the children's stacked span — a
  // real y is the user's own, never shifted to "look centered".
  const nodeY = node.y !== undefined ? ny : ny + (consumed - size.height) / 2;
  boxes.set(node.id, { x: nx, y: nodeY, ...size });
  return consumed;
}

/** Overrides every group's box with the bounding box of its full subtree —
 * mirrors `layout.rs`'s `layout_groups`, deepest groups first so a
 * group-of-groups sees its nested group's already-resolved box. */
function layoutGroups(
  canvas: CanvasDoc,
  boxes: Map<string, LayoutBox>,
  sizeFor: (node: CanvasNode, depth: number) => { width: number; height: number; maxHeight?: number },
) {
  const groups = canvas.nodes
    .filter((n) => n.type === "group")
    .sort((a, b) => treeDepth(canvas, b.id) - treeDepth(canvas, a.id));

  for (const group of groups) {
    const memberBoxes = subtreeIds(canvas, group.id)
      .map((id) => memberBox(canvas, id, boxes, sizeFor))
      .filter((b): b is LayoutBox => b !== undefined);
    if (memberBoxes.length === 0) continue;
    const minX = Math.min(...memberBoxes.map((b) => b.x));
    const minY = Math.min(...memberBoxes.map((b) => b.y));
    const maxX = Math.max(...memberBoxes.map((b) => b.x + b.width));
    const maxY = Math.max(...memberBoxes.map((b) => b.y + b.height));
    boxes.set(group.id, {
      x: minX - GROUP_PADDING,
      y: minY - GROUP_PADDING - GROUP_TITLE_SPACE,
      width: maxX - minX + GROUP_PADDING * 2,
      height: maxY - minY + GROUP_PADDING * 2 + GROUP_TITLE_SPACE,
    });
  }
}

/** A group member's box for bounding-box purposes: its real, authored (or
 * dragged) position/size when it has one — since that's what actually
 * renders on screen — falling back to the synthetic layout box only for a
 * member that's still unpositioned. Mirrors `layout.rs`'s `member_box`. */
function memberBox(
  canvas: CanvasDoc,
  id: string,
  boxes: Map<string, LayoutBox>,
  sizeFor: (node: CanvasNode, depth: number) => { width: number; height: number; maxHeight?: number },
): LayoutBox | undefined {
  const node = canvas.nodes.find((n) => n.id === id);
  if (!node) return undefined;
  if (node.x !== undefined && node.y !== undefined) {
    const size = sizeFor(node, treeDepth(canvas, id));
    return { x: node.x, y: node.y, ...size };
  }
  return boxes.get(id);
}
