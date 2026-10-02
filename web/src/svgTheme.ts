// Client-side twin of crates/core/src/svg.rs (`inject_theme`/`theme_css`) —
// read that module's doc comment for the theme contract. An SVG shown
// through `<img>` can't see the page's CSS, so a diagram drawn
// black-on-transparent vanishes on a dark theme. Right before showing a
// `data:image/svg+xml;base64,…` image, `MeshNode.tsx`'s `img` component
// rewrites the SVG's *text* (never stored — the document keeps whatever the
// tool produced) so that:
//
// - `currentColor` resolves to the page's foreground (`svg{color:…}`), and
// - `var(--mf-fg|bg|accent|border[, fallback])` resolve to the page's
//   live colors (a browser resolves `var()` itself from the `:root` custom
//   properties injected here; the Rust side has to substitute them textually
//   because resvg doesn't implement CSS variables at all).
//
// The result is still shown through `<img>`, so scripts inside the SVG stay
// inert — nothing here ever inlines it into the page.

export interface SvgTheme {
  fg: string;
  bg: string;
  accent: string;
  border: string;
}

/** The CSS written into the SVG — keep in step with `svg::theme_css`. */
export function themeCss(theme: SvgTheme): string {
  return (
    `svg{color:${theme.fg}}` +
    `:root{--mf-fg:${theme.fg};--mf-bg:${theme.bg};--mf-accent:${theme.accent};--mf-border:${theme.border}}`
  );
}

function isTagBoundary(ch: string | undefined): boolean {
  return ch !== undefined && (ch === ">" || ch === "/" || /\s/.test(ch));
}

/** Index just past the `>` closing the root `<svg ...>` start tag
 * (quote-aware, comments skipped), or `null` if there is none, it's
 * unterminated, or it's self-closing. Mirrors `svg::root_tag_end`. */
function rootTagEnd(svg: string): number | null {
  let i = 0;
  let start = -1;
  while (i < svg.length) {
    if (svg.startsWith("<!--", i)) {
      const end = svg.indexOf("-->", i);
      if (end === -1) return null;
      i = end + 3;
      continue;
    }
    if (svg.startsWith("<svg", i) && isTagBoundary(svg[i + 4])) {
      start = i;
      break;
    }
    i++;
  }
  if (start === -1) return null;
  let quote: string | null = null;
  for (let j = start + 4; j < svg.length; j++) {
    const ch = svg[j];
    if (quote !== null) {
      if (ch === quote) quote = null;
    } else if (ch === '"' || ch === "'") {
      quote = ch;
    } else if (ch === ">") {
      return svg[j - 1] === "/" ? null : j + 1;
    }
  }
  return null;
}

/** Inserts a `<style>` right after the root `<svg>` start tag — before the
 * tool's own styles, so a tool's own rule for the same selector still wins.
 * Unchanged if there's no such tag. Mirrors `svg::inject_theme`. */
export function injectTheme(svg: string, theme: SvgTheme): string {
  const at = rootTagEnd(svg);
  if (at === null) return svg;
  return `${svg.slice(0, at)}<style>${themeCss(theme)}</style>${svg.slice(at)}`;
}

const SVG_BASE64_PREFIX = "data:image/svg+xml;base64,";

function decodeBase64Utf8(b64: string): string | null {
  try {
    const bin = atob(b64);
    const bytes = Uint8Array.from(bin, (c) => c.charCodeAt(0));
    return new TextDecoder("utf-8", { fatal: true }).decode(bytes);
  } catch {
    return null;
  }
}

export function encodeBase64Utf8(text: string): string {
  const bytes = new TextEncoder().encode(text);
  let bin = "";
  // Chunked: `String.fromCharCode(...bytes)` overflows the argument limit
  // on a large diagram.
  for (let i = 0; i < bytes.length; i += 0x8000) {
    bin += String.fromCharCode(...bytes.subarray(i, i + 0x8000));
  }
  return btoa(bin);
}

/** For a `data:image/svg+xml;base64,…` URL, the same URL with the theme
 * injected into the SVG; any other `src` (or one that fails to decode) is
 * returned untouched. */
export function themeSvgDataUrl(src: string, theme: SvgTheme): string {
  if (!src.startsWith(SVG_BASE64_PREFIX)) return src;
  const svg = decodeBase64Utf8(src.slice(SVG_BASE64_PREFIX.length));
  if (svg === null) return src;
  const themed = injectTheme(svg, theme);
  if (themed === svg) return src;
  return SVG_BASE64_PREFIX + encodeBase64Utf8(themed);
}

/** The page's live colors, read from the CSS custom properties index.css
 * defines per theme (so it follows the OS preference and the toolbar's
 * manual `data-theme` override alike). Browser-only — call it at render
 * time, never at import time. */
export function readPageTheme(): SvgTheme {
  const css = getComputedStyle(document.documentElement);
  const get = (name: string, fallback: string) => css.getPropertyValue(name).trim() || fallback;
  return {
    fg: get("--fg", "#201a14"),
    bg: get("--node-bg", "#ffffff"),
    accent: get("--accent", "#ea580c"),
    border: get("--node-border", "#e5ddd2"),
  };
}
