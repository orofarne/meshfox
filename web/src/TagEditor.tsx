import { useState } from "react";

interface TagEditorProps {
  tags: string[];
  onChange: (tags: string[]) => void;
  /** Every tag worth suggesting — typically every distinct tag already
   * used somewhere else in the document (see App.tsx's `documentTags`,
   * `NodeSettings`' own `allNodes`-derived list). Narrowed down to
   * whatever matches the current draft and isn't already in `tags`, shown
   * as a small clickable dropdown while the input has focus. Omit (or
   * pass `[]`) for "no suggestions", same as before this existed. */
  suggestions?: string[];
}

/**
 * Small chip-list tag input, shared by `NodeSettings` (a node's own tags)
 * and `DeletableEdge`'s edge editor (an extra edge's tags) — same
 * "type, press Enter to add, × to remove" shape as any other tag field,
 * plus an optional autocomplete dropdown (`suggestions`) so a tag already
 * used elsewhere in the document is one click away instead of having to be
 * retyped exactly (a typo here silently creates an unrelated new tag
 * rather than erroring, since tags have no fixed vocabulary — see
 * SPEC.md). Doesn't persist anything itself: `onChange` hands back the
 * full replaced list, and the caller's own auto-save (debounced, same as
 * every other field in both of those) takes it from there.
 */
export function TagEditor({ tags, onChange, suggestions = [] }: TagEditorProps) {
  const [draft, setDraft] = useState("");
  const [focused, setFocused] = useState(false);

  const commit = (value?: string) => {
    const t = (value ?? draft).trim();
    setDraft("");
    if (t !== "" && !tags.includes(t)) onChange([...tags, t]);
  };

  const query = draft.trim().toLowerCase();
  const matches = suggestions.filter((s) => !tags.includes(s) && s.toLowerCase().includes(query));
  const showSuggestions = focused && matches.length > 0;

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
      <div className="tag-editor-input-wrap">
        <input
          type="text"
          value={draft}
          onChange={(e) => setDraft(e.target.value)}
          onFocus={() => setFocused(true)}
          onKeyDown={(e) => {
            if (e.key === "Enter") {
              e.preventDefault();
              commit();
            } else if (e.key === "Backspace" && draft === "" && tags.length > 0) {
              onChange(tags.slice(0, -1));
            } else if (e.key === "Escape") {
              setFocused(false);
            }
          }}
          onBlur={() => {
            commit();
            setFocused(false);
          }}
          placeholder="add a tag, press Enter"
        />
        {showSuggestions && (
          <ul className="tag-editor-suggestions">
            {matches.map((s) => (
              <li key={s}>
                {/* `onMouseDown` (not `onClick`) prevents the default focus
                 * shift, which would otherwise blur the input — and commit
                 * whatever's currently typed — before this button's own
                 * click ever fires. */}
                <button type="button" onMouseDown={(e) => e.preventDefault()} onClick={() => commit(s)}>
                  {s}
                </button>
              </li>
            ))}
          </ul>
        )}
      </div>
    </div>
  );
}
