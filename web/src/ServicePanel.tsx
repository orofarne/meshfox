import { useEffect, useState } from "react";
import type { ServiceStatusDto } from "./types";
import { fetchServiceLog } from "./api";
import { AnsiText } from "./AnsiText";

interface ServicePanelProps {
  services: ServiceStatusDto[];
  /** Which service to pre-select when the panel opens — the node whose
   * title-bar `ServiceBadge` was clicked. `null` for "not scoped to any
   * particular node" (still shows every service). */
  focusNodeId: string | null;
  onClose: () => void;
  onStop: (nodeId: string, block: string) => void;
  onRestart: (nodeId: string, block: string) => void;
}

function serviceKey(nodeId: string, block: string): string {
  return `${nodeId}/${block}`;
}

function formatUptime(ms: number): string {
  const totalSeconds = Math.floor(ms / 1000);
  const h = Math.floor(totalSeconds / 3600);
  const m = Math.floor((totalSeconds % 3600) / 60);
  const s = totalSeconds % 60;
  if (h > 0) return `${h}h ${m}m`;
  if (m > 0) return `${m}m ${s}s`;
  return `${s}s`;
}

/**
 * Global (not per-node) panel listing every `service` block this server
 * process knows about — status, pid, uptime, CPU/mem, log, and Stop/Restart
 * — opened via any node's title-bar `ServiceBadge`. **Experimental**, see
 * SPEC.md's "Service blocks (experimental)". The first genuinely global
 * panel in this webui: constraints (the closest existing precedent) have no
 * such surface, only per-node inline badges — see that feature's own design
 * notes for why this needed something new rather than reusing anything of
 * theirs beyond the badge's own visual language.
 */
export function ServicePanel({ services, focusNodeId, onClose, onStop, onRestart }: ServicePanelProps) {
  const initialSelected = focusNodeId
    ? services.find((s) => s.nodeId === focusNodeId)
    : services[0];
  const [selectedKey, setSelectedKey] = useState<string | null>(
    initialSelected ? serviceKey(initialSelected.nodeId, initialSelected.block) : null,
  );
  const selected = services.find((s) => serviceKey(s.nodeId, s.block) === selectedKey);

  const [log, setLog] = useState<{ stream: "stdout" | "stderr"; text: string }[]>([]);
  useEffect(() => {
    if (!selected) {
      setLog([]);
      return;
    }
    let cancelled = false;
    const poll = () => {
      fetchServiceLog(selected.nodeId, selected.block)
        .then((lines) => {
          if (!cancelled) setLog(lines);
        })
        .catch(() => {
          // Best-effort — the service may have just been stopped/removed.
        });
    };
    poll();
    const id = setInterval(poll, 2000);
    return () => {
      cancelled = true;
      clearInterval(id);
    };
  }, [selected?.nodeId, selected?.block]);

  return (
    <div className="vars-modal-backdrop" onClick={onClose}>
      <div className="service-panel" onClick={(e) => e.stopPropagation()}>
        <div className="service-panel-header">
          <h3>Services</h3>
          <button type="button" onClick={onClose}>
            ×
          </button>
        </div>
        <div className="service-panel-body">
          <div className="service-panel-list">
            {services.length === 0 && <p className="vars-modal-hint">No services running.</p>}
            {services.map((s) => {
              const key = serviceKey(s.nodeId, s.block);
              return (
                <button
                  type="button"
                  key={key}
                  className={key === selectedKey ? "service-panel-row service-panel-row-selected" : "service-panel-row"}
                  onClick={() => setSelectedKey(key)}
                >
                  <span className={`service-panel-status-dot service-panel-status-${s.status}`} />
                  <span className="service-panel-row-block">{s.block}</span>
                  <span className="service-panel-row-node">{s.nodeId}</span>
                </button>
              );
            })}
          </div>
          <div className="service-panel-detail">
            {selected ? (
              <>
                <div className="service-panel-detail-meta">
                  <span>pid {selected.pid}</span>
                  <span>uptime {formatUptime(selected.uptimeMs)}</span>
                  {/* `!= null` rather than `!== undefined` — defensive:
                   * the server (`ServiceDto`) is annotated to omit a `None`
                   * resource sample entirely rather than send JSON `null`,
                   * but this field crossing the Rust/TS boundary is exactly
                   * the kind of thing worth not trusting blindly (a real
                   * regression here once already reached `.toFixed()` on
                   * `null` and crashed the whole page). */}
                  {selected.cpuPercent != null && <span>cpu {selected.cpuPercent.toFixed(0)}%</span>}
                  {selected.memBytes != null && (
                    <span>mem {(selected.memBytes / 1024 / 1024).toFixed(0)} MB</span>
                  )}
                  {selected.status === "crashed" && selected.exitCode != null && (
                    <span className="service-panel-exit-code">exit {selected.exitCode}</span>
                  )}
                </div>
                <div className="service-panel-detail-actions">
                  {/* Nothing live to stop once it's already `"stopped"`/
                   * `"crashed"` — only shown while there's a real process
                   * behind it. `restart` stays available regardless: it's
                   * exactly how you bring a stopped/crashed one back. */}
                  {selected.status === "running" && (
                    <button type="button" onClick={() => onStop(selected.nodeId, selected.block)}>
                      stop
                    </button>
                  )}
                  <button type="button" onClick={() => onRestart(selected.nodeId, selected.block)}>
                    restart
                  </button>
                </div>
                <pre className="service-panel-log">
                  <AnsiText text={log.map((l) => l.text).join("\n")} />
                </pre>
              </>
            ) : (
              <p className="vars-modal-hint">Select a service to see its log.</p>
            )}
          </div>
        </div>
      </div>
    </div>
  );
}
