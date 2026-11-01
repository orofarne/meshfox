/** The document being edited in the page: its text, and the snapshots that
 * undo/redo walk. The page has no disk, so this is the source of truth until
 * the user downloads the result.
 *
 * Revisions are opaque ids, one per snapshot (what the Source editor sends
 * back as `If-Match`). History "seq" numbers follow the server's undo log:
 * the initial text is seq 0, each edit is the next seq, and the cursor is
 * the seq of the state shown. */
export interface HistoryEntry {
  seq: number;
  createdAt: string;
  opKind: string;
  applied: boolean;
  summary: string;
}

interface Snapshot {
  id: number;
  raw: string;
  opKind: string;
  summary: string;
  createdAt: string;
}

export class EditSession {
  private readonly snapshots: Snapshot[];
  private cursor = 0;
  private nextId = 1;
  private savedId = 0;
  private listeners: Array<() => void> = [];

  constructor(initial: string) {
    this.snapshots = [{ id: 0, raw: initial, opKind: "open", summary: "opened", createdAt: new Date().toISOString() }];
  }

  get raw(): string {
    return this.snapshots[this.cursor].raw;
  }
  /** Identifies the text as it is now; changes with every edit/undo/redo. */
  get rev(): string {
    return `"mfx-${this.snapshots[this.cursor].id}"`;
  }
  get canUndo(): boolean {
    return this.cursor > 0;
  }
  get canRedo(): boolean {
    return this.cursor < this.snapshots.length - 1;
  }
  get seq(): number {
    return this.cursor;
  }
  /** Whether what is shown differs from what was last downloaded (or opened). */
  get dirty(): boolean {
    return this.snapshots[this.cursor].id !== this.savedId;
  }

  /** Whether `header` (an `If-Match` value) names the current text. */
  matches(header: string | null): boolean {
    return header !== null && header.replace(/^W\//, "") === this.rev;
  }

  /** Records `raw` as the next state; the redo tail is dropped. No-op (false)
   * when it is the text already shown. */
  edit(raw: string, opKind: string, summary: string): boolean {
    if (raw === this.raw) return false;
    this.snapshots.length = this.cursor + 1;
    this.snapshots.push({ id: this.nextId++, raw, opKind, summary, createdAt: new Date().toISOString() });
    this.cursor++;
    this.changed();
    return true;
  }

  undo(): boolean {
    return this.goto(this.cursor - 1);
  }
  redo(): boolean {
    return this.goto(this.cursor + 1);
  }
  goto(seq: number): boolean {
    if (!Number.isInteger(seq) || seq < 0 || seq >= this.snapshots.length || seq === this.cursor) return false;
    this.cursor = seq;
    this.changed();
    return true;
  }

  /** The state shown is the one that was saved (downloaded). */
  markSaved(): void {
    this.savedId = this.snapshots[this.cursor].id;
    this.changed();
  }

  /** The last `limit` applied steps plus the whole redo tail, most-future first
   * (what `GET /api/history` lists). */
  history(limit: number): HistoryEntry[] {
    const firstApplied = Math.max(1, this.cursor - limit + 1);
    const out: HistoryEntry[] = [];
    for (let seq = this.snapshots.length - 1; seq >= 1; seq--) {
      if (seq <= this.cursor && seq < firstApplied) break;
      const s = this.snapshots[seq];
      out.push({ seq, createdAt: s.createdAt, opKind: s.opKind, applied: seq <= this.cursor, summary: s.summary });
    }
    return out;
  }

  onChange(listener: () => void): void {
    this.listeners.push(listener);
  }

  private changed(): void {
    for (const l of this.listeners) l();
  }
}
