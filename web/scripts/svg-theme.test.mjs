import assert from "node:assert/strict";
import test from "node:test";
import { injectTheme, themeCss, themeSvgDataUrl } from "../src/svgTheme.ts";
import { parseBackground } from "../src/remarkImageAttrs.ts";

// Same vectors as crates/core/src/svg.rs's tests — the two implementations
// are twins, so they must agree.
const DARK = { fg: "#f2ede6", bg: "#131316", accent: "#ff6e15", border: "#7a4620" };
const SVG =
  '<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10"><rect fill="currentColor"/></svg>';

test("injectTheme puts a style right after the root tag", () => {
  const out = injectTheme(SVG, DARK);
  assert.ok(
    out.startsWith(
      '<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10"><style>svg{color:#f2ede6}',
    ),
    out,
  );
  assert.ok(out.includes("--mf-bg:#131316"));
  assert.ok(out.endsWith('<rect fill="currentColor"/></svg>'));
  assert.ok(out.includes(themeCss(DARK)));
});

test("injectTheme is quote-aware and skips comments", () => {
  const svg = '<!-- <svg> --><svg data-x="a>b" viewBox="0 0 1 1"><g/></svg>';
  const out = injectTheme(svg, DARK);
  assert.ok(out.includes('data-x="a>b" viewBox="0 0 1 1"><style>'), out);
  assert.ok(out.startsWith("<!-- <svg> -->"));
});

test("injectTheme leaves a self-closing or missing root alone", () => {
  assert.equal(injectTheme("<svg/>", DARK), "<svg/>");
  assert.equal(injectTheme('<svg width="1"/>', DARK), '<svg width="1"/>');
  assert.equal(injectTheme("plain", DARK), "plain");
  assert.equal(injectTheme('<svg width="1', DARK), '<svg width="1');
  assert.equal(injectTheme("<svgfoo>", DARK), "<svgfoo>");
});

test("themeSvgDataUrl rewrites an SVG data URL and round-trips non-ASCII", () => {
  const svg = '<svg xmlns="http://www.w3.org/2000/svg"><text>Температура °C — ✓</text></svg>';
  const src = "data:image/svg+xml;base64," + Buffer.from(svg, "utf8").toString("base64");
  const out = themeSvgDataUrl(src, DARK);
  assert.notEqual(out, src);
  const decoded = Buffer.from(out.slice("data:image/svg+xml;base64,".length), "base64").toString("utf8");
  assert.equal(decoded, injectTheme(svg, DARK));
  assert.ok(decoded.includes("Температура °C — ✓"));
});

test("themeSvgDataUrl leaves everything else untouched", () => {
  assert.equal(themeSvgDataUrl("data:image/png;base64,AAAA", DARK), "data:image/png;base64,AAAA");
  assert.equal(themeSvgDataUrl("pic.svg", DARK), "pic.svg");
  assert.equal(themeSvgDataUrl("data:image/svg+xml;base64,!!!notbase64", DARK), "data:image/svg+xml;base64,!!!notbase64");
  const notSvg = "data:image/svg+xml;base64," + Buffer.from("hello", "utf8").toString("base64");
  assert.equal(themeSvgDataUrl(notSvg, DARK), notSvg);
});

test("themeSvgDataUrl copes with a large SVG", () => {
  const svg = `<svg xmlns="x">${"<g/>".repeat(100000)}</svg>`;
  const src = "data:image/svg+xml;base64," + Buffer.from(svg, "utf8").toString("base64");
  const out = themeSvgDataUrl(src, DARK);
  assert.ok(out.length > src.length);
});

// Same cases as image_attrs.rs's tests.
test("parseBackground accepts hex and transparent, normalising", () => {
  assert.equal(parseBackground("#fff"), "#ffffff");
  assert.equal(parseBackground("#1A2b3C"), "#1a2b3c");
  assert.equal(parseBackground("transparent"), "transparent");
  assert.equal(parseBackground("TRANSPARENT"), "transparent");
});

test("parseBackground rejects anything else", () => {
  for (const bad of ["white", "#ff", "#ggg", "#ffff", "#ffffffff", "red;x:y", "", "fff"]) {
    assert.equal(parseBackground(bad), null, bad);
  }
});

// imageOutput.ts / fence fingerprint — mirror crates/core/src/output.rs and fence.rs.
import { looksLikeSvg, imageOutputMarkdown } from "../src/imageOutput.ts";
import { parseImageAttrsInner, imageAttrsToBraces } from "../src/remarkImageAttrs.ts";
import { fingerprint, parseBody } from "../src/fence.ts";

const PLAIN_SVG =
  '<svg xmlns="http://www.w3.org/2000/svg" width="4" height="4"><rect width="4" height="4"/></svg>';

test("looksLikeSvg agrees with the Rust sniff", () => {
  assert.ok(looksLikeSvg(PLAIN_SVG));
  assert.ok(looksLikeSvg("\n  <svg>"));
  assert.ok(looksLikeSvg('﻿<?xml version="1.0"?>\n<svg xmlns="x">'));
  assert.ok(looksLikeSvg('<?xml version="1.0"?><!-- made by x --><!DOCTYPE svg><svg>'));
  for (const bad of ["", "hello", "<html><svg></svg></html>", "<svgfoo>", '<?xml version="1.0"?>', "Error: <svg> missing"]) {
    assert.ok(!looksLikeSvg(bad), bad);
  }
});

test("imageOutputMarkdown wraps an SVG and forwards canonical attrs", () => {
  const b64 = Buffer.from(PLAIN_SVG, "utf8").toString("base64");
  assert.equal(imageOutputMarkdown("d", PLAIN_SVG, 0, undefined), `![d](data:image/svg+xml;base64,${b64})`);
  assert.equal(
    imageOutputMarkdown("d", `\n${PLAIN_SVG}\n`, 0, "bg=#FFF width=50%"),
    `![d](data:image/svg+xml;base64,${b64}){width=50% bg=#ffffff}`,
  );
  // Invalid attrs are simply left off.
  assert.equal(imageOutputMarkdown("d", PLAIN_SVG, 0, "color=red"), `![d](data:image/svg+xml;base64,${b64})`);
});

test("imageOutputMarkdown is null for a failed run or non-SVG stdout", () => {
  assert.equal(imageOutputMarkdown("d", PLAIN_SVG, 2, undefined), null);
  assert.equal(imageOutputMarkdown("d", "plantuml: not found", 0, undefined), null);
});

test("imageAttrs helpers round-trip like Rust's parse_inner/to_braces", () => {
  const attrs = parseImageAttrsInner("bg=#FFF  width=50%");
  assert.equal(imageAttrsToBraces(attrs), "{width=50% bg=#ffffff}");
  assert.equal(parseImageAttrsInner(""), null);
  assert.equal(parseImageAttrsInner("width=1}"), null);
  assert.equal(imageAttrsToBraces({}), "");
});

test("output-attrs changes the fence fingerprint only when present", () => {
  const base = fingerprint("bash", "x", undefined, undefined, undefined);
  assert.equal(base, fingerprint("bash", "x", undefined, undefined, undefined, undefined));
  assert.notEqual(base, fingerprint("bash", "x", undefined, undefined, undefined, "bg=#fff"));
});

test("an output=image fence is parsed as a markdown-mode block with its attrs", () => {
  const md = '```bash name="d" cache output="image" output-attrs="bg=#fff width=50%"\nx\n```\n';
  const seg = parseBody(md, "n").find((s) => s.type === "code");
  assert.equal(seg.outputImage, true);
  assert.equal(seg.outputMarkdown, true);
  assert.equal(seg.outputAttrs, "bg=#fff width=50%");
});
