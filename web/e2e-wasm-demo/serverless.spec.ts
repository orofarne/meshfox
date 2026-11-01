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
  return fs.mkdtempSync(path.join(os.tmpdir(), "mfx-wasm-demo-"));
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
  await expect(page.getByText("read-only", { exact: true })).toBeVisible();
  await expect(page.getByText("noexec", { exact: true })).toBeVisible();
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
