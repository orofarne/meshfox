import { test, expect, type Page, type WebSocketRoute } from "@playwright/test";
import { clickFitViewAndWait } from "./helpers";

const bar = (page: Page) => page.getByLabel("Current activity", { exact: true });
const node = (page: Page, id: string) => page.locator(`.react-flow__node[data-id="${id}"]`);
async function launch(page: Page) {
  await page.goto("/");
  await page.waitForSelector(".mesh-node");
  await clickFitViewAndWait(page);
}
async function controlledRun(page: Page) {
  let socket: WebSocketRoute | undefined;
  await page.routeWebSocket(/\/api\/run\?/, ws => { socket = ws; });
  await launch(page);
  await node(page, "plain").locator(".mesh-node-quick-run-icon").click();
  await expect.poll(() => !!socket).toBe(true);
  return socket!;
}
const send = (socket: WebSocketRoute, event: object) => socket.send(JSON.stringify(event));

test("activity follows the chain phases while the console stays closed", async ({ page }, testInfo) => {
  const socket = await controlledRun(page);
  const status = bar(page);
  await expect(page.getByRole("button", { name: "Console", exact: true })).toHaveCount(1);
  await expect(status).toContainText("Preparing…");
  await expect(page.locator(".console-panel")).toHaveAttribute("aria-hidden", "true");
  send(socket, { type: "started", runId: "activity-test" });
  send(socket, { type: "step-start", nodeId: "dep-source", block: "setup" });
  await expect(status).toContainText("Running:");
  await expect(status).toContainText("dep-source/setup");
  await expect(status.locator(".activity-time")).toHaveText(/00:0[1-9]/);
  await status.getByRole("button", { name: "Show dep-source/setup" }).click();
  await expect(node(page, "dep-source").locator(".mesh-node")).toHaveAttribute("data-focused", "true");
  await page.screenshot({ path: testInfo.outputPath("running.png") });
  send(socket, { type: "step-end", nodeId: "dep-source", block: "setup", exitCode: 0, durationMs: 1234 });
  await expect(status).toContainText("Between steps…");
  await expect(status.locator(".activity-time")).toContainText("total");
  await expect(status).not.toContainText("dep-source/setup");
  send(socket, { type: "step-start", nodeId: "plain", block: "plain" });
  await expect(status).toContainText("Running: plain/plain");
  send(socket, { type: "step-end", nodeId: "plain", block: "plain", exitCode: 0, durationMs: 1 });
  await expect(status).toContainText("Between steps…");
  send(socket, { type: "done", exitCode: 0 });
  socket.close();
  await expect(status).toContainText("Completed: plain/plain");
  await expect(status.getByRole("button", { name: "■ Stop", exact: true })).toHaveCount(0);
  const finalTime = await status.locator(".activity-time").textContent();
  await page.clock.install();
  await page.clock.fastForward(3000);
  await expect(status.locator(".activity-time")).toHaveText(finalTime!);
  await status.getByRole("button", { name: "Console", exact: true }).click();
  await expect(page.locator(".console-panel")).toHaveAttribute("aria-hidden", "false");
  await page.clock.runFor(300);
  const stripTop = await status.evaluate(el => el.getBoundingClientRect().top);
  await expect.poll(() => page.locator(".console-panel").evaluate(el => el.getBoundingClientRect().bottom)).toBeLessThanOrEqual(stripTop);
  expect(await status.evaluate(el => el.getBoundingClientRect().bottom)).toBeLessThanOrEqual(page.viewportSize()!.height);
  await status.getByRole("button", { name: "Console", exact: true }).click();
  await expect(page.locator(".console-panel")).toHaveAttribute("aria-hidden", "true");
  await expect(status.getByRole("button", { name: "Console", exact: true })).toHaveAttribute("aria-expanded", "false");
});

test("Stop targets the chain and killed is not overwritten by Done", async ({ page }) => {
  const socket = await controlledRun(page);
  send(socket, { type: "started", runId: "kill-this-chain" });
  send(socket, { type: "step-start", nodeId: "plain", block: "plain" });
  let requestBody: unknown;
  await page.route("**/api/kill", async route => {
    requestBody = route.request().postDataJSON();
    await route.fulfill({ status: 200, body: "{}" });
  });
  await bar(page).getByRole("button", { name: "■ Stop", exact: true }).click();
  await expect.poll(() => requestBody).toEqual({ runId: "kill-this-chain" });
  send(socket, { type: "killed", nodeId: "plain", block: "plain" });
  send(socket, { type: "done", exitCode: 0 });
  socket.close();
  await expect(bar(page)).toContainText("Killed: plain/plain");
});

for (const outcome of ["failed", "error", "disconnect"] as const) {
  test(`activity preserves ${outcome} outcome`, async ({ page }) => {
    const socket = await controlledRun(page);
    send(socket, { type: "started", runId: outcome });
    send(socket, { type: "step-start", nodeId: "plain", block: "plain" });
    await expect(bar(page)).toContainText("Running:");
    if (outcome === "failed") send(socket, { type: "done", exitCode: 7 });
    if (outcome === "error") send(socket, { type: "error", message: "Preparation failed" });
    socket.close();
    await expect(bar(page)).toContainText(outcome === "disconnect" ? "Result unknown:" : "Failed:");
    if (outcome === "failed") await expect(bar(page)).toContainText("exit 7");
  });
}

test("concurrent chains have separate status rows and stop controls", async ({ page }) => {
  const sockets: WebSocketRoute[] = [];
  await page.routeWebSocket(/\/api\/run\?/, ws => { sockets.push(ws); });
  await launch(page);
  for (const id of ["plain", "chained"]) {
    await node(page, id).locator(".mesh-node-quick-run-icon").click();
  }
  await expect.poll(() => sockets.length).toBe(2);
  sockets.forEach((socket, i) => {
    send(socket, { type: "started", runId: `concurrent-${i}` });
    send(socket, { type: "step-start", nodeId: i === 0 ? "plain" : "chained", block: i === 0 ? "plain" : "chained" });
  });
  await bar(page).getByRole("button", { name: /2 running/ }).click();
  await expect(bar(page).getByLabel("Running chains").locator(".activity-row")).toHaveCount(2);
  send(sockets[0], { type: "done", exitCode: 0 });
  sockets[0].close();
  await expect(bar(page)).toContainText("Running: chained/chained");
  send(sockets[1], { type: "done", exitCode: 0 });
  sockets[1].close();
  await expect(bar(page)).toContainText("Completed: chained/chained");
});

test("activity strip fits a narrow viewport", async ({ page }, testInfo) => {
  await page.emulateMedia({ colorScheme: "dark" });
  await page.setViewportSize({ width: 360, height: 760 });
  const socket = await controlledRun(page);
  send(socket, { type: "started", runId: "narrow" });
  send(socket, { type: "step-start", nodeId: "plain", block: "plain" });
  await expect(bar(page)).toContainText("Running:");
  await expect(bar(page).getByRole("button", { name: "■ Stop", exact: true })).toBeInViewport();
  expect(await bar(page).evaluate(el => el.scrollWidth <= el.clientWidth)).toBe(true);
  await page.screenshot({ path: testInfo.outputPath("narrow-dark.png") });
  send(socket, { type: "done", exitCode: 0 });
  socket.close();
});

test("a real terminal run can be stopped from the strip after minimizing", async ({ page }) => {
  await launch(page);
  await node(page, "monitor").locator(".mesh-node-quick-run-icon").click();
  const panel = page.locator(".mesh-tty-panel");
  await expect(panel.locator(".mesh-tty-head-status")).toHaveText("interactive");
  await expect(bar(page)).toContainText("Interactive: monitor/monitor");
  await panel.getByTitle("Minimize — session keeps running", { exact: true }).click();
  await bar(page).getByRole("button", { name: "■ Stop", exact: true }).click();
  await expect(bar(page)).toContainText("Killed: monitor/monitor");
  await expect(bar(page).getByRole("button", { name: "■ Stop", exact: true })).toHaveCount(0);
});

test("an auto-opened chain console closes during a subsequent live terminal", async ({ page }) => {
  await launch(page);
  const consolePanel = page.locator(".console-panel");
  await expect(consolePanel).toHaveAttribute("aria-hidden", "true");
  await node(page, "chained").locator(".mesh-node-quick-run-icon").click();
  await expect(consolePanel).toHaveAttribute("aria-hidden", "false");
  await expect(bar(page)).toContainText("Completed: chained/chained");
  // Both nodes really executed; no mocked run stream or cached dependency.
  await expect(consolePanel).toContainText("dep-ran");
  await expect(consolePanel).toContainText("chained-ran");
  await node(page, "monitor").locator(".mesh-node-quick-run-icon").click();
  const terminal = page.locator(".mesh-tty-panel");
  try {
    await expect(terminal.locator(".mesh-tty-head-status")).toHaveText("interactive");
    await expect(consolePanel).toHaveAttribute("aria-hidden", "false");
    // Keep the real PTY alive past the console's ten-second grace period.
    await expect(consolePanel).toHaveAttribute("aria-hidden", "true", { timeout: 13_000 });
    await expect(terminal.locator(".mesh-tty-head-status")).toHaveText("interactive");
    await terminal.getByTitle("Minimize — session keeps running", { exact: true }).click();
    // An explicit user open, unlike the automatic one, must stay pinned.
    await bar(page).getByRole("button", { name: "Console", exact: true }).click();
    await page.clock.install();
    await page.clock.runFor(11_000);
    await expect(consolePanel).toHaveAttribute("aria-hidden", "false");
  } finally {
    if (await terminal.isVisible()) {
      await terminal.getByTitle("Minimize — session keeps running", { exact: true }).click();
    }
    await bar(page).getByRole("button", { name: "■ Stop", exact: true }).click();
    await expect(bar(page)).toContainText("Killed: monitor/monitor");
  }
});

test("checks, terminals and services live only in the bottom strip", async ({ page }, testInfo) => {
  await page.route("**/api/canvas", async route => {
    const response = await route.fetch();
    const canvas = await response.json();
    canvas.nodes[0].constraintResults = [
      { label: "passing", ok: true, messages: [] },
      { label: "failing", ok: false, messages: ["check failed"] },
    ];
    await route.fulfill({ response, json: canvas });
  });
  await page.route("**/api/services", route => route.fulfill({ json: [
    { nodeId: "plain", block: "server", status: "running", pid: 123, uptimeMs: 1000 },
    { nodeId: "plain", block: "broken", status: "crashed", exitCode: 1, pid: 124, uptimeMs: 1000 },
  ] }));
  await page.route("**/api/services/log?*", route => route.fulfill({ json: [] }));
  await page.route("**/api/runs", route => route.fulfill({ json: [
    { nodeId: "monitor", block: "monitor", kind: "tty", status: "running", uptimeMs: 1000 },
  ] }));
  await launch(page);
  const status = bar(page);
  await expect(status.getByLabel("Checks: 1/2 passed")).toBeVisible();
  await expect(status.getByRole("button", { name: "TTY: 1 live sessions", exact: true })).toBeVisible();
  await expect(status.getByRole("button", { name: "Services: 1/2 running, 1 failed", exact: true })).toBeVisible();
  await expect(page.locator(".toolbar .constraint-stats, .toolbar .service-stats")).toHaveCount(0);
  await status.getByRole("button", { name: /^Services:/ }).click();
  await expect(page.getByRole("heading", { name: "Services", exact: true })).toBeVisible();
  await page.locator(".service-panel-header button").click();
  await status.getByRole("button", { name: /^TTY:/ }).click();
  await expect(page.getByRole("heading", { name: "Live terminals", exact: true })).toBeVisible();
  await expect(page.locator(".tty-sessions-list")).toContainText("monitor");
  await page.locator(".service-panel-header button").click();
  await page.setViewportSize({ width: 360, height: 760 });
  await expect(status.getByRole("button", { name: "Console", exact: true })).toBeInViewport();
  expect(await status.evaluate(el => el.scrollWidth <= el.clientWidth)).toBe(true);
  await page.screenshot({ path: testInfo.outputPath("all-controls-narrow.png") });
});
