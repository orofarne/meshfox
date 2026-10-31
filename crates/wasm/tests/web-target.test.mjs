// The `web` build is the one a browser (or Vite) loads: an ES module whose
// default export fetches the .wasm by URL. Node has no fetch-by-URL for
// local files, so these tests hand it the bytes through `initSync` — the
// same module, the same exports, a different loader.
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import test from "node:test";

const here = import.meta.dirname;
const pkg = process.env.MESHFOX_WASM_WEB_PKG ?? path.join(here, "../pkg");
const glue = await import(path.join(pkg, "meshfox_wasm.js"));
glue.initSync({ module: fs.readFileSync(path.join(pkg, "meshfox_wasm_bg.wasm")) });

const repo = path.join(here, "../../..");

test("the ES module exposes the same API as the Node build", () => {
  for (const name of ["renderCanvas", "hostNow", "version"]) {
    assert.equal(typeof glue[name], "function", name);
  }
  assert.equal(typeof glue.default, "function", "default export is the async loader");
});

test("renders a canvas and resolves an include through the host", () => {
  const doc = fs.readFileSync(path.join(repo, "examples/hello.canvas.md"), "utf8");
  assert.equal(JSON.parse(glue.renderCanvas(doc, "hello.canvas.md")).nodes.length, 15);

  const withInclude = [
    '# Root\n<!-- meshfox:node id="root" -->\n\n',
    '## Notes\n<!-- meshfox:node id="notes" type="include" -->\n\n[notes](./notes.md)\n',
  ].join("");
  const canvas = JSON.parse(
    glue.renderCanvas(withInclude, "doc.canvas.md", { read: (p) => (p === "notes.md" ? "# N\n\nweb\n" : null) }),
  );
  assert.match(canvas.nodes.find((n) => n.id === "notes").text, /web/);
  assert.match(glue.hostNow(), /Z$/);
});
