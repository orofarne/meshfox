import { useEffect, useState } from "react";

/** The wrapper of one labelled field. A list is several controls, which one
 * `<label>` would tie to the first only (and read out as one long name), so a
 * list sits in a plain box and labels its own group. */
export function FieldBox({ list, children }: { list: boolean; children: React.ReactNode }) {
  return list ? <div className="vars-modal-field">{children}</div> : <label className="vars-modal-field">{children}</label>;
}

/** The stored form of a `list` variable is a JSON array of strings. */
export function parseListValue(value: string | undefined): string[] {
  if (!value) return [];
  try {
    const parsed: unknown = JSON.parse(value);
    if (!Array.isArray(parsed)) return [];
    return parsed.map((item) => (typeof item === "string" ? item : String(item)));
  } catch {
    return [];
  }
}

export function formatListValue(items: string[]): string {
  return JSON.stringify(items);
}

/**
 * Input for a `type="list"` variable (SPEC.md, "List variables"). With
 * `choices` it is a set of checkboxes (the value keeps the choices' order);
 * without, a textarea with one element per line — a representation for
 * typing only, the variable itself has no separator. The value travels as
 * the stored JSON array.
 */
export function ListField({
  value,
  choices,
  onChange,
  label,
  autoFocus,
}: {
  value: string | undefined;
  choices?: string[];
  onChange: (next: string) => void;
  label: string;
  autoFocus?: boolean;
}) {
  const items = parseListValue(value);
  if (choices && choices.length > 0) {
    // Elements the choices do not offer (a stale value) stay visible and checked.
    const extras = items.filter((item) => !choices.includes(item));
    const all = [...choices, ...extras];
    const toggle = (item: string, on: boolean) => {
      const next = all.filter((c) => (c === item ? on : items.includes(c)));
      onChange(formatListValue(next));
    };
    return (
      <span role="group" aria-label={label} className="mesh-list-choices">
        {all.map((choice, i) => (
          <label key={choice} className="mesh-list-choice">
            <input
              type="checkbox"
              checked={items.includes(choice)}
              onChange={(e) => toggle(choice, e.target.checked)}
              autoFocus={autoFocus && i === 0}
            />{" "}
            {choice}
          </label>
        ))}
      </span>
    );
  }
  return <LinesField value={items} onChange={(next) => onChange(formatListValue(next))} label={label} autoFocus={autoFocus} />;
}

/** A textarea holding one element per line. The text is kept locally so a
 * trailing newline (the next, still empty element) survives typing. */
function LinesField({
  value,
  onChange,
  label,
  autoFocus,
}: {
  value: string[];
  onChange: (next: string[]) => void;
  label: string;
  autoFocus?: boolean;
}) {
  const [text, setText] = useState(value.join("\n"));
  const external = value.join("\n");
  // Follow an outside change (a reset, a reload) but not our own typing.
  useEffect(() => {
    setText((current) => {
      const mine = current
        .split("\n")
        .map((line) => line.trim())
        .filter((line) => line.length > 0)
        .join("\n");
      return mine === external ? current : external;
    });
  }, [external]);
  return (
    <textarea
      aria-label={label}
      rows={Math.min(8, Math.max(3, value.length + 1))}
      value={text}
      placeholder="one element per line"
      autoFocus={autoFocus}
      onChange={(e) => {
        setText(e.target.value);
        onChange(
          e.target.value
            .split("\n")
            .map((line) => line.trim())
            .filter((line) => line.length > 0),
        );
      }}
    />
  );
}
