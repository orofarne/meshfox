import type { Point, Rect, Segment } from "./edgeRouting.ts";

export function labelSize(label = "", tags: string[] = []): { width: number; height: number } {
  // Fira Code is monospace: 12px text, with badge padding and chip borders.
  const textWidth = Math.max(...label.split("\n").map(line => [...line].length * 7.2), 0);
  const width = textWidth + tags.reduce((sum, tag) => sum + [...tag].length * 7.2 + 20, 0) + 18;
  return { width: Math.min(256, width), height: 24 * Math.max(label.split("\n").length, Math.ceil(width / 256)) };
}
export function labelRect(point: Point, size: { width: number; height: number }): Rect {
  return { x: point.x - size.width / 2, y: point.y - size.height / 2, ...size };
}
function overlaps(a: Rect, b: Rect, gap = 10): boolean {
  return a.x < b.x + b.width + gap && a.x + a.width + gap > b.x &&
    a.y < b.y + b.height + gap && a.y + a.height + gap > b.y;
}
function crosses(rect: Rect, segment: Segment): boolean {
  const a = segment.from, b = segment.to;
  return a.y === b.y
    ? a.y >= rect.y - 8 && a.y <= rect.y + rect.height + 8 && Math.max(a.x, b.x) >= rect.x - 8 && Math.min(a.x, b.x) <= rect.x + rect.width + 8
    : a.x >= rect.x - 8 && a.x <= rect.x + rect.width + 8 && Math.max(a.y, b.y) >= rect.y - 8 && Math.min(a.y, b.y) <= rect.y + rect.height + 8;
}
/** Keep badges on their own path, preferring the authored position, then the
 * closest free stretch. All edges participate in one deterministic placement. */
export function placeLabel(points: Point[], size: { width: number; height: number }, at: number,
  nodes: Rect[], labels: Rect[], otherRoutes: Segment[]): Point {
  const lengths = points.slice(1).map((p, i) => Math.hypot(p.x - points[i].x, p.y - points[i].y));
  const total = lengths.reduce((a, b) => a + b, 0);
  const candidates: { point: Point; distance: number }[] = [];
  let traveled = 0;
  for (let i = 0; i < lengths.length; i++) {
    const length = lengths[i];
    if (!length) continue;
    const offsets = [Math.max(0, Math.min(length, total * at / 1000 - traveled)), length / 2];
    for (let offset = 12; offset < length; offset += 12) offsets.push(offset);
    for (const offset of offsets) candidates.push({ point: {
      x: points[i].x + (points[i + 1].x - points[i].x) * offset / length,
      y: points[i].y + (points[i + 1].y - points[i].y) * offset / length,
    }, distance: Math.abs(traveled + offset - total * at / 1000) });
    traveled += length;
  }
  let best = points[0], bestScore = Infinity;
  for (const candidate of candidates) {
    const rect = labelRect(candidate.point, size);
    const score = nodes.filter(n => overlaps(rect, n, 14)).length * 1e7 +
      labels.filter(l => overlaps(rect, l)).length * 1e6 +
      otherRoutes.filter(s => crosses(rect, s)).length * 1e5 + candidate.distance;
    if (score < bestScore) { bestScore = score; best = candidate.point; }
  }
  return best;
}
