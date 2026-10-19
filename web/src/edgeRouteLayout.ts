import type { Edge, Node } from "@xyflow/react";
import type { MeshNodeData } from "./MeshNode";
import type { DeletableEdgeData } from "./DeletableEdge";
import { distributeEdgePorts, type EdgeSide } from "./edgePorts.ts";
import { draw, routeAroundNodes, type Point, type Rect, type Segment } from "./edgeRouting.ts";
import { labelSize, labelRect, placeLabel } from "./edgeLabels.ts";
import { getSmoothStepPath, Position } from "@xyflow/react";

// The smooth-step renderer's quadratic controls are the orthogonal corners.
// Keep those corners for lane occupancy while retaining its original SVG.
function smoothStepPoints(path: string): Point[] {
  const tokens = path.match(/[MLQ]|-?\d+(?:\.\d+)?(?:e[+-]?\d+)?/gi) ?? [];
  const points: Point[] = [];
  for (let i = 0; i < tokens.length;) {
    const command = tokens[i++];
    points.push({ x: Number(tokens[i++]), y: Number(tokens[i++]) });
    if (command === "Q") i += 2;
  }
  return points;
}

function sideFor(handle: string | null | undefined, source: boolean, level: number): EdgeSide {
  if (handle?.endsWith("-left")) return "left";
  if (handle?.endsWith("-right")) return "right";
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
    // Derived group frames include route padding after this calculation;
    // use their base dimensions so measurement does not feed expansion back.
    const group = node.data.nodeType === "group";
    const width = group ? node.width ?? node.measured?.width : node.measured?.width ?? node.width;
    const height = group ? node.height ?? node.measured?.height : node.measured?.height ?? node.height;
    if (width && height) {
      boxes.set(node.id, { ...absolute(node.id), width, height });
      if (node.data.nodeType !== "group") solidIds.add(node.id);
    }
  }
  // Folding, measuring and dragging change geometry without rebuilding the
  // authored edge state. Choose automatic ports from the same live boxes as
  // the router, otherwise a stacked pair can retain a far-side attachment.
  const ports = distributeEdgePorts(edges.map((edge) => {
    const data = edge.data as DeletableEdgeData | undefined;
    return { id: edge.id, source: edge.source, target: edge.target,
      extra: edge.type === "extra", sourceSide: data?.sourceSide, targetSide: data?.targetSide };
  }), boxes, new Map(nodes.map((node) => [node.id, node.data.level])));
  edges = edges.map((edge) => {
    const port = ports.get(edge.id);
    return (edge.type === "extra" || edge.type === "tree") && port
      ? { ...edge, sourceHandle: port.sourceHandle, targetHandle: port.targetHandle,
          data: { ...edge.data, sourceOffset: port.sourceOffset, targetOffset: port.targetOffset } }
      : edge;
  });
  const occupied: Segment[] = [];
  const routes = new Map<string, [string, number, number, Point[]]>();
  const failedManual = new Set<string>();
  const plainTrees = new Set<string>();
  // Plain tree arrows keep their familiar smooth-step drawing, but occupy
  // lanes too. Otherwise extra links could be routed directly over them.
  for (const edge of edges.filter(edge => edge.type === "tree")) {
    const data = edge.data as DeletableEdgeData | undefined;
    if (data?.via?.length || data?.sourceSide || data?.targetSide || data?.label || data?.tags?.length) continue;
    const source = boxes.get(edge.source), target = boxes.get(edge.target);
    if (!source || !target) continue;
    const sourceSide = sideFor(edge.sourceHandle, true, byId.get(edge.source)?.data.level ?? 0);
    const targetSide = sideFor(edge.targetHandle, false, byId.get(edge.target)?.data.level ?? 0);
    const start = endpoint(source, sourceSide, data?.sourceOffset ?? 0), end = endpoint(target, targetSide, data?.targetOffset ?? 0);
    const path = getSmoothStepPath({ sourceX: start.x, sourceY: start.y, targetX: end.x, targetY: end.y,
      sourcePosition: sourceSide as Position, targetPosition: targetSide as Position });
    const points = smoothStepPoints(path[0]);
    routes.set(edge.id, [path[0], path[1], path[2], points]);
    plainTrees.add(edge.id);
    for (let i = 1; i < points.length; i++) occupied.push({ from: points[i - 1], to: points[i] });
  }
  for (const edge of [...edges].filter((e) => {
    const data = e.data as DeletableEdgeData | undefined;
    return e.type === "extra" || (e.type === "tree" && !!(data?.via?.length || data?.sourceSide || data?.targetSide || data?.label || data?.tags?.length));
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
    const labelClearance = data?.label || data?.tags?.length ? labelSize(data.label, data.tags).width / 2 + 14 : 14;
    const routeLegs = (badgeClearance: number) => stops.slice(1).map((stop, index) => routeAroundNodes(
      stops[index], index === 0 ? sourceSide : "right",
      stop, index === stops.length - 2 ? targetSide : "left",
      obstacles,
      index === 0 && solidIds.has(edge.source) ? sourceBox : undefined,
      index === stops.length - 2 && solidIds.has(edge.target) ? targetBox : undefined,
      occupied, index > 0, index < stops.length - 2, badgeClearance,
    ));
    let legs = routeLegs(labelClearance);
    // A badge must not make an otherwise routable narrow corridor fall back
    // to a bezier through cards. Placement still searches the entire path.
    if (!legs.every(Boolean) && labelClearance > 14) legs = routeLegs(14);
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
  const labels: Rect[] = [];
  const labelPositions = new Map<string, Point>();
  for (const edge of [...edges].sort((a, b) => a.id.localeCompare(b.id))) {
    const data = edge.data as DeletableEdgeData | undefined;
    const route = routes.get(edge.id);
    if (!route || !(data?.label || data?.tags?.length)) continue;
    const others = [...routes].filter(([id]) => id !== edge.id).flatMap(([, r]) =>
      r[3].slice(1).map((to, i) => ({ from: r[3][i], to })));
    const size = labelSize(data.label, data.tags);
    const point = placeLabel(route[3], size, data.labelAt ?? 500,
      [...boxes].filter(([id]) => solidIds.has(id)).map(([, box]) => box), labels, others);
    labelPositions.set(edge.id, point);
    labels.push(labelRect(point, size));
  }
  return edges.map((edge) => {
    const route = routes.get(edge.id);
    // A group's presentation frame can grow around routes after this pass.
    // Let its plain connector use React Flow's live frame endpoint.
    if (plainTrees.has(edge.id) && [edge.source, edge.target].some(id => byId.get(id)?.data.nodeType === "group")) {
      return { ...edge, data: { ...edge.data, routedPath: undefined, routedPoints: undefined } };
    }
    return route
      ? { ...edge, data: { ...edge.data, routedPath: [route[0], route[1], route[2]], routedPoints: route[3], routedLabel: labelPositions.get(edge.id), routeFailed: failedManual.has(edge.id) } }
      : failedManual.has(edge.id) ? { ...edge, data: { ...edge.data, routeFailed: true } } : edge;
  });
}

/** Presentation-only frames also contain internal arrow lanes and their badges.
 * Preserve every member's absolute position when a frame grows left/up. */
export function withRouteGroupFrames(nodes: Node<MeshNodeData>[], edges: Edge[]): Node<MeshNodeData>[] {
  const byId = new Map(nodes.map(n => [n.id, n]));
  const absolute = (node: Node<MeshNodeData>): Point => {
    const parent = node.parentId && byId.get(node.parentId);
    const origin = parent ? absolute(parent) : { x: 0, y: 0 };
    return { x: origin.x + node.position.x, y: origin.y + node.position.y };
  };
  const origins = new Map(nodes.map(n => [n.id, absolute(n)]));
  const frames = new Map<string, Rect>();
  const within = (id: string, group: string): boolean => {
    let node = byId.get(id);
    while (node?.parentId) {
      if (node.parentId === group) return true;
      node = byId.get(node.parentId);
    }
    return false;
  };
  const depth = (n: Node<MeshNodeData>): number => n.parentId && byId.has(n.parentId) ? 1 + depth(byId.get(n.parentId)!) : 0;
  for (const group of nodes.filter(n => n.data.nodeType === "group").sort((a, b) => depth(b) - depth(a))) {
    const origin = origins.get(group.id)!;
    const width = group.width ?? group.measured?.width, height = group.height ?? group.measured?.height;
    if (!width || !height) continue;
    let left = origin.x, top = origin.y, right = left + width, bottom = top + height;
    const include = (rect: Rect, padding: number, topPadding = padding) => {
      left = Math.min(left, rect.x - padding); top = Math.min(top, rect.y - topPadding);
      right = Math.max(right, rect.x + rect.width + padding); bottom = Math.max(bottom, rect.y + rect.height + padding);
    };
    for (const member of nodes) {
      if (!within(member.id, group.id)) continue;
      const frame = frames.get(member.id);
      const memberWidth = member.measured?.width ?? member.width, memberHeight = member.measured?.height ?? member.height;
      if (frame) include(frame, 40, 80);
      else if (memberWidth && memberHeight) include({ ...origins.get(member.id)!, width: memberWidth, height: memberHeight }, 40, 80);
    }
    for (const edge of edges) {
      if (!within(edge.source, group.id) || !within(edge.target, group.id)) continue;
      const data = edge.data as DeletableEdgeData | undefined;
      for (const point of data?.routedPoints ?? []) include({ ...point, width: 0, height: 0 }, 24, 64);
      if (data?.routedLabel) include(labelRect(data.routedLabel, labelSize(data.label, data.tags)), 24, 64);
    }
    frames.set(group.id, { x: left, y: top, width: right - left, height: bottom - top });
  }
  return nodes.map(node => {
    const frame = frames.get(node.id), origin = frame ?? origins.get(node.id)!;
    const parent = node.parentId && (frames.get(node.parentId) ?? origins.get(node.parentId));
    return { ...node, position: { x: origin.x - (parent ? parent.x : 0), y: origin.y - (parent ? parent.y : 0) },
      ...(frame ? { width: frame.width, height: frame.height, measured: { width: frame.width, height: frame.height } } : {}) };
  });
}

/** Keep the complete group frame clear of peers. Only automatic positions
 * move; a group carries its whole subtree, so member coordinates stay local. */
function groupCollisionShifts(base: Node<MeshNodeData>[], framed: Node<MeshNodeData>[]): Map<string, Point> {
  const shifts = new Map<string, Point>();
  if (base.some(n => n.dragging || n.resizing)) return shifts;
  const byId = new Map(framed.map(n => [n.id, n]));
  const baseById = new Map(base.map(n => [n.id, n]));
  const absolute = (n: Node<MeshNodeData>, map: Map<string, Node<MeshNodeData>>): Point => {
    const parent = n.parentId && map.get(n.parentId);
    const p = parent ? absolute(parent, map) : { x: 0, y: 0 };
    return { x: p.x + n.position.x, y: p.y + n.position.y };
  };
  const boxes = new Map(framed.map(n => [n.id, { ...absolute(n, byId),
    width: n.width ?? n.measured?.width ?? 0, height: n.measured?.height ?? n.height ?? 0 }]));
  const originals = new Map(base.map(n => [n.id, { ...absolute(n, baseById),
    width: n.width ?? n.measured?.width ?? 0, height: n.height ?? n.measured?.height ?? 0 }]));
  const movable = (n: Node<MeshNodeData>) => !!n.data.suggested && n.data.level > 1;
  const belongs = (id: string, ancestor: string): boolean => {
    let n = byId.get(id);
    while (n) {
      if (n.id === ancestor) return true;
      n = n.parentId ? byId.get(n.parentId) : undefined;
    }
    return false;
  };
  const gap = 25;
  for (const group of framed.filter(n => n.data.nodeType === "group")) {
    for (const other of framed) {
      if (other.id === group.id || other.parentId !== group.parentId) continue;
      const a = boxes.get(group.id)!, b = boxes.get(other.id)!;
      if (!a.width || !b.width || !a.height || !b.height ||
        a.x + a.width + gap <= b.x || b.x + b.width + gap <= a.x ||
        a.y + a.height + gap <= b.y || b.y + b.height + gap <= a.y) continue;
      if (!movable(group) && !movable(other)) continue;
      const originalA = originals.get(group.id)!, originalB = originals.get(other.id)!;
      const columns = originalA.x + originalA.width <= originalB.x || originalB.x + originalB.width <= originalA.x;
      let moving: Node<MeshNodeData>, delta: Point;
      if (columns && originalA.x > originalB.x && movable(group)) {
        moving = group; delta = { x: b.x + b.width + gap - a.x, y: 0 };
      } else if (columns && originalA.x <= originalB.x && movable(other)) {
        moving = other; delta = { x: a.x + a.width + gap - b.x, y: 0 };
      } else if (originalA.y <= originalB.y && movable(other)) {
        moving = other; delta = { x: 0, y: a.y + a.height + gap - b.y };
      } else if (movable(group)) {
        moving = group; delta = { x: 0, y: b.y + b.height + gap - a.y };
      } else {
        moving = other; delta = { x: 0, y: a.y + a.height + gap - b.y };
      }
      const previous = shifts.get(moving.id) ?? { x: 0, y: 0 };
      shifts.set(moving.id, { x: previous.x + delta.x, y: previous.y + delta.y });
      for (const [id, box] of boxes) if (belongs(id, moving.id)) { box.x += delta.x; box.y += delta.y; }
    }
  }
  return shifts;
}

/** Routing, frames and placement form one geometry calculation. Re-route
 * after shifting an automatic group/peer so arrow endpoints follow the move.
 * Keep layoutNodes for translating drag coordinates through frame expansion;
 * the automatic collision displacement is part of the visible position. */
export function withEdgeGeometry(nodes: Node<MeshNodeData>[], edges: Edge[]): {
  nodes: Node<MeshNodeData>[]; edges: Edge[]; layoutNodes: Node<MeshNodeData>[];
} {
  let layoutNodes = nodes;
  for (let pass = 0; pass < Math.min(32, nodes.length * 4 + 1); pass++) {
    const routes = withEdgeRoutes(layoutNodes, edges);
    const frames = withRouteGroupFrames(layoutNodes, routes);
    const shifts = groupCollisionShifts(layoutNodes, frames);
    if (!shifts.size) return { nodes: frames, edges: routes, layoutNodes };
    layoutNodes = layoutNodes.map(n => {
      const shift = shifts.get(n.id);
      return shift ? { ...n, position: { x: n.position.x + shift.x, y: n.position.y + shift.y } } : n;
    });
  }
  const routes = withEdgeRoutes(layoutNodes, edges);
  return { nodes: withRouteGroupFrames(layoutNodes, routes), edges: routes, layoutNodes };
}
