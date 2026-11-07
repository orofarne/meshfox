import { useEffect, useId, useMemo, useRef, useState, type KeyboardEvent } from "react";
import { describePending, filterChoices } from "./choiceFilter";
import type { ChoicesPending } from "./types";

interface ChoiceSelectProps {
  value: string;
  choices: readonly string[];
  onChange: (value: string) => void;
  /** Accessible name of the control (the field's label). */
  label: string;
  autoFocus?: boolean;
  /** Set when the choices were to be computed and could not be: the control
   * is disabled and says why instead of offering an empty list. */
  pending?: ChoicesPending;
}

/**
 * A `select` variable's control: a button that opens a filterable list — the
 * browser counterpart of the TUI's select picker, with the same keys. Typing
 * narrows the list (case-insensitive substring, `filterChoices`), ↑/↓ move
 * (wrapping), Home/End jump, Enter picks, Ctrl-U clears the text, and Esc
 * clears the text first and closes the list on the second press.
 */
export function ChoiceSelect({ value, choices, onChange, label, autoFocus, pending }: ChoiceSelectProps) {
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState("");
  const [active, setActive] = useState(0);
  const root = useRef<HTMLSpanElement>(null);
  const button = useRef<HTMLButtonElement>(null);
  const listId = useId();

  const visible = useMemo(() => filterChoices(choices, query), [choices, query]);

  // Narrowing keeps the highlighted entry when it is still listed.
  const setFilter = (next: string) => {
    const keep = visible[active];
    const nextVisible = filterChoices(choices, next);
    const kept = keep === undefined ? -1 : nextVisible.indexOf(keep);
    setQuery(next);
    setActive(kept >= 0 ? kept : 0);
  };

  const openList = () => {
    setQuery("");
    setActive(Math.max(0, choices.indexOf(value)));
    setOpen(true);
  };

  const close = (refocus: boolean) => {
    setOpen(false);
    setQuery("");
    if (refocus) button.current?.focus();
  };

  const pick = (index: number) => {
    const choice = choices[index];
    if (choice !== undefined) onChange(choice);
    close(true);
  };

  useEffect(() => {
    if (!open) return;
    root.current?.querySelector('[aria-selected="true"]')?.scrollIntoView({ block: "nearest" });
  }, [open, active, query]);

  if (pending) {
    return (
      <span className="choice-select">
        <button type="button" className="choice-select-button" role="combobox" aria-label={label} aria-expanded={false} disabled>
          {value || "—"} ▾
        </button>
        <span className="choice-select-pending" role="note">
          No choices to pick from: {describePending(pending)}.
        </span>
      </span>
    );
  }

  const onFilterKey = (e: KeyboardEvent<HTMLInputElement>) => {
    const n = visible.length;
    if (e.key === "ArrowDown" && n) {
      e.preventDefault();
      setActive((active + 1) % n);
    } else if (e.key === "ArrowUp" && n) {
      e.preventDefault();
      setActive((active + n - 1) % n);
    } else if (e.key === "Home") {
      e.preventDefault();
      setActive(0);
    } else if (e.key === "End") {
      e.preventDefault();
      setActive(Math.max(0, n - 1));
    } else if (e.key === "Enter") {
      // Picks; never submits the form the list sits in.
      e.preventDefault();
      if (n) pick(visible[active]);
    } else if (e.key === "Escape") {
      // Same two-step Esc as the TUI; never closes the dialog around it.
      e.preventDefault();
      e.stopPropagation();
      if (query) setFilter("");
      else close(true);
    } else if (e.key === "u" && e.ctrlKey) {
      e.preventDefault();
      setFilter("");
    } else if (e.key === "Tab") {
      close(false);
    }
  };

  return (
    <span
      className="choice-select"
      ref={root}
      onBlur={(e) => {
        if (open && !root.current?.contains(e.relatedTarget as Node | null)) close(false);
      }}
    >
      <button
        type="button"
        ref={button}
        className="choice-select-button"
        role="combobox"
        aria-label={label}
        aria-haspopup="listbox"
        aria-expanded={open}
        aria-controls={open ? listId : undefined}
        autoFocus={autoFocus}
        onClick={(e) => {
          e.preventDefault();
          if (open) close(false);
          else openList();
        }}
        onKeyDown={(e) => {
          if (!open && (e.key === "ArrowDown" || e.key === "ArrowUp")) {
            e.preventDefault();
            openList();
          }
        }}
      >
        {value || "—"} ▾
      </button>
      {open && (
        <span
          className="choice-select-popup"
          onClick={(e) => {
            // The list sits inside the field's <label>, whose default action
            // would click the button again (and reopen the list just closed).
            e.preventDefault();
            e.stopPropagation();
          }}
        >
          <input
            type="text"
            className="choice-select-filter"
            aria-label={`Filter ${label}`}
            placeholder={`Filter ${choices.length} choices…`}
            value={query}
            autoFocus
            onChange={(e) => setFilter(e.target.value)}
            onKeyDown={onFilterKey}
          />
          <ul className="choice-select-list" role="listbox" id={listId} aria-label={label}>
            {visible.map((index, row) => (
              <li
                key={choices[index]}
                role="option"
                aria-selected={row === active}
                className={choices[index] === value ? "choice-select-current" : undefined}
                onMouseDown={(e) => e.preventDefault()}
                onClick={() => pick(index)}
              >
                {choices[index]}
              </li>
            ))}
            {visible.length === 0 && <li className="choice-select-empty">Nothing matches “{query}”.</li>}
          </ul>
        </span>
      )}
    </span>
  );
}
