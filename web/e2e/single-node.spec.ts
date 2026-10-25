import { test, expect } from "@playwright/test";

test("single root is a live document; editing and real descendants restore the canvas", async ({ page, request }) => {
  const errors: string[] = [];
  page.on("pageerror", error => errors.push(error.message));
  expect((await request.patch("/api/nodes/root", { data: { x: 230, y: 190, width: 400, height: 260 } })).ok()).toBeTruthy();
  const before = await (await request.get("/api/canvas")).json();
  await page.goto("/");
  const document = page.locator(".mesh-document");
  await expect(document.getByRole("heading", { level: 1 })).toHaveText("A single-node document");
  await expect(page.locator(".react-flow")).toHaveCount(0);
  await document.getByRole("button", { name: "run hello", exact: true }).click();
  await expect(document.locator(".mesh-code-output")).toContainText("Hello from a single-node document!");

  await page.getByRole("button", { name: "search", exact: false }).click();
  const search = page.getByPlaceholder("Search title/body… (Enter: next, Shift+Enter: prev, Esc: close)");
  await search.fill("must actually contain");
  await expect.poll(() => document.evaluate(el => el.scrollTop)).toBeGreaterThan(0);
  await search.press("Escape");

  await page.getByRole("button", { name: "Edit", exact: true }).click();
  await expect(page.locator('.react-flow__node[data-id="root"]')).toBeVisible();
  await expect(document).toHaveCount(0);
  await page.getByRole("button", { name: "done", exact: true }).click();
  await expect(document).toBeVisible();
  const after = await (await request.get("/api/canvas")).json();
  for (const key of ["x", "y", "width", "height"]) expect(after.nodes[0][key]).toEqual(before.nodes[0][key]);

  const added = await request.post("/api/nodes", { data: { parentId: "root", title: "Single root regression child" } });
  expect(added.ok()).toBeTruthy();
  const child = (await added.json()).nodes.find((node: { parent?: string }) => node.parent === "root");
  try {
    await expect(page.locator(".react-flow")).toBeVisible();
    await expect(document).toHaveCount(0);
    await page.locator('.react-flow__node[data-id="root"] button[title="Fold this subtree"]').click();
    await expect(page.locator(".react-flow__node")).toHaveCount(1);
    await expect(document).toHaveCount(0);
    await page.getByRole("button", { name: "search", exact: false }).click();
    await search.fill("Single root regression child");
    await search.press("Enter");
    await expect(page.locator(`.react-flow__node[data-id="${child.id}"]`)).toBeVisible();
    await search.press("Escape");
  } finally {
    expect((await request.delete(`/api/nodes/${child.id}`)).ok()).toBeTruthy();
  }
  await expect(document).toBeVisible();
  await expect(document.getByRole("heading", { name: "Back to a canvas" })).toBeAttached();
  await page.setViewportSize({ width: 390, height: 760 });
  await expect.poll(() => document.evaluate(el => el.scrollWidth <= el.clientWidth)).toBe(true);
  expect(errors).toEqual([]);
});
