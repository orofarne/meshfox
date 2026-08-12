#!/usr/bin/env node
// Regenerates ../../screenshot-tui.webp — the terminal-viewer counterpart to
// gen-screenshot.mjs's browser screenshot, and built the same way: point a
// real headless browser at the real app and capture actual pixels, not a
// mockup. There's no OS-level terminal screenshot to fall back to here, so
// this is the *only* way this image gets made.
//
// The trick: `meshfox view` already gives a `tty` block a real pseudo-
// terminal and streams it into an in-browser `xterm.js` panel (see
// TtyPanel.tsx) — that's exactly "a real terminal, rendered as pixels in a
// page Playwright can screenshot". So this generates a throwaway outer
// canvas with a single `tty` block whose command is `meshfox tui
// demo.canvas.md`, and driving the actual TUI is then just "click run" and
// screenshot the resulting panel — same demo content as the browser
// screenshot's demo.canvas.md.
//
// That outer canvas is written out here rather than checked in as its own
// fixture because a `tty` block's process doesn't inherit this script's cwd
// or `PATH` (`meshfox view` gives it a fresh pty via `portable-pty`, which
// defaults an unset cwd to `$HOME` and starts from a cleared environment —
// see `pty_exec.rs`), so its command needs this machine's absolute path to
// the `meshfox` binary and to demo.canvas.md baked in, not a relative one a
// checked-in file could carry.
import { chromium } from "@playwright/test";
import { execFileSync, spawn } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import readline from "node:readline";
import { fileURLToPath } from "node:url";

const webRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const repoRoot = path.resolve(webRoot, "..");
const demoCanvas = path.join(webRoot, "scripts/demo.canvas.md");
const bin = path.join(repoRoot, "target/debug/meshfox");
const demoTuiCanvas = path.join(repoRoot, "_demo-tui-tmp.canvas.md");
const outPath = path.join(repoRoot, "screenshot-tui.webp");
// Room for the tty panel's own max size (min(900px, 92vw) x min(560px,
// 82vh), see TtyPanel's CSS) to render unclipped.
const width = 1000;
const height = 700;

console.log("building frontend (npm run build)...");
execFileSync("npm", ["run", "build"], { cwd: webRoot, stdio: "inherit" });

console.log("building meshfox-cli (cargo build)...");
execFileSync("cargo", ["build", "-p", "meshfox-cli"], { cwd: repoRoot, stdio: "inherit" });

fs.writeFileSync(
  demoTuiCanvas,
  [
    "<!-- meshfox:canvas -->",
    "# tui screenshot",
    '<!-- meshfox:node id="root" -->',
    "",
    "```bash name=\"tui\" tty",
    `${bin} tui ${demoCanvas}`,
    "```",
    "",
  ].join("\n"),
);

const server = spawn(bin, ["view", demoTuiCanvas, "--port", "0", "--no-open"], {
  cwd: repoRoot,
  stdio: ["ignore", "pipe", "pipe"],
});

const url = await new Promise((resolve, reject) => {
  const rl = readline.createInterface({ input: server.stdout });
  rl.on("line", (line) => {
    const m = line.match(/serving .+ on (http:\/\/\S+)/);
    if (m) resolve(m[1]);
  });
  server.stderr.on("data", (d) => process.stderr.write(d));
  server.on("exit", (code) => reject(new Error(`meshfox view exited early (code ${code})`)));
  setTimeout(() => reject(new Error("timed out waiting for meshfox view to start")), 10_000);
});

try {
  const tmpPng = path.join(repoRoot, "_screenshot-tui-tmp.png");
  const browser = await chromium.launch();
  const page = await browser.newPage({
    viewport: { width, height },
    deviceScaleFactor: 1,
    colorScheme: "dark",
  });
  await page.goto(url);
  await page.waitForSelector(".mesh-node-body");
  // Same reasoning as gen-screenshot.mjs's own fit-view call — the app
  // opens at a fixed zoom on the root node, not fitted to content, and the
  // run button has to actually be in-viewport for the click below.
  await page.click(".react-flow__controls-fitview");
  await page.waitForTimeout(300);

  await page.getByRole("button", { name: "run tui", exact: true }).click();
  // "tty" status means the pty is live and `meshfox tui` has taken over it
  // — before that, the panel is just a "starting…" placeholder.
  await page.waitForSelector('.mesh-tty-head-status[data-status="tty"]');
  // Lets the TUI's own first draw (tree walk, markdown render, syntax
  // highlighting) land before the shot.
  await page.waitForTimeout(500);

  // The TUI opens with the root node selected, which is just mascot art
  // (see demo.canvas.md) — clicking the "Hello" tree row (mouse support
  // covers this, see SPEC.md/README's "Terminal viewer" section) selects
  // it instead, whose body shows off syntax highlighting and a cached run
  // result, the same reason gen-screenshot.mjs's own demo picks it. It's
  // always the pty's second row (first is the tree's own top border), a
  // fixed spot given the panel's fixed pixel size and font metrics.
  const panelBox = await page.locator(".mesh-tty-panel").boundingBox();
  await page.mouse.click(panelBox.x + 65, panelBox.y + 87);
  await page.waitForTimeout(300);

  await page.locator(".mesh-tty-panel").screenshot({ path: tmpPng });
  await browser.close();

  execFileSync("magick", [tmpPng, "-quality", "90", outPath]);
  fs.rmSync(tmpPng);
  console.log(`wrote ${path.relative(repoRoot, outPath)}`);
} finally {
  server.kill();
  fs.rmSync(demoTuiCanvas, { force: true });
}
