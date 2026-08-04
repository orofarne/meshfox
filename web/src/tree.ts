// Client-side mirror of crates/core/src/tree.rs — nodes are addressed
// (and here, laid out) by `id` and `parent`, same as the Rust model.

import type { CanvasDoc, CanvasNode } from "./types";

export function findRoot(canvas: CanvasDoc): CanvasNode | undefined {
  return canvas.nodes.find((n) => !n.parent);
}

/** Node-id path from the root's children down to `nodeId`, as expected by /api/run. */
export function pathTo(canvas: CanvasDoc, nodeId: string): string[] {
  const byId = new Map(canvas.nodes.map((n) => [n.id, n]));
  const path: string[] = [];
  let current = byId.get(nodeId);
  while (current?.parent) {
    path.unshift(current.id);
    current = byId.get(current.parent);
  }
  return path;
}

/** All descendant ids of `nodeId` (children, grandchildren, ...) — used to
 * find which nodes need to move along when a group is dragged, since a
 * group's box is just the derived bounding box of its full subtree (see
 * `layout.rs`'s `layout_groups`), not something stored in its own right. */
export function subtreeIds(canvas: CanvasDoc, nodeId: string): string[] {
  const out: string[] = [];
  const stack = canvas.nodes.filter((n) => n.parent === nodeId).map((n) => n.id);
  while (stack.length > 0) {
    const id = stack.pop()!;
    out.push(id);
    for (const n of canvas.nodes) {
      if (n.parent === id) stack.push(n.id);
    }
  }
  return out;
}

export interface DerivedEdge {
  id: string;
  source: string;
  target: string;
  /** false for the implicit nesting edge, true for a `meshfox:edge` extra. */
  extra: boolean;
  /** Per-edge styling — only ever set when `extra` is true (see
   * `ExtraEdgeDto`). */
  label?: string;
  color?: string;
  style?: "solid" | "dashed" | "dotted";
  arrowStart?: "none" | "arrow";
  arrowEnd?: "none" | "arrow";
  tags?: string[];
}

/** All edges implied by the tree (`parent`) plus `extraParents`, for rendering. */
export function deriveEdges(canvas: CanvasDoc): DerivedEdge[] {
  const byId = new Map(canvas.nodes.map((n) => [n.id, n]));
  const edges: DerivedEdge[] = [];
  for (const n of canvas.nodes) {
    if (n.parent) {
      // A group shows containment spatially (its children render inside its
      // box) — drawing a nesting line on top of that would just be clutter.
      // Extra `meshfox:edge` relations are a deliberate author statement
      // regardless of type, so those are never suppressed.
      const isGroupParent = byId.get(n.parent)?.type === "group";
      if (!isGroupParent) {
        edges.push({ id: `${n.parent}->${n.id}`, source: n.parent, target: n.id, extra: false });
      }
    }
    for (const extra of n.extraParents ?? []) {
      edges.push({
        id: `${extra.from}->${n.id}:extra`,
        source: extra.from,
        target: n.id,
        extra: true,
        label: extra.label,
        color: extra.color,
        style: extra.style,
        arrowStart: extra.arrowStart,
        arrowEnd: extra.arrowEnd,
        tags: extra.tags,
      });
    }
  }
  return edges;
}
