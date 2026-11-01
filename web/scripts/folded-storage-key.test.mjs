import assert from "node:assert/strict";
import test from "node:test";
import { foldedStorageKey } from "../src/foldedStorageKey.ts";

test("a plain view page keeps the original key, so saved fold state survives", () => {
  assert.equal(foldedStorageKey("root", undefined, "/"), "meshfox-folded:root");
});

test("two canvases behind one gateway do not share a key, though both are `root`", () => {
  assert.notEqual(foldedStorageKey("root", undefined, "/c/a-k7m2q5xa/"), foldedStorageKey("root", undefined, "/c/b-p3x9z1ab/"));
});

test("two documents opened at one address are told apart by their own key", () => {
  const a = foldedStorageKey("root", "project/README.canvas.md", "/editor/");
  const b = foldedStorageKey("root", "other/README.canvas.md", "/editor/");
  const c = foldedStorageKey("root", "project/docs/x.canvas.md", "/editor/");
  assert.equal(new Set([a, b, c]).size, 3);
  assert.equal(a, foldedStorageKey("root", "project/README.canvas.md", "/editor/"));
});
