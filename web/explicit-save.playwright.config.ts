import { defineConfig, devices } from "@playwright/test";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

// Editor regressions over real workers; all fixtures live in a disposable
// directory so saved drafts never alter the repository or user config.
const dir = fs.mkdtempSync(path.join(os.tmpdir(), "meshfox-explicit-save-"));
fs.cpSync(path.join(import.meta.dirname, "e2e/fixtures"), dir, { recursive: true });
fs.mkdirSync(path.join(dir, "home"));
process.env.MESHFOX_SERVER_SOCKET = "";
process.on("exit", () => fs.rmSync(dir, { recursive: true, force: true }));
const suites = ["body-conflict", "settings", "undo-redo", "image-paste"];
export default defineConfig({
  testDir: "./e2e", workers: 1, timeout: 45_000,
  use: { ...devices["Desktop Chrome"], viewport: { width: 1440, height: 1000 } },
  projects: suites.map((name, i) => ({ name, testMatch: `${name}.spec.ts`, use: { baseURL: `http://127.0.0.1:${4637 + i}` } })),
  webServer: suites.map((name, i) => ({
    command: `cargo run -q --manifest-path ../Cargo.toml -p meshfox-cli -- view ${dir}/${name}.canvas.md --port ${4637 + i} --no-open --no-auto-exit`,
    env: { HOME: path.join(dir, "home"), CARGO_HOME: process.env.CARGO_HOME ?? path.join(os.homedir(), ".cargo"), RUSTUP_HOME: process.env.RUSTUP_HOME ?? path.join(os.homedir(), ".rustup"), PATH: process.env.PATH ?? "" },
    url: `http://127.0.0.1:${4637 + i}/api/canvas`, reuseExistingServer: false, timeout: 180_000,
  })),
});
