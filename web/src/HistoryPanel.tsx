import { HISTORY_FETCH_LIMIT, type HistoryEntry } from "./api";

interface HistoryPanelProps {
  /** From `GET /api/history`, most-future first — see `fetchHistory`'s
   * own doc comment. Owned by `App.tsx`, refetched there after every
   * `onGoto` (and whenever this panel is opened) so the list reflects
   * the current cursor even when another client moved it meanwhile. */
  entries: HistoryEntry[];
  /** The currently-applied step's own `seq` — the entry with this `seq`
   * is highlighted as "you are here"; nothing to do with which entries
   * count as `applied` (that's per-entry, from the API response). */
  cursor: number;
  onClose: () => void;
  /** Jumps to `seq` in one step (`POST /api/history/goto`) — the parent
   * applies the resulting canvas/undo state and refetches `entries`. */
  onGoto: (seq: number) => void;
}

/** Formats an ISO timestamp as a short relative label ("2 min ago", "3h
 * ago") — no live-ticking clock, just computed once per render off
 * `Date.now()`, since the list only re-renders when refetched anyway
 * (on open, and after every `onGoto`). */
function formatRelativeTime(iso: string): string {
  if (!iso) return "";
  const ms = Date.now() - new Date(iso).getTime();
  if (!Number.isFinite(ms) || ms < 0) return "just now";
  const sec = Math.round(ms / 1000);
  if (sec < 5) return "just now";
  if (sec < 60) return `${sec}s ago`;
  const min = Math.round(sec / 60);
  if (min < 60) return `${min}m ago`;
  const hr = Math.round(min / 60);
  if (hr < 24) return `${hr}h ago`;
  const day = Math.round(hr / 24);
  return `${day}d ago`;
}

/**
 * Global panel listing this canvas's shared undo/redo history (see
 * `crates/server/src/undo_log.rs`'s own doc comment — one linear stack
 * per canvas, not per-tab), opened from the toolbar's "history" button
 * next to the ↶/↷ buttons. Reuses `ServicePanel`/`TtySessionsPanel`'s own
 * card/header chrome (`.service-panel*`/`.vars-modal-backdrop` classes)
 * and `ServicePanel`'s own clickable `.service-panel-row` button styling
 * (`.service-panel-row-selected` for "current position") in a flat,
 * full-width list rather than `ServicePanel`'s own fixed-width sidebar —
 * there's no per-entry detail view here, just click-to-jump.
 *
 * Clicking any row calls `onGoto` with that entry's `seq` — a direct
 * jump via `/api/history/goto`, not a series of single-step undo/redo
 * calls. A divider marks the boundary between the (not-yet-applied) redo
 * tail above the current position and applied history below it, mirroring
 * how `entries` itself is already split by `applied`.
 */
export function HistoryPanel({ entries: listed, cursor, onClose, onGoto }: HistoryPanelProps) {
  // "Start of history": `goto` seq 0 undoes everything. No real row can
  // undo the oldest step itself (each jumps to the state right *after* its
  // own step), so without this that step could never be reverted. Only
  // offered when the list holds the whole log — with more applied steps
  // than the fetch limit, seq 0 would silently undo far more than the rows
  // shown. (`oldest.seq - 1` isn't a safe target instead: seqs have gaps
  // once a redo tail is dropped, so it may never be a cursor value.)
  // Marked current when nothing real is applied.
  const oldest = listed[listed.length - 1];
  const wholeLogShown = listed.filter((e) => e.applied).length < HISTORY_FETCH_LIMIT;
  const entries: HistoryEntry[] =
    oldest && wholeLogShown
      ? [...listed, { seq: 0, createdAt: "", opKind: "", applied: true, summary: "Start of history" }]
      : listed;
  const nothingApplied = !listed.some((e) => e.applied);
  return (
    <div className="vars-modal-backdrop" onClick={onClose}>
      <div className="service-panel" onClick={(e) => e.stopPropagation()}>
        <div className="service-panel-header">
          <h3>History</h3>
          <button type="button" onClick={onClose}>
            ×
          </button>
        </div>
        <div className="service-panel-body">
          <div className="history-panel-list">
            {entries.length === 0 && <p className="vars-modal-hint">No history yet.</p>}
            {entries.map((entry, i) => {
              const prev = entries[i - 1];
              const isBeforeOldest = entry.seq === 0 && entry.opKind === "";
              const isCurrent = isBeforeOldest
                ? nothingApplied || cursor === 0
                : entry.applied && entry.seq === cursor;
              const showDivider = i > 0 && prev.applied === false && entry.applied === true;
              return (
                <div key={entry.seq}>
                  {showDivider && <div className="history-panel-divider" />}
                  <button
                    type="button"
                    className={
                      "service-panel-row history-panel-row" +
                      (isCurrent ? " service-panel-row-selected" : "") +
                      (!entry.applied ? " history-panel-row-future" : "")
                    }
                    onClick={() => onGoto(entry.seq)}
                    title={isBeforeOldest ? entry.summary : `#${entry.seq} — ${entry.opKind}`}
                  >
                    <span className="history-panel-row-marker">{isCurrent ? "●" : ""}</span>
                    <span className="service-panel-row-block">{entry.summary}</span>
                    <span className="service-panel-row-node">{formatRelativeTime(entry.createdAt)}</span>
                  </button>
                </div>
              );
            })}
          </div>
        </div>
      </div>
    </div>
  );
}
