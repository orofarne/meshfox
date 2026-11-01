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
import { createRenderer, installBackend, type Backend, type Document, type IncludeWarning } from "./backend";
import { VirtualFiles } from "./files";
import { fromDrop, fromFileList, type Picked } from "./pick";
import { applyThemePreference, getThemePreference } from "../src/theme";
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
  .row { display:flex; gap:.6rem; justify-content:center; flex-wrap:wrap; margin-top:1.1rem; }
  .launcher button { font:inherit; font-size:.85rem; padding:.4rem .9rem; border-radius:6px; border:1px solid var(--node-border); background:transparent; color:var(--fg); cursor:pointer; }
  .launcher button:hover { border-color:var(--accent); color:var(--accent); }
  .launcher button.primary { background:var(--accent); border-color:var(--accent); color:var(--bg); }
  .launcher button.primary:hover { color:var(--bg); filter:brightness(1.08); }
  .launcher h2 { font-size:.9rem; margin:1.5rem 0 .5rem; }
  .list { display:flex; flex-wrap:wrap; gap:.5rem; }
  #status { margin-top:1rem; min-height:1.3em; font-size:.82rem; opacity:.75; }
  #status.error { color:var(--fail); opacity:1; }
  .launcher .note { font-size:.75rem; opacity:.55; margin-top:2rem; line-height:1.6; }
  /* Floats over the app, bottom centre: React Flow's controls sit bottom left
     and its minimap bottom right, so the middle is the free place. */
  #wasm-pill { position:fixed; left:50%; transform:translateX(-50%); bottom:12px; z-index:2147483000; max-width:calc(100vw - 460px); font:12px "Fira Code", system-ui, sans-serif; background:var(--node-bg); color:var(--fg); border:1px solid var(--node-border); padding:4px 10px; border-radius:999px; opacity:.9; }
  #wasm-pill a { color:var(--accent); margin-left:8px; cursor:pointer; text-decoration:underline; }
`;

type HistoryState = { meshfox: "launcher" | "document" } | null;

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
const ready = init();
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

/** The edited text as a file: the page has no disk, so this is how it leaves. */
function download(doc: Document, backend: Backend) {
  const blob = new Blob([backend.session.raw], { type: "text/markdown;charset=utf-8" });
  const url = URL.createObjectURL(blob);
  const link = el("a", { href: url, download: doc.path.split("/").pop() ?? "canvas.canvas.md" });
  document.body.append(link);
  link.click();
  link.remove();
  setTimeout(() => URL.revokeObjectURL(url), 10_000);
  backend.session.markSaved();
}

/** The strip at the bottom of a document: what is missing, and ways to add it. */
function pill(doc: Document, backend: Backend): HTMLElement {
  let problem: string | undefined;
  const label = el("span");
  const warnings = (): IncludeWarning[] => backend.current().warnings ?? [];
  const draw = () => {
    const names = [...new Set(warnings().map((w) => w.target.replace(/^\.\//, "")))];
    label.textContent = [
      backend.session.dirty ? "in your browser · unsaved changes" : "in your browser",
      doc.note,
      names.length > 0 &&
        `${names.length} include${names.length === 1 ? "" : "s"} missing: ${names.slice(0, 3).join(", ")}${names.length > 3 ? ", …" : ""}`,
      problem,
    ]
      .filter(Boolean)
      .join(" · ");
  };
  const canvasDir = VirtualFiles.dirname(doc.path);
  const add = async (task: Promise<Picked>, loose: boolean) => {
    try {
      const picked = await task;
      const n = addPicked(doc.files, canvasDir, warnings().map((w) => w.target), picked.files, loose);
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
  backend.session.onChange(draw);
  draw();
  return el(
    "div",
    { id: "wasm-pill" },
    label,
    filePicker,
    folderPicker,
    el("a", { id: "wasm-download", textContent: "download", onclick: () => download(doc, backend) }),
    el("a", { id: "wasm-add-files", textContent: "add files…", onclick: () => filePicker.click() }),
    el("a", { id: "wasm-add-folder", textContent: "add folder…", onclick: () => folderPicker.click() }),
    el("a", { textContent: "open another", onclick: () => history.back() }),
  );
}

async function start(doc: Document) {
  status(`Loading the model…`);
  await ready;
  // Find out now, with the launcher still on screen, whether this can be shown.
  const render = createRenderer(wasmApi, doc);
  const first = render();
  if (first.error !== undefined) {
    status(`${doc.path}: ${first.error}`, true);
    return;
  }
  const backend = installBackend(wasmApi, doc, render);
  document.getElementById("launcher")?.remove();
  // The URL stays as it is: the app's own relative `api/...` requests (and the
  // fetch interception) depend on it.
  history.pushState({ meshfox: "document" } satisfies HistoryState, "");
  documentOpen = true;
  document.body.append(pill(doc, backend));
  unsaved = () => backend.session.dirty;
  await import("../src/main");
}

/** What tells this document from every other the page has opened. */
const documentKey = (picked: Picked, path: string) => (picked.rootName ? `${picked.rootName}/${path}` : path);

/** A new, empty canvas: its text is the template `meshfox create` writes. */
async function newCanvas(title: string) {
  status("Loading the model…");
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
    await start({ path: pool[0], files: picked.files, key: documentKey(picked, pool[0]), note: skippedNote(picked.skipped) });
    return;
  }
  document.getElementById("choices")!.replaceChildren(
    el("h2", { textContent: "Which one?" }),
    el(
      "div",
      { className: "list" },
      ...pool.slice(0, 40).map((path) =>
        el("button", { textContent: path, onclick: () => void start({ path, files: picked.files, key: documentKey(picked, path), note: skippedNote(picked.skipped) }) }),
      ),
    ),
  );
  status([`${picked.files.size} files read`, skippedNote(picked.skipped)].filter(Boolean).join("; ") + ".");
}

function launcher(note?: string) {
  const root = document.getElementById("root")!;

  const filePicker = el("input", { type: "file", multiple: true, hidden: true, accept: ".md,.json,.csv,.txt,.yaml,.yml,.toml,.png,.jpg,.jpeg,.gif,.svg,.webp" });
  const folderPicker = el("input", { type: "file", hidden: true });
  folderPicker.setAttribute("webkitdirectory", "");
  const handle = (task: Promise<Picked>) => {
    status("Reading…");
    task.then(chooseAndStart).catch((e) => status(String(e), true));
  };
  filePicker.onchange = () => filePicker.files && handle(fromFileList(filePicker.files, newRules));
  folderPicker.onchange = () => folderPicker.files && handle(fromFileList(folderPicker.files, newRules));

  const drop = el(
    "div",
    { className: "drop", id: "drop" },
    el("div", { className: "hint", textContent: "Drop a .canvas.md here" }),
    el("div", { className: "sub", textContent: "or a whole folder, so its include, image and file links resolve" }),
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
    if (e.dataTransfer) handle(fromDrop(e.dataTransfer.items, newRules));
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

  root.append(
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
        filePicker,
        folderPicker,
        el("div", { id: "choices" }),
        el("div", { id: "status" }),
        el("p", {
          className: "note",
          textContent:
            "Preview: read-only for now. Includes, file links, images, constraints and computed views work; running blocks and tables need the meshfox app.",
        }),
      ),
    ),
  );
  if (note) status(note);
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
    event.preventDefault();
    event.returnValue = "";
  }
});

window.addEventListener("popstate", (event) => {
  const state = event.state as HistoryState;
  if (documentOpen && state?.meshfox !== "document") {
    if (unsaved() && !confirm("Leave without downloading? Your edits exist only in this tab.")) {
      // Back cannot be cancelled, only undone: put the document's entry back.
      history.pushState({ meshfox: "document" } satisfies HistoryState, "");
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
