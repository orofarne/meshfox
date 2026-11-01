import { defineConfig, type Plugin } from "vite";
import fs from "node:fs";
import path from "node:path";
import react from "@vitejs/plugin-react";

// The in-browser build: the same app as `vite.config.ts`, but its entry is
// `editor.html`, whose script swaps the HTTP backend for the WebAssembly one
// (see `wasm-demo/`). A separate build, into `dist-wasm/`, so the `web/dist`
// that `meshfox view` embeds is untouched. Run `scripts/build-wasm.sh web
// crates/wasm/pkg` first (`npm run build:wasm` does both).
//
// The output is a plain static site with relative URLs (`base: "./"`), so it
// can be served from any path, e.g. https://meshfox.orofarne.net/editor/ —
// the page is emitted as `index.html` for that.
const pageAsIndex: Plugin = {
  name: "editor-page-as-index",
  apply: "build",
  closeBundle() {
    const dir = path.resolve(__dirname, "dist-wasm");
    if (fs.existsSync(path.join(dir, "editor.html"))) {
      fs.renameSync(path.join(dir, "editor.html"), path.join(dir, "index.html"));
    }
  },
};

export default defineConfig({
  base: "./",
  plugins: [react(), pageAsIndex],
  build: {
    outDir: "dist-wasm",
    emptyOutDir: true,
    chunkSizeWarningLimit: 4000,
    rollupOptions: { input: "editor.html" },
  },
  // The generated wasm package lives above `web/`.
  server: { fs: { allow: [".."] } },
});
