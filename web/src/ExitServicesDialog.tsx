import { useEffect, useState } from "react";
import type { ServiceStatusDto } from "./types";

const LIVE = new Set<ServiceStatusDto["status"]>(["running", "starting", "restarting", "stopping", "stop_failed"]);

/** Services that outlive this page: alive and not `autostop`. */
export function persistentServices(services: ServiceStatusDto[]): ServiceStatusDto[] {
  return services.filter((s) => s.autostop !== true && LIVE.has(s.status));
}

interface ExitServicesDialogProps {
  services: ServiceStatusDto[];
  /** Stops one service; rejects when the stop failed. */
  onStop: (nodeId: string, block: string) => Promise<void>;
  /** Closes the page (the tab may refuse; the caller then says so). */
  onClose: () => void;
  onCancel: () => void;
}

/**
 * Asks what to do with still-running services before leaving: stop them and
 * close, or close and leave them running (the worker keeps them, like a
 * page reload does). Interface only — it uses the ordinary stop endpoint.
 */
export function ExitServicesDialog({ services, onStop, onClose, onCancel }: Readonly<ExitServicesDialogProps>) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // Esc cancels, like a native dialog (no key handler on a plain div).
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape" && !busy) onCancel();
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [busy, onCancel]);

  const stopAndClose = async () => {
    setBusy(true);
    setError(null);
    const failed: string[] = [];
    for (const s of services) {
      try {
        await onStop(s.nodeId, s.block);
      } catch (e) {
        failed.push(`${s.block}: ${String(e)}`);
      }
    }
    setBusy(false);
    if (failed.length) setError(`Failed to stop ${failed.join("; ")}`);
    else onClose();
  };

  return (
    <div className="vars-modal-backdrop">
      <div className="vars-modal" role="dialog" aria-modal="true" aria-label="Services still running">
        <h3>Services still running</h3>
        <p className="vars-modal-hint">These services keep running after you leave:</p>
        <ul>
          {services.map((s) => (
            <li key={`${s.nodeId}/${s.block}`}>
              {s.block} <span className="vars-modal-hint">({s.status})</span>
            </li>
          ))}
        </ul>
        {error && <p role="alert">{error}</p>}
        <div className="vars-modal-actions">
          <button type="button" disabled={busy} onClick={onCancel}>
            cancel
          </button>
          <button type="button" disabled={busy} onClick={onClose}>
            keep running &amp; close
          </button>
          <button type="button" className="autolayout-confirm-button" disabled={busy} onClick={stopAndClose}>
            {busy ? "stopping…" : "stop services & close"}
          </button>
        </div>
      </div>
    </div>
  );
}
