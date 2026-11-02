import { expect, test, type Page } from "@playwright/test";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

// The page must work with no `meshfox` behind it: every real network request
// to `/api/` is refused, so anything that still renders was answered by the
// WebAssembly model inside the page.
test.beforeEach(async ({ page }) => {
  await page.route("**/api/**", (route) => route.abort());
});

const canvasJson = (page: Page) =>
  page.evaluate(() => fetch("api/canvas").then((r) => r.json())) as Promise<{
    nodes: Array<{ id: string; text: string; plainMarkdownInclude?: boolean; effectiveColor?: string }>;
    options: string[];
  }>;

function tmpDir(): string {
  return fs.mkdtempSync(path.join(os.tmpdir(), "mfx-wasm-editor-"));
}

const REPO = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../..");

/** A temp folder holding copies of some of the repository's own documents,
 * at the same relative paths, ready for the folder picker. */
function repoFolder(...rel: string[]): string {
  const dir = tmpDir();
  for (const r of rel) {
    fs.mkdirSync(path.dirname(path.join(dir, r)), { recursive: true });
    fs.cpSync(path.join(REPO, r), path.join(dir, r), { recursive: true });
  }
  return dir;
}

const ROOT = '# Root\n<!-- meshfox:node id="root" -->\n\nThe root.\n\n';
const INCLUDE = '## Notes\n<!-- meshfox:node id="notes" type="include" -->\n\n[notes](./notes.md)\n';

// 1x1 transparent PNG.
const PNG = Buffer.from(
  "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==",
  "base64",
);

test("a repository example opens from a picked file, with no server behind it", async ({ page }) => {
  await page.goto("/");
  await page.locator("input[type=file]:not([webkitdirectory])").setInputFiles(path.join(REPO, "examples/hello.canvas.md"));
  await expect(page.getByText("Hello Project")).toBeVisible();
  await expect(page.getByText("noexec", { exact: true })).toBeVisible();
  await expect(page.getByRole("button", { name: "Edit", exact: true })).toBeVisible();
  await expect(page.locator("#wasm-pill")).toContainText("in your browser");
});

test("constraint fences run in the page and badge the nodes", async ({ page }) => {
  await page.goto("/");
  await page
    .locator("input[webkitdirectory]")
    .setInputFiles(repoFolder("examples/constraints.canvas.md", "examples/constraints-data"));
  await expect(page.getByText("3/3")).toBeVisible();
});

test("README resolves its include from the picked folder", async ({ page }) => {
  await page.goto("/");
  await page.locator("input[webkitdirectory]").setInputFiles(repoFolder("README.md", "TEMPLATES.md"));
  await page.getByRole("button", { name: "README.md" }).click();
  await expect(page.locator(".react-flow__node").first()).toBeVisible();
  const canvas = await canvasJson(page);
  const included = canvas.nodes.find((n) => n.id === "static-site-templates")!;
  expect(included.plainMarkdownInclude).toBe(true);
  expect(included.text.length).toBeGreaterThan(5000);
});

test("opening a folder resolves includes, assets and file previews from it", async ({ page }) => {
  const dir = tmpDir();
  fs.writeFileSync(
    path.join(dir, "doc.canvas.md"),
    ROOT +
      INCLUDE +
      '\n## Data\n<!-- meshfox:node id="data" type="file" display="code" -->\n\n[data](./data.txt)\n',
  );
  fs.writeFileSync(path.join(dir, "notes.md"), "# Notes\n\nfrom the opened folder\n");
  fs.writeFileSync(path.join(dir, "data.txt"), "hello data\n");
  fs.writeFileSync(path.join(dir, "pic.png"), PNG);
  fs.mkdirSync(path.join(dir, "node_modules"));
  fs.writeFileSync(path.join(dir, "node_modules", "skipped.md"), "# not read\n");

  await page.goto("/");
  await page.locator("input[webkitdirectory]").setInputFiles(dir);
  await expect(page.getByText("The root.")).toBeVisible();

  const canvas = await canvasJson(page);
  expect(canvas.nodes.find((n) => n.id === "notes")!.text).toContain("from the opened folder");

  const content = await page.evaluate(() => fetch("api/nodes/data/file-content").then((r) => r.json()));
  expect(content).toEqual({ content: "hello data\n", truncated: false });

  const asset = await page.evaluate(async () => {
    const r = await fetch("api/asset?file=pic.png");
    return { status: r.status, type: r.headers.get("content-type"), size: (await r.arrayBuffer()).byteLength };
  });
  expect(asset).toEqual({ status: 200, type: "image/png", size: PNG.length });

  const missing = await page.evaluate(() => fetch("api/asset?file=nope.png").then((r) => r.status));
  expect(missing).toBe(404);
});

test("a picked folder is read by the model's ignore rules: dots and .meshfoxignore, nothing by name", async ({ page }) => {
  // Safari's folder picker leaves out every dot-file, `.meshfoxignore` included,
  // so a *picked* folder there cannot honour it (a dropped one can).
  test.skip(isWebkit(page), "Safari's folder picker omits dot-files");
  const dir = tmpDir();
  const put = (rel: string, text: string) => {
    fs.mkdirSync(path.dirname(path.join(dir, rel)), { recursive: true });
    fs.writeFileSync(path.join(dir, rel), text);
  };
  put(
    "doc.canvas.md",
    ROOT +
      '## A\n<!-- meshfox:node id="a" type="include" -->\n\n[a](./node_modules/a.md)\n\n' +
      '## B\n<!-- meshfox:node id="b" type="include" -->\n\n[b](./drafts/b.md)\n',
  );
  put("node_modules/a.md", "# A\n\nread, though it is called node_modules\n");
  put("drafts/b.md", "# B\n\nexcluded by .meshfoxignore\n");
  put("drafts/other.canvas.md", ROOT); // would make the pick ask "which one?" if it were read
  put(".hidden/c.canvas.md", ROOT); // dot directory
  put(".meshfoxignore", "# not for the viewer\ndrafts/\n");

  await page.goto("/");
  await page.locator("input[webkitdirectory]").setInputFiles(dir);
  // `drafts/b.md` was never read, so its include shows a notice. (And there is
  // no "which one?": the excluded and hidden canvases are not there.)
  await expect(page.getByText("The root.")).toBeVisible();
  await expect(page.locator("#wasm-pill")).toContainText("1 include missing: drafts/b.md");

  // Without the ignore file the same folder opens, `node_modules` included.
  fs.rmSync(path.join(dir, ".meshfoxignore"));
  put("doc.canvas.md", ROOT + '## A\n<!-- meshfox:node id="a" type="include" -->\n\n[a](./node_modules/a.md)\n');
  fs.rmSync(path.join(dir, "drafts"), { recursive: true });
  await page.goto("/");
  await page.reload();
  await page.locator("input[webkitdirectory]").setInputFiles(dir);
  await expect(page.getByText("The root.")).toBeVisible();
  expect((await canvasJson(page)).nodes.find((n) => n.id === "a")!.text).toContain("read, though it is called node_modules");
});

test("images in a document are shown, not just fetchable", async ({ page }) => {
  // `<img src="api/asset?...">` is loaded by the browser itself, not by
  // `fetch`: the page has to hand it a blob URL built from the opened files.
  const dir = tmpDir();
  fs.writeFileSync(
    path.join(dir, "pics.canvas.md"),
    '# Pics\n<!-- meshfox:node id="root" -->\n\n![a tiny picture](./pic.png)\n',
  );
  fs.writeFileSync(path.join(dir, "pic.png"), PNG);
  await page.goto("/");
  await page.locator("input[webkitdirectory]").setInputFiles(dir);
  await expect
    .poll(() =>
      page.evaluate(() =>
        [...document.images].some((i) => i.alt === "a tiny picture" && i.src.startsWith("blob:") && i.naturalWidth > 0),
      ),
    )
    .toBe(true);
});

test("a document with SVG diagrams shows them, with no failed requests", async ({ page }) => {
  const refused: string[] = [];
  page.on("requestfailed", (r) => refused.push(r.url()));
  page.on("response", (r) => r.status() >= 400 && refused.push(`${r.status()} ${r.url()}`));
  await page.goto("/");
  await page
    .locator("input[webkitdirectory]")
    .setInputFiles(repoFolder("examples/canvas-call.canvas.md", "examples/canvas-call-data"));
  await page.getByRole("button", { name: "examples/canvas-call.canvas.md" }).click();
  const node = page.locator(".react-flow__node", { hasText: "How the call works" }).first();
  await node.getByText("▸").first().click();
  await expect
    .poll(() =>
      page.evaluate(() => {
        const images = [...document.images];
        return images.length > 0 && images.every((i) => !i.src.includes("api/asset") && i.complete && i.naturalWidth > 0);
      }),
    )
    .toBe(true);
  expect(refused).toEqual([]);
});

test("a single file works when it needs nothing else", async ({ page }) => {
  const dir = tmpDir();
  const file = path.join(dir, "solo.canvas.md");
  fs.writeFileSync(
    file,
    '# Solo\n<!-- meshfox:node id="root" -->\n<!-- meshfox:tag-color tag="bug" color="1" -->\n\n' +
      '## Child\n<!-- meshfox:node id="child" tags="bug" -->\n\nbody\n',
  );
  await page.goto("/");
  await page.locator("input[type=file]:not([webkitdirectory])").setInputFiles(file);
  await expect(page.getByText("Solo", { exact: true }).first()).toBeVisible();
  expect((await canvasJson(page)).nodes.find((n) => n.id === "child")!.effectiveColor).toBe("1");
});

test("a canvas whose include is missing opens, with a notice where it goes", async ({ page }) => {
  const dir = tmpDir();
  const file = path.join(dir, "doc.canvas.md");
  fs.writeFileSync(file, ROOT + INCLUDE);
  await page.goto("/");
  await page.locator("input[type=file]:not([webkitdirectory])").setInputFiles(file);
  await expect(page.getByText("The root.")).toBeVisible();
  await expect(page.locator("#launcher")).toHaveCount(0);
  await expect(page.locator("#wasm-pill")).toContainText("1 include missing: notes.md");
  const notes = (await canvasJson(page)).nodes.find((n) => n.id === "notes")!;
  expect(notes.text).toContain("[!WARNING]");
  expect(notes.text).toContain("./notes.md");
  // The notice is drawn as a GFM alert, in the node.
  await expect(page.locator(".markdown-alert-warning")).toContainText("Include not available");
});

test("a document that does not parse is still refused in the launcher", async ({ page }) => {
  const file = path.join(tmpDir(), "bad.canvas.md");
  fs.writeFileSync(file, "no heading here\n");
  await page.goto("/");
  await page.locator("input[type=file]:not([webkitdirectory])").setInputFiles(file);
  await expect(page.locator("#status.error")).toBeVisible();
  await expect(page.locator("#launcher")).toBeVisible();
});

test("a folder with several canvases asks which one", async ({ page }) => {
  const dir = tmpDir();
  fs.writeFileSync(path.join(dir, "a.canvas.md"), ROOT);
  fs.writeFileSync(path.join(dir, "b.canvas.md"), ROOT.replace("# Root", "# Second one"));
  await page.goto("/");
  await page.locator("input[webkitdirectory]").setInputFiles(dir);
  await page.getByRole("button", { name: "b.canvas.md" }).click();
  await expect(page.getByText("Second one").first()).toBeVisible();
});

test("Back returns to the launcher and Forward does not leave a blank page", async ({ page }) => {
  const file = path.join(tmpDir(), "doc.canvas.md");
  fs.writeFileSync(file, ROOT);
  await page.goto("/");
  await page.locator("input[type=file]:not([webkitdirectory])").setInputFiles(file);
  await expect(page.getByText("The root.")).toBeVisible();

  await page.goBack();
  await expect(page.locator("#launcher")).toBeVisible();
  await expect(page.locator(".react-flow__node")).toHaveCount(0);

  await page.goForward();
  await expect(page.locator("#launcher")).toBeVisible();
  await expect(page.locator("#status")).toContainText("only in memory");
  await expect(page.locator("#wasm-pill")).toHaveCount(0);
});

test("\"open another\" in the pill goes back to the launcher", async ({ page }) => {
  const file = path.join(tmpDir(), "doc.canvas.md");
  fs.writeFileSync(file, ROOT);
  await page.goto("/");
  await page.locator("input[type=file]:not([webkitdirectory])").setInputFiles(file);
  await expect(page.getByText("The root.")).toBeVisible();
  await page.locator("#wasm-pill").getByText("open another").click();
  await expect(page.locator("#launcher")).toBeVisible();
});

test("reloading a document page lands on the launcher", async ({ page }) => {
  const file = path.join(tmpDir(), "doc.canvas.md");
  fs.writeFileSync(file, ROOT);
  await page.goto("/");
  await page.locator("input[type=file]:not([webkitdirectory])").setInputFiles(file);
  await expect(page.getByText("The root.")).toBeVisible();
  await page.reload();
  await expect(page.locator("#launcher")).toBeVisible();
  await expect(page.locator("#status")).toContainText("only in memory");
});

test("nothing was ever sent to a server", async ({ page }) => {
  const sent: string[] = [];
  page.on("request", (r) => {
    if (new URL(r.url()).pathname.includes("/api/")) sent.push(r.url());
  });
  await page.goto("/");
  await page
    .locator("input[webkitdirectory]")
    .setInputFiles(repoFolder("examples/constraints.canvas.md", "examples/constraints-data"));
  await expect(page.getByText("3/3")).toBeVisible();
  expect(sent).toEqual([]);
});

test("with execution disabled there is nothing to click that would run", async ({ page }) => {
  const file = path.join(tmpDir(), "run.canvas.md");
  fs.writeFileSync(
    file,
    [
      '# Root\n<!-- meshfox:node id="root" -->\n<!-- meshfox:option name="unfold" -->\n\nThe root.\n',
      '## Runnable\n<!-- meshfox:node id="runnable" -->\n\nBefore.\n\n```bash name="hello" deps="other"\necho hello\n```\n',
      '## Other\n<!-- meshfox:node id="other" -->\n\n```bash name="other"\necho other\n```\n',
      '## Pipeline\n<!-- meshfox:node id="pipeline" -->\n\n```button name="go" deps="other/other"\nBuild everything\n```\n',
      '## Settings\n<!-- meshfox:node id="settings" -->\n\n```form name="settings" send="Apply"\nfield var="X" label="An x"\n```\n',
    ].join("\n"),
  );
  await page.goto("/");
  await page.locator("input[type=file]:not([webkitdirectory])").setInputFiles(file);
  await expect(page.getByText("The root.")).toBeVisible();
  await expect(page.getByText("noexec", { exact: true })).toBeVisible();

  // The code stays; every control that would run it is gone.
  await expect(page.locator(".mesh-code-block")).toHaveCount(2);
  await expect(page.locator(".mesh-code-block button", { hasText: /^run / })).toHaveCount(0);
  await expect(page.locator(".mesh-run-chain")).toHaveCount(0);
  await expect(page.locator(".mesh-kill-button")).toHaveCount(0);
  await expect(page.locator(".mesh-node-quick-run-icon")).toHaveCount(0);
  await expect(page.locator(".mesh-button-block, .mesh-run-button")).toHaveCount(0);
  await expect(page.locator(".mesh-form-block")).toHaveCount(0);
  // Nothing ever ran here, so there is no run history either (a `--no-exec`
  // worker, by contrast, keeps the button: earlier runs may be on record).
  await expect(page.getByRole("button", { name: "Run history" })).toHaveCount(0);

  // And nothing in the toolbar for runs.
  await expect(page.getByRole("button", { name: /Console/ })).toHaveCount(0);
  await expect(page.getByRole("button", { name: /reset session/ })).toHaveCount(0);
  await expect(page.getByRole("button", { name: /search/ })).toBeVisible();
});

test("add files fills a missing include, wherever the picked file sits", async ({ page }) => {
  const dir = tmpDir();
  const doc = path.join(dir, "doc.canvas.md");
  fs.writeFileSync(
    doc,
    ROOT +
      '## Notes\n<!-- meshfox:node id="notes" type="include" -->\n\n[notes](./docs/notes.md)\n\n' +
      '## Other\n<!-- meshfox:node id="other" type="include" -->\n\n[other](./other.md)\n',
  );
  const notes = path.join(dir, "notes.md");
  fs.writeFileSync(notes, "# Notes\n\nfrom the added file\n");
  await page.goto("/");
  await page.locator("input[type=file]:not([webkitdirectory])").setInputFiles(doc);
  await expect(page.getByText("The root.")).toBeVisible();
  await expect(page.locator("#wasm-pill")).toContainText("2 includes missing: docs/notes.md, other.md");
  await expect(page.locator(".markdown-alert-warning")).toHaveCount(2);

  // `notes.md` alone goes to `docs/notes.md`, where the include looks.
  await page.locator("#wasm-pill input[type=file]:not([webkitdirectory])").setInputFiles(notes);
  await expect(page.locator("#wasm-pill")).toContainText("1 include missing: other.md");
  await expect(page.getByText("from the added file")).toBeVisible();
  await expect(page.locator(".markdown-alert-warning")).toHaveCount(1);

  // A whole folder fills the rest, and the page never reloaded.
  const more = tmpDir();
  fs.writeFileSync(path.join(more, "other.md"), "# Other\n\nfrom the added folder\n");
  await page.locator("#wasm-pill input[webkitdirectory]").setInputFiles(more);
  await expect(page.locator("#wasm-pill")).not.toContainText("missing");
  await expect(page.getByText("from the added folder")).toBeVisible();
  await expect(page.locator(".markdown-alert-warning")).toHaveCount(0);
  await expect(page.getByText("from the added file")).toBeVisible();
});

test("what the page remembers about a document is keyed by the document, not just its root id", async ({ page }) => {
  // Every document's default root id is `root`; two of them must not share
  // their folded-nodes entry.
  const dir = tmpDir();
  fs.writeFileSync(path.join(dir, "doc.canvas.md"), ROOT + '## Child\n<!-- meshfox:node id="child" -->\n\nbody\n');
  await page.goto("/");
  await page.locator("input[webkitdirectory]").setInputFiles(dir);
  await expect(page.getByText("The root.")).toBeVisible();
  await expect
    .poll(() => page.evaluate(() => Object.keys(localStorage).filter((k) => k.startsWith("meshfox-folded:"))))
    .toEqual([`meshfox-folded:${path.basename(dir)}/doc.canvas.md:root`]);
});

// Monaco's shortcuts follow the browser's reported platform, and Playwright's
// desktop presets report Linux/Windows whatever the host: always Control.
// The device presets report Linux/Windows (Control) except Safari's, which is a Mac (Meta).
const isWebkit = (page: Page) => page.context().browser()?.browserType().name() === "webkit";
const selectAll = (page: Page) => (isWebkit(page) ? "Meta+A" : "Control+A");
const endOfDocument = (page: Page) => (isWebkit(page) ? "Meta+ArrowDown" : "Control+End");

/** Opens Source mode and types into Monaco: `replace` swaps the whole text,
 * otherwise `text` goes at the end. */
async function typeInSource(page: Page, text: string, replace = false) {
  // Already editing, on a second edit: there is no "Edit" button then.
  const edit = page.getByRole("button", { name: "Edit", exact: true });
  const source = page.getByRole("button", { name: "Source" });
  await expect(edit.or(source)).toBeVisible();
  if ((await edit.count()) > 0) await edit.click();
  await source.click();
  const editor = page.locator(".mesh-source-editor-body .monaco-editor");
  await editor.waitFor();
  // Only once the document's text is in the editor does a key press mean anything.
  await expect(page.locator(".mesh-source-editor-body .view-lines")).toContainText("meshfox:");
  await editor.click();
  await page.keyboard.press(replace ? selectAll(page) : endOfDocument(page));
  // Inserted in one go, not typed: typing makes Monaco complete and commit words.
  // Chromium takes a (synthetic) paste event; Firefox's Monaco ignores that and
  // doubles `insertText`, so it pastes from the real clipboard.
  if (page.context().browser()?.browserType().name() === "firefox") {
    await page.evaluate((value) => navigator.clipboard.writeText(value), text);
    await page.keyboard.press("Control+V");
  } else {
    await page.locator(".mesh-source-editor-body textarea").evaluate((el, value) => {
      const data = new DataTransfer();
      data.setData("text/plain", value);
      el.dispatchEvent(new ClipboardEvent("paste", { clipboardData: data, bubbles: true, cancelable: true }));
    }, text);
  }
}

async function editSource(page: Page, addition: string) {
  await typeInSource(page, addition);
  const save = page.locator(".mesh-source-editor-actions button", { hasText: "Save" });
  await expect(save).toBeEnabled();
  await save.click();
  // A saved edit closes the source editor, back to the canvas.
  await expect(page.locator(".mesh-source-editor")).toHaveCount(0);
}

const ADDED = '\n## Added\n<!-- meshfox:node id="added" -->\n\nadded in the browser\n';

test("edit the source, download the result, undo", async ({ page }) => {
  const file = path.join(tmpDir(), "doc.canvas.md");
  fs.writeFileSync(file, ROOT);
  await page.goto("/");
  await page.locator("input[type=file]:not([webkitdirectory])").setInputFiles(file);
  await expect(page.getByText("The root.")).toBeVisible();
  await expect(page.locator("#wasm-pill")).not.toContainText("unsaved");

  await editSource(page, ADDED);
  await expect(page.getByText("added in the browser")).toBeVisible();
  await expect(page.locator("#wasm-pill")).toContainText("unsaved changes");

  // What leaves the page is the edited file, byte for byte.
  const downloading = page.waitForEvent("download");
  await page.locator("#wasm-download").click();
  const download = await downloading;
  expect(download.suggestedFilename()).toBe("doc.canvas.md");
  const saved = fs.readFileSync((await download.path())!, "utf8");
  expect(saved).toBe(ROOT + ADDED);
  await expect(page.locator("#wasm-pill")).not.toContainText("unsaved");

  // Undo walks back through the edit; the shown text follows, and the page is
  // "unsaved" again: it no longer matches what was downloaded. Redo returns to
  // the downloaded text, so it is clean again.
  await page.getByRole("button", { name: /undo/ }).click();
  await expect(page.getByText("added in the browser")).toHaveCount(0);
  await expect(page.locator("#wasm-pill")).toContainText("unsaved changes");
  await page.getByRole("button", { name: /redo/ }).click();
  await expect(page.getByText("added in the browser")).toBeVisible();
  await expect(page.locator("#wasm-pill")).not.toContainText("unsaved");
});

test("a source edit that does not parse is refused and changes nothing", async ({ page }) => {
  const file = path.join(tmpDir(), "doc.canvas.md");
  fs.writeFileSync(file, ROOT);
  await page.goto("/");
  await page.locator("input[type=file]:not([webkitdirectory])").setInputFiles(file);
  await expect(page.getByText("The root.")).toBeVisible();
  await typeInSource(page, "---\n", true); // no letters: nothing for Monaco to autocomplete
  const save = page.locator(".mesh-source-editor-actions button", { hasText: "Save" });
  await expect(save).toBeEnabled();
  await save.click();
  await expect(page.locator(".mesh-source-editor-toolbar .error")).toBeVisible();
  await expect(page.locator("#wasm-pill")).not.toContainText("unsaved");
});

test("leaving a document with unsaved edits asks first; staying keeps it", async ({ page }) => {
  const file = path.join(tmpDir(), "doc.canvas.md");
  fs.writeFileSync(file, ROOT);
  await page.goto("/");
  await page.locator("input[type=file]:not([webkitdirectory])").setInputFiles(file);
  await expect(page.getByText("The root.")).toBeVisible();
  await editSource(page, ADDED);
  await expect(page.locator("#wasm-pill")).toContainText("unsaved changes");

  const asked: string[] = [];
  page.once("dialog", (d) => {
    asked.push(d.message());
    void d.dismiss();
  });
  await page.goBack();
  await expect.poll(() => asked.length).toBe(1);
  await expect(page.getByText("added in the browser")).toBeVisible();
  await expect(page.locator("#launcher")).toHaveCount(0);

  page.once("dialog", (d) => void d.accept());
  await page.goBack();
  await expect(page.locator("#launcher")).toBeVisible();
});

test("the history panel lists the steps, and jumping back through it restores the text", async ({ page }) => {
  const file = path.join(tmpDir(), "doc.canvas.md");
  fs.writeFileSync(file, ROOT);
  await page.goto("/");
  await page.locator("input[type=file]:not([webkitdirectory])").setInputFiles(file);
  await expect(page.getByText("The root.")).toBeVisible();
  await editSource(page, ADDED);
  await expect(page.getByText("added in the browser")).toBeVisible();

  await page.getByRole("button", { name: /history/ }).click();
  const entry = page.getByRole("button", { name: /edited the source/ });
  await expect(entry).toHaveCount(1);
  // Jump to the start through the panel itself: the text goes...
  await page.getByRole("button", { name: "Start of history" }).click();
  await expect(page.getByText("added in the browser")).toHaveCount(0);
  // ...the undone step stays listed (the redo tail, until a fresh edit drops it)...
  await expect(entry).toHaveCount(1);
  // ...and jumping to it brings the text back.
  await entry.click();
  await expect(page.getByText("added in the browser")).toBeVisible();
});

test("Enter in the new-canvas name starts it, like the button", async ({ page }) => {
  await page.goto("/");
  const name = page.locator("#new-name");
  await name.click();
  await page.keyboard.press("ControlOrMeta+A");
  await page.keyboard.type("quick");
  await page.keyboard.press("Enter");
  await expect(page.locator("#launcher")).toHaveCount(0);
  await expect(page.getByText("quick", { exact: true }).first()).toBeVisible();
});

test("a new canvas can be started, edited and downloaded without opening anything", async ({ page }) => {
  await page.goto("/");
  // Typed with real key presses (`fill` would not notice a field that eats keys).
  const name = page.locator("#new-name");
  await name.click();
  await page.keyboard.press("ControlOrMeta+A");
  await page.keyboard.type("my notes");
  await expect(name).toHaveValue("my notes");
  await page.locator("#new-canvas").click();
  // The root is named after the file, as `meshfox create` does.
  await expect(page.getByText("my notes", { exact: true }).first()).toBeVisible();
  await expect(page.locator("#wasm-pill")).not.toContainText("unsaved");

  await editSource(page, ADDED);
  await expect(page.getByText("added in the browser")).toBeVisible();
  const downloading = page.waitForEvent("download");
  await page.locator("#wasm-download").click();
  const download = await downloading;
  expect(download.suggestedFilename()).toBe("my notes.canvas.md");
  const saved = fs.readFileSync((await download.path())!, "utf8");
  expect(saved.startsWith("<!-- meshfox:canvas -->\n# my notes\n")).toBe(true);
  expect(saved.endsWith(ADDED)).toBe(true);
});

test("a node's settings are edited in the page by the same code as the server's, and undone", async ({ page }) => {
  const file = path.join(tmpDir(), "doc.canvas.md");
  fs.writeFileSync(file, ROOT + '## Child\n<!-- meshfox:node id="child" -->\n\nchild body\n');
  await page.goto("/");
  await page.locator("input[type=file]:not([webkitdirectory])").setInputFiles(file);
  await expect(page.getByText("child body")).toBeVisible();
  await page.getByRole("button", { name: "Edit", exact: true }).click();

  const child = page.locator('.react-flow__node[data-id="child"]');
  await child.locator(".mesh-node-title-text, .mesh-node-title-centered-text").first().click();
  await page.locator('.mesh-node-toolbar button[title*="settings" i]').click();
  const modal = page.locator(".node-settings-modal");
  await modal.getByLabel("Title").fill("Renamed child");
  await modal.getByLabel("Color").fill("3");
  await page.locator(".vars-modal-actions button", { hasText: "ok" }).click();
  await expect(modal).toHaveCount(0);

  // The node shows the new title, and the document text says so too.
  await expect(child).toContainText("Renamed child");
  const canvas = await canvasJson(page);
  const node = canvas.nodes.find((n) => n.id === "child")! as unknown as { title: string; color?: string };
  expect([node.title, node.color]).toEqual(["Renamed child", "3"]);
  await expect(page.locator("#wasm-pill")).toContainText("unsaved changes");
  const raw = await page.evaluate(() => fetch("api/canvas/raw").then((r) => r.text()));
  expect(raw).toContain("## Renamed child");
  expect(raw).toContain('color="3"');
  expect(raw).toContain("child body");

  // One undo takes both fields back.
  await page.getByRole("button", { name: /undo/ }).click();
  await expect(child).not.toContainText("Renamed child");
  const back = await page.evaluate(() => fetch("api/canvas/raw").then((r) => r.text()));
  expect(back).not.toContain("Renamed");
  expect(back).not.toContain('color="3"');
});

test("add a child node, delete it, save a layout — each is one undoable step of the page's own", async ({ page }) => {
  const file = path.join(tmpDir(), "doc.canvas.md");
  fs.writeFileSync(file, ROOT);
  await page.goto("/");
  await page.locator("input[type=file]:not([webkitdirectory])").setInputFiles(file);
  await expect(page.getByText("The root.")).toBeVisible();
  const raw = () => page.evaluate(() => fetch("api/canvas/raw").then((r) => r.text()));
  await page.getByRole("button", { name: "Edit", exact: true }).click();

  // "+" next to a node adds a child under it.
  const root = page.locator('.react-flow__node[data-id="root"]');
  await root.locator(".mesh-node-title-text, .mesh-node-title-centered-text").first().click();
  await page.locator(".mesh-node-add-child").click();
  await expect.poll(async () => (await canvasJson(page)).nodes.length).toBe(2);
  const created = (await canvasJson(page)).nodes.find((n) => n.id !== "root")!;
  expect(await raw()).toContain(`id="${created.id}"`);
  await expect(page.locator("#wasm-pill")).toContainText("unsaved changes");

  // Undo takes the new node away again; redo brings it back.
  await page.getByRole("button", { name: /undo/ }).click();
  await expect.poll(async () => (await canvasJson(page)).nodes.length).toBe(1);
  await page.getByRole("button", { name: /redo/ }).click();
  await expect.poll(async () => (await canvasJson(page)).nodes.length).toBe(2);

  // Delete it through the toolbar's button and the confirmation.
  const child = page.locator(`.react-flow__node[data-id="${created.id}"]`);
  await child.locator(".mesh-node-title-text, .mesh-node-title-centered-text").first().click();
  await page.locator('.mesh-node-toolbar button[title="Delete this node"]').click();
  await page.locator(".node-delete-modal button", { hasText: /delete/i }).first().click();
  await expect.poll(async () => (await canvasJson(page)).nodes.length).toBe(1);
  expect(await raw()).not.toContain(`id="${created.id}"`);
});

test("the edits a page can make are the ones the server's endpoints make (api level)", async ({ page }) => {
  const file = path.join(tmpDir(), "doc.canvas.md");
  fs.writeFileSync(file, ROOT + '## A\n<!-- meshfox:node id="a" -->\n\nfirst\n\n## B\n<!-- meshfox:node id="b" -->\n\nsecond\n');
  await page.goto("/");
  await page.locator("input[type=file]:not([webkitdirectory])").setInputFiles(file);
  await expect(page.getByText("The root.")).toBeVisible();
  const call = (method: string, url: string, body?: unknown) =>
    page.evaluate(
      async ([m, u, b]) => {
        const r = await fetch(u as string, {
          method: m as string,
          headers: { "content-type": "application/json" },
          body: b === undefined ? undefined : typeof b === "string" ? b : JSON.stringify(b),
        });
        return { status: r.status, text: await r.text() };
      },
      [method, url, body] as const,
    );

  const created = await call("POST", "api/nodes", { parentId: "a", title: "Deep", titleSlugId: true, body: "inside" });
  expect(created.status).toBe(200);
  expect(JSON.parse(created.text).newId).toBe("deep");
  expect((await call("POST", "api/nodes/b/move", { before: "a" })).status).toBe(200);
  expect((await call("POST", "api/nodes/a/move", { before: "a" })).status).toBe(422);
  expect((await call("POST", "api/nodes/a/rename-id", { newId: "alpha" })).status).toBe(200);
  expect((await call("POST", "api/nodes/alpha/rename-id", { newId: "b" })).status).toBe(422);
  expect((await call("POST", "api/nodes/ghost/rename-id", { newId: "x" })).status).toBe(404);
  const cleared = await call("POST", "api/nodes/alpha/clear-id");
  expect(JSON.parse(cleared.text).id).toBe("a");
  expect((await call("PUT", "api/options", { options: ["unfold"] })).status).toBe(200);
  expect((await call("PUT", "api/canvas", { nodes: [{ id: "a", x: 10, y: 20, width: 300 }], layoutHints: {} })).status).toBe(204);
  expect((await call("POST", "api/nodes/a/clear-layout")).status).toBe(200);
  // A block's attributes (the CLI/MCP's `node block`): the same endpoint, in the page.
  await call("POST", "api/nodes/b/append", "\n```bash name=\"build\"\necho hi\n```\n");
  expect((await call("PATCH", "api/nodes/b/block/build", { cache: true, code: "echo bye" })).status).toBe(200);
  expect((await call("PATCH", "api/nodes/b/block/none", { cache: true })).status).toBe(404);
  expect((await call("PATCH", "api/nodes/b/block/build", { interpreter: "python", clearInterpreter: true })).status).toBe(422);
  const afterBlock = await page.evaluate(() => fetch("api/canvas/raw").then((r) => r.text()));
  expect(afterBlock).toContain("echo bye");
  expect(afterBlock).not.toContain("echo hi");
  expect((await call("DELETE", "api/nodes/root")).status).toBe(422);
  expect((await call("DELETE", "api/nodes/a")).status).toBe(200);
  const nodes = (await canvasJson(page)).nodes.map((n) => n.id);
  expect(nodes).toEqual(["root", "b"]);
});

test("the pill covers none of React Flow's controls or minimap", async ({ page }) => {
  const file = path.join(tmpDir(), "doc.canvas.md");
  // Two nodes: a single node is shown as a page, with no React Flow around it.
  fs.writeFileSync(file, ROOT + '## Child\n<!-- meshfox:node id="child" -->\n\nchild body\n');
  await page.goto("/");
  await page.locator("input[type=file]:not([webkitdirectory])").setInputFiles(file);
  await expect(page.getByText("The root.")).toBeVisible();
  await expect(page.locator(".react-flow__controls")).toBeVisible();
  const box = async (selector: string) => (await page.locator(selector).first().boundingBox())!;
  const pill = await box("#wasm-pill");
  for (const selector of [".react-flow__controls", ".react-flow__minimap"]) {
    const other = await box(selector);
    const apart =
      pill.x + pill.width <= other.x || other.x + other.width <= pill.x || pill.y + pill.height <= other.y || other.y + other.height <= pill.y;
    expect(apart, `the pill must not overlap ${selector}`).toBe(true);
  }
});

// ---- drafts: unsaved work survives a closed tab ----

const drafts = (page: Page) =>
  page.evaluate(
    () =>
      new Promise<Array<{ key: string; raw: string }>>((resolve) => {
        const open = indexedDB.open("meshfox-editor", 1);
        open.onupgradeneeded = () => open.result.createObjectStore("drafts", { keyPath: "key" });
        open.onsuccess = () => {
          const all = open.result.transaction("drafts").objectStore("drafts").getAll();
          all.onsuccess = () => resolve(all.result);
        };
      }),
  );

async function openAndEdit(page: Page, file: string) {
  await page.goto("/");
  await page.locator("input[type=file]:not([webkitdirectory])").setInputFiles(file);
  await expect(page.getByText("The root.")).toBeVisible();
  await editSource(page, ADDED);
  await expect(page.getByText("added in the browser")).toBeVisible();
  await expect.poll(async () => (await drafts(page)).length).toBe(1);
}

test("unsaved edits come back after the tab was closed: from the launcher", async ({ page }) => {
  const file = path.join(tmpDir(), "doc.canvas.md");
  fs.writeFileSync(file, ROOT);
  await openAndEdit(page, file);
  expect((await drafts(page))[0].raw).toBe(ROOT + ADDED);

  page.on("dialog", (d) => void d.accept()); // "leave without downloading?"
  await page.reload();
  await expect(page.locator("#launcher")).toBeVisible();
  await expect(page.locator("#drafts")).toContainText("doc.canvas.md");
  await page.locator("#drafts button", { hasText: "Restore" }).click();
  await expect(page.getByText("added in the browser")).toBeVisible();
  // It was never saved anywhere: still unsaved, and still kept.
  await expect(page.locator("#wasm-pill")).toContainText("unsaved changes");
  expect((await drafts(page))[0].raw).toBe(ROOT + ADDED);
});

test("opening the same file again offers the draft, and keeps it until the user decides", async ({ page }) => {
  const file = path.join(tmpDir(), "doc.canvas.md");
  fs.writeFileSync(file, ROOT);
  await openAndEdit(page, file);
  page.on("dialog", (d) => void d.accept());
  await page.reload();
  await page.locator("input[type=file]:not([webkitdirectory])").setInputFiles(file);
  await expect(page.getByText("The root.")).toBeVisible();
  await expect(page.locator("#wasm-pill")).toContainText("unsaved edits from");
  await expect(page.getByText("added in the browser")).toHaveCount(0);
  // Not touched, not deleted, though the document in the page is clean.
  await page.waitForTimeout(800);
  expect(await drafts(page)).toHaveLength(1);

  await page.locator("#wasm-restore").click();
  await expect(page.getByText("added in the browser")).toBeVisible();
  await expect(page.locator("#wasm-pill")).toContainText("unsaved changes");
  // Restoring is a step like any other: one undo goes back to the file as opened.
  await page.getByRole("button", { name: "Edit", exact: true }).click();
  await page.getByRole("button", { name: /undo/ }).click();
  await expect(page.getByText("added in the browser")).toHaveCount(0);
});

test("discarding the draft, and saving, both delete it", async ({ page }) => {
  const file = path.join(tmpDir(), "doc.canvas.md");
  fs.writeFileSync(file, ROOT);
  await openAndEdit(page, file);
  page.on("dialog", (d) => void d.accept());
  await page.reload();
  await page.locator("input[type=file]:not([webkitdirectory])").setInputFiles(file);
  await page.locator("#wasm-discard").click();
  await expect.poll(async () => (await drafts(page)).length).toBe(0);

  // A fresh edit makes a draft again; downloading it (the work is safe) removes it.
  await editSource(page, ADDED);
  await expect.poll(async () => (await drafts(page)).length).toBe(1);
  const downloading = page.waitForEvent("download");
  await page.locator("#wasm-download").click();
  await downloading;
  await expect.poll(async () => (await drafts(page)).length).toBe(0);
});

test("a draft removes itself when the edit is undone back to the saved text", async ({ page }) => {
  const file = path.join(tmpDir(), "doc.canvas.md");
  fs.writeFileSync(file, ROOT);
  await openAndEdit(page, file);
  await page.getByRole("button", { name: /undo/ }).click();
  await expect(page.getByText("added in the browser")).toHaveCount(0);
  await expect.poll(async () => (await drafts(page)).length).toBe(0);
});

test("a new canvas's work is kept too", async ({ page }) => {
  await page.goto("/");
  await page.locator("#new-canvas").click();
  await expect(page.locator("#wasm-pill")).toBeVisible();
  await editSource(page, ADDED);
  await expect.poll(async () => (await drafts(page)).map((d) => d.key)).toEqual(["new:untitled.canvas.md"]);
});

// ---- save into a file (File System Access API: Chromium) ----

test("save writes into the chosen file, and again without asking", async ({ page }) => {
  await page.addInitScript(() => {
    const w = window as unknown as Record<string, unknown>;
    w.__saves = { asked: 0, name: "", writes: [] as string[], closed: 0 };
    w.showSaveFilePicker = async (options: { suggestedName: string }) => {
      const saves = w.__saves as { asked: number; name: string; writes: string[]; closed: number };
      saves.asked++;
      saves.name = options.suggestedName;
      return {
        createWritable: async () => ({
          write: async (data: string) => void saves.writes.push(data),
          close: async () => void saves.closed++,
        }),
      };
    };
  });
  const file = path.join(tmpDir(), "doc.canvas.md");
  fs.writeFileSync(file, ROOT);
  await page.goto("/");
  await page.locator("input[type=file]:not([webkitdirectory])").setInputFiles(file);
  await expect(page.getByText("The root.")).toBeVisible();
  await editSource(page, ADDED);
  await expect(page.locator("#wasm-pill")).toContainText("unsaved changes");

  const saves = () =>
    page.evaluate(() => (window as unknown as { __saves: { asked: number; name: string; writes: string[]; closed: number } }).__saves);
  await page.locator("#wasm-save").click();
  await expect(page.locator("#wasm-pill")).not.toContainText("unsaved");
  expect(await saves()).toEqual({ asked: 1, name: "doc.canvas.md", writes: [ROOT + ADDED], closed: 1 });

  // A second save goes to the same file without a picker.
  await editSource(page, "\nmore text\n");
  await expect(page.locator("#wasm-pill")).toContainText("unsaved changes");
  await page.locator("#wasm-save").click();
  await expect(page.locator("#wasm-pill")).not.toContainText("unsaved");
  const after = await saves();
  expect(after.asked).toBe(1);
  expect(after.writes).toHaveLength(2);
  expect(after.writes[1]).toContain("more text");
  expect(after.closed).toBe(2);
});

test("nodes that need a server say so plainly, and offer no button that cannot work", async ({ page }) => {
  const dir = tmpDir();
  fs.writeFileSync(path.join(dir, "other.canvas.md"), '# Other\n<!-- meshfox:node id="root" -->\n\nOther.\n');
  fs.writeFileSync(path.join(dir, "data.csv"), "a,b\n1,2\n");
  fs.writeFileSync(
    path.join(dir, "doc.canvas.md"),
    [
      '# Root\n<!-- meshfox:node id="root" -->\n<!-- meshfox:option name="unfold" -->\n\nThe root.\n',
      '## Sibling\n<!-- meshfox:node id="sibling" type="file" -->\n\n[sibling canvas](./other.canvas.md)\n',
      '## Data\n<!-- meshfox:node id="data" type="file" display="table" -->\n\n[data table](./data.csv)\n',
    ].join("\n"),
  );
  await page.goto("/");
  await page.locator("input[webkitdirectory]").setInputFiles(dir);
  await page.getByRole("button", { name: "doc.canvas.md" }).click();
  await expect(page.getByText("The root.")).toBeVisible();
  await expect(page.getByText("Sibling", { exact: true })).toBeVisible();
  // "Open in the default application" has no meaning in a page: only the node
  // that leads to another canvas keeps its button.
  await expect(page.getByTitle("Open this file in the default application")).toHaveCount(1);
  // A table says it is not supported here, not that something is broken.
  await expect(page.getByText(/not supported in the browser editor/)).toBeVisible();
});

test("a cold start shows the engine loading, and a file picked meanwhile opens once it is there", async ({ page }) => {
  let release!: () => void;
  const gate = new Promise<void>((ok) => (release = ok));
  await page.route("**/*.wasm", async (route) => {
    await gate;
    await route.continue();
  });
  const file = path.join(tmpDir(), "slow.canvas.md");
  fs.writeFileSync(file, ROOT);
  await page.goto("/");
  // Not blank: the launcher is there and says what it is waiting for.
  await expect(page.getByRole("heading", { name: "Open a canvas" })).toBeVisible();
  await expect(page.locator("#engine")).toBeVisible();
  await expect(page.locator("#engine")).toContainText("Loading the engine");

  await page.locator("input[type=file]:not([webkitdirectory])").setInputFiles(file);
  await expect(page.locator("#status")).toContainText("Waiting for the engine");
  release();

  await expect(page.getByText("The root.")).toBeVisible();
  await expect(page.locator("#booting")).toHaveCount(0);
});

test("when the engine cannot be fetched the launcher says so", async ({ page }) => {
  await page.route("**/*.wasm", (route) => route.abort());
  await page.goto("/");
  await expect(page.locator("#engine")).toContainText("could not be loaded");
});

test("a canvas node opens the other canvas of the folder; Back and Forward move between them", async ({ page }) => {
  const dir = tmpDir();
  fs.writeFileSync(
    path.join(dir, "a.canvas.md"),
    [
      '# Alpha\n<!-- meshfox:node id="root" -->\n<!-- meshfox:option name="unfold" -->\n\nFirst canvas.\n',
      '## To B\n<!-- meshfox:node id="tob" type="file" -->\n\n[b](./sub/b.canvas.md#deep)\n',
      '## Nowhere\n<!-- meshfox:node id="nowhere" type="file" -->\n\n[missing](./missing.canvas.md)\n',
    ].join("\n"),
  );
  fs.mkdirSync(path.join(dir, "sub"));
  fs.writeFileSync(
    path.join(dir, "sub", "b.canvas.md"),
    [
      '# Beta\n<!-- meshfox:node id="root" -->\n\nSecond canvas.\n',
      '## Deep\n<!-- meshfox:node id="deep" x="4000" y="4000" width="300" height="100" -->\n\nthe linked node\n',
    ].join("\n"),
  );
  await page.goto("/");
  await page.locator("input[webkitdirectory]").setInputFiles(dir);
  await page.getByRole("button", { name: "a.canvas.md" }).click();
  await expect(page.getByText("First canvas.")).toBeVisible();

  // A canvas that is not among the files says so and stays where it is.
  await page.getByTitle("Open this file in the default application").nth(1).click();
  await expect(page.getByText(/not among the opened files/)).toBeVisible();
  await expect(page.getByText("First canvas.")).toBeVisible();

  await page.getByTitle("Open this file in the default application").first().click();
  await expect(page.getByText("Second canvas.")).toBeVisible();
  await expect(page.getByText("First canvas.")).toHaveCount(0);
  // The link named a node (`#deep`): the view is on it.
  await expect(page.getByText("the linked node")).toBeInViewport();

  // Edit it, go Back to the first, Forward again: the edit is still there.
  await editSource(page, ADDED);
  await expect(page.getByText("added in the browser")).toBeVisible();
  await page.goBack();
  await expect(page.getByText("First canvas.")).toBeVisible();
  await expect(page.getByText("added in the browser")).toHaveCount(0);
  await page.goForward();
  await expect(page.getByText("added in the browser")).toBeVisible();

  // Leaving for the launcher asks, because the other canvas has edits.
  let asked = false;
  page.on("dialog", (d) => {
    asked = true;
    void d.dismiss();
  });
  await page.goBack();
  await page.goBack();
  await expect.poll(() => asked).toBe(true);
});

test("a link to an SVG from the folder shows its text; it never runs on the editor's origin", async ({ page, context }) => {
  const dir = tmpDir();
  fs.writeFileSync(
    path.join(dir, "doc.canvas.md"),
    '# Root\n<!-- meshfox:node id="root" -->\n\n[open the picture](./evil.svg)\n\n![shown](./evil.svg)\n',
  );
  fs.writeFileSync(
    path.join(dir, "evil.svg"),
    '<svg xmlns="http://www.w3.org/2000/svg" width="20" height="20"><script>document.title="RAN:"+location.origin</script><rect width="20" height="20"/></svg>',
  );
  await page.goto("/");
  await page.locator("input[type=file]:not([webkitdirectory])").setInputFiles([path.join(dir, "doc.canvas.md"), path.join(dir, "evil.svg")]);
  await expect(page.getByText("open the picture")).toBeVisible();
  // As a picture it is still drawn (an <img> runs no scripts) ...
  await expect
    .poll(() => page.evaluate(() => [...document.images].some((i) => i.alt === "shown" && i.naturalWidth > 0)))
    .toBe(true);
  // ... but followed as a link it is only text.
  const [popup] = await Promise.all([context.waitForEvent("page"), page.getByText("open the picture").click()]);
  await popup.waitForLoadState();
  await popup.waitForTimeout(500);
  expect(await popup.title()).not.toContain("RAN:");
  expect(await popup.evaluate(() => document.contentType)).toBe("text/plain");
});

test("the page runs under its Content-Security-Policy, and a picture opened on its own stays inert", async ({ page, context }) => {
  const violations: string[] = [];
  page.on("console", (m) => /content security policy|violates the/i.test(m.text()) && violations.push(m.text()));
  const response = await page.goto("/");
  expect(response?.headers()["content-security-policy"]).toContain("script-src 'self' 'wasm-unsafe-eval'");
  expect(response?.headers()["x-content-type-options"]).toBe("nosniff");

  const dir = tmpDir();
  fs.writeFileSync(path.join(dir, "doc.canvas.md"), '# Root\n<!-- meshfox:node id="root" -->\n\nThe root.\n\n![shown](./evil.svg)\n\n```js\nlet a = 1;\n```\n');
  fs.writeFileSync(
    path.join(dir, "evil.svg"),
    '<svg xmlns="http://www.w3.org/2000/svg" width="20" height="20"><script>document.title="RAN"</script><rect width="20" height="20"/></svg>',
  );
  await page.locator("input[type=file]:not([webkitdirectory])").setInputFiles([path.join(dir, "doc.canvas.md"), path.join(dir, "evil.svg")]);
  await expect(page.getByText("The root.")).toBeVisible();
  // Monaco (its worker) and the highlighter are the usual victims of a strict policy.
  await editSource(page, ADDED);
  await expect(page.getByText("added in the browser")).toBeVisible();

  // "Open image in new tab" navigates to the picture's own blob URL: a page of
  // this origin, where the policy must still stop its script.
  const src = await page.evaluate(() => [...document.images].find((i) => i.alt === "shown")!.src);
  expect(src.startsWith("blob:")).toBe(true);
  const popup = await context.newPage();
  await popup.goto(src).catch(() => {});
  await popup.waitForTimeout(500);
  expect(await popup.title()).not.toBe("RAN");

  expect(violations.filter((v) => !v.includes("evil") && !/inline script/i.test(v))).toEqual([]);
});

test("the first screen says what happens to a file, and the help explains it", async ({ page }) => {
  await page.goto("/");
  await expect(page.locator(".lede")).toContainText("never uploaded");
  await expect(page.getByText("Nothing is uploaded or tracked")).toBeVisible();
  const help = page.locator("#help");
  await expect(help.getByRole("heading", { name: "Privacy" })).toBeHidden();
  await help.locator("summary").click();
  await expect(help.getByRole("heading", { name: "Privacy" })).toBeVisible();
  await expect(help.getByText(/draft of the text is kept in this browser/)).toBeVisible();
  await expect(help.getByRole("heading", { name: "Not here" })).toBeVisible();
});

test("on a phone-sized screen the strip is a bar across the bottom and covers nothing", async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 780 });
  const file = path.join(tmpDir(), "doc.canvas.md");
  fs.writeFileSync(file, ROOT + '## Child\n<!-- meshfox:node id="child" -->\n\nchild body\n');
  await page.goto("/");
  await page.locator("input[type=file]:not([webkitdirectory])").setInputFiles(file);
  await expect(page.getByText("The root.")).toBeVisible();
  await expect(page.locator(".react-flow__controls")).toBeVisible();
  const box = async (selector: string) => (await page.locator(selector).first().boundingBox())!;
  const pill = await box("#wasm-pill");
  // Across the width, not squeezed into a sliver ...
  expect(pill.width).toBeGreaterThan(300);
  expect(pill.height).toBeLessThan(140);
  // ... with the text on one readable block, and nothing of React Flow under it.
  for (const selector of [".react-flow__controls", ".react-flow__minimap"]) {
    if ((await page.locator(selector).count()) === 0) continue;
    const other = await box(selector);
    expect(other.y + other.height, `${selector} must end above the strip`).toBeLessThanOrEqual(pill.y + 1);
  }
});

test("where the browser has locked img.src (iOS content blockers) the launcher says so, plainly", async ({ page }) => {
  await page.addInitScript(() => {
    const d = Object.getOwnPropertyDescriptor(HTMLImageElement.prototype, "src")!;
    Object.defineProperty(HTMLImageElement.prototype, "src", { ...d, configurable: false });
  });
  const file = path.join(tmpDir(), "any.canvas.md");
  fs.writeFileSync(file, ROOT);
  await page.goto("/");
  await page.locator("input[type=file]:not([webkitdirectory])").setInputFiles(file);
  await expect(page.locator("#status.error")).toContainText("does not let the editor take over image loading");
  // Still the launcher: nothing was half-started.
  await expect(page.getByRole("heading", { name: "Open a canvas" })).toBeVisible();
});

test.describe("touch", () => {
  test.use({ hasTouch: true, isMobile: true, viewport: { width: 390, height: 780 } });

  test("a finger dragging from on a node pans the canvas, like one dragging from the gap", async ({ page, browserName }) => {
    test.skip(browserName !== "chromium", "synthetic touch drags go through the DevTools protocol");
    const file = path.join(tmpDir(), "doc.canvas.md");
    fs.writeFileSync(file, ROOT + '## Child\n<!-- meshfox:node id="child" -->\n\nchild body\n');
    await page.goto("/");
    await page.locator("input[type=file]:not([webkitdirectory])").setInputFiles(file);
    await expect(page.getByText("The root.")).toBeVisible();
    // The node's text must not opt out of panning (it does on a mouse).
    await expect(page.locator(".mesh-node-body.nopan, .mesh-node-title-text.nopan")).toHaveCount(0);

    const viewport = () => page.locator(".react-flow__viewport").evaluate((el) => (el as HTMLElement).style.transform);
    const text = page.getByText("The root.");
    const at = (await text.boundingBox())!;
    const x = at.x + at.width / 2;
    const y = at.y + at.height / 2;
    const before = await viewport();
    const cdp = await page.context().newCDPSession(page);
    await cdp.send("Input.dispatchTouchEvent", { type: "touchStart", touchPoints: [{ x, y }] });
    for (let i = 1; i <= 8; i++) {
      await cdp.send("Input.dispatchTouchEvent", { type: "touchMove", touchPoints: [{ x: x + i * 8, y: y + i * 8 }] });
    }
    await cdp.send("Input.dispatchTouchEvent", { type: "touchEnd", touchPoints: [] });
    await expect.poll(viewport).not.toBe(before);
  });
});

test("with a mouse, a drag on a node's text still selects instead of panning", async ({ page }) => {
  const file = path.join(tmpDir(), "doc.canvas.md");
  fs.writeFileSync(file, ROOT + '## Child\n<!-- meshfox:node id="child" -->\n\nchild body\n');
  await page.goto("/");
  await page.locator("input[type=file]:not([webkitdirectory])").setInputFiles(file);
  await expect(page.getByText("The root.")).toBeVisible();
  expect(await page.locator(".mesh-node-body.nopan").count()).toBeGreaterThan(0);
});

test("an arrow's label keeps its shape wherever the view is, and moves with the canvas for free", async ({ page }) => {
  const file = path.join(tmpDir(), "labels.canvas.md");
  fs.writeFileSync(
    file,
    [
      '# Root\n<!-- meshfox:node id="root" -->\n<!-- meshfox:option name="unfold" -->\n\nThe root.\n',
      '## Left\n<!-- meshfox:node id="left" -->\n\nleft body\n',
      '## Right\n<!-- meshfox:node id="right" -->\n<!-- meshfox:edge from="left" label="a cheap alternative for everyone" -->\n\nright body\n',
    ].join("\n"),
  );
  await page.setViewportSize({ width: 420, height: 700 });
  await page.goto("/");
  await page.locator("input[type=file]:not([webkitdirectory])").setInputFiles(file);
  await expect(page.getByText("The root.")).toBeVisible();
  const badge = page.locator(".mesh-edge-label-badge");
  await expect(badge).toHaveCount(1);

  const shape = async () => {
    const r = (await badge.boundingBox())!;
    return { r, text: await badge.evaluate((el) => (el as HTMLElement).innerText) };
  };
  // Not squeezed into a column of single letters, at any scroll position.
  for (const dx of [0, -120, 90]) {
    const pane = page.locator(".react-flow__pane");
    const box = (await pane.boundingBox())!;
    // The top corner: the strip across the bottom would take the press.
    await page.mouse.move(box.x + 30, box.y + 12);
    await page.mouse.down();
    await page.mouse.move(box.x + 30 + dx, box.y + 12, { steps: 4 });
    await page.mouse.up();
    const { r } = await shape();
    expect(r.width, "wide enough to read").toBeGreaterThan(r.height * 1.5);
  }

  // It travels with the canvas: panning moves it by exactly what moves a node.
  const node = page.locator('.react-flow__node[data-id="right"]');
  const before = { b: (await badge.boundingBox())!, n: (await node.boundingBox())! };
  const pane = (await page.locator(".react-flow__pane").boundingBox())!;
  await page.mouse.move(pane.x + 60, pane.y + 12);
  await page.mouse.down();
  await page.mouse.move(pane.x + 60 - 40, pane.y + 12 + 25, { steps: 4 });
  await page.mouse.up();
  const after = { b: (await badge.boundingBox())!, n: (await node.boundingBox())! };
  expect(Math.round(after.b.x - before.b.x)).toBe(Math.round(after.n.x - before.n.x));
  expect(Math.round(after.b.y - before.b.y)).toBe(Math.round(after.n.y - before.n.y));
});

test("the strip stays under the node text editor: its buttons can be pressed", async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 780 });
  const file = path.join(tmpDir(), "doc.canvas.md");
  fs.writeFileSync(file, ROOT + '## Child\n<!-- meshfox:node id="child" -->\n\nchild body\n');
  await page.goto("/");
  await page.locator("input[type=file]:not([webkitdirectory])").setInputFiles(file);
  await expect(page.getByText("The root.")).toBeVisible();
  await page.getByRole("button", { name: "Edit", exact: true }).click();
  await page.locator('.react-flow__node[data-id="root"] .mesh-node-title-text').first().click();
  await page.getByTitle("Edit this node's Markdown text").click();
  await expect(page.locator(".mesh-text-editor-source .monaco-editor")).toBeVisible();
  // The strip is below the editor's backdrop, whatever the screen's height ...
  const zs = await page.evaluate(() => ({
    strip: Number(getComputedStyle(document.getElementById("wasm-pill")!).zIndex),
    editor: Number(getComputedStyle(document.querySelector(".mesh-text-editor-backdrop")!).zIndex),
  }));
  expect(zs.strip).toBeLessThan(zs.editor);
  // ... so nothing it holds is under it. Whatever is at the middle of each button must be that button (a disabled
  // one included: Playwright's own click check refuses those).
  for (const name of ["Cancel", "Apply", "Save & close"]) {
    const covered = await page
      .getByRole("button", { name, exact: true })
      .evaluate((button) => {
        const r = button.getBoundingClientRect();
        const top = document.elementFromPoint(r.x + r.width / 2, r.y + r.height / 2);
        return top !== null && !button.contains(top);
      });
    expect(covered, `"${name}" must not be covered`).toBe(false);
  }
});

test("the toolbar scrolls sideways on its own when it does not fit; the page does not get wider", async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 780 });
  const file = path.join(tmpDir(), "doc.canvas.md");
  fs.writeFileSync(file, ROOT + '## Child\n<!-- meshfox:node id="child" -->\n\nchild body\n');
  await page.goto("/");
  await page.locator("input[type=file]:not([webkitdirectory])").setInputFiles(file);
  await expect(page.getByText("The root.")).toBeVisible();
  await page.getByRole("button", { name: "Edit", exact: true }).click();

  const bar = page.locator(".toolbar");
  const m = await bar.evaluate((el) => ({ scroll: el.scrollWidth, client: el.clientWidth }));
  expect(m.scroll, "edit mode has more buttons than 390px holds").toBeGreaterThan(m.client);
  // The page itself stays the width of the screen.
  expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(390);
  // Labels stay on one line.
  const heights = await bar.locator("button").evaluateAll((bs) => bs.map((b) => b.getBoundingClientRect().height));
  expect(Math.max(...heights) - Math.min(...heights)).toBeLessThan(12);
  // The last button is reachable by scrolling the bar.
  await bar.evaluate((el) => (el.scrollLeft = el.scrollWidth));
  const last = (await bar.locator("button").last().boundingBox())!;
  expect(last.x + last.width).toBeLessThanOrEqual(391);
  expect(last.x).toBeGreaterThanOrEqual(0);
});

test.describe("iOS", () => {
  test.use({
    userAgent:
      "Mozilla/5.0 (iPhone; CPU iPhone OS 17_5 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.5 Mobile/15E148 Safari/604.1",
    viewport: { width: 390, height: 780 },
    hasTouch: true, // Monaco takes a Mac/iPhone user agent *with* touch points for iOS
  });

  test("Monaco's on-screen-keyboard button (a dead textarea over the text) is not shown", async ({ page, browserName }) => {
    // Only Chromium reports touch points under emulation, which Monaco needs to believe it is on iOS.
    test.skip(browserName !== "chromium", "touch points are not emulated");
    const file = path.join(tmpDir(), "doc.canvas.md");
    fs.writeFileSync(file, ROOT + '## Child\n<!-- meshfox:node id="child" -->\n\nchild body\n');
    await page.goto("/");
    await page.locator("input[type=file]:not([webkitdirectory])").setInputFiles(file);
    await expect(page.getByText("The root.")).toBeVisible();
    await page.getByRole("button", { name: "Edit", exact: true }).click();
    await page.locator('.react-flow__node[data-id="root"] .mesh-node-title-text').first().click();
    await page.getByTitle("Edit this node's Markdown text").click();
    await expect(page.locator(".mesh-text-editor-source .monaco-editor")).toBeVisible();
    // It is created on iOS only: first make sure this really is that case.
    await expect.poll(() => page.evaluate(() => navigator.maxTouchPoints)).toBeGreaterThan(0);
    const shown = await page.evaluate(() =>
      [...document.querySelectorAll<HTMLElement>(".iPadShowKeyboard")].filter((el) => getComputedStyle(el).display !== "none").length,
    );
    expect(shown).toBe(0);
  });
});
