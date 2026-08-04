interface AutoLayoutConfirmDialogProps {
  onConfirm: () => void;
  onCancel: () => void;
}

/**
 * Confirmation dialog for the edit-mode toolbar's "Auto-layout" button (see
 * App.tsx) — clearing every node's stored position/size is destructive (it
 * overwrites the file, not just this session's view), so this is the one
 * gate before `clearLayout` actually runs. Reuses `DeleteNodeDialog`'s modal
 * styling.
 */
export function AutoLayoutConfirmDialog({ onConfirm, onCancel }: AutoLayoutConfirmDialogProps) {
  return (
    <div className="vars-modal-backdrop" onClick={onCancel}>
      <div className="vars-modal" onClick={(e) => e.stopPropagation()}>
        <h3>Reset layout?</h3>
        <p className="vars-modal-hint">
          This clears every node's stored position and size, reverting the whole document to auto-placed. This can't
          be undone.
        </p>
        <div className="vars-modal-actions">
          <button type="button" onClick={onCancel}>
            cancel
          </button>
          <button type="button" className="node-settings-delete-button" onClick={onConfirm}>
            reset layout
          </button>
        </div>
      </div>
    </div>
  );
}
