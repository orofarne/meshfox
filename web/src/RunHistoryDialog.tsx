import { useEffect, useState } from "react";
import { createPortal } from "react-dom";
import { AnsiText } from "./AnsiText";
import { fetchRunHistory, subscribeRun, type RunHistoryEntry } from "./api";
import { formatDurationMs } from "./format";

/** `"Sep 30, 11:24:15 AM"`-style local time for a run's own RFC 3339
 * timestamp — the full value is always in the row's `title`. */
function formatRunTime(rfc3339: string): string {
  const d = new Date(rfc3339);
  if (Number.isNaN(d.getTime())) return rfc3339;
  return d.toLocaleString(undefined, {
    day: "numeric",
    month: "short",
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
  });
}

function runState(run: RunHistoryEntry): "ok" | "fail" | "killed" {
  return run.outcome === "killed" ? "killed" : run.exitCode === 0 ? "ok" : "fail";
}

function runLabel(run: RunHistoryEntry): string {
  return run.outcome === "killed" ? "killed" : `exit ${run.exitCode ?? "?"}`;
}

const STALE_HINT =
  "This run no longer describes the document: the block, something it depends on, or a variable it used " +
  "changed since — or the session was reset. It is kept as history but isn't shown as the block's latest result.";

/**
 * A block's run history, as a modal — the finished runs the server keeps in
 * its session database (see `crates/server/src/run_ledger.rs`; how many is
 * the `[session] max_runs_per_block` setting), newest first, each with its
 * exit code, duration and stored output. Left: the list; right: the
 * selected run's output (the newest run is selected on open). A `stale` run
 * is still listed and viewable, just marked. Independent of the block's own
 * live/cached output on the canvas — this only ever shows a *past* run.
 *
 * Portaled to `document.body`: a React Flow node sits inside a transformed
 * ancestor, which would turn this `position: fixed` backdrop into one
 * that's positioned relative to the node instead of the viewport. Reloads
 * the list whenever the block's own live run finishes (the server stores a
 * run before it reports it done, so that fetch already sees it), though the
 * modal itself covers the page while open, so that mostly matters for a run
 * started elsewhere (another tab, `autorun`).
 */
export function RunHistoryDialog({
  nodeId,
  blockName,
  tty,
  liveStatus,
  onClose,
}: {
  nodeId: string;
  blockName: string;
  /** A `tty` block: its runs are listed with outcome and timing, but no
   * output is stored for them. */
  tty?: boolean;
  liveStatus?: string;
  onClose: () => void;
}) {
  const [runs, setRuns] = useState<RunHistoryEntry[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [selected, setSelected] = useState<number | null>(null);
  const [output, setOutput] = useState<{ id: number; text: string; loading: boolean } | null>(null);

  const settled = liveStatus === "done" || liveStatus === "killed";
  useEffect(() => {
    let cancelled = false;
    fetchRunHistory(nodeId, blockName).then(
      (r) => {
        if (cancelled) return;
        setRuns(r);
        setError(null);
        // Newest run on open; keep the user's own pick across a reload.
        setSelected((cur) => (cur !== null && r.some((x) => x.id === cur) ? cur : (r[0]?.id ?? null)));
      },
      (e: unknown) => {
        if (!cancelled) setError(e instanceof Error ? e.message : String(e));
      },
    );
    return () => {
      cancelled = true;
    };
  }, [nodeId, blockName, settled]);

  useEffect(() => {
    if (selected === null) {
      setOutput(null);
      return;
    }
    let cancelled = false;
    const lines: string[] = [];
    setOutput({ id: selected, text: "", loading: true });
    subscribeRun(
      nodeId,
      blockName,
      0,
      (ev) => {
        if (ev.type === "line") lines.push(ev.text);
      },
      selected,
    ).then(
      () => {
        if (!cancelled) setOutput({ id: selected, text: lines.join("\n"), loading: false });
      },
      () => {
        if (!cancelled) setOutput({ id: selected, text: "", loading: false });
      },
    );
    return () => {
      cancelled = true;
    };
  }, [nodeId, blockName, selected]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.stopPropagation();
        onClose();
      } else if ((e.key === "ArrowDown" || e.key === "ArrowUp") && runs && runs.length > 0) {
        e.preventDefault();
        const at = runs.findIndex((r) => r.id === selected);
        const next = e.key === "ArrowDown" ? Math.min(runs.length - 1, at + 1) : Math.max(0, at - 1);
        setSelected(runs[next].id);
      }
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [onClose, runs, selected]);

  const current = runs?.find((r) => r.id === selected);

  return createPortal(
    <div className="vars-modal-backdrop" onClick={onClose}>
      <div
        className="vars-modal run-history-modal nodrag nopan"
        role="dialog"
        aria-label={`Run history: ${blockName}`}
        onClick={(e) => e.stopPropagation()}
      >
        <div className="run-history-modal-head">
          <h3>
            Run history <span className="run-history-modal-block">{blockName}</span>
          </h3>
          <button type="button" className="run-history-modal-close" onClick={onClose} aria-label="Close" title="Close (Esc)">
            ✕
          </button>
        </div>
        <div className="run-history-modal-body">
          <div className="run-history-list" role="listbox" aria-label="Runs">
            {error && <div className="run-history-empty">couldn't load the history: {error}</div>}
            {!error && runs === null && <div className="run-history-empty">loading…</div>}
            {runs?.length === 0 && <div className="run-history-empty">no earlier runs of this block</div>}
            {runs?.map((run) => (
              <button
                key={run.id}
                type="button"
                role="option"
                aria-selected={selected === run.id}
                className="run-history-row"
                data-exit={runState(run)}
                data-stale={run.stale || undefined}
                data-selected={selected === run.id || undefined}
                onClick={() => setSelected(run.id)}
                title={`started ${run.startedAt}${run.endedAt ? `, ended ${run.endedAt}` : ""}`}
              >
                <span className="run-history-time">{formatRunTime(run.startedAt)}</span>
                <span className="run-history-exit">{runLabel(run)}</span>
                {run.durationMs !== null && (
                  <span className="run-history-duration">{formatDurationMs(run.durationMs)}</span>
                )}
                {run.stale && (
                  <span className="run-history-stale" title={STALE_HINT}>
                    stale
                  </span>
                )}
              </button>
            ))}
          </div>
          <div className="run-history-detail">
            {current && (
              <div className="run-history-detail-head" data-exit={runState(current)}>
                {formatRunTime(current.startedAt)} · {runLabel(current)}
                {current.durationMs !== null && ` · ${formatDurationMs(current.durationMs)}`}
                {current.stale && <span className="run-history-stale-note"> · stale — {STALE_HINT}</span>}
              </div>
            )}
            {current && output?.id === current.id ? (
              output.loading ? (
                <div className="run-history-empty">loading…</div>
              ) : output.text ? (
                <pre>
                  <code>
                    <AnsiText text={output.text} />
                  </code>
                </pre>
              ) : (
                <div className="run-history-empty">
                  {tty ? "a tty session's output isn't stored" : "no output stored for this run"}
                </div>
              )
            ) : (
              !current && runs && runs.length > 0 && <div className="run-history-empty">select a run</div>
            )}
          </div>
        </div>
      </div>
    </div>,
    document.body,
  );
}
