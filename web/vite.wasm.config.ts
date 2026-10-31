import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// The serverless prototype: the same app as `vite.config.ts`, but its entry is
// `wasm.html`, whose script swaps the HTTP backend for the WebAssembly one
// (see `wasm-demo/`). A separate build, into `dist-wasm/`, so the `web/dist`
// that `meshfox view` embeds is untouched. Run `scripts/build-wasm.sh web
// crates/wasm/pkg` first (`npm run build:wasm` does both).
export default defineConfig({
  base: "./",
  plugins: [react()],
  build: {
    outDir: "dist-wasm",
    emptyOutDir: true,
    chunkSizeWarningLimit: 4000,
    rollupOptions: { input: "wasm.html" },
  },
  // The bundled examples and the generated wasm package live above `web/`.
  server: { fs: { allow: [".."] } },
});
