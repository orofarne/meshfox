import { VirtualFiles } from "./files";

/** Puts newly picked files into the opened tree, where the document looks for
 * them. A file lands exactly where a missing target (as written in the
 * document, relative to the canvas) points when the picked path is that
 * target or its tail — so picking `notes.md` alone fills `./docs/notes.md`
 * — and otherwise at its own path: relative to the folder root for a folder
 * (assumed to be the same root as the first pick), beside the canvas for
 * loose files. Returns how many files were added. */
export function addPicked(
  files: VirtualFiles,
  canvasDir: string,
  missingTargets: string[],
  picked: VirtualFiles,
  loose: boolean,
): number {
  const wanted = missingTargets.map((t) => VirtualFiles.join(canvasDir, t.split("#")[0]));
  let added = 0;
  for (const path of picked.paths()) {
    const bytes = picked.get(path)!;
    const hit = wanted.find((w) => w === path || w.endsWith(`/${path}`));
    files.set(hit ?? VirtualFiles.join(loose ? canvasDir : "", path), bytes);
    added++;
  }
  return added;
}
