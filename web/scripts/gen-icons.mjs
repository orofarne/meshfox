#!/usr/bin/env node
// Renders ../../mascot.txt (the square ASCII mascot, see README/--help for the
// long variant) into the icon set under web/public/. Goes through a real
// browser rather than ImageMagick so the mascot's "‿" (U+203F, the smile)
// gets picked up from font fallback — Fira Code itself doesn't carry that
// glyph, and single-font rasterizers like ImageMagick just drop it.
import { chromium } from "@playwright/test";
import { execFileSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const webRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const repoRoot = path.resolve(webRoot, "..");
const outDir = path.join(webRoot, "public");
const masterSize = 1024;

const mascot = fs.readFileSync(path.join(repoRoot, "mascot.txt"), "utf8").replace(/\n$/, "");
const fontPath = path.join(
  webRoot,
  "node_modules/@fontsource/fira-code/files/fira-code-latin-500-normal.woff2",
);
const fontBase64 = fs.readFileSync(fontPath).toString("base64");

// Each character gets its own fixed-width cell instead of relying on the
// text's own advance widths. Two things break plain `white-space: pre`
// monospacing here: Fira Code's ligatures (its whole selling point) can
// fuse adjacent characters into a wider glyph, and the fallback font that
// draws "‿" (U+203F — Fira Code has no glyph for it) isn't guaranteed to
// share Fira Code's column width. Either one throws every character after
// it out of alignment with the rows above/below. A fixed-width cell per
// character can't drift regardless of which font/glyph lands inside it.
const escapeHtml = (s) => s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");
const rows = mascot
  .split("\n")
  .map((line) => {
    const cells = Array.from(line)
      .map((ch) => `<span class="cell">${ch === " " ? "&nbsp;" : escapeHtml(ch)}</span>`)
      .join("");
    return `<div class="row">${cells}</div>`;
  })
  .join("\n");

const html = `<!doctype html>
<html><head><style>
@font-face {
  font-family: "Fira Code";
  src: url(data:font/woff2;base64,${fontBase64}) format("woff2");
}
html, body { margin: 0; padding: 0; background: transparent; }
body {
  width: ${masterSize}px;
  height: ${masterSize}px;
  display: flex;
  align-items: center;
  justify-content: center;
}
.mascot {
  font-family: "Fira Code", monospace;
  font-variant-ligatures: none;
  font-feature-settings: "calt" 0, "liga" 0, "dlig" 0;
  font-size: ${masterSize * 0.2}px;
  color: #ff6e15;
}
.row {
  display: flex;
  line-height: 1.15;
}
.cell {
  display: inline-block;
  width: 1ch;
  text-align: center;
}
</style></head>
<body><div class="mascot">${rows}</div></body></html>`;

fs.mkdirSync(outDir, { recursive: true });
const glyphPath = path.join(outDir, "_mascot-glyph.png");
const glyphTrimmedPath = path.join(outDir, "_mascot-glyph-trimmed.png");
const bgPath = path.join(outDir, "_mascot-bg.png");
const masterPath = path.join(outDir, "_mascot-master.png");

// Render the glyph alone on a transparent canvas, then trim it down to its
// own ink so it can be sized and centered independently of font metrics
// (a monospace font's line-height leaves uneven padding above/below the
// last line, which threw off naive flexbox centering).
const browser = await chromium.launch();
const page = await browser.newPage({ viewport: { width: masterSize, height: masterSize } });
await page.setContent(html);
await page.screenshot({ path: glyphPath, omitBackground: true });
await browser.close();

execFileSync("magick", [glyphPath, "-trim", "+repage", glyphTrimmedPath]);

const corner = Math.round(masterSize * 0.18);
execFileSync("magick", [
  "-size", `${masterSize}x${masterSize}`,
  "xc:none",
  "-fill", "#0a0a0c",
  "-draw", `roundrectangle 0,0 ${masterSize - 1},${masterSize - 1} ${corner},${corner}`,
  bgPath,
]);

const glyphBox = Math.round(masterSize * 0.72);
execFileSync("magick", [
  bgPath,
  "(", glyphTrimmedPath, "-resize", `${glyphBox}x${glyphBox}`, ")",
  "-gravity", "center",
  "-compose", "over",
  "-composite",
  masterPath,
]);

fs.rmSync(glyphPath);
fs.rmSync(glyphTrimmedPath);
fs.rmSync(bgPath);

// Each [name, size] pair below is one output file, all resized down from
// the single master render above so every icon stays crisp and consistent.
const targets = [
  ["favicon-16.png", 16],
  ["favicon-32.png", 32],
  ["apple-touch-icon.png", 180],
  ["icon-192.png", 192],
  ["icon-512.png", 512],
];

for (const [name, size] of targets) {
  execFileSync("magick", [
    masterPath,
    "-filter", "Lanczos",
    "-resize", `${size}x${size}`,
    path.join(outDir, name),
  ]);
}

// A multi-resolution favicon.ico, for browsers that still ask for one directly.
execFileSync("magick", [
  masterPath,
  "-filter", "Lanczos",
  "-define", "icon:auto-resize=16,32,48",
  path.join(outDir, "favicon.ico"),
]);

fs.rmSync(masterPath);

console.log(`wrote icon set to ${path.relative(repoRoot, outDir)}/`);
