import { test, expect } from "@playwright/test";

// Drives web/e2e/fixtures/markdown-extensions.canvas.md — the web-side
// render path for three of meshfox's own narrow Markdown extensions (see
// TODO.canvas.md's "Формальные граматики для meshfox:*" subtree):
// `{width=..}`/`{height=..}` after an image (remarkImageAttrs.ts),
// `~sub~`/`^sup^` (remarkSubSup.ts), and GFM alert blockquotes
// (remarkGfmAlerts.ts). Read-only — this suite never edits the canvas, so
// there's no "Edit" click here unlike most other specs in this directory.

test.beforeEach(async ({ page }) => {
  await page.goto("/");
  await page.waitForSelector(".mesh-node");
});

test("image size attributes become real width/height on the rendered <img>", async ({ page }) => {
  const root = page.locator('.react-flow__node[data-id="root"]');
  const img = root.locator(".mesh-node-body img");
  await expect(img).toHaveAttribute("width", "300");
  await expect(img).toHaveAttribute("height", "50%");
});

test("subscript and superscript render as real <sub>/<sup> elements", async ({ page }) => {
  const root = page.locator('.react-flow__node[data-id="root"]');
  const body = root.locator(".mesh-node-body");
  await expect(body.locator("sub")).toHaveText("2");
  await expect(body.locator("sup")).toHaveText("n");
  // The whole word, not just the marked part, has to actually be there —
  // guards against the delimiters simply vanishing instead of becoming
  // real elements.
  await expect(body).toContainText("H2O and xn");
});

test("a GFM alert blockquote gets its type class and strips the marker line", async ({ page }) => {
  const root = page.locator('.react-flow__node[data-id="root"]');
  const alert = root.locator(".mesh-node-body blockquote.markdown-alert-warning");
  await expect(alert).toBeVisible();
  await expect(alert).toContainText("be careful here");
  await expect(alert).not.toContainText("[!WARNING]");
});

test("an ordinary blockquote is left alone", async ({ page }) => {
  const root = page.locator('.react-flow__node[data-id="root"]');
  const body = root.locator(".mesh-node-body");
  const plain = body.locator("blockquote:not([class])");
  await expect(plain).toContainText("just a quote");
});

// A 4-space-indented ```fence``` is a CommonMark *indented* code block,
// not a real fence — the same escaping trick SPEC.md's own docs rely on
// to show fence syntax inertly. `fence.ts`'s `parseBody` used to detect a
// fence opener via `line.trimStart().startsWith("```")`, which strips any
// amount of leading whitespace and so mistook this for a real runnable
// fence — turning a plain documentation example into a live "run" button
// (visible in the actual SPEC.md render). `fenceIndentOk` fixes this by
// rejecting a would-be opener indented 4+ spaces, mirroring
// `core::fence::fence_open`'s own check.
test("a 4-space-indented fence example is not treated as a runnable block", async ({ page }) => {
  const root = page.locator('.react-flow__node[data-id="root"]');
  const body = root.locator(".mesh-node-body");
  await expect(body.locator(".mesh-code-block-head", { hasText: "not-really-runnable" })).toHaveCount(0);
  await expect(body.getByRole("button", { name: /run/i })).toHaveCount(0);
  // Still visible as inert text/code, just not wrapped in the runnable-
  // fence UI (name/lang header + Run button).
  await expect(body).toContainText("not-really-runnable");
  await expect(body).toContainText("echo hi");
});
