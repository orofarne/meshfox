// Answers the app's `api/*` requests from the WebAssembly model instead of a
// `meshfox view` server: `window.fetch` and `window.WebSocket` are replaced
// for the page's own `api/...` URLs, everything else passes through. Read-only
// by construction: the info endpoint says so, so the UI offers no editing or
// running, and anything that would need a worker answers 501.
import { VirtualFiles } from "./files";
import { EditSession, type UndoWasm } from "./session";

export interface WasmApi extends UndoWasm {
  renderCanvasWithWarnings(source: string, path: string, files?: unknown): string;
  applyNodeUpdate(raw: string, id: string, requestJson: string): string;
  applyEdit(raw: string, op: string, argsJson: string): string;
  applyBlockAttrs(raw: string, id: string, blockName: string, requestJson: string): string;
  version(): string;
}

export interface Document {
  /** The canvas's own path inside `files`. */
  path: string;
  files: VirtualFiles;
  /** Tells this document from others opened at the same address (the picked
   * folder's name and the canvas's path): scopes what the page remembers. */
  key?: string;
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

/** An `include` the opened files do not hold: its node shows a notice. */
export interface IncludeWarning {
  nodeId: string;
  target: string;
  reason: string;
}

export type Rendered =
  | { json: string; warnings: IncludeWarning[]; error?: undefined }
  | { error: string; json?: undefined; warnings?: undefined };

/** Renders the document once (it never changes) — also how the launcher finds
 * out, before loading the UI, that a document cannot be shown. */
export type Renderer = (() => Rendered) & {
  /** Forget the result: the files changed, render again on the next call. */
  reset(): void;
};

export function createRenderer(wasm: WasmApi, doc: Document): Renderer {
  const provider = { read: (path: string) => doc.files.get(path) ?? null };
  let rendered: Rendered | undefined;
  const render = () => {
    if (rendered === undefined) {
      try {
        const source = doc.files.text(doc.path);
        if (source === undefined) throw new Error(`${doc.path} is not among the opened files`);
        // Lenient: someone who opened a single file has not got its includes,
        // and should still see the document, with a notice where they go.
        const out = JSON.parse(wasm.renderCanvasWithWarnings(source, doc.path, provider));
        rendered = { json: JSON.stringify(out.canvas), warnings: out.warnings };
      } catch (e) {
        rendered = { error: e instanceof Error ? e.message : String(e) };
      }
    }
    return rendered;
  };
  return Object.assign(render, {
    reset() {
      rendered = undefined;
    },
  });
}

export interface Backend {
  /** The text being edited and its undo history. */
  session: EditSession;
  /** The document as rendered now. */
  current(): Rendered;
  /** The files changed: render again and tell the page, which reloads the
   * canvas as it does when a file changes under a real server. */
  refresh(): Rendered;
}

export function installBackend(wasm: WasmApi, doc: Document, render: Renderer): Backend {
  const nativeFetch = window.fetch.bind(window);
  const NativeWebSocket = window.WebSocket;
  const base = new URL(".", document.baseURI).pathname;
  const canvasDir = VirtualFiles.dirname(doc.path);
  // Bumped by every `refresh`: the page compares it to know the canvas moved on.
  let version = 1;
  let seq = 0;
  const watchers = new Set<{ push(event: unknown): void }>();
  const session = new EditSession(wasm, doc.files.text(doc.path) ?? "");

  /** Tells every open page what a server would after a save: the undo state,
   * and (when the document moved) that it changed. */
  const announce = (changed: boolean) => {
    version++;
    for (const socket of watchers) {
      socket.push({ type: "undo-state-changed", canUndo: session.canUndo, canRedo: session.canRedo, seq: ++seq });
      if (changed) socket.push({ type: "changed", seq: ++seq });
    }
  };
  /** Makes the session's text the document: the file the renderer reads, then
   * a fresh render. */
  const show = (): Rendered => {
    doc.files.set(doc.path, session.raw);
    render.reset();
    return render();
  };
  /** A new text for the document, if it renders; otherwise nothing changes and
   * the reason comes back. */
  const commit = (raw: string, opKind: string, quiet = false): string | undefined => {
    const before = session.raw;
    doc.files.set(doc.path, raw);
    render.reset();
    const result = render();
    if (result.error !== undefined) {
      doc.files.set(doc.path, before);
      render.reset();
      return result.error;
    }
    // `quiet`: the page that made the edit already shows it (a layout save) —
    // only the undo state needs telling, not a reload.
    if (session.edit(raw, opKind)) announce(!quiet);
    return undefined;
  };
  const canvasObject = (extra: Record<string, unknown> = {}): Record<string, unknown> | string => {
    const r = render();
    if (r.json === undefined) return r.error;
    return { ...JSON.parse(r.json), canvasVersion: version, serverSession: SESSION, documentKey: doc.key, ...extra };
  };
  const canvasResponse = (extra: Record<string, unknown> = {}): Response => {
    const c = canvasObject(extra);
    return typeof c === "string" ? text(c, 422) : json(c);
  };
  /** One of the document edits (`applyEdit`): applied and recorded, or the
   * server's own refusal. */
  const edit = (
    op: string,
    args: unknown,
    kind: string,
    quiet = false,
  ): { response: Response } | { out: { raw: string; newId?: string } } => {
    const out = JSON.parse(wasm.applyEdit(session.raw, op, JSON.stringify(args))) as {
      raw?: string;
      newId?: string;
      error?: { kind: string; message: string };
    };
    if (out.error) return { response: text(out.error.message, out.error.kind === "notFound" ? 404 : 422) };
    const problem = commit(out.raw!, kind, quiet);
    if (problem !== undefined) return { response: text(problem, 422) };
    return { out: { raw: out.raw!, newId: out.newId } };
  };
  const stepResult = (changed: boolean): Response =>
    canvasResponse({ changed, canUndo: session.canUndo, canRedo: session.canRedo });

  const canvas = (): { nodes: Array<{ id: string; target?: string | null }> } | undefined => {
    const r = render();
    return r.json === undefined ? undefined : JSON.parse(r.json);
  };

  /** Where an `api/asset?file=&dir=` request points, inside the opened files. */
  const assetPath = (params: URLSearchParams): string =>
    VirtualFiles.join(params.get("dir") || canvasDir, params.get("file") ?? "");
  const mimeOf = (path: string): string => MIME[path.split(".").pop()?.toLowerCase() ?? ""] ?? "application/octet-stream";

  const handle = async (
    route: string,
    params: URLSearchParams,
    method: string,
    body: string | undefined,
    headers: Headers,
  ): Promise<Response> => {
    if (method === "GET") {
      switch (route) {
        case "ping":
          return text("ok");
        case "info":
          // `remote`: nothing here can open a file on "the server's machine".
          return json({ readOnly: false, noExec: true, remote: true, serverless: true, version: wasm.version() });
        case "canvas":
          return canvasResponse();
        case "canvas/raw": {
          const include = params.get("include");
          if (include === null) {
            return new Response(session.raw, {
              headers: { "Content-Type": "text/plain; charset=utf-8", ETag: session.rev, "x-meshfox-read-only": "false" },
            });
          }
          // An included file: shown, not editable here yet.
          const node = canvas()?.nodes.find((n) => n.id === include);
          const target = node?.target;
          const path = target ? VirtualFiles.join(canvasDir, target.split("#")[0]) : undefined;
          const data = path === undefined ? undefined : doc.files.get(path);
          if (data === undefined) return text(`no readable include ${include}`, 404);
          return new Response(data as BodyInit, {
            headers: { "Content-Type": "text/plain; charset=utf-8", ETag: '"mfx-include"', "x-meshfox-read-only": "true" },
          });
        }
        case "includes":
        case "services":
        case "runs":
        case "syntax":
        case "vars":
          return json([]);
        case "history": {
          const limit = Number(params.get("limit") ?? 50);
          return json({
            cursor: session.seq,
            canUndo: session.canUndo,
            canRedo: session.canRedo,
            entries: session.history(Number.isFinite(limit) ? limit : 50),
          });
        }
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
    if (method === "PUT" && route === "canvas/raw") {
      if (params.get("include") !== null) {
        return text("an included file cannot be edited in the browser yet", 403);
      }
      if (!session.matches(headers.get("if-match"))) {
        return json(
          {
            error: "sourceConflict",
            message: "the document changed since it was read — reread, then retry with the current ETag",
            currentText: session.raw,
            currentRev: session.rev,
          },
          412,
        );
      }
      const problem = commit(body ?? "", "source");
      if (problem !== undefined) return text(problem, 422);
      return new Response(null, { status: 204, headers: { ETag: session.rev } });
    }
    const nodeRoute = /^nodes\/([^/]+)$/.exec(route);
    const blockRoute = /^nodes\/([^/]+)\/block\/([^/]+)$/.exec(route);
    if (method === "PATCH" && blockRoute) {
      const out = JSON.parse(
        wasm.applyBlockAttrs(
          session.raw,
          decodeURIComponent(blockRoute[1]),
          decodeURIComponent(blockRoute[2]),
          body ?? "{}",
        ),
      ) as { raw?: string; error?: { kind: string; message: string } };
      if (out.error) return text(out.error.message, out.error.kind === "notFound" ? 404 : 422);
      const problem = commit(out.raw!, "node_upserted");
      if (problem !== undefined) return text(problem, 422);
      return canvasResponse();
    }
    if (method === "PATCH" && nodeRoute) {
      // The same edit `PATCH /api/nodes/:id` makes (`meshfox_model::edit`).
      const id = decodeURIComponent(nodeRoute[1]);
      const out = JSON.parse(wasm.applyNodeUpdate(session.raw, id, body ?? "{}")) as {
        raw?: string;
        conflict?: { currentText: string; currentRev: string; currentTitle: string };
        error?: { kind: string; message: string };
      };
      if (out.conflict) {
        return json(
          {
            error: "bodyConflict",
            message: "the node's body changed since it was read — reread, then retry with the current revision",
            ...out.conflict,
          },
          409,
        );
      }
      if (out.error) return text(out.error.message, out.error.kind === "notFound" ? 404 : 422);
      const problem = commit(out.raw!, "node_upserted");
      if (problem !== undefined) return text(problem, 422);
      return canvasResponse();
    }
    // The document edits: the same ones `meshfox view`'s endpoints make
    // (`meshfox_model::edit`), recorded in the undo log.
    {
      const json_ = (): Record<string, unknown> => {
        try {
          return JSON.parse(body || "{}");
        } catch {
          return {};
        }
      };
      const idRoute = /^nodes\/([^/]+)\/(reparent|move|rename-id|clear-id|append|clear-layout)$/.exec(route);
      const id = idRoute ? decodeURIComponent(idRoute[1]) : nodeRoute ? decodeURIComponent(nodeRoute[1]) : "";
      let result: { response: Response } | { out: { raw: string; newId?: string } } | undefined;
      let respond: ((out: { raw: string; newId?: string }) => Response) | undefined;
      if (method === "POST" && route === "nodes") {
        result = edit("create", json_(), "node_created");
        respond = (out) => canvasResponse({ newId: out.newId });
      } else if (method === "DELETE" && nodeRoute) {
        result = edit("remove", { id, reparent: params.get("children") === "reparent" }, "node_removed");
        respond = () => canvasResponse();
      } else if (method === "POST" && idRoute?.[2] === "reparent") {
        result = edit("reparent", { id, ...json_() }, "node_moved");
        respond = () => canvasResponse();
      } else if (method === "POST" && idRoute?.[2] === "move") {
        result = edit("move", { id, ...json_() }, "node_moved");
        respond = () => canvasResponse();
      } else if (method === "POST" && idRoute?.[2] === "rename-id") {
        result = edit("renameId", { id, ...json_() }, "node_id_renamed");
        respond = () => canvasResponse();
      } else if (method === "POST" && idRoute?.[2] === "clear-id") {
        result = edit("clearId", { id }, "node_id_renamed");
        respond = (out) => {
          const c = canvasObject();
          return typeof c === "string" ? text(c, 422) : json({ id: out.newId, canvas: c });
        };
      } else if (method === "POST" && idRoute?.[2] === "append") {
        result = edit("append", { id, text: body ?? "" }, "node_upserted");
        respond = () => new Response(null, { status: 204 });
      } else if (method === "POST" && idRoute?.[2] === "clear-layout") {
        result = edit("clearNodeLayout", { id }, "layout_cleared");
        respond = () => canvasResponse();
      } else if (method === "PUT" && route === "options") {
        result = edit("options", json_(), "options_changed");
        respond = () => canvasResponse();
      } else if (method === "PUT" && route === "canvas") {
        // A drag or a resize: the page already shows it.
        result = edit("layout", json_(), "layout", true);
        respond = () => new Response(null, { status: 204 });
      } else if (method === "POST" && route === "canvas/clear-layout") {
        result = edit("clearLayout", {}, "layout_cleared");
        respond = () => canvasResponse();
      } else if (method === "POST" && route === "canvas/reorder-siblings") {
        result = edit("reorder", {}, "node_moved");
        respond = () => canvasResponse();
      }
      if (result !== undefined) return "response" in result ? result.response : respond!(result.out);
    }
    if (method === "POST") {
      switch (route) {
        case "undo":
        case "redo":
        case "history/goto": {
          let changed: boolean;
          try {
            changed =
              route === "undo"
                ? session.undo()
                : route === "redo"
                  ? session.redo()
                  : session.goto(Number(JSON.parse(body ?? "{}").seq));
          } catch (e) {
            // A step that no longer fits the document: nothing moved.
            return text(e instanceof Error ? e.message : String(e), 422);
          }
          if (changed) {
            show();
            announce(true);
          }
          return stepResult(changed);
        }
      }
    }
    return text(
      method === "GET"
        ? "not available in the browser (nothing runs here)"
        : "this edit is not available in the browser editor yet — Source mode can change anything",
      501,
    );
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
  let swapped = new Map<string, string>();
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
    const headers = new Headers(init?.headers ?? (input instanceof Request ? input.headers : undefined));
    const body = typeof init?.body === "string" ? init.body : undefined;
    return handle(url.pathname.slice(base.length + "api/".length), url.searchParams, method, body, headers);
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
      watchers.add(this);
    }

    push(event: unknown) {
      this.emit(event);
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
      watchers.delete(this);
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

  return {
    session,
    current: () => render(),
    refresh() {
      render.reset();
      swapped = new Map(); // blob URLs of the old bytes
      version++;
      const result = render();
      // The same event a real server sends when the file changes under it.
      for (const socket of watchers) socket.push({ type: "changed", seq: ++seq });
      return result;
    },
  };
}
