interface ResetSessionConfirmDialogProps {
  onConfirm: () => void;
  onCancel: () => void;
}

/**
 * Confirmation dialog for the toolbar's "↺ reset session" button (see
 * App.tsx). The reset itself is purely in-memory — it never touches the
 * canvas file or any persisted `<!-- meshfox:output ... -->` cache, just
 * this session's "already ran" bookkeeping — but its effect is still easy
 * to trigger by accident (same row as "🔍 search") and mildly costly to
 * shrug off (every dependency in the next `⛓ run chain` re-runs for real
 * instead of skipping ones already known fresh), so it gets the same gate
 * as the genuinely destructive dialogs. Reuses `AutoLayoutConfirmDialog`'s
 * modal styling/accent button, not `DeleteNodeDialog`'s red one — nothing
 * here is deleted.
 */
export function ResetSessionConfirmDialog({ onConfirm, onCancel }: ResetSessionConfirmDialogProps) {
  return (
    <div className="vars-modal-backdrop" onClick={onCancel}>
      <div className="vars-modal" onClick={(e) => e.stopPropagation()}>
        <h3>Reset session?</h3>
        <p className="vars-modal-hint">
          This forgets which blocks already ran successfully this session, so the next ⛓ run chain re-runs every
          dependency instead of skipping unchanged ones. Doesn't touch the canvas file or any saved output.
        </p>
        <div className="vars-modal-actions">
          <button type="button" onClick={onCancel}>
            cancel
          </button>
          <button type="button" className="autolayout-confirm-button" onClick={onConfirm}>
            reset session
          </button>
        </div>
      </div>
    </div>
  );
}
