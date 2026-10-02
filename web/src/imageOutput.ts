// Client-side twin of `core::output::render_output_block_image`'s content
// handling — only needed for the *live* "done" view of an `output="image"`
// block (MeshNode.tsx's `LiveRunOutput`), where the captured stdout is the
// raw SVG and has to be turned into the same one-line Markdown image the
// cached copy in the file already is. Keep the rules in step with the Rust
// side: only a successful run printing an SVG becomes an image; anything
// else is shown as the plain text it is.

import { imageAttrsToBraces, parseImageAttrsInner } from "./remarkImageAttrs.ts";
import { encodeBase64Utf8 } from "./svgTheme.ts";

/** Mirrors `svg::looks_like_svg`: an optional BOM/whitespace, optional XML
 * prolog/comments/doctype, then a root `<svg`. */
export function looksLikeSvg(text: string): boolean {
  let rest = text.replace(/^﻿/, "").trimStart();
  for (;;) {
    if (rest.startsWith("<svg")) {
      const next = rest[4];
      return next !== undefined && (next === ">" || next === "/" || /\s/.test(next));
    }
    let end: string;
    if (rest.startsWith("<?")) end = "?>";
    else if (rest.startsWith("<!--")) end = "-->";
    else if (rest.startsWith("<!")) end = ">";
    else return false;
    const at = rest.indexOf(end);
    if (at === -1) return false;
    rest = rest.slice(at + end.length).trimStart();
  }
}

/** The Markdown image line for an `output="image"` run's stdout, or `null`
 * when it isn't one (a failed run, or stdout that isn't an SVG) — the caller
 * then shows the output as ordinary text. `attrs` is the fence's
 * `output-attrs=`, forwarded in canonical form; an invalid one is left off,
 * same as the Rust side (`meshfox validate` is what reports it). */
export function imageOutputMarkdown(
  name: string,
  stdout: string,
  exitCode: number | undefined,
  attrs: string | undefined,
): string | null {
  const svg = stdout.trim();
  if ((exitCode ?? 0) !== 0 || !looksLikeSvg(svg)) return null;
  const alt = name.replace(/[[\]\\\r\n]/g, "");
  const parsed = attrs !== undefined ? parseImageAttrsInner(attrs) : null;
  const braces = parsed ? imageAttrsToBraces(parsed) : "";
  return `![${alt}](data:image/svg+xml;base64,${encodeBase64Utf8(svg)})${braces}`;
}
