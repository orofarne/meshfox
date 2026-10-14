import assert from "node:assert/strict";
import test from "node:test";
import { draw, routeAroundNodes } from "../src/edgeRouting.ts";
import { distributeEdgePorts } from "../src/edgePorts.ts";
import { withEdgeRoutes } from "../src/edgeRouteLayout.ts";

test("folded adjacent rows use a visible right lane and update ports after moving", () => {
  const nodes = Array.from({ length: 5 }, (_, i) => ({
    id: `row-${i}`, position: { x: 100, y: i * 69 }, width: 800, height: 44,
    data: { level: 3, nodeType: "text" },
  }));
  const edge = { id: "request->jwt", source: "row-0", target: "row-1", type: "extra",
    sourceHandle: "source-default", targetHandle: "target-default", data: {} };
  const routed = withEdgeRoutes(nodes, [edge])[0];
  assert.equal(routed.sourceHandle, "source-default");
  assert.equal(routed.targetHandle, "target-right");
  assert.ok(routed.data.routedPoints);
  assert.ok(routed.data.routedPoints.every(p => p.x >= 902));
  assert.ok(routed.data.routedPoints.some(p => p.x >= 936));
  assertOutside(routed.data.routedPath[0], nodes.map(n => ({ ...n.position, width: n.width, height: n.height })));

  const sideBySide = nodes.map((n, i) => i === 1 ? { ...n, position: { x: 1100, y: 0 } } : n);
  const moved = withEdgeRoutes(sideBySide, [routed])[0];
  assert.equal(moved.sourceHandle, "source-default");
  assert.equal(moved.targetHandle, "target-default");
});

// Sample the actual SVG path, including its rounded quadratic corners.
// Checking just the orthogonal waypoints would miss a curve cutting into a box.
function sampledPath(path) {
  const tokens = path.match(/[MLQ]|-?\d+(?:\.\d+)?/g) ?? [];
  const points = [];
  let i = 0;
  let current;
  while (i < tokens.length) {
    const command = tokens[i++];
    if (command === "M") {
      current = { x: Number(tokens[i++]), y: Number(tokens[i++]) };
      points.push(current);
    } else if (command === "L") {
      const end = { x: Number(tokens[i++]), y: Number(tokens[i++]) };
      for (let step = 1; step <= 40; step++) {
        const t = step / 40;
        points.push({ x: current.x + (end.x - current.x) * t, y: current.y + (end.y - current.y) * t });
      }
      current = end;
    } else if (command === "Q") {
      const control = { x: Number(tokens[i++]), y: Number(tokens[i++]) };
      const end = { x: Number(tokens[i++]), y: Number(tokens[i++]) };
      for (let step = 1; step <= 40; step++) {
        const t = step / 40, u = 1 - t;
        points.push({ x: u * u * current.x + 2 * u * t * control.x + t * t * end.x,
          y: u * u * current.y + 2 * u * t * control.y + t * t * end.y });
      }
      current = end;
    } else throw new Error(`Unexpected SVG command: ${command}`);
  }
  return points;
}

function assertOutside(path, boxes) {
  for (const point of sampledPath(path)) {
    for (const box of boxes) {
      assert.ok(!(point.x > box.x + 0.5 && point.x < box.x + box.width - 0.5 &&
        point.y > box.y + 0.5 && point.y < box.y + box.height - 0.5),
      `path crossed ${JSON.stringify(box)} at ${JSON.stringify(point)}`);
    }
  }
}

test("an intermediate node diverts an extra edge, including its rounded corners", () => {
  const source = { x: -40, y: -40, width: 80, height: 40 };
  const target = { x: -40, y: 200, width: 80, height: 40 };
  const blocker = { x: -50, y: 70, width: 100, height: 60 };
  const routed = routeAroundNodes({ x: 0, y: 0 }, "bottom", { x: 0, y: 200 }, "top", [blocker], source, target);
  assert.ok(routed);
  assertOutside(routed[0], [source, target, blocker]);
});

test("a distant group link can exit through narrow gaps beside intermediate cards", () => {
  const group = { id: "g1", position: { x: 320, y: 240 }, width: 880, height: 1000,
    data: { level: 2, nodeType: "group" } };
  const members = Array.from({ length: 12 }, (_, i) => ({
    id: `node${i + 1}`, parentId: "g1", position: { x: 40, y: 80 + i * 85 },
    width: 800, height: 60, data: { level: 3, nodeType: "text" },
  }));
  const routed = withEdgeRoutes([group, ...members], [{ id: "node2->node10:extra", type: "extra",
    source: "node2", target: "node10", data: {} }])[0];
  assert.ok(routed.data.routedPath, "must not fall back to a curve through the cards");
  assertOutside(routed.data.routedPath[0], members.map(n => ({
    x: group.position.x + n.position.x, y: group.position.y + n.position.y, width: n.width, height: n.height,
  })));
});

test("a link entering the far side of its target goes around the target", () => {
  const source = { x: 300, y: 100, width: 100, height: 100 };
  const target = { x: 0, y: 100, width: 100, height: 100 };
  const routed = routeAroundNodes({ x: 400, y: 150 }, "right", { x: 0, y: 150 }, "left", [], source, target);
  assert.ok(routed);
  assertOutside(routed[0], [source, target]);
});

test("a nearby node shortens the exit stub; moving it changes the route", () => {
  const source = { x: 0, y: 0, width: 100, height: 100 };
  const target = { x: 0, y: 300, width: 100, height: 100 };
  const blocker = { x: 40, y: 136, width: 100, height: 100 };
  const start = { x: 50, y: 100 }, end = { x: 50, y: 300 };
  const blocked = routeAroundNodes(start, "bottom", end, "top", [blocker], source, target);
  const moved = routeAroundNodes(start, "bottom", end, "top", [{ ...blocker, x: 200 }], source, target);
  assert.ok(blocked && moved);
  assertOutside(blocked[0], [source, target, blocker]);
  assert.notEqual(blocked[0], moved[0]);
});

test("a second edge uses a separate lane when one is available", () => {
  const blocker = { x: -50, y: 70, width: 100, height: 60 };
  const start = { x: 0, y: 0 }, end = { x: 0, y: 200 };
  const first = routeAroundNodes(start, "bottom", end, "top", [blocker]);
  assert.ok(first);
  const occupied = first[3].slice(1).map((to, index) => ({ from: first[3][index], to }));
  const second = routeAroundNodes(start, "bottom", end, "top", [blocker], undefined, undefined, occupied);
  assert.ok(second);
  assertOutside(first[0], [blocker]);
  assertOutside(second[0], [blocker]);
  assert.notEqual(first[0], second[0]);
  assert.ok(first[3].some(point => point.x < -50));
  assert.ok(second[3].some(point => point.x > 50));
});

test("a saved waypoint and chosen sides survive rerouting around a moved obstacle", () => {
  const source = { x: 0, y: 0, width: 100, height: 60 };
  const target = { x: 300, y: 0, width: 100, height: 60 };
  const via = { x: 200, y: 180 };
  for (const blocker of [{ x: 150, y: 40, width: 80, height: 80 }, { x: 160, y: 65, width: 80, height: 80 }]) {
    const first = routeAroundNodes({ x: 50, y: 60 }, "bottom", via, "left", [blocker], source, undefined, [], false, true);
    const second = routeAroundNodes(via, "right", { x: 350, y: 60 }, "bottom", [blocker], undefined, target, [], true, false);
    assert.ok(first && second);
    const routed = draw([...first[3], ...second[3].slice(1)]);
    assert.ok(routed[3].some(p => p.x === via.x && p.y === via.y));
    assertOutside(routed[0], [blocker]);
  }
});

test("explicit sides select the matching routing handles", () => {
  const edges = [{ id: "root->child", source: "root", target: "child", extra: false,
    sourceSide: "right", targetSide: "right" }];
  const boxes = new Map([
    ["root", { x: 0, y: 0, width: 100, height: 60 }],
    ["child", { x: 200, y: 0, width: 100, height: 60 }],
  ]);
  const ports = distributeEdgePorts(edges, boxes, new Map([["root", 1], ["child", 2]])).get("root->child");
  assert.equal(ports.sourceHandle, "source-right");
  assert.equal(ports.targetHandle, "target-right");
});

test("automatic outgoing arrows share the source center while pinned ones can spread", () => {
  const boxes = new Map([
    ["root", { x: 0, y: 0, width: 140, height: 100 }],
    ["a", { x: 240, y: 0, width: 100, height: 60 }],
    ["b", { x: 240, y: 180, width: 100, height: 60 }],
    ["c", { x: 240, y: 360, width: 100, height: 60 }],
  ]);
  const edges = ["a", "b", "c"].map((target) => ({ id: `root->${target}`, source: "root", target, extra: false }));
  const levels = new Map([["root", 1], ["a", 2], ["b", 2], ["c", 2]]);
  const auto = distributeEdgePorts(edges, boxes, levels);
  for (const edge of edges) assert.equal(auto.get(edge.id).sourceOffset, 0);

  const pinned = distributeEdgePorts(edges.map((edge) => ({ ...edge, sourceSide: "left" })), boxes, levels);
  assert.notEqual(pinned.get("root->a").sourceOffset, pinned.get("root->c").sourceOffset);
});

test("the routed path uses explicit left and right handles", () => {
  const nodes = [
    { id: "root", type: "mesh", position: { x: 0, y: 0 }, width: 100, height: 60,
      data: { level: 1, nodeType: "text" } },
    { id: "child", type: "mesh", position: { x: 300, y: 0 }, width: 100, height: 60,
      data: { level: 2, nodeType: "text" } },
  ];
  const edge = { id: "root->child", source: "root", target: "child", type: "extra",
    sourceHandle: "source-right", targetHandle: "target-right", data: { sourceSide: "right", targetSide: "right" } };
  const routed = withEdgeRoutes(nodes, [edge])[0].data.routedPoints;
  assert.ok(routed);
  assert.equal(routed[0].x, 102);
  assert.equal(routed.at(-1).x, 402);
});

test("stacked links route upward, share their source and use separate right lanes", () => {
  const nodes = [0, 1, 2].map(i => ({ id: `n${i}`, position: { x: 100, y: i * 100 },
    width: 800, height: 44, data: { level: 1, nodeType: "text" } }));
  const edges = ["n0", "n1"].map(target => ({ id: `n2->${target}`, source: "n2", target, type: "extra", data: {} }));
  const routes = withEdgeRoutes(nodes, edges);
  for (const route of routes) {
    assert.equal(route.sourceHandle, "source-right", "level-one default is left, so right must be explicit");
    assert.equal(route.targetHandle, "target-right");
    assert.equal(route.data.sourceOffset, 0);
    assertOutside(route.data.routedPath[0], nodes.map(n => ({ ...n.position, width: n.width, height: n.height })));
  }
  assert.deepEqual(routes[0].data.routedPoints[0], routes[1].data.routedPoints[0]);
  const lanes = routes.map(route => route.data.routedPoints.slice(1).filter((p, i) =>
    p.x === route.data.routedPoints[i].x && p.y !== route.data.routedPoints[i].y).map(p => p.x));
  assert.notDeepEqual(lanes[0], lanes[1]);
});

test("stacked right lanes avoid a neighboring card and respect pinned vertical sides", () => {
  const nodes = [
    { id: "a", position: { x: 100, y: 0 }, width: 800, height: 44, data: { level: 3, nodeType: "text" } },
    { id: "b", position: { x: 100, y: 200 }, width: 800, height: 44, data: { level: 3, nodeType: "text" } },
    { id: "obstacle", position: { x: 920, y: 80 }, width: 100, height: 60, data: { level: 3, nodeType: "text" } },
  ];
  const edge = { id: "a->b", source: "a", target: "b", type: "extra", data: {} };
  const route = withEdgeRoutes(nodes, [edge])[0];
  assert.ok(route.data.routedPath);
  assertOutside(route.data.routedPath[0], nodes.map(n => ({ ...n.position, width: n.width, height: n.height })));
  const pinned = withEdgeRoutes(nodes, [{ ...edge, data: { sourceSide: "bottom", targetSide: "top" } }])[0];
  assert.equal(pinned.sourceHandle, "source-bottom");
  assert.equal(pinned.targetHandle, "target-top");
});

test("labels avoid crossings, other badges, and node clearance on their own route", async () => {
  const { placeLabel, labelRect } = await import("../src/edgeLabels.ts");
  const points = [{ x: 0, y: 100 }, { x: 600, y: 100 }];
  const size = { width: 120, height: 24 };
  const nodes = [{ x: -100, y: 80, width: 180, height: 40 }, { x: 520, y: 80, width: 180, height: 40 }];
  const crossing = [{ from: { x: 300, y: 0 }, to: { x: 300, y: 200 } }];
  const first = placeLabel(points, size, 500, nodes, [], crossing);
  assert.equal(first.y, 100);
  assert.ok(Math.abs(first.x - 300) > 68);
  assert.ok(first.x - 60 >= 94 && first.x + 60 <= 506);
  const second = placeLabel(points, size, 500, nodes, [labelRect(first, size)], crossing);
  assert.equal(second.y, 100);
  assert.ok(Math.abs(second.x - first.x) >= 130);
  assert.ok(Math.abs(second.x - 300) > 68);
});

test("group frames contain internal routes and badges without moving members", async () => {
  const { withRouteGroupFrames } = await import("../src/edgeRouteLayout.ts");
  const nodes = [
    { id: "g", position: { x: 100, y: 100 }, width: 300, height: 400, data: { nodeType: "group" } },
    { id: "a", parentId: "g", position: { x: 40, y: 80 }, width: 200, height: 80, data: { nodeType: "text" } },
    { id: "b", parentId: "g", position: { x: 40, y: 280 }, width: 200, height: 80, data: { nodeType: "text" } },
  ];
  const edges = [{ id: "ab", source: "a", target: "b", data: {
    label: "a long internal link", routedLabel: { x: 80, y: 300 },
    routedPoints: [{ x: 340, y: 220 }, { x: 400, y: 220 }, { x: 400, y: 420 }, { x: 340, y: 420 }],
  } }];
  const result = withRouteGroupFrames(nodes, edges);
  assert.ok(result[0].width > 400);
  assert.ok(result[0].position.x < nodes[0].position.x);
  for (let i = 1; i < nodes.length; i++) {
    assert.equal(result[0].position.x + result[i].position.x, nodes[0].position.x + nodes[i].position.x);
    assert.equal(result[0].position.y + result[i].position.y, nodes[0].position.y + nodes[i].position.y);
  }
});

test("incoming tips do not retrace the shared automatic outgoing port, including folded cards", () => {
  for (const height of [44, 200]) {
    const nodes = [0, 1].map(i => ({ id: `n${i}`, position: { x: 100, y: i * 400 },
      width: 800, height, data: { level: 3, nodeType: "text" } }));
    const routes = withEdgeRoutes(nodes, [
      { id: "n0->n1", source: "n0", target: "n1", type: "extra", data: {} },
      { id: "n1->n0", source: "n1", target: "n0", type: "extra", data: {} },
    ]);
    for (const route of routes) {
      assert.equal(route.data.sourceOffset, 0, "automatic outgoing port remains centered");
      assert.ok(Math.abs(route.data.targetOffset) >= 12, "incoming tip is visibly separated from the outgoing port");
    }
    assert.notDeepEqual(routes[0].data.routedPoints[0], routes[1].data.routedPoints.at(-1));
    assert.notDeepEqual(routes[1].data.routedPoints[0], routes[0].data.routedPoints.at(-1));
  }
});

test("tree and extra incoming links refresh their ports together after folding", () => {
  const nodes = [
    { id: "above", position: { x: 0, y: 0 }, width: 200, height: 100, data: { level: 3, nodeType: "text" } },
    { id: "below", position: { x: 0, y: 800 }, width: 200, height: 100, data: { level: 3, nodeType: "text" } },
    { id: "target", position: { x: 500, y: 400 }, width: 200, height: 44, data: { level: 3, nodeType: "text" } },
  ];
  const routes = withEdgeRoutes(nodes, [
    { id: "above->target", source: "above", target: "target", type: "tree", data: { targetOffset: 0 } },
    { id: "below->target", source: "below", target: "target", type: "extra", data: { sourceSide: "right", targetSide: "left", targetOffset: 0 } },
  ]);
  assert.notEqual(routes[0].data.targetOffset, routes[1].data.targetOffset);
  assert.ok(Math.abs(routes[0].data.targetOffset - routes[1].data.targetOffset) >= 24);
  const ends = routes.map(r => r.data.routedPoints.at(-1));
  assert.equal(ends[0].x, ends[1].x);
  assert.notEqual(ends[0].y, ends[1].y);
  const segments = r => r.data.routedPoints.slice(1).map((to, i) => ({ from: r.data.routedPoints[i], to }));
  for (const a of segments(routes[0])) for (const b of segments(routes[1])) {
    const horizontal = a.from.y === a.to.y;
    if (horizontal !== (b.from.y === b.to.y)) continue;
    if (horizontal ? a.from.y !== b.from.y : a.from.x !== b.from.x) continue;
    const axis = horizontal ? "x" : "y";
    const overlap = Math.min(Math.max(a.from[axis], a.to[axis]), Math.max(b.from[axis], b.to[axis])) -
      Math.max(Math.min(a.from[axis], a.to[axis]), Math.min(b.from[axis], b.to[axis]));
    assert.ok(overlap <= 0, "tree and extra routes must not merge into a shared lane");
  }
});

function assertSimpleRoute(points) {
  for (let i = 1; i < points.length; i++) for (let j = 1; j < i - 1; j++) {
    const a = points[j - 1], b = points[j], c = points[i - 1], d = points[i];
    const horizontal = a.y === b.y, otherHorizontal = c.y === d.y;
    if (horizontal === otherHorizontal) {
      if (horizontal ? a.y !== c.y : a.x !== c.x) continue;
      const axis = horizontal ? "x" : "y";
      const overlap = Math.min(Math.max(a[axis], b[axis]), Math.max(c[axis], d[axis])) -
        Math.max(Math.min(a[axis], b[axis]), Math.min(c[axis], d[axis]));
      assert.ok(overlap < 0, `route retraces itself: ${JSON.stringify(points)}`);
    } else {
      const point = horizontal ? { x: c.x, y: a.y } : { x: a.x, y: c.y };
      const inside = (p, q) => point.x >= Math.min(p.x, q.x) && point.x <= Math.max(p.x, q.x) &&
        point.y >= Math.min(p.y, q.y) && point.y <= Math.max(p.y, q.y);
      assert.ok(!(inside(a, b) && inside(c, d)), `route crosses itself: ${JSON.stringify(points)}`);
    }
  }
}

test("a lane detour cannot loop across its own source or target stub on any side", () => {
  const rotate = p => ({ x: -p.y, y: p.x });
  const rotateBox = box => ({ x: -box.y - box.height, y: box.x, width: box.height, height: box.width });
  const rotateSide = side => ({ left: "top", top: "right", right: "bottom", bottom: "left" })[side];
  let start = { x: 150, y: 127 }, end = { x: 600, y: 198 };
  let source = { x: 0, y: 0, width: 300, height: 125 }, target = { x: 450, y: 200, width: 300, height: 125 };
  let startSide = "bottom", endSide = "top";
  let occupied = [{ from: { x: 152, y: 170 }, to: { x: 800, y: 170 } },
    { from: { x: 152, y: 0 }, to: { x: 152, y: 400 } }];
  for (let rotation = 0; rotation < 4; rotation++) {
    for (const reverse of [false, true]) {
      const route = reverse
        ? routeAroundNodes(end, endSide, start, startSide, [], target, source, occupied)
        : routeAroundNodes(start, startSide, end, endSide, [], source, target, occupied);
      assert.ok(route, "detouring around a terminal stub must still produce a route");
      assertSimpleRoute(route[3]);
      assertOutside(route[0], [source, target]);
    }
    start = rotate(start); end = rotate(end); source = rotateBox(source); target = rotateBox(target);
    startSide = rotateSide(startSide); endSide = rotateSide(endSide);
    occupied = occupied.map(segment => ({ from: rotate(segment.from), to: rotate(segment.to) }));
  }
});

test("facing ports retain a straight connector when their short stubs overlap", () => {
  const route = routeAroundNodes({ x: 102, y: 50 }, "right", { x: 138, y: 50 }, "left", [],
    { x: 0, y: 0, width: 100, height: 100 }, { x: 140, y: 0, width: 100, height: 100 });
  assert.ok(route);
  assert.deepEqual(route[3], [{ x: 102, y: 50 }, { x: 138, y: 50 }]);
});

test("nearby perpendicular ports join intersecting stubs without a rectangular loop", () => {
  const route = routeAroundNodes({ x: 0, y: 0 }, "right", { x: 12, y: 12 }, "top", []);
  assert.ok(route);
  assert.deepEqual(route[3], [{ x: 0, y: 0 }, { x: 12, y: 0 }, { x: 12, y: 12 }]);
  assertSimpleRoute(route[3]);
});

test("parallel lanes keep a real gap even when a nearby free grid line is only a pixel away", () => {
  const occupied = [{ from: { x: 0, y: 100 }, to: { x: 700, y: 100 } }];
  const route = routeAroundNodes({ x: 50, y: 101 }, "bottom", { x: 600, y: 101 }, "bottom", [], undefined, undefined, occupied);
  assert.ok(route);
  for (let i = 1; i < route[3].length; i++) {
    const a = route[3][i - 1], b = route[3][i];
    if (a.y === b.y && Math.abs(a.x - b.x) > 40) assert.ok(Math.abs(a.y - 100) >= 20);
  }
  assertSimpleRoute(route[3]);
});

test("route-expanded automatic groups clear neighboring cards and reroute their endpoints", async () => {
  const { withEdgeGeometry } = await import("../src/edgeRouteLayout.ts");
  const nodes = [
    { id: "neighbor", position: { x: 0, y: 140 }, width: 120, height: 120, data: { level: 2, nodeType: "text", suggested: false } },
    { id: "g", position: { x: 140, y: 0 }, width: 300, height: 400, data: { level: 2, nodeType: "group", suggested: true } },
    { id: "a", parentId: "g", position: { x: 40, y: 80 }, width: 200, height: 80, data: { level: 3, nodeType: "text", suggested: true } },
    { id: "b", parentId: "g", position: { x: 40, y: 280 }, width: 200, height: 80, data: { level: 3, nodeType: "text", suggested: true } },
  ];
  const edges = [{ id: "ab", source: "a", target: "b", type: "extra", data: {
    sourceSide: "left", targetSide: "left", label: "an internal labeled arrow" } }];
  const result = withEdgeGeometry(nodes, edges);
  const group = result.nodes[1], neighbor = result.nodes[0];
  assert.ok(group.position.x >= neighbor.position.x + neighbor.width + 25 || group.position.y >= neighbor.position.y + neighbor.height + 25);
  assert.deepEqual(neighbor.position, nodes[0].position, "authored neighbor must stay put");
  const start = result.edges[0].data.routedPoints[0];
  const layoutGroup = result.layoutNodes[1], member = result.layoutNodes[2];
  assert.equal(start.x, layoutGroup.position.x + member.position.x - 2);
  assert.equal(start.y, layoutGroup.position.y + member.position.y + member.height / 2);
  const again = withEdgeGeometry(nodes.map(n => n.id === "g" ? { ...n, measured: { width: group.width, height: group.height } } : n), edges);
  assert.deepEqual(again.nodes.map(n => n.position), result.nodes.map(n => n.position), "measurement must not keep shifting the group");
});

test("a crossing is kept away from another arrow's bend or tip", () => {
  const route = routeAroundNodes({ x: 190, y: 0 }, "bottom", { x: 190, y: 300 }, "top", [],
    undefined, undefined, [{ from: { x: 0, y: 100 }, to: { x: 200, y: 100 } }]);
  assert.ok(route);
  assertSimpleRoute(route[3]);
  for (let i = 1; i < route[3].length; i++) {
    const a = route[3][i - 1], b = route[3][i];
    if (a.x === b.x && Math.min(a.y, b.y) <= 100 && Math.max(a.y, b.y) >= 100 && a.x >= 0 && a.x <= 200) {
      assert.ok(a.x >= 20 && a.x <= 180, "crossing must leave room around both endpoints");
    }
  }
});
