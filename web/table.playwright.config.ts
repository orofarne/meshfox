import { defineConfig, devices } from "@playwright/test";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

// One real worker over a throwaway directory holding a table canvas plus a
// generated 400,000-row CSV — more rows than a browser can give a scroll
// track (see MAX_TRACK_PX in src/tableView.ts), so the scaled-scroll path is
// exercised too. Requires the `duckdb` CLI on PATH (the spec skips otherwise).
const dir = fs.mkdtempSync(path.join(os.tmpdir(), "meshfox-table-e2e-"));
fs.copyFileSync(path.join(import.meta.dirname, "e2e/fixtures/table.canvas.md"), path.join(dir, "table.canvas.md"));
fs.mkdirSync(path.join(dir, "home"));
const ROWS = 400_000;
const lines = ["id,name,amount,note"];
for (let i = 1; i <= ROWS; i++) {
  // amount is a permutation of 1..ROWS, so sorting it has one right answer.
  const amount = ((i * 7919) % ROWS) + 1;
  lines.push(`${i},name-${String(i).padStart(6, "0")},${amount},${i % 5 === 0 ? "" : `note ${i}`}`);
}
fs.writeFileSync(path.join(dir, "orders.csv"), lines.join("\n") + "\n");
process.env.MESHFOX_SERVER_SOCKET = "";
process.on("exit", () => fs.rmSync(dir, { recursive: true, force: true }));
const port = 4636;
export default defineConfig({
  testDir: "./e2e",
  testMatch: "table.spec.ts",
  workers: 1,
  timeout: 90_000,
  use: { ...devices["Desktop Chrome"], baseURL: `http://127.0.0.1:${port}`, viewport: { width: 1440, height: 1000 } },
  webServer: {
    command: `cargo run -q --manifest-path ../Cargo.toml -p meshfox-cli -- view ${dir}/table.canvas.md --port ${port} --no-open --no-auto-exit`,
    env: {
      HOME: path.join(dir, "home"),
      CARGO_HOME: process.env.CARGO_HOME ?? path.join(os.homedir(), ".cargo"),
      RUSTUP_HOME: process.env.RUSTUP_HOME ?? path.join(os.homedir(), ".rustup"),
      PATH: process.env.PATH ?? "",
    },
    url: `http://127.0.0.1:${port}/api/canvas`,
    reuseExistingServer: false,
    timeout: 120_000,
  },
});
