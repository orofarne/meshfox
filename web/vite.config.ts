import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// The dev server proxies /api to meshfoxd (default port 4590) so `npm run
// dev` works standalone; in production meshfoxd serves this app's build
// output directly, same-origin.
export default defineConfig({
  plugins: [react()],
  server: {
    proxy: {
      "/api": "http://127.0.0.1:4590",
    },
  },
});
