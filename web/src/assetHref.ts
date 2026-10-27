/** Preserve relative paths (especially `..`) until the worker checks FileAccess. */
export function resolveAssetHref(href: string | undefined, assetBase: string | undefined): string | undefined {
  if (!href) return href;
  if (/^[a-z][a-z0-9+.-]*:/i.test(href) || href.startsWith("/") || href.startsWith("#") || href.startsWith("?")) return href;
  const fragmentAt = href.indexOf("#");
  const fragment = fragmentAt < 0 ? "" : href.slice(fragmentAt);
  const file = fragmentAt < 0 ? href : href.slice(0, fragmentAt);
  const dir = assetBase ? `&dir=${encodeURIComponent(assetBase)}` : "";
  return `api/asset?file=${encodeURIComponent(file)}${dir}${fragment}`;
}

