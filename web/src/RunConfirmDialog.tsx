import { useEffect, useRef, type KeyboardEvent } from "react";

interface RunConfirmDialogProps {
  blocks: string[];
  onConfirm: () => void;
  onCancel: () => void;
}

/** Uses Configure's modal card and actions; approval belongs to one run. */
export function RunConfirmDialog({ blocks, onConfirm, onCancel }: RunConfirmDialogProps) {
  const cancelRef = useRef<HTMLButtonElement>(null);
  const confirmRef = useRef<HTMLButtonElement>(null);
  useEffect(() => {
    const previous = document.activeElement;
    cancelRef.current?.focus();
    return () => { if (previous instanceof HTMLElement && previous.isConnected) previous.focus(); };
  }, []);

  const onKeyDown = (event: KeyboardEvent) => {
    event.stopPropagation();
    if (event.key === "Escape") {
      event.preventDefault();
      onCancel();
    } else if (event.key === "Tab") {
      event.preventDefault();
      const next = document.activeElement === cancelRef.current ? confirmRef : cancelRef;
      next.current?.focus();
    }
  };

  return (
    <div className="vars-modal-backdrop" onClick={onCancel} onKeyDown={onKeyDown}>
      <form className="vars-modal run-confirm-modal" role="dialog" aria-modal="true"
        aria-labelledby="run-confirm-title" aria-describedby="run-confirm-hint"
        onClick={(event) => event.stopPropagation()}
        onSubmit={(event) => { event.preventDefault(); onConfirm(); }}>
        <h3 id="run-confirm-title">Confirm run</h3>
        <p id="run-confirm-hint" className="vars-modal-hint">
          These blocks may perform destructive operations. Approve them for this run?
        </p>
        <ul className="run-confirm-blocks">
          {blocks.map((block) => <li key={block}><code>{block}</code></li>)}
        </ul>
        <div className="vars-modal-actions">
          <button ref={cancelRef} type="button" onClick={onCancel}>cancel</button>
          <button ref={confirmRef} type="submit">run</button>
        </div>
      </form>
    </div>
  );
}
