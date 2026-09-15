import { useEffect, useRef } from "react";
import { AnsiText } from "./AnsiText";

/** One line of the console's aggregated transcript — see `App.tsx`'s own
 * `appendConsoleLine`/`consoleLines`. */
export interface ConsoleLine {
  nodeId: string;
  block: string;
  stream: "stdout" | "stderr";
  text: string;
}

/**
 * The "console" — a bottom-docked, semi-transparent panel that slides up
 * over the canvas (never claims layout space) showing a live, aggregated
 * transcript of the most recent run across every block it touched. The
 * web counterpart to the TUI's own Output pane — same collapse/auto-expand
 * policy (see `App.tsx`'s `consoleCollapsed`/`anyBlockRunning` effect),
 * same "not persisted, just a live monitor" scope.
 */
export function ConsolePanel({
  open,
  lines,
  onClose,
}: {
  open: boolean;
  lines: ConsoleLine[];
  onClose: () => void;
}) {
  const bodyRef = useRef<HTMLDivElement>(null);
  // Pinned to the live tail — a console you have to scroll down every time
  // something new prints isn't much of a "live monitor."
  useEffect(() => {
    if (open && bodyRef.current) {
      bodyRef.current.scrollTop = bodyRef.current.scrollHeight;
    }
  }, [open, lines.length]);

  return (
    <div className={open ? "console-panel console-panel-open" : "console-panel"} aria-hidden={!open}>
      <div className="console-panel-header">
        <span>Console</span>
        <button type="button" className="console-panel-close" onClick={onClose} title="Collapse the console">
          ✕
        </button>
      </div>
      <div className="console-panel-body" ref={bodyRef}>
        {lines.length === 0 ? (
          <div className="console-panel-empty">Nothing has run yet this session.</div>
        ) : (
          lines.map((line, i) => (
            <div key={i} className={line.stream === "stderr" ? "console-line console-line-stderr" : "console-line"}>
              <span className="console-line-addr">
                {line.nodeId}::{line.block}
              </span>{" "}
              <AnsiText text={line.text} />
            </div>
          ))
        )}
      </div>
    </div>
  );
}
