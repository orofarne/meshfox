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

  // Creating a node now starts inline title editing directly on the
  // canvas instead of opening NodeSettings (TODO.canvas.md: "Позволить
  // редактировать заголовок прямо на канвасе") — Escape leaves the node's
  // placeholder title untouched (nothing saved), then the gear button
  // opens NodeSettings same as for any other node, to reach the id+title
  // rename-together path this test is actually about.
  const titleEditInput = page.locator(".mesh-node-title-edit-input");
  await expect(titleEditInput).toBeFocused();
  await page.keyboard.press("Escape");

  const newNode = page.locator(".react-flow__node", { hasText: "New Node" });
  await selectNode(newNode);
  await toolbarButton(page, "settings").click();
  await expect(page.locator(".node-settings-modal")).toBeVisible();

  const titleInput = page.locator(".vars-modal-field", { hasText: "Title" }).locator("input");
  const idInput = page.locator(".vars-modal-field", { hasText: "ID" }).locator("input");
  // A freshly-created node's id is a random base36 hash now, not a slug of
  // its placeholder title (TODO.canvas.md: "Id-хэши вместо new-node-X по
  // умолчанию") — just confirm it's non-empty rather than asserting an
  // exact value.
  const initialId = await idInput.inputValue();
  expect(initialId).not.toBe("");

  await titleInput.fill("My title");
  await idInput.fill("my-title");
  await page.locator(".vars-modal-actions button", { hasText: "ok" }).click();

  await expect(page.locator(".node-settings-modal")).toHaveCount(0);
  await expect(page.locator(".error")).toHaveCount(0);
  const raw = await fetchRaw(page);
  expect(raw).toContain('id="my-title"');
  expect(raw).toContain("My title");
  expect(raw).not.toContain(`id="${initialId}"`);

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

// TODO.canvas.md: "Id-хэши вместо new-node-X по умолчанию" — a freshly
// created node's id is a random base36 hash now, not a slug of its
// placeholder title, so two default-titled "New Node" children in a row no
// longer produce a "new-node"/"new-node-2" dedup pair at all (there's
// nothing left to dedupe against — see `insert_child_node_random_id` in
// crates/core/src/mdcanvas.rs). This replaces an older test for that now-
// impossible dedup-suffix id, which also assumed "add child" opened
// NodeSettings immediately (it now starts inline title editing on the
// canvas instead — TODO.canvas.md: "Позволить редактировать заголовок
// прямо на канвасе").
test("a freshly created node gets a random base36 id, not a slug of its placeholder title", async ({ page }) => {
  const before = await fetchRaw(page);

  await page.locator(".mesh-node-add-child").first().click();
  const titleEditInput = page.locator(".mesh-node-title-edit-input");
  await expect(titleEditInput).toBeFocused();
  await page.keyboard.press("Escape");

  const newNode = page.locator(".react-flow__node", { hasText: "New Node" });
  await selectNode(newNode);
  await toolbarButton(page, "settings").click();
  await expect(page.locator(".node-settings-modal")).toBeVisible();
  const idInput = page.locator(".vars-modal-field", { hasText: "ID" }).locator("input");
  const id = await idInput.inputValue();
  expect(id).not.toBe("new-node");
  expect(id).toMatch(/^[0-9a-z]+$/);

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

// TODO.canvas.md: "Позволить редактировать заголовок прямо на канвасе" —
// double-clicking a node's title in Edit mode renames it in place, no
// NodeSettings modal involved at all.
test("double-clicking a node's title renames it inline on the canvas", async ({ page }) => {
  const before = await fetchRaw(page);

  const node = page.locator('.react-flow__node[data-id="text-plain"]');
  await node.locator(".mesh-node-title-text, .mesh-node-title-centered-text").first().dblclick();
  const input = node.locator(".mesh-node-title-edit-input");
  await expect(input).toBeFocused();
  await expect(input).toHaveValue("Plain Text");
  await input.fill("Renamed Plain Text");
  await input.press("Enter");

  await expect(node.locator(".mesh-node-title-text, .mesh-node-title-centered-text")).toContainText(
    "Renamed Plain Text",
  );
  const raw = await fetchRaw(page);
  expect(raw).toContain("Renamed Plain Text");
  expect(raw).not.toContain("## Plain Text\n");

  await page.evaluate(
    (raw) => fetch("/api/canvas/raw", { method: "PUT", headers: { "content-type": "text/plain" }, body: raw }),
    before,
  );
  expect(await fetchRaw(page)).toBe(before);
});

test("Escape cancels an inline title edit without saving", async ({ page }) => {
  const before = await fetchRaw(page);

  const node = page.locator('.react-flow__node[data-id="text-plain"]');
  await node.locator(".mesh-node-title-text, .mesh-node-title-centered-text").first().dblclick();
  const input = node.locator(".mesh-node-title-edit-input");
  await expect(input).toBeFocused();
  await input.fill("Should not be saved");
  await input.press("Escape");

  await expect(node.locator(".mesh-node-title-text, .mesh-node-title-centered-text")).toContainText("Plain Text");
  expect(await fetchRaw(page)).toBe(before);
});

// TODO.canvas.md: "Редактирование заголовка и вызов node settings внутри
// редактора" — Node settings isn't removed, it's just no longer the only
// way to rename: the inline body editor gets its own title field, plus a
// gear button straight into NodeSettings for everything else.
test("the body editor's header can rename the node and open NodeSettings", async ({ page }) => {
  const before = await fetchRaw(page);

  const node = page.locator('.react-flow__node[data-id="text-plain"]');
  await selectNode(node);
  await toolbarButton(page, "Edit this node's Markdown text").click();
  await expect(page.locator(".mesh-text-editor")).toBeVisible();

  const titleInput = page.locator(".mesh-text-editor-title-input");
  await expect(titleInput).toHaveValue("Plain Text");
  await titleInput.fill("Renamed From Editor");
  await titleInput.press("Enter");
  await expect(node.locator(".mesh-node-title-text, .mesh-node-title-centered-text")).toContainText(
    "Renamed From Editor",
  );

  await page.locator(".mesh-text-editor-header button", { hasText: "⚙" }).click();
  await expect(page.locator(".node-settings-modal")).toBeVisible();
  const settingsTitleInput = page.locator(".vars-modal-field", { hasText: "Title" }).locator("input");
  await expect(settingsTitleInput).toHaveValue("Renamed From Editor");
  await page.locator(".vars-modal-actions button", { hasText: "cancel" }).click();
  await expect(page.locator(".node-settings-modal")).toHaveCount(0);

  await page.locator(".mesh-text-editor-actions button", { hasText: "done" }).click();

  await page.evaluate(
    (raw) => fetch("/api/canvas/raw", { method: "PUT", headers: { "content-type": "text/plain" }, body: raw }),
    before,
  );
  expect(await fetchRaw(page)).toBe(before);
});

// TODO.canvas.md: "Позволить редактировать заголовок прямо на канвасе"
// extended to the body — double-clicking a text node's rendered body in
// Edit mode opens the same inline editor its own ✏ button does, without
// needing to select the node or hunt down the toolbar button first.
test("double-clicking a node's body opens its inline text editor", async ({ page }) => {
  const node = page.locator('.react-flow__node[data-id="text-plain"]');
  await node.locator(".mesh-node-body").dblclick();
  await expect(page.locator(".mesh-text-editor")).toBeVisible();
  await expect(page.locator(".mesh-text-editor-title-input")).toHaveValue("Plain Text");

  await page.locator(".mesh-text-editor-actions button", { hasText: "done" }).click();
  await expect(page.locator(".mesh-text-editor")).toHaveCount(0);
});

// Same gesture, but back in read-only mode — must stay a no-op, same as
// the ✏ button itself, which isn't even rendered outside Edit mode.
// `beforeEach` above always starts a test in Edit mode, so this leaves it
// via the toolbar's own "done" button first.
test("double-clicking a node's body does nothing outside Edit mode", async ({ page }) => {
  await page.getByRole("button", { name: "done" }).click();
  const node = page.locator('.react-flow__node[data-id="text-plain"]');
  await node.locator(".mesh-node-body").dblclick();
  await expect(page.locator(".mesh-text-editor")).toHaveCount(0);
});

// TODO.canvas.md: "Текст в нодах типа link/file" — a `file`/`link`/
// `include` body may carry a plain-prose caption after the required link
// (`mdcanvas::parse_link_body`/`caption_is_plain_prose`). Hand-writes the
// caption via Source mode's own raw endpoint (there's no UI to author one
// yet — see this feature's own "Сделано" note) and checks it renders.
test("a link node's caption renders below the plain link, with inline formatting", async ({ page }) => {
  const before = await fetchRaw(page);
  const withCaption = before.replace(
    "[example](https://example.com)\n",
    "[example](https://example.com)\n\nA **bold** note with `code` and a [nested link](https://other.example).\n",
  );
  await page.evaluate(
    (raw) => fetch("/api/canvas/raw", { method: "PUT", headers: { "content-type": "text/plain" }, body: raw }),
    withCaption,
  );
  await page.reload();
  await page.waitForSelector(".mesh-node");

  const node = page.locator('.react-flow__node[data-id="link-node"]');
  const body = node.locator(".mesh-node-body");
  await expect(body).toContainText("A bold note with code and a nested link.");
  await expect(body.locator("strong", { hasText: "bold" })).toBeVisible();
  await expect(body.locator("code", { hasText: "code" })).toBeVisible();
  await expect(body.locator("a", { hasText: "nested link" })).toHaveAttribute(
    "href",
    "https://other.example",
  );

  await page.evaluate(
    (raw) => fetch("/api/canvas/raw", { method: "PUT", headers: { "content-type": "text/plain" }, body: raw }),
    before,
  );
  expect(await fetchRaw(page)).toBe(before);
});

test("a file node's display=code caption renders after the file preview", async ({ page }) => {
  const before = await fetchRaw(page);
  const withCaption = before.replace(
    '<!-- meshfox:node id="file-code" type="file" display="code" lang="text" interpreter="cat" -->\n\n[settings-file-target.txt](./settings-file-target.txt)\n',
    '<!-- meshfox:node id="file-code" type="file" display="code" lang="text" interpreter="cat" -->\n\n[settings-file-target.txt](./settings-file-target.txt)\n\nCaption for the preview.\n',
  );
  expect(withCaption).not.toBe(before);
  await page.evaluate(
    (raw) => fetch("/api/canvas/raw", { method: "PUT", headers: { "content-type": "text/plain" }, body: raw }),
    withCaption,
  );
  await page.reload();
  await page.waitForSelector(".mesh-node");

  const node = page.locator('.react-flow__node[data-id="file-code"]');
  await expect(node).toContainText("Caption for the preview.");

  await page.evaluate(
    (raw) => fetch("/api/canvas/raw", { method: "PUT", headers: { "content-type": "text/plain" }, body: raw }),
    before,
  );
  expect(await fetchRaw(page)).toBe(before);
});

// TODO.canvas.md: same item — NodeSettings' "URL" field used to rewrite a
// link/file node's whole body as just `[title](target)`, silently dropping
// any caption. `crates/server/src/lib.rs`'s `update_node` now preserves it.
test("changing a link node's URL via NodeSettings preserves its existing caption", async ({ page }) => {
  const before = await fetchRaw(page);
  const withCaption = before.replace(
    "[example](https://example.com)\n",
    "[example](https://example.com)\n\nAn existing caption.\n",
  );
  await page.evaluate(
    (raw) => fetch("/api/canvas/raw", { method: "PUT", headers: { "content-type": "text/plain" }, body: raw }),
    withCaption,
  );
  // A reload resets Edit mode and the viewport — both needed again for
  // `openSettings` below (the settings gear only shows in Edit mode, and
  // `selectNode`'s click needs the node actually on-screen).
  await page.reload();
  await page.waitForSelector(".mesh-node");
  await clickFitViewAndWait(page);
  await page.getByRole("button", { name: "Edit" }).click();

  await openSettings(page, "link-node");
  const urlInput = page.locator(".vars-modal-field", { hasText: "URL" }).locator("input");
  await urlInput.fill("https://changed.example");
  await clickOk(page);

  const node = page.locator('.react-flow__node[data-id="link-node"]');
  await expect(node).toContainText("An existing caption.");
  const raw = await fetchRaw(page);
  expect(raw).toContain("https://changed.example");
  expect(raw).toContain("An existing caption.");

  await page.evaluate(
    (raw) => fetch("/api/canvas/raw", { method: "PUT", headers: { "content-type": "text/plain" }, body: raw }),
    before,
  );
  expect(await fetchRaw(page)).toBe(before);
});
