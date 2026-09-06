import type * as MonacoNS from "monaco-editor";

/** How long to wait, after a Ctrl/Cmd+V keydown reaches the editor, before
 * concluding the browser's own native paste never arrived and falling back
 * to a scripted one. Real native paste (confirmed directly, in an actual
 * browser tab and in headless Chromium) fires its `paste` event essentially
 * immediately — this only needs to outlast that, not accommodate anything
 * slow. */
const NATIVE_PASTE_GRACE_MS = 250;

/**
 * Real VS Code (TODO.canvas.md: "VSCode: вставка текста (Cmd+V и
 * контекстное меню) в редактор ноды не работает") never delivers a `paste`
 * DOM event for a genuine, trusted Cmd+V here at all — confirmed directly
 * (a real VS Code window driven end-to-end via `playwright`'s Electron
 * support, `--extensionDevelopmentPath` pointed at this extension): the
 * `keydown`/`keyup` for Cmd+V reach `.native-edit-context` correctly
 * (`isTrusted: true`, `defaultPrevented: false`, correct target), but no
 * `paste` event ever follows — nothing in this app's own code, or even
 * Monaco's, ever gets a chance to act on it, since a normal `paste`-event
 * listener (the same shape `imagePaste.ts` uses) has nothing to listen to.
 * A real Chromium tab and headless Chromium (this file's own paste e2e
 * suite, `e2e/copy-paste.spec.ts`) don't have this problem — confirmed the
 * exact same trusted-Cmd+V sequence delivers a normal `paste` event and
 * inserts correctly there. This looks like a genuine gap in how Electron's
 * own native menu-accelerator-driven paste command resolves focus through
 * a VS Code webview's own (unavoidable, extension-independent) nested
 * iframe down to this specific EditContext-API input surface — well
 * outside anything this app controls or could patch from here.
 *
 * What *is* available and does work in that same real VS Code session
 * (confirmed directly): `navigator.clipboard.readText()`. This attaches a
 * `keydown` listener for Ctrl/Cmd+V and, only if no `paste` event actually
 * follows within `NATIVE_PASTE_GRACE_MS`, performs the paste itself via
 * that API instead — a correction applied only where the native path
 * genuinely never showed up, not a wholesale replacement of it. That
 * matters for two reasons: it leaves the already-working native path (a
 * real browser tab, most of the time inside VS Code's own "Open in
 * Browser" escape hatch too) completely alone, and it avoids `navigator.
 * clipboard.readText()`'s own permission prompt ever appearing for someone
 * whose Ctrl/Cmd+V already just works.
 *
 * Deliberately narrow like `imagePaste.ts`: only reacts to the paste
 * *shortcut* itself, never to a context-menu-triggered paste (Monaco's own
 * menu is unconditionally disabled — see `MONACO_OPTIONS`'
 * `contextmenu` — precisely because scripting a paste from a menu click
 * turned out unreliable across browsers; this fallback doesn't reopen that
 * door, it only ever fires from a real, trusted keydown).
 */
export function attachTextPasteFallback(editor: MonacoNS.editor.IStandaloneCodeEditor): () => void {
  const node = editor.getDomNode();
  if (!node) return () => {};

  let timeoutId: ReturnType<typeof setTimeout> | null = null;
  let nativePasteSeen = false;

  const onPaste = (event: ClipboardEvent) => {
    if (!(event.target instanceof Node) || !node.contains(event.target)) return;
    nativePasteSeen = true;
  };

  const onKeyDown = (event: KeyboardEvent) => {
    if (!(event.target instanceof Node) || !node.contains(event.target)) return;
    const isPasteShortcut =
      (event.metaKey || event.ctrlKey) && !event.altKey && event.key.toLowerCase() === "v";
    if (!isPasteShortcut) return;

    nativePasteSeen = false;
    if (timeoutId) clearTimeout(timeoutId);
    timeoutId = setTimeout(() => {
      timeoutId = null;
      if (nativePasteSeen) return;
      navigator.clipboard.readText().then(
        (text) => {
          if (!text) return;
          const selection = editor.getSelection();
          if (!selection) return;
          editor.executeEdits("text-paste-fallback", [{ range: selection, text, forceMoveMarkers: true }]);
          editor.focus();
        },
        () => {
          // clipboard-read unavailable/denied here (e.g. a browser that
          // never grants it for a plain page) — nothing more to try;
          // whatever the (already apparently unsuccessful) native path
          // left in place is what the editor keeps showing.
        },
      );
    }, NATIVE_PASTE_GRACE_MS);
  };

  document.addEventListener("paste", onPaste, true);
  document.addEventListener("keydown", onKeyDown, true);
  return () => {
    document.removeEventListener("paste", onPaste, true);
    document.removeEventListener("keydown", onKeyDown, true);
    if (timeoutId) clearTimeout(timeoutId);
  };
}
