import assert from "node:assert/strict";
import { readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";
import test from "node:test";

// The app can be served under a path prefix (`/c/<id>/`), so a request must
// never start with `/`: a root-relative URL drops the prefix. Requests are
// written without the leading slash and resolve against `document.baseURI`.
function sources(dir) {
  return readdirSync(dir, { withFileTypes: true }).flatMap((e) => {
    const path = join(dir, e.name);
    if (e.isDirectory()) return sources(path);
    return /\.(ts|tsx)$/.test(e.name) && !/\.(test|spec)\./.test(e.name) ? [path] : [];
  });
}

test("no source builds a root-relative /api or /assets URL", () => {
  const offenders = [];
  for (const file of sources(new URL("../src", import.meta.url).pathname)) {
    readFileSync(file, "utf8").split("\n").forEach((line, i) => {
      if (/^\s*(\/\/|\*|\/\*)/.test(line)) return;
      if (/["'`]\/(api|assets)\//.test(line)) offenders.push(`${file}:${i + 1}: ${line.trim()}`);
    });
  }
  assert.deepEqual(offenders, []);
});
