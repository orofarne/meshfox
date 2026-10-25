// Values of `type="date"` / `type="datetime"` variables — the browser side of
// `meshfox_core::datevars` (SPEC.md, "Date and datetime variables"). The
// stored form is always canonical: `YYYY-MM-DD`, and RFC3339 with an offset
// (`2026-10-25T14:30:00+03:00` or `...Z`). Kept strict like the server's
// `validate_value`, which is the authority; this only stops a bad value
// before the round trip.

const DATE_PATTERN = /^(\d{4})-(\d{2})-(\d{2})$/;
const DATETIME_PATTERN =
  /^(\d{4}-\d{2}-\d{2})T(\d{2}):(\d{2}):(\d{2})(?:\.\d{1,9})?(Z|[+-]\d{2}:\d{2})$/;
const OFFSET_PATTERN = /^(?:Z|[+-]([01]\d|2[0-3]):[0-5]\d)$/;

export function isValidDate(value: string): boolean {
  const m = DATE_PATTERN.exec(value);
  if (!m) return false;
  const [y, mo, d] = [Number(m[1]), Number(m[2]), Number(m[3])];
  const probe = new Date(Date.UTC(y, mo - 1, d));
  // A real calendar day: the round trip must not roll over (Feb 30 → Mar 2).
  return probe.getUTCFullYear() === y && probe.getUTCMonth() === mo - 1 && probe.getUTCDate() === d;
}

export function isValidOffset(offset: string): boolean {
  return OFFSET_PATTERN.test(offset);
}

export function isValidDateTime(value: string): boolean {
  const m = DATETIME_PATTERN.exec(value);
  if (!m) return false;
  return (
    isValidDate(m[1]) && Number(m[2]) < 24 && Number(m[3]) < 60 && Number(m[4]) < 60 && isValidOffset(m[5])
  );
}

/** A canonical datetime split into the wall-clock part a
 * `<input type="datetime-local">` holds (`YYYY-MM-DDTHH:MM:SS`) and its
 * offset suffix. `null` when `value` isn't canonical (empty, half-typed). */
export function splitDateTime(value: string): { wall: string; offset: string } | null {
  const m = DATETIME_PATTERN.exec(value);
  if (!m) return null;
  return { wall: `${m[1]}T${m[2]}:${m[3]}:${m[4]}`, offset: m[5] };
}

/** Inverse of `splitDateTime`. A `datetime-local` value may omit seconds. */
export function joinDateTime(wall: string, offset: string): string {
  if (!wall) return "";
  const full = /T\d{2}:\d{2}$/.test(wall) ? `${wall}:00` : wall;
  return `${full}${offset}`;
}

/** The browser's own UTC offset at the given wall-clock time, formatted
 * `±HH:MM` — what a freshly picked time gets when no offset was chosen yet. */
export function browserOffset(wall: string): string {
  const d = wall ? new Date(wall) : new Date();
  const minutes = -(Number.isNaN(d.getTime()) ? new Date() : d).getTimezoneOffset();
  const sign = minutes < 0 ? "-" : "+";
  const abs = Math.abs(minutes);
  return `${sign}${String(Math.floor(abs / 60)).padStart(2, "0")}:${String(abs % 60).padStart(2, "0")}`;
}

/** Whether `value` is acceptable for a variable of `type` — `true` for any
 * type this file doesn't know. */
export function isValidDateValue(type: string, value: string): boolean {
  if (type === "date") return isValidDate(value);
  if (type === "datetime") return isValidDateTime(value);
  return true;
}

export function dateValueProblem(type: string, prompt: string, value: string): string {
  const shown = JSON.stringify(value);
  return type === "date"
    ? `${prompt} needs a date (YYYY-MM-DD), not ${shown}.`
    : `${prompt} needs a date and time with an offset (like 2026-10-25T14:30:00+03:00), not ${shown}.`;
}
