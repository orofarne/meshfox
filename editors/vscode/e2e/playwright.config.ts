import { defineConfig } from "@playwright/test";

// A separate, deliberately *not* wired into `web/e2e`'s regular
// Chromium/Firefox suite: every test here launches a real, installed VS
// Code binary via Playwright's Electron support (`_electron.launch()`,
// used directly in `helpers.ts` — there's no `devices["electron"]` preset
// the way there is for a real browser, so this config carries none of
// `web/playwright.config.ts`'s per-browser `projects`/`webServer` machinery).
//
// Why this exists at all: `web/e2e`'s own suite runs against a real browser
// tab, and a real, CDP-trusted Cmd+V there reliably fires a native `paste`
// DOM event — confirmed directly. Real VS Code never does, for the exact
// same trusted keystroke against the exact same input surface (TODO.canvas.md:
// "VSCode: вставка текста (Cmd+V и контекстное меню) в редактор ноды не
// работает") — a gap specific to how VS Code's own webview (an
// extension-independent nested iframe VS Code itself always adds) and
// Electron's native accelerator-driven paste command interact, that a
// browser-only suite structurally cannot catch. `textPasteFallback.ts`'s
// fix, and any future one touching this same real-VS-Code-only path, needs
// a suite that actually drives a real VS Code window to mean anything.
//
// Heavier and slower than `web/e2e` on purpose (see `helpers.ts`'
// `launchVSCode` — a real ~15-20s VS Code + extension-host + meshfox-worker
// startup per test): `workers: 1` and no retries, so a real, unambiguous
// failure is never masked by two overlapping VS Code instances fighting
// over the same real OS clipboard (see `web/e2e/copy-paste.spec.ts`'s own
// doc comment on that exact risk) or hidden behind a flaky-look retry.
//
// Prerequisites this config does *not* build for you (unlike `web/e2e`'s
// own `pretest:e2e`/`webServer`, which build everything they need):
//   1. `cargo build -p meshfox-cli` at the repo root (`helpers.ts` points
//      `meshfox.executablePath` at that debug binary directly, so a
//      frontend-only change never needs a Rust rebuild between runs — see
//      its own comment).
//   2. `npm run build` in `web/` (the debug `meshfox` binary reads `web/dist`
//      fresh off disk on every request, same as `web/e2e` already relies on).
//   3. `npm run compile` in `editors/vscode` (compiles this extension's own
//      `out/extension.js`, which `helpers.ts` loads via
//      `--extensionDevelopmentPath`).
// All three are cheap to re-run and safe to skip once already done for a
// given change — left as manual steps rather than an automated `pretest`
// hook specifically so this suite stays opt-in/manual (see this repo's
// README.md "VS Code end-to-end tests" section) rather than something that
// silently triggers a Rust build on every invocation.
export default defineConfig({
  testDir: ".",
  timeout: 90_000,
  fullyParallel: false,
  workers: 1,
  retries: 0,
  reporter: [["list"]],
});
