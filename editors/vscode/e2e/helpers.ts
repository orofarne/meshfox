import { _electron as electron, type ElectronApplication, type Frame, type Page } from "@playwright/test";
import { existsSync, mkdtempSync, cpSync, mkdirSync, writeFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = dirname(fileURLToPath(import.meta.url));
// `editors/vscode/e2e` -> repo root.
const REPO_ROOT = join(__dirname, "..", "..", "..");
const MESHFOX_BIN = join(REPO_ROOT, "target", "debug", "meshfox");

/** `VSCODE_ELECTRON_PATH` overrides this for anyone whose install isn't at
 * either default location (a different drive/prefix, a non-default
 * channel). macOS/Linux only, matching this extension's own current
 * platform support (`editors/vscode/CHANGELOG.md`'s 0.1.0 entry) — no
 * Windows path is worth guessing here. */
function findVSCodeBinary(): string {
  if (process.env.VSCODE_ELECTRON_PATH) return process.env.VSCODE_ELECTRON_PATH;
  const candidates =
    process.platform === "darwin"
      ? ["/Applications/Visual Studio Code.app/Contents/MacOS/Code"]
      : ["/usr/share/code/code", "/usr/bin/code"];
  const found = candidates.find(existsSync);
  if (found) return found;
  throw new Error(
    `Couldn't find a VS Code install to test against (looked at: ${candidates.join(", ")}). ` +
      `Set VSCODE_ELECTRON_PATH to the actual Electron/Code binary inside your install.`,
  );
}

/** VS Code puts a unix domain socket directly under `--user-data-dir`
 * (`<dir>/1.<n>-main.sock`) — macOS's `sockaddr_un` has a ~104-byte path
 * limit, confirmed directly to break ("EINVAL: invalid argument ...
 * -main.sock") under a long profile-scoped temp path. Short, `/tmp`-rooted
 * dirs side-step it; `mkdtempSync` under a nested per-session temp root
 * (the usual "put scratch files in your own scratch dir" advice) would
 * reintroduce exactly the same failure. */
function shortTempDir(prefix: string): string {
  const dir = mkdtempSync(join("/tmp", prefix));
  return dir;
}

interface VSCodeSession {
  app: ElectronApplication;
  page: Page;
  workspaceDir: string;
  /** Removes every temp dir this session created — user-data, extensions,
   * and the workspace copy alike. Call after `app.close()`. */
  cleanup: () => void;
}

/**
 * Launches a real, installed VS Code in Extension Development Host mode
 * against this repo's own `editors/vscode` extension, opened on a fresh
 * copy of `fixtureName` (from `e2e/fixtures/`). Everything here is
 * throwaway per call — its own user-data-dir, extensions-dir, and
 * workspace copy — so tests never share or accumulate state across runs,
 * same isolation `web/playwright.config.ts` gets from a fresh fixture copy
 * per suite.
 *
 * Points `meshfox.executablePath` (via the workspace's own
 * `.vscode/settings.json`) at this repo's debug binary directly rather
 * than whatever `meshfox` a developer happens to have on `PATH` — see
 * `playwright.config.ts`'s own doc comment for why that's a prerequisite
 * this suite expects already built, not something it builds itself.
 */
export async function launchVSCode(fixtureName: string): Promise<VSCodeSession> {
  const userDataDir = shortTempDir("mfx-e2e-ud-");
  const extensionsDir = shortTempDir("mfx-e2e-ext-");
  const workspaceDir = shortTempDir("mfx-e2e-ws-");

  mkdirSync(join(workspaceDir, ".vscode"), { recursive: true });
  writeFileSync(
    join(workspaceDir, ".vscode", "settings.json"),
    JSON.stringify({ "meshfox.executablePath": MESHFOX_BIN }, null, 2),
  );
  cpSync(join(__dirname, "fixtures", fixtureName), join(workspaceDir, fixtureName));

  const app = await electron.launch({
    executablePath: findVSCodeBinary(),
    args: [
      `--extensionDevelopmentPath=${join(__dirname, "..")}`,
      `--user-data-dir=${userDataDir}`,
      `--extensions-dir=${extensionsDir}`,
      "--skip-release-notes",
      "--skip-welcome",
      "--disable-workspace-trust",
      "--new-window",
      workspaceDir,
      join(workspaceDir, fixtureName),
    ],
    timeout: 60_000,
  });

  const page = await app.firstWindow();
  await page.waitForTimeout(6000);

  return {
    app,
    page,
    workspaceDir,
    cleanup: () => {
      for (const dir of [userDataDir, extensionsDir, workspaceDir]) {
        rmSync(dir, { recursive: true, force: true });
      }
    },
  };
}

/**
 * Switches the active tab to the meshfox canvas editor and returns its
 * real content frame (see this function's own inline comments for why
 * that takes two steps). Confirmed directly: a cold profile's first-ever
 * open of a `*.canvas.md` file doesn't auto-pick this extension's
 * `"priority": "default"` custom editor the way a normal, already-used VS
 * Code profile does for a real user — a separate quirk from anything this
 * suite means to test, worked around here via the same manual "Reopen
 * Editor With..." VS Code itself offers.
 */
export async function openMeshfoxCanvas(page: Page): Promise<Frame> {
  await page.keyboard.press("Meta+Shift+KeyP");
  await page.waitForTimeout(400);
  await page.keyboard.insertText("Reopen Editor With...");
  await page.waitForTimeout(600);
  await page.keyboard.press("Enter");
  await page.waitForTimeout(600);
  await page.getByText("meshfox canvas", { exact: true }).first().click();
  await page.waitForTimeout(4000);

  // VS Code's own generic webview *host* page (loaded at a
  // `vscode-webview://.../index.html` URL) is not this extension's
  // content — it's VS Code's own bootstrap script, present for every
  // webview any extension ever shows. It embeds a further child iframe
  // ("active-frame") that starts on a "fake.html" placeholder and later
  // gets the extension's real `webview.html` swapped into it *in place*
  // (confirmed directly: Playwright/CDP keeps reporting that same
  // "fake.html" URL for this frame even once real content — real buttons,
  // real text — is visibly rendered inside it; the swap isn't a normal
  // navigation). `childFrames()[0]` is that real content frame regardless
  // of what its own `.url()` still claims.
  const hostFrame = page
    .frames()
    .find((f) => f.url().includes("vscode-webview://") && f.url().includes("index.html"));
  if (!hostFrame) {
    throw new Error(`No meshfox webview host frame found. Frames: ${page.frames().map((f) => f.url())}`);
  }
  const contentFrame = hostFrame.childFrames()[0];
  if (!contentFrame) throw new Error("Webview host frame has no content child frame.");
  return contentFrame;
}

/** Selects the root node and opens its own body editor (`NodeTextEditor`)
 * — the starting point most tests here need. Assumes Edit mode is already
 * on (see `enterEditMode`) — the toolbar's node-selection/toolbar-button
 * affordances this uses don't exist in read-only mode at all. */
export async function openRootBodyEditor(frame: Frame): Promise<void> {
  await frame.locator(".mesh-node-title-text, .mesh-node-title-centered-text").first().click();
  await frame.locator('.mesh-node-toolbar button[title*="Edit this node" i]').click();
}

/** Clicks the toolbar's read-only → editing toggle once. Every test in
 * this suite needs Edit mode (the "Source" toggle, node toolbars, and
 * NodeSettings are all edit-mode-only), so this is meant to run once per
 * session rather than per test. */
export async function enterEditMode(frame: Frame): Promise<void> {
  await frame.getByRole("button", { name: "Edit" }).click();
}
