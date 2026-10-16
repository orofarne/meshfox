import { defineConfig, devices } from "@playwright/test";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

const owner = !process.env.MESHFOX_SHARED_FILES_FIXTURE;
const dir = process.env.MESHFOX_SHARED_FILES_FIXTURE ?? fs.mkdtempSync(path.join(os.tmpdir(), "meshfox-shared-files-"));
process.env.MESHFOX_SHARED_FILES_FIXTURE = dir;
process.env.MESHFOX_SERVER_SOCKET = "";
if (owner) {
  for (const sub of [".meshfox", "docs", "child", "home"]) fs.mkdirSync(path.join(dir, sub));
  fs.writeFileSync(path.join(dir, ".meshfox/config.toml"), '[files]\nallow = ["docs"]\n');
  fs.writeFileSync(path.join(dir, "docs/sensor.svg"), '<svg xmlns="http://www.w3.org/2000/svg" width="32" height="24"><rect width="32" height="24" fill="red"/></svg>');
  fs.writeFileSync(path.join(dir, "docs/datasheet.txt"), "shared datasheet");
  fs.writeFileSync(path.join(dir, "docs/readme.md"), "![Included](sensor.svg)");
  fs.writeFileSync(path.join(dir, "child/README.canvas.md"), '<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id="root" -->\n\n![SCD41](../docs/sensor.svg)\n\n[datasheet](../docs/datasheet.txt)\n\n## Included\n<!-- meshfox:node id="inc" type="include" -->\n\n[docs](../docs/readme.md)\n');
  process.on("exit", () => fs.rmSync(dir, { recursive: true, force: true }));
}
export default defineConfig({
  testDir: "./e2e", testMatch: "shared-files.spec.ts", workers: 1, timeout: 30_000,
  use: { ...devices["Desktop Chrome"], baseURL: "http://127.0.0.1:4648", viewport: { width: 1440, height: 1000 } },
  webServer: {
    command: `cargo run -q --manifest-path ../Cargo.toml -p meshfox-cli -- view ${dir}/child/README.canvas.md --port 4648 --no-open --no-auto-exit`,
    env: { HOME: path.join(dir, "home"), CARGO_HOME: process.env.CARGO_HOME ?? path.join(os.homedir(), ".cargo"), RUSTUP_HOME: process.env.RUSTUP_HOME ?? path.join(os.homedir(), ".rustup"), PATH: process.env.PATH ?? "" },
    url: "http://127.0.0.1:4648/api/canvas", reuseExistingServer: false, timeout: 180_000,
  },
});
