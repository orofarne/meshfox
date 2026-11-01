// End-to-end tests of the WebAssembly binding: the real .wasm, loaded in
// Node, rendering real canvases. Build first: scripts/test-wasm.sh does both.
import assert from "node:assert/strict";
import fs from "node:fs";
import { createRequire } from "node:module";
import path from "node:path";
import test from "node:test";

const require = createRequire(import.meta.url);
const here = import.meta.dirname;
const pkg = process.env.MESHFOX_WASM_PKG ?? path.join(here, "../pkg-node");
const wasm = require(path.join(pkg, "meshfox_wasm.js"));
const repo = path.join(here, "../../..");
const read = (rel) => fs.readFileSync(path.join(repo, rel), "utf8");
const render = (source, file, files) => JSON.parse(wasm.renderCanvas(source, file, files));

const INCLUDE_DOC = [
  '# Root\n<!-- meshfox:node id="root" -->\n\n',
  '## Notes\n<!-- meshfox:node id="notes" type="include" -->\n\n[notes](./notes.md)\n',
].join("");

test("the module reports a version", () => {
  assert.match(wasm.version(), /^\d+\.\d+\.\d+/);
});

test("the host clock is wired: the model can stamp the current time", () => {
  const stamp = wasm.hostNow();
  assert.match(stamp, /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(\.\d+)?Z$/);
  assert.ok(Math.abs(Date.parse(stamp) - Date.now()) < 5000, stamp);
});

test("renders a plain example with no files at all", () => {
  const canvas = render(read("examples/hello.canvas.md"), "hello.canvas.md");
  assert.equal(canvas.nodes.length, 15);
  assert.equal(canvas.nodes[0].id, "root");
  assert.ok(canvas.nodes.every((n) => typeof n.bodyRev === "string" && n.bodyRev.length > 0));
});

test("runs constraint fences and computed views", () => {
  const canvas = render(read("examples/constraints.canvas.md"), "constraints.canvas.md");
  const withConstraints = canvas.nodes.filter((n) => n.constraintResults?.length);
  assert.equal(withConstraints.length, 3);
  const withViews = canvas.nodes.filter((n) => n.renderResults?.length);
  assert.equal(withViews.length, 1);
});

test("a tag-derived colour and declared options come through", () => {
  const doc = [
    '# Root\n<!-- meshfox:node id="root" -->\n',
    '<!-- meshfox:tag-color tag="bug" color="1" -->\n',
    '<!-- meshfox:option name="auto-timestamps" -->\n\n',
    '## Child\n<!-- meshfox:node id="child" tags="bug" -->\n\nbody\n',
  ].join("");
  const canvas = render(doc, "x.canvas.md");
  assert.equal(canvas.nodes.find((n) => n.id === "child").effectiveColor, "1");
  assert.deepEqual(canvas.options, ["auto-timestamps"]);
});

test("include targets come from files.read(path), as an object method", () => {
  const seen = [];
  const files = {
    read(p) {
      seen.push(p);
      return p === "notes.md" ? "# Notes\n\nfrom the host\n" : null;
    },
  };
  const canvas = render(INCLUDE_DOC, "doc.canvas.md", files);
  assert.match(canvas.nodes.find((n) => n.id === "notes").text, /from the host/);
  assert.deepEqual(seen.filter((p) => p === "notes.md").length > 0, true);
});

test("files may be a bare function returning bytes", () => {
  const bytes = new TextEncoder().encode("# Notes\n\nas bytes\n");
  const canvas = render(INCLUDE_DOC, "doc.canvas.md", (p) => (p === "notes.md" ? bytes : undefined));
  assert.match(canvas.nodes.find((n) => n.id === "notes").text, /as bytes/);
});

test("paths are relative to the canvas's own directory", () => {
  const files = (p) => (p === "docs/notes.md" ? "# Notes\n\nnested\n" : null);
  const canvas = render(INCLUDE_DOC, "docs/doc.canvas.md", files);
  assert.match(canvas.nodes.find((n) => n.id === "notes").text, /nested/);
});

test("a missing include throws, with and without a provider", () => {
  assert.throws(() => render(INCLUDE_DOC, "doc.canvas.md"), /notes\.md|not found/i);
  assert.throws(() => render(INCLUDE_DOC, "doc.canvas.md", () => null), /notes\.md|not found/i);
});

test("an include that escapes the canvas directory is refused before the host is asked", () => {
  const doc = INCLUDE_DOC.replace("./notes.md", "../outside.md");
  let asked = false;
  assert.throws(
    () =>
      render(doc, "doc.canvas.md", () => {
        asked = true;
        return "# leaked\n";
      }),
    /outside/i,
  );
  assert.equal(asked, false);
});

test("a throwing or misbehaving provider is an error, not a crash", () => {
  assert.throws(() => render(INCLUDE_DOC, "doc.canvas.md", () => {
    throw new Error("disk on fire");
  }));
  assert.throws(() => render(INCLUDE_DOC, "doc.canvas.md", () => 42), /Uint8Array|string/);
  assert.throws(() => render(INCLUDE_DOC, "doc.canvas.md", { nope: 1 }), /read\(path\)/);
});

test("a document that does not parse throws", () => {
  assert.throws(() => render("no heading here\n", "x.canvas.md"), /root|heading/i);
});

test("the large repository documents render", () => {
  for (const f of ["README.md", "SPEC.md"]) {
    const files = (p) => (fs.existsSync(path.join(repo, p)) ? fs.readFileSync(path.join(repo, p)) : null);
    const canvas = render(read(f), f, files);
    assert.ok(canvas.nodes.length >= 3, f);
  }
});

test("IgnoreRules applies the model's rules: dots and .meshfoxignore", () => {
  const rules = new wasm.IgnoreRules();
  assert.equal(rules.isIgnored("node_modules/x.md", false), false, "nothing is left out by name alone");
  assert.equal(rules.isIgnored(".git/config", false), true);
  assert.equal(rules.isIgnored("a/.notes/x.md", false), true);
  rules.addFile("", "node_modules/\n*.log\n");
  rules.addFile("docs", "!keep.log\n");
  assert.equal(rules.isIgnored("node_modules", true), true);
  assert.equal(rules.isIgnored("a/node_modules/x.md", false), true);
  assert.equal(rules.isIgnored("run.log", false), true);
  assert.equal(rules.isIgnored("docs/keep.log", false), false, "a deeper file brings it back");
  assert.equal(rules.isIgnored("a.canvas.md", false), false);
  rules.free();
});
