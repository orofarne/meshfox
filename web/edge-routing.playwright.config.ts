import { defineConfig, devices } from "@playwright/test";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

// Focused routing run without starting every unrelated suite's worker.
const dir = fs.mkdtempSync(path.join(os.tmpdir(), "meshfox-routing-e2e-"));
fs.cpSync(path.join(import.meta.dirname, "e2e/fixtures"), dir, { recursive: true });
fs.mkdirSync(path.join(dir, "home"));
process.env.MESHFOX_SERVER_SOCKET = "";
process.on("exit", () => fs.rmSync(dir, { recursive: true, force: true }));
const suites = ["edge-routing", "group-edge-routing"];
export default defineConfig({
  testDir: "./e2e", workers: 1, timeout: 45_000,
  use: { viewport: { width: 1280, height: 1600 }, launchOptions: { timeout: 15_000 },
    screenshot: "only-on-failure", trace: "retain-on-failure" },
  projects: suites.flatMap((name, i) => [
    { browser: "chrome", device: devices["Desktop Chrome"] },
    { browser: "firefox", device: devices["Desktop Firefox"] },
  ].map(({ browser, device }) => ({ name: `${browser}-${name}`, testMatch: `${name}.spec.ts`,
    use: { ...device, viewport: { width: 1280, height: 1600 },
      baseURL: `http://127.0.0.1:${4645 + i}` } }))),
  webServer: suites.map((name, i) => ({
    command: `cargo run -q --manifest-path ../Cargo.toml -p meshfox-cli -- view ${dir}/${name}.canvas.md --port ${4645 + i} --no-open --no-auto-exit`,
    env: { HOME: path.join(dir, "home"), CARGO_HOME: process.env.CARGO_HOME ?? path.join(os.homedir(), ".cargo"),
      RUSTUP_HOME: process.env.RUSTUP_HOME ?? path.join(os.homedir(), ".rustup"), PATH: process.env.PATH ?? "" },
    url: `http://127.0.0.1:${4645 + i}/api/canvas`, reuseExistingServer: false, timeout: 180_000,
  })),
});
