import assert from "node:assert/strict";
import test from "node:test";
import { draw, routeAroundNodes } from "../src/edgeRouting.ts";
import { distributeEdgePorts } from "../src/edgePorts.ts";
import { withEdgeRoutes } from "../src/edgeRouteLayout.ts";

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
