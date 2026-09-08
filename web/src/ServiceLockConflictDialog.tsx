interface ServiceLockConflictDialogProps {
  block: string;
  ownerPid: number;
  ownerDesc: string;
  onKillAndStart: () => void;
  onCancel: () => void;
}

/**
 * Confirmation dialog for a `"service-lock-conflict"` run event (see
 * App.tsx's `serviceConflict` state and `api.ts`'s `RunEvent`) —
 * **experimental**, see SPEC.md's "Service blocks (experimental)". A
 * service's lock file (`.meshfox/services/...`) is already held by another
 * live-or-stale process; per the product decision this is always surfaced
 * here, never silently resolved either way. Reuses `AutoLayoutConfirmDialog`'s
 * modal shape, with `DeleteNodeDialog`'s red "destructive" button style
 * since confirming here kills another process.
 */
export function ServiceLockConflictDialog({
  block,
  ownerPid,
  ownerDesc,
  onKillAndStart,
  onCancel,
}: ServiceLockConflictDialogProps) {
  return (
    <div className="vars-modal-backdrop" onClick={onCancel}>
      <div className="vars-modal" onClick={(e) => e.stopPropagation()}>
        <h3>Service already running</h3>
        <p className="vars-modal-hint">
          "{block}" is already running elsewhere — pid {ownerPid}, started via {ownerDesc}. Kill that process and
          start a fresh one here, or cancel and leave it running.
        </p>
        <div className="vars-modal-actions">
          <button type="button" onClick={onCancel}>
            cancel
          </button>
          <button type="button" className="node-settings-delete-button" onClick={onKillAndStart}>
            kill and start
          </button>
        </div>
      </div>
    </div>
  );
}
