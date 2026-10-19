import assert from "node:assert/strict";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { createServer } from "vite";
import { chromium, expect } from "@playwright/test";

// Exercise the real TtyPanel, including its binary WebSocket -> term.write
// path. Only the PTY peer is mocked; no Rust build or canvas mutation needed.
test("TTY panels render SIXEL pixels and still send keyboard input", async () => {
  const route = "/__tty_images_test__";
  const server = await createServer({
    root: fileURLToPath(new URL("../", import.meta.url)),
    server: { port: 0 },
    plugins: [{
      name: "tty-image-test-harness",
      resolveId(id) { if (id === `${route}.tsx`) return id; },
      load(id) {
        if (id !== `${route}.tsx`) return;
        return `
          import React from 'react';
          import { createRoot } from 'react-dom/client';
          import { TtyPanel } from '/src/TtyPanel.tsx';
          import '/src/index.css';
          createRoot(document.getElementById('root')).render(
            <TtyPanel path={[]} blockName="image" withDeps={false}
              persist={false} autoclose={false} onClose={() => {}} />
          );`;
      },
      configureServer(vite) {
        vite.middlewares.use(async (req, res, next) => {
          if (req.url !== route) return next();
          res.setHeader("Content-Type", "text/html");
          res.end(await vite.transformIndexHtml(route,
            `<div id="root"></div><script type="module" src="${route}.tsx"></script>`));
        });
      },
    }],
  });
  let browser;
  try {
    await server.listen();
    browser = await chromium.launch();
    const page = await browser.newPage({ viewport: { width: 1200, height: 800 } });
    const errors = [];
    const input = [];
    page.on("pageerror", error => errors.push(error.message));
    await page.routeWebSocket(/\/api\/run\/tty\?/, socket => {
      socket.onMessage(data => input.push(data.toString()));
      socket.send(JSON.stringify({ type: "tty-start", nodeId: "root", block: "image" }));
      // A 24 x 12 red rectangle, two six-pixel bands, with raster attributes.
      socket.send(Buffer.from('\x1bPq"1;1;24;12#1;2;100;0;0#1!24~-!24~\x1b\\'));
    });
    await page.goto(`${server.resolvedUrls.local[0].replace(/\/$/, "")}${route}`);
    await page.waitForFunction(() => {
      const canvas = document.querySelector("canvas.xterm-image-layer");
      if (!canvas) return false;
      const pixels = canvas.getContext("2d").getImageData(0, 0, canvas.width, canvas.height).data;
      let red = 0;
      for (let i = 0; i < pixels.length; i += 4) {
        if (pixels[i] > 240 && pixels[i + 1] < 10 && pixels[i + 2] < 10 && pixels[i + 3] > 240) red++;
      }
      return red >= 24 * 12;
    });
    await page.locator(".xterm-helper-textarea").focus();
    await page.keyboard.type("hello");
    await expect.poll(() => input.join("")).toContain("hello");
    assert.deepEqual(errors, []);
  } finally {
    await browser?.close();
    await server.close();
  }
});
