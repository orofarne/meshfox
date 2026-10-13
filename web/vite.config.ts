import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// The dev server proxies /api to meshfoxd (default port 4590) so `npm run
// dev` works standalone; in production meshfoxd serves this app's build
// output directly, same-origin.
export default defineConfig({
  plugins: [react()],
  build: {
    // Monaco's core (~2.5 MB) and a few on-demand Shiki grammars (cpp,
    // emacs-lisp, wasm: 600-800 kB each) are already lazy-loaded chunks
    // that can't be split further; the default 500 kB limit just warned
    // about them on every build.
    chunkSizeWarningLimit: 3000,
  },
  server: {
    proxy: {
      "/api": "http://127.0.0.1:4590",
    },
  },
});
