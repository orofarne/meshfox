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

function visit(parent: Node, source: string) {
  if (!parent.children || parent.type === "image" || parent.type === "imageReference") return;
  for (const child of parent.children) visit(child, source);
  if (!parent.children.some(node => node.type === "text" && node.value?.includes("["))) return;
  const tokens: Token[] = [];
  for (const node of parent.children) {
    if (node.type !== "text") { tokens.push({ node }); continue; }
    const start = node.position?.start.offset;
    const raw = start === undefined ? "" : source.slice(start, node.position?.end.offset);
    // Align bracket characters with their original spelling. Remark coalesces
    // entities and escapes into text; neither may introduce a delimiter.
    const brackets: { delimiter: boolean; offset: number }[] = [];
    for (const match of raw.matchAll(/\\[\s\S]|&(?:#x[0-9a-f]+|#[0-9]+|[a-z][a-z0-9]+);|[\[\]]/gi)) {
      const spelling = match[0];
      let decoded = spelling;
      if (spelling.startsWith("\\")) decoded = spelling.slice(1);
      else if (spelling.startsWith("&#")) {
        const hex = spelling[2].toLowerCase() === "x";
        const number = parseInt(spelling.slice(hex ? 3 : 2, -1), hex ? 16 : 10);
        decoded = number === 91 ? "[" : number === 93 ? "]" : "";
      } else if (/^&(lbrack|lsqb);$/.test(spelling)) decoded = "[";
      else if (/^&(rbrack|rsqb);$/.test(spelling)) decoded = "]";
      if (decoded === "[" || decoded === "]") brackets.push({ delimiter: spelling === decoded, offset: (start ?? 0) + match.index! });
    }
    for (const ch of node.value ?? "") {
      const bracket = ch === "[" || ch === "]" ? brackets.shift() : undefined;
      tokens.push({ node: text(ch), delimiter: bracket?.delimiter, sourceOffset: bracket?.offset });
    }
  }
  const frames: Node[][] = [[]];
  for (let i = 0; i < tokens.length; i++) {
    const { node, delimiter, sourceOffset } = tokens[i];
    if (delimiter && node.value === "[") { frames.push([]); continue; }
    if (delimiter && node.value === "]" && frames.length > 1) {
      const content = frames.pop()!;
      let tail = "";
      for (let j = i + 1; j < tokens.length && tokens[j].node.type === "text"; j++) {
        tail += tokens[j].node.value;
        if (tail.endsWith("}")) break;
      }
      const attrs = sourceOffset === undefined ? null : parseColors(source.slice(sourceOffset + 1));
      if (attrs && tail.slice(0, attrs.consumed) === source.slice(sourceOffset! + 1, sourceOffset! + 1 + attrs.consumed)) {
        append(frames.at(-1)!, { type: "colorSpan", children: content, data: { hName: "span", hProperties: { style: attrs.style } } });
        i += attrs.consumed;
        continue;
      }
      append(frames.at(-1)!, text("["));
      for (const child of content) append(frames.at(-1)!, child);
    }
    append(frames.at(-1)!, node);
  }
  while (frames.length > 1) {
    const content = frames.pop()!;
    append(frames.at(-1)!, text("["));
    for (const node of content) append(frames.at(-1)!, node);
  }
  parent.children = frames[0];
}

export default function remarkColorSpans() {
  return (tree: Root, file: { value: string | Uint8Array }) => visit(tree as unknown as Node, String(file.value));
}
