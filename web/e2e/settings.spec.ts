import { test, expect, type Page } from "@playwright/test";
import { clickFitViewAndWait, disableDefaultFold, selectNode, toolbarButton } from "./helpers";

// Drives web/e2e/fixtures/settings.canvas.md: one node per NodeSettings-
// relevant type/field combination (see the fixture's own root body for the
// full rationale). Every node here gets the exact same treatment — open its
// settings modal, click "ok" without touching a single field — and the raw
// file (`GET /api/canvas/raw`) must come out byte-for-byte identical every
// time.
//
// This suite exists because "ok" resending fields nobody touched has twice
// silently rewritten the file out from under a no-op click: once by always
// including `color` (turning an absent color into a stored `color=""`), and
// once by always including the *resolved* node type — which, for an
// `include` node, is never actually `"include"` (the server always resolves
// one into a `group`/`text` node before it reaches the client; see
// `crates/core/src/include.rs`), so resending it silently overwrote the raw
// `type="include"` with whatever it happened to resolve to, destroying the
// include outright. `NodeSettings.tsx`'s `buildPatch` now diffs against the
// node's original values and sends only what actually changed — this suite
// is the regression test for that, across every node type the settings
// modal can show.

async function openSettings(page: Page, nodeId: string) {
  const node = page.locator(`.react-flow__node[data-id="${nodeId}"]`);
  // The settings gear now lives in a floating `NodeToolbar` above the
  // node, shown once it's selected — not inline in its title bar, hover-
  // revealed, the way it used to be (see `selectNode`/`toolbarButton`'s
  // own doc comments in helpers.ts).
  await selectNode(node);
  await toolbarButton(page, "settings").click();
  await expect(page.locator(".node-settings-modal")).toBeVisible();
}

async function clickOk(page: Page) {
  await page.locator(".vars-modal-actions button", { hasText: "ok" }).click();
  await expect(page.locator(".node-settings-modal")).toHaveCount(0);
}

function fetchRaw(page: Page): Promise<string> {
  return page.evaluate(() => fetch("/api/canvas/raw").then((r) => r.text()));
}

test.beforeEach(async ({ page }) => {
  // This fixture nests real nodes (group-node -> group-child,
  // include-canvas -> its own spliced children) that App.tsx's
  // fold-everything-with-children-by-default behavior would otherwise
  // hide on first load — this suite is about NodeSettings, not fold, so
  // start from every node visible instead.
  await disableDefaultFold(page, "root");
  await page.goto("/");
  await page.waitForSelector(".mesh-node");
  // Settings are Edit-mode-only (see MeshNode.tsx's gear icon) — every test
  // below needs it on. Fit-view first, same reasoning as every other spec
  // in this suite (see helpers.ts): gets every node genuinely on screen so
  // Firefox's stricter in-viewport click check doesn't fail on the later
  // ones.
  await clickFitViewAndWait(page);
  await page.getByRole("button", { name: "Edit" }).click();
});

// One entry per NodeSettings-relevant combination in the fixture:
// - a bare text node (nothing optional set at all)
// - a text node with every optional field set at once (color, tags, an
//   extra incoming edge)
// - a `file` node, plain link display
// - a `file` node, code-preview display + a language hint + an interpreter
// - a `link` node
// - an `include` node whose target is plain Markdown (resolves to `text`)
// - an `include` node whose target is itself a canvas (resolves to `group`)
// - a `group` node, and one of its structural children
const NODE_IDS = [
  "root",
  "text-plain",
  "text-styled",
  "file-link",
  "file-code",
  "link-node",
  "include-text",
  "include-canvas",
  "group-node",
  "group-child",
];

for (const nodeId of NODE_IDS) {
  test(`"ok" with no edits leaves the file unchanged — ${nodeId}`, async ({ page }) => {
    const before = await fetchRaw(page);

    await openSettings(page, nodeId);
    await clickOk(page);

    expect(await fetchRaw(page)).toBe(before);
  });
}

// TODO.canvas.md: "Смена id при создании ноды" — create a new node, then in
// the settings modal that opens for it right away, change *both* its title
// and its id, then "ok". Repro'd as a 404 ("no node ...") from the field
// patch that follows the rename. Root cause: NodeSettings' `handleOk`
// renames the id first, then commits every other changed field via a
// separate `onChange` call — App.tsx used to build that second call's own
// target id from a `settingsNode.id` closure captured when *this specific*
// modal instance was rendered, i.e. the *pre-rename* id, since a successful
// rename remounts a fresh `NodeSettings` (`key={settingsNode.id}`) without
// waiting for this still-running "ok" handler to finish — so the field
// patch landed on an id the rename had just made stale. Fixed by having
// `NodeSettings` itself pass its post-rename id straight to `onChange`
// instead of leaving the caller to infer it. Cleans up the node it creates
// so this suite's fixture stays byte-for-byte reusable across runs, same as
// every other test above.
test("renaming the id and changing another field in the same 'ok' click both land on the same node", async ({
  page,
}) => {
  const before = await fetchRaw(page);

  // The "+" button is portaled via `NodeToolbar` into React Flow's shared
  // top-level layer (see MeshNode.tsx), not nested under the node's own
  // `[data-id]` element — so it can't be scoped to a specific node by DOM
  // nesting. Which parent gets the new child doesn't matter for this test.
  await page.locator(".mesh-node-add-child").first().click();

  await expect(page.locator(".node-settings-modal")).toBeVisible();
  const titleInput = page.locator(".vars-modal-field", { hasText: "Title" }).locator("input");
  const idInput = page.locator(".vars-modal-field", { hasText: "ID" }).locator("input");
  await expect(idInput).toHaveValue("new-node");

  await titleInput.fill("My title");
  await idInput.fill("my-title");
  await page.locator(".vars-modal-actions button", { hasText: "ok" }).click();

  await expect(page.locator(".node-settings-modal")).toHaveCount(0);
  await expect(page.locator(".error")).toHaveCount(0);
  const raw = await fetchRaw(page);
  expect(raw).toContain('id="my-title"');
  expect(raw).toContain("My title");

  // `DELETE` alone doesn't guarantee coming back byte-for-byte (it's not
  // meant to — `insert_child_node`/`delete_node` aren't exact inverses down
  // to whitespace), so restore the fixture's exact original text directly
  // via Source mode's own save endpoint instead, same as every other test
  // above expects to find the file in.
  await page.evaluate(
    (raw) => fetch("/api/canvas/raw", { method: "PUT", headers: { "content-type": "text/plain" }, body: raw }),
    before,
  );
  expect(await fetchRaw(page)).toBe(before);
});

// TODO.canvas.md: "Если не задан id в meshfox:node, использовать заоголовок"
// — the ID field can be left empty on "ok", which drops the node's explicit
// `id=` attribute entirely rather than rejecting the click as incomplete.
// "text-plain"'s id ("text-plain") deliberately doesn't match its title's
// slug ("plain-text" — see the fixture) so this also exercises the rename +
// reference-sweep path (`text-styled` carries `meshfox:edge from="text-plain"`),
// not just the trivial "id already equals slug(title)" case.
test("leaving the ID field empty on 'ok' clears the explicit id back to the title's slug", async ({ page }) => {
  const before = await fetchRaw(page);

  await openSettings(page, "text-plain");
  const idInput = page.locator(".vars-modal-field", { hasText: "ID" }).locator("input");
  await idInput.fill("");
  await clickOk(page);

  await expect(page.locator(".error")).toHaveCount(0);
  const raw = await fetchRaw(page);
  expect(raw).not.toContain('id="text-plain"');
  expect(raw).not.toContain('from="text-plain"');
  expect(raw).toContain('from="plain-text"');
  // The node itself is still there, just under the slug-derived id — the
  // client re-fetched it under the new id rather than losing track of it.
  await expect(page.locator('.react-flow__node[data-id="plain-text"]')).toBeVisible();

  await page.evaluate(
    (raw) => fetch("/api/canvas/raw", { method: "PUT", headers: { "content-type": "text/plain" }, body: raw }),
    before,
  );
  expect(await fetchRaw(page)).toBe(before);
});

// TODO.canvas.md: same item — the id-suggestion hint used to stop appearing
// forever on any node whose auto-generated id picked up a `-2`/`-3`... dedup
// suffix (`insert_child_node`'s own collision handling), since the gate only
// ever compared against the bare, unsuffixed slug. Two default-titled "New
// Node" children in a row reproduces the suffix; editing the second one's
// title should still offer to update its id.
test("the id-suggestion hint still appears for an auto-generated id that picked up a dedup suffix", async ({
  page,
}) => {
  const before = await fetchRaw(page);

  await page.locator(".mesh-node-add-child").first().click();
  await expect(page.locator(".node-settings-modal")).toBeVisible();
  await page.locator(".vars-modal-actions button", { hasText: "ok" }).click();
  await expect(page.locator(".node-settings-modal")).toHaveCount(0);
  // "ok" right after creating a node auto-opens its own body editor
  // (TODO.canvas.md: "Редактирование после Node settings") — a real user
  // creating a second node right away would close this first (its own
  // full-screen backdrop otherwise just intercepts the next click below);
  // do the same here rather than fighting the backdrop.
  await page.locator(".mesh-text-editor-actions button", { hasText: "done" }).click();

  await page.locator(".mesh-node-add-child").first().click();
  await expect(page.locator(".node-settings-modal")).toBeVisible();
  const idInput = page.locator(".vars-modal-field", { hasText: "ID" }).locator("input");
  await expect(idInput).toHaveValue("new-node-2");

  const titleInput = page.locator(".vars-modal-field", { hasText: "Title" }).locator("input");
  await titleInput.fill("Another Node");
  await expect(page.locator(".node-settings-id-hint")).toContainText("another-node");

  await page.locator(".vars-modal-actions button", { hasText: "cancel" }).click();
  await expect(page.locator(".node-settings-modal")).toHaveCount(0);

  await page.evaluate(
    (raw) => fetch("/api/canvas/raw", { method: "PUT", headers: { "content-type": "text/plain" }, body: raw }),
    before,
  );
  expect(await fetchRaw(page)).toBe(before);
});

// TODO.canvas.md: "Tag suggest" — the Tags field autocompletes from every
// tag already used elsewhere in the document ("text-styled" carries
// tags="alpha, beta" in this fixture), not just whatever's already on the
// node being edited.
test("the Tags field suggests tags already used elsewhere in the document", async ({ page }) => {
  await openSettings(page, "text-plain");
  const tagInput = page.locator(".tag-editor input");

  await tagInput.fill("al");
  await expect(page.locator(".tag-editor-suggestions button", { hasText: "alpha" })).toBeVisible();
  await expect(page.locator(".tag-editor-suggestions button", { hasText: "beta" })).toHaveCount(0);

  await page.locator(".tag-editor-suggestions button", { hasText: "alpha" }).click();
  await expect(page.locator(".tag-chip", { hasText: "alpha" })).toBeVisible();
  // "alpha" is already added — no longer offered, even though it still
  // matches; "beta" (not yet added, and also contains "a") still is.
  await tagInput.fill("a");
  await expect(page.locator(".tag-editor-suggestions button", { hasText: /^alpha$/ })).toHaveCount(0);
  await expect(page.locator(".tag-editor-suggestions button", { hasText: "beta" })).toBeVisible();

  await page.locator(".vars-modal-actions button", { hasText: "cancel" }).click();
  await expect(page.locator(".node-settings-modal")).toHaveCount(0);
});
