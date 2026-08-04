export type DeleteMode = "subtree" | "reparent";

interface DeleteNodeDialogProps {
  title: string;
  /** Number of direct children — determines whether there's actually a
   * choice to make (a childless node only ever has one way to delete it). */
  childCount: number;
  /** Title of the node's own parent — only used (and only defined) when
   * `childCount > 0`, to name the "move them up" option's destination. */
  parentTitle?: string;
  onDelete: (mode: DeleteMode) => void;
  onCancel: () => void;
}

/**
 * Confirmation dialog for the title bar's delete button (see MeshNode) —
 * replaces the old settings-modal "delete node" confirm, which only ever
 * deleted the whole subtree. A node with children now asks *how* to handle
 * them instead of assuming: drop them too, or promote them to this node's
 * own parent (`mdcanvas::delete_node_reparent_children` on the server side).
 * Reuses `VarsForm`/`NodeSettings`' modal styling.
 */
export function DeleteNodeDialog({ title, childCount, parentTitle, onDelete, onCancel }: DeleteNodeDialogProps) {
  return (
    <div className="vars-modal-backdrop" onClick={onCancel}>
      <div className="vars-modal node-delete-modal" onClick={(e) => e.stopPropagation()}>
        <h3>Delete "{title}"?</h3>
        {childCount > 0 ? (
          <>
            <p className="vars-modal-hint">
              This node has {childCount} child node{childCount === 1 ? "" : "s"}. What should happen to{" "}
              {childCount === 1 ? "it" : "them"}?
            </p>
            <div className="node-delete-options">
              <button type="button" className="node-settings-delete-button" onClick={() => onDelete("subtree")}>
                delete node and {childCount === 1 ? "its child" : "all its children"}
              </button>
              <button type="button" className="node-settings-delete-button" onClick={() => onDelete("reparent")}>
                delete only this node — move {childCount === 1 ? "it" : "them"} up to "{parentTitle}"
              </button>
            </div>
            <div className="vars-modal-actions">
              <button type="button" onClick={onCancel}>
                cancel
              </button>
            </div>
          </>
        ) : (
          <>
            <p className="vars-modal-hint">This can't be undone.</p>
            <div className="vars-modal-actions">
              <button type="button" onClick={onCancel}>
                cancel
              </button>
              <button type="button" className="node-settings-delete-button" onClick={() => onDelete("subtree")}>
                delete
              </button>
            </div>
          </>
        )}
      </div>
    </div>
  );
}
