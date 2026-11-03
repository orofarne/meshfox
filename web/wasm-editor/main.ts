// Entry of the in-browser editor/viewer. Shows a small launcher (open a canvas,
// a folder, or drop either); once a document is chosen it starts the
// WebAssembly model, swaps the app's HTTP backend for it, and then loads the
// real meshfox UI unchanged.
//
// Browser history: opening a document pushes an entry, so Back returns to the
// launcher (a document lives only in memory, so "back" is a reload of the
// launcher), and Forward to a document that is gone lands on the launcher with
// a note instead of a blank page.
import init, * as wasmApi from "../../crates/wasm/pkg/meshfox_wasm.js";
import { addPicked } from "./add";
import { describeProgress, fetchWithProgress, fraction, type EngineProgress } from "./engine";
import { createRenderer, installBackend, unsupportedBrowser, type Backend, type Document, type IncludeWarning } from "./backend";
import { ago, deleteDraft, getDraft, listDrafts, putDraft, type Draft } from "./drafts";
import { VirtualFiles } from "./files";
import { fromDrop, fromFileList, type Picked } from "./pick";
import { applyThemePreference, getThemePreference } from "../src/theme";
import wasmUrl from "../../crates/wasm/pkg/meshfox_wasm_bg.wasm?url";
import "@fontsource/fira-code/400.css";
import "@fontsource/fira-code/600.css";
// The webui's own stylesheet: the launcher wears the same palette, font and
// light/dark handling as the app it opens.
import "../src/index.css";

const STYLE = `
  .launcher { min-height:100%; display:flex; flex-direction:column; }
  .launcher .toolbar .brand { font-weight:600; color:var(--accent); letter-spacing:.02em; display:flex; align-items:center; gap:.5rem; }
  .launcher .toolbar .brand img { width:20px; height:20px; }
  .launcher .toolbar .spacer { flex:1; }
  .launcher main { width:min(640px, 100% - 2rem); margin:0 auto; padding:3rem 0 4rem; }
  .launcher h1 { font-size:1.4rem; margin:0 0 .5rem; }
  .launcher .lede { opacity:.75; margin:0 0 1.5rem; font-size:.9rem; line-height:1.55; }
  .drop { border:1px dashed var(--node-border); border-radius:10px; padding:2rem 1.5rem; text-align:center; background:var(--node-bg); transition:border-color .15s, background .15s; }
  .drop.over { border-color:var(--accent); background:color-mix(in srgb, var(--accent) 8%, var(--node-bg)); }
  .drop .hint { font-size:.9rem; opacity:.85; }
  .drop .sub { font-size:.78rem; opacity:.55; margin-top:.4rem; }
  .fresh { display:flex; gap:.5rem; align-items:center; justify-content:center; flex-wrap:wrap; margin-top:1rem; font-size:.85rem; }
  .fresh span { opacity:.65; }
  .fresh .suffix { opacity:.5; margin-left:-.4rem; }
  .fresh input { font:inherit; font-size:.85rem; padding:.35rem .6rem; border-radius:6px; border:1px solid var(--node-border); background:var(--node-bg); color:var(--fg); width:10rem; }
  .draft { display:flex; gap:.6rem; align-items:center; flex-wrap:wrap; font-size:.82rem; margin:.4rem 0; }
  .draft span { margin-right:auto; opacity:.85; overflow-wrap:anywhere; }
  .row { display:flex; gap:.6rem; justify-content:center; flex-wrap:wrap; margin-top:1.1rem; }
  .launcher button { font:inherit; font-size:.85rem; padding:.4rem .9rem; border-radius:6px; border:1px solid var(--node-border); background:transparent; color:var(--fg); cursor:pointer; }
  .launcher button:hover { border-color:var(--accent); color:var(--accent); }
  .launcher button.primary { background:var(--accent); border-color:var(--accent); color:var(--bg); }
  .launcher button.primary:hover { color:var(--bg); filter:brightness(1.08); }
  .launcher h2 { font-size:.9rem; margin:1.5rem 0 .5rem; }
  .list { display:flex; flex-wrap:wrap; gap:.5rem; }
  #engine { margin-top:1rem; display:flex; gap:.6rem; align-items:center; font-size:.8rem; opacity:.8; }
  #engine[hidden] { display:none; }
  #engine progress { width:8rem; accent-color:var(--accent); }
  #engine.error { color:var(--fail); opacity:1; }
  #status { margin-top:1rem; min-height:1.3em; font-size:.82rem; opacity:.75; }
  #status.error { color:var(--fail); opacity:1; }
  #help { margin-top:1rem; font-size:.82rem; line-height:1.55; }
  #help summary { cursor:pointer; opacity:.8; }
  #help summary:hover { color:var(--accent); }
  #help h3 { font-size:.82rem; margin:1rem 0 .3rem; }
  #help ul { margin:0; padding-left:1.1rem; opacity:.85; }
  #help li { margin:.3rem 0; }
  .launcher .note { font-size:.75rem; opacity:.55; margin-top:2rem; line-height:1.6; }
  /* Above the canvas but below every editor, dialog and backdrop of the app
     (z-index 2000 and up): where one is open, it is what the person is using. */
  /* Floats over the app, bottom centre: React Flow's controls sit bottom left
     and its minimap bottom right, so the middle is the free place. */
  #wasm-pill { position:fixed; left:50%; transform:translateX(-50%); bottom:12px; z-index:1000; max-width:calc(100vw - 460px); font:12px "Fira Code", system-ui, sans-serif; background:var(--node-bg); color:var(--fg); border:1px solid var(--node-border); padding:4px 10px; border-radius:999px; opacity:.9; }
  /* A narrow screen has no free middle: the strip becomes a bar across the
     bottom, and React Flow's controls and minimap sit above it (--wasm-pill-h
     is its height, kept by pill()). */
  @media (max-width: 900px) {
    #wasm-pill { left:8px; right:8px; bottom:8px; transform:none; max-width:none; border-radius:12px; padding:6px 10px; line-height:1.7; }
    .react-flow__panel.bottom { margin-bottom: calc(var(--wasm-pill-h, 0px) + 15px) !important; }
  }
  #wasm-pill a { color:var(--accent); margin-left:8px; cursor:pointer; text-decoration:underline; }
`;

type HistoryState = { meshfox: "launcher" } | { meshfox: "document"; path?: string } | null;

function el<K extends keyof HTMLElementTagNameMap>(
  tag: K,
  props: Partial<HTMLElementTagNameMap[K]> = {},
  ...kids: (Node | string)[]
): HTMLElementTagNameMap[K] {
  const node = Object.assign(document.createElement(tag), props);
  node.append(...kids);
  return node;
}

function status(message: string, isError = false) {
  const s = document.getElementById("status");
  if (s) {
    s.textContent = message;
    s.className = isError ? "error" : "";
  }
}

let documentOpen = false;
/** Whether the open document has edits that were never downloaded. */
let unsaved: () => boolean = () => false;
let discarding = false;

// Loaded as soon as the page is: a folder pick needs the model's ignore rules,
// and a drop must hand over its entries before its first `await`.
let engine: EngineProgress = { loaded: 0, done: false };
let engineFailed: string | undefined;
/** Paints the engine's progress wherever the launcher has a place for it. */
function drawEngine() {
  const box = document.getElementById("engine");
  if (!box) return;
  box.hidden = engine.done && !engineFailed;
  const bar = box.querySelector("progress");
  const label = box.querySelector("span");
  const f = fraction(engine);
  if (bar) {
    if (f === undefined) bar.removeAttribute("value");
    else bar.value = f;
  }
  if (label) label.textContent = engineFailed ?? `Loading the engine… ${describeProgress(engine)}`;
  box.classList.toggle("error", !!engineFailed);
}
const ready = init({
  module_or_path: fetchWithProgress(wasmUrl, (p) => {
    engine = p;
    drawEngine();
  }),
}).catch((e) => {
  engineFailed = `The engine could not be loaded: ${e instanceof Error ? e.message : String(e)}. Reload the page to try again.`;
  drawEngine();
  throw e;
});
const newRules = async () => {
  await ready;
  return new wasmApi.IgnoreRules();
};

const skippedNote = (skipped: Picked["skipped"]): string | undefined => {
  const parts = [
    skipped.tooLarge > 0 && `${skipped.tooLarge} over 25 MB`,
    skipped.tooMany > 0 && `${skipped.tooMany} beyond the 20 000-file limit`,
  ].filter(Boolean);
  return parts.length > 0 ? `not read: ${parts.join(", ")}` : undefined;
};

/** Hands `blob` to the user as a file called `name`. */
function offer(blob: Blob, name: string) {
  const url = URL.createObjectURL(blob);
  const link = el("a", { href: url, download: name });
  document.body.append(link);
  link.click();
  link.remove();
  setTimeout(() => URL.revokeObjectURL(url), 10_000);
}

/** The edited text as a file: the page has no disk, so this is how it leaves. */
function download(doc: Document, backend: Backend) {
  offer(
    new Blob([backend.session.raw], { type: "text/markdown;charset=utf-8" }),
    doc.path.split("/").pop() ?? "canvas.canvas.md",
  );
  backend.session.markSaved();
}

/** The opened pack with the edits in it, as a new pack under the name of the
 * one that was opened (which itself is never changed). The reason it cannot be
 * made, or `undefined`. */
function downloadPack(doc: Document, backend: Backend): string | undefined {
  const pack = doc.pack;
  if (!pack) return undefined;
  try {
    // Every canvas edited so far has its latest text in the files already.
    const files = doc.files.paths().map((path) => ({ path, data: doc.files.get(path)! }));
    const bytes = wasmApi.buildPack(pack.entry, `meshfox editor ${wasmApi.version()}`, files);
    offer(new Blob([bytes as BlobPart], { type: "application/x-xz" }), pack.name);
    backend.markAllSaved();
    return undefined;
  } catch (e) {
    return `not packed: ${e instanceof Error ? e.message : String(e)}`;
  }
}

/** A packed canvas (`.canvas.tar.xz`), if `file` is one: told by its first
 * bytes, not its name. Unpacked and checked completely by the model before
 * anything is shown; one that fails a check is refused with the reason. */
async function openAsPack(file: File): Promise<Picked | undefined> {
  const head = new Uint8Array(await file.slice(0, 6).arrayBuffer());
  await ready;
  if (!wasmApi.isPack(head)) return undefined;
  let opened: { entry: string; files: Array<{ path: string; data: Uint8Array }> };
  try {
    opened = wasmApi.openPack(new Uint8Array(await file.arrayBuffer())) as typeof opened;
  } catch (e) {
    throw new Error(`${file.name}: ${e instanceof Error ? e.message : String(e)}`);
  }
  const files = new VirtualFiles();
  for (const f of opened.files) files.set(f.path, f.data);
  return {
    files,
    candidates: [opened.entry],
    rootName: file.name,
    skipped: { tooLarge: 0, tooMany: 0 },
    pack: { name: file.name, entry: opened.entry },
  };
}

/** What a pick of these files is: one packed canvas, or loose files. */
async function pickedFiles(list: FileList): Promise<Picked> {
  if (list.length === 1) {
    const pack = await openAsPack(list[0]);
    if (pack) return pack;
  }
  return fromFileList(list, newRules);
}

type SaveFilePicker = (options: {
  suggestedName: string;
  types: Array<{ description: string; accept: Record<string, string[]> }>;
}) => Promise<{ createWritable(): Promise<{ write(data: string): Promise<void>; close(): Promise<void> }> }>;

/** Where "save" writes, once the user has said so (File System Access API: Chromium). */
const savePicker = (): SaveFilePicker | undefined =>
  (window as unknown as { showSaveFilePicker?: SaveFilePicker }).showSaveFilePicker;

/** The strip at the bottom of a document: what is missing, and ways to add it. */
function pill(doc: Document, backend: Backend, pending?: Draft): { element: HTMLElement; dispose(): void } {
  let problem: string | undefined;
  const label = el("span");
  const warnings = (): IncludeWarning[] => backend.current().warnings ?? [];
  const draw = () => {
    const names = [...new Set(warnings().map((w) => w.target.replace(/^\.\//, "")))];
    label.textContent = [
      backend.session.dirty ? "in your browser · unsaved changes" : "in your browser",
      pending && `unsaved edits from ${ago(pending.savedAt)} found`,
      doc.note,
      names.length > 0 &&
        `${names.length} include${names.length === 1 ? "" : "s"} missing: ${names.slice(0, 3).join(", ")}${names.length > 3 ? ", …" : ""}`,
      problem,
    ]
      .filter(Boolean)
      .join(" · ");
  };
  const add = async (task: Promise<Picked>, loose: boolean) => {
    try {
      const picked = await task;
      const n = addPicked(doc.files, VirtualFiles.dirname(doc.path), warnings().map((w) => w.target), picked.files, loose);
      problem = n === 0 ? "nothing to add in that" : undefined;
      const result = backend.refresh();
      if (result.error !== undefined) problem = result.error;
    } catch (e) {
      problem = String(e);
    }
    draw();
  };
  const filePicker = el("input", { type: "file", multiple: true, hidden: true });
  const folderPicker = el("input", { type: "file", hidden: true });
  folderPicker.setAttribute("webkitdirectory", "");
  filePicker.onchange = () => {
    if (filePicker.files) void add(fromFileList(filePicker.files, newRules), true);
    filePicker.value = "";
  };
  folderPicker.onchange = () => {
    if (folderPicker.files) void add(fromFileList(folderPicker.files, newRules), false);
    folderPicker.value = "";
  };
  // Writing into a file, where the browser lets a page do that: asked about once,
  // then the same file is written again silently.
  let target: Awaited<ReturnType<SaveFilePicker>> | undefined;
  const save = async () => {
    const pick = savePicker();
    if (!pick) return;
    try {
      target ??= await pick({
        suggestedName: doc.path.split("/").pop() ?? "canvas.canvas.md",
        types: [{ description: "meshfox canvas", accept: { "text/markdown": [".md"] } }],
      });
      const writable = await target.createWritable();
      await writable.write(backend.session.raw);
      await writable.close();
      backend.session.markSaved();
      problem = undefined;
    } catch (e) {
      // Closing the picker is not a problem.
      problem = e instanceof DOMException && e.name === "AbortError" ? undefined : `not saved: ${String(e)}`;
    }
    draw();
  };
  const decide = (restore: boolean) => {
    const draft = pending;
    pending = undefined;
    if (draft && restore) problem = backend.replace(draft.raw, "restored");
    else if (draft) void deleteDraft(draft.key);
    for (const id of ["wasm-restore", "wasm-discard"]) document.getElementById(id)?.remove();
    draw();
  };
  const unsubscribe = backend.session.onChange(draw);
  draw();
  const element = el(
    "div",
    { id: "wasm-pill" },
    label,
    filePicker,
    folderPicker,
    ...(pending
      ? [
          el("a", { id: "wasm-restore", textContent: "restore them", onclick: () => decide(true) }),
          el("a", { id: "wasm-discard", textContent: "discard", onclick: () => decide(false) }),
        ]
      : []),
    ...(savePicker() && !doc.pack ? [el("a", { id: "wasm-save", textContent: "save", onclick: () => void save() })] : []),
    ...(doc.pack
      ? [
          el("a", {
            id: "wasm-download",
            textContent: "download pack",
            onclick: () => {
              problem = downloadPack(doc, backend);
              draw();
            },
          }),
          el("a", { id: "wasm-download-canvas", textContent: "download canvas", onclick: () => download(doc, backend) }),
        ]
      : [el("a", { id: "wasm-download", textContent: "download", onclick: () => download(doc, backend) })]),
    el("a", { id: "wasm-add-files", textContent: "add files…", onclick: () => filePicker.click() }),
    el("a", { id: "wasm-add-folder", textContent: "add folder…", onclick: () => folderPicker.click() }),
    el("a", { textContent: "open another", onclick: () => history.back() }),
  );
  // The height the bar takes at the bottom, for the rules above.
  const watcher = new ResizeObserver(() =>
    document.documentElement.style.setProperty("--wasm-pill-h", `${element.offsetHeight + 8}px`),
  );
  watcher.observe(element);
  return {
    element,
    dispose: () => {
      unsubscribe();
      watcher.disconnect();
      element.remove();
    },
  };
}

/** Keeps the unsaved text as a draft while it differs from what was saved, and
 * drops it once it does not (saved, or undone back). Held back while an older
 * draft is waiting for the user's decision: it must not be overwritten (or
 * deleted by a clean document) before they have chosen. */
function keepDraft(doc: Document, backend: Backend, waiting: () => boolean): () => void {
  const key = doc.key!;
  const path = doc.path;
  let timer: ReturnType<typeof setTimeout> | undefined;
  const write = () => {
    clearTimeout(timer);
    timer = undefined;
    if (waiting()) return;
    if (backend.session.dirty) {
      void putDraft({ key, path, raw: backend.session.raw, savedAt: Date.now() });
    } else {
      void deleteDraft(key);
    }
  };
  const unsubscribe = backend.session.onChange(() => {
    clearTimeout(timer);
    timer = setTimeout(write, 400);
  });
  // A debounce that has not fired yet must not be lost with the tab (or with
  // a move to another canvas).
  const flush = () => timer !== undefined && write();
  const hidden = () => document.visibilityState === "hidden" && flush();
  addEventListener("pagehide", flush);
  document.addEventListener("visibilitychange", hidden);
  return () => {
    flush();
    unsubscribe();
    removeEventListener("pagehide", flush);
    document.removeEventListener("visibilitychange", hidden);
  };
}

/** What is mounted for the canvas now shown: the strip at the bottom and the
 * draft-keeping. Taken down, and put up again, when another canvas is shown. */
let mounted: { dispose(): void } | undefined;
/** The document the page is showing, for Back/Forward between canvases. */
let shown: { doc: Document; backend: Backend } | undefined;

async function mount(doc: Document, backend: Backend) {
  const session = backend.session;
  // Work left over from an earlier visit to this same document: offered, never
  // applied on its own (and kept until the user decides).
  const stored = doc.key && !doc.unsaved ? await getDraft(doc.key) : undefined;
  if (backend.session !== session) return; // already moved on
  const pending = stored && stored.raw !== session.raw ? stored : undefined;
  const strip = pill(doc, backend, pending);
  document.body.append(strip.element);
  const stopKeeping = doc.key
    ? keepDraft(doc, backend, () => pending !== undefined && document.getElementById("wasm-restore") !== null)
    : () => {};
  mounted = {
    dispose: () => {
      stopKeeping();
      strip.dispose();
    },
  };
}

/** Shows another canvas of the opened files. Edits to the one left stay in the
 * page (and in its draft); going back finds them as they were. */
function switchTo(doc: Document, backend: Backend, open: () => string | undefined, how: { push: boolean; fragment?: string | null }): string | undefined {
  const left = mounted;
  mounted = undefined;
  left?.dispose();
  const problem = open();
  if (problem === undefined && how.push) {
    history.pushState({ meshfox: "document", path: doc.path } satisfies HistoryState, "");
  }
  void mount(doc, backend);
  if (problem === undefined) {
    // The app is told which canvas is now shown, to place the view on it.
    const detail = { fragment: how.fragment ? decodeURIComponent(how.fragment) : null, version: backend.version };
    window.dispatchEvent(new CustomEvent("meshfox:canvas-opened", { detail }));
  }
  return problem;
}

async function start(doc: Document) {
  status(engine.done ? "Reading the document…" : "Waiting for the engine…");
  await ready;
  status("Reading the document…");
  // Find out now, with the launcher still on screen, whether this can be shown.
  const render = createRenderer(wasmApi, doc);
  const first = render();
  if (first.error !== undefined) {
    status(`${doc.path}: ${first.error}`, true);
    return;
  }
  const unsupported = unsupportedBrowser();
  if (unsupported !== undefined) {
    status(unsupported, true);
    return;
  }
  const backend = installBackend(wasmApi, doc, render);
  document.getElementById("launcher")?.remove();
  // The URL stays as it is: the app's own relative `api/...` requests (and the
  // fetch interception) depend on it.
  history.pushState({ meshfox: "document", path: doc.path } satisfies HistoryState, "");
  documentOpen = true;
  shown = { doc, backend };
  backend.navigate = (path, fragment) => switchTo(doc, backend, () => backend.open(path), { push: true, fragment });
  unsaved = () => backend.dirty;
  await mount(doc, backend);
  // The app is the next heavy download; until it has mounted the page would
  // be empty, so say what is happening.
  const booting = el("p", { id: "booting", textContent: "Loading the app…" });
  booting.style.cssText = "font:13px 'Fira Code', system-ui, sans-serif; opacity:.7; text-align:center; margin-top:30vh";
  document.body.append(booting);
  try {
    await import("../src/main");
  } finally {
    booting.remove();
  }
}

/** What tells this document from every other the page has opened. */
const documentKey = (picked: Picked, path: string) => (picked.rootName ? `${picked.rootName}/${path}` : path);

/** A new, empty canvas: its text is the template `meshfox create` writes. */
async function newCanvas(title: string) {
  status(engine.done ? "Reading…" : "Waiting for the engine…");
  await ready;
  const path = `${title}.canvas.md`;
  const files = new VirtualFiles();
  files.set(path, wasmApi.newCanvasContent(title));
  await start({ path, files, key: `new:${path}` });
}

async function chooseAndStart(picked: Picked) {
  // Canvases first; any other Markdown file only if there is no canvas.
  const canvases = picked.candidates.filter((p) => p.endsWith(".canvas.md"));
  const pool = canvases.length > 0 ? canvases : picked.candidates;
  if (pool.length === 0) {
    status("No Markdown files in what you picked: a canvas is a .canvas.md file.", true);
    return;
  }
  if (pool.length === 1) {
    await start({ path: pool[0], files: picked.files, key: documentKey(picked, pool[0]), keyFor: (p) => documentKey(picked, p), note: skippedNote(picked.skipped), pack: picked.pack });
    return;
  }
  document.getElementById("choices")!.replaceChildren(
    el("h2", { textContent: "Which one?" }),
    el(
      "div",
      { className: "list" },
      ...pool.slice(0, 40).map((path) =>
        el("button", { textContent: path, onclick: () => void start({ path, files: picked.files, key: documentKey(picked, path), keyFor: (p) => documentKey(picked, p), note: skippedNote(picked.skipped), pack: picked.pack }) }),
      ),
    ),
  );
  status([`${picked.files.size} files read`, skippedNote(picked.skipped)].filter(Boolean).join("; ") + ".");
}

/** What the page does with a file, in plain words: the answer to "is this safe
 * to open my notes in?" has to be on the first screen. */
function help(): HTMLElement {
  // Which build this is, for a bug report: what `meshfox --version` would say.
  const version = el("p", { id: "version", className: "note" });
  void ready.then(() => (version.textContent = `meshfox editor ${wasmApi.version()}`)).catch(() => {});
  const section = (title: string, ...items: string[]) => [
    el("h3", { textContent: title }),
    el("ul", {}, ...items.map((text) => el("li", { textContent: text }))),
  ];
  return el(
    "details",
    { id: "help" },
    el("summary", { textContent: "Help and privacy" }),
    ...section(
      "Privacy",
      "Your files are read in this tab and never uploaded. The page has no server to send them to; it only downloads its own code, and runs no analytics.",
      "Your edits live in this tab. While they are unsaved, a draft of the text is kept in this browser's storage on this device, so a closed tab or a crash does not lose them. It is removed when you save, discard it, or undo back to the saved text; clearing the site's data removes it too.",
      "Nothing in a document is executed: code blocks are shown, not run, and HTML in Markdown is shown as text. A picture or link that points to another website is fetched from that website, which can tell that the document was opened; links leave without saying where you came from.",
    ),
    ...section(
      "Using it",
      "Open a folder, or drop it here, when the canvas includes other files, images or links to other canvases; for a single self-contained file, opening just that file is enough. “add files…” brings in what is missing later.",
      "A packed canvas (.canvas.tar.xz, made by meshfox pack) opens like a folder: the whole pack is checked first, then its files, includes, images and linked canvases are all there. “download pack” hands you a new pack with your edits; the file you opened never changes.",
      "Edit changes nodes in place; Source edits the whole file. Undo, redo and the history list work on every edit, for each canvas separately.",
      "Nothing changes your original until you save. “save” writes into a file you choose (Chrome and Edge), “download” hands you the text anywhere else. A canvas that links to another one in the folder opens it with ↗; Back returns.",
    ),
    ...section(
      "Not here",
      "Running code blocks, buttons and forms, services, tables (display=table), link preview cards, and editing the files a canvas includes. Those need the meshfox app (meshfox view).",
    ),
    version,
  );
}

function launcher(note?: string) {
  const root = document.getElementById("root")!;

  const filePicker = el("input", { type: "file", multiple: true, hidden: true, accept: ".md,.json,.csv,.txt,.yaml,.yml,.toml,.png,.jpg,.jpeg,.gif,.svg,.webp,.xz" });
  const folderPicker = el("input", { type: "file", hidden: true });
  folderPicker.setAttribute("webkitdirectory", "");
  const handle = (task: Promise<Picked>) => {
    status(engine.done ? "Reading…" : "Waiting for the engine…");
    task.then(chooseAndStart).catch((e) => status(String(e), true));
  };
  filePicker.onchange = () => filePicker.files && handle(pickedFiles(filePicker.files));
  folderPicker.onchange = () => folderPicker.files && handle(fromFileList(folderPicker.files, newRules));

  const drop = el(
    "div",
    { className: "drop", id: "drop" },
    el("div", { className: "hint", textContent: "Drop a .canvas.md here" }),
    el("div", { className: "sub", textContent: "or a whole folder, so its include, image and file links resolve, or a packed canvas (.canvas.tar.xz)" }),
    el(
      "div",
      { className: "row" },
      el("button", { className: "primary", textContent: "Open file…", onclick: () => filePicker.click() }),
      el("button", { textContent: "Open folder…", onclick: () => folderPicker.click() }),
    ),
  );
  drop.ondragover = (e) => { e.preventDefault(); drop.classList.add("over"); };
  drop.ondragleave = () => drop.classList.remove("over");
  drop.ondrop = (e) => {
    e.preventDefault();
    drop.classList.remove("over");
    const dropped = e.dataTransfer;
    if (!dropped) return;
    // One dropped file may be a pack. The drop's entries can only be taken
    // before the first `await`, so a folder (or several things) goes straight
    // to the entries API, and a lone file is looked at first.
    const lone = dropped.files.length === 1 && dropped.items.length === 1 && dropped.items[0].webkitGetAsEntry?.()?.isFile;
    handle(lone ? pickedFiles(dropped.files) : fromDrop(dropped.items, newRules));
  };

  const newName = el("input", { id: "new-name", type: "text", value: "untitled", ariaLabel: "name of the new canvas" });
  newName.spellcheck = false;
  const create = () => {
    const title = newName.value.trim().replace(/(\.canvas)?\.md$/, "") || "untitled";
    void newCanvas(title);
  };
  newName.onkeydown = (e) => {
    // A statement, not `&&`: an `on*` handler that returns `false` cancels the
    // key, and then nothing could be typed.
    if (e.key === "Enter") create();
  };
  const fresh = el(
    "div",
    { className: "fresh" },
    el("span", { textContent: "or start a new canvas:" }),
    newName,
    el("span", { className: "suffix", textContent: ".canvas.md" }),
    el("button", { id: "new-canvas", textContent: "New canvas", onclick: create }),
  );

  root.replaceChildren(
    el(
      "div",
      { id: "launcher", className: "launcher" },
      el(
        "div",
        { className: "toolbar" },
        el("span", { className: "brand" }, el("img", { src: "favicon-32.png", alt: "" }), "meshfox"),
        el("span", { className: "spacer" }),
        el("span", { className: "mode-badge", textContent: "editor" }),
      ),
      el(
        "main",
        {},
        el("h1", { textContent: "Open a canvas" }),
        el("p", {
          className: "lede",
          textContent:
            "meshfox runs entirely in this tab, as WebAssembly. The file you open is read here and never uploaded.",
        }),
        drop,
        fresh,
        el("div", { id: "drafts" }),
        filePicker,
        folderPicker,
        el("div", { id: "choices" }),
        el("div", { id: "engine" }, el("progress", { max: 1 }), el("span")),
        el("div", { id: "status" }),
        el("p", {
          className: "note",
          textContent:
            "Nothing is uploaded or tracked. Unsaved edits are kept in this browser only. Running blocks, forms and tables need the meshfox app.",
        }),
        help(),
      ),
    ),
  );
  if (note) status(note);
  drawEngine();
  void showDrafts();
}

/** Drafts left in this browser, on the launcher: where work lost to a closed
 * tab or a crash comes back from. */
async function showDrafts() {
  const box = document.getElementById("drafts");
  if (!box) return;
  const drafts = await listDrafts();
  if (drafts.length === 0) return;
  const row = (draft: Draft) => {
    const line: HTMLElement = el(
      "div",
      { className: "draft" },
      el("span", { textContent: `${draft.key} · ${ago(draft.savedAt)}` }),
      el("button", {
        textContent: "Restore",
        onclick: () => {
          // Only the canvas text was kept: its includes and pictures show as
          // missing until the files are added back ("add files…").
          const files = new VirtualFiles();
          files.set(draft.path, draft.raw);
          void start({ path: draft.path, files, key: draft.key, unsaved: true });
        },
      }),
      el("button", {
        textContent: "Discard",
        onclick: () => {
          void deleteDraft(draft.key);
          line.remove();
          if (box.querySelectorAll(".draft").length === 0) box.replaceChildren();
        },
      }),
    );
    return line;
  };
  box.replaceChildren(el("h2", { textContent: "Unsaved work in this browser" }), ...drafts.map(row));
}

applyThemePreference(getThemePreference());
document.head.append(el("style", { textContent: STYLE }));
// This entry is the launcher; mark it so Back/Forward can tell it from a
// document entry (a reload keeps `history.state`, which is how a stale
// "document" entry is recognised).
const arrivedOnDocument = (history.state as HistoryState)?.meshfox === "document";
history.replaceState({ meshfox: "launcher" } satisfies HistoryState, "");
launcher(arrivedOnDocument ? "That document was only in memory. Open it again to continue." : undefined);

window.addEventListener("beforeunload", (event) => {
  if (documentOpen && !discarding && unsaved()) {
    // `preventDefault` is the standard way to ask for the "leave this page?"
    // prompt; the old `returnValue = ""` is deprecated and no longer needed.
    event.preventDefault();
  }
});

window.addEventListener("popstate", (event) => {
  const state = event.state as HistoryState;
  if (documentOpen && state?.meshfox === "document") {
    // Back or Forward between two canvases of the same folder.
    if (state.path !== undefined && state.path !== shown?.doc.path) {
      switchTo(shown!.doc, shown!.backend, () => shown!.backend.open(state.path!), { push: false });
    }
  } else if (documentOpen) {
    if (unsaved() && !confirm("Leave without downloading? Your edits exist only in this tab.")) {
      // Back cannot be cancelled, only undone: put the document's entry back.
      history.pushState({ meshfox: "document", path: shown?.doc.path } satisfies HistoryState, "");
      return;
    }
    discarding = true;
    // Back out of a document: the app cannot be unmounted cleanly (its fetch
    // and socket are replaced), so start over from the launcher.
    location.reload();
  } else if (!documentOpen && state?.meshfox === "document") {
    // Forward onto a document that no longer exists.
    history.replaceState({ meshfox: "launcher" } satisfies HistoryState, "");
    status("That document was only in memory. Open it again to continue.");
  }
});
