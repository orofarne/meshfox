import { defineConfig, devices } from "@playwright/test";

// Drives the real `meshfox view` server (embedded UI + axum backend +
// actual bash execution), not a mocked frontend — the two bugs this suite
// exists to catch (a clipped dependency badge, a box-shadow eaten by
// `overflow: hidden`) were only visible against the genuinely rendered,
// genuinely laid-out canvas. See README.md's "End-to-end tests" section
// for the full pipeline this config drives.
const PORT = 4590;
// Separate server + port for scroll.spec.ts: it needs its own fixture
// (scroll.canvas.md, with nodes sized via explicit w=/h= to force specific
// overflow combinations) rather than deps.canvas.md's content, so it can't
// share the server above — a second `webServer` entry keeps the two fixture
// documents, and their servers, from ever fighting over the same canvas file.
const SCROLL_PORT = 4591;
// Third server + port for select.spec.ts — same reasoning as SCROLL_PORT
// above, just its own fixture (select.canvas.md) and port.
const SELECT_PORT = 4592;
// Fourth server + port for settings.spec.ts — same reasoning again, its own
// fixture (settings.canvas.md, one node per NodeSettings type/field
// combination) and port, so its Edit-mode writes (or, if a regression
// reintroduces one, unwanted writes) can't collide with any other suite's
// canvas file.
const SETTINGS_PORT = 4593;

// Taller than Playwright's 720px default — the app's own `minZoom` (0.5)
// is a hard floor on how far "fit view" can zoom out, and deps.canvas.md's
// content needs more room than a 720px-tall viewport gives it at that
// floor to fit without clipping the last node or two. Chromium's
// CDP-based click dispatch tolerated that clipping (it can still click a
// coordinate beyond what's actually visible); Firefox's driver correctly
// refuses to click a target that isn't genuinely in-viewport. A taller
// viewport sidesteps the floor entirely instead of relying on that
// Chromium leniency. Set per-project below (each `devices["Desktop …"]`
// preset already carries its own `viewport`, which would otherwise win
// over a top-level `use.viewport`), not just once at the top level.
const VIEWPORT = { width: 1280, height: 1600 };

export default defineConfig({
  testDir: "./e2e",
  timeout: 30_000,
  fullyParallel: false,
  forbidOnly: !!process.env.CI,
  retries: process.env.CI ? 1 : 0,
  reporter: [["list"]],
  use: {
    baseURL: `http://127.0.0.1:${PORT}`,
    trace: "retain-on-failure",
    screenshot: "only-on-failure",
  },
  // One (deps/scroll/select) trio of projects per browser — `<browser>-deps`,
  // `<browser>-scroll`, `<browser>-select`, e.g. `chrome-deps`/`firefox-deps`.
  projects: (
    [
      ["chrome", devices["Desktop Chrome"]],
      ["firefox", devices["Desktop Firefox"]],
    ] as const
  ).flatMap(([browser, device]) => [
    { name: `${browser}-deps`, testMatch: /deps\.spec\.ts/, use: { ...device, viewport: VIEWPORT } },
    {
      name: `${browser}-scroll`,
      testMatch: /scroll\.spec\.ts/,
      use: { ...device, viewport: VIEWPORT, baseURL: `http://127.0.0.1:${SCROLL_PORT}` },
    },
    {
      name: `${browser}-select`,
      testMatch: /select\.spec\.ts/,
      use: { ...device, viewport: VIEWPORT, baseURL: `http://127.0.0.1:${SELECT_PORT}` },
    },
    {
      name: `${browser}-settings`,
      testMatch: /settings\.spec\.ts/,
      use: { ...device, viewport: VIEWPORT, baseURL: `http://127.0.0.1:${SETTINGS_PORT}` },
    },
  ]),
  webServer: [
    {
      // `web/dist` must already be built (see the `pretest:e2e` npm script) —
      // `cargo run` here only builds/starts the Rust side. Debug builds of
      // meshfox-server read `web/dist` fresh off disk on every request
      // (rust-embed's `debug-embed` feature is off — see its Cargo.toml),
      // so no Rust rebuild is needed between a frontend change and the next
      // test run, just `npm run build` again.
      // `--no-auto-exit`: `meshfox view` otherwise exits a few seconds after
      // its last connected tab closes (see README's roadmap) — Playwright
      // opens/closes a fresh page between tests against this one shared
      // server, which would otherwise risk killing it mid-suite during that
      // gap.
      command: `cargo run -q --manifest-path ../Cargo.toml -p meshfox-cli -- view e2e/fixtures/deps.canvas.md --port ${PORT} --no-open --no-auto-exit`,
      url: `http://127.0.0.1:${PORT}/api/canvas`,
      reuseExistingServer: !process.env.CI,
      timeout: 60_000,
    },
    {
      command: `cargo run -q --manifest-path ../Cargo.toml -p meshfox-cli -- view e2e/fixtures/scroll.canvas.md --port ${SCROLL_PORT} --no-open --no-auto-exit`,
      url: `http://127.0.0.1:${SCROLL_PORT}/api/canvas`,
      reuseExistingServer: !process.env.CI,
      timeout: 60_000,
    },
    {
      command: `cargo run -q --manifest-path ../Cargo.toml -p meshfox-cli -- view e2e/fixtures/select.canvas.md --port ${SELECT_PORT} --no-open --no-auto-exit`,
      url: `http://127.0.0.1:${SELECT_PORT}/api/canvas`,
      reuseExistingServer: !process.env.CI,
      timeout: 60_000,
    },
    {
      command: `cargo run -q --manifest-path ../Cargo.toml -p meshfox-cli -- view e2e/fixtures/settings.canvas.md --port ${SETTINGS_PORT} --no-open --no-auto-exit`,
      url: `http://127.0.0.1:${SETTINGS_PORT}/api/canvas`,
      reuseExistingServer: !process.env.CI,
      timeout: 60_000,
    },
  ],
});
