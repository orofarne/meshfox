import type { EdgeSide } from "./edgePorts";

export interface Point { x: number; y: number }
export interface Rect { x: number; y: number; width: number; height: number }
export interface Segment { from: Point; to: Point }
export type RoutedPath = [path: string, labelX: number, labelY: number, points: Point[]];

const CLEARANCE = 14;
const END_STUB = 24;
const ROUTE_MARGIN = 240;
const BEND_COST = 28;
const CORNER_RADIUS = 8;
const LANE_GAP = 20;

function outside(point: Point, side: EdgeSide, length: number): Point {
  switch (side) {
    case "left": return { x: point.x - length, y: point.y };
    case "right": return { x: point.x + length, y: point.y };
    case "top": return { x: point.x, y: point.y - length };
    case "bottom": return { x: point.x, y: point.y + length };
  }
}

function stubLength(point: Point, side: EdgeSide, own: Rect | undefined, others: Rect[]): number | undefined {
  const ownExit = own ? {
    left: point.x - own.x, right: own.x + own.width - point.x,
    top: point.y - own.y, bottom: own.y + own.height - point.y,
  }[side] + 2 : 0;
  let length = END_STUB;
  for (const r of others) {
    const transverse = side === "left" || side === "right"
      ? point.y > r.y && point.y < r.y + r.height
      : point.x > r.x && point.x < r.x + r.width;
    if (!transverse) continue;
    const distance = {
      left: point.x - (r.x + r.width), right: r.x - point.x,
      top: point.y - (r.y + r.height), bottom: r.y - point.y,
    }[side];
    if (distance >= 0) length = Math.min(length, distance - 2);
    else if (inside(point, r)) return undefined;
  }
  return length >= ownExit ? length : undefined;
}

function uniqueSorted(values: number[]): number[] {
  return [...new Set(values)].sort((a, b) => a - b);
}

function inside(p: Point, r: Rect): boolean {
  return p.x > r.x && p.x < r.x + r.width && p.y > r.y && p.y < r.y + r.height;
}

function segmentBlocked(a: Point, b: Point, obstacles: Rect[]): boolean {
  return obstacles.some((r) =>
    a.y === b.y
      ? a.y > r.y && a.y < r.y + r.height && Math.max(a.x, b.x) > r.x && Math.min(a.x, b.x) < r.x + r.width
      : a.x > r.x && a.x < r.x + r.width && Math.max(a.y, b.y) > r.y && Math.min(a.y, b.y) < r.y + r.height,
  );
}

class MinHeap {
  private items: { state: number; cost: number }[] = [];
  get length() { return this.items.length; }
  push(item: { state: number; cost: number }) {
    const a = this.items;
    let i = a.length;
    a.push(item);
    while (i > 0) {
      const p = (i - 1) >> 1;
      if (a[p].cost <= item.cost) break;
      a[i] = a[p]; i = p;
    }
    a[i] = item;
  }
  pop(): { state: number; cost: number } | undefined {
    const a = this.items;
    if (!a.length) return undefined;
    const first = a[0];
    const last = a.pop()!;
    if (a.length) {
      let i = 0;
      while (2 * i + 1 < a.length) {
        let child = 2 * i + 1;
        if (child + 1 < a.length && a[child + 1].cost < a[child].cost) child++;
        if (a[child].cost >= last.cost) break;
        a[i] = a[child]; i = child;
      }
      a[i] = last;
    }
    return first;
  }
}

function simplify(points: Point[]): Point[] {
  const out: Point[] = [];
  for (const point of points) {
    if (out.length && out.at(-1)!.x === point.x && out.at(-1)!.y === point.y) continue;
    while (out.length >= 2) {
      const a = out[out.length - 2], b = out[out.length - 1];
      if ((a.x === b.x && b.x === point.x) || (a.y === b.y && b.y === point.y)) out.pop();
      else break;
    }
    out.push(point);
  }
  return out;
}

export function draw(points: Point[]): RoutedPath {
  const lengths = points.slice(1).map((p, i) => Math.abs(p.x - points[i].x) + Math.abs(p.y - points[i].y));
  let half = lengths.reduce((a, b) => a + b, 0) / 2;
  let label = points[0];
  for (let i = 0; i < lengths.length; i++) {
    if (half <= lengths[i]) {
      const t = lengths[i] ? half / lengths[i] : 0;
      label = { x: points[i].x + (points[i + 1].x - points[i].x) * t,
        y: points[i].y + (points[i + 1].y - points[i].y) * t };
      break;
    }
    half -= lengths[i];
  }
  let path = `M${points[0].x},${points[0].y}`;
  for (let i = 1; i < points.length - 1; i++) {
    const before = points[i - 1], current = points[i], after = points[i + 1];
    const incoming = Math.hypot(current.x - before.x, current.y - before.y);
    const outgoing = Math.hypot(after.x - current.x, after.y - current.y);
    const radius = Math.min(CORNER_RADIUS, incoming / 2, outgoing / 2);
    if (radius === 0) { path += ` L${current.x},${current.y}`; continue; }
    const inPoint = { x: current.x - (current.x - before.x) / incoming * radius,
      y: current.y - (current.y - before.y) / incoming * radius };
    const outPoint = { x: current.x + (after.x - current.x) / outgoing * radius,
      y: current.y + (after.y - current.y) / outgoing * radius };
    path += ` L${inPoint.x},${inPoint.y} Q${current.x},${current.y} ${outPoint.x},${outPoint.y}`;
  }
  path += ` L${points.at(-1)!.x},${points.at(-1)!.y}`;
  return [path, label.x, label.y, points];
}

function sharedLaneCost(a: Point, b: Point, occupied: Segment[]): number {
  let cost = 0;
  for (const segment of occupied) {
    const c = segment.from, d = segment.to;
    const horizontal = a.y === b.y;
    const otherHorizontal = c.y === d.y;
    if (horizontal === otherHorizontal) {
      const distance = horizontal ? Math.abs(a.y - c.y) : Math.abs(a.x - c.x);
      if (distance >= LANE_GAP) continue;
      const overlap = horizontal
        ? Math.max(0, Math.min(Math.max(a.x, b.x), Math.max(c.x, d.x)) - Math.max(Math.min(a.x, b.x), Math.min(c.x, d.x)))
        : Math.max(0, Math.min(Math.max(a.y, b.y), Math.max(c.y, d.y)) - Math.max(Math.min(a.y, b.y), Math.min(c.y, d.y)));
      if (overlap) cost += (1 - distance / LANE_GAP) * (overlap * 3 + 80);
    } else {
      const h1 = horizontal ? a : c, h2 = horizontal ? b : d;
      const v1 = horizontal ? c : a, v2 = horizontal ? d : b;
      if (v1.x > Math.min(h1.x, h2.x) && v1.x < Math.max(h1.x, h2.x) &&
          h1.y > Math.min(v1.y, v2.y) && h1.y < Math.max(v1.y, v2.y)) cost += 30;
    }
  }
  return cost;
}

/** Finds an orthogonal route through the free corridors around node boxes.
 * The start/end stubs keep arrowheads perpendicular to their chosen sides;
 * rounded corners retain the authored-link look of extra edges. */
export function routeAroundNodes(
  start: Point, startSide: EdgeSide, end: Point, endSide: EdgeSide,
  boxes: Rect[], sourceBox?: Rect, targetBox?: Rect, occupied: Segment[] = [],
  noStartStub = false, noEndStub = false,
): RoutedPath | undefined {
  const bounds = {
    left: Math.min(start.x, end.x) - ROUTE_MARGIN, right: Math.max(start.x, end.x) + ROUTE_MARGIN,
    top: Math.min(start.y, end.y) - ROUTE_MARGIN, bottom: Math.max(start.y, end.y) + ROUTE_MARGIN,
  };
  const expand = (r: Rect): Rect => ({ x: r.x - CLEARANCE, y: r.y - CLEARANCE,
    width: r.width + 2 * CLEARANCE, height: r.height + 2 * CLEARANCE });
  const obstacles = boxes
    .filter((r) => r.x < bounds.right && r.x + r.width > bounds.left && r.y < bounds.bottom && r.y + r.height > bounds.top)
    .map(expand);
  const sourceObstacle = sourceBox && expand(sourceBox);
  const targetObstacle = targetBox && expand(targetBox);
  if (sourceObstacle) obstacles.push(sourceObstacle);
  if (targetObstacle) obstacles.push(targetObstacle);
  const startLength = noStartStub ? 0 : stubLength(start, startSide, sourceObstacle, obstacles.filter((r) => r !== sourceObstacle));
  const endLength = noEndStub ? 0 : stubLength(end, endSide, targetObstacle, obstacles.filter((r) => r !== targetObstacle));
  if (startLength === undefined || endLength === undefined) return undefined;
  const from = outside(start, startSide, startLength);
  const to = outside(end, endSide, endLength);
  if (obstacles.some((r) => inside(from, r) || inside(to, r)) ||
      segmentBlocked(start, from, obstacles.filter((r) => r !== sourceObstacle)) ||
      segmentBlocked(to, end, obstacles.filter((r) => r !== targetObstacle))) return undefined;

  const nearby = occupied.filter(({ from: a, to: b }) =>
    Math.max(a.x, b.x) >= bounds.left && Math.min(a.x, b.x) <= bounds.right &&
    Math.max(a.y, b.y) >= bounds.top && Math.min(a.y, b.y) <= bounds.bottom);
  const xs = uniqueSorted([from.x, to.x, bounds.left, bounds.right,
    ...obstacles.flatMap((r) => [r.x, r.x + r.width]),
    ...nearby.filter((s) => s.from.x === s.to.x).flatMap((s) => [s.from.x - LANE_GAP, s.from.x + LANE_GAP])]);
  const ys = uniqueSorted([from.y, to.y, bounds.top, bounds.bottom,
    ...obstacles.flatMap((r) => [r.y, r.y + r.height]),
    ...nearby.filter((s) => s.from.y === s.to.y).flatMap((s) => [s.from.y - LANE_GAP, s.from.y + LANE_GAP])]);
  const nx = xs.length, ny = ys.length;
  const point = (index: number): Point => ({ x: xs[index % nx], y: ys[Math.floor(index / nx)] });
  const free = new Uint8Array(nx * ny);
  for (let i = 0; i < free.length; i++) free[i] = obstacles.some((r) => inside(point(i), r)) ? 0 : 1;
  const startIndex = ys.indexOf(from.y) * nx + xs.indexOf(from.x);
  const endIndex = ys.indexOf(to.y) * nx + xs.indexOf(to.x);
  const costs = new Float64Array(nx * ny * 3).fill(Infinity);
  const previous = new Int32Array(costs.length).fill(-1);
  const heap = new MinHeap();
  const initial = startIndex * 3;
  costs[initial] = 0;
  heap.push({ state: initial, cost: 0 });
  let found = -1;
  while (heap.length) {
    const current = heap.pop()!;
    if (current.cost !== costs[current.state]) continue;
    const index = Math.floor(current.state / 3), direction = current.state % 3;
    if (index === endIndex) { found = current.state; break; }
    const x = index % nx, y = Math.floor(index / nx);
    for (const [next, nextDirection] of [
      [x > 0 ? index - 1 : -1, 1], [x + 1 < nx ? index + 1 : -1, 1],
      [y > 0 ? index - nx : -1, 2], [y + 1 < ny ? index + nx : -1, 2],
    ]) {
      if (next < 0 || !free[next] || segmentBlocked(point(index), point(next), obstacles)) continue;
      const a = point(index), b = point(next);
      const nextCost = current.cost + Math.abs(a.x - b.x) + Math.abs(a.y - b.y) +
        (direction && direction !== nextDirection ? BEND_COST : 0) + sharedLaneCost(a, b, nearby);
      const state = next * 3 + nextDirection;
      if (nextCost >= costs[state]) continue;
      costs[state] = nextCost;
      previous[state] = current.state;
      heap.push({ state, cost: nextCost });
    }
  }
  if (found < 0) return undefined;
  const reversed: Point[] = [];
  for (let state = found; state >= 0; state = previous[state]) reversed.push(point(Math.floor(state / 3)));
  const points = simplify([start, ...reversed.reverse(), end]);
  return draw(points);
}
