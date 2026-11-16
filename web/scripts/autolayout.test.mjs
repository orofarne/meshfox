import assert from "node:assert/strict";
import test from "node:test";
import { computeAutoLayout, FOLDED_HEIGHT } from "../src/autolayout.ts";

const canvas = { nodes: [
  { id: "root", title: "Root", level: 1 },
  { id: "hardware", title: "Hardware", level: 2, parent: "root" },
  { id: "keyboard", title: "Keyboard", level: 3, parent: "hardware" },
  { id: "next", title: "Next", level: 3, parent: "hardware" },
] };
const layout = (viewportHeight, overrides = {}) => computeAutoLayout({
  canvas, viewportWidth: 1600, viewportHeight,
  measuredHeight: () => 1800, ...overrides,
});

test("deep auto-height follows viewport height and reserves that space for siblings", () => {
  for (const viewportHeight of [400, 800, 1600]) {
    const boxes = layout(viewportHeight);
    const keyboard = boxes.get("keyboard");
    assert.equal(keyboard.maxHeight, viewportHeight * 0.6);
    assert.equal(keyboard.height, keyboard.maxHeight);
    assert.equal(boxes.get("next").y, keyboard.y + keyboard.height + 25);
    assert.equal(boxes.get("hardware").height, 1800);
    assert.equal(boxes.get("hardware").maxHeight, undefined);
  }
});

test("short content, explicit height and folded nodes keep their own height", () => {
  assert.equal(layout(800, { measuredHeight: () => 120 }).get("keyboard").height, 120);
  const fixed = layout(400, { canvas: { nodes: canvas.nodes.map(n =>
    n.id === "keyboard" ? { ...n, height: 900 } : n) } }).get("keyboard");
  assert.equal(fixed.height, 900);
  assert.equal(fixed.maxHeight, undefined);
  const folded = layout(400, { foldedNodeIds: new Set(["keyboard"]) }).get("keyboard");
  assert.equal(folded.height, FOLDED_HEIGHT);
  assert.equal(folded.maxHeight, undefined);
});
