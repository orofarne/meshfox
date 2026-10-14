import { test, expect, type Page } from "@playwright/test";
import { clickFitViewAndWait, disableDefaultFold } from "./helpers";
import type { CanvasNode } from "../src/types";

// Browser projects share this server. Size/content examples must restore the
// card before the next project checks the original group's routing geometry.
const fitOriginal = new WeakMap<Page, CanvasNode>();
test.beforeEach(async ({ page }, info) => {
  if (!info.title.startsWith("Fit ")) return;
  const canvas = await (await page.request.get("/api/canvas")).json();
  fitOriginal.set(page, canvas.nodes.find((n: CanvasNode) => n.id === "node2"));
});
test.afterEach(async ({ page }) => {
  const original = fitOriginal.get(page);
  if (!original) return;
  await page.request.post("/api/nodes/node2/clear-layout");
  const canvas = await (await page.request.get("/api/canvas")).json();
  const current = canvas.nodes.find((n: CanvasNode) => n.id === "node2");
  const response = await page.request.patch("/api/nodes/node2", { data: {
    title: original.title, text: original.text, baseRev: current.bodyRev, baseTitle: current.title,
    width: original.width, height: original.height, x: original.x, y: original.y,
  } });
  expect(response.ok(), await response.text()).toBe(true);
});

// Real, auto-sized members use group-relative coordinates. Sample the SVG
// curve in screen space so incorrect parent offsets, rounded corners and
// a fallback bezier disappearing behind cards are all covered.
async function inspectLink(page: Page) {
  return page.evaluate(() => {
    const path = document.querySelector<SVGPathElement>(
      '.react-flow__edge[data-id="node2->node10:extra"] .react-flow__edge-path',
    );
    if (!path) throw new Error("node2 -> node10 is missing");
    const matrix = path.getScreenCTM()!;
    const length = path.getTotalLength();
    const screenPoint = (distance: number) => {
      const point = path.getPointAtLength(distance);
      return new DOMPoint(point.x, point.y).matrixTransform(matrix);
    };
    const cards = Array.from({ length: 12 }, (_, i) => {
      const id = `node${i + 1}`;
      const node = document.querySelector<HTMLElement>(`.react-flow__node[data-id="${id}"]`)!;
      return { id, node, box: node.getBoundingClientRect() };
    });
    const crossings: string[] = [];
    const hidden: string[] = [];
    let rightmost = -Infinity;
    // At most one screen pixel between samples, including both endpoints.
    const steps = Math.ceil(length * Math.hypot(matrix.a, matrix.b));
    for (let i = 0; i <= steps; i++) {
      const point = screenPoint(length * i / steps);
      rightmost = Math.max(rightmost, point.x);
      for (const { id, box } of cards) {
        if (point.x > box.left + 1 && point.x < box.right - 1 &&
            point.y > box.top + 1 && point.y < box.bottom - 1) crossings.push(id);
      }
      // The group's background must not paint over the link either.
      // React Flow's interaction path shares the edge's stacking position.
      if (i > 0 && i < steps && point.x > 0 && point.x < innerWidth && point.y > 0 && point.y < innerHeight) {
        const hits = document.elementsFromPoint(point.x, point.y);
        const edgeIndex = hits.findIndex(el => el.closest('.react-flow__edge[data-id="node2->node10:extra"]'));
        const cardIndex = hits.findIndex(el => el.closest('.mesh-node'));
        if (edgeIndex < 0 || (cardIndex >= 0 && cardIndex < edgeIndex)) hidden.push(`${Math.round(point.x)},${Math.round(point.y)}`);
      }
    }
    const endpointDistance = (id: string, handle: string, distance: number) => {
      const node = cards.find(card => card.id === id)!.node;
      const box = node.querySelector(`[data-handleid="${handle}"]`)!.getBoundingClientRect();
      const point = screenPoint(distance);
      return Math.hypot(point.x - box.x - box.width / 2, point.y - box.y - box.height / 2);
    };
    const end = screenPoint(length);
    const start = screenPoint(0);
    return {
      crossings: [...new Set(crossings)], hidden: hidden.slice(0, 5),
      rightLane: rightmost > Math.max(cards[1].box.right, cards[9].box.right) + 10,
      sourceDistance: endpointDistance("node2", "source-default", 0),
      targetDistance: endpointDistance("node10", "target-right", length),
      marker: path.getAttribute("marker-end"),
      endpointsInView: [start, end].every(p => p.x > 0 && p.x < innerWidth && p.y > 0 && p.y < innerHeight),
    };
  });
}

for (const folded of [false, true]) {
  test(`node2 -> node10 remains visible around 12 ${folded ? "folded" : "expanded"} group members`, async ({ page }) => {
    await disableDefaultFold(page, "root");
    await page.goto("/");
    await expect(page.locator('.react-flow__node[data-id="g1"]')).toBeVisible();
    await expect(page.locator('.react-flow__node[data-id^="node"]')).toHaveCount(12);
    await clickFitViewAndWait(page);
    if (folded) {
      for (let i = 1; i <= 12; i++) {
        await page.locator(`.react-flow__node[data-id="node${i}"] .mesh-node-fold-toggle`).click();
      }
      await clickFitViewAndWait(page);
    }
    await expect.poll(async () => {
      const link = await inspectLink(page);
      return { ...link, sourceDistance: link.sourceDistance < 4, targetDistance: link.targetDistance < 4,
        marker: !!link.marker && link.marker !== "none" };
    }).toEqual({ crossings: [], hidden: [], rightLane: true, sourceDistance: true, targetDistance: true, marker: true, endpointsInView: true });
  });
}

test("internal labels stay inside the group and clear cards, lanes and each other", async ({ page }) => {
  await disableDefaultFold(page, "root");
  await page.route("**/api/canvas", async route => {
    if (route.request().method() !== "GET") { await route.continue(); return; }
    const response = await route.fetch();
    const canvas = await response.json();
    const target = canvas.nodes.find((n: { id: string }) => n.id === "node10");
    target.extraParents[0].label = "metrics, logs, traces";
    canvas.nodes.find((n: { id: string }) => n.id === "node11").extraParents = [{ from: "node3", label: "Grafana" }];
    await route.fulfill({ response, json: canvas });
  });
  await page.goto("/");
  await expect(page.locator(".mesh-edge-label-badge")).toHaveCount(2);
  await clickFitViewAndWait(page);
  await expect.poll(() => page.evaluate(() => {
    const group = document.querySelector('.react-flow__node[data-id="g1"]')!.getBoundingClientRect();
    const badges = [...document.querySelectorAll<HTMLElement>(".mesh-edge-label-badge")];
    const labels = badges.map(n => n.getBoundingClientRect());
    const cards = [...document.querySelectorAll('.react-flow__node[data-id^="node"]')].map(n => n.getBoundingClientRect());
    const overlap = (a: DOMRect, b: DOMRect) => a.left < b.right && a.right > b.left && a.top < b.bottom && a.bottom > b.top;
    return {
      contained: labels.every(b => b.left > group.left + 5 && b.right < group.right - 5 && b.top > group.top + 5 && b.bottom < group.bottom - 5),
      cardsClear: labels.every(b => cards.every(c => !overlap(b, c))),
      labelsClear: !overlap(labels[0], labels[1]),
      lanesClear: badges.every((badge, index) => [...document.querySelectorAll<SVGPathElement>(".react-flow__edge-path")].every(path => {
        if (path.closest(".react-flow__edge")?.getAttribute("data-id") === badge.dataset.edgeId) return true;
        const matrix = path.getScreenCTM()!, length = path.getTotalLength(), box = labels[index];
        for (let distance = 0; distance <= length; distance += 2) {
          const point = path.getPointAtLength(distance);
          const screen = new DOMPoint(point.x, point.y).matrixTransform(matrix);
          if (screen.x > box.left && screen.x < box.right && screen.y > box.top && screen.y < box.bottom) return false;
        }
        return true;
      })),
    };
  })).toEqual({ contained: true, cardsClear: true, labelsClear: true, lanesClear: true });
  await page.screenshot({ path: test.info().outputPath("labels.png") });
});

test("body inset and header height are stable across edit mode", async ({ page }) => {
  await disableDefaultFold(page, "root");
  await page.goto("/");
  await expect(page.locator('.react-flow__node[data-id="node2"] .mesh-node-body')).toBeVisible();
  const metrics = () => page.locator('.react-flow__node[data-id="node2"]').evaluate(node => {
    const title = node.querySelector(".mesh-node-title")!, body = node.querySelector(".mesh-node-body")!;
    const text = body.firstElementChild!;
    return { header: (title as HTMLElement).offsetHeight,
      top: text.getBoundingClientRect().top - body.getBoundingClientRect().top,
      left: text.getBoundingClientRect().left - body.getBoundingClientRect().left };
  });
  const before = await metrics();
  await page.getByRole("button", { name: "Edit", exact: true }).click();
  await expect.poll(metrics).toEqual(before);
  await page.getByRole("button", { name: "done", exact: true }).click();
  await expect.poll(metrics).toEqual(before);
});

test("growing a group to the left preserves member dragging coordinates", async ({ page }) => {
  await disableDefaultFold(page, "root");
  await page.route("**/api/canvas", async route => {
    if (route.request().method() !== "GET") { await route.continue(); return; }
    const response = await route.fetch();
    const canvas = await response.json();
    const edge = canvas.nodes.find((n: { id: string }) => n.id === "node10").extraParents[0];
    Object.assign(edge, { label: "wide left-side arrow label", sourceSide: "left", targetSide: "left" });
    await route.fulfill({ response, json: canvas });
  });
  await page.goto("/");
  const node = page.locator('.react-flow__node[data-id="node2"]');
  await expect(node).toBeVisible();
  await clickFitViewAndWait(page);
  await page.getByRole("button", { name: "Edit", exact: true }).click();
  const title = (await node.locator(".mesh-node-title-text").boundingBox())!;
  await page.mouse.move(title.x + title.width / 2, title.y + title.height / 2);
  await page.mouse.down();
  // Cross React Flow's drag threshold before measuring actual movement.
  await page.mouse.move(title.x + title.width / 2 + 8, title.y + title.height / 2 + 6);
  const before = (await node.boundingBox())!;
  await page.mouse.move(title.x + title.width / 2 + 48, title.y + title.height / 2 + 36, { steps: 10 });
  await expect.poll(async () => {
    const after = (await node.boundingBox())!;
    return { x: Math.round(after.x - before.x), y: Math.round(after.y - before.y) };
  }).toEqual({ x: 40, y: 30 });
  const saved = page.waitForResponse(response => response.url().endsWith("/api/canvas") && response.request().method() === "PUT");
  await page.mouse.up();
  await saved;
  await page.unrouteAll({ behavior: "wait" });
});

for (const direction of ["Down", "Right", "Proportionally"]) {
  test(`Fit to content ${direction} previews, resizes and persists without moving the corner`, async ({ page }) => {
    await disableDefaultFold(page, "root");
    const document = await (await page.request.get("/api/canvas")).json();
    const initial = document.nodes.find((n: { id: string }) => n.id === "node2");
    const patch = await page.request.patch("/api/nodes/node2", { data: {
      title: "Fit", width: 221, height: 163, x: 41, y: 230.5,
      baseRev: initial.bodyRev,
      baseTitle: initial.title,
      text: "A paragraph with enough words to wrap into many lines inside a narrow node. ".repeat(12),
    } });
    expect(patch.ok(), await patch.text()).toBe(true);
    await page.goto("/");
    const node = page.locator('.react-flow__node[data-id="node2"]');
    await expect(node.locator(".mesh-node-body")).toBeVisible();
    await clickFitViewAndWait(page);
    await page.getByRole("button", { name: "Edit", exact: true }).click();
    await node.locator(".mesh-node-title-text").click();
    const metrics = () => node.evaluate(n => {
      const box = n.getBoundingClientRect();
      const root = n.querySelector<HTMLElement>(".mesh-node")!;
      const body = n.querySelector<HTMLElement>(".mesh-node-body")!;
      return { x: box.x, y: box.y, width: root.offsetWidth, height: root.offsetHeight,
        fits: body.scrollHeight <= body.clientHeight + 1 && body.scrollWidth <= body.clientWidth + 1 };
    });
    const before = await metrics();
    expect(before.fits).toBe(false);
    await page.getByRole("button", { name: "Fit to content", exact: true }).click();
    const choice = page.getByRole("menuitem", { name: new RegExp(`^${direction}`) });
    await expect(choice).toBeEnabled();
    await choice.hover();
    await expect(node.locator(".mesh-node-fit-preview")).toBeVisible();
    expect(await metrics()).toEqual(before);
    await page.screenshot({ path: test.info().outputPath(`fit-${direction}-preview.png`) });
    const saved = page.waitForResponse(response => response.url().endsWith("/api/canvas") && response.request().method() === "PUT");
    await choice.click();
    await saved;
    await expect(node.locator(".mesh-node-fit-preview")).toHaveCount(0);
    await expect.poll(async () => (await metrics()).fits).toBe(true);
    const after = await metrics();
    expect(Math.abs(after.x - before.x)).toBeLessThan(0.1);
    expect(Math.abs(after.y - before.y)).toBeLessThan(0.1);
    if (direction === "Down") { expect(after.width).toBe(before.width); expect(after.height).toBeGreaterThan(before.height); }
    if (direction === "Right") { expect(after.height).toBe(before.height); expect(after.width).toBeGreaterThan(before.width); }
    if (direction === "Proportionally") expect(Math.abs(after.width / after.height - before.width / before.height)).toBeLessThan(0.03);
    await page.reload();
    await expect(node.locator(".mesh-node-body")).toBeVisible();
    await expect.poll(async () => {
      const size = await metrics(); return { width: size.width, height: size.height, fits: size.fits };
    }).toEqual({ width: after.width, height: after.height, fits: true });
  });
}

test("Fit menu disables fitting content and impossible directions, and Escape cancels", async ({ page }) => {
  await disableDefaultFold(page, "root");
  const initial = (await (await page.request.get("/api/canvas")).json()).nodes.find((n: { id: string }) => n.id === "node2");
  const patch = await page.request.patch("/api/nodes/node2", { data: {
    title: "Fit", baseTitle: initial.title, baseRev: initial.bodyRev, width: 220, height: 240,
    text: "```text\n" + "long_unwrapped_code_line_".repeat(8) + "\n```",
  } });
  expect(patch.ok(), await patch.text()).toBe(true);
  await page.goto("/");
  const node = page.locator('.react-flow__node[data-id="node2"]');
  await expect(node.locator("pre")).toBeVisible();
  await clickFitViewAndWait(page);
  await page.getByRole("button", { name: "Edit", exact: true }).click();
  await node.locator(".mesh-node-title-text").click();
  await page.getByRole("button", { name: "Fit to content", exact: true }).click();
  await expect(page.getByRole("menuitem", { name: "Down", exact: true })).toBeDisabled();
  const right = page.getByRole("menuitem", { name: "Right", exact: true });
  await expect(right).toBeEnabled();
  await right.focus();
  await expect(node.locator(".mesh-node-fit-preview")).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(page.getByRole("menu", { name: "Fit to content" })).toHaveCount(0);
  await expect(node.locator(".mesh-node-fit-preview")).toHaveCount(0);
  await page.getByRole("button", { name: "Fit to content", exact: true }).click();
  const saved = page.waitForResponse(response => response.url().endsWith("/api/canvas") && response.request().method() === "PUT");
  await page.getByRole("menuitem", { name: "Right", exact: true }).click();
  await saved;
  await expect.poll(() => node.locator("pre").evaluate(pre => pre.scrollWidth <= pre.clientWidth + 1)).toBe(true);
  await page.getByRole("button", { name: "Fit to content", exact: true }).click();
  for (const title of ["Down", "Right", "Proportionally"]) await expect(page.getByRole("menuitem", { name: title, exact: true })).toBeDisabled();
});

test("opposite arrows retain separate entry and exit stubs on folded cards", async ({ page }) => {
  await disableDefaultFold(page, "root");
  // Earlier drag tests author a fixed position. Restore this auto-layout
  // scenario before folding, otherwise a later card can cover its toggle.
  const cleared = await page.request.post("/api/nodes/node2/clear-layout");
  expect(cleared.ok()).toBe(true);
  await page.route("**/api/canvas", async route => {
    if (route.request().method() !== "GET") { await route.continue(); return; }
    const response = await route.fetch();
    const canvas = await response.json();
    canvas.nodes.find((n: CanvasNode) => n.id === "node2").extraParents = [{ from: "node10" }];
    await route.fulfill({ response, json: canvas });
  });
  await page.goto("/");
  await expect(page.locator('.react-flow__node[data-id="node2"]')).toBeVisible();
  await clickFitViewAndWait(page);
  for (let i = 1; i <= 12; i++) await page.locator(`.react-flow__node[data-id="node${i}"] .mesh-node-fold-toggle`).click();
  await clickFitViewAndWait(page);
  await expect.poll(() => page.evaluate(() => {
    const endpoints = (id: string) => {
      const path = document.querySelector<SVGPathElement>(`.react-flow__edge[data-id="${id}"] .react-flow__edge-path`)!;
      const start = path.getPointAtLength(0), end = path.getPointAtLength(path.getTotalLength());
      return { start, end };
    };
    const a = endpoints("node2->node10:extra"), b = endpoints("node10->node2:extra");
    return { node2: Math.abs(a.start.y - b.end.y) >= 12, node10: Math.abs(b.start.y - a.end.y) >= 12 };
  })).toEqual({ node2: true, node10: true });
  await page.screenshot({ path: test.info().outputPath("separate-arrow-tips.png") });
});

test("automatic group placement includes the full internal-route frame beside an authored neighbor", async ({ page }) => {
  await disableDefaultFold(page, "root");
  await page.route("**/api/canvas", async route => {
    if (route.request().method() !== "GET") { await route.continue(); return; }
    const response = await route.fetch();
    const canvas = await response.json();
    canvas.nodes = [
      { id: "root", title: "Root", text: "", level: 1, x: 0, y: 0, width: 120, height: 100 },
      { id: "group", title: "Components", text: "", level: 2, parent: "root", type: "group" },
      { id: "a", title: "A", text: "A short body.", level: 3, parent: "group", x: 40, y: 80, width: 200, height: 80 },
      { id: "b", title: "B", text: "A short body.", level: 3, parent: "group", x: 40, y: 280, width: 200, height: 80,
        extraParents: [{ from: "a", sourceSide: "left", targetSide: "left", label: "an internal labeled arrow" }] },
      { id: "neighbor", title: "Neighbor", text: "Keep this authored position.", level: 2, parent: "root", x: 0, y: 180, width: 340, height: 220 },
    ];
    await route.fulfill({ response, json: canvas });
  });
  await page.goto("/");
  await expect(page.locator('.react-flow__node[data-id="group"]')).toBeVisible();
  await clickFitViewAndWait(page);
  await expect.poll(() => page.evaluate(() => {
    const group = document.querySelector('.react-flow__node[data-id="group"]')!.getBoundingClientRect();
    const neighbor = document.querySelector('.react-flow__node[data-id="neighbor"]')!.getBoundingClientRect();
    const path = document.querySelector<SVGPathElement>('.react-flow__edge[data-id="a->b:extra"] .react-flow__edge-path')!;
    const matrix = path.getScreenCTM()!, zoom = Math.hypot(matrix.a, matrix.b);
    const badge = document.querySelector(".mesh-edge-label-badge")!.getBoundingClientRect();
    const start = path.getPointAtLength(0);
    const screen = new DOMPoint(start.x, start.y).matrixTransform(matrix);
    const handle = document.querySelector('.react-flow__node[data-id="a"] [data-handleid="source-left"]')!.getBoundingClientRect();
    return {
      clear: group.left >= neighbor.right + 24 * zoom || group.top >= neighbor.bottom + 24 * zoom,
      labelInside: badge.left > group.left && badge.right < group.right && badge.top > group.top && badge.bottom < group.bottom,
      attached: Math.hypot(screen.x - handle.x - handle.width / 2, screen.y - handle.y - handle.height / 2) / zoom < 4,
    };
  })).toEqual({ clear: true, labelInside: true, attached: true });
  await page.screenshot({ path: test.info().outputPath("group-neighbor-clearance.png") });
});
