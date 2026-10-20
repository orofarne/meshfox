import { spawnSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";

// The command prefix every `webServer` in `playwright.config.ts` starts a
// worker with (`<prefix> <fixture> --port N …`).
//
// The suite starts ~40 workers at once, one per fixture. Through `cargo run`
// each of them takes the build lock and re-checks the whole workspace for
// freshness (~3 s even when nothing changed, one after another) and then goes
// through the macOS signing runner — about two minutes before the first test,
// even to run a single spec, for the same binary every time. `pretest:e2e`
// builds (and signs) it once instead, and the servers run it directly.
//
// Falls back to `cargo run` — which rebuilds and signs as needed — whenever the
// binary can't be trusted: missing, older than the Rust sources (a bare
// `npx playwright test` after an edit that skipped `pretest:e2e`), or still
// carrying only the linker's own signature on macOS (see scripts/run-signed.sh).
const REPO = path.join(import.meta.dirname, "../..");
const CARGO_RUN = "cargo run -q --manifest-path ../Cargo.toml -p meshfox-cli -- view";

const targetDir = process.env.CARGO_TARGET_DIR ? path.resolve(process.env.CARGO_TARGET_DIR) : path.join(REPO, "target");
export const MESHFOX_BIN = path.join(targetDir, "debug", process.platform === "win32" ? "meshfox.exe" : "meshfox");

function newestMtime(dir: string): number {
  let newest = 0;
  for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
    // Test, bench and example sources never go into the `meshfox` binary.
    if (["node_modules", "target", "tests", "benches", "examples"].includes(entry.name) || entry.name.startsWith(".")) continue;
    const full = path.join(dir, entry.name);
    newest = Math.max(newest, entry.isDirectory() ? newestMtime(full) : fs.statSync(full).mtimeMs);
  }
  return newest;
}

function linkerSignedOnly(): boolean {
  if (process.platform !== "darwin") return false;
  const info = spawnSync("/usr/bin/codesign",["-dv", MESHFOX_BIN], { encoding: "utf8" });
  return info.status !== 0 || `${info.stdout}${info.stderr}`.includes("linker-signed");
}

function binaryIsReady(): boolean {
  if (process.env.MESHFOX_E2E_CARGO_RUN) return false;
  let built: number;
  try {
    built = fs.statSync(MESHFOX_BIN).mtimeMs;
  } catch {
    return false;
  }
  const sources = Math.max(
    newestMtime(path.join(REPO, "crates")),
    fs.statSync(path.join(REPO, "Cargo.toml")).mtimeMs,
    fs.statSync(path.join(REPO, "Cargo.lock")).mtimeMs,
    fs.statSync(path.join(REPO, ".cargo/config.toml")).mtimeMs,
  );
  return built >= sources && !linkerSignedOnly();
}

export const MESHFOX_VIEW = binaryIsReady() ? `${MESHFOX_BIN} view` : CARGO_RUN;
