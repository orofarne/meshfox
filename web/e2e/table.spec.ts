import { test, expect, type Page } from "@playwright/test";
import { clickFitViewAndWait } from "./helpers";

// Drives `display="table"` (web/src/TablePreview.tsx) against a real worker
// and the real `duckdb` CLI over a generated 400,000-row CSV (see
// table.playwright.config.ts) — enough rows that the scroll track is capped
// and the scaled-scroll mapping is what reaches the bottom. Run with
// `npm run test:table`; skipped when no duckdb is installed.

async function tableMeta(page: Page, id: string) {
  const res = await page.request.get(`/api/nodes/${id}/table`);
  return { status: res.status(), body: await res.json() };
}

test("a huge CSV is windowed, sortable, filterable and searchable", async ({ page }, testInfo) => {
  const probe = await tableMeta(page, "orders");
  test.skip(probe.body.error?.kind === "duckdb-missing", "no duckdb CLI on this machine");

  await page.goto("/");
  await page.waitForSelector(".mesh-node");
  await clickFitViewAndWait(page);

  const node = page.locator('.react-flow__node[data-id="orders"]');
  const table = node.locator(".mesh-table");
  const status = table.locator(".mesh-table-status");
  const rows = table.locator(".mesh-table-row");
  const scroller = table.locator(".mesh-table-scroller");
  const cell = (row: ReturnType<typeof rows.nth>, col: number) => row.locator(".mesh-table-cell").nth(col);
  const NAME = 2;
  const AMOUNT = 3;
  const NOTE = 4;

  // The first rows are readable right away; the import finishes behind them.
  await expect(cell(rows.first(), NAME)).toHaveText("name-000001", { timeout: 60_000 });
  await expect(status).toHaveText("400,000 rows × 4 cols", { timeout: 60_000 });
  await expect(table.locator(".mesh-table-hname")).toHaveText(["id", "name", "amount", "note"]);
  await table.screenshot({ path: testInfo.outputPath("table-initial.png") });

  // --- sorting: click cycles asc → desc → none ---
  const amountHeader = table.locator(".mesh-table-hcell", { hasText: "amount" });
  await amountHeader.click();
  await expect(table.getByTestId("sort-indicator")).toHaveText("▲");
  await expect(cell(rows.first(), AMOUNT)).toHaveText("1");
  await amountHeader.click();
  await expect(table.getByTestId("sort-indicator")).toHaveText("▼");
  await expect(cell(rows.first(), AMOUNT)).toHaveText("400000");
  await amountHeader.click();
  await expect(table.getByTestId("sort-indicator")).toHaveCount(0);
  await expect(cell(rows.first(), NAME)).toHaveText("name-000001");

  // --- per-column filter ---
  await table.getByRole("button", { name: "filters" }).click();
  await table.getByLabel("filter id").fill(">=399991");
  await expect(status).toHaveText("10 of 400,000 rows · 4 cols");
  await expect(rows).toHaveCount(10);
  await expect(cell(rows.last(), NAME)).toHaveText("name-400000");

  // A value the column can't hold is reported, and the view recovers.
  await table.getByLabel("filter id").fill("abc");
  await expect(table.locator(".mesh-table-banner.is-error")).toBeVisible();
  await table.getByRole("button", { name: "reset" }).click();
  await expect(table.locator(".mesh-table-banner.is-error")).toHaveCount(0);
  await expect(status).toHaveText("400,000 rows × 4 cols");

  // --- search across all columns ---
  await table.getByPlaceholder("search all columns").fill("NAME-000042");
  await expect(status).toHaveText("1 of 400,000 rows · 4 cols");
  await expect(cell(rows.first(), NAME)).toHaveText("name-000042");
  await table.getByRole("button", { name: "reset" }).click();
  await expect(status).toHaveText("400,000 rows × 4 cols");

  // --- scrolling reaches every region, through the capped scroll track ---
  await scroller.evaluate((el) => {
    el.scrollTop = el.scrollHeight;
  });
  await expect(rows.last().locator(".mesh-table-gutter")).toHaveText("400,000");
  await expect(cell(rows.last(), NAME)).toHaveText("name-400000");

  await scroller.evaluate((el) => {
    el.scrollTop = (el.scrollHeight - el.clientHeight) / 2;
  });
  await expect
    .poll(async () => Number((await rows.first().locator(".mesh-table-gutter").innerText()).replace(/,/g, "")))
    .toBeGreaterThan(150_000);
  const middle = Number((await rows.first().locator(".mesh-table-gutter").innerText()).replace(/,/g, ""));
  expect(middle).toBeLessThan(250_000);
  await expect(cell(rows.first(), NAME)).toHaveText(`name-${String(middle).padStart(6, "0")}`);

  await scroller.evaluate((el) => {
    el.scrollTop = 0;
  });
  await expect(cell(rows.first(), NAME)).toHaveText("name-000001");

  // --- selecting a cell shows its full value; NULLs are shown as such ---
  const detail = table.getByTestId("cell-detail");
  await cell(rows.first(), NOTE).click();
  await expect(detail).toContainText("note 1");
  const rowFive = rows.filter({ has: page.locator(".mesh-table-gutter", { hasText: /^5$/ }) });
  await cell(rowFive, NOTE).click();
  await expect(detail).toContainText("NULL");

  // --- the expanded window shows the same table, with more of it ---
  const inNode = await rows.count();
  await node.locator(".mesh-node-expand-icon").click();
  const expanded = page.locator(".mesh-expand-panel .mesh-table");
  await expect(expanded.locator(".mesh-table-status")).toHaveText("400,000 rows × 4 cols");
  await expect.poll(() => expanded.locator(".mesh-table-row").count()).toBeGreaterThan(inNode);
  await page.locator(".mesh-expand-panel").screenshot({ path: testInfo.outputPath("table-expanded.png") });
});

test("a table whose file is missing says so in place", async ({ page }) => {
  await page.goto("/");
  await page.waitForSelector(".mesh-node");
  await clickFitViewAndWait(page);
  await expect(page.locator('.react-flow__node[data-id="ghost"]')).toContainText("table preview unavailable");
});
