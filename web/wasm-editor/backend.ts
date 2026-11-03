// Answers the app's `api/*` requests from the WebAssembly model instead of a
// `meshfox view` server: `window.fetch` and `window.WebSocket` are replaced
// for the page's own `api/...` URLs, everything else passes through. Read-only
// by construction: the info endpoint says so, so the UI offers no editing or
// running, and anything that would need a worker answers 501.
import { VirtualFiles } from "./files";
import { cleanSvg } from "./svg";
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
  /** The text was never saved anywhere (a recovered draft): unsaved from the start. */
  unsaved?: boolean;
  /** The key of another canvas of the same files, for `open`. */
  keyFor?: (path: string) => string;
  /** Shown beside the "in your browser" pill, e.g. what was not read. */
  note?: string;
  /** Set when the files came out of a packed canvas (`.canvas.tar.xz`): the
   * name of the file that was opened and the canvas it opens at. "download"
   * then hands back a new pack instead of the canvas text. */
  pack?: { name: string; entry: string };
}

const SESSION = "wasm-editor";
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

/** What a link may open as its own page: pictures and PDFs, which a browser
 * shows without running anything. Every other type (SVG above all) is text. */
const NAVIGABLE = new Set(["image/png", "image/jpeg", "image/gif", "image/webp", "application/pdf"]);
/** The address a `fetch` call names, whichever way it was given. */
function requestHref(input: RequestInfo | URL): string {
  if (typeof input === "string") return input;
  return input instanceof URL ? input.href : input.url;
}

export const navigableType = (type: string): string => (NAVIGABLE.has(type) ? type : "text/plain; charset=utf-8");

/** Why the page cannot take over `fetch`, `WebSocket` and image loading here,
 * or `undefined` when it can. Some browsers and privacy tools lock those down
 * (iOS content blockers lock `img.src`); trying anyway fails half-way with a
 * message nobody can act on, so it is checked, and said, before anything starts. */
export function unsupportedBrowser(): string | undefined {
  const locked = (target: object, name: string, writable = false) => {
    const d = Object.getOwnPropertyDescriptor(target, name);
    return d !== undefined && !d.configurable && (!writable || !d.writable);
  };
  const what = [
    locked(HTMLImageElement.prototype, "src") && "image loading",
    (locked(window, "fetch", true) || locked(window, "WebSocket", true)) && "network requests",
    locked(Element.prototype, "setAttribute", true) && "page attributes",
  ].filter(Boolean);
  if (what.length === 0) return undefined;
  return `This browser does not let the editor take over ${what.join(" and ")} (a content blocker or privacy setting may be locking them). Try another browser, or turn the blocker off for this site.`;
}

export interface Backend {
  /** The text being edited and its undo history (of the canvas now shown). */
  readonly session: EditSession;
  /** Counts the changes of what is shown; a canvas response carries it as `canvasVersion`. */
  readonly version: number;
  /** Whether any canvas opened so far has edits that were never saved. */
  readonly dirty: boolean;
  /** Makes another canvas of the opened files the one shown, the way a
   * server's "open" of a canvas node does in a new tab. The reason it cannot
   * be shown, or `undefined`. */
  open(path: string): string | undefined;
  /** What a click on a canvas node's "↗ open" does; set by the page, which
   * owns the browser history. Without it, nothing opens. */
  navigate?: (path: string, fragment: string | null) => string | undefined;
  /** The document as rendered now. */
  current(): Rendered;
  /** Makes `raw` the document, as one undoable step (a recovered draft). The
   * reason it does not render, or `undefined`. */
  replace(raw: string, opKind: string): string | undefined;
  /** The files changed: render again and tell the page, which reloads the
   * canvas as it does when a file changes under a real server. */
  refresh(): Rendered;
  /** Marks every canvas opened so far as saved (a pack holds them all). */
  markAllSaved(): void;
}

export function installBackend(wasm: WasmApi, doc: Document, render: Renderer): Backend {
  const nativeFetch = window.fetch.bind(window);
  const NativeWebSocket = window.WebSocket;
  const base = new URL(".", document.baseURI).pathname;
  const canvasDir = () => VirtualFiles.dirname(doc.path);
  // Bumped by every `refresh`: the page compares it to know the canvas moved on.
  let version = 1;
  let seq = 0;
  const watchers = new Set<{ push(event: unknown): void }>();
  // One session per canvas opened so far: going back to a canvas finds its
  // edits and its undo history as they were left.
  const sessions = new Map<string, EditSession>();
  let session = new EditSession(wasm, doc.files.text(doc.path) ?? "", doc.unsaved);
  sessions.set(doc.path, session);

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
    VirtualFiles.join(params.get("dir") || canvasDir(), params.get("file") ?? "");
  const mimeOf = (path: string): string => MIME[path.split(".").pop()?.toLowerCase() ?? ""] ?? "application/octet-stream";

  type Handled = Response | Promise<Response> | undefined;
  type EditOut = { raw: string; newId?: string };

  /** `GET api/<route>`: the document, its files and the empty lists. */
  const handleGet = (route: string, params: URLSearchParams): Handled => {
    switch (route) {
      case "ping":
        return text("ok");
      case "info":
        // `remote`: nothing here can open a file on "the server's machine".
        return json({ readOnly: false, noExec: true, remote: true, serverless: true, version: wasm.version() });
      case "canvas":
        return canvasResponse();
      case "canvas/raw":
        return rawResponse(params.get("include"));
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
      case "asset":
        return assetResponse(params);
    }
    const fileContent = /^nodes\/([^/]+)\/file-content$/.exec(route);
    return fileContent ? fileContentResponse(decodeURIComponent(fileContent[1])) : undefined;
  };

  /** The canvas text, or (with `include`) an included file, which is shown but not editable here yet. */
  const rawResponse = (include: string | null): Response => {
    if (include === null) {
      return new Response(session.raw, {
        headers: { "Content-Type": "text/plain; charset=utf-8", ETag: session.rev, "x-meshfox-read-only": "false" },
      });
    }
    const target = canvas()?.nodes.find((n) => n.id === include)?.target;
    const path = target ? VirtualFiles.join(canvasDir(), target.split("#")[0]) : undefined;
    const data = path === undefined ? undefined : doc.files.get(path);
    if (data === undefined) return text(`no readable include ${include}`, 404);
    return new Response(data as BodyInit, {
      headers: { "Content-Type": "text/plain; charset=utf-8", ETag: '"mfx-include"', "x-meshfox-read-only": "true" },
    });
  };

  const assetResponse = (params: URLSearchParams): Response | Promise<Response> => {
    const path = assetPath(params);
    const data = doc.files.get(path);
    if (data !== undefined) return new Response(data as BodyInit, { headers: { "Content-Type": mimeOf(path) } });
    const url = doc.files.urlOf(path);
    return url !== undefined ? nativeFetch(url) : text(`${path}: not found`, 404);
  };

  const fileContentResponse = (id: string): Response => {
    const node = canvas()?.nodes.find((n) => n.id === id);
    if (!node?.target) return text(`no file node ${id}`, 404);
    const path = VirtualFiles.join(canvasDir(), node.target.split("#")[0]);
    const data = doc.files.get(path);
    if (data === undefined) return text(`${path}: not among the opened files`, 404);
    const truncated = data.length > FILE_PREVIEW_MAX_BYTES;
    const content = new TextDecoder().decode(truncated ? data.slice(0, FILE_PREVIEW_MAX_BYTES) : data);
    return json({ content, truncated });
  };

  /** `PUT canvas/raw`: the whole text, from Source mode. */
  const putRaw = (params: URLSearchParams, body: string | undefined, headers: Headers): Response => {
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
  };

  type Applied = {
    raw?: string;
    conflict?: { currentText: string; currentRev: string; currentTitle: string };
    error?: { kind: string; message: string };
  };

  /** What a model edit that returns the new text answers with: the failure, or the committed document. */
  const commitApplied = (out: Applied): Response => {
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
    return problem === undefined ? canvasResponse() : text(problem, 422);
  };

  /** `PATCH nodes/:id` and `PATCH nodes/:id/block/:name`: the edits `meshfox view` makes (`meshfox_model::edit`). */
  const patch = (route: string, body: string | undefined): Handled => {
    const blockRoute = /^nodes\/([^/]+)\/block\/([^/]+)$/.exec(route);
    if (blockRoute) {
      const id = decodeURIComponent(blockRoute[1]);
      const block = decodeURIComponent(blockRoute[2]);
      return commitApplied(JSON.parse(wasm.applyBlockAttrs(session.raw, id, block, body ?? "{}")));
    }
    const nodeRoute = /^nodes\/([^/]+)$/.exec(route);
    if (!nodeRoute) return undefined;
    return commitApplied(JSON.parse(wasm.applyNodeUpdate(session.raw, decodeURIComponent(nodeRoute[1]), body ?? "{}")));
  };

  /** "↗ open" on a file node that is a canvas: another canvas of the opened
   * files, shown in this page (nothing else can be opened from a page). */
  const openCanvas = (route: string): Handled => {
    const openRoute = /^nodes\/([^/]+)\/open$/.exec(route);
    if (!openRoute) return undefined;
    const node = canvas()?.nodes.find((n) => n.id === decodeURIComponent(openRoute[1]));
    if (!node?.target) return text(`no file node ${openRoute[1]}`, 404);
    const [target, fragment] = node.target.split("#");
    if (!target.endsWith(".canvas.md")) return text("only a canvas can be opened in the browser editor", 422);
    const problem = backend.navigate?.(VirtualFiles.join(canvasDir(), target), fragment ?? null);
    return problem === undefined ? json({ action: "done" }) : text(problem, 404);
  };

  type EditSpec = { op: string; args: Record<string, unknown>; kind: string; quiet?: boolean; respond: (out: EditOut) => Response };
  const done = (): Response => canvasResponse();
  const noContent = (): Response => new Response(null, { status: 204 });
  const parsedBody = (body: string | undefined): Record<string, unknown> => {
    try {
      return JSON.parse(body || "{}");
    } catch {
      return {};
    }
  };

  /** Each document edit, by the request that asks for it: the model operation, its arguments, how it is logged and answered. */
  const EDITS: Record<string, (id: string, params: URLSearchParams, body: string | undefined) => EditSpec> = {
    "POST nodes": (_id, _p, body) => ({
      op: "create", args: parsedBody(body), kind: "node_created", respond: (out) => canvasResponse({ newId: out.newId }),
    }),
    "DELETE nodes/:id": (id, params) => ({
      op: "remove", args: { id, reparent: params.get("children") === "reparent" }, kind: "node_removed", respond: done,
    }),
    "POST nodes/:id/reparent": (id, _p, body) => ({ op: "reparent", args: { id, ...parsedBody(body) }, kind: "node_moved", respond: done }),
    "POST nodes/:id/move": (id, _p, body) => ({ op: "move", args: { id, ...parsedBody(body) }, kind: "node_moved", respond: done }),
    "POST nodes/:id/rename-id": (id, _p, body) => ({
      op: "renameId", args: { id, ...parsedBody(body) }, kind: "node_id_renamed", respond: done,
    }),
    "POST nodes/:id/clear-id": (id) => ({
      op: "clearId", args: { id }, kind: "node_id_renamed",
      respond: (out) => {
        const c = canvasObject();
        return typeof c === "string" ? text(c, 422) : json({ id: out.newId, canvas: c });
      },
    }),
    "POST nodes/:id/append": (id, _p, body) => ({
      op: "append", args: { id, text: body ?? "" }, kind: "node_upserted", respond: noContent,
    }),
    "POST nodes/:id/clear-layout": (id) => ({ op: "clearNodeLayout", args: { id }, kind: "layout_cleared", respond: done }),
    "PUT options": (_id, _p, body) => ({ op: "options", args: parsedBody(body), kind: "options_changed", respond: done }),
    // A drag or a resize: the page already shows it.
    "PUT canvas": (_id, _p, body) => ({ op: "layout", args: parsedBody(body), kind: "layout", quiet: true, respond: noContent }),
    "POST canvas/clear-layout": () => ({ op: "clearLayout", args: {}, kind: "layout_cleared", respond: done }),
    "POST canvas/reorder-siblings": () => ({ op: "reorder", args: {}, kind: "node_moved", respond: done }),
  };

  /** The request's shape: `nodes/abc/move` is `nodes/:id/move`, and carries the id. */
  const editKey = (method: string, route: string): { key: string; id: string } => {
    const node = /^nodes\/([^/]+)(\/(?:reparent|move|rename-id|clear-id|append|clear-layout))?$/.exec(route);
    if (!node) return { key: `${method} ${route}`, id: "" };
    return { key: `${method} nodes/:id${node[2] ?? ""}`, id: decodeURIComponent(node[1]) };
  };

  /** The document edits: the same ones `meshfox view`'s endpoints make (`meshfox_model::edit`), recorded in the undo log. */
  const documentEdit = (method: string, route: string, params: URLSearchParams, body: string | undefined): Handled => {
    const { key, id } = editKey(method, route);
    const spec = EDITS[key]?.(id, params, body);
    if (spec === undefined) return undefined;
    const result = edit(spec.op, spec.args, spec.kind, spec.quiet);
    return "response" in result ? result.response : spec.respond(result.out);
  };

  /** `POST undo`, `redo` and `history/goto`. */
  const undoStep = (route: string, body: string | undefined): Handled => {
    if (route !== "undo" && route !== "redo" && route !== "history/goto") return undefined;
    const step = (): boolean => {
      if (route === "undo") return session.undo();
      if (route === "redo") return session.redo();
      return session.goto(Number(JSON.parse(body ?? "{}").seq));
    };
    let changed: boolean;
    try {
      changed = step();
    } catch (e) {
      // A step that no longer fits the document: nothing moved. What the
      // exception says is for whoever debugs this, so it goes to the
      // console, and the answer carries only the fixed sentence.
      console.warn("meshfox editor: an undo/redo step was refused", e);
      return text("that step no longer fits the document", 422);
    }
    if (changed) {
      show();
      announce(true);
    }
    return stepResult(changed);
  };

  const handle = async (
    route: string,
    params: URLSearchParams,
    method: string,
    body: string | undefined,
    headers: Headers,
  ): Promise<Response> => {
    let answer: Handled;
    if (method === "GET") answer = handleGet(route, params);
    else if (method === "PUT" && route === "canvas/raw") answer = putRaw(params, body, headers);
    else if (method === "PATCH") answer = patch(route, body);
    else if (method === "POST") answer = openCanvas(route) ?? documentEdit(method, route, params, body) ?? undoStep(route, body);
    else answer = documentEdit(method, route, params, body);
    if (answer !== undefined) return answer;
    return text(
      method === "GET"
        ? "this is not supported in the browser editor"
        : "this is not supported in the browser editor yet — Source mode can change anything",
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
  /** `link`: the URL is where a click goes (an `<a href>`), not what the page
   * itself draws. A blob opened as its own page belongs to this page's origin,
   * so anything a browser would run there (an SVG's `<script>`) must not carry
   * its real type: it is shown as text instead. An `<img>` never runs scripts. */
  const assetNow = (value: string | null, link = false): string | undefined => {
    const url = assetUrl(value);
    if (!url) return undefined;
    const cacheKey = `${link ? "link" : "draw"}${url.search}`;
    const known = swapped.get(cacheKey);
    if (known) return known;
    const path = assetPath(url.searchParams);
    const data = doc.files.get(path);
    const type = link ? navigableType(mimeOf(path)) : mimeOf(path);
    // A drawn SVG is cleaned of anything executable: opened on its own, its
    // blob URL would be a page of this origin (see svg.ts).
    const bytes = data !== undefined && type === "image/svg+xml" ? cleanSvg(data) : data;
    const out =
      bytes !== undefined ? URL.createObjectURL(new Blob([bytes as BlobPart], { type })) : doc.files.urlOf(path);
    if (out) swapped.set(cacheKey, out);
    return out;
  };
  // Only an `<img src>` draws; any other element could navigate to, frame or run it.
  const isLink = (element: Element, attr: string) => !(attr.toLowerCase() === "src" && element instanceof HTMLImageElement);
  const nativeSetAttribute = Element.prototype.setAttribute;
  Element.prototype.setAttribute = function (name: string, value: string) {
    return nativeSetAttribute.call(this, name, ASSET_ATTRS.includes(name.toLowerCase()) ? (assetNow(value, isLink(this, name)) ?? value) : value);
  };
  // `unsupportedBrowser()` has already refused a browser where this cannot be done.
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
      const now = assetNow(element.getAttribute(attr), isLink(element, attr));
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
    const href = requestHref(input);
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

    send() {
      // The page only listens to this socket; nothing it sends is of interest.
    }

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

  const backend: Backend = {
    get session() {
      return session;
    },
    get version() {
      return version;
    },
    get dirty() {
      return [...sessions.values()].some((x) => x.dirty);
    },
    open(path) {
      const text = doc.files.text(path);
      if (text === undefined) return `${path} is not among the opened files — add it with "add files…"`;
      const before = { path: doc.path, key: doc.key, unsaved: doc.unsaved };
      doc.path = path;
      doc.key = doc.keyFor?.(path) ?? doc.key;
      doc.unsaved = false;
      render.reset();
      const result = render();
      if (result.error !== undefined) {
        Object.assign(doc, before);
        render.reset();
        return `${path}: ${result.error}`;
      }
      session = sessions.get(path) ?? new EditSession(wasm, text);
      sessions.set(path, session);
      swapped = new Map();
      announce(true);
      return undefined;
    },
    current: () => render(),
    markAllSaved() {
      for (const each of sessions.values()) each.markSaved();
    },
    replace: (raw, opKind) => commit(raw, opKind),
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
  return backend;
}
