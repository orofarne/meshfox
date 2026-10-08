import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { KeyboardEvent as ReactKeyboardEvent, MouseEvent as ReactMouseEvent, PointerEvent as ReactPointerEvent } from "react";
import { fetchTableMeta, fetchTableRows } from "./api";
import {
  BLOCK_ROWS,
  EMPTY_VIEW,
  ROW_H,
  blocksFor,
  buildFilters,
  defaultColumnWidth,
  firstRowAt,
  formatBytes,
  formatCount,
  isEmptyView,
  scrollTopForRow,
  toggleSort,
  trackHeight,
  viewKey,
  visibleRowCount,
  type Filter,
  type SortKey,
  type TableColumn,
  type TableMeta,
  type ViewSpec,
} from "./tableView";

const NAME_ROW_H = 44;
const FILTER_ROW_H = 30;
const GUTTER_W = 64;
const INPUT_DEBOUNCE_MS = 300;
const FETCH_DEBOUNCE_MS = 60;
const IMPORT_POLL_MS = 700;
const READY_POLL_MS = 4000;
const MAX_CACHED_BLOCKS = 300;
/** However tall the box ends up, never draw/fetch more than this much at once. */
const MAX_VIEWPORT_PX = 2400;
const NO_COLUMNS: TableColumn[] = [];

type Cell = string | null;

function sameMeta(a: TableMeta | null, b: TableMeta): boolean {
  return a !== null && JSON.stringify(a) === JSON.stringify(b);
}

/**
 * A `file` node's `display: "table"` body (see SPEC.md "Table previews"): a
 * read-only, virtualized grid over the target, windowed through the worker's
 * `/table` and `/table/rows` endpoints so a file of any size costs only the
 * rows on screen. Sorting (header click, shift for a second key), per-column
 * filters (see `parseFilterExpression` for the little language), a search
 * across all columns and a detail bar for the selected cell are all
 * server-side and become available when the import finishes; until then the
 * first rows of the file are shown from a direct read.
 *
 * Fills whatever box it's given (the node, or the expanded panel) and keeps
 * its own scrollbars: `nowheel`/`nopan`/`nodrag` keep the canvas from
 * hijacking wheel, drag and text selection inside it.
 */
export function FileTablePreview({ nodeId, target }: { nodeId: string; target?: string }) {
  const [meta, setMeta] = useState<TableMeta | null>(null);
  const [metaError, setMetaError] = useState<string | null>(null);

  // --- view state: what the user asked for ---
  const [sort, setSort] = useState<SortKey[]>([]);
  const [filterText, setFilterText] = useState<Record<number, string>>({});
  const [appliedFilters, setAppliedFilters] = useState<Filter[]>([]);
  const [searchText, setSearchText] = useState("");
  const [appliedSearch, setAppliedSearch] = useState("");
  const [showFilters, setShowFilters] = useState(false);
  const [widths, setWidths] = useState<Record<number, number>>({});
  const [selected, setSelected] = useState<{ row: number; col: number } | null>(null);

  // --- fetched rows: refs for the cache, `tick` to re-render on arrival ---
  const pagesRef = useRef(new Map<string, Cell[][]>());
  const pendingRef = useRef(new Set<string>());
  const matchedRef = useRef<Record<string, number>>({});
  const knownRowsRef = useRef(0);
  const generationRef = useRef(0);
  const [, setTick] = useState(0);
  const bump = useCallback(() => setTick((t) => t + 1), []);
  const [viewError, setViewError] = useState<string | null>(null);

  // --- scrolling ---
  const scrollerRef = useRef<HTMLDivElement | null>(null);
  const [scrollTop, setScrollTop] = useState(0);
  const [boxHeight, setBoxHeight] = useState(0);

  const columns = meta?.columns ?? NO_COLUMNS;
  const ready = meta?.state === "ready";

  // The schema/state: polled quickly while importing, slowly afterwards (to
  // notice the file changing underneath).
  useEffect(() => {
    let cancelled = false;
    let timer: number | undefined;
    const controller = new AbortController();
    const poll = async () => {
      let next = READY_POLL_MS * 2;
      try {
        const m = await fetchTableMeta(nodeId, controller.signal);
        if (cancelled) return;
        setMetaError(null);
        setMeta((prev) => (sameMeta(prev, m) ? prev : m));
        next = m.state === "importing" ? IMPORT_POLL_MS : m.state === "ready" ? READY_POLL_MS : next;
      } catch (err) {
        if (cancelled) return;
        setMetaError(err instanceof Error ? err.message : String(err));
      }
      timer = window.setTimeout(poll, next);
    };
    void poll();
    return () => {
      cancelled = true;
      controller.abort();
      window.clearTimeout(timer);
    };
  }, [nodeId]);

  // Whatever rows were cached belong to one version/state of the table.
  const resetKey = `${meta?.version ?? ""}|${meta?.state ?? ""}`;
  useEffect(() => {
    pagesRef.current.clear();
    pendingRef.current.clear();
    matchedRef.current = {};
    knownRowsRef.current = 0;
    generationRef.current += 1;
    setViewError(null);
    bump();
  }, [resetKey, bump]);

  // The filter boxes and the search box apply after a short pause.
  useEffect(() => {
    const t = window.setTimeout(() => {
      setAppliedFilters(buildFilters(filterText, columns));
      setAppliedSearch(searchText.trim());
    }, INPUT_DEBOUNCE_MS);
    return () => window.clearTimeout(t);
  }, [filterText, searchText, columns]);

  // Sorting, filtering and searching need the imported table.
  const view: ViewSpec = useMemo(
    () => (ready ? { sort, filters: appliedFilters, search: appliedSearch || undefined } : EMPTY_VIEW),
    [ready, sort, appliedFilters, appliedSearch],
  );
  const vk = viewKey(view);
  const plain = isEmptyView(view);

  // A new view starts from the top.
  useEffect(() => {
    pagesRef.current.clear();
    pendingRef.current.clear();
    generationRef.current += 1;
    setViewError(null);
    setSelected(null);
    if (scrollerRef.current) scrollerRef.current.scrollTop = 0;
    setScrollTop(0);
    bump();
  }, [vk, bump]);

  // The scroller's height decides how many rows to draw and fetch.
  const observer = useRef<ResizeObserver | null>(null);
  const attachScroller = useCallback((el: HTMLDivElement | null) => {
    observer.current?.disconnect();
    scrollerRef.current = el;
    if (!el) return;
    setBoxHeight(el.clientHeight);
    observer.current = new ResizeObserver(() => setBoxHeight(el.clientHeight));
    observer.current.observe(el);
  }, []);

  const headH = NAME_ROW_H + (showFilters && ready ? FILTER_ROW_H : 0);
  const viewportPx = Math.min(MAX_VIEWPORT_PX, Math.max(0, boxHeight - headH));

  const matched: number | null =
    plain && meta?.totalRows != null
      ? meta.totalRows
      : (matchedRef.current[vk] ?? (meta?.state === "importing" && knownRowsRef.current > 0 ? knownRowsRef.current : null));
  const rowCount = matched ?? 0;
  const first = firstRowAt(scrollTop, rowCount, viewportPx);
  const visibleRows = visibleRowCount(viewportPx);

  // Fetch the blocks around what's on screen, after the scroll settles.
  const needed = (matched == null ? [0] : blocksFor(first.row, visibleRows, rowCount)).join(",");
  useEffect(() => {
    if (!meta || meta.state === "failed" || viewportPx <= 0) return;
    const wanted = needed ? needed.split(",").map(Number) : [];
    const missing = wanted.filter((b) => {
      const key = `${vk}#${b}`;
      return !pagesRef.current.has(key) && !pendingRef.current.has(key);
    });
    if (missing.length === 0) return;
    const timer = window.setTimeout(() => {
      const generation = generationRef.current;
      for (const block of missing) {
        const key = `${vk}#${block}`;
        if (pagesRef.current.has(key) || pendingRef.current.has(key)) continue;
        pendingRef.current.add(key);
        fetchTableRows(nodeId, block * BLOCK_ROWS, BLOCK_ROWS, view)
          .then((page) => {
            pendingRef.current.delete(key);
            if (generation !== generationRef.current) return;
            if (page.version !== meta.version) return; // the poll will pick up the change
            const cache = pagesRef.current;
            cache.set(key, page.rows);
            while (cache.size > MAX_CACHED_BLOCKS) cache.delete(cache.keys().next().value as string);
            if (page.matchedRows != null) matchedRef.current[vk] = page.matchedRows;
            else knownRowsRef.current = Math.max(knownRowsRef.current, page.offset + page.rows.length);
            bump();
          })
          .catch((err: unknown) => {
            pendingRef.current.delete(key);
            if (generation !== generationRef.current) return;
            setViewError(err instanceof Error ? err.message : String(err));
          });
      }
    }, FETCH_DEBOUNCE_MS);
    return () => window.clearTimeout(timer);
    // `view` is covered by `vk`; `meta` by its version/state.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [needed, vk, meta?.version, meta?.state, viewportPx, nodeId, bump]);

  const getRow = (row: number): Cell[] | undefined => {
    const block = Math.floor(row / BLOCK_ROWS);
    return pagesRef.current.get(`${vk}#${block}`)?.[row - block * BLOCK_ROWS];
  };

  const colWidth = (c: TableColumn) => widths[c.index] ?? defaultColumnWidth(c);
  const totalW = GUTTER_W + columns.reduce((sum, c) => sum + colWidth(c), 0);

  const scrollToRow = (row: number) => {
    const el = scrollerRef.current;
    if (el) el.scrollTop = scrollTopForRow(row, rowCount, viewportPx);
  };

  const select = (row: number, col: number) => {
    const r = Math.min(Math.max(row, 0), Math.max(rowCount - 1, 0));
    const c = Math.min(Math.max(col, 0), Math.max(columns.length - 1, 0));
    setSelected({ row: r, col: c });
    const page = Math.max(visibleRows - 2, 1);
    if (r < first.row) scrollToRow(r);
    else if (r >= first.row + page) scrollToRow(r - page + 1);
  };

  const onKeyDown = (e: ReactKeyboardEvent<HTMLDivElement>) => {
    const sel = selected ?? { row: first.row, col: 0 };
    const page = Math.max(visibleRows - 2, 1);
    let handled = true;
    switch (e.key) {
      case "ArrowDown":
        select(sel.row + 1, sel.col);
        break;
      case "ArrowUp":
        select(sel.row - 1, sel.col);
        break;
      case "ArrowRight":
        select(sel.row, sel.col + 1);
        break;
      case "ArrowLeft":
        select(sel.row, sel.col - 1);
        break;
      case "PageDown":
        select(sel.row + page, sel.col);
        break;
      case "PageUp":
        select(sel.row - page, sel.col);
        break;
      case "Home":
        select(e.ctrlKey || e.metaKey ? 0 : sel.row, e.ctrlKey || e.metaKey ? sel.col : 0);
        break;
      case "End":
        select(e.ctrlKey || e.metaKey ? rowCount - 1 : sel.row, e.ctrlKey || e.metaKey ? sel.col : columns.length - 1);
        break;
      case "c":
      case "C":
        if ((e.ctrlKey || e.metaKey) && selected) {
          const value = getRow(selected.row)?.[selected.col];
          try {
            void navigator.clipboard.writeText(value ?? "");
          } catch {
            /* clipboard unavailable — nothing to do */
          }
        } else {
          handled = false;
        }
        break;
      default:
        handled = false;
    }
    if (handled) {
      e.preventDefault();
      e.stopPropagation(); // not the canvas's own keyboard navigation
    }
  };

  const startResize = (e: ReactPointerEvent<HTMLDivElement>, col: TableColumn) => {
    e.preventDefault();
    e.stopPropagation();
    const startX = e.clientX;
    const startW = colWidth(col);
    const target = e.currentTarget;
    target.setPointerCapture(e.pointerId);
    const move = (ev: PointerEvent) =>
      setWidths((w) => ({ ...w, [col.index]: Math.max(48, Math.round(startW + ev.clientX - startX)) }));
    const up = () => {
      target.removeEventListener("pointermove", move);
      target.removeEventListener("pointerup", up);
    };
    target.addEventListener("pointermove", move);
    target.addEventListener("pointerup", up);
  };

  const reset = () => {
    setSort([]);
    setFilterText({});
    setSearchText("");
  };

  // ---------------------------------------------------------------------
  if (metaError && !meta) {
    return (
      <div className="mesh-node-body nopan">
        {target && <code>{target}</code>}
        <p className="mesh-node-hint">table preview unavailable: {metaError}</p>
      </div>
    );
  }
  if (!meta) return <p className="mesh-node-hint">loading table…</p>;
  if (meta.state === "failed") {
    return (
      <div className="mesh-node-body mesh-table-failed nopan" data-error-kind={meta.error?.kind}>
        {target && <code>{target}</code>}
        <p className="mesh-table-error">{meta.error?.message ?? "table preview failed"}</p>
      </div>
    );
  }

  const importing = meta.state === "importing";
  const viewActive = !plain || Object.values(filterText).some((t) => t.trim()) || searchText.trim() !== "";
  const selectedValue = selected ? getRow(selected.row)?.[selected.col] : undefined;
  const selectedColumn = selected ? columns[selected.col] : undefined;

  const status = importing
    ? `importing… showing the first ${formatCount(knownRowsRef.current || 0)} rows`
    : matched != null && meta.totalRows != null && matched !== meta.totalRows
      ? `${formatCount(matched)} of ${formatCount(meta.totalRows)} rows · ${columns.length} cols`
      : `${formatCount(meta.totalRows ?? 0)} rows × ${columns.length} cols`;

  return (
    <div className="mesh-table nodrag nopan nowheel" data-state={meta.state}>
      <div className="mesh-table-toolbar">
        <span className="mesh-table-status" title={`${formatBytes(meta.fileSize)}`}>
          {status}
        </span>
        <input
          type="search"
          className="mesh-table-search"
          placeholder="search all columns"
          value={searchText}
          disabled={!ready}
          title={ready ? undefined : "available once the import has finished"}
          onChange={(e) => setSearchText(e.target.value)}
          onKeyDown={(e) => e.stopPropagation()}
        />
        <button
          type="button"
          className={showFilters ? "mesh-table-btn is-on" : "mesh-table-btn"}
          disabled={!ready}
          title="Per-column filters: text · >10 >=10 <10 · =x !=x · ^starts $ends ~contains · null !null"
          onClick={() => setShowFilters((v) => !v)}
        >
          filters
        </button>
        {viewActive && (
          <button type="button" className="mesh-table-btn" onClick={reset} title="Clear sorting, filters and search">
            reset
          </button>
        )}
      </div>
      {importing && (
        <div className="mesh-table-banner">
          Importing the file in the background. Sorting, filtering and search unlock when it's done.
        </div>
      )}
      {viewError && <div className="mesh-table-banner is-error">{viewError}</div>}
      <div
        className="mesh-table-scroller"
        ref={attachScroller}
        tabIndex={0}
        onScroll={(e) => setScrollTop(e.currentTarget.scrollTop)}
        onKeyDown={onKeyDown}
      >
        <div className="mesh-table-head" style={{ width: totalW, height: headH }}>
          <div className="mesh-table-head-row" style={{ height: NAME_ROW_H }}>
            <div className="mesh-table-hcell mesh-table-gutter" style={{ width: GUTTER_W }}>
              #
            </div>
            {columns.map((c) => {
              const rank = sort.findIndex((k) => k.column === c.index);
              return (
                <div
                  key={c.index}
                  className={`mesh-table-hcell is-${c.kind}${ready ? " is-sortable" : ""}`}
                  style={{ width: colWidth(c) }}
                  title={`${c.name} · ${c.type}`}
                  onClick={(e: ReactMouseEvent) => ready && setSort((s) => toggleSort(s, c.index, e.shiftKey))}
                >
                  <span className="mesh-table-hname">{c.name}</span>
                  <span className="mesh-table-htype">{c.type.toLowerCase()}</span>
                  {rank >= 0 && (
                    <span className="mesh-table-sort" data-testid="sort-indicator">
                      {sort[rank].desc ? "▼" : "▲"}
                      {sort.length > 1 ? rank + 1 : ""}
                    </span>
                  )}
                  <div className="mesh-table-resizer" onPointerDown={(e) => startResize(e, c)} onClick={(e) => e.stopPropagation()} />
                </div>
              );
            })}
          </div>
          {showFilters && ready && (
            <div className="mesh-table-head-row mesh-table-filter-row" style={{ height: FILTER_ROW_H }}>
              <div className="mesh-table-hcell mesh-table-gutter" style={{ width: GUTTER_W }} />
              {columns.map((c) => (
                <div key={c.index} className="mesh-table-hcell" style={{ width: colWidth(c) }}>
                  <input
                    type="text"
                    className="mesh-table-filter"
                    aria-label={`filter ${c.name}`}
                    value={filterText[c.index] ?? ""}
                    onChange={(e) => setFilterText((t) => ({ ...t, [c.index]: e.target.value }))}
                    onKeyDown={(e) => e.stopPropagation()}
                  />
                </div>
              ))}
            </div>
          )}
        </div>
        <div className="mesh-table-track" style={{ width: totalW, height: trackHeight(rowCount) }}>
          <div
            className="mesh-table-layer"
            style={{ top: headH, height: Math.min(viewportPx, trackHeight(rowCount)) }}
          >
            {Array.from({ length: Math.max(0, Math.min(visibleRows, rowCount - first.row)) }, (_, i) => {
              const r = first.row + i;
              const data = getRow(r);
              return (
                <div
                  key={r}
                  className={`mesh-table-row${r % 2 ? " is-odd" : ""}${data ? "" : " is-loading"}`}
                  style={{ top: i * ROW_H - first.offsetPx, height: ROW_H, width: totalW }}
                >
                  <div className="mesh-table-cell mesh-table-gutter" style={{ width: GUTTER_W }}>
                    {formatCount(r + 1)}
                  </div>
                  {columns.map((c) => {
                    const value = data?.[c.index];
                    const isSel = selected?.row === r && selected.col === c.index;
                    return (
                      <div
                        key={c.index}
                        className={`mesh-table-cell is-${c.kind}${isSel ? " is-selected" : ""}`}
                        style={{ width: colWidth(c) }}
                        onClick={() => {
                          setSelected({ row: r, col: c.index });
                          scrollerRef.current?.focus({ preventScroll: true });
                        }}
                      >
                        {data === undefined ? null : value === null || value === undefined ? (
                          <span className="mesh-table-null">NULL</span>
                        ) : (
                          value
                        )}
                      </div>
                    );
                  })}
                </div>
              );
            })}
          </div>
        </div>
        {matched === 0 && !importing && <div className="mesh-table-empty">no rows{viewActive ? " match" : ""}</div>}
      </div>
      <div className="mesh-table-detail" data-testid="cell-detail">
        {selected && selectedColumn ? (
          <>
            <span className="mesh-table-detail-where">
              row {formatCount(selected.row + 1)} · {selectedColumn.name}
            </span>
            {selectedValue === undefined ? (
              <span className="mesh-table-null">…</span>
            ) : selectedValue === null ? (
              <span className="mesh-table-null">NULL</span>
            ) : (
              <span className="mesh-table-detail-value">{selectedValue}</span>
            )}
          </>
        ) : (
          <span className="mesh-table-hint">click a cell to see its full value · ⌘/Ctrl-C copies it</span>
        )}
      </div>
    </div>
  );
}
