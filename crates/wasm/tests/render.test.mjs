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
  assert.match(wasm.version(), /^(v\d+\.\d+\.\d+\S*|commit \S+) \(.+\)$/);
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

test("renderCanvasWithWarnings shows a missing include as a notice instead of failing", () => {
  const out = JSON.parse(wasm.renderCanvasWithWarnings(INCLUDE_DOC, "doc.canvas.md", () => null));
  assert.deepEqual(out.warnings.map((w) => [w.nodeId, w.target]), [["notes", "./notes.md"]]);
  assert.match(out.warnings[0].reason, /not among the files/);
  const notes = out.canvas.nodes.find((n) => n.id === "notes");
  assert.match(notes.text, /\[!WARNING\]/);
  assert.match(notes.text, /\.\/notes\.md/);
  // With the file, no warnings and the content is there.
  const ok = JSON.parse(
    wasm.renderCanvasWithWarnings(INCLUDE_DOC, "doc.canvas.md", (p) => (p.endsWith("notes.md") ? "# N\n\nhi\n" : null)),
  );
  assert.deepEqual(ok.warnings, []);
  assert.match(ok.canvas.nodes.find((n) => n.id === "notes").text, /hi/);
  // A document that does not parse is still an error.
  assert.throws(() => wasm.renderCanvasWithWarnings("no heading here\n", "x.canvas.md"), /root|heading/i);
  // Strict stays strict.
  assert.throws(() => render(INCLUDE_DOC, "doc.canvas.md", () => null), /notes\.md/);
});

test("bodyRev is the server's fingerprint", () => {
  assert.equal(wasm.bodyRev(""), "cbf29ce484222325");
  assert.notEqual(wasm.bodyRev("a"), wasm.bodyRev("b"));
});

test("UndoLog: the model's undo log, in memory, walked by the page", () => {
  const log = new wasm.UndoLog("v0");
  assert.equal(log.canUndo(), false);
  assert.equal(log.undo("v0"), undefined);
  log.push("source", "v0", "v1");
  log.push("source", "v1", "v2");
  assert.deepEqual([log.cursor(), log.canUndo(), log.canRedo()], [2, true, false]);

  assert.equal(log.undo("v2"), "v1");
  assert.equal(log.undo("v1"), "v0");
  assert.equal(log.undo("v0"), undefined, "nothing left to undo");
  assert.deepEqual([log.cursor(), log.canUndo(), log.canRedo()], [0, false, true]);
  assert.equal(log.redo("v0"), "v1");
  assert.equal(log.jumpTo(2, "v1"), "v2");
  assert.equal(log.jumpTo(2, "v2"), undefined, "already there");

  // The listing is the server's: redo tail first, then applied, newest first.
  log.undo("v2");
  const listed = JSON.parse(log.history(10));
  assert.deepEqual(listed.map((e) => [e.seq, e.applied, e.opKind]), [[2, false, "source"], [1, true, "source"]]);
  assert.match(listed[0].createdAt, /^\d{4}-\d\d-\d\dT/);

  // A fresh edit after an undo drops the redo tail, and numbers are never reused.
  log.push("source", "v1", "v1b");
  assert.equal(log.canRedo(), false);
  assert.deepEqual(JSON.parse(log.history(10)).map((e) => e.seq), [3, 1]);
  // A step that no longer fits the document is an error, not a silent no-op.
  assert.throws(() => log.undo("something else entirely"), /no longer fits/);
  assert.equal(log.cursor(), 3, "and nothing moved");
  log.free();
});

test("newCanvasContent is the template `meshfox create` writes, and renders", () => {
  const text = wasm.newCanvasContent("my notes");
  assert.ok(text.startsWith("<!-- meshfox:canvas -->\n# my notes\n"));
  const canvas = render(text, "my notes.canvas.md");
  assert.equal(canvas.nodes.length, 1);
  assert.equal(canvas.nodes[0].title, "my notes");
});

test("applyNodeUpdate: PATCH /api/nodes/:id's edit, in the page", () => {
  const doc =
    '# Root\n<!-- meshfox:node id="root" -->\n\n## Child\n<!-- meshfox:node id="child" tags="a" -->\n\nbody\n';
  const call = (id, req) => JSON.parse(wasm.applyNodeUpdate(doc, id, JSON.stringify(req)));
  const rev = render(doc, "d.canvas.md").nodes.find((n) => n.id === "child").bodyRev;

  const ok = call("child", { text: "new body", baseRev: rev, tags: { add: ["b"] }, color: "2" });
  const child = render(ok.raw, "d.canvas.md").nodes.find((n) => n.id === "child");
  assert.equal(child.text.trim(), "new body");
  assert.deepEqual(child.tags, ["a", "b"]);
  assert.equal(child.color, "2");

  // A stale base is a conflict, with the node as it is now; nothing applied.
  const stale = call("child", { text: "mine", baseRev: "0000" });
  assert.equal(stale.conflict.currentText.trim(), "body");
  assert.equal(stale.conflict.currentRev, rev);
  // A body without a base is refused; so is a node that is not there.
  assert.equal(call("child", { text: "mine" }).error.kind, "invalid");
  assert.equal(call("ghost", { color: "1" }).error.kind, "notFound");
  // An edge from a node that does not exist.
  assert.equal(call("child", { edges: { add: ["ghost"] } }).error.kind, "notFound");
  // A request that is not one.
  assert.equal(call("child", { width: "wide" }).error.kind, "invalid");
});

test("applyEdit: create, move, rename, remove, options, layout — the server's own edits", () => {
  let doc = '# Root\n<!-- meshfox:node id="root" -->\n\n## A\n<!-- meshfox:node id="a" -->\n\nfirst\n\n## B\n<!-- meshfox:node id="b" -->\n\nsecond\n';
  const edit = (op, args) => {
    const out = JSON.parse(wasm.applyEdit(doc, op, JSON.stringify(args)));
    if (out.raw !== undefined) doc = out.raw;
    return out;
  };
  const ids = () => render(doc, "d.canvas.md").nodes.map((n) => n.id);

  const created = edit("create", { parentId: "a", title: "Deep one", titleSlugId: true, body: "deep body" });
  assert.equal(created.newId, "deep-one");
  assert.equal(render(doc, "d.canvas.md").nodes.find((n) => n.id === "deep-one").parent, "a");

  edit("move", { id: "b", before: "a" });
  assert.ok(ids().indexOf("b") < ids().indexOf("a"));
  assert.equal(edit("move", { id: "b" }).error.kind, "invalid", "needs before or after");

  edit("renameId", { id: "a", newId: "alpha" });
  assert.ok(ids().includes("alpha") && !ids().includes("a"));
  assert.equal(edit("renameId", { id: "alpha", newId: "b" }).error.kind, "invalid", "taken");

  edit("options", { options: ["unfold"] });
  assert.deepEqual(render(doc, "d.canvas.md").options, ["unfold"]);

  edit("layout", { nodes: [{ id: "alpha", x: 10, y: 20, width: 300 }], layoutHints: {} });
  const alpha = render(doc, "d.canvas.md").nodes.find((n) => n.id === "alpha");
  assert.deepEqual([alpha.x, alpha.y, alpha.width], [10, 20, 300]);
  edit("clearNodeLayout", { id: "alpha" });
  assert.equal(render(doc, "d.canvas.md").nodes.find((n) => n.id === "alpha").x, undefined);

  edit("append", { id: "b", text: "\nappended" });
  assert.match(render(doc, "d.canvas.md").nodes.find((n) => n.id === "b").text, /appended/);

  edit("remove", { id: "alpha" });
  assert.ok(!ids().includes("alpha") && !ids().includes("deep-one"), "the subtree goes");
  assert.equal(edit("remove", { id: "root" }).error.kind, "invalid");
  assert.equal(edit("remove", { id: "ghost" }).error.kind, "notFound");
  assert.equal(edit("bogus", {}).error.kind, "invalid");
});

test("applyBlockAttrs: a block's attributes and code, as PATCH /api/nodes/:id/block/:name", () => {
  const doc = '# Root\n<!-- meshfox:node id="root" -->\n\n```bash name="build"\necho hi\n```\n';
  const call = (block, req) => JSON.parse(wasm.applyBlockAttrs(doc, "root", block, JSON.stringify(req)));
  const out = call("build", { cache: true, code: "echo bye", deps: "other" });
  assert.match(out.raw, /cache/);
  assert.match(out.raw, /echo bye/);
  assert.equal(call("nope", { cache: true }).error.kind, "notFound");
  assert.equal(call("build", { interpreter: "python", clearInterpreter: true }).error.kind, "invalid");
  assert.equal(call("build", { cache: "yes" }).error.kind, "invalid");
});
