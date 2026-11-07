import { useEffect, useState } from "react";
import { activityIsLive, type ActivityAddress, type RunActivity } from "./runActivity";

const labels = {
  preparing: "Preparing…", running: "Running:", between: "Between steps…", interactive: "Interactive:",
  completed: "Completed:", failed: "Failed:", killed: "Killed:", unknown: "Result unknown:",
};
function duration(ms: number): string {
  const seconds = Math.floor(Math.max(0, ms) / 1000);
  return `${String(Math.floor(seconds / 60)).padStart(2, "0")}:${String(seconds % 60).padStart(2, "0")}`;
}
export function ActivityBar({ activities, services, onLocate, onStop, onConsole, consoleOpen, checks, ttyCount, onTerminals, onServices, executionEnabled, checkingVars = false }: {
  activities: RunActivity[];
  services: { total: number; running: number; failed: number };
  checks: { total: number; failed: number } | null;
  ttyCount: number;
  onTerminals: () => void;
  onServices: () => void;
  executionEnabled: boolean;
  /** A run was requested and its variables/arguments are still being
   * resolved (a computed `select` list may have to run its source first). */
  checkingVars?: boolean;
  onLocate: (address: ActivityAddress) => void;
  onStop: (activity: RunActivity) => Promise<void>;
  onConsole: () => void;
  consoleOpen: boolean;
}) {
  const [now, setNow] = useState(Date.now());
  const [expanded, setExpanded] = useState(false);
  const [stopping, setStopping] = useState<string[]>([]);
  const live = activities.filter(activityIsLive);
  const ticking = live.length > 0;
  useEffect(() => {
    if (!ticking) return;
    setNow(Date.now());
    const timer = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(timer);
  }, [ticking]);
  const shown = live.length ? live : [...activities].sort((a, b) => (b.endedAt ?? 0) - (a.endedAt ?? 0)).slice(0, 1);
  const stop = async (a: RunActivity) => {
    setStopping(keys => [...keys, a.key]);
    try { await onStop(a); } finally { setStopping(keys => keys.filter(key => key !== a.key)); }
  };
  function row(a: RunActivity) {
    const address = a.current ?? a.target;
    const isLive = activityIsLive(a);
    const processClock = (a.phase === "running" || a.phase === "interactive") && a.stepStartedAt !== undefined;
    return <div className="activity-row" key={a.key} data-phase={a.phase}>
      <button type="button" className="activity-current" onClick={() => onLocate(address)}
        title={a.message ?? `Show ${address.nodeId}/${address.block}`}>
        <span className={isLive ? "activity-symbol activity-busy" : "activity-symbol"} aria-hidden="true">
          {isLive ? "◌" : a.phase === "completed" ? "✓" : a.phase === "killed" ? "■" : "✕"}
        </span>
        <span role="status">{labels[a.phase]} <span className="activity-address">{address.nodeId}/{address.block}</span></span>
      </button>
      <span className="activity-time">{a.exitCode !== undefined && a.exitCode !== 0 ? `exit ${a.exitCode} · ` : ""}
        {processClock ? "" : "total "}{duration(processClock ? now - a.stepStartedAt! : (a.endedAt ?? now) - a.startedAt)}</span>
      {isLive && (a.runId || a.current) && <button type="button" className="activity-stop"
        disabled={stopping.includes(a.key)} onClick={() => void stop(a)}>
        {stopping.includes(a.key) ? "Stopping…" : "■ Stop"}
      </button>}
    </div>;
  }
  return <div className="activity-bar" aria-label="Current activity">
    {expanded && live.length > 1 && <div className="activity-list" aria-label="Running chains">{live.map(row)}</div>}
    <div className="activity-main">
      <div className="activity-summary">
        {checkingVars ? <span className="activity-idle" role="status">
          <span className="activity-symbol activity-busy" aria-hidden="true">◌</span> Checking variables…
        </span> : live.length > 1 ? <button type="button" onClick={() => setExpanded(!expanded)} aria-expanded={expanded}>
          <span className="activity-symbol activity-busy" aria-hidden="true">◌</span> {live.length} running {expanded ? "▾" : "▴"}
        </button> : shown.length ? row(shown[0]) : <span className="activity-idle">{executionEnabled ? "Ready" : "Execution disabled"}</span>}
      </div>
      <div className="activity-actions">
      {checks && <span className={checks.failed ? "activity-checks activity-failed" : "activity-checks activity-ok"}
        aria-label={`Checks: ${checks.total - checks.failed}/${checks.total} passed`}
        title={checks.failed ? `${checks.failed} of ${checks.total} checks failing` : `All ${checks.total} checks pass`}>
        <span aria-hidden="true">{checks.failed ? "!" : "✓"}</span> <span className="activity-control-label">Checks</span> {checks.total - checks.failed}/{checks.total}
      </span>}
      {ttyCount > 0 && <button type="button" onClick={onTerminals} aria-label={`TTY: ${ttyCount} live sessions`}
        title="Open live terminal sessions">TTY {ttyCount}</button>}
      {services.total > 0 && <button type="button" className={services.failed ? "activity-failed" : ""}
        onClick={onServices} aria-label={`Services: ${services.running}/${services.total} running${services.failed ? `, ${services.failed} failed` : ""}`}
        title={services.failed ? `${services.failed} of ${services.total} services failed — open services` : "Open services"}>
        <span aria-hidden="true">⚙</span> <span className="activity-control-label">Services</span> {services.running}/{services.total}
      </button>}
      {executionEnabled && <button type="button" className="activity-console" aria-label="Console" onClick={onConsole} aria-expanded={consoleOpen}
        title={consoleOpen ? "Collapse the console" : "Open the console"}>
        <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.7" aria-hidden="true">
          <rect x="3" y="4" width="18" height="16" rx="2" />
          <path d="m7 9 3 3-3 3m6 0h4" />
        </svg>
        <span className="activity-control-label">Console</span>
      </button>}
      </div>
    </div>
  </div>;
}
