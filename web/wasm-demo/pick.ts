import { VirtualFiles } from "./files";

/** The rules for what a folder pick leaves out are the model's own (names
 * starting with `.`, and `.meshfoxignore` files, with `.gitignore` syntax) —
 * the same ones `meshfox serve --http` indexes by. */
export interface IgnoreRules {
  addFile(dir: string, text: string): void;
  isIgnored(path: string, isDir: boolean): boolean;
}
/** Resolves once the wasm module is ready. */
export type NewIgnoreRules = () => Promise<IgnoreRules>;

const IGNORE_FILE = ".meshfoxignore";
/** Guardrails on memory, not on content: everything read stays in the page. */
const MAX_FILES = 20_000;
const MAX_FILE_BYTES = 25 * 1024 * 1024;

export interface Picked {
  files: VirtualFiles;
  /** Every Markdown file, most canvas-like first. */
  candidates: string[];
  /** The picked folder's own name, `""` for loose files: with a canvas's path
   * it identifies the document. */
  rootName: string;
  /** What the guardrails left out (not what the ignore rules did). */
  skipped: { tooLarge: number; tooMany: number };
}

function finish(files: VirtualFiles, skipped: Picked["skipped"], rootName: string): Picked {
  const mds = files.paths().filter((p) => p.endsWith(".md"));
  const rank = (p: string) => (p.endsWith(".canvas.md") ? 0 : 1);
  mds.sort((a, b) => rank(a) - rank(b) || a.split("/").length - b.split("/").length || a.localeCompare(b));
  return { files, candidates: mds, skipped, rootName };
}

const dirname = (path: string) => (path.includes("/") ? path.slice(0, path.lastIndexOf("/")) : "");

class Reader {
  readonly files = new VirtualFiles();
  readonly skipped = { tooLarge: 0, tooMany: 0 };
  rootName = "";
  constructor(readonly rules: IgnoreRules) {}

  /** Reads `file` into the tree unless a guardrail says no. */
  async add(path: string, file: File): Promise<void> {
    if (this.files.size >= MAX_FILES) {
      this.skipped.tooMany++;
    } else if (file.size > MAX_FILE_BYTES) {
      this.skipped.tooLarge++;
    } else {
      this.files.set(path, new Uint8Array(await file.arrayBuffer()));
    }
  }

  async addIgnoreFile(path: string, file: File): Promise<void> {
    this.rules.addFile(dirname(path), await file.text());
  }

  done(): Picked {
    return finish(this.files, this.skipped, this.rootName);
  }
}

/** From an `<input type=file>`: a folder (`webkitRelativePath`, rooted at the
 * folder itself) or a handful of loose files (rooted at nothing). The browser
 * lists every file up front, so the ignore files are read first and the rest
 * only if they pass; nothing is read from an excluded directory. */
export async function fromFileList(list: FileList, newRules: NewIgnoreRules): Promise<Picked> {
  const entries = Array.from(list).map((file) => {
    const rel = file.webkitRelativePath;
    return { file, path: rel ? rel.split("/").slice(1).join("/") : file.name };
  });
  const reader = new Reader(await newRules());
  reader.rootName = Array.from(list).find((f) => f.webkitRelativePath)?.webkitRelativePath.split("/")[0] ?? "";
  // Shallow ignore files first: a deeper one only matters inside its own
  // directory, and a directory excluded from above is never entered.
  const ignoreFiles = entries
    .filter((e) => e.path.split("/").pop() === IGNORE_FILE)
    .sort((a, b) => a.path.split("/").length - b.path.split("/").length);
  for (const e of ignoreFiles) {
    if (!reader.rules.isIgnored(dirname(e.path), true)) await reader.addIgnoreFile(e.path, e.file);
  }
  for (const e of entries) {
    if (e.path.split("/").pop() === IGNORE_FILE || reader.rules.isIgnored(e.path, false)) continue;
    await reader.add(e.path, e.file);
  }
  return reader.done();
}

/** From a drop: files and whole folders, via the entries API. A directory is
 * listed, its ignore file read, and only then are its children visited, so an
 * excluded directory (`node_modules/`) is never walked at all. */
export async function fromDrop(items: DataTransferItemList, newRules: NewIgnoreRules): Promise<Picked> {
  // The entries must be taken synchronously: the browser invalidates the
  // drop's items once the handler returns.
  const entries = Array.from(items)
    .map((item) => item.webkitGetAsEntry?.())
    .filter((e): e is FileSystemEntry => !!e);
  const reader = new Reader(await newRules());
  // A single dropped folder is the root; anything else is dropped loose.
  const rootIsFolder = entries.length === 1 && entries[0].isDirectory;

  const fileOf = (entry: FileSystemEntry) =>
    new Promise<File>((ok, fail) => (entry as FileSystemFileEntry).file(ok, fail));

  const list = async (dir: FileSystemDirectoryEntry): Promise<FileSystemEntry[]> => {
    const batches = dir.createReader();
    const all: FileSystemEntry[] = [];
    // `readEntries` returns at most ~100 at a time; ask until it is empty.
    for (;;) {
      const batch = await new Promise<FileSystemEntry[]>((ok, fail) => batches.readEntries(ok, fail));
      if (batch.length === 0) return all;
      all.push(...batch);
    }
  };

  /** `prefix` is the path of `dir` itself, `""` or ending in `/`. */
  const walkDir = async (dir: FileSystemDirectoryEntry, prefix: string): Promise<void> => {
    const children = await list(dir);
    const ignoreFile = children.find((c) => c.isFile && c.name === IGNORE_FILE);
    if (ignoreFile) await reader.addIgnoreFile(`${prefix}${IGNORE_FILE}`, await fileOf(ignoreFile));
    for (const child of children) {
      if (child === ignoreFile) continue;
      const path = prefix + child.name;
      if (reader.rules.isIgnored(path, child.isDirectory)) continue;
      if (child.isDirectory) await walkDir(child as FileSystemDirectoryEntry, `${path}/`);
      else await reader.add(path, await fileOf(child));
    }
  };

  if (rootIsFolder) reader.rootName = entries[0].name;
  for (const entry of entries) {
    if (rootIsFolder) await walkDir(entry as FileSystemDirectoryEntry, "");
    else if (entry.isDirectory) {
      if (!reader.rules.isIgnored(entry.name, true)) await walkDir(entry as FileSystemDirectoryEntry, `${entry.name}/`);
    } else if (!reader.rules.isIgnored(entry.name, false)) {
      await reader.add(entry.name, await fileOf(entry));
    }
  }
  return reader.done();
}
