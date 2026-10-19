import { Suspense, useEffect, useRef, useState } from "react";
import type { OnMount } from "@monaco-editor/react";
import {
  type ConflictHunk,
  fetchCanvasSource,
  fetchIncludes,
  saveCanvasSource,
  SourceConflictError,
  type IncludeManifestEntry,
} from "./api";
import { ConflictDiff } from "./ConflictDiff";
import { LazyEditor } from "./monacoSetup";
import { MONACO_OPTIONS, attachMeshfoxEditorExtensions, useMonacoReady, usePrefersDark } from "./NodeTextEditor";
import { THEMES } from "./shiki";

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
 * A document that pulls in other files via `include` (see SPEC.md) shows
 * each one's own content dumped straight into its own node's body in the
 * graph view — one *visual* canvas either way. The picker below is Source
 * mode's own equivalent: pick "this document" (the default) or any
 * include to view/edit *its* raw text instead — still one file at a time,
 * since that's what's actually on disk.
 */
export function CanvasSourceEditor({ initialInclude, onSaved, onClose, onDirtyChange }: CanvasSourceEditorProps) {
  const [includes, setIncludes] = useState<IncludeManifestEntry[]>([]);
  const [selected, setSelected] = useState(initialInclude ?? PRIMARY);
  const [text, setText] = useState<string | null>(null);
  const [original, setOriginal] = useState<string | null>(null);
  // The file's revision as this editor last saw it (its `ETag`) — what a
  // save is written against.
  const [rev, setRev] = useState("");
  // The file as another client left it, when a save found it had changed
  // since this editor read it: nothing is written until a side is picked.
  // `hunks`/`mine` — the lines that overlapped and the exact text that was
  // compared — are there when a merge was attempted.
  const [conflict, setConflict] = useState<{
    text: string;
    rev: string;
    hunks?: ConflictHunk[];
    mine?: string;
  } | null>(null);
  const [showTheirs, setShowTheirs] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [readOnly, setReadOnly] = useState(true);
  const [saving, setSaving] = useState(false);
  const dark = usePrefersDark();
  const monacoReady = useMonacoReady();
  const detachRef = useRef<(() => void) | null>(null);
  useEffect(() => () => detachRef.current?.(), []);
  const handleMount: OnMount = (editor, monaco) => {
    if (!readOnly) detachRef.current = attachMeshfoxEditorExtensions(editor, monaco);
    editor.focus();
  };

  useEffect(() => {
    fetchIncludes()
      .then(setIncludes)
      .catch(() => setIncludes([])); // non-fatal: the picker just won't offer any include
  }, []);

  useEffect(() => {
    setText(null);
    setReadOnly(true);
    setError(null);
    setConflict(null);
    setShowTheirs(false);
    let cancelled = false;
    fetchCanvasSource(selected === PRIMARY ? undefined : selected)
      .then(({ text: t, rev: r, readOnly: ro }) => {
        if (cancelled) return;
        setReadOnly(ro);
        setText(t);
        setOriginal(t);
        setRev(r);
      })
      .catch((e) => { if (!cancelled) setError(String(e)); });
    return () => { cancelled = true; };
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

  const handleSave = async (baseRev: string = rev) => {
    if (text === null || readOnly) return;
    setSaving(true);
    setError(null);
    try {
      // `merge`: a file changed elsewhere since it was read is combined with
      // this text by the server when the edits don't overlap (the editor
      // closes on success either way, so a merged result needs no buffer
      // update here).
      await saveCanvasSource(text, baseRev, selected === PRIMARY ? undefined : selected, true);
      onSaved();
    } catch (e) {
      if (e instanceof SourceConflictError) {
        setConflict({ text: e.currentText, rev: e.currentRev, hunks: e.conflicts, mine: text });
      } else {
        setError(String(e));
      }
    } finally {
      setSaving(false);
    }
  };

  /** Drops this editor's edits in favor of the file as it is now. */
  const takeTheirs = () => {
    if (!conflict) return;
    setText(conflict.text);
    setOriginal(conflict.text);
    setRev(conflict.rev);
    setConflict(null);
    setShowTheirs(false);
  };

  /** Writes this editor's text over the file as it is now — against *its*
   * revision, so it still fails if the file changes again before it lands. */
  const keepMine = () => {
    if (!conflict) return;
    const theirRev = conflict.rev;
    setRev(theirRev);
    setConflict(null);
    setShowTheirs(false);
    void handleSave(theirRev);
  };

  return (
    <div className="mesh-source-editor nokey">
      <div className="mesh-source-editor-toolbar">
        <span className="mesh-source-editor-label">{readOnly ? "Read-only canvas include — open it separately to edit" : "Editing raw Markdown source"}</span>
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
          <button type="button" onClick={() => handleSave()} disabled={readOnly || saving || text === null || !dirty || !!conflict}>
            {saving ? "Saving…" : "Save"}
          </button>
        </div>
      </div>
      {conflict && (
        <div className="mesh-source-editor-conflict" role="alert">
          <span>
            {conflict.hunks && conflict.hunks.length > 0
              ? "Your edits overlap changes made elsewhere since you opened this file. Nothing has been saved."
              : "This file was changed elsewhere since you opened it. Nothing has been saved."}
          </span>
          <button type="button" onClick={takeTheirs}>
            take theirs
          </button>
          <button type="button" onClick={keepMine}>
            keep mine
          </button>
          <button type="button" onClick={() => setShowTheirs((v) => !v)}>
            {showTheirs ? "hide theirs" : "compare"}
          </button>
          {showTheirs && (
            <div className="mesh-source-editor-conflict-compare">
              <ConflictDiff
                theirs={conflict.text}
                mine={text ?? ""}
                hunks={conflict.mine === text ? conflict.hunks : undefined}
                dark={dark}
              />
            </div>
          )}
        </div>
      )}
      <div className="mesh-source-editor-body" data-vscode-context="{}">
        {text === null || !monacoReady ? (
          <div className="mesh-source-editor-loading">Loading…</div>
        ) : (
          <Suspense fallback={<div className="mesh-source-editor-loading">Loading…</div>}>
            <LazyEditor
              key={selected}
              height="100%"
              language="markdown"
              theme={dark ? THEMES.dark : THEMES.light}
              value={text}
              onChange={(v) => { if (!readOnly) setText(v ?? ""); }}
              onMount={handleMount}
              options={{ ...MONACO_OPTIONS, readOnly }}
            />
          </Suspense>
        )}
      </div>
    </div>
  );
}
