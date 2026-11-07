import { defineConfig, devices } from "@playwright/test";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { MESHFOX_VIEW } from "./e2e/meshfox-bin";

// Focused activity and terminal regressions against an isolated real worker.
const dir = fs.mkdtempSync(path.join(os.tmpdir(), "meshfox-activity-e2e-"));
fs.copyFileSync(path.join(import.meta.dirname, "e2e/fixtures/quick-run.canvas.md"), path.join(dir, "activity.canvas.md"));
fs.copyFileSync(path.join(import.meta.dirname, "e2e/fixtures/file-block.sh"), path.join(dir, "file-block.sh"));
fs.mkdirSync(path.join(dir, "home"));
process.env.MESHFOX_SERVER_SOCKET = "";
process.on("exit", () => fs.rmSync(dir, { recursive: true, force: true }));
const port = 4637;
export default defineConfig({
  testDir: "./e2e",
  testMatch: ["activity-bar.spec.ts", "quick-run.spec.ts"],
  workers: 1,
  use: { ...devices["Desktop Chrome"], baseURL: `http://127.0.0.1:${port}`, viewport: { width: 1440, height: 1000 } },
  webServer: {
    command: `${MESHFOX_VIEW} ${dir}/activity.canvas.md --port ${port} --no-open --no-auto-exit`,
    env: { HOME: path.join(dir, "home"), CARGO_HOME: process.env.CARGO_HOME ?? path.join(os.homedir(), ".cargo"), RUSTUP_HOME: process.env.RUSTUP_HOME ?? path.join(os.homedir(), ".rustup") },
    url: `http://127.0.0.1:${port}/api/canvas`,
    reuseExistingServer: false,
    timeout: 120_000,
  },
});
