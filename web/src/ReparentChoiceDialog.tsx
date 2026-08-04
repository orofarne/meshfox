interface Candidate {
  id: string;
  title: string;
}

interface ReparentChoiceDialogProps {
  /** Title of the node whose structural parent edge is being deleted. */
  nodeTitle: string;
  /** Its other declared incoming edges — one of these takes over as the
   * new structural parent. Always has at least 2 entries: with exactly one,
   * App.tsx skips this dialog and promotes it directly. */
  candidates: Candidate[];
  onChoose: (newParentId: string) => void;
  onCancel: () => void;
}

/**
 * Asks which of a node's *other* incoming edges should become its new
 * structural (nesting) parent, once the current one is deleted — only shown
 * when there's more than one candidate; with just one, App.tsx promotes it
 * without asking. Reuses `VarsForm`/`NodeSettings`' modal styling, same
 * "stacked choice buttons" shape as `DeleteNodeDialog`.
 */
export function ReparentChoiceDialog({ nodeTitle, candidates, onChoose, onCancel }: ReparentChoiceDialogProps) {
  return (
    <div className="vars-modal-backdrop" onClick={onCancel}>
      <div className="vars-modal node-delete-modal" onClick={(e) => e.stopPropagation()}>
        <h3>Delete "{nodeTitle}"'s parent link?</h3>
        <p className="vars-modal-hint">
          It has {candidates.length} other incoming edges. Which one should become its new parent?
        </p>
        <div className="node-delete-options">
          {candidates.map((c) => (
            <button
              type="button"
              key={c.id}
              className="node-settings-delete-button"
              onClick={() => onChoose(c.id)}
            >
              {c.title}
            </button>
          ))}
        </div>
        <div className="vars-modal-actions">
          <button type="button" onClick={onCancel}>
            cancel
          </button>
        </div>
      </div>
    </div>
  );
}
