import { Suspense } from "react";
import type { DiffOnMount } from "@monaco-editor/react";
import type * as MonacoNS from "monaco-editor";
import type { ConflictHunk } from "./api";
import { LazyDiffEditor } from "./monacoSetup";
import { THEMES } from "./shiki";

interface ConflictDiffProps {
  /** The text as someone else left it (read-only, left). */
  theirs: string;
  /** The text this editor holds (right). */
  mine: string;
  /** The line ranges both sides changed — highlighted when given. Only
   * meaningful for the exact `mine` text the server compared, so a caller
   * whose buffer has moved on since passes none. */
  hunks?: ConflictHunk[];
  dark: boolean;
}

/**
 * Side-by-side comparison shown by the conflict banners' "compare": what
 * someone else wrote on the left, this editor's text on the right, with the
 * regions that actually overlapped (the server's `conflicts`) marked on top
 * of the ordinary line diff. Read-only — resolving is still "take theirs",
 * "keep mine", or editing the buffer and applying again.
 */
export function ConflictDiff({ theirs, mine, hunks, dark }: ConflictDiffProps) {
  const handleMount: DiffOnMount = (editor, monaco) => {
    if (!hunks || hunks.length === 0) return;
    const mark = (target: MonacoNS.editor.IStandaloneCodeEditor, side: "mine" | "theirs") => {
      target.createDecorationsCollection(
        hunks.map((h) => {
          const first = h[side].start + 1;
          const last = Math.max(h[side].end, first);
          return {
            range: new monaco.Range(first, 1, last, 1),
            options: { isWholeLine: true, className: "mesh-conflict-line" },
          };
        }),
      );
    };
    mark(editor.getOriginalEditor(), "theirs");
    mark(editor.getModifiedEditor(), "mine");
  };
  return (
    <div className="mesh-conflict-diff" data-testid="conflict-diff">
      <Suspense fallback={<p className="mesh-node-hint">loading comparison…</p>}>
        <LazyDiffEditor
          height="100%"
          language="markdown"
          theme={dark ? THEMES.dark : THEMES.light}
          original={theirs}
          modified={mine}
          onMount={handleMount}
          options={{
            readOnly: true,
            originalEditable: false,
            renderSideBySide: true,
            minimap: { enabled: false },
            wordWrap: "on",
            scrollBeyondLastLine: false,
            fontSize: 12,
            renderOverviewRuler: false,
            contextmenu: false,
            unicodeHighlight: { ambiguousCharacters: false, invisibleCharacters: false },
            // A whole file with one conflicting line: fold the identical
            // stretches (a few lines of context stay) so the difference is
            // what is on screen, not somewhere far below.
            hideUnchangedRegions: { enabled: true, contextLineCount: 3, minimumLineCount: 4 },
          }}
        />
      </Suspense>
    </div>
  );
}
