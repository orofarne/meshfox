import { defineConfig } from "vite";

// Builds `src/siteHighlight.ts` into one classic script for the static-site
// templates (not part of the web UI bundle) — run via
// `npm run build:site-highlight`, output committed next to the template.
export default defineConfig({
  publicDir: false,
  build: {
    outDir: "../site-template-archive",
    emptyOutDir: false,
    lib: {
      entry: "src/siteHighlight.ts",
      formats: ["iife"],
      name: "meshfoxSiteHighlight",
      fileName: () => "highlight.js",
    },
    minify: true,
    reportCompressedSize: true,
  },
});
