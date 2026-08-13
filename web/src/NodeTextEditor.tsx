import { useEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import CodeMirror from "@uiw/react-codemirror";
import { NodeBodyPreview } from "./MeshNode";
import { meshfoxMarkdown } from "./meshfoxSyntax";
import { THEME_CHANGE_EVENT } from "./theme";

/** How long to wait after the last keystroke before auto-saving — see
 * NodeSettings.tsx's identical constant/rationale. */
const AUTOSAVE_DELAY_MS = 700;

/** The effective theme right now: the toolbar's manual override (see
 * theme.ts's `data-theme` attribute) if one is set, else the OS
 * `prefers-color-scheme`. */
function resolveDark(): boolean {
  const override = document.documentElement.dataset.theme;
  if (override === "dark") return true;
  if (override === "light") return false;
  return window.matchMedia("(prefers-color-scheme: dark)").matches;
}

/** Tracks the effective light/dark theme so CodeMirror's built-in theme
 * follows the same signal `index.css`'s `@media (prefers-color-scheme)` +
 * `data-theme` override already does for the rest of the app, rather than
 * picking its own. */
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
  initialText: string;
  /** Auto-save callback — fired (debounced) as the text changes, and once
   * more on close to flush anything still pending. Doesn't close the
   * editor itself; only `onClose` does. */
  onChange: (text: string) => void;
  onClose: () => void;
}

/**
 * Split-pane editor for a node's raw Markdown body: a CodeMirror source
 * editor (syntax highlighting, undo, bracket matching) on the left, a live
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
export function NodeTextEditor({ initialText, onChange, onClose }: NodeTextEditorProps) {
  const [text, setText] = useState(initialText);
  const dark = usePrefersDark();

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

  const handleClose = () => {
    if (pendingSave.current) {
      clearTimeout(pendingSave.current);
      pendingSave.current = null;
      onChange(text);
    }
    onClose();
  };

  return createPortal(
    <div className="mesh-text-editor-backdrop" onClick={handleClose}>
      <div className="mesh-text-editor" onClick={(e) => e.stopPropagation()}>
        <div className="mesh-text-editor-panes">
          <div className="mesh-text-editor-source">
            <CodeMirror
              value={text}
              height="100%"
              theme={dark ? "dark" : "light"}
              extensions={[meshfoxMarkdown]}
              onChange={setText}
              autoFocus
            />
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
