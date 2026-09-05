import { Suspense, useEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import type { OnMount } from "@monaco-editor/react";
import type * as MonacoNS from "monaco-editor";
import { NodeBodyPreview } from "./MeshNode";
import { attachMeshfoxMarkers } from "./meshfoxMarkers";
import { attachImagePaste } from "./imagePaste";
import { ensureMonacoConfigured, LazyEditor } from "./monacoSetup";
import { THEMES } from "./shiki";
import { THEME_CHANGE_EVENT } from "./theme";

/** Resolves once Monaco is self-hosted and ready to mount (see
 * `monacoSetup.ts`'s own doc comment for why this is lazy rather than
 * loaded at app startup). Both editors gate their own `<Editor>` render on
 * this — mounting one before `loader.config` has actually run risks a
 * race against `@monaco-editor/react`'s default CDN loader kicking in
 * first. */
export function useMonacoReady(): boolean {
  const [ready, setReady] = useState(false);
  useEffect(() => {
    let cancelled = false;
    ensureMonacoConfigured().then(() => {
      if (!cancelled) setReady(true);
    });
    return () => {
      cancelled = true;
    };
  }, []);
  return ready;
}

/** How long to wait after the last keystroke before auto-saving — see
 * NodeSettings.tsx's identical constant/rationale. */
const AUTOSAVE_DELAY_MS = 700;

/** Shared Monaco `options` for every editor in the app (NodeTextEditor,
 * CanvasSourceEditor) — kept as one stable module-level object rather than
 * a fresh literal per render, same reasoning the old CodeMirror
 * `EDITOR_EXTENSIONS` constant documented (avoids Monaco treating it as a
 * changed prop on every keystroke). */
export const MONACO_OPTIONS: MonacoNS.editor.IStandaloneEditorConstructionOptions = {
  fontFamily: "'Fira Code', ui-monospace, SFMono-Regular, Menlo, Consolas, monospace",
  fontSize: 13,
  minimap: { enabled: false },
  wordWrap: "on",
  scrollBeyondLastLine: false,
  // Monaco's own "confusable Unicode character" detection (meant to flag
  // homoglyph attacks in *code* — a Cyrillic а disguised as a Latin a in an
  // identifier) draws a box around every Cyrillic letter it considers
  // visually ambiguous next to Latin text. A node's body is ordinary,
  // legitimately multilingual prose (this app's own docs are half
  // Russian), not untrusted source code — the boxes are just noise here,
  // not a real signal, so this is off rather than tuned.
  unicodeHighlight: { ambiguousCharacters: false, invisibleCharacters: false },
  // Highlights every other occurrence of whatever word the cursor is
  // currently on — meant for jumping between uses of a variable/symbol in
  // real code (where "same word" usually means "same identifier"). In
  // prose it just means "same word" in the mundane sense (an article, a
  // common verb), which lights up half the paragraph for no useful reason.
  occurrencesHighlight: "off",
};

/**
 * Wires up meshfox's own extensions on a freshly-mounted Monaco editor —
 * marker-comment/fence-attribute highlighting (`meshfoxMarkers.ts`) and
 * image-paste-as-base64 (`imagePaste.ts`) — the Monaco counterparts of the
 * old CodeMirror `EDITOR_EXTENSIONS`. Shared between `NodeTextEditor` and
 * `CanvasSourceEditor`, both the same setup, rather than each wiring it in
 * separately. Returns a cleanup function.
 */
export function attachMeshfoxEditorExtensions(
  editor: MonacoNS.editor.IStandaloneCodeEditor,
  monaco: typeof MonacoNS,
): () => void {
  const detachMarkers = attachMeshfoxMarkers(editor, monaco);
  const detachPaste = attachImagePaste(editor);
  return () => {
    detachMarkers();
    detachPaste();
  };
}

/** The effective theme right now: the toolbar's manual override (see
 * theme.ts's `data-theme` attribute) if one is set, else the OS
 * `prefers-color-scheme`. */
function resolveDark(): boolean {
  const override = document.documentElement.dataset.theme;
  if (override === "dark") return true;
  if (override === "light") return false;
  return window.matchMedia("(prefers-color-scheme: dark)").matches;
}

/** Tracks the effective light/dark theme so Monaco's theme (`THEMES.dark`/
 * `THEMES.light`, `shiki.ts` — registered into Monaco by `monacoSetup.ts`,
 * not one of Monaco's own built-in `vs`/`vs-dark`) follows the same signal
 * `index.css`'s `@media (prefers-color-scheme)` + `data-theme` override
 * already does for the rest of the app, rather than picking its own. */
export function usePrefersDark(): boolean {
  const [dark, setDark] = useState(resolveDark);
  useEffect(() => {
    const mq = window.matchMedia("(prefers-color-scheme: dark)");
    const onChange = () => setDark(resolveDark());
    mq.addEventListener("change", onChange);
    window.addEventListener(THEME_CHANGE_EVENT, onChange);
    return () => {
      mq.removeEventListener("change", onChange);
      window.removeEventListener(THEME_CHANGE_EVENT, onChange);
    };
  }, []);
  return dark;
}

interface NodeTextEditorProps {
  /** Current title, shown (and editable) in the header — see
   * `onSaveTitle`. */
  title: string;
  initialText: string;
  /** Auto-save callback — fired (debounced) as the text changes, and once
   * more on close to flush anything still pending. Doesn't close the
   * editor itself; only `onClose` does. */
  onChange: (text: string) => void;
  /** Commits the header's title field — fired on blur/Enter, same
   * "wherever the title is being edited" shape as MeshNode's own inline
   * canvas title edit (TODO.canvas.md: "Редактирование заголовка и вызов
   * node settings внутри редактора"), not debounced like `onChange` above:
   * a title is one short field, not worth a partial-typing round trip. */
  onSaveTitle: (title: string) => void;
  /** The header's gear button — opens NodeSettings without leaving this
   * editor open underneath it (TODO.canvas.md: same node as `onSaveTitle`
   * above — "убрать отдельное меню node settings" turned out to mean "stop
   * requiring it for a rename", not remove it: it's still where type,
   * color, tags, target, and edges live). */
  onOpenSettings: () => void;
  onClose: () => void;
}

/**
 * Split-pane editor for a node's raw Markdown body: a Monaco source editor
 * (syntax highlighting, undo, bracket matching) on the left, a live
 * preview using the exact same rendering the canvas itself uses
 * (`NodeBodyPreview`) on the right. Deliberately a *source* editor, not a
 * WYSIWYG one — the body can contain meshfox-specific syntax (fence
 * attributes `name=`/`cache`/`deps=`/`env=`, `<!-- meshfox:output -->`
 * markers) that a WYSIWYG round-trip risks corrupting; editing the raw text
 * never touches a byte it doesn't need to.
 *
 * Rendered via a portal into `document.body`, as a fixed, centered overlay
 * — not inline inside the node's own box. A node's stored/suggested size
 * is rarely anywhere near big enough for a real code editor, and inline
 * would also mean the editor's own size/position is at the mercy of the
 * canvas's current pan/zoom (which can push its controls — even itself —
 * off-screen entirely, unreachable, for a node near the edge of the
 * current view). A portal sidesteps both: fixed size, fixed position,
 * regardless of where the node sits on the canvas.
 *
 * Auto-saves (debounced) as you type, same as NodeSettings — no separate
 * Save/Cancel step, just a "done" button to close once whatever's pending
 * has flushed.
 */
export function NodeTextEditor({ title, initialText, onChange, onSaveTitle, onOpenSettings, onClose }: NodeTextEditorProps) {
  const [text, setText] = useState(initialText);
  const [titleDraft, setTitleDraft] = useState(title);
  // Keeps the header's title field in sync with a rename made elsewhere
  // while this editor stayed open — most commonly via the gear button
  // right next to it (NodeSettings can change the title too). Harmless
  // while the user is actively typing here instead: `title` only changes
  // once *this* field's own `onSaveTitle` round-trips back through a fresh
  // canvas, at which point it matches `titleDraft` already.
  useEffect(() => setTitleDraft(title), [title]);
  const dark = usePrefersDark();
  const monacoReady = useMonacoReady();
  const detachRef = useRef<(() => void) | null>(null);

  const isFirstRender = useRef(true);
  const pendingSave = useRef<ReturnType<typeof setTimeout> | null>(null);
  useEffect(() => {
    if (isFirstRender.current) {
      isFirstRender.current = false;
      return;
    }
    pendingSave.current = setTimeout(() => {
      onChange(text);
      pendingSave.current = null;
    }, AUTOSAVE_DELAY_MS);
    return () => {
      if (pendingSave.current) clearTimeout(pendingSave.current);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [text]);

  useEffect(() => () => detachRef.current?.(), []);

  const handleMount: OnMount = (editor, monaco) => {
    detachRef.current = attachMeshfoxEditorExtensions(editor, monaco);
    editor.focus();
  };

  const handleClose = () => {
    if (pendingSave.current) {
      clearTimeout(pendingSave.current);
      pendingSave.current = null;
      onChange(text);
    }
    onClose();
  };

  // Not debounced like the body's own autosave above — a title is one
  // short field, committed whole on blur/Enter, same as MeshNode's inline
  // canvas title edit.
  const handleTitleBlur = () => onSaveTitle(titleDraft);
  const handleTitleKeyDown = (e: React.KeyboardEvent<HTMLInputElement>) => {
    if (e.key === "Enter") {
      e.preventDefault();
      e.currentTarget.blur();
    } else if (e.key === "Escape") {
      e.preventDefault();
      setTitleDraft(title);
      e.currentTarget.blur();
    }
  };

  // `nokey`: React Flow's own global Space-to-pan shortcut (`panActivationKeyCode`,
  // default-on, never opted into by this app) decides whether to ignore a
  // keydown via `isInputDOMNode` — an input/textarea/select tag or a
  // `contenteditable` attribute. Monaco 0.53's real typing surface, when the
  // browser supports the EditContext API (Chromium-based — VS Code's webview
  // among them), is `.native-edit-context`, a plain `<div>` with neither of
  // those, so React Flow doesn't recognize it as text input and swallows
  // Space (`preventDefault()`s it) meant for typing a space character in the
  // editor. `nokey` is `isInputDOMNode`'s own documented escape hatch
  // (`target.closest('.nokey')`) for exactly this case — same fix applied to
  // CanvasSourceEditor's wrapper for its Monaco instance.
  return createPortal(
    <div className="mesh-text-editor-backdrop nokey" onClick={handleClose}>
      <div className="mesh-text-editor" onClick={(e) => e.stopPropagation()}>
        <div className="mesh-text-editor-header">
          <input
            className="mesh-text-editor-title-input"
            value={titleDraft}
            onChange={(e) => setTitleDraft(e.target.value)}
            onBlur={handleTitleBlur}
            onKeyDown={handleTitleKeyDown}
          />
          <button
            type="button"
            className="mesh-node-icon-button"
            onClick={onOpenSettings}
            title="Edit node settings (type, color, tags, target, edges)"
          >
            ⚙
          </button>
        </div>
        <div className="mesh-text-editor-panes">
          <div className="mesh-text-editor-source">
            {monacoReady ? (
              <Suspense fallback={<p className="mesh-node-hint">loading editor…</p>}>
                <LazyEditor
                  height="100%"
                  language="markdown"
                  theme={dark ? THEMES.dark : THEMES.light}
                  value={text}
                  onChange={(v) => setText(v ?? "")}
                  onMount={handleMount}
                  options={MONACO_OPTIONS}
                />
              </Suspense>
            ) : (
              <p className="mesh-node-hint">loading editor…</p>
            )}
          </div>
          <div className="mesh-text-editor-preview">
            <NodeBodyPreview text={text} />
          </div>
        </div>
        <div className="mesh-text-editor-actions">
          <button type="button" onClick={handleClose}>
            done
          </button>
        </div>
      </div>
    </div>,
    document.body,
  );
}
