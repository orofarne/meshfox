// Pure logic behind a `display="table"` file node (see SPEC.md "Table
// previews" and `TablePreview.tsx`): the wire types, the filter mini-language,
// sort cycling and the virtual-scroll arithmetic. No React, no DOM — so it
// can be unit-tested with `node --test` (scripts/table-view.test.mjs).

export type ColumnKind = "number" | "text" | "temporal" | "bool" | "other";

export interface TableColumn {
  index: number;
  name: string;
  /** DuckDB's own type name (`VARCHAR`, `DECIMAL(18,3)`, ...). */
  type: string;
  kind: ColumnKind;
}

export interface TableFailure {
  /** `duckdb-missing`, `cache`, `import` or `limit`. */
  kind: string;
  message: string;
}

export interface TableMeta {
  state: "importing" | "ready" | "failed";
  error: TableFailure | null;
  columns: TableColumn[];
  /** `null` until the import has finished. */
  totalRows: number | null;
  fileSize: number;
  mtimeMs: number;
  /** Changes whenever the target file does. */
  version: string;
  /** `true` while the columns come from the quick read of the file itself,
   * before the import: sorting/filtering aren't available yet. */
  preview: boolean;
}

export interface TableRowsPage {
  state: "importing" | "ready" | "failed";
  version: string;
  offset: number;
  /** `null` is SQL NULL; everything else is DuckDB's text rendering. */
  rows: (string | null)[][];
  /** `null` while importing. */
  matchedRows: number | null;
  totalRows: number | null;
}

export type FilterOp =
  | "eq"
  | "ne"
  | "lt"
  | "le"
  | "gt"
  | "ge"
  | "contains"
  | "startswith"
  | "endswith"
  | "isnull"
  | "notnull";

export interface SortKey {
  column: number;
  desc: boolean;
}

export interface Filter {
  column: number;
  op: FilterOp;
  value?: string;
}

export interface ViewSpec {
  sort: SortKey[];
  filters: Filter[];
  search?: string;
}

export const EMPTY_VIEW: ViewSpec = { sort: [], filters: [] };

export function isEmptyView(view: ViewSpec): boolean {
  return view.sort.length === 0 && view.filters.length === 0 && !view.search;
}

/** Stable identity of a view, for cache keys. */
export function viewKey(view: ViewSpec): string {
  return JSON.stringify({
    sort: view.sort,
    filters: view.filters,
    search: view.search || undefined,
  });
}

// ---------------------------------------------------------------------------
// Filter mini-language: what a user types into a column's filter box.
//
//   text        → "contains" for text-like columns, "equals" for numbers/bools
//   >10 >=10 <10 <=10 =x !=x   comparisons (numbers, dates, text)
//   ^ab         starts with     $ab  ends with     ~ab  contains
//   null        is NULL         !null  is not NULL
// ---------------------------------------------------------------------------

const PREFIXES: [string, FilterOp][] = [
  [">=", "ge"],
  ["<=", "le"],
  ["!=", "ne"],
  ["<>", "ne"],
  [">", "gt"],
  ["<", "lt"],
  ["=", "eq"],
  ["^", "startswith"],
  ["$", "endswith"],
  ["~", "contains"],
];

export function parseFilterExpression(
  text: string,
  kind: ColumnKind,
): { op: FilterOp; value?: string } | null {
  const t = text.trim();
  if (!t) return null;
  const lower = t.toLowerCase();
  if (lower === "null") return { op: "isnull" };
  if (lower === "!null") return { op: "notnull" };
  for (const [prefix, op] of PREFIXES) {
    if (t.startsWith(prefix)) {
      const value = t.slice(prefix.length).trim();
      return value ? { op, value } : null;
    }
  }
  // Numbers and booleans compare exactly; dates match by text (so "2024-01"
  // finds a month), as do strings.
  return { op: kind === "number" || kind === "bool" ? "eq" : "contains", value: t };
}

/** The server-side filters for a set of per-column filter-box texts. */
export function buildFilters(
  texts: Record<number, string>,
  columns: TableColumn[],
): Filter[] {
  const out: Filter[] = [];
  for (const col of columns) {
    const parsed = parseFilterExpression(texts[col.index] ?? "", col.kind);
    if (parsed) out.push({ column: col.index, ...parsed });
  }
  return out;
}

// ---------------------------------------------------------------------------
// Sorting
// ---------------------------------------------------------------------------

/** Click on a column header: none → ascending → descending → none. A plain
 * click makes that column the only sort key; with `additive` (shift-click)
 * it's added to, updated in, or removed from the existing keys. */
export function toggleSort(sort: SortKey[], column: number, additive: boolean): SortKey[] {
  const existing = sort.find((k) => k.column === column);
  const next: SortKey | null = !existing
    ? { column, desc: false }
    : !existing.desc
      ? { column, desc: true }
      : null;
  if (!additive) return next ? [next] : [];
  const rest = sort.filter((k) => k.column !== column);
  if (!next) return rest;
  return existing ? sort.map((k) => (k.column === column ? next : k)) : [...rest, next];
}

// ---------------------------------------------------------------------------
// Virtual scrolling
//
// Browsers cap an element's height (~33M px in Chrome, ~17M in Firefox), so a
// 100M-row table can't simply be `rows * ROW_H` tall. Past MAX_TRACK_PX the
// scroll track is shortened and scroll positions are scaled up to row
// positions: dragging the scrollbar still reaches every region, and the
// keyboard (see TablePreview) moves by exact rows.
// ---------------------------------------------------------------------------

export const ROW_H = 26;
export const MAX_TRACK_PX = 8_000_000;
export const BLOCK_ROWS = 100;

export function trackHeight(rows: number): number {
  return Math.min(rows * ROW_H, MAX_TRACK_PX);
}

/** Data pixels per scroll pixel: 1 until the track is shortened. */
export function scrollRatio(rows: number, viewportPx: number): number {
  const total = rows * ROW_H;
  const track = trackHeight(rows);
  if (total <= viewportPx || track <= viewportPx) return 1;
  return (total - viewportPx) / (track - viewportPx);
}

/** The first row to draw for a scroll position, and how many pixels of it
 * are scrolled off the top. */
export function firstRowAt(
  scrollTop: number,
  rows: number,
  viewportPx: number,
): { row: number; offsetPx: number } {
  if (rows <= 0) return { row: 0, offsetPx: 0 };
  const data = Math.max(0, scrollTop) * scrollRatio(rows, viewportPx);
  const row = Math.min(Math.floor(data / ROW_H), rows - 1);
  return { row, offsetPx: Math.max(0, data - row * ROW_H) };
}

/** Inverse of `firstRowAt`: the scroll position that puts `row` at the top. */
export function scrollTopForRow(row: number, rows: number, viewportPx: number): number {
  return (Math.max(0, row) * ROW_H) / scrollRatio(rows, viewportPx);
}

/** Rows that fit (a partial one included) in `viewportPx`. */
export function visibleRowCount(viewportPx: number): number {
  return Math.ceil(viewportPx / ROW_H) + 1;
}

/** Block numbers covering the visible rows plus `margin` blocks each side. */
export function blocksFor(
  firstRow: number,
  count: number,
  rows: number,
  margin = 1,
): number[] {
  if (rows <= 0) return [];
  const last = Math.min(rows - 1, firstRow + count);
  const from = Math.max(0, Math.floor(firstRow / BLOCK_ROWS) - margin);
  const to = Math.min(Math.floor((rows - 1) / BLOCK_ROWS), Math.floor(last / BLOCK_ROWS) + margin);
  const out: number[] = [];
  for (let b = from; b <= to; b++) out.push(b);
  return out;
}

export function defaultColumnWidth(col: TableColumn): number {
  const base = Math.max(col.name.length, 4) * 8 + 52;
  return col.kind === "number" || col.kind === "bool"
    ? Math.min(Math.max(base, 90), 160)
    : Math.min(Math.max(base, 110), 260);
}

export function formatCount(n: number): string {
  return n.toLocaleString("en-US");
}

export function formatBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  const units = ["KiB", "MiB", "GiB", "TiB"];
  let v = n / 1024;
  let i = 0;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i++;
  }
  return `${v >= 10 ? Math.round(v) : v.toFixed(1)} ${units[i]}`;
}
