import { EditorView } from "@codemirror/view";
import type { Extension } from "@codemirror/state";

/** Same threshold `crates/core/src/file_read.rs`'s `FILE_PREVIEW_MAX_BYTES`
 * already uses for "big enough to ask first" — not a hard cap, just where
 * the soft `confirm()` warning below kicks in (base64 inflates the file by
 * ~33% on top of this). */
const SOFT_WARN_BYTES = 1_000_000;

function insertAtCursor(view: EditorView, text: string) {
  const { from, to } = view.state.selection.main;
  view.dispatch({
    changes: { from, to, insert: text },
    selection: { anchor: from + text.length },
  });
}

/**
 * TODO.canvas.md: "Base64 image" — pasting an image (from a screenshot
 * tool, or copied from a file manager/browser) embeds it directly as
 * `![](data:image/...;base64,...)` at the cursor, rather than the browser's
 * own default paste (which has nothing to insert for an image into a plain
 * text buffer — CodeMirror's content is just text, so nothing would happen
 * either way, but intercepting it explicitly means we choose what does).
 * Shared between `NodeTextEditor.tsx` (a node's own body) and
 * `CanvasSourceEditor.tsx` (the whole document) — both are the same
 * CodeMirror setup, so one extension covers both rather than wiring the
 * same paste handler into each separately.
 *
 * Deliberately narrow: only the *first* image item in the clipboard is
 * used, and only when there is one — a normal text paste (the overwhelming
 * common case) is left completely alone, never even inspected past
 * `clipboardData.items`.
 *
 * Empty alt text (`![]`, not a placeholder like "pasted image") — nothing
 * useful to say by default that the pasting person wouldn't rather write
 * themselves.
 */
export const imagePaste: Extension = EditorView.domEventHandlers({
  paste(event, view) {
    const items = event.clipboardData?.items;
    if (!items) return false;
    const imageItem = Array.from(items).find((item) => item.type.startsWith("image/"));
    const file = imageItem?.getAsFile();
    if (!file) return false;
    // Nothing meaningful a plain-text CodeMirror buffer could do with the
    // browser's own default image paste — always ours to handle once an
    // image item is actually present.
    event.preventDefault();

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
      insertAtCursor(view, `![](${dataUrl})`);
    };
    reader.readAsDataURL(file);
    return true;
  },
});
