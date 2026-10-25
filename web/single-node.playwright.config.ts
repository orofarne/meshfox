import { defineConfig, devices } from "@playwright/test";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { MESHFOX_VIEW } from "./e2e/meshfox-bin";

const dir = fs.mkdtempSync(path.join(os.tmpdir(), "meshfox-single-node-e2e-"));
fs.copyFileSync(path.join(import.meta.dirname, "../examples/single-node.canvas.md"), path.join(dir, "document.canvas.md"));
process.env.MESHFOX_SERVER_SOCKET = "";
process.env.PATH = `${path.join(import.meta.dirname, "../target/debug")}${path.delimiter}${process.env.PATH}`;
process.on("exit", () => fs.rmSync(dir, { recursive: true, force: true }));
const port = 4692;
export default defineConfig({
  testDir: "./e2e", testMatch: "single-node.spec.ts", workers: 1,
  use: { ...devices["Desktop Chrome"], baseURL: `http://127.0.0.1:${port}`, viewport: { width: 1280, height: 800 } },
  webServer: {
    command: `${MESHFOX_VIEW} ${dir}/document.canvas.md --port ${port} --no-open --no-auto-exit`,
    url: `http://127.0.0.1:${port}/api/canvas`, reuseExistingServer: false, timeout: 180_000,
  },
});
