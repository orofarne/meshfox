import { Suspense, useEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import type { OnMount } from "@monaco-editor/react";
import type * as MonacoNS from "monaco-editor";
import { NodeBodyPreview } from "./MeshNode";
import { attachMeshfoxMarkers } from "./meshfoxMarkers";
import { attachImagePaste } from "./imagePaste";
import { attachTextPasteFallback } from "./textPasteFallback";
import { attachVSCodeContextMenu } from "./vsCodeContextMenu";
import { ensureMonacoConfigured, LazyEditor } from "./monacoSetup";
import { THEMES } from "./shiki";
import { THEME_CHANGE_EVENT } from "./theme";
import type { SaveTextOutcome } from "./api";

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
  // Monaco's own custom right-click menu's "Paste" item is broken
  // everywhere, not just on Chromium: it calls `document.execCommand
  // ("paste")`, confirmed (TODO.canvas.md: "VSCode: вставка текста... не
  // работает") to silently no-op against Monaco 0.53's Chromium-only
  // `EditContext`-API input surface (`.native-edit-context`) — an earlier
  // version of this fix left it enabled on Firefox specifically, since the
  // exact same `execCommand("paste")` call does insert real clipboard
  // content there when invoked directly via `execCommand`. But a real
  // right-click → Paste through Firefox's own rendered menu item doesn't
  // (confirmed directly, by hand, after that first fix shipped) — whatever
  // Monaco's own Paste *action* actually does differs from a bare
  // `execCommand` call in a way that breaks it there too. `false`
  // unconditionally, everywhere: no custom Monaco menu entries
  // (`editor.addAction`) exist anywhere in this app to lose, and every
  // browser's own native context menu pastes as a trusted OS-level action
  // instead of a scripted one — confirmed a real (CDP-trusted) Cmd+V
  // already works fine against the same `.native-edit-context` element, so
  // the native menu's Paste, going through that same non-scripted path,
  // does too.
  contextmenu: false,
};

/**
 * Wires up meshfox's own extensions on a freshly-mounted Monaco editor —
 * marker-comment/fence-attribute highlighting (`meshfoxMarkers.ts`),
 * image-paste-as-base64 (`imagePaste.ts`), a scripted-clipboard fallback
 * for when a real Ctrl/Cmd+V never produces a native `paste` event at all
 * (`textPasteFallback.ts` — real VS Code, confirmed), and a full Cut/Copy/
 * Paste right-click menu of its own to replace the native one that same
 * gap breaks there (`vsCodeContextMenu.ts` — both real-VS-Code-only) — the
 * Monaco counterparts of the old CodeMirror `EDITOR_EXTENSIONS`. Shared
 * between `NodeTextEditor` and `CanvasSourceEditor`, both the same setup,
 * rather than each wiring it in separately. Returns a cleanup function.
 */
export function attachMeshfoxEditorExtensions(
  editor: MonacoNS.editor.IStandaloneCodeEditor,
  monaco: typeof MonacoNS,
): () => void {
  const detachMarkers = attachMeshfoxMarkers(editor, monaco);
  const detachPaste = attachImagePaste(editor);
  const detachTextPasteFallback = attachTextPasteFallback(editor);
  const detachContextMenu = attachVSCodeContextMenu(editor);
  return () => {
    detachMarkers();
    detachPaste();
    detachTextPasteFallback();
    detachContextMenu();
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
  title: string;
  initialText: string;
  serverText: string;
  serverRev: string;
  serverVersion: number;
  serverSession: string;
  onChange: (text: string, baseRev: string, title: string, baseTitle: string) => Promise<SaveTextOutcome>;
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
 * Title and body stay local until Apply or Save & close succeeds.
 */
export function NodeTextEditor({
  title, initialText, serverText, serverRev, serverVersion, serverSession,
  onChange, onOpenSettings, onClose,
}: NodeTextEditorProps) {
  const [text, setTextState] = useState(initialText);
  const textRef = useRef(initialText);
  const setText = (value: string) => {
    textRef.current = value;
    setTextState(value);
  };
  const [titleDraft, setTitleState] = useState(title);
  const titleRef = useRef(title);
  const setTitleDraft = (value: string) => {
    titleRef.current = value;
    setTitleState(value);
  };
  const dark = usePrefersDark();
  const monacoReady = useMonacoReady();
  const detachRef = useRef<(() => void) | null>(null);
  const known = useRef({ text: initialText, title, rev: serverRev, version: serverVersion, session: serverSession });
  // Submitted buffers are tracked separately from the server's normalized
  // text. An acknowledgement never edits the Monaco model or its undo stack.
  const synced = useRef({ text: initialText, title });
  type Conflict = { text: string; title: string; rev: string };
  const [conflict, setConflict] = useState<Conflict | null>(null);
  const conflictRef = useRef<Conflict | null>(null);
  const [showTheirs, setShowTheirs] = useState(false);
  const raiseConflict = (theirs: Conflict | null) => {
    conflictRef.current = theirs;
    setConflict(theirs);
    if (!theirs) setShowTheirs(false);
  };
  const [saving, setSaving] = useState(false);
  const savingRef = useRef(false);
  const [closePrompt, setClosePrompt] = useState(false);
  const [saveError, setSaveError] = useState<string | null>(null);
  const [, refresh] = useState(0);
  const dirtyNow = () => textRef.current !== synced.current.text || titleRef.current !== synced.current.title;
  const latestServer = useRef({ text: serverText, title, rev: serverRev, version: serverVersion, session: serverSession });
  const reconcile = () => {
    const theirs = latestServer.current;
    const base = known.current;
    if (theirs.session === base.session && theirs.version <= base.version) return;
    if (theirs.text === base.text && theirs.title === base.title) {
      known.current = theirs;
      return;
    }
    if (!dirtyNow()) {
      known.current = theirs;
      synced.current = { text: theirs.text, title: theirs.title };
      setText(theirs.text);
      setTitleDraft(theirs.title);
      raiseConflict(null);
    } else {
      raiseConflict(theirs);
    }
  };
  useEffect(() => {
    latestServer.current = { text: serverText, title, rev: serverRev, version: serverVersion, session: serverSession };
    if (!savingRef.current) reconcile();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [serverText, title, serverRev, serverVersion, serverSession]);

  const save = async (close: boolean) => {
    if (savingRef.current || conflictRef.current || !titleRef.current.trim()) return;
    if (!dirtyNow()) { if (close) onClose(); return; }
    const sent = { text: textRef.current, title: titleRef.current };
    savingRef.current = true;
    setSaving(true);
    setSaveError(null);
    try {
      const outcome = await onChange(sent.text, known.current.rev, sent.title, known.current.title);
      if (outcome.status === "saved") {
        known.current = { text: outcome.text, title: outcome.title, rev: outcome.rev, version: outcome.version, session: outcome.session };
        synced.current = sent;
        if (latestServer.current.session === outcome.session) reconcile();
        refresh((n) => n + 1);
        // Typing during Apply stays dirty; never close over newer edits.
        if (close && !dirtyNow() && !conflictRef.current) onClose();
      } else if (outcome.status === "conflict") {
        raiseConflict({ text: outcome.currentText, title: outcome.currentTitle ?? known.current.title, rev: outcome.currentRev });
      } else {
        setSaveError("Could not save. Your changes are still here; try again.");
      }
    } catch (error) {
      setSaveError(String(error));
    } finally {
      savingRef.current = false;
      setSaving(false);
    }
  };
  const saveRef = useRef(save);
  saveRef.current = save;
  useEffect(() => () => detachRef.current?.(), []);
  const handleMount: OnMount = (editor, monaco) => {
    detachRef.current = attachMeshfoxEditorExtensions(editor, monaco);
    editor.focus();
  };
  const takeTheirs = () => {
    const theirs = conflictRef.current;
    if (!theirs) return;
    known.current = { ...known.current, ...theirs };
    synced.current = { text: theirs.text, title: theirs.title };
    setText(theirs.text);
    setTitleDraft(theirs.title);
    raiseConflict(null);
  };
  const keepMine = () => {
    const theirs = conflictRef.current;
    if (!theirs) return;
    known.current = { ...known.current, ...theirs };
    raiseConflict(null);
    void save(false);
  };
  const handleClose = () => {
    if (savingRef.current) return;
    if (dirtyNow()) { setClosePrompt(true); return; }
    onClose();
  };
  const dirty = text !== synced.current.text || titleDraft !== synced.current.title;

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
    <div className="mesh-text-editor-backdrop nokey" onClick={handleClose}
      onKeyDownCapture={(e) => {
        if ((e.ctrlKey || e.metaKey) && e.code === "KeyS") {
          e.preventDefault();
          e.stopPropagation();
          void saveRef.current(false);
        }
      }}>
      <div className="mesh-text-editor" onClick={(e) => e.stopPropagation()}>
        <div className="mesh-text-editor-header">
          <input
            className="mesh-text-editor-title-input"
            value={titleDraft}
            onChange={(e) => setTitleDraft(e.target.value)}
            disabled={saving}
            onKeyDown={(e) => { if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "s") { e.preventDefault(); void save(false); } }}
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
        {conflict && (
          <div className="mesh-text-editor-conflict" role="alert">
            <span>This node was changed elsewhere while you were editing.</span>
            <button type="button" onClick={takeTheirs} disabled={saving}>
              take theirs
            </button>
            <button type="button" onClick={keepMine} disabled={saving}>
              keep mine
            </button>
            <button type="button" onClick={() => setShowTheirs((v) => !v)}>
              {showTheirs ? "hide theirs" : "compare"}
            </button>
            {showTheirs && <pre className="mesh-text-editor-conflict-theirs">{`${conflict.title}\n\n${conflict.text}`}</pre>}
          </div>
        )}
        <div className="mesh-text-editor-panes">
          <div className="mesh-text-editor-source" data-vscode-context="{}">
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
        {closePrompt && (
          <div className="mesh-text-editor-close-prompt" role="alertdialog" aria-label="Unsaved changes">
            <span>Save your changes before closing?</span>
            <button type="button" onClick={() => void save(true)} disabled={saving || !!conflict || !titleDraft.trim()}>Save &amp; close</button>
            <button type="button" onClick={onClose} disabled={saving}>Discard</button>
            <button type="button" onClick={() => setClosePrompt(false)}>Keep editing</button>
          </div>
        )}
        {saveError && <p className="mesh-text-editor-error" role="alert">{saveError}</p>}
        <div className="mesh-text-editor-actions">
          <span>{saving ? "Saving…" : dirty ? "Unsaved changes" : "Saved"}</span>
          <button type="button" onClick={() => onClose()} disabled={saving}>Cancel</button>
          <button type="button" onClick={() => void save(false)} disabled={saving || !dirty || !!conflict || !titleDraft.trim()}>Apply</button>
          <button type="button" onClick={() => void save(true)} disabled={saving || !!conflict || !titleDraft.trim()}>Save &amp; close</button>
        </div>
      </div>
    </div>,
    document.body,
  );
}
