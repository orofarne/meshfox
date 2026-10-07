import { test, expect } from "@playwright/test";
import { clickFitViewAndWait } from "./helpers";

test("manual arguments precede global variables and remain per-application", async ({ page }, testInfo) => {
  const canvas = await (await page.request.get("/api/canvas")).json();
  const node = canvas.nodes.find((node: { id: string }) => node.id === "dynamic");
  const root = canvas.nodes.find((node: { parent?: string | null }) => !node.parent);
  const rootResponse = await page.request.patch(`/api/nodes/${root.id}`, {data: {
    baseRev: root.bodyRev,
    text: `${root.text}\n<!-- meshfox:var name="PDF_URL_en" from="url-observer" -->
<!-- meshfox:var name="PDF_URL_hy" from="url-observer" -->
<!-- meshfox:var name="COUNT" type="int" from="url-observer" -->
\`\`\`bash name="url-observer" always
printf 'PDF_URL_en=en-url\\nPDF_URL_hy=hy-url\\nCOUNT=2\\n' > "$MESHFOX_VARS_OUT"
\`\`\`
\`\`\`button name="full-import" deps="url-observer"
🚀 Run full import
\`\`\`\n`,
  }});
  expect(rootResponse.ok()).toBeTruthy();
  const response = await page.request.patch("/api/nodes/dynamic", { data: {
    baseRev: node.bodyRev,
    text: `${node.text}\n<!-- meshfox:arg name="lang" type="select" choices="en,hy" default="en" -->
<!-- meshfox:arg name="n" type="int" default="09" required -->
\`\`\`bash name="extract" deps="${root.id}/url-observer" env="REGION,PDF_URL=PDF_URL_\${lang}" inputs="arguments.canvas.md" outputs="report_\${lang}.txt"
printf '%s:%s:%s\\n' "$lang" "$n" "$REGION" | tee "report_\${lang}.txt"
\`\`\`\n`,
  } });
  expect(response.ok()).toBeTruthy();
  await page.goto("/");
  await page.waitForSelector(".mesh-node");
  await clickFitViewAndWait(page);
  const producer = page.locator(".mesh-code-block").filter({has: page.getByRole("button", {name:"run url-observer",exact:true})});
  const exports = producer.locator('.mesh-block-detail-row').filter({has: page.locator('.mesh-block-detail-label').filter({hasText:/^exports$/})});
  await expect(exports.locator('code')).toHaveText(['PDF_URL_en', 'PDF_URL_hy', 'COUNT']);
  await expect(exports.locator('.mesh-block-argument-type')).toHaveText(['string', 'string', 'int']);
  await producer.getByTitle('Collapse this block (code + output)', {exact:true}).click();
  await expect(exports).toBeVisible();
  await producer.screenshot({path:testInfo.outputPath('export-details.png')});
  const launch = page.getByRole('button', {name: '🚀 Run full import', exact: true});
  const buttonDetails = page.getByRole('dialog', {name: 'Details for 🚀 Run full import'});
  await expect(buttonDetails).toBeHidden();
  await launch.hover();
  await expect(buttonDetails).toBeVisible();
  await expect(buttonDetails.locator('.mesh-dep-link')).toHaveText('url-observer');
  await buttonDetails.screenshot({path: testInfo.outputPath('button-details-tooltip.png')});
  await buttonDetails.locator('.mesh-dep-link').click();
  await expect(producer).toHaveClass(/mesh-code-block-flash/);
  await page.mouse.move(0, 0);
  await launch.evaluate(el => (el as HTMLButtonElement).blur());
  await expect(buttonDetails).toBeHidden();
  await launch.focus();
  await expect(buttonDetails).toBeVisible();
  await page.keyboard.press('Escape');
  await expect(buttonDetails).toBeHidden();
  await clickFitViewAndWait(page);

  const details = page.locator(".mesh-code-block").filter({ has: page.getByRole("button", { name: "run extract", exact: true }) }).locator(".mesh-block-details");
  const deps = details.locator('.mesh-block-detail-row').filter({has: page.locator('.mesh-block-detail-label').filter({hasText: /^deps$/})});
  const via = details.locator('.mesh-block-detail-row').filter({has: page.locator('.mesh-block-detail-label').filter({hasText: /^via var$/})});
  await expect(deps.locator('.mesh-dep-link')).toHaveText(`${root.id}/url-observer`);
  await expect(via).toContainText('REGIONS_LIST');
  await expect(via.locator('.mesh-dep-link')).toHaveText('list-regions');
  await expect(page.locator('.mesh-implicit-deps-toggle')).toHaveCount(0);
  await expect(details).toContainText('lang');
  await expect(details).toContainText('PDF_URL ← PDF_URL_${lang}');
  await expect(details).toContainText('[en | hy]');
  await expect(details).toContainText('required');
  await expect(details).toContainText('suggestion: "09"');
  await expect(details.locator('.mesh-block-detail-row').filter({hasText: 'inputs'}).locator('code')).toHaveText('arguments.canvas.md');
  await expect(details.locator('.mesh-block-detail-row').filter({hasText: 'outputs'}).locator('code')).toHaveText('report_${lang}.txt');
  const codeBlock = details.locator('..');
  await codeBlock.getByTitle('Collapse this block (code + output)', {exact: true}).click();
  await expect(details).toBeVisible();
  await expect(deps).toBeVisible();
  await expect(via).toBeVisible();
  await deps.locator('.mesh-dep-link').click();
  await expect(producer).toHaveClass(/mesh-code-block-flash/);
  await clickFitViewAndWait(page);
  await codeBlock.getByTitle('Show this block (code + output)', {exact: true}).click();
  await codeBlock.screenshot({path: testInfo.outputPath('block-details.png')});
  const run = page.getByRole("button", { name: "run extract", exact: true });
  await run.click();
  const modal = page.locator(".vars-modal");
  await expect(modal).toContainText("Configure arguments");
  await expect(modal.locator('input[type="number"]')).toHaveValue("09");
  await modal.locator("select").selectOption("hy");
  await modal.getByRole("button", { name: "run", exact: true }).click();
  await expect(modal).toContainText("Configure variables");
  await modal.locator("select").selectOption("eu-west-1");
  await modal.getByRole("button", { name: "run", exact: true }).click();
  await expect(modal).toHaveCount(0);
  const historyUrl = `/api/run/history?${new URLSearchParams({nodeId: "dynamic", block: "extract[lang=hy,n=9]"})}`;
  await expect.poll(async () => (await (await page.request.get(historyUrl)).json()).length).toBe(1);
  const history = await (await page.request.get(historyUrl)).json();
  expect(history[0].exitCode).toBe(0);
  await expect(details).toContainText('extract[lang=hy,n=9]: PDF_URL_hy');
  await expect(details).toContainText(`${root.id}/url-observer`);
  const combinedHistory = await (await page.request.get('/api/run/history?nodeId=dynamic&block=extract')).json();
  expect(combinedHistory).toHaveLength(1);
  expect(combinedHistory[0].block).toBe('extract[lang=hy,n=9]');
  await codeBlock.getByRole('button', {name: 'Run history', exact: true}).click();
  const historyDialog = page.getByRole('dialog', {name: 'Run history: extract', exact: true});
  await expect(historyDialog.getByRole('option')).toContainText('extract[lang=hy,n=9]');
  await expect(historyDialog.locator('.run-history-detail pre')).toContainText('hy:9:eu-west-1');
  await historyDialog.screenshot({path: testInfo.outputPath('parameterized-history.png')});
  await historyDialog.getByRole('button', {name: 'Close', exact: true}).click();
  await page.reload();
  await page.waitForSelector(".mesh-node");
  await clickFitViewAndWait(page);
  await run.click();
  await expect(modal).toContainText("Configure arguments");
  await expect(modal.locator("select")).toHaveValue("en");
  await expect(modal.locator('input[type="number"]')).toHaveValue("09");
  await modal.getByRole("button", { name: "cancel", exact: true }).click();
});


test("parameterized runs show live status, isolated output and kill at the source block", async ({ page }) => {
  test.setTimeout(45_000);
  const canvas = await (await page.request.get('/api/canvas')).json();
  const node = canvas.nodes.find((node: {id: string}) => node.id === 'dynamic');
  const response = await page.request.patch('/api/nodes/dynamic', {data: {
    baseRev: node.bodyRev,
    text: `${node.text}
<!-- meshfox:arg name="lang" type="select" choices="en,hy" required -->
\`\`\`bash name="watch" fold
printf 'start-%s\\n' "$lang"
sleep 5
printf 'finish-%s\\n' "$lang"
\`\`\`
\`\`\`button name="launch-hy" deps="watch[lang=hy]"
Launch hy chain
\`\`\`
`,
  }});
  expect(response.ok()).toBeTruthy();
  await page.goto('/');
  await page.waitForSelector('.mesh-node');
  await clickFitViewAndWait(page);
  const block = page.locator('[id="mesh-block-dynamic::watch"]');
  await block.getByRole('button', {name: 'run watch', exact: true}).click();
  const modal = page.locator('.vars-modal');
  await expect(modal).toContainText('Configure arguments');
  await modal.locator('select').selectOption('en');
  await modal.getByRole('button', {name: 'run', exact: true}).click();
  const application = block.getByRole('combobox', {name: 'Application of watch'});
  await expect(application).toHaveValue('watch[lang=en]');
  await expect(block.getByRole('button', {name: 'running watch…', exact: true})).toBeDisabled();
  await expect(block.locator('[data-exit="running"]')).toBeVisible();
  await expect(block.locator('.mesh-code-output')).toContainText('start-en');
  await expect(block.locator('.mesh-code-output')).toContainText('finish-en', {timeout: 10_000});
  await expect(block.locator('[data-exit="ok"]')).toBeVisible();
  await expect(block.getByRole('button', {name: 'run watch', exact: true})).toBeEnabled();
  await page.getByRole('button', {name: 'Launch hy chain', exact: true}).click();
  await expect(application).toHaveValue('watch[lang=hy]');
  await expect(block.locator('.mesh-code-output')).toContainText('start-hy');
  await expect(block.locator('.mesh-code-output')).not.toContainText('start-en');
  await page.reload();
  await page.waitForSelector('.mesh-node');
  await clickFitViewAndWait(page);
  await expect(application).toHaveValue('watch[lang=hy]');
  await expect(block.locator('[data-exit="running"]')).toBeVisible();
  await expect(block.locator('.mesh-code-output')).toContainText('start-hy');
  await expect(block.locator('[data-exit="running"]')).toBeVisible();
  await block.locator('.mesh-kill-button').click();
  await expect(block.locator('[data-exit="killed"]')).toBeVisible();
  await expect(block.locator('.mesh-code-output')).not.toContainText('finish-hy');
  await application.selectOption('watch[lang=en]');
  await expect(block.locator('.mesh-code-output')).toContainText('finish-en', {timeout: 10_000});
  await expect(block.locator('.mesh-code-output')).not.toContainText('start-hy');
  await expect(block.locator('[data-exit="ok"]')).toBeVisible();
  await page.reload();
  await page.waitForSelector('.mesh-node');
  await clickFitViewAndWait(page);
  await expect(application.locator('option')).toHaveCount(2);
  await expect(application).toHaveValue('watch[lang=hy]');
  await expect(block.locator('[data-exit="killed"]')).toBeVisible();
  await application.selectOption('watch[lang=en]');
  await expect(block.locator('.mesh-code-output')).toContainText('finish-en', {timeout: 10_000});
  await application.selectOption('watch[lang=hy]');
  await expect(block.locator('[data-exit="killed"]')).toBeVisible();
  await expect(block.locator('.mesh-code-output')).toContainText('start-hy');
  await expect(block.locator('.mesh-code-output')).not.toContainText('finish-en', {timeout: 10_000});
});
