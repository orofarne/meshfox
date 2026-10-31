import { VirtualFiles } from "./files";

/** Directories that are never a canvas's business and can be enormous. */
const SKIP_DIRS = new Set([".git", "node_modules", "target", ".venv", ".meshfox", "__pycache__", "dist"]);
const MAX_FILES = 20_000;
const MAX_FILE_BYTES = 25 * 1024 * 1024;

export interface Picked {
  files: VirtualFiles;
  /** Every Markdown file, most canvas-like first. */
  candidates: string[];
}

function finish(files: VirtualFiles): Picked {
  const mds = files.paths().filter((p) => p.endsWith(".md"));
  const rank = (p: string) => (p.endsWith(".canvas.md") ? 0 : 1);
  mds.sort((a, b) => rank(a) - rank(b) || a.split("/").length - b.split("/").length || a.localeCompare(b));
  return { files, candidates: mds };
}

async function addFile(files: VirtualFiles, path: string, file: File): Promise<void> {
  if (file.size > MAX_FILE_BYTES) return;
  files.set(path, new Uint8Array(await file.arrayBuffer()));
}

/** From an `<input type=file>`: a folder (`webkitRelativePath`, rooted at the
 * folder itself) or a handful of loose files (rooted at nothing). */
export async function fromFileList(list: FileList): Promise<Picked> {
  const files = new VirtualFiles();
  for (const file of Array.from(list)) {
    const rel = file.webkitRelativePath;
    const path = rel ? rel.split("/").slice(1).join("/") : file.name;
    if (path.split("/").some((part) => SKIP_DIRS.has(part))) continue;
    if (files.size >= MAX_FILES) break;
    await addFile(files, path, file);
  }
  return finish(files);
}

/** From a drop: files and whole folders, via the entries API. */
export async function fromDrop(items: DataTransferItemList): Promise<Picked> {
  const files = new VirtualFiles();
  const entries = Array.from(items)
    .map((item) => item.webkitGetAsEntry?.())
    .filter((e): e is FileSystemEntry => !!e);
  // A single dropped folder is the root; anything else is dropped loose.
  const rootIsFolder = entries.length === 1 && entries[0].isDirectory;

  const walk = async (entry: FileSystemEntry, prefix: string): Promise<void> => {
    if (files.size >= MAX_FILES) return;
    if (entry.isFile) {
      const file = await new Promise<File>((ok, fail) => (entry as FileSystemFileEntry).file(ok, fail));
      await addFile(files, prefix + entry.name, file);
    } else if (entry.isDirectory && !SKIP_DIRS.has(entry.name)) {
      const reader = (entry as FileSystemDirectoryEntry).createReader();
      // `readEntries` returns at most ~100 at a time; ask until it is empty.
      for (;;) {
        const batch = await new Promise<FileSystemEntry[]>((ok, fail) => reader.readEntries(ok, fail));
        if (batch.length === 0) break;
        for (const child of batch) await walk(child, `${prefix}${entry.name}/`);
      }
    }
  };
  for (const entry of entries) {
    if (rootIsFolder) {
      const reader = (entry as FileSystemDirectoryEntry).createReader();
      for (;;) {
        const batch = await new Promise<FileSystemEntry[]>((ok, fail) => reader.readEntries(ok, fail));
        if (batch.length === 0) break;
        for (const child of batch) await walk(child, "");
      }
    } else {
      await walk(entry, "");
    }
  }
  return finish(files);
}
