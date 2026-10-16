/** Invalidate only worker-served Markdown images, preserving URL structure. */
export function refreshImageUrl(src: string | undefined, version: number, base: string): string | undefined {
  if (!src || version === 0 || /^(?:[a-z][a-z0-9+.-]*:|\/\/|#)/i.test(src)) return src;
  const url = new URL(src, base);
  url.searchParams.set("meshfox-image-version", String(version));
  return url.pathname + url.search + url.hash;
}
