import { defineConfig, devices } from "@playwright/test";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

// Same TEMPORARY workaround as `playwright.config.ts` for Playwright's bundled
// Firefox on macOS 27+ ("Could not find profile folder"): point CoreFoundation's
// home at a writable per-run dir. See microsoft/playwright#42768.
if (process.platform === "darwin" && !process.env.CFFIXED_USER_HOME) {
  process.env.CFFIXED_USER_HOME = fs.mkdtempSync(path.join(os.tmpdir(), "meshfox-e2e-firefox-home-"));
}

// E2E for the serverless prototype (`wasm-demo/`): a static build served by
// `vite preview`, no `meshfox` process anywhere. `npm run test:wasm-demo`
// builds it first.
const PORT = 4600;

export default defineConfig({
  testDir: "./e2e-wasm-demo",
  timeout: 45_000,
  fullyParallel: true,
  reporter: "list",
  webServer: {
    command: "npm run preview:wasm",
    url: `http://localhost:${PORT}/`,
    reuseExistingServer: !process.env.CI,
    timeout: 60_000,
  },
  use: { baseURL: `http://localhost:${PORT}` },
  projects: [
    { name: "chrome", use: { ...devices["Desktop Chrome"] } },
    { name: "firefox", use: { ...devices["Desktop Firefox"] } },
  ],
});
