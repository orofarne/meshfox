import type * as MonacoNS from "monaco-editor";

/** Same threshold `crates/core/src/file_read.rs`'s `FILE_PREVIEW_MAX_BYTES`
 * already uses for "big enough to ask first" — not a hard cap, just where
 * the soft `confirm()` warning below kicks in (base64 inflates the file by
 * ~33% on top of this). */
const SOFT_WARN_BYTES = 1_000_000;

function insertAtCursor(editor: MonacoNS.editor.IStandaloneCodeEditor, text: string) {
  const selection = editor.getSelection();
  if (!selection) return;
  editor.executeEdits("image-paste", [{ range: selection, text, forceMoveMarkers: true }]);
  editor.focus();
}

/**
 * TODO.canvas.md: "Base64 image" — pasting an image (from a screenshot
 * tool, or copied from a file manager/browser) embeds it directly as
 * `![](data:image/...;base64,...)` at the cursor, rather than the browser's
 * own default paste (which has nothing to insert for an image into a plain
 * text buffer — Monaco's content is just text, so nothing would happen
 * either way, but intercepting it explicitly means we choose what does).
 * Shared between `NodeTextEditor.tsx` (a node's own body) and
 * `CanvasSourceEditor.tsx` (the whole document) — both are the same Monaco
 * setup, so one attach function covers both rather than wiring the same
 * paste handler into each separately.
 *
 * Deliberately narrow: only the *first* image item in the clipboard is
 * used, and only when there is one — a normal text paste (the overwhelming
 * common case) is left completely alone, never even inspected past
 * `clipboardData.items`.
 *
 * Empty alt text (`![]`, not a placeholder like "pasted image") — nothing
 * useful to say by default that the pasting person wouldn't rather write
 * themselves.
 *
 * Listens in the *capture* phase on `document` itself, not the editor's own
 * DOM node — ahead of Monaco's own internal paste handling, the same
 * "intercept before Monaco/CodeMirror gets it" shape the old CodeMirror
 * `domEventHandlers` version relied on. Has to be `document` specifically:
 * Monaco 0.53's EditContext-API input model (`.native-edit-context`, a
 * `div[role=textbox]`, not a real textarea) does its own paste interception
 * somewhere at or above `@monaco-editor/react`'s own `<section>` wrapper —
 * confirmed empirically (a listener planted at each ancestor level, capture
 * phase, showed propagation reaching the wrapper `<section>` but never any
 * node inside it, meaning something there calls `stopPropagation()` before
 * a listener on `editor.getDomNode()` itself would ever run). `document` is
 * the one node guaranteed to sit above that boundary, so a capture listener
 * there always runs first regardless of registration order. Filtered by
 * `node.contains(event.target)` so a paste elsewhere on the page (or into a
 * different, unrelated Monaco instance) is left alone. Returns a cleanup
 * function.
 */
export function attachImagePaste(editor: MonacoNS.editor.IStandaloneCodeEditor): () => void {
  const node = editor.getDomNode();
  if (!node) return () => {};

  const handler = (event: ClipboardEvent) => {
    if (!(event.target instanceof Node) || !node.contains(event.target)) return;
    const items = event.clipboardData?.items;
    if (!items) return;
    const imageItem = Array.from(items).find((item) => item.type.startsWith("image/"));
    const file = imageItem?.getAsFile();
    if (!file) return;
    // Nothing meaningful a plain-text Monaco buffer could do with the
    // browser's own default image paste — always ours to handle once an
    // image item is actually present.
    event.preventDefault();
    event.stopPropagation();

    const reader = new FileReader();
    reader.onload = () => {
      const dataUrl = reader.result;
      if (typeof dataUrl !== "string") return;
      if (dataUrl.length > SOFT_WARN_BYTES) {
        const mb = (dataUrl.length / 1_000_000).toFixed(1);
        const proceed = window.confirm(
          `This image is about ${mb} MB once embedded as base64 — it'll bloat this document's ` +
            `file (and any git history of it). Insert it anyway?`,
        );
        if (!proceed) return;
      }
      insertAtCursor(editor, `![](${dataUrl})`);
    };
    reader.readAsDataURL(file);
  };

  document.addEventListener("paste", handler, true);
  return () => document.removeEventListener("paste", handler, true);
}
