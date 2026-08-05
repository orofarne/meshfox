#!/usr/bin/env node
// Regenerates ../../screenshot.webp from demo.canvas.md (a small fixture
// built just for this: a root node with the mascot, one child running a
// cached "hello world" — not README.md itself, so the screenshot doesn't
// depend on whatever README happens to look like at the time).
// The screenshot that's there now was a native macOS window capture of
// Safari (real traffic-light chrome, drop shadow) — not reproducible from a
// script, since headless Chromium can't fake a browser's own OS chrome. This
// renders the app content alone, at a fixed viewport, no frame around it.
import { chromium } from "@playwright/test";
import { execFileSync, spawn } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import readline from "node:readline";
import { fileURLToPath } from "node:url";

const webRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const repoRoot = path.resolve(webRoot, "..");
const demoCanvas = path.join(webRoot, "scripts/demo.canvas.md");
const outPath = path.join(repoRoot, "screenshot.webp");
const width = 800;
const height = 400;

console.log("building frontend (npm run build)...");
execFileSync("npm", ["run", "build"], { cwd: webRoot, stdio: "inherit" });

console.log("building meshfox-cli (cargo build)...");
execFileSync("cargo", ["build", "-p", "meshfox-cli"], { cwd: repoRoot, stdio: "inherit" });

const bin = path.join(repoRoot, "target/debug/meshfox");
const server = spawn(bin, ["view", demoCanvas, "--port", "0", "--no-open"], {
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
  const tmpPng = path.join(repoRoot, "_screenshot-tmp.png");
  const browser = await chromium.launch();
  const page = await browser.newPage({
    viewport: { width, height },
    deviceScaleFactor: 1,
    colorScheme: "dark",
  });
  await page.goto(url);
  await page.waitForSelector(".mesh-node-body");
  // The minimap is only useful once a canvas is too big to see at a glance —
  // for two nodes it's just clutter, and at this viewport size it overlaps
  // the content. Fit-view first, since the default viewport (anchored on the
  // root node's top-left, see App.tsx's INITIAL_ZOOM) doesn't guarantee both
  // nodes fit inside a short 800x400 frame.
  await page.addStyleTag({ content: ".react-flow__minimap { display: none !important; }" });
  await page.waitForTimeout(300);
  await page.click(".react-flow__controls-fitview");
  await page.waitForTimeout(300);
  await page.screenshot({ path: tmpPng });
  await browser.close();

  execFileSync("magick", [tmpPng, "-quality", "90", outPath]);
  fs.rmSync(tmpPng);
  console.log(`wrote ${path.relative(repoRoot, outPath)}`);
} finally {
  server.kill();
}
