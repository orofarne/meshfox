// Entry of the serverless prototype. Shows a small launcher (open a canvas, a
// folder, drop files, or try a bundled example); once a document is chosen it
// starts the WebAssembly model, swaps the app's HTTP backend for it, and then
// loads the real meshfox UI unchanged.
import init, * as wasmApi from "../../crates/wasm/pkg/meshfox_wasm.js";
import { createRenderer, installBackend, type Document } from "./backend";
import { EXAMPLES, loadExample } from "./bundled";
import type { VirtualFiles } from "./files";
import { fromDrop, fromFileList, type Picked } from "./pick";

const STYLE = `
  :root { color-scheme: light dark; --bg:#f6f7f9; --fg:#1d2330; --muted:#697386; --card:#fff; --line:#d9dde5; --accent:#3b6fe0; }
  @media (prefers-color-scheme: dark) { :root { --bg:#14171d; --fg:#e6e9ef; --muted:#97a0b0; --card:#1c2029; --line:#2d3340; --accent:#7ba2ff; } }
  body { margin:0; background:var(--bg); color:var(--fg); font:15px/1.5 system-ui, sans-serif; }
  #launcher { max-width:720px; margin:0 auto; padding:40px 20px 64px; }
  #launcher h1 { font-size:26px; margin:0 0 4px; }
  #launcher p { color:var(--muted); margin:4px 0 20px; }
  .drop { border:2px dashed var(--line); border-radius:14px; padding:28px; text-align:center; background:var(--card); transition:border-color .15s; }
  .drop.over { border-color:var(--accent); }
  .row { display:flex; gap:10px; justify-content:center; flex-wrap:wrap; margin-top:14px; }
  button, select { font:inherit; padding:8px 14px; border-radius:8px; border:1px solid var(--line); background:var(--card); color:var(--fg); cursor:pointer; }
  button.primary { background:var(--accent); border-color:var(--accent); color:#fff; }
  h2 { font-size:16px; margin:28px 0 8px; }
  .list { display:flex; flex-wrap:wrap; gap:8px; }
  #status { margin-top:16px; min-height:1.4em; color:var(--muted); }
  #status.error { color:#d64545; }
  .note { font-size:13px; color:var(--muted); margin-top:28px; }
  #wasm-pill { position:fixed; left:12px; bottom:12px; z-index:2147483000; font:12px system-ui, sans-serif; background:#1d2330; color:#fff; padding:5px 10px; border-radius:999px; opacity:.85; }
  #wasm-pill a { color:#9db8ff; margin-left:8px; cursor:pointer; text-decoration:underline; }
`;

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

async function start(doc: Document) {
  status(`Loading the model…`);
  await init();
  // Find out now, with the launcher still on screen, whether this can be shown.
  const render = createRenderer(wasmApi, doc);
  const first = render();
  if (first.error !== undefined) {
    status(`${doc.path}: ${first.error}`, true);
    return;
  }
  installBackend(wasmApi, doc, render);
  document.getElementById("launcher")?.remove();
  const pill = el("div", { id: "wasm-pill", textContent: "serverless · read-only · wasm" });
  pill.append(el("a", { textContent: "open another", onclick: () => location.assign(location.pathname) }));
  document.body.append(pill);
  await import("../src/main");
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
    await start({ path: pool[0], files: picked.files });
    return;
  }
  document.getElementById("choices")!.replaceChildren(
    el("h2", { textContent: "Which one?" }),
    el(
      "div",
      { className: "list" },
      ...pool.slice(0, 40).map((path) =>
        el("button", { textContent: path, onclick: () => void start({ path, files: picked.files }) }),
      ),
    ),
  );
  status(`${picked.files.size} files read.`);
}

function launcher() {
  document.head.append(el("style", { textContent: STYLE }));
  const root = document.getElementById("root")!;

  const filePicker = el("input", { type: "file", multiple: true, hidden: true, accept: ".md,.json,.csv,.txt,.yaml,.yml,.toml,.png,.jpg,.jpeg,.gif,.svg,.webp" });
  const folderPicker = el("input", { type: "file", hidden: true });
  folderPicker.setAttribute("webkitdirectory", "");
  const handle = (task: Promise<Picked>) => {
    status("Reading…");
    task.then(chooseAndStart).catch((e) => status(String(e), true));
  };
  filePicker.onchange = () => filePicker.files && handle(fromFileList(filePicker.files));
  folderPicker.onchange = () => folderPicker.files && handle(fromFileList(folderPicker.files));

  const drop = el(
    "div",
    { className: "drop", id: "drop" },
    el("div", { textContent: "Drop a .canvas.md here — or a whole folder, so its include and file links resolve" }),
    el(
      "div",
      { className: "row" },
      el("button", { className: "primary", textContent: "Open file(s)…", onclick: () => filePicker.click() }),
      el("button", { textContent: "Open folder…", onclick: () => folderPicker.click() }),
    ),
  );
  drop.ondragover = (e) => { e.preventDefault(); drop.classList.add("over"); };
  drop.ondragleave = () => drop.classList.remove("over");
  drop.ondrop = (e) => {
    e.preventDefault();
    drop.classList.remove("over");
    if (e.dataTransfer) handle(fromDrop(e.dataTransfer.items));
  };

  const examples = el("div", { className: "list" });
  for (const example of EXAMPLES) {
    examples.append(
      el("button", {
        textContent: example.label,
        onclick: () => {
          status("Reading the example…");
          loadExample()
            .then((files: VirtualFiles) => start({ path: example.path, files }))
            .catch((e) => status(String(e), true));
        },
      }),
    );
  }

  root.append(
    el(
      "div",
      { id: "launcher" },
      el("h1", { textContent: "meshfox, without a server" }),
      el("p", { textContent: "The real meshfox UI, with the document model running in your browser as WebAssembly. Nothing is uploaded; nothing runs." }),
      drop,
      filePicker,
      folderPicker,
      el("div", { id: "choices" }),
      el("h2", { textContent: "…or try an example" }),
      examples,
      el("div", { id: "status" }),
      el("p", { className: "note", textContent: "Prototype: read-only. Includes, file links, constraints and computed views work; running blocks, editing and tables do not." }),
    ),
  );

  // `?example=hello` (or `?example=examples/hello.canvas.md`) skips the launcher.
  const wanted = new URLSearchParams(location.search).get("example");
  if (wanted) {
    const match = EXAMPLES.find((e) => e.label === wanted || e.path === wanted);
    if (match) {
      status("Reading the example…");
      void loadExample().then((files) => start({ path: match.path, files }));
    } else {
      status(`No bundled example "${wanted}".`, true);
    }
  }
}

launcher();
