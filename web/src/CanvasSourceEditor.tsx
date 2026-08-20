import { useEffect, useState } from "react";
import CodeMirror from "@uiw/react-codemirror";
import { fetchCanvasSource, fetchIncludes, saveCanvasSource, type IncludeManifestEntry } from "./api";
import { EDITOR_EXTENSIONS, usePrefersDark } from "./NodeTextEditor";

interface CanvasSourceEditorProps {
  /** Seeds the file picker's initial selection — an include's own
   * `nodeId` (see `IncludeManifestEntry`), or `undefined` for the
   * document itself. Read once, on mount, same as any other React
   * `useState` initializer: switching files after that is the picker's
   * own job (`handleSelect`), not this prop's. */
  initialInclude?: string;
  /** Fires once a save actually succeeds — the caller should reload the
   * parsed canvas and switch back to the graph view. Fired the same way
   * regardless of which file was actually saved (the primary document or
   * an include's own target — see `selected` below): either way, a fresh
   * `GET /api/canvas` reflects it. */
  onSaved: () => void;
  /** Leaves Source mode without saving. Prompts for confirmation itself if
   * there are unsaved edits, so the caller can wire this straight to a
   * "Canvas" toggle button with no extra dirty-tracking of its own. */
  onClose: () => void;
  /** Reports whether there are unsaved edits, so the caller can e.g. disable
   * a "done" button that would otherwise leave Source mode silently. */
  onDirtyChange?: (dirty: boolean) => void;
}

/** `"primary"` (the document itself) or an include's own `nodeId` (see
 * `IncludeManifestEntry`) — kept as a plain string rather than
 * `string | undefined` so it works directly as a controlled `<select>`
 * value. */
const PRIMARY = "primary";

/**
 * Full-document raw Markdown editor — the toolbar's "Source" mode,
 * alongside the per-node body editor (NodeTextEditor). Unlike that one,
 * this doesn't auto-save: the whole file (headings, `meshfox:node`
 * comments, fences) is fair game for editing here, so a typo mid-edit could
 * otherwise get written half-formed. Saving is an explicit action, and the
 * server validates the full document parses before writing anything —
 * a rejected save leaves both the file and this editor's contents
 * untouched, with the parser's own error shown so it's fixable.
 *
 * A document that pulls in other files via `include` (see SPEC.md) is
 * still just one *visual* canvas — the graph view already lets an
 * included subtree's nodes be dragged/edited transparently, writing back
 * to whichever file they actually live in. The picker below is Source
 * mode's own equivalent: pick "this document" (the default) or any
 * include, however deeply nested, to view/edit *its* raw text instead —
 * still one file at a time, since that's what's actually on disk.
 */
export function CanvasSourceEditor({ initialInclude, onSaved, onClose, onDirtyChange }: CanvasSourceEditorProps) {
  const [includes, setIncludes] = useState<IncludeManifestEntry[]>([]);
  const [selected, setSelected] = useState(initialInclude ?? PRIMARY);
  const [text, setText] = useState<string | null>(null);
  const [original, setOriginal] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const dark = usePrefersDark();

  useEffect(() => {
    fetchIncludes()
      .then(setIncludes)
      .catch(() => setIncludes([])); // non-fatal: the picker just won't offer any include
  }, []);

  useEffect(() => {
    setText(null);
    setError(null);
    fetchCanvasSource(selected === PRIMARY ? undefined : selected)
      .then((t) => {
        setText(t);
        setOriginal(t);
      })
      .catch((e) => setError(String(e)));
  }, [selected]);

  const dirty = text !== null && text !== original;

  useEffect(() => {
    onDirtyChange?.(dirty);
  }, [dirty, onDirtyChange]);

  const confirmDiscardIfDirty = () => !dirty || window.confirm("Discard unsaved source changes?");

  const handleClose = () => {
    if (!confirmDiscardIfDirty()) return;
    onClose();
  };

  const handleSelect = (nodeId: string) => {
    if (nodeId === selected) return;
    if (!confirmDiscardIfDirty()) return;
    setSelected(nodeId);
  };

  const handleSave = async () => {
    if (text === null) return;
    setSaving(true);
    setError(null);
    try {
      await saveCanvasSource(text, selected === PRIMARY ? undefined : selected);
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
        {includes.length > 0 && (
          <select
            className="mesh-source-editor-file-select"
            value={selected}
            onChange={(e) => handleSelect(e.target.value)}
            disabled={saving}
          >
            <option value={PRIMARY}>This document</option>
            {includes.map((inc) => (
              <option key={inc.nodeId} value={inc.nodeId}>
                {"  ".repeat(inc.depth + 1)}
                {"↳ "}
                {inc.title} ({inc.target})
              </option>
            ))}
          </select>
        )}
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
            key={selected}
            value={text}
            height="100%"
            theme={dark ? "dark" : "light"}
            extensions={EDITOR_EXTENSIONS}
            onChange={setText}
            autoFocus
          />
        )}
      </div>
    </div>
  );
}
