// Client-side counterpart to `Options::ENABLE_GFM` on the Rust side
// (`crates/core/src/staticgen.rs`, the TUI's `markdown.rs`) — GitHub's
// alert blockquote syntax, `> [!NOTE]`/`> [!TIP]`/`> [!IMPORTANT]`/
// `> [!WARNING]`/`> [!CAUTION]` (see SPEC.md and TODO.canvas.md's
// `admonition-callouts` task). `remark-gfm` itself doesn't parse this —
// alerts are a GitHub UI convention layered on top of an ordinary
// blockquote, not part of the GFM spec `remark-gfm` implements — so this
// is a small plugin of its own, matching `pulldown-cmark`'s own
// `Options::ENABLE_GFM` behavior byte-for-byte: the marker line is
// stripped entirely (never left as visible text) and the blockquote gets
// a `markdown-alert-<type>` class, same single-class scheme
// `pulldown-cmark`'s HTML writer uses — `index.css`'s own
// `.markdown-alert-*` rules (mirroring `site-template/style.css`'s) are
// what actually add the icon/label/color, purely via CSS, so web and
// static/PDF end up looking the same with no extra injected DOM either
// side.

import type { Blockquote, Paragraph, Root, RootContent, Text } from "mdast";

const ALERT_TYPES = ["note", "tip", "important", "warning", "caution"] as const;
type AlertType = (typeof ALERT_TYPES)[number];

const MARKER_RE = /^\[!(note|tip|important|warning|caution)\](?:\r?\n|$)/i;

function matchMarker(value: string): { type: AlertType; rest: string } | null {
  const m = MARKER_RE.exec(value);
  if (!m) return null;
  return { type: m[1].toLowerCase() as AlertType, rest: value.slice(m[0].length) };
}

function applyAlert(bq: Blockquote): void {
  const first = bq.children[0];
  if (!first || first.type !== "paragraph") return;
  const paragraph = first as Paragraph;
  const firstInline = paragraph.children[0];
  if (!firstInline || firstInline.type !== "text") return;
  const matched = matchMarker((firstInline as Text).value);
  if (!matched) return;

  if (matched.rest.length === 0) {
    paragraph.children.shift();
  } else {
    (firstInline as Text).value = matched.rest;
  }
  if (paragraph.children.length === 0) {
    bq.children.shift();
  }

  const className = [`markdown-alert-${matched.type}`];
  bq.data = { ...bq.data, hProperties: { ...(bq.data?.hProperties as object), className } };
}

function hasChildren(node: RootContent | Root): node is (RootContent | Root) & { children: RootContent[] } {
  return "children" in node && Array.isArray((node as { children?: unknown }).children);
}

function visit(node: RootContent | Root): void {
  if (!hasChildren(node)) return;
  for (const child of node.children) {
    if (child.type === "blockquote") {
      applyAlert(child as Blockquote);
    }
    visit(child);
  }
}

export default function remarkGfmAlerts() {
  return (tree: Root) => {
    visit(tree);
  };
}
