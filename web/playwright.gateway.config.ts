import { defineConfig, devices } from "@playwright/test";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { MESHFOX_BIN } from "./e2e/meshfox-bin";
import { OPEN_PORT, TOKEN, TOKEN_PORT } from "./e2e-gateway/constants";

// `meshfox serve --http` end to end, in a real browser: the page and
// everything it loads (assets, Monaco and Shiki chunks, the watch socket,
// runs) have to work under `/c/<slug>/`, through the gateway's token
// handling, with no request escaping the prefix. Kept apart from the main
// config (`playwright.config.ts`): two servers instead of ~40, and the
// suite is about the gateway, not about the canvas UI.

// Never read the developer's own ~/.meshfox config or talk to their daemon.
process.env.MESHFOX_SERVER_SOCKET = "";

// One served root per gateway: two coordinators must not both run workers
// for the same canvas file.
const roots = ["open", "token"].map((name) => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), `mfx-e2e-gw-${name}-root-`));
  fs.cpSync(path.join(import.meta.dirname, "e2e-gateway/fixtures"), root, { recursive: true });
  return root;
});
const [OPEN_ROOT, TOKEN_ROOT] = roots;
const HOME = fs.mkdtempSync(path.join(os.tmpdir(), "mfx-e2e-gw-home-"));
// A unix socket path has a tight length budget, so not under the temp dirs
// above. Fixed names: Playwright stops the servers with SIGKILL, which leaves
// the socket file behind, and a restarted `serve` replaces a stale one.
const socket = (name: string) => `/tmp/mfx-e2e-gw-${name}.sock`;
process.on("exit", () => {
  for (const dir of [...roots, HOME]) fs.rmSync(dir, { recursive: true, force: true });
  // This file is loaded by every Playwright worker process too, and the
  // socket names are shared: only the main process may remove them, or a
  // worker finishing its project would pull the socket out from under the
  // other project's still-running server.
  if (process.env.TEST_WORKER_INDEX === undefined) {
    for (const name of ["open", "token"]) fs.rmSync(socket(name), { force: true });
  }
});


const serve = (port: number, name: string, root: string) =>
  `${MESHFOX_BIN} serve --socket ${socket(name)} --http 127.0.0.1:${port} --root ${root}`;

export default defineConfig({
  testDir: "./e2e-gateway",
  timeout: 45_000,
  fullyParallel: false,
  forbidOnly: !!process.env.CI,
  retries: process.env.CI ? 1 : 0,
  workers: 1,
  reporter: [["list"]],
  use: { trace: "retain-on-failure", screenshot: "only-on-failure", viewport: { width: 1280, height: 1000 } },
  projects: [
    {
      name: "open",
      testMatch: /(^|\/)gateway\.spec\.ts$/,
      use: { ...devices["Desktop Chrome"], baseURL: `http://127.0.0.1:${OPEN_PORT}` },
    },
    {
      name: "token",
      testMatch: /(^|\/)gateway-token\.spec\.ts$/,
      use: { ...devices["Desktop Chrome"], baseURL: `http://127.0.0.1:${TOKEN_PORT}` },
    },
  ],
  webServer: [
    {
      command: serve(OPEN_PORT, "open", OPEN_ROOT),
      url: `http://127.0.0.1:${OPEN_PORT}/_/ping`,
      env: { HOME, MESHFOX_SERVER_SOCKET: "" },
      reuseExistingServer: false,
      timeout: 60_000,
      stdout: "pipe",
      stderr: "pipe",
    },
    {
      command: serve(TOKEN_PORT, "token", TOKEN_ROOT),
      url: `http://127.0.0.1:${TOKEN_PORT}/_/ping`,
      env: { HOME, MESHFOX_SERVER_SOCKET: "", MESHFOX_TOKEN: TOKEN },
      reuseExistingServer: false,
      timeout: 60_000,
      stdout: "pipe",
      stderr: "pipe",
    },
  ],
});
