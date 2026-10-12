import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { fetchFsList } from "./api";
import type { FsEntry, FsListing } from "./types";

interface PathPickerProps {
  /** What is being chosen: a directory, or a file. */
  kind: "dir" | "file";
  /** `file` only. `open` picks an existing file; `save` also takes a new
   * name. Defaults to `open`. */
  mode?: "open" | "save";
  /** `file` only: extensions (lowercase, no dot) the list is narrowed to —
   * a hint, switched off with the "only" toggle. */
  ext?: string[];
  /** Where to start: the field's current value, or empty for the canvas
   * directory. Anything `GET /api/fs/list` accepts. */
  initialPath: string;
  title: string;
  onChoose: (path: string) => void;
  onCancel: () => void;
}

function joinPath(dir: string, name: string): string {
  return dir.endsWith("/") ? `${dir}${name}` : `${dir}/${name}`;
}

function formatSize(n: number): string {
  if (n < 1024) return `${n} B`;
  const units = ["KB", "MB", "GB", "TB"];
  let v = n / 1024;
  let i = 0;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i += 1;
  }
  return `${v < 10 ? v.toFixed(1) : Math.round(v)} ${units[i]}`;
}

function extOf(name: string): string {
  const dot = name.lastIndexOf(".");
  return dot > 0 ? name.slice(dot + 1).toLowerCase() : "";
}

/**
 * The file/directory chooser behind a `dir`/`file` variable's "Browse…"
 * button (SPEC.md, "Path pickers"). It browses the *worker's* file system
 * through `GET /api/fs/list` — a browser can't hand over absolute paths —
 * and answers with an absolute path. The TUI has its own modal over the
 * same endpoint.
 */
export function PathPicker({ kind, mode = "open", ext = [], initialPath, title, onChoose, onCancel }: PathPickerProps) {
  const [listing, setListing] = useState<FsListing | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [hidden, setHidden] = useState(false);
  const [onlyExt, setOnlyExt] = useState(true);
  const [filter, setFilter] = useState("");
  const [location, setLocation] = useState(initialPath);
  const [picked, setPicked] = useState<string | null>(null);
  const [saveName, setSaveName] = useState("");
  // Guards against a slow earlier listing overwriting a newer one.
  const seq = useRef(0);

  const go = useCallback(
    (path: string, showHidden: boolean, keepName = false) => {
      const mine = ++seq.current;
      setError(null);
      fetchFsList(path, kind, showHidden)
        .then((l) => {
          if (mine !== seq.current) return;
          setListing(l);
          setLocation(l.path);
          setFilter("");
          setPicked(l.selected ?? null);
          if (!keepName && l.selected) setSaveName(l.selected);
        })
        .catch((e) => {
          if (mine === seq.current) setError(String(e instanceof Error ? e.message : e));
        });
    },
    [kind],
  );

  useEffect(() => {
    // Only the first listing is driven by `initialPath`; later ones by the
    // user's own navigation.
    go(initialPath, false);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.stopPropagation();
        onCancel();
      }
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [onCancel]);

  const shown = useMemo(() => {
    if (!listing) return [] as FsEntry[];
    const f = filter.trim().toLowerCase();
    const narrowByExt = kind === "file" && onlyExt && ext.length > 0;
    return listing.entries.filter((e) => {
      if (f && !e.name.toLowerCase().includes(f)) return false;
      if (narrowByExt && !e.isDir && !ext.includes(extOf(e.name))) return false;
      return true;
    });
  }, [listing, filter, kind, onlyExt, ext]);

  const open = (entry: FsEntry) => {
    if (!listing) return;
    if (entry.isDir) {
      go(joinPath(listing.path, entry.name), hidden, true);
    } else if (kind === "file") {
      setPicked(entry.name);
      if (mode === "save") setSaveName(entry.name);
    }
  };

  const chosenPath = (): string | null => {
    if (!listing) return null;
    if (kind === "dir") return listing.path;
    if (mode === "save") {
      const name = saveName.trim();
      return name && !name.includes("/") ? joinPath(listing.path, name) : null;
    }
    return picked ? joinPath(listing.path, picked) : null;
  };
  const target = chosenPath();
  const overwrites =
    kind === "file" &&
    mode === "save" &&
    !!listing &&
    listing.entries.some((e) => !e.isDir && e.name === saveName.trim());

  return createPortal(
    <div className="vars-modal-backdrop path-picker-backdrop" onClick={onCancel}>
      <div className="vars-modal path-picker" role="dialog" aria-label={title} onClick={(e) => e.stopPropagation()}>
        <h3>{title}</h3>
        <div className="path-picker-bar">
          <button
            type="button"
            title="Parent directory"
            disabled={!listing?.parent}
            onClick={() => listing?.parent && go(listing.parent, hidden)}
          >
            ↑
          </button>
          <button type="button" title="Home directory" disabled={!listing?.home} onClick={() => listing?.home && go(listing.home, hidden)}>
            ~
          </button>
          <button
            type="button"
            title="This canvas's directory"
            disabled={!listing}
            onClick={() => listing && go(listing.canvasDir, hidden)}
          >
            canvas
          </button>
          <input
            className="path-picker-location"
            aria-label="Location"
            value={location}
            onChange={(e) => setLocation(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") {
                e.preventDefault();
                go(location, hidden);
              }
            }}
            spellCheck={false}
          />
        </div>
        <div className="path-picker-bar">
          <input
            className="path-picker-filter"
            aria-label="Filter"
            placeholder="filter…"
            value={filter}
            onChange={(e) => setFilter(e.target.value)}
            autoFocus
          />
          <label className="path-picker-toggle">
            <input
              type="checkbox"
              checked={hidden}
              onChange={(e) => {
                setHidden(e.target.checked);
                if (listing) go(listing.path, e.target.checked, true);
              }}
            />
            hidden
          </label>
          {kind === "file" && ext.length > 0 && (
            <label className="path-picker-toggle" title={`Only show .${ext.join(", .")} files`}>
              <input type="checkbox" checked={onlyExt} onChange={(e) => setOnlyExt(e.target.checked)} />
              .{ext.join(" .")} only
            </label>
          )}
        </div>
        {error && <p className="vars-modal-error">{error}</p>}
        <ul className="path-picker-list" aria-label="Directory contents">
          {listing === null && !error && <li className="path-picker-empty">loading…</li>}
          {listing !== null && shown.length === 0 && <li className="path-picker-empty">(nothing here)</li>}
          {shown.map((entry) => (
            <li
              key={entry.name}
              className={`path-picker-entry${entry.isDir ? " is-dir" : ""}${!entry.isDir && picked === entry.name ? " is-picked" : ""}`}
              onClick={() => open(entry)}
              onDoubleClick={() => {
                if (!entry.isDir && kind === "file" && mode === "open" && listing) {
                  onChoose(joinPath(listing.path, entry.name));
                }
              }}
            >
              <span className="path-picker-icon" aria-hidden>
                {entry.isDir ? "📁" : "📄"}
              </span>
              <span className="path-picker-name">{entry.name}</span>
              {!entry.isDir && entry.size !== undefined && <span className="path-picker-size">{formatSize(entry.size)}</span>}
            </li>
          ))}
        </ul>
        {listing?.truncated && <p className="vars-modal-hint">Only the first entries are shown — use the filter.</p>}
        {kind === "file" && mode === "save" && (
          <label className="vars-modal-field">
            <span>File name</span>
            <input
              aria-label="File name"
              value={saveName}
              onChange={(e) => setSaveName(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter" && target) {
                  e.preventDefault();
                  onChoose(target);
                }
              }}
              spellCheck={false}
            />
          </label>
        )}
        {overwrites && <p className="vars-modal-hint">This file exists and may be overwritten by the block.</p>}
        <div className="vars-modal-actions">
          <button type="button" onClick={onCancel}>
            cancel
          </button>
          <button type="button" className="path-picker-choose" disabled={!target} onClick={() => target && onChoose(target)}>
            {kind === "dir" ? "choose this folder" : "choose"}
          </button>
        </div>
        {target && (
          <p className="vars-modal-hint path-picker-target" title={target}>
            {target}
          </p>
        )}
      </div>
    </div>,
    document.body,
  );
}

interface PathFieldProps {
  kind: "dir" | "file";
  mode?: "open" | "save";
  ext?: string[];
  value: string;
  onChange: (value: string) => void;
  /** Dialog title, usually the field's label. */
  label: string;
  autoFocus?: boolean;
}

/** A path text field plus a "Browse…" button opening [`PathPicker`]. The
 * text stays editable: a person may type `@tmp/out.mp4` or `~/x`, which a
 * picker's absolute answer can't express. */
export function PathField({ kind, mode, ext, value, onChange, label, autoFocus }: PathFieldProps) {
  const [picking, setPicking] = useState(false);
  return (
    <span className="path-field">
      <input
        type="text"
        value={value}
        onChange={(e) => onChange(e.target.value)}
        autoFocus={autoFocus}
        spellCheck={false}
        placeholder={kind === "dir" ? "directory path" : "file path"}
      />
      <button type="button" className="path-field-browse" onClick={() => setPicking(true)}>
        Browse…
      </button>
      {picking && (
        <PathPicker
          kind={kind}
          mode={mode}
          ext={ext}
          initialPath={value}
          title={label}
          onChoose={(p) => {
            setPicking(false);
            onChange(p);
          }}
          onCancel={() => setPicking(false)}
        />
      )}
    </span>
  );
}
