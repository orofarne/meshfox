import assert from "node:assert/strict";
import test from "node:test";
import { resolveAssetHref } from "../src/assetHref.ts";

test("parent paths reach the worker intact, including encoded filenames and fragments", () => {
  const url = new URL(resolveAssetHref("../docs/scd%2041.pdf#page=2"), "http://localhost/");
  assert.equal(url.pathname, "/api/asset");
  assert.equal(url.searchParams.get("file"), "../docs/scd%2041.pdf");
  assert.equal(url.hash, "#page=2");
  const included = new URL(resolveAssetHref("../image.svg", "/project/docs"), "http://localhost/");
  assert.equal(included.searchParams.get("dir"), "/project/docs");
  assert.equal(included.searchParams.get("file"), "../image.svg");
});

test("external URLs, anchors and root URLs keep their meaning", () => {
  for (const href of [undefined, "", "#part", "?view=x", "/api/test", "//example.org/x", "https://example.org/x", "data:image/png;base64,AA"])
    assert.equal(resolveAssetHref(href), href);
});
