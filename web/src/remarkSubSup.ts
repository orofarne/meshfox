// Client-side mirror of crates/core/src/subsup.rs — the narrow
// Pandoc/kramdown-style `x~2~` (subscript) / `x^2^` (superscript) syntax
// (see SPEC.md and TODO.canvas.md's `sub-superscript` task).
//
// Unlike the TUI (which substitutes Unicode small-form characters, since
// a terminal has no real subscript/superscript styling), the browser can
// just render real `<sub>`/`<sup>` elements — so a matched run becomes a
// small custom mdast node with `data.hName` set, the standard
// `mdast-util-to-hast` mechanism for "render this node as a specific HTML
// element", picked up with no `components` override needed.
//
// `^sup^` is untouched by anything else in the pipeline, so it's handled
// by a plain text scan (`scanText`/`visitText` below) exactly like the
// Rust side's `subsup::scan`. `~sub~` needs one extra step first:
// `remark-gfm`'s own strikethrough extension (unlike `pulldown-cmark`'s
// — see the Rust side's own comment) claims *any* well-formed single- or
// double-tilde run, including a word-attached one like `H~2~O`, as a
// `delete` node before this plugin ever runs — by the time a tree
// transform sees the document, "H~2~O" is no longer raw text at all.
// `reclaimSingleTildeDeletes` below un-does exactly the subset of that
// which matches this narrow grammar (single tilde, no internal
// whitespace, not flanked by outer whitespace — the same shape
// `pulldown-cmark` itself happens to leave alone as literal text, since
// its own strikethrough flanking rules reject that particular shape),
// converting it back into a subscript node; a `~~doubled~~` run, or a
// single-tilde run with internal or flanking whitespace (`~two words~`,
// ` ~word~ `), is left as real strikethrough on both sides — this is a
// best-effort match to `pulldown-cmark`'s own flanking behavior, not a
// byte-for-byte port of it, so a handful of exotic edge cases may still
// render differently between the two; the common `x~2~`/`H~2~O` shape
// this syntax actually exists for does not.

import type { Delete, Root, RootContent, Text } from "mdast";

type Script = "sub" | "sup";

interface TextPiece {
  kind: "text";
  text: string;
}

interface MarkedPiece {
  kind: "marked";
  script: Script;
  text: string;
}

type Piece = TextPiece | MarkedPiece;

function delimFor(c: string): Script | null {
  if (c === "~") return "sub";
  if (c === "^") return "sup";
  return null;
}

/** Mirrors `subsup::scan` — see its own doc comment for the exact rules. */
function scanText(text: string): Piece[] {
  const chars = Array.from(text);
  const n = chars.length;
  const pieces: Piece[] = [];
  let textStart = 0;
  let i = 0;
  while (i < n) {
    const c = chars[i];
    const script = delimFor(c);
    if (!script) {
      i++;
      continue;
    }
    const prevSame = i > 0 && chars[i - 1] === c;
    const nextSame = i + 1 < n && chars[i + 1] === c;
    if (prevSame || nextSame) {
      i++;
      continue;
    }
    let j = i + 1;
    let close = -1;
    while (j < n) {
      const cj = chars[j];
      if (/\s/.test(cj)) break;
      if (cj === c) {
        const closeNextSame = j + 1 < n && chars[j + 1] === c;
        if (!closeNextSame) close = j;
        break;
      }
      j++;
    }
    if (close === -1) {
      i++;
      continue;
    }
    if (i > textStart) {
      pieces.push({ kind: "text", text: chars.slice(textStart, i).join("") });
    }
    pieces.push({ kind: "marked", script, text: chars.slice(i + 1, close).join("") });
    textStart = close + 1;
    i = close + 1;
  }
  if (textStart < n) {
    pieces.push({ kind: "text", text: chars.slice(textStart).join("") });
  }
  return pieces;
}

function piecesToNodes(pieces: Piece[]): RootContent[] {
  return pieces.map((p): RootContent => {
    if (p.kind === "text") {
      return { type: "text", value: p.text } as Text;
    }
    return {
      type: p.script === "sub" ? "subscript" : "superscript",
      data: { hName: p.script === "sub" ? "sub" : "sup" },
      children: [{ type: "text", value: p.text } as Text],
    } as unknown as RootContent;
  });
}

function hasChildren(node: RootContent | Root): node is (RootContent | Root) & { children: RootContent[] } {
  return "children" in node && Array.isArray((node as { children?: unknown }).children);
}

function visitText(node: RootContent | Root): void {
  if (!hasChildren(node)) return;
  const children = node.children;
  for (let i = 0; i < children.length; i++) {
    const child = children[i];
    if (child.type === "text") {
      const pieces = scanText(child.value);
      if (pieces.length === 1 && pieces[0].kind === "text") continue; // no mark found, leave as-is
      const replacement = piecesToNodes(pieces);
      children.splice(i, 1, ...replacement);
      i += replacement.length - 1;
      continue;
    }
    visitText(child);
  }
}

function isWhitespaceChar(c: string | undefined): boolean {
  return c !== undefined && /\s/.test(c);
}

/** `node` is a `remark-gfm` `delete` (`~~`/`~`) node — reclaims it as a
 * `subscript` node (in place, keeping its existing children) if its
 * source span is single-tilde, its content is one plain whitespace-free
 * text node, and it isn't flanked by whitespace on either outer side. A
 * `~~doubled~~` run, or a single-tilde run that doesn't match this
 * narrow shape, is left alone as real strikethrough. */
function tryReclaimDelete(node: Delete, source: string): void {
  const pos = node.position;
  if (!pos || pos.start.offset === undefined || pos.end.offset === undefined) return;
  if (node.children.length !== 1 || node.children[0].type !== "text") return;
  const textNode = node.children[0] as Text;
  const cPos = textNode.position;
  if (!cPos || cPos.start.offset === undefined || cPos.end.offset === undefined) return;

  const delimsBefore = cPos.start.offset - pos.start.offset;
  const delimsAfter = pos.end.offset - cPos.end.offset;
  if (delimsBefore !== 1 || delimsAfter !== 1) return; // `~~doubled~~` or malformed

  if (textNode.value.length === 0 || /\s/.test(textNode.value)) return; // narrow grammar: no internal whitespace

  const before = pos.start.offset > 0 ? source[pos.start.offset - 1] : undefined;
  const after = pos.end.offset < source.length ? source[pos.end.offset] : undefined;
  if (isWhitespaceChar(before) || isWhitespaceChar(after)) return; // space-flanked -> real strikethrough

  (node as unknown as { type: string }).type = "subscript";
  node.data = { ...node.data, hName: "sub" };
}

function visitDeletes(node: RootContent | Root, source: string): void {
  if (!hasChildren(node)) return;
  for (const child of node.children) {
    if (child.type === "delete") {
      tryReclaimDelete(child as Delete, source);
    }
    visitDeletes(child, source);
  }
}

export default function remarkSubSup() {
  return (tree: Root, file: { value: string | Uint8Array }) => {
    visitDeletes(tree, String(file.value));
    visitText(tree);
  };
}
