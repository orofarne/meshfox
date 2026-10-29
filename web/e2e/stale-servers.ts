import { spawnSync } from "node:child_process";

// Matches every process the suite starts for a fixture: the `meshfox view`
// server and the worker it hands the canvas to, both of which carry the
// fixture path on their command line. The config keeps its fixtures in
// `<tmpdir>/meshfox-e2e-*` (a fresh `meshfox-e2e-fixtures-XXXX` per run, plus
// a few fixed directories such as `meshfox-e2e-mcp-live-*-fixture`).
const STALE_SERVER = "meshfox view .*meshfox-e2e-";

function running(): number {
  const found = spawnSync("pgrep", ["-f", STALE_SERVER], { encoding: "utf8" });
  return found.status === 0 ? found.stdout.split("\n").filter(Boolean).length : 0;
}

/**
 * Stops the fixture servers an earlier, interrupted run left behind.
 *
 * Each server stays on its fixed port with whatever state the earlier run
 * gave it, and a new run would otherwise find the port taken. The config
 * therefore never reuses a running server (`reuseExistingServer: false`), and
 * this clears the way so that costs nothing: killing a run (Ctrl-C twice, a
 * crashed terminal, a stopped task) cannot make the next one fail.
 *
 * Only the main Playwright process calls this, before any server starts —
 * test workers load the config too, but have nothing to clear.
 *
 * @returns how many processes were running when it started.
 */
export function stopStaleServers(): number {
  if (process.platform === "win32") return 0;
  const before = running();
  if (before === 0) return 0;
  spawnSync("pkill", ["-f", STALE_SERVER]);
  // A server exits on SIGTERM within a moment; its port is free once it has.
  // `Atomics.wait` sleeps without spinning (this runs while the config loads,
  // outside any async context).
  const sleeper = new Int32Array(new SharedArrayBuffer(4));
  for (let waited = 0; running() > 0 && waited < 5_000; waited += 100) Atomics.wait(sleeper, 0, 0, 100);
  if (running() > 0) {
    spawnSync("pkill", ["-9", "-f", STALE_SERVER]);
    for (let waited = 0; running() > 0 && waited < 2_000; waited += 100) Atomics.wait(sleeper, 0, 0, 100);
  }
  console.warn(`e2e: stopped ${before} fixture server process(es) left by an earlier run`);
  return before;
}
