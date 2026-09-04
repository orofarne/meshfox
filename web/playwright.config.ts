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
// Fifth server + port for fold.spec.ts — same reasoning again, its own
// fixture (fold.canvas.md, a parent with two children plus an auto-placed
// sibling) and port, so a fold-state localStorage key or a reload doesn't
// collide with any other suite's server/canvas.
const FOLD_PORT = 4594;
// Sixth server + port for keyboard-nav.spec.ts — same reasoning again, its
// own fixture (keyboard-nav.canvas.md) and port.
const KEYBOARD_NAV_PORT = 4595;
// Seventh server + port for group-drag.spec.ts — same reasoning again, its
// own fixture (group-drag.canvas.md, a group with a real anchor plus two
// real, group-relative members) and port, so its own Edit-mode drags never
// collide with any other suite's server/canvas.
const GROUP_DRAG_PORT = 4596;
// Eighth server + port for group-enter.spec.ts — same reasoning again, its
// own fixture (group-enter.canvas.md) and port.
const GROUP_ENTER_PORT = 4597;
// Ninth server + port for document-options.spec.ts — same reasoning
// again, its own fixture (document-options.canvas.md) and port, so its
// own `PUT /api/options` writes never collide with any other suite's
// server/canvas.
const DOCUMENT_OPTIONS_PORT = 4598;
// Tenth server + port for default-fold.spec.ts — same reasoning again,
// its own fixture (default-fold.canvas.md, deliberately undeclaring
// `unfold` so `resolveDefaultFold`'s own default is what's under test)
// and port.
const DEFAULT_FOLD_PORT = 4599;
// Eleventh server + port for quick-run.spec.ts — same reasoning again, its
// own fixture (quick-run.canvas.md) and port, so its `tty` run never
// collides with any other suite's server/canvas.
const QUICK_RUN_PORT = 4600;
// Twelfth server + port for edge-routing.spec.ts — same reasoning again,
// its own fixture (edge-routing.canvas.md, a root with two children, one
// of which has a child of its own) and port.
const EDGE_ROUTING_PORT = 4601;
// Thirteenth server + port for vars-form.spec.ts — same reasoning again,
// its own fixture (vars-form.canvas.md, a `choices_var` chain reaching a
// `from=`-computed variable) and port.
const VARS_FORM_PORT = 4602;
// Fourteenth server + port for move-sibling.spec.ts — same reasoning
// again, its own fixture (move-sibling.canvas.md, three auto-placed
// siblings with a positioned one sandwiched between two of them) and
// port, so its own ↑/↓ reorder writes never collide with any other
// suite's server/canvas.
const MOVE_SIBLING_PORT = 4603;
// Fifteenth server + port for clear-node-layout.spec.ts — same reasoning
// again, its own fixture (clear-node-layout.canvas.md, one positioned and
// one auto-placed node) and port, so its own ↺ reset writes never collide
// with any other suite's server/canvas.
const CLEAR_NODE_LAYOUT_PORT = 4604;
// Sixteenth server + port for image-paste.spec.ts — same reasoning again,
// its own fixture (image-paste.canvas.md, one bare root) and port, so its
// own node-body/source saves never collide with any other suite's
// server/canvas.
const IMAGE_PASTE_PORT = 4605;
// Seventeenth server + port for markdown-extensions.spec.ts — same
// reasoning again, its own fixture (markdown-extensions.canvas.md, one
// bare root exercising image-attrs/subsup/GFM-alert markup) and port;
// this suite never writes to its canvas, but every other one still gets
// its own server, so this one does too.
const MARKDOWN_EXTENSIONS_PORT = 4606;
// Eighteenth server + port for search.spec.ts — same reasoning again, its
// own fixture (search.canvas.md, two independently-foldable branches each
// with one matching leaf) and port, so its own fold/unfold-via-search
// writes never collide with any other suite's server/canvas.
const SEARCH_PORT = 4607;
// Nineteenth server + port for search-pan.spec.ts — split out from
// search.spec.ts (own fixture, search-pan.canvas.md) rather than sharing
// its port: its one node is deliberately taller than the browser viewport,
// which wrecks `clickFitViewAndWait`'s fit-to-everything zoom for every
// other node on a shared canvas (confirmed directly) — see the fixture's
// own doc comment.
const SEARCH_PAN_PORT = 4608;
// Twentieth server + port for autofit-title.spec.ts — same reasoning
// again, its own fixture (autofit-title.canvas.md, header-only nodes with
// an authored `width`/`height` — see `MeshNode.tsx`'s `useAutoFitTitleFontSize`)
// and port.
const AUTOFIT_TITLE_PORT = 4609;

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
  // Every `testMatch` below is anchored to the *end* of the path, requiring
  // a `/` or start-of-string right before the filename (`(^|\/)name\.spec\.ts$`)
  // — a bare `$`-only anchor still isn't enough on its own: Playwright
  // matches against each file's full path (test dir prefix and all), which
  // an anchor covering the *whole* string would never match. Left fully
  // unanchored (as this used to be), `/fold\.spec\.ts/` would also match
  // `default-fold.spec.ts` (a valid substring), silently running it as
  // part of the wrong project against the wrong fixture/server.
  projects: (
    [
      ["chrome", devices["Desktop Chrome"]],
      ["firefox", devices["Desktop Firefox"]],
    ] as const
  ).flatMap(([browser, device]) => [
    { name: `${browser}-deps`, testMatch: /(^|\/)deps\.spec\.ts$/, use: { ...device, viewport: VIEWPORT } },
    {
      name: `${browser}-scroll`,
      testMatch: /(^|\/)scroll\.spec\.ts$/,
      use: { ...device, viewport: VIEWPORT, baseURL: `http://127.0.0.1:${SCROLL_PORT}` },
    },
    {
      name: `${browser}-select`,
      testMatch: /(^|\/)select\.spec\.ts$/,
      use: { ...device, viewport: VIEWPORT, baseURL: `http://127.0.0.1:${SELECT_PORT}` },
    },
    {
      name: `${browser}-settings`,
      testMatch: /(^|\/)settings\.spec\.ts$/,
      use: { ...device, viewport: VIEWPORT, baseURL: `http://127.0.0.1:${SETTINGS_PORT}` },
    },
    {
      name: `${browser}-fold`,
      testMatch: /(^|\/)fold\.spec\.ts$/,
      use: { ...device, viewport: VIEWPORT, baseURL: `http://127.0.0.1:${FOLD_PORT}` },
    },
    {
      name: `${browser}-keyboard-nav`,
      testMatch: /(^|\/)keyboard-nav\.spec\.ts$/,
      use: { ...device, viewport: VIEWPORT, baseURL: `http://127.0.0.1:${KEYBOARD_NAV_PORT}` },
    },
    {
      name: `${browser}-group-drag`,
      testMatch: /(^|\/)group-drag\.spec\.ts$/,
      use: { ...device, viewport: VIEWPORT, baseURL: `http://127.0.0.1:${GROUP_DRAG_PORT}` },
    },
    {
      name: `${browser}-group-enter`,
      testMatch: /(^|\/)group-enter\.spec\.ts$/,
      use: { ...device, viewport: VIEWPORT, baseURL: `http://127.0.0.1:${GROUP_ENTER_PORT}` },
    },
    {
      name: `${browser}-document-options`,
      testMatch: /(^|\/)document-options\.spec\.ts$/,
      use: { ...device, viewport: VIEWPORT, baseURL: `http://127.0.0.1:${DOCUMENT_OPTIONS_PORT}` },
    },
    {
      name: `${browser}-default-fold`,
      testMatch: /(^|\/)default-fold\.spec\.ts$/,
      use: { ...device, viewport: VIEWPORT, baseURL: `http://127.0.0.1:${DEFAULT_FOLD_PORT}` },
    },
    {
      name: `${browser}-quick-run`,
      testMatch: /(^|\/)quick-run\.spec\.ts$/,
      use: { ...device, viewport: VIEWPORT, baseURL: `http://127.0.0.1:${QUICK_RUN_PORT}` },
    },
    {
      name: `${browser}-edge-routing`,
      testMatch: /(^|\/)edge-routing\.spec\.ts$/,
      use: { ...device, viewport: VIEWPORT, baseURL: `http://127.0.0.1:${EDGE_ROUTING_PORT}` },
    },
    {
      name: `${browser}-vars-form`,
      testMatch: /(^|\/)vars-form\.spec\.ts$/,
      use: { ...device, viewport: VIEWPORT, baseURL: `http://127.0.0.1:${VARS_FORM_PORT}` },
    },
    {
      name: `${browser}-move-sibling`,
      testMatch: /(^|\/)move-sibling\.spec\.ts$/,
      use: { ...device, viewport: VIEWPORT, baseURL: `http://127.0.0.1:${MOVE_SIBLING_PORT}` },
    },
    {
      name: `${browser}-clear-node-layout`,
      testMatch: /(^|\/)clear-node-layout\.spec\.ts$/,
      use: { ...device, viewport: VIEWPORT, baseURL: `http://127.0.0.1:${CLEAR_NODE_LAYOUT_PORT}` },
    },
    {
      name: `${browser}-image-paste`,
      testMatch: /(^|\/)image-paste\.spec\.ts$/,
      use: { ...device, viewport: VIEWPORT, baseURL: `http://127.0.0.1:${IMAGE_PASTE_PORT}` },
    },
    {
      name: `${browser}-markdown-extensions`,
      testMatch: /(^|\/)markdown-extensions\.spec\.ts$/,
      use: { ...device, viewport: VIEWPORT, baseURL: `http://127.0.0.1:${MARKDOWN_EXTENSIONS_PORT}` },
    },
    {
      name: `${browser}-search`,
      testMatch: /(^|\/)search\.spec\.ts$/,
      use: { ...device, viewport: VIEWPORT, baseURL: `http://127.0.0.1:${SEARCH_PORT}` },
    },
    {
      name: `${browser}-search-pan`,
      testMatch: /(^|\/)search-pan\.spec\.ts$/,
      use: { ...device, viewport: VIEWPORT, baseURL: `http://127.0.0.1:${SEARCH_PAN_PORT}` },
    },
    {
      name: `${browser}-autofit-title`,
      testMatch: /(^|\/)autofit-title\.spec\.ts$/,
      use: { ...device, viewport: VIEWPORT, baseURL: `http://127.0.0.1:${AUTOFIT_TITLE_PORT}` },
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
    {
      command: `cargo run -q --manifest-path ../Cargo.toml -p meshfox-cli -- view e2e/fixtures/fold.canvas.md --port ${FOLD_PORT} --no-open --no-auto-exit`,
      url: `http://127.0.0.1:${FOLD_PORT}/api/canvas`,
      reuseExistingServer: !process.env.CI,
      timeout: 60_000,
    },
    {
      command: `cargo run -q --manifest-path ../Cargo.toml -p meshfox-cli -- view e2e/fixtures/keyboard-nav.canvas.md --port ${KEYBOARD_NAV_PORT} --no-open --no-auto-exit`,
      url: `http://127.0.0.1:${KEYBOARD_NAV_PORT}/api/canvas`,
      reuseExistingServer: !process.env.CI,
      timeout: 60_000,
    },
    {
      command: `cargo run -q --manifest-path ../Cargo.toml -p meshfox-cli -- view e2e/fixtures/group-drag.canvas.md --port ${GROUP_DRAG_PORT} --no-open --no-auto-exit`,
      url: `http://127.0.0.1:${GROUP_DRAG_PORT}/api/canvas`,
      reuseExistingServer: !process.env.CI,
      timeout: 60_000,
    },
    {
      command: `cargo run -q --manifest-path ../Cargo.toml -p meshfox-cli -- view e2e/fixtures/group-enter.canvas.md --port ${GROUP_ENTER_PORT} --no-open --no-auto-exit`,
      url: `http://127.0.0.1:${GROUP_ENTER_PORT}/api/canvas`,
      reuseExistingServer: !process.env.CI,
      timeout: 60_000,
    },
    {
      command: `cargo run -q --manifest-path ../Cargo.toml -p meshfox-cli -- view e2e/fixtures/document-options.canvas.md --port ${DOCUMENT_OPTIONS_PORT} --no-open --no-auto-exit`,
      url: `http://127.0.0.1:${DOCUMENT_OPTIONS_PORT}/api/canvas`,
      reuseExistingServer: !process.env.CI,
      timeout: 60_000,
    },
    {
      command: `cargo run -q --manifest-path ../Cargo.toml -p meshfox-cli -- view e2e/fixtures/default-fold.canvas.md --port ${DEFAULT_FOLD_PORT} --no-open --no-auto-exit`,
      url: `http://127.0.0.1:${DEFAULT_FOLD_PORT}/api/canvas`,
      reuseExistingServer: !process.env.CI,
      timeout: 60_000,
    },
    {
      command: `cargo run -q --manifest-path ../Cargo.toml -p meshfox-cli -- view e2e/fixtures/quick-run.canvas.md --port ${QUICK_RUN_PORT} --no-open --no-auto-exit`,
      url: `http://127.0.0.1:${QUICK_RUN_PORT}/api/canvas`,
      reuseExistingServer: !process.env.CI,
      timeout: 60_000,
    },
    {
      command: `cargo run -q --manifest-path ../Cargo.toml -p meshfox-cli -- view e2e/fixtures/edge-routing.canvas.md --port ${EDGE_ROUTING_PORT} --no-open --no-auto-exit`,
      url: `http://127.0.0.1:${EDGE_ROUTING_PORT}/api/canvas`,
      reuseExistingServer: !process.env.CI,
      timeout: 60_000,
    },
    {
      command: `cargo run -q --manifest-path ../Cargo.toml -p meshfox-cli -- view e2e/fixtures/vars-form.canvas.md --port ${VARS_FORM_PORT} --no-open --no-auto-exit`,
      url: `http://127.0.0.1:${VARS_FORM_PORT}/api/canvas`,
      reuseExistingServer: !process.env.CI,
      timeout: 60_000,
    },
    {
      command: `cargo run -q --manifest-path ../Cargo.toml -p meshfox-cli -- view e2e/fixtures/move-sibling.canvas.md --port ${MOVE_SIBLING_PORT} --no-open --no-auto-exit`,
      url: `http://127.0.0.1:${MOVE_SIBLING_PORT}/api/canvas`,
      reuseExistingServer: !process.env.CI,
      timeout: 60_000,
    },
    {
      command: `cargo run -q --manifest-path ../Cargo.toml -p meshfox-cli -- view e2e/fixtures/clear-node-layout.canvas.md --port ${CLEAR_NODE_LAYOUT_PORT} --no-open --no-auto-exit`,
      url: `http://127.0.0.1:${CLEAR_NODE_LAYOUT_PORT}/api/canvas`,
      reuseExistingServer: !process.env.CI,
      timeout: 60_000,
    },
    {
      command: `cargo run -q --manifest-path ../Cargo.toml -p meshfox-cli -- view e2e/fixtures/image-paste.canvas.md --port ${IMAGE_PASTE_PORT} --no-open --no-auto-exit`,
      url: `http://127.0.0.1:${IMAGE_PASTE_PORT}/api/canvas`,
      reuseExistingServer: !process.env.CI,
      timeout: 60_000,
    },
    {
      command: `cargo run -q --manifest-path ../Cargo.toml -p meshfox-cli -- view e2e/fixtures/markdown-extensions.canvas.md --port ${MARKDOWN_EXTENSIONS_PORT} --no-open --no-auto-exit`,
      url: `http://127.0.0.1:${MARKDOWN_EXTENSIONS_PORT}/api/canvas`,
      reuseExistingServer: !process.env.CI,
      timeout: 60_000,
    },
    {
      command: `cargo run -q --manifest-path ../Cargo.toml -p meshfox-cli -- view e2e/fixtures/search.canvas.md --port ${SEARCH_PORT} --no-open --no-auto-exit`,
      url: `http://127.0.0.1:${SEARCH_PORT}/api/canvas`,
      reuseExistingServer: !process.env.CI,
      timeout: 60_000,
    },
    {
      command: `cargo run -q --manifest-path ../Cargo.toml -p meshfox-cli -- view e2e/fixtures/search-pan.canvas.md --port ${SEARCH_PAN_PORT} --no-open --no-auto-exit`,
      url: `http://127.0.0.1:${SEARCH_PAN_PORT}/api/canvas`,
      reuseExistingServer: !process.env.CI,
      timeout: 60_000,
    },
    {
      command: `cargo run -q --manifest-path ../Cargo.toml -p meshfox-cli -- view e2e/fixtures/autofit-title.canvas.md --port ${AUTOFIT_TITLE_PORT} --no-open --no-auto-exit`,
      url: `http://127.0.0.1:${AUTOFIT_TITLE_PORT}/api/canvas`,
      reuseExistingServer: !process.env.CI,
      timeout: 60_000,
    },
  ],
});
