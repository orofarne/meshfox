import { defineConfig, devices } from "@playwright/test";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { MESHFOX_BIN } from "./e2e/meshfox-bin";

process.env.MESHFOX_SERVER_SOCKET = "";
if (process.platform === "darwin" && !process.env.CFFIXED_USER_HOME) {
  process.env.CFFIXED_USER_HOME = fs.mkdtempSync(path.join(os.tmpdir(), "mfx-large-firefox-"));
}
const scratch = fs.mkdtempSync(path.join(os.tmpdir(), "mfx-large-"));
const root = path.join(scratch, "root");
fs.mkdirSync(root);
fs.copyFileSync(path.join(import.meta.dirname, "e2e-large-canvas/fixtures/synthetic.canvas.md"), path.join(root, "synthetic.canvas.md"));
const direct = path.join(scratch, "direct");
fs.mkdirSync(direct);
fs.copyFileSync(path.join(root, "synthetic.canvas.md"), path.join(direct, "synthetic.canvas.md"));
process.on("exit", () => fs.rmSync(scratch, { recursive: true, force: true }));

export default defineConfig({
  testDir: "./e2e-large-canvas",
  timeout: 90_000,
  workers: 1,
  retries: 0,
  reporter: [["list"], ["json", { outputFile: "test-results/large-canvas-summary.json" }]],
  use: {
    baseURL: "http://127.0.0.1:4768",
    viewport: { width: 960, height: 720 },
    trace: "retain-on-failure",
    screenshot: "only-on-failure",
  },
  projects: [
    ...["firefox", "chromium"].flatMap(browser => ["gateway", "direct"].map(mode => ({
      name: `${browser}-${mode}`,
      use: {
        ...devices[browser === "firefox" ? "Desktop Firefox" : "Desktop Chrome"],
        viewport: { width: 960, height: 720 },
        baseURL: `http://127.0.0.1:${mode === "gateway" ? 4768 : 4769}`,
      },
    }))),
  ],
  webServer: [
    {
      command: `"${MESHFOX_BIN}" --no-exec serve --socket ${scratch}/s.sock --http 127.0.0.1:4768 --root ${root}`,
      url: "http://127.0.0.1:4768/_/ping",
      env: { HOME: scratch, MESHFOX_SERVER_SOCKET: "" },
      reuseExistingServer: false,
      timeout: 60_000,
    },
    {
      command: `"${MESHFOX_BIN}" --no-exec view ${direct}/synthetic.canvas.md --port 4769 --no-open --no-auto-exit`,
      url: "http://127.0.0.1:4769/api/canvas",
      env: { HOME: scratch, MESHFOX_SERVER_SOCKET: "" },
      reuseExistingServer: false,
      timeout: 60_000,
    },
  ],
});
