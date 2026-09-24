import type { Edge, Node } from "@xyflow/react";
import type { MeshNodeData } from "./MeshNode";
import type { DeletableEdgeData } from "./DeletableEdge";
import type { EdgeSide } from "./edgePorts";
import { draw, routeAroundNodes, type Point, type Rect, type Segment } from "./edgeRouting";

function sideFor(handle: string | null | undefined, source: boolean, level: number): EdgeSide {
  if (handle?.endsWith("-top")) return "top";
  if (handle?.endsWith("-bottom")) return "bottom";
  return source ? (level === 1 ? "left" : "right") : "left";
}

function endpoint(box: Rect, side: EdgeSide, offset: number): Point {
  switch (side) {
    case "left": return { x: box.x - 2, y: box.y + box.height / 2 + offset };
    case "right": return { x: box.x + box.width + 2, y: box.y + box.height / 2 + offset };
    case "top": return { x: box.x + box.width / 2 + offset, y: box.y - 2 };
    case "bottom": return { x: box.x + box.width / 2 + offset, y: box.y + box.height + 2 };
  }
}

/** Routes extra edges and manually adjusted tree edges in stable id order, treating each previous
 * route as a soft obstacle. The edge data returned here is presentation-only;
 * no authored canvas field or React Flow edge state is changed. */
export function withEdgeRoutes(nodes: Node<MeshNodeData>[], edges: Edge[]): Edge[] {
  const byId = new Map(nodes.map((node) => [node.id, node]));
  const positions = new Map<string, Point>();
  const absolute = (id: string): Point => {
    const cached = positions.get(id);
    if (cached) return cached;
    const node = byId.get(id)!;
    const parent = node.parentId && byId.has(node.parentId) ? absolute(node.parentId) : { x: 0, y: 0 };
    const point = { x: parent.x + node.position.x, y: parent.y + node.position.y };
    positions.set(id, point);
    return point;
  };
  const boxes = new Map<string, Rect>();
  const solidIds = new Set<string>();
  for (const node of nodes) {
    const width = node.measured?.width ?? node.width;
    const height = node.measured?.height ?? node.height;
    if (width && height) {
      boxes.set(node.id, { ...absolute(node.id), width, height });
      if (node.data.nodeType !== "group") solidIds.add(node.id);
    }
  }
  const occupied: Segment[] = [];
  const routes = new Map<string, [string, number, number, Point[]]>();
  const failedManual = new Set<string>();
  for (const edge of [...edges].filter((e) => {
    const data = e.data as DeletableEdgeData | undefined;
    return e.type === "extra" || (e.type === "tree" && !!(data?.via?.length || data?.sourceSide || data?.targetSide));
  }).sort((a, b) => a.id.localeCompare(b.id))) {
    const sourceBox = boxes.get(edge.source), targetBox = boxes.get(edge.target);
    if (!sourceBox || !targetBox) continue;
    const data = edge.data as DeletableEdgeData | undefined;
    const sourceSide = sideFor(edge.sourceHandle, true, byId.get(edge.source)?.data.level ?? 0);
    const targetSide = sideFor(edge.targetHandle, false, byId.get(edge.target)?.data.level ?? 0);
    const start = endpoint(sourceBox, sourceSide, data?.sourceOffset ?? 0);
    const end = endpoint(targetBox, targetSide, data?.targetOffset ?? 0);
    const obstacles = [...boxes].filter(([id]) => solidIds.has(id) && id !== edge.source && id !== edge.target).map(([, box]) => box);
    const via = (data?.via ?? []).map((point) => ({ x: start.x + point.x, y: start.y + point.y }));
    const stops = [start, ...via, end];
    const legs = stops.slice(1).map((stop, index) => routeAroundNodes(
      stops[index], index === 0 ? sourceSide : "right",
      stop, index === stops.length - 2 ? targetSide : "left",
      obstacles,
      index === 0 && solidIds.has(edge.source) ? sourceBox : undefined,
      index === stops.length - 2 && solidIds.has(edge.target) ? targetBox : undefined,
      occupied, index > 0, index < stops.length - 2,
    ));
    let route = legs.every(Boolean)
      ? draw(legs.flatMap((leg, index) => index === 0 ? leg![3] : leg![3].slice(1)))
      : undefined;
    if (!route && (via.length || data?.sourceSide || data?.targetSide)) {
      failedManual.add(edge.id);
      // Keep a blocked waypoint from forcing the old bezier through a node.
      route = routeAroundNodes(start, sourceSide, end, targetSide, obstacles,
        solidIds.has(edge.source) ? sourceBox : undefined,
        solidIds.has(edge.target) ? targetBox : undefined, occupied);
    }
    if (!route) continue;
    routes.set(edge.id, route);
    for (let i = 1; i < route[3].length; i++) occupied.push({ from: route[3][i - 1], to: route[3][i] });
  }
  return edges.map((edge) => {
    const route = routes.get(edge.id);
    return route
      ? { ...edge, data: { ...edge.data, routedPath: [route[0], route[1], route[2]], routedPoints: route[3], routeFailed: failedManual.has(edge.id) } }
      : failedManual.has(edge.id) ? { ...edge, data: { ...edge.data, routeFailed: true } } : edge;
  });
}
