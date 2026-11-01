/** Where a document's folded set is remembered (`localStorage`). A root id
 * alone is no identity — the default root id is `root` in every document — so
 * the key also carries what tells documents apart: the page's own path
 * (`basePath`; a gateway serves each canvas under `/c/<slug>/`) and, where one
 * address serves any file (the in-browser build), the document's own key. A
 * plain `meshfox view` page at `/` keeps the original key, so nothing already
 * saved is lost. */
export function foldedStorageKey(rootId: string, documentKey: string | undefined, basePath: string): string {
  const scope = `${basePath === "/" ? "" : basePath}${documentKey ?? ""}`;
  return scope ? `meshfox-folded:${scope}:${rootId}` : `meshfox-folded:${rootId}`;
}
