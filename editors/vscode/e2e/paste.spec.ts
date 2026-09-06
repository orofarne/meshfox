import { test, expect, type Frame, type Page } from "@playwright/test";
import { enterEditMode, launchVSCode, openMeshfoxCanvas, openRootBodyEditor } from "./helpers";

// Drives a real, installed VS Code (not headless Chromium — see
// playwright.config.ts's own doc comment for why a browser-only suite
// structurally can't catch what this one exists for) through
// `editors/vscode`'s own Extension Development Host, exercising the exact
// real-VS-Code-only paste gap `web/src/textPasteFallback.ts` fixes
// (TODO.canvas.md: "VSCode: вставка текста (Cmd+V и контекстное меню) в
// редактор ноды не работает") — and, since that fix is wired up only for
// Monaco (`attachMeshfoxEditorExtensions`), whether the *same* real-VS-Code
// gap also reaches the app's plain `<input>` fields (the node body editor's
// own title field, NodeSettings' ID field), which never went through that
// fix at all.
//
// One shared VS Code window for the whole file (`test.describe.serial` +
// `beforeAll`/`afterAll`) rather than one per test — a real launch here
// costs ~15-20s (extension host + meshfox worker spawn), and unlike
// `web/e2e/copy-paste.spec.ts`'s browser suite there's no second project
// (no Firefox variant) that could ever race this one for the real OS
// clipboard, so sharing one session carries none of that suite's own
// cross-project isolation concern. Each test still copies its own marker
// text immediately before pasting it (never relies on a previous test's
// clipboard content), and always pastes into a field that started this
// test empty or at a known cursor position, so leftover state from an
// earlier test can't produce a false pass.

let session: Awaited<ReturnType<typeof launchVSCode>>;
let page: Page;
let frame: Frame;

test.describe.serial("real VS Code paste", () => {
  test.beforeAll(async () => {
    session = await launchVSCode("paste.canvas.md");
    page = session.page;
    frame = await openMeshfoxCanvas(page);
    await enterEditMode(frame);
  });

  test.afterAll(async () => {
    await session.app.close();
    session.cleanup();
  });

  function selectRoot() {
    return frame.locator('.react-flow__node[data-id="root"]').locator(".mesh-node-title-text, .mesh-node-title-centered-text").first().click();
  }

  /** Copies `marker` onto the real, global clipboard via NodeSettings'
   * own Title field — a plain `<input>`, not Monaco, deliberately: no
   * `EditContext` involved (so none of the keyboard-selection caveats
   * `web/e2e/copy-paste.spec.ts` documents for Monaco apply), and
   * NodeSettings' "cancel" discards its draft with a plain `onClose()`
   * (`NodeSettings.tsx`'s `handleCancel`) — no `window.confirm` in the
   * way. That matters here specifically: confirmed directly that a real
   * VS Code webview's `window.confirm()` does *not* surface as a normal
   * Playwright `page.on("dialog", ...)` event the way it does in a plain
   * browser tab or headless Chromium (`CanvasSourceEditor`'s own
   * discard-confirm, tried first, left this suite stuck on "Editing raw
   * Markdown source" forever) — so every test here avoids needing one at
   * all, rather than trying to handle it. Leaves NodeSettings closed and
   * the node itself completely untouched (Cancel, not "ok") when done. */
  async function copyMarkerViaNodeSettingsTitle(marker: string) {
    await selectRoot();
    await frame.locator('.mesh-node-toolbar button[title*="settings" i]').click();
    await expect(frame.locator(".node-settings-modal")).toBeVisible();
    const titleInput = frame.locator(".vars-modal-field", { hasText: "Title" }).locator("input");
    await titleInput.click();
    await page.keyboard.press("Meta+KeyA");
    await page.keyboard.insertText(marker);
    await page.keyboard.press("Meta+KeyA");
    await page.keyboard.press("Meta+KeyC");
    await frame.locator(".vars-modal-actions button", { hasText: "cancel" }).click();
    await expect(frame.locator(".node-settings-modal")).toHaveCount(0);
  }

  test("Monaco node-body editor: real Cmd+V paste lands the real clipboard's content", async () => {
    const marker = "MONACOBODYPASTE";
    await copyMarkerViaNodeSettingsTitle(marker);

    await openRootBodyEditor(frame);
    const body = frame.locator(".mesh-text-editor-source .monaco-editor");
    await expect(body).toBeVisible();
    await body.locator(".view-lines").click();
    await page.keyboard.press("Meta+KeyV");

    await expect(body).toContainText(marker);
    await frame.locator(".mesh-text-editor-actions button", { hasText: "done" }).click();
  });

  test("node body editor's title <input>: real Cmd+V paste lands the real clipboard's content", async () => {
    const marker = "TITLEINPUTPASTE";
    await copyMarkerViaNodeSettingsTitle(marker);

    await openRootBodyEditor(frame);
    const titleInput = frame.locator(".mesh-text-editor-title-input");
    await expect(titleInput).toBeVisible();
    await titleInput.click();
    await page.keyboard.press("End");
    await page.keyboard.press("Meta+KeyV");

    await expect(titleInput).toHaveValue(new RegExp(marker));
    // Escape reverts this inline title edit without saving (see
    // `handleTitleKeyDown`) — this test only cares whether the paste
    // landed in the field, not about leaving a renamed node behind for
    // the next test.
    await page.keyboard.press("Escape");
    await frame.locator(".mesh-text-editor-actions button", { hasText: "done" }).click();
  });

  test("NodeSettings' ID <input>: real Cmd+V paste lands the real clipboard's content", async () => {
    const marker = "IDINPUTPASTE";
    await copyMarkerViaNodeSettingsTitle(marker);

    await selectRoot();
    await frame.locator('.mesh-node-toolbar button[title*="settings" i]').click();
    await expect(frame.locator(".node-settings-modal")).toBeVisible();

    const idInput = frame.locator(".vars-modal-field", { hasText: "ID" }).locator("input");
    await idInput.click();
    await page.keyboard.press("End");
    await page.keyboard.press("Meta+KeyV");

    await expect(idInput).toHaveValue(new RegExp(marker));
    // Cancel rather than "ok" — this test only cares whether the paste
    // landed in the field, not about actually renaming the node's id (a
    // real rename here would also need a valid, unique id, which
    // `IDINPUTPASTE` pasted after the already-present default isn't).
    await frame.locator(".vars-modal-actions button", { hasText: "cancel" }).click();
  });
});
