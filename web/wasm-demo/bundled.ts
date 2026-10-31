/// <reference types="vite/client" />
// Example documents shipped with the demo, so it can be tried without having
// a canvas at hand. Their paths are relative to the repository root, exactly
// as in the repo, so the `examples/...` and `./TEMPLATES.md` links inside keep
// working. (Not all of `examples/`: it holds hundreds of MB of tool state.)
import { VirtualFiles } from "./files";

const canvases = import.meta.glob("../../examples/*.canvas.md", { query: "?raw", import: "default" });
const data = import.meta.glob(
  "../../examples/{constraints,runnable-files,canvas-call,interpreters,services}-data/**/*.{md,json,csv,txt,yaml,yml,toml,py,sh}",
  { query: "?raw", import: "default" },
);
const images = import.meta.glob(
  [
    "../../examples/*.{png,svg,jpg,jpeg,gif,webp}",
    "../../examples/{constraints,runnable-files,canvas-call,interpreters,services}-data/**/*.{png,svg,jpg,jpeg,gif,webp}",
  ],
  { query: "?url", import: "default", eager: true },
);
const docs = import.meta.glob("../../{README,SPEC,TEMPLATES,MARKDOWN}.md", { query: "?raw", import: "default" });

const repoPath = (globKey: string) => globKey.replace(/^\.\.\/\.\.\//, "");

export interface Example {
  path: string;
  label: string;
}

export const EXAMPLES: Example[] = [
  ...Object.keys(canvases).map(repoPath).sort(),
  ...["README.md", "SPEC.md"],
].map((path) => ({ path, label: path.replace(/^examples\//, "").replace(/\.canvas\.md$/, "") }));

/** Loads one example and everything it may link to into a fresh tree. */
export async function loadExample(): Promise<VirtualFiles> {
  const files = new VirtualFiles();
  const all = { ...canvases, ...data, ...docs };
  await Promise.all(
    Object.entries(all).map(async ([key, load]) => files.set(repoPath(key), (await load()) as string)),
  );
  for (const [key, url] of Object.entries(images)) files.setUrl(repoPath(key), url as string);
  return files;
}
