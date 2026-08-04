import { useState } from "react";

interface TagEditorProps {
  tags: string[];
  onChange: (tags: string[]) => void;
}

/**
 * Small chip-list tag input, shared by `NodeSettings` (a node's own tags)
 * and `DeletableEdge`'s edge editor (an extra edge's tags) — same
 * "type, press Enter to add, × to remove" shape as any other tag field.
 * Doesn't persist anything itself: `onChange` hands back the full replaced
 * list, and the caller's own auto-save (debounced, same as every other
 * field in both of those) takes it from there.
 */
export function TagEditor({ tags, onChange }: TagEditorProps) {
  const [draft, setDraft] = useState("");

  const commit = () => {
    const t = draft.trim();
    setDraft("");
    if (t !== "" && !tags.includes(t)) onChange([...tags, t]);
  };

  return (
    <div className="tag-editor">
      <div className="tag-editor-chips">
        {tags.map((t) => (
          <span className="tag-chip" key={t}>
            {t}
            <button
              type="button"
              className="tag-chip-remove"
              onClick={() => onChange(tags.filter((x) => x !== t))}
              title={`remove tag ${t}`}
            >
              ×
            </button>
          </span>
        ))}
      </div>
      <input
        type="text"
        value={draft}
        onChange={(e) => setDraft(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Enter") {
            e.preventDefault();
            commit();
          } else if (e.key === "Backspace" && draft === "" && tags.length > 0) {
            onChange(tags.slice(0, -1));
          }
        }}
        onBlur={commit}
        placeholder="add a tag, press Enter"
      />
    </div>
  );
}
