import assert from "node:assert/strict";
import test from "node:test";
import { canOpenOnServer, canvasOpenUrl } from "../src/openUrl.ts";

const location = { protocol: "http:", hostname: "localhost" };

test("a local worker is opened on its own port of the same host", () => {
  assert.equal(
    canvasOpenUrl({ url: null, port: 4242 }, location, "http://localhost:4000/"),
    "http://localhost:4242/",
  );
});

test("behind the gateway the relative url resolves against the page", () => {
  assert.equal(
    canvasOpenUrl({ url: "../todo-k7m2q5xa/", port: null }, location, "http://gw.example:4590/c/notes-aaaaaaaa/"),
    "http://gw.example:4590/c/todo-k7m2q5xa/",
  );
});

test("an outer path prefix is kept", () => {
  assert.equal(
    canvasOpenUrl({ url: "../b-bbbbbbbb/", port: null }, location, "https://host.example/meshfox/c/a-aaaaaaaa/"),
    "https://host.example/meshfox/c/b-bbbbbbbb/",
  );
});

test("the url wins over a port when both are present", () => {
  assert.equal(
    canvasOpenUrl({ url: "../x-cccccccc/", port: 1 }, location, "http://h/c/y-dddddddd/"),
    "http://h/c/x-cccccccc/",
  );
});

test("through the gateway only canvases can be opened on the server", () => {
  assert.equal(canOpenOnServer("./notes.txt", false), true);
  assert.equal(canOpenOnServer("./other.canvas.md#node", true), true);
  assert.equal(canOpenOnServer("./notes.txt", true), false);
  assert.equal(canOpenOnServer("./image.png#x", true), false);
  assert.equal(canOpenOnServer(undefined, true), false);
});
