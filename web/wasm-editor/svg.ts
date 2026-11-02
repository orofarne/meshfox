// An SVG drawn by `<img>` runs nothing — but its blob URL is a page of the
// editor's own origin, and "Open image in new tab" (or dragging the image to
// the tab bar) navigates to exactly that, where the browser *does* run its
// scripts, with the editor's storage in reach. The Content-Security-Policy does
// not follow a blob into a top-level page, so the picture itself is cleaned
// before it gets a URL: whatever could execute is dropped, the drawing is not.

const REMOVE = new Set(["script", "iframe", "object", "embed", "frame", "frameset", "applet", "meta", "base", "link"]);
const URL_ATTRS = new Set(["href", "xlink:href", "src", "action", "formaction", "data", "to", "values", "from", "by"]);
const ACTIVE_URL = /^\s*(javascript|vbscript|data\s*:\s*text\/html)/i;

function clean(element: Element): void {
  for (const child of Array.from(element.children)) {
    if (REMOVE.has(child.localName.toLowerCase())) {
      child.remove();
      continue;
    }
    clean(child);
  }
  for (const attr of Array.from(element.attributes)) {
    const name = attr.name.toLowerCase();
    if (name.startsWith("on") || (URL_ATTRS.has(name) && ACTIVE_URL.test(attr.value))) {
      element.removeAttribute(attr.name);
    }
  }
}

/** The SVG with nothing in it that a browser would run. A file that is not
 * well-formed XML is not a picture a browser draws either: it comes back empty. */
export function cleanSvg(bytes: Uint8Array): Uint8Array {
  const text = new TextDecoder().decode(bytes);
  const doc = new DOMParser().parseFromString(text, "image/svg+xml");
  const root = doc.documentElement;
  if (doc.querySelector("parsererror") || root.localName.toLowerCase() !== "svg") {
    return new TextEncoder().encode('<svg xmlns="http://www.w3.org/2000/svg"/>');
  }
  clean(root);
  return new TextEncoder().encode(new XMLSerializer().serializeToString(root));
}
