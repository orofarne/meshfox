import { test, expect } from "@playwright/test";
import fs from "node:fs";

// Regression coverage for the on-disk file watcher
// (`spawn_file_watcher`, `crates/server/src/lib.rs`) actually reaching an
// already-open tab: this test edits `external-edit.canvas.md` the way an
// external tool (a plain text editor, another process, an agent session
// that isn't going through meshfox's own MCP server or CLI) would —
// `fs.writeFileSync` straight to the file `meshfox view` is already
// serving, nothing else. No `/api/*` call, no `meshfox node <op>`, no MCP
// `node_*` tool — those all route through the running worker's own HTTP
// API (or, for MCP, `coordinator::discover` — see `crates/cli/src/
// coordinator.rs`) and would update the server's in-memory state directly,
// which proves nothing about the *file-watching* path specifically.
//
// The path this test actually exercises: `spawn_file_watcher` polls the
// file's mtime every 500ms, and once it notices the content actually
// differs from what the server has cached, pushes `ServerEvent::Changed`
// over `/api/watch` (a WebSocket every open tab holds) — the client's own
// `onChanged` (`web/src/api.ts`) then refetches `/api/canvas`. The test
// never calls `page.reload()`: the new title showing up on its own is what
// proves that whole chain actually fired, not just that a fresh page load
// would have picked up the edit.
//
// `MESHFOX_E2E_EXTERNAL_EDIT_CANVAS_PATH` (set in `playwright.config.ts`)
// is this fixture's real on-disk path — a fixed directory rebuilt from the
// checked-in fixture on every config load, deliberately *not* a slice of
// the shared `FIXTURES_DIR` every other suite uses: Playwright reloads the
// config fresh in every worker process, and `FIXTURES_DIR` is a fresh
// `mkdtemp` each time, so a worker's own copy of that path would silently
// diverge from whatever directory the root/orchestrator process actually
// pointed the running server at. Every other suite never notices (their
// specs only ever reach their fixture over `/api/*`, never the filesystem
// directly) — this spec is the one exception, so it needs a path stable
// across reloads. See `playwright.config.ts`'s own `EXTERNAL_EDIT_DIR`
// comment.
const CANVAS_PATH = process.env.MESHFOX_E2E_EXTERNAL_EDIT_CANVAS_PATH;
if (!CANVAS_PATH) {
  throw new Error(
    "MESHFOX_E2E_EXTERNAL_EDIT_CANVAS_PATH is unset — playwright.config.ts should have set it " +
      "before this spec file ever runs; see its own EXTERNAL_EDIT_PORT comment.",
  );
}

test.beforeEach(async ({ page }) => {
  await page.goto("/");
  await page.waitForSelector(".mesh-node");
});

test("a direct on-disk edit (no MCP, no CLI, no API call) shows up in an already-open tab without a reload", async ({
  page,
}) => {
  const rootTitle = page.locator(
    '.react-flow__node[data-id="root"] .mesh-node-title-text, ' +
      '.react-flow__node[data-id="root"] .mesh-node-title-centered-text',
  );
  await expect(rootTitle).toHaveText("Before External Edit");

  // The direct filesystem edit this whole test is about — equivalent to
  // opening the file in a plain text editor (or an agent session editing
  // it with a raw text-edit tool) and saving, not to anything meshfox's
  // own server, CLI, or MCP tools would ever do on a caller's behalf.
  const original = fs.readFileSync(CANVAS_PATH, "utf-8");
  const edited = original.replace("# Before External Edit", "# After External Edit");
  expect(edited).not.toBe(original); // sanity: the replace actually matched something
  fs.writeFileSync(CANVAS_PATH, edited, "utf-8");

  // No `page.reload()` here — if this only passed with one, it'd mean the
  // file watcher never actually pushed anything, and the fixture's fresh
  // content was only picked up by the *next* full load.
  await expect(rootTitle).toHaveText("After External Edit", { timeout: 5_000 });
});

// Same regression, aimed at a node's *body* text instead of its heading —
// a heading-only edit (above) reparses the node's own title straight off
// the `#`/`##` line, while the body goes through Markdown rendering
// (`.mesh-node-body`); worth covering separately in case the two ever
// diverge in how a live `Changed` reload applies. Targets a marker line
// this test never shares with the heading test above (see the fixture's
// own "Body marker" line) so the two tests stay order-independent even
// though they run against the same on-disk file/server.
test("a direct on-disk edit to a node's body text also shows up live, without a reload", async ({ page }) => {
  const rootBody = page.locator('.react-flow__node[data-id="root"] .mesh-node-body');
  await expect(rootBody).toContainText("Body marker: BEFORE-BODY-EDIT.");

  const original = fs.readFileSync(CANVAS_PATH, "utf-8");
  const edited = original.replace("Body marker: BEFORE-BODY-EDIT.", "Body marker: AFTER-BODY-EDIT.");
  expect(edited).not.toBe(original); // sanity: the replace actually matched something
  fs.writeFileSync(CANVAS_PATH, edited, "utf-8");

  await expect(rootBody).toContainText("Body marker: AFTER-BODY-EDIT.", { timeout: 5_000 });
});
