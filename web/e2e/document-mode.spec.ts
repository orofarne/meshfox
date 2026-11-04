import { test, expect } from "@playwright/test";

test("Document button shows the whole canvas as a flat page with edges as links", async ({ page, request }) => {
  const errors: string[] = [];
  page.on("pageerror", error => errors.push(error.message));
  const before = await (await request.get("/api/canvas")).json();
  await page.goto("/");
  await expect(page.locator(".react-flow")).toBeVisible();

  await page.getByRole("button", { name: "Document", exact: true }).click();
  const doc = page.locator(".mesh-canvas-document");
  await expect(doc).toBeVisible();
  await expect(page.locator(".react-flow")).toHaveCount(0);

  // Sections follow tree order, not coordinates (Deploy sits far from Setup).
  await expect(doc.locator(".mesh-doc-section")).toHaveCount(5);
  await expect(doc.getByRole("heading", { level: 1, name: "Document mode fixture" })).toBeVisible();
  await expect(doc.getByRole("heading", { level: 2, name: "Setup" })).toBeVisible();
  await expect(doc.getByRole("heading", { level: 3, name: "Inside a group" })).toBeVisible();
  await expect(doc.locator("#doc-setup")).toContainText("Install the tools first.");
  // A group is just its heading.
  await expect(doc.locator("#doc-groups")).not.toContainText("Child of a group");

  // The edge is listed on both ends, with its label.
  await expect(doc.locator("#doc-setup .mesh-doc-links")).toContainText("Leads to");
  await expect(doc.locator("#doc-setup").getByRole("link", { name: "needs: Deploy" })).toBeVisible();
  await expect(doc.locator("#doc-deploy").getByRole("link", { name: "needs: Setup" })).toBeVisible();
  // Plain structural edges are the heading hierarchy, not links.
  await expect(doc.locator("#doc-root .mesh-doc-links")).toHaveCount(0);

  // Following a link moves focus to the target heading.
  await doc.locator("#doc-setup").getByRole("link", { name: "needs: Deploy" }).click();
  await expect(page.locator("#doc-h-deploy")).toBeFocused();

  // Reading never writes: layout is untouched.
  const after = await (await request.get("/api/canvas")).json();
  expect(after.nodes.map((n: { x?: number; y?: number }) => [n.x, n.y])).toEqual(before.nodes.map((n: { x?: number; y?: number }) => [n.x, n.y]));

  // Edit returns to the graph; the toggle goes back too.
  await page.getByRole("button", { name: "Edit", exact: true }).click();
  await expect(page.locator(".react-flow")).toBeVisible();
  await expect(doc).toHaveCount(0);
  await page.getByRole("button", { name: "done", exact: true }).click();
  await expect(doc).toBeVisible();
  await page.getByRole("button", { name: "Canvas", exact: true }).click();
  await expect(page.locator(".react-flow")).toBeVisible();
  expect(errors).toEqual([]);
});
