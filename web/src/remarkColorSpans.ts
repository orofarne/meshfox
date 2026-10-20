import type { Root } from "mdast";
import { parseBackground } from "./remarkImageAttrs.ts";

// The same bounded grammar as core::color_spans. Never accepts free-form CSS.
export function parseColors(input: string): { style: string; consumed: number } | null {
  const match = /^\{([^}]*)\}/.exec(input);
  if (!match) return null;
  const seen = new Set<string>();
  const styles: string[] = [];
  for (const pair of match[1].trim().split(/\s+/)) {
    const attr = /^(color|background-color)=(?:"(#[\da-fA-F]+)"|'(#[\da-fA-F]+)'|(#[\da-fA-F]+))$/.exec(pair);
    if (!attr || seen.has(attr[1])) return null;
    const color = parseBackground(attr[2] ?? attr[3] ?? attr[4]);
    if (!color) return null;
    seen.add(attr[1]);
    styles.push(`${attr[1]}:${color};`);
  }
  return styles.length ? { style: styles.join(""), consumed: match[0].length } : null;
}

type Node = { type: string; value?: string; children?: Node[]; position?: { start: { offset?: number }; end: { offset?: number } }; data?: object };
type Token = { node: Node; delimiter?: boolean; sourceOffset?: number };
function text(value: string): Node { return { type: "text", value }; }
function append(nodes: Node[], node: Node) {
  const last = nodes.at(-1);
  if (node.type === "text" && last?.type === "text") last.value = (last.value ?? "") + node.value;
  else nodes.push(node);
}

type Bracket = { delimiter: boolean; offset: number };
const BRACKET_BY_CODE: Record<number, string> = { 91: "[", 93: "]" };

// What a bracket-ish source spelling decodes to ("" for any other entity).
function decodeSpelling(spelling: string): string {
  if (spelling.startsWith("\\")) return spelling.slice(1);
  if (spelling.startsWith("&#")) {
    const hex = spelling[2].toLowerCase() === "x";
    const code = Number.parseInt(spelling.slice(hex ? 3 : 2, -1), hex ? 16 : 10);
    return BRACKET_BY_CODE[code] ?? "";
  }
  if (/^&(lbrack|lsqb);$/.test(spelling)) return "[";
  if (/^&(rbrack|rsqb);$/.test(spelling)) return "]";
  return spelling;
}

// Align bracket characters with their original spelling. Remark coalesces
// entities and escapes into text; neither may introduce a delimiter.
function bracketsIn(raw: string, start: number): Bracket[] {
  const brackets: Bracket[] = [];
  for (const match of raw.matchAll(/\\[\s\S]|&(?:#x[0-9a-f]+|#\d+|[a-z][a-z0-9]+);|[[\]]/gi)) {
    const spelling = match[0];
    const decoded = decodeSpelling(spelling);
    if (decoded === "[" || decoded === "]") brackets.push({ delimiter: spelling === decoded, offset: start + match.index! });
  }
  return brackets;
}

function tokenize(children: Node[], source: string): Token[] {
  const tokens: Token[] = [];
  for (const node of children) {
    if (node.type !== "text") { tokens.push({ node }); continue; }
    const start = node.position?.start.offset;
    const raw = start === undefined ? "" : source.slice(start, node.position?.end.offset);
    const brackets = bracketsIn(raw, start ?? 0);
    for (const ch of node.value ?? "") {
      const bracket = ch === "[" || ch === "]" ? brackets.shift() : undefined;
      tokens.push({ node: text(ch), delimiter: bracket?.delimiter, sourceOffset: bracket?.offset });
    }
  }
  return tokens;
}

// The `{...}` attributes right after the `]` at tokens[i], if the tokens'
// own text spells them exactly as the source does.
function colorAttrsAfter(tokens: Token[], i: number, source: string): { style: string; consumed: number } | null {
  const sourceOffset = tokens[i].sourceOffset;
  if (sourceOffset === undefined) return null;
  const attrs = parseColors(source.slice(sourceOffset + 1));
  if (!attrs) return null;
  let tail = "";
  for (let j = i + 1; j < tokens.length && tokens[j].node.type === "text"; j++) {
    tail += tokens[j].node.value;
    if (tail.endsWith("}")) break;
  }
  const spelled = source.slice(sourceOffset + 1, sourceOffset + 1 + attrs.consumed);
  return tail.slice(0, attrs.consumed) === spelled ? attrs : null;
}

// An unmatched `[`: back to literal text, followed by what it enclosed.
function appendUnclosed(frames: Node[][], content: Node[]) {
  const top = frames.at(-1)!;
  append(top, text("["));
  for (const child of content) append(top, child);
}

function nestSpans(tokens: Token[], source: string): Node[] {
  const frames: Node[][] = [[]];
  for (let i = 0; i < tokens.length; i++) {
    const { node, delimiter } = tokens[i];
    if (delimiter && node.value === "[") { frames.push([]); continue; }
    if (delimiter && node.value === "]" && frames.length > 1) {
      const content = frames.pop()!;
      const attrs = colorAttrsAfter(tokens, i, source);
      if (attrs) {
        append(frames.at(-1)!, { type: "colorSpan", children: content, data: { hName: "span", hProperties: { style: attrs.style } } });
        i += attrs.consumed;
        continue;
      }
      appendUnclosed(frames, content);
    }
    append(frames.at(-1)!, node);
  }
  while (frames.length > 1) {
    const content = frames.pop()!;
    appendUnclosed(frames, content);
  }
  return frames[0];
}

function visit(parent: Node, source: string) {
  if (!parent.children || parent.type === "image" || parent.type === "imageReference") return;
  for (const child of parent.children) visit(child, source);
  if (!parent.children.some(node => node.type === "text" && node.value?.includes("["))) return;
  parent.children = nestSpans(tokenize(parent.children, source), source);
}

export default function remarkColorSpans() {
  return (tree: Root, file: { value: string | Uint8Array }) => visit(tree as unknown as Node, String(file.value));
}
