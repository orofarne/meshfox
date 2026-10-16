import assert from "node:assert/strict";
import test from "node:test";
import { refreshImageUrl } from "../src/imageRefresh.ts";
const base = "http://localhost:3000/";
test("local images get a new URL preserving include parameters and fragments", () => {
  const src = "/api/include-asset?dir=%2Ftmp%2Fdocs&file=plot.svg#view";
  const url = new URL(refreshImageUrl(src, 2, base), base);
  assert.equal(url.searchParams.get("dir"), "/tmp/docs");
  assert.equal(url.searchParams.get("file"), "plot.svg");
  assert.equal(url.searchParams.get("meshfox-image-version"), "2");
  assert.equal(url.hash, "#view");
  assert.notEqual(refreshImageUrl("plot.png", 1, base), refreshImageUrl("plot.png", 2, base));
});
test("remote and inline image URLs retain their semantics", () => {
  for (const src of [undefined, "", "data:image/png;base64,aA==", "https://example.org/x", "//example.org/x", "blob:abc", "#view"])
    assert.equal(refreshImageUrl(src, 3, base), src);
});
