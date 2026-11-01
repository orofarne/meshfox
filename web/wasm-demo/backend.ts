// Answers the app's `api/*` requests from the WebAssembly model instead of a
// `meshfox view` server: `window.fetch` and `window.WebSocket` are replaced
// for the page's own `api/...` URLs, everything else passes through. Read-only
// by construction: the info endpoint says so, so the UI offers no editing or
// running, and anything that would need a worker answers 501.
import { VirtualFiles } from "./files";

export interface WasmApi {
  renderCanvas(source: string, path: string, files?: unknown): string;
  version(): string;
}

export interface Document {
  /** The canvas's own path inside `files`. */
  path: string;
  files: VirtualFiles;
  /** Shown beside the "in your browser" pill, e.g. what was not read. */
  note?: string;
}

const SESSION = "wasm-demo";
const FILE_PREVIEW_MAX_BYTES = 1_000_000;

const MIME: Record<string, string> = {
  png: "image/png",
  jpg: "image/jpeg",
  jpeg: "image/jpeg",
  gif: "image/gif",
  webp: "image/webp",
  svg: "image/svg+xml",
  pdf: "application/pdf",
  json: "application/json",
  txt: "text/plain",
  md: "text/markdown",
  csv: "text/csv",
};

const json = (value: unknown, status = 200): Response =>
  new Response(JSON.stringify(value), { status, headers: { "Content-Type": "application/json" } });

const text = (body: string, status = 200): Response =>
  new Response(body, { status, headers: { "Content-Type": "text/plain; charset=utf-8" } });

export type Rendered = { json: string; error?: undefined } | { error: string; json?: undefined };

/** Renders the document once (it never changes) — also how the launcher finds
 * out, before loading the UI, that a document cannot be shown. */
export function createRenderer(wasm: WasmApi, doc: Document): () => Rendered {
  const provider = { read: (path: string) => doc.files.get(path) ?? null };
  let rendered: Rendered | undefined;
  return () => {
    if (rendered === undefined) {
      try {
        const source = doc.files.text(doc.path);
        if (source === undefined) throw new Error(`${doc.path} is not among the opened files`);
        rendered = { json: wasm.renderCanvas(source, doc.path, provider) };
      } catch (e) {
        rendered = { error: e instanceof Error ? e.message : String(e) };
      }
    }
    return rendered;
  };
}

export function installBackend(wasm: WasmApi, doc: Document, render: () => Rendered): void {
  const nativeFetch = window.fetch.bind(window);
  const NativeWebSocket = window.WebSocket;
  const base = new URL(".", document.baseURI).pathname;
  const canvasDir = VirtualFiles.dirname(doc.path);

  const canvas = (): { nodes: Array<{ id: string; target?: string | null }> } | undefined => {
    const r = render();
    return r.json === undefined ? undefined : JSON.parse(r.json);
  };

  /** Where an `api/asset?file=&dir=` request points, inside the opened files. */
  const assetPath = (params: URLSearchParams): string =>
    VirtualFiles.join(params.get("dir") || canvasDir, params.get("file") ?? "");
  const mimeOf = (path: string): string => MIME[path.split(".").pop()?.toLowerCase() ?? ""] ?? "application/octet-stream";

  const handle = async (route: string, params: URLSearchParams, method: string): Promise<Response> => {
    if (method === "GET") {
      switch (route) {
        case "ping":
          return text("ok");
        case "info":
          // `remote`: nothing here can open a file on "the server's machine".
          return json({ readOnly: true, noExec: true, remote: true, serverless: true, version: wasm.version() });
        case "canvas": {
          const r = render();
          if (r.json === undefined) return text(r.error, 422);
          return json({ ...JSON.parse(r.json), canvasVersion: 1, serverSession: SESSION });
        }
        case "includes":
        case "services":
        case "runs":
        case "syntax":
        case "vars":
          return json([]);
        case "history":
          return json({ cursor: 0, canUndo: false, canRedo: false, entries: [] });
        case "link-preview":
          return json({ preview: null });
        case "asset": {
          const path = assetPath(params);
          const data = doc.files.get(path);
          if (data !== undefined) {
            return new Response(data as BodyInit, { headers: { "Content-Type": mimeOf(path) } });
          }
          const url = doc.files.urlOf(path);
          return url !== undefined ? nativeFetch(url) : text(`${path}: not found`, 404);
        }
      }
      const fileContent = /^nodes\/([^/]+)\/file-content$/.exec(route);
      if (fileContent) {
        const id = decodeURIComponent(fileContent[1]);
        const node = canvas()?.nodes.find((n) => n.id === id);
        if (!node?.target) return text(`no file node ${id}`, 404);
        const path = VirtualFiles.join(canvasDir, node.target.split("#")[0]);
        const data = doc.files.get(path);
        if (data === undefined) return text(`${path}: not among the opened files`, 404);
        const truncated = data.length > FILE_PREVIEW_MAX_BYTES;
        const content = new TextDecoder().decode(truncated ? data.slice(0, FILE_PREVIEW_MAX_BYTES) : data);
        return json({ content, truncated });
      }
    }
    return text("not available in the serverless viewer (read-only, nothing runs here)", 501);
  };

  // `fetch` is only half of it: an `<img src="api/asset?...">` is loaded by the
  // browser itself, straight from the network, the moment its `src` is set.
  // So the swap has to happen *before* the browser sees the URL: `setAttribute`
  // and `img.src` are wrapped to hand it a blob URL (or a bundled asset's own
  // URL) built from the same files. A MutationObserver catches whatever slips
  // past (markup assigned through `innerHTML`).
  const ASSET_ATTRS = ["src", "href", "poster", "data"];
  const assetUrl = (value: string | null): URL | undefined => {
    if (!value || value.startsWith("blob:") || value.startsWith("data:")) return undefined;
    try {
      const url = new URL(value, document.baseURI);
      return url.origin === location.origin && url.pathname === `${base}api/asset` ? url : undefined;
    } catch {
      return undefined;
    }
  };
  const swapped = new Map<string, string>();
  const assetNow = (value: string | null): string | undefined => {
    const url = assetUrl(value);
    if (!url) return undefined;
    const known = swapped.get(url.search);
    if (known) return known;
    const path = assetPath(url.searchParams);
    const data = doc.files.get(path);
    const out =
      data !== undefined
        ? URL.createObjectURL(new Blob([data as BlobPart], { type: mimeOf(path) }))
        : doc.files.urlOf(path);
    if (out) swapped.set(url.search, out);
    return out;
  };
  const nativeSetAttribute = Element.prototype.setAttribute;
  Element.prototype.setAttribute = function (name: string, value: string) {
    return nativeSetAttribute.call(this, name, ASSET_ATTRS.includes(name.toLowerCase()) ? (assetNow(value) ?? value) : value);
  };
  const srcProperty = Object.getOwnPropertyDescriptor(HTMLImageElement.prototype, "src");
  if (srcProperty?.set) {
    Object.defineProperty(HTMLImageElement.prototype, "src", {
      ...srcProperty,
      set(value: string) {
        srcProperty.set!.call(this, assetNow(String(value)) ?? value);
      },
    });
  }
  const fixElement = (element: Element) => {
    for (const attr of ASSET_ATTRS) {
      const now = assetNow(element.getAttribute(attr));
      if (now) nativeSetAttribute.call(element, attr, now);
    }
  };
  const sweep = (root: ParentNode) => {
    if (root instanceof Element) fixElement(root);
    root.querySelectorAll(ASSET_ATTRS.map((a) => `[${a}*="api/asset"]`).join(",")).forEach(fixElement);
  };
  sweep(document);
  new MutationObserver((records) => {
    for (const record of records) {
      if (record.type === "attributes" && record.target instanceof Element) fixElement(record.target);
      record.addedNodes.forEach((node) => node instanceof Element && sweep(node));
    }
  }).observe(document, { subtree: true, childList: true, attributes: true, attributeFilter: ASSET_ATTRS });

  window.fetch = (input: RequestInfo | URL, init?: RequestInit): Promise<Response> => {
    const href = typeof input === "string" ? input : input instanceof URL ? input.href : input.url;
    const url = new URL(href, document.baseURI);
    if (url.origin !== location.origin || !url.pathname.startsWith(`${base}api/`)) {
      return nativeFetch(input, init);
    }
    const method = (init?.method ?? (input instanceof Request ? input.method : "GET")).toUpperCase();
    return handle(url.pathname.slice(base.length + "api/".length), url.searchParams, method);
  };

  // `api/watch` is the live-update channel: nothing ever changes here, so it
  // opens, says hello, and sends the heartbeats the page expects. The run and
  // terminal sockets can't exist without a worker: they fail like a refused
  // connection.
  class FakeSocket extends EventTarget {
    static readonly CONNECTING = 0;
    static readonly OPEN = 1;
    static readonly CLOSING = 2;
    static readonly CLOSED = 3;
    readyState = 0;
    onopen: ((ev: Event) => void) | null = null;
    onmessage: ((ev: MessageEvent) => void) | null = null;
    onclose: ((ev: CloseEvent) => void) | null = null;
    onerror: ((ev: Event) => void) | null = null;
    private timer: ReturnType<typeof setInterval> | undefined;

    constructor(readonly url: string, private readonly live: boolean) {
      super();
      setTimeout(() => (live ? this.open() : this.fail()), 0);
    }

    private emit(data: unknown) {
      this.onmessage?.(new MessageEvent("message", { data: JSON.stringify(data) }));
    }

    private open() {
      this.readyState = 1;
      this.onopen?.(new Event("open"));
      this.emit({ type: "connected", seq: 0 });
      this.timer = setInterval(() => this.emit({ type: "heartbeat" }), 10_000);
    }

    private fail() {
      this.readyState = 3;
      this.onerror?.(new Event("error"));
      this.onclose?.(new CloseEvent("close", { code: 1006 }));
    }

    send() {}

    close() {
      if (this.readyState === 3) return;
      clearInterval(this.timer);
      this.readyState = 3;
      this.onclose?.(new CloseEvent("close", { code: 1000 }));
    }
  }

  window.WebSocket = function (url: string | URL, protocols?: string | string[]) {
    const href = String(url);
    const path = new URL(href, document.baseURI).pathname;
    if (path.startsWith(`${base}api/`)) {
      return new FakeSocket(href, path === `${base}api/watch`) as unknown as WebSocket;
    }
    return new NativeWebSocket(url, protocols);
  } as unknown as typeof WebSocket;
  Object.assign(window.WebSocket, { CONNECTING: 0, OPEN: 1, CLOSING: 2, CLOSED: 3 });
}
