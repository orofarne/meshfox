import type { LayoutBox } from "./autolayout";
import type { DerivedEdge } from "./tree";

export type EdgeSide = "left" | "right" | "top" | "bottom";
export interface EdgePorts {
  sourceHandle: string;
  targetHandle: string;
  sourceOffset: number;
  targetOffset: number;
}

/** Spread connections along each occupied side, in the order of their other
 * endpoints. Offsets are relative to the existing centered React Flow handle;
 * the visible path uses them without changing the connection UI. */
export function distributeEdgePorts(
  edges: DerivedEdge[],
  boxes: Map<string, LayoutBox>,
  levels: Map<string, number>,
): Map<string, EdgePorts> {
  const result = new Map<string, EdgePorts>();
  const groups = new Map<string, { edgeId: string; end: "source" | "target"; order: number }[]>();
  const center = (box: LayoutBox, side: EdgeSide) =>
    side === "top" || side === "bottom" ? box.x + box.width / 2 : box.y + box.height / 2;

  for (const edge of edges) {
    const source = boxes.get(edge.source);
    const target = boxes.get(edge.target);
    if (!source || !target) continue;
    let sourceSide: EdgeSide = levels.get(edge.source) === 1 ? "left" : "right";
    let targetSide: EdgeSide = "left";
    if (edge.extra && (source.y + source.height <= target.y || target.y + target.height <= source.y)) {
      sourceSide = source.y < target.y ? "bottom" : "top";
      targetSide = source.y < target.y ? "top" : "bottom";
    }
    result.set(edge.id, {
      sourceHandle: sourceSide === "left" || sourceSide === "right" ? "source-default" : `source-${sourceSide}`,
      targetHandle: targetSide === "left" ? "target-default" : `target-${targetSide}`,
      sourceOffset: 0,
      targetOffset: 0,
    });
    for (const [nodeId, side, end, other] of [
      [edge.source, sourceSide, "source", target],
      [edge.target, targetSide, "target", source],
    ] as const) {
      const key = `${nodeId}\0${side}`;
      const group = groups.get(key) ?? [];
      group.push({ edgeId: edge.id, end, order: center(other, side) });
      groups.set(key, group);
    }
  }

  for (const [key, group] of groups) {
    if (group.length < 2) continue;
    const [nodeId, side] = key.split("\0") as [string, EdgeSide];
    const box = boxes.get(nodeId)!;
    const length = side === "top" || side === "bottom" ? box.width : box.height;
    // Keep endpoints away from rounded corners. Dense groups compress
    // rather than letting their outer endpoints leave the node's border.
    const halfSpan = Math.max(0, length / 2 - 20);
    // An arrowhead is wider than the old 18 px pitch. Use the other
    // endpoint's projected position when there is room, while keeping at
    // least 40 px between neighboring tips. Very crowded sides compress
    // only as much as their available length requires.
    const step = Math.min(40, (halfSpan * 2) / (group.length - 1));
    group.sort((a, b) => a.order - b.order || a.edgeId.localeCompare(b.edgeId) || a.end.localeCompare(b.end));
    const middle = center(box, side);
    const positions = group.map(({ order }) => Math.max(-halfSpan, Math.min(halfSpan, order - middle)));
    for (let i = 1; i < positions.length; i++) positions[i] = Math.max(positions[i], positions[i - 1] + step);
    positions[positions.length - 1] = Math.min(positions.at(-1)!, halfSpan);
    for (let i = positions.length - 2; i >= 0; i--) positions[i] = Math.min(positions[i], positions[i + 1] - step);
    group.forEach(({ edgeId, end }, index) => {
      const ports = result.get(edgeId)!;
      ports[`${end}Offset`] = positions[index];
    });
  }
  return result;
}
