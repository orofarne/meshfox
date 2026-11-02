// Fetches the WebAssembly engine with progress. The module is the heaviest
// thing on a cold start, so the launcher starts it the moment the page loads
// (while the person is still choosing a file) and shows how far it has got.
//
// The bytes still go through `WebAssembly.instantiateStreaming`: this only
// wraps the response body in a counting stream, and hands the browser a
// `Response` that looks like the original (same `application/wasm` type).

export interface EngineProgress {
  /** Bytes received so far. */
  loaded: number;
  /** Size announced by the server; `undefined` when it did not say. */
  total?: number;
  done: boolean;
}

export type OnProgress = (p: EngineProgress) => void;

/** `fetch`es `url` like `fetch` does, reporting the body as it arrives. */
export async function fetchWithProgress(url: URL | string, onProgress: OnProgress): Promise<Response> {
  const response = await fetch(url);
  const announced = Number(response.headers.get("Content-Length"));
  const total = Number.isFinite(announced) && announced > 0 ? announced : undefined;
  if (!response.ok || !response.body) return response;

  let loaded = 0;
  const counted = response.body.pipeThrough(
    new TransformStream<Uint8Array, Uint8Array>({
      transform(chunk, controller) {
        loaded += chunk.byteLength;
        onProgress({ loaded, total, done: false });
        controller.enqueue(chunk);
      },
      flush() {
        onProgress({ loaded, total: total ?? loaded, done: true });
      },
    }),
  );
  return new Response(counted, {
    status: response.status,
    statusText: response.statusText,
    headers: response.headers,
  });
}

/** "2.1 of 4.2 MB", or "2.1 MB" when the total is not known. With a
 * compressing server `Content-Length` counts the compressed body while the
 * stream counts the decoded one, so the figure is never allowed past the total. */
export function describeProgress(p: EngineProgress): string {
  const mb = (n: number) => (n / 1_048_576).toFixed(1);
  if (p.total === undefined) return `${mb(p.loaded)} MB`;
  return `${mb(Math.min(p.loaded, p.total))} of ${mb(p.total)} MB`;
}

/** 0..1 for a progress bar, or `undefined` when it cannot be known. */
export function fraction(p: EngineProgress): number | undefined {
  if (p.done) return 1;
  if (p.total === undefined) return undefined;
  return Math.min(p.loaded / p.total, 0.99);
}
