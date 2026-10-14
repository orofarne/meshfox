import assert from "node:assert/strict";
import test from "node:test";
import { findFitSize } from "../src/contentFit.ts";

test("height fitting grows just enough and retains width", () => {
  assert.deepEqual(findFitSize({ width: 220, height: 160 }, "down", size => size.height >= 413), { width: 220, height: 416 });
});
test("width fitting retains height and finds reflow width", () => {
  assert.deepEqual(findFitSize({ width: 220, height: 160 }, "right", size => size.width >= 731), { width: 732, height: 160 });
});
test("proportional fitting retains aspect ratio within grid rounding", () => {
  const size = findFitSize({ width: 220, height: 160 }, "proportional", size => size.width * size.height >= 300000);
  assert.ok(size.width * size.height >= 300000);
  assert.ok(Math.abs(size.width / size.height - 220 / 160) < 0.01);
});
test("already fitting content does not shrink; impossible direction is unavailable", () => {
  const current = { width: 220, height: 160 };
  assert.deepEqual(findFitSize(current, "right", () => true), current);
  assert.equal(findFitSize(current, "right", size => size.height >= 400), null);
});
