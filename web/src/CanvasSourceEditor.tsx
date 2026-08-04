import { useEffect, useState } from "react";
import CodeMirror from "@uiw/react-codemirror";
import { fetchCanvasSource, saveCanvasSource } from "./api";
import { usePrefersDark } from "./NodeTextEditor";
import { meshfoxMarkdown } from "./meshfoxSyntax";

interface CanvasSourceEditorProps {
  /** Fires once a save actually succeeds — the caller should reload the
   * parsed canvas and switch back to the graph view. */
  onSaved: () => void;
  /** Leaves Source mode without saving. Prompts for confirmation itself if
   * there are unsaved edits, so the caller can wire this straight to a
   * "Canvas" toggle button with no extra dirty-tracking of its own. */
  onClose: () => void;
  /** Reports whether there are unsaved edits, so the caller can e.g. disable
   * a "done" button that would otherwise leave Source mode silently. */
  onDirtyChange?: (dirty: boolean) => void;
}

/**
 * Full-document raw Markdown editor — the toolbar's "Source" mode,
 * alongside the per-node body editor (NodeTextEditor). Unlike that one,
 * this doesn't auto-save: the whole file (headings, `meshfox:node`
 * comments, fences) is fair game for editing here, so a typo mid-edit could
 * otherwise get written half-formed. Saving is an explicit action, and the
 * server validates the full document parses before writing anything —
 * a rejected save leaves both the file and this editor's contents
 * untouched, with the parser's own error shown so it's fixable.
 */
export function CanvasSourceEditor({ onSaved, onClose, onDirtyChange }: CanvasSourceEditorProps) {
  const [text, setText] = useState<string | null>(null);
  const [original, setOriginal] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const dark = usePrefersDark();

  useEffect(() => {
    fetchCanvasSource()
      .then((t) => {
        setText(t);
        setOriginal(t);
      })
      .catch((e) => setError(String(e)));
  }, []);

  const dirty = text !== null && text !== original;

  useEffect(() => {
    onDirtyChange?.(dirty);
  }, [dirty, onDirtyChange]);

  const handleClose = () => {
    if (dirty && !window.confirm("Discard unsaved source changes?")) return;
    onClose();
  };

  const handleSave = async () => {
    if (text === null) return;
    setSaving(true);
    setError(null);
    try {
      await saveCanvasSource(text);
      onSaved();
    } catch (e) {
      setError(String(e));
    } finally {
      setSaving(false);
    }
  };

  return (
    <div className="mesh-source-editor">
      <div className="mesh-source-editor-toolbar">
        <span className="mesh-source-editor-label">Editing raw Markdown source</span>
        {error && <span className="error">{error}</span>}
        <div className="mesh-source-editor-actions">
          <button type="button" onClick={handleClose} disabled={saving}>
            Cancel
          </button>
          <button type="button" onClick={handleSave} disabled={saving || text === null || !dirty}>
            {saving ? "Saving…" : "Save"}
          </button>
        </div>
      </div>
      <div className="mesh-source-editor-body">
        {text === null ? (
          <div className="mesh-source-editor-loading">Loading…</div>
        ) : (
          <CodeMirror
            value={text}
            height="100%"
            theme={dark ? "dark" : "light"}
            extensions={[meshfoxMarkdown]}
            onChange={setText}
            autoFocus
          />
        )}
      </div>
    </div>
  );
}
