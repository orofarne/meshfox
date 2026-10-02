// Client-side mirror of crates/core/src/image_attrs.rs — the narrow
// Pandoc/GitLab-style `{width=..}`/`{height=..}`/`{bg=..}` syntax right
// after an image, with no space (see SPEC.md's "Formal grammar" / TODO.canvas.md's
// `image-attrs` task). Deliberately hand-rolled rather than pulling in
// `remark-attr` (the same reasoning TODO.canvas.md records for the Rust
// side: that package's full Pandoc `{.class #id ...}` grammar would become
// a de-facto spec this narrow syntax would then have to track).
//
// A plain unified/remark plugin: walks the tree looking for an `image`
// node immediately followed by a `text` sibling starting with `{...}`,
// parses it, and sets `data.hProperties` on the image node — the standard
// `mdast-util-to-hast` mechanism for attaching extra HTML attributes to a
// node's rendered element, picked up automatically by `remark-rehype`
// with no `components` override needed (`MeshNode.tsx`'s own `img`
// component already spreads through whatever `hProperties` produced).

import type { Image, Root, RootContent, Text } from "mdast";

interface ParsedImageAttrs {
  width?: string;
  height?: string;
  /** Canonical CSS spelling (`#rrggbb` lowercase, or `transparent`) —
   * the same normalisation `image_attrs::Background`'s `Display` does. */
  bg?: string;
  consumed: number;
}

/** Mirrors `image_attrs::Background::parse`: `#rgb`/`#rrggbb` (any case) or
 * `transparent`, normalised to `#rrggbb`/`transparent`. Only ever these
 * shapes reach a `style` attribute, so nothing else can be smuggled into it. */
export function parseBackground(value: string): string | null {
  if (value.toLowerCase() === "transparent") return "transparent";
  const m = /^#([0-9a-fA-F]{3}|[0-9a-fA-F]{6})$/.exec(value);
  if (!m) return null;
  let hex = m[1].toLowerCase();
  if (hex.length === 3) hex = [...hex].map((c) => c + c).join("");
  return `#${hex}`;
}

export interface ImageAttrValues {
  width?: string;
  height?: string;
  bg?: string;
}

/** Mirrors `image_attrs::parse_inner`: the grammar between the braces —
 * `width=`/`height=` (a bare integer or integer+`%`) and `bg=` (a hex color
 * or `transparent`), each at most once. `null` for anything else (including
 * empty input). */
export function parseImageAttrsInner(inner: string): ImageAttrValues | null {
  const tokens = inner.split(/\s+/).filter((t) => t.length > 0);
  if (tokens.length === 0) return null;
  const result: ImageAttrValues = {};
  for (const tok of tokens) {
    const eq = tok.indexOf("=");
    if (eq === -1) return null;
    const key = tok.slice(0, eq);
    const value = tok.slice(eq + 1);
    if (key === "bg") {
      if (result.bg !== undefined) return null;
      const bg = parseBackground(value);
      if (bg === null) return null;
      result.bg = bg;
      continue;
    }
    if (!/^\d+%?$/.test(value)) return null;
    if (key === "width") {
      if (result.width !== undefined) return null;
      result.width = value;
    } else if (key === "height") {
      if (result.height !== undefined) return null;
      result.height = value;
    } else {
      return null;
    }
  }
  return result;
}

/** Mirrors `ImageAttrs::to_braces`: the canonical `{width=.. height=.. bg=..}`
 * spelling, or `""` when nothing is set. */
export function imageAttrsToBraces(attrs: ImageAttrValues): string {
  const parts: string[] = [];
  if (attrs.width !== undefined) parts.push(`width=${attrs.width}`);
  if (attrs.height !== undefined) parts.push(`height=${attrs.height}`);
  if (attrs.bg !== undefined) parts.push(`bg=${attrs.bg}`);
  return parts.length === 0 ? "" : `{${parts.join(" ")}}`;
}

/** Mirrors `image_attrs::parse`: a leading `{...}` right after an image.
 * `null` for anything else, in which case the text is left completely
 * alone. */
function parseImageAttrs(text: string): ParsedImageAttrs | null {
  if (!text.startsWith("{")) return null;
  const close = text.indexOf("}");
  if (close === -1) return null;
  const values = parseImageAttrsInner(text.slice(1, close));
  if (values === null) return null;
  return { ...values, consumed: close + 1 };
}

function hasChildren(node: RootContent | Root): node is (RootContent | Root) & { children: RootContent[] } {
  return "children" in node && Array.isArray((node as { children?: unknown }).children);
}

function visit(node: RootContent | Root): void {
  if (!hasChildren(node)) return;
  const children = node.children;
  for (let i = 0; i < children.length; i++) {
    const child = children[i];
    if (child.type === "image") {
      const next = children[i + 1];
      if (next && next.type === "text") {
        applyImageAttrs(child as Image, next as Text, children, i);
      }
    }
    visit(child);
  }
}

function applyImageAttrs(image: Image, next: Text, siblings: RootContent[], imageIndex: number): void {
  const parsed = parseImageAttrs(next.value);
  if (!parsed) return;
  const hProperties: Record<string, string> = {
    ...(image.data?.hProperties as Record<string, string> | undefined),
  };
  if (parsed.width) hProperties.width = parsed.width;
  if (parsed.height) hProperties.height = parsed.height;
  if (parsed.bg) hProperties.style = `background:${parsed.bg}`;
  image.data = { ...image.data, hProperties };
  const rest = next.value.slice(parsed.consumed);
  if (rest.length === 0) {
    siblings.splice(imageIndex + 1, 1);
  } else {
    next.value = rest;
  }
}

export default function remarkImageAttrs() {
  return (tree: Root) => {
    visit(tree);
  };
}
