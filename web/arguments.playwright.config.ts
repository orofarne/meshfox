import { defineConfig, devices } from "@playwright/test";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

// A single real worker for argument launch checks; edits only touch this copy.
const dir = fs.mkdtempSync(path.join(os.tmpdir(), "meshfox-arguments-e2e-"));
fs.copyFileSync(path.join(import.meta.dirname, "e2e/fixtures/vars-form.canvas.md"), path.join(dir, "arguments.canvas.md"));
fs.mkdirSync(path.join(dir, "home"));
process.env.MESHFOX_SERVER_SOCKET = "";
process.on("exit", () => fs.rmSync(dir, { recursive: true, force: true }));
const port = 4635;
export default defineConfig({
  testDir: "./e2e",
  testMatch: "arguments.spec.ts",
  workers: 1,
  use: { ...devices["Desktop Chrome"], baseURL: `http://127.0.0.1:${port}`, viewport: { width: 1440, height: 1000 } },
  webServer: {
    command: `cargo run -q --manifest-path ../Cargo.toml -p meshfox-cli -- view ${dir}/arguments.canvas.md --port ${port} --no-open --no-auto-exit`,
    env: { HOME: path.join(dir, "home"), CARGO_HOME: process.env.CARGO_HOME ?? path.join(os.homedir(), ".cargo"), RUSTUP_HOME: process.env.RUSTUP_HOME ?? path.join(os.homedir(), ".rustup") },
    url: `http://127.0.0.1:${port}/api/canvas`,
    reuseExistingServer: false,
    timeout: 120_000,
  },
});
