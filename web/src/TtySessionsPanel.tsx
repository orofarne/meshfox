import type { ActiveRunDto } from "./api";

interface TtySessionsPanelProps {
  /** Every live (`status === "running"`) `tty` session this server process
   * currently knows about — the caller filters `fetchActiveRuns()`'s own
   * result down to `kind === "tty"` before passing it in. */
  sessions: ActiveRunDto[];
  onClose: () => void;
  /** Re-mounts a `TtyPanel` in attach mode for this address (see
   * `App.tsx`'s `ttySession.attachTo`) — joins the session already
   * running server-side (`GET /api/run/tty/attach`) instead of starting a
   * fresh one, same as reattaching to a `tmux` pane. */
  onReopen: (nodeId: string, block: string) => void;
  onKill: (nodeId: string, block: string) => void;
}

/**
 * Global panel listing every live `tty` session this server process
 * currently knows about, whether or not any tab still has it open —
 * closing the tab/panel a `tty` block was opened from no longer ends the
 * session (see `tty_registry`'s own module doc comment,
 * `crates/server/src/tty_registry.rs`), so this is how to find and
 * re-attach to one afterward, the same way `ServicePanel` already lets you
 * find a `service` block's own status/log across page reloads. Reuses
 * `ServicePanel`'s own card/header chrome (`.service-panel*` classes) but a
 * flat single-column list — there's no per-session detail to show besides
 * "reopen"/"kill".
 */
export function TtySessionsPanel({ sessions, onClose, onReopen, onKill }: TtySessionsPanelProps) {
  return (
    <div className="vars-modal-backdrop" onClick={onClose}>
      <div className="service-panel" onClick={(e) => e.stopPropagation()}>
        <div className="service-panel-header">
          <h3>Live terminals</h3>
          <button type="button" onClick={onClose}>
            ×
          </button>
        </div>
        <div className="service-panel-body">
          <div className="tty-sessions-list">
            {sessions.length === 0 && <p className="vars-modal-hint">No live terminal sessions.</p>}
            {sessions.map((s) => (
              <div className="tty-sessions-row" key={`${s.nodeId}/${s.block}`}>
                <span className="service-panel-status-dot service-panel-status-running" />
                <span className="service-panel-row-block">{s.block}</span>
                <span className="service-panel-row-node">{s.nodeId}</span>
                <span className="tty-sessions-row-actions">
                  <button type="button" onClick={() => onReopen(s.nodeId, s.block)}>
                    reopen
                  </button>
                  <button type="button" onClick={() => onKill(s.nodeId, s.block)}>
                    kill
                  </button>
                </span>
              </div>
            ))}
          </div>
        </div>
      </div>
    </div>
  );
}
