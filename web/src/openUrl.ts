/**
 * Where an `open_canvas` action points, without the fragment. Behind the
 * gateway that is the worker's relative `url`, resolved against this page
 * (so it survives any outer path prefix); otherwise the target worker's own
 * port on the host this page was loaded from.
 */
export function canvasOpenUrl(
  action: { url: string | null; port: number | null },
  location: { protocol: string; hostname: string },
  baseURI: string,
): string {
  if (action.url !== null) return new URL(action.url, baseURI).href;
  return `${location.protocol}//${location.hostname}:${action.port}/`;
}

/**
 * Whether a `file` node's target can be opened by the server (the OS default
 * application, the file manager). Always, except through the gateway, where
 * only a canvas can be opened: it is another page of the same gateway.
 */
export function canOpenOnServer(target: string | undefined, remote: boolean): boolean {
  if (!remote) return true;
  const path = (target ?? "").split("#")[0];
  return path.endsWith(".canvas.md");
}
