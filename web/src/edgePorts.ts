import type { LayoutBox } from "./autolayout";
import type { DerivedEdge } from "./tree";

export type EdgeSide = "left" | "right" | "top" | "bottom";
export interface EdgePorts {
  sourceHandle: string;
  targetHandle: string;
  sourceOffset: number;
  targetOffset: number;
}

/** Automatically placed outgoing edges share the centered source handle.
 * Explicit source sides and incoming edges can still spread along an
 * occupied side. Offsets are relative to the centered React Flow handle. */
export function distributeEdgePorts(
  edges: DerivedEdge[],
  boxes: Map<string, LayoutBox>,
  levels: Map<string, number>,
): Map<string, EdgePorts> {
  const result = new Map<string, EdgePorts>();
  const groups = new Map<string, { edgeId: string; end: "source" | "target"; order: number }[]>();
  const automaticSources = new Set<string>();
  const center = (box: LayoutBox, side: EdgeSide) =>
    side === "top" || side === "bottom" ? box.x + box.width / 2 : box.y + box.height / 2;

  for (const edge of edges) {
    const source = boxes.get(edge.source);
    const target = boxes.get(edge.target);
    if (!source || !target) continue;
    const defaultSourceSide = levels.get(edge.source) === 1 ? "left" : "right";
    let sourceSide: EdgeSide = defaultSourceSide;
    let targetSide: EdgeSide = "left";
    if (edge.extra && (source.y + source.height <= target.y || target.y + target.height <= source.y)) {
      const overlap = Math.min(source.x + source.width, target.x + target.width) - Math.max(source.x, target.x);
      // Stacked cards in one column need a visible side lane rather than
      // a tiny vertical link hidden in the gap between their wide bodies.
      if (overlap >= Math.min(source.width, target.width) / 2) {
        sourceSide = "right";
        targetSide = "right";
      } else {
        sourceSide = source.y < target.y ? "bottom" : "top";
        targetSide = source.y < target.y ? "top" : "bottom";
      }
    }
    sourceSide = edge.sourceSide ?? sourceSide;
    targetSide = edge.targetSide ?? targetSide;
    if (!edge.sourceSide) automaticSources.add(`${edge.source}\0${sourceSide}`);
    result.set(edge.id, {
      sourceHandle: !edge.sourceSide && sourceSide === defaultSourceSide ? "source-default" : `source-${sourceSide}`,
      targetHandle: targetSide === "left" ? "target-default" : `target-${targetSide}`,
      sourceOffset: 0,
      targetOffset: 0,
    });
    for (const [nodeId, side, end, other] of [
      ...(edge.sourceSide ? [[edge.source, sourceSide, "source", target] as const] : []),
      [edge.target, targetSide, "target", source] as const,
    ]) {
      const key = `${nodeId}\0${side}`;
      const group = groups.get(key) ?? [];
      group.push({ edgeId: edge.id, end, order: center(other, side) });
      groups.set(key, group);
    }
  }

  for (const [key, group] of groups) {
    const reserveCenter = automaticSources.has(key) && group.some(item => item.end === "target");
    if (group.length < 2 && !reserveCenter) continue;
    const [nodeId, side] = key.split("\0") as [string, EdgeSide];
    const box = boxes.get(nodeId)!;
    const length = side === "top" || side === "bottom" ? box.width : box.height;
    // Keep endpoints away from rounded corners. Dense groups compress
    // rather than letting their outer endpoints leave the node's border.
    const halfSpan = Math.max(0, length / 2 - 6);
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
    // Automatic outgoing links retain their shared center. Incoming tips
    // must not retrace that exit stub, even with just one incoming link.
    if (reserveCenter) {
      const slots = [...positions, 0].sort((a, b) => a - b);
      const reserved = slots.indexOf(0);
      const pitch = Math.min(40, halfSpan / Math.max(reserved, group.length - reserved, 1));
      const available = slots.filter((_, i) => i !== reserved).map((value, i) => {
        const index = i < reserved ? i : i + 1;
        const lower = index < reserved ? -halfSpan + index * pitch : (index - reserved) * pitch;
        const upper = index < reserved ? -(reserved - index) * pitch : halfSpan - (group.length - index) * pitch;
        return Math.max(lower, Math.min(upper, value));
      });
      for (let i = 1; i < available.length; i++) available[i] = Math.max(available[i], available[i - 1] + pitch);
      positions.splice(0, positions.length, ...available);
    }
    group.forEach(({ edgeId, end }, index) => {
      const ports = result.get(edgeId)!;
      ports[`${end}Offset`] = positions[index];
    });
  }
  return result;
}
