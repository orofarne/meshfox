import { useEffect, useRef, useState } from "react";
import { browserOffset, isValidOffset, joinDateTime, splitDateTime } from "./dateValues";

interface DateFieldProps {
  kind: "date" | "datetime";
  /** Canonical value (`YYYY-MM-DD` / RFC3339 with an offset), or "" for none. */
  value: string;
  onChange: (value: string) => void;
  label: string;
  autoFocus?: boolean;
  required?: boolean;
}

/** Focus the returned ref's element once on mount when `enabled` (stands in for `autoFocus`). */
function useFocusOnMount(enabled: boolean | undefined) {
  const ref = useRef<HTMLInputElement>(null);
  useEffect(() => {
    if (enabled) ref.current?.focus();
  }, [enabled]);
  return ref;
}

/**
 * The input of a `date`/`datetime` variable: the browser's own calendar
 * (`<input type="date">` / `type="datetime-local">`), which can also be
 * typed into. The native datetime control holds no offset, so a datetime
 * also shows its offset as a small editable field next to it; it starts at
 * the browser's own offset and the pair is always combined into the one
 * canonical RFC3339 value that is stored and sent.
 */
export function DateField({ kind, value, onChange, label, autoFocus, required }: Readonly<DateFieldProps>) {
  const dateRef = useFocusOnMount(kind === "date" && autoFocus);
  if (kind === "date") {
    return (
      <input
        type="date"
        className="date-field"
        value={value}
        onChange={(e) => onChange(e.target.value)}
        aria-label={label}
        ref={dateRef}
        required={required}
      />
    );
  }
  return <DateTimeInputs value={value} onChange={onChange} label={label} autoFocus={autoFocus} required={required} />;
}

function DateTimeInputs({
  value,
  onChange,
  label,
  autoFocus,
  required,
}: Readonly<Omit<DateFieldProps, "kind">>) {
  const wallRef = useFocusOnMount(autoFocus);
  const parts = splitDateTime(value);
  // The wall-clock and offset the user is editing. Kept locally because a
  // half-typed datetime-local reports "" (the canonical value is then empty)
  // and the offset must survive that.
  const [wall, setWall] = useState(parts?.wall ?? "");
  const [offset, setOffset] = useState(parts?.offset ?? browserOffset(parts?.wall ?? ""));
  // Once the offset has been typed (or came with the value) a later pick of
  // the date/time must not replace it with the browser's.
  const [offsetKept, setOffsetKept] = useState(parts !== null);

  // Follow a value set from outside (a form reset, the initial fetch).
  useEffect(() => {
    const next = splitDateTime(value);
    if (next) {
      setWall(next.wall);
      setOffset(next.offset);
      setOffsetKept(true);
    }
  }, [value]);

  const emit = (nextWall: string, nextOffset: string) => {
    onChange(isValidOffset(nextOffset) ? joinDateTime(nextWall, nextOffset) : "");
  };

  return (
    <span className="date-field-time">
      <input
        type="datetime-local"
        className="date-field"
        step={1}
        value={wall}
        onChange={(e) => {
          const nextWall = e.target.value;
          // A first pick takes the browser's offset *at that time* (DST-aware).
          const nextOffset = offsetKept ? offset : browserOffset(nextWall);
          setWall(nextWall);
          setOffset(nextOffset);
          emit(nextWall, nextOffset);
        }}
        aria-label={`${label} (date and time)`}
        ref={wallRef}
        required={required}
      />
      <input
        type="text"
        className="date-field-offset"
        value={offset}
        size={6}
        maxLength={6}
        title="UTC offset, like +03:00 or Z"
        aria-label={`${label} (UTC offset)`}
        aria-invalid={!isValidOffset(offset)}
        onChange={(e) => {
          setOffset(e.target.value);
          setOffsetKept(true);
          emit(wall, e.target.value);
        }}
      />
    </span>
  );
}
