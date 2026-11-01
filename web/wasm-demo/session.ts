/** The document being edited in the page: its text, and the undo/redo history
 * over it. The page has no disk, so this is the source of truth until the user
 * downloads the result.
 *
 * The history is `meshfox_model::undo` itself, compiled into the wasm module —
 * the log `meshfox view` keeps in SQLite, here over memory — so what undo
 * means (cursor, redo tail, `seq` numbering, depth cap, replay) is the same on
 * both. This class only adds what a page needs around it: the current text, a
 * revision for `If-Match`, "unsaved", and change callbacks. */
export interface HistoryEntry {
  seq: number;
  createdAt: string;
  opKind: string;
  applied: boolean;
  summary: string;
}

/** The part of the wasm module this needs. */
export interface UndoWasm {
  UndoLog: new (initial: string) => {
    push(opKind: string, before: string, after: string): void;
    cursor(): number;
    canUndo(): boolean;
    canRedo(): boolean;
    undo(current: string): string | undefined;
    redo(current: string): string | undefined;
    jumpTo(seq: number, current: string): string | undefined;
    history(limit: number): string;
  };
  bodyRev(text: string): string;
}

/** What the history panel says about a step, by what kind of step it was. */
const SUMMARIES: Record<string, string> = {
  source: "edited the source",
  node_upserted: "edited a node",
  external_edit: "changed outside the editor",
};

export class EditSession {
  private readonly log: InstanceType<UndoWasm["UndoLog"]>;
  private current: string;
  private savedRev: string;
  private listeners: Array<() => void> = [];

  constructor(
    private readonly wasm: UndoWasm,
    initial: string,
  ) {
    this.log = new wasm.UndoLog(initial);
    this.current = initial;
    this.savedRev = wasm.bodyRev(initial);
  }

  get raw(): string {
    return this.current;
  }
  /** Identifies the text as it is now (an `ETag`): its fingerprint, as the
   * server computes it. */
  get rev(): string {
    return `"${this.wasm.bodyRev(this.current)}"`;
  }
  get canUndo(): boolean {
    return this.log.canUndo();
  }
  get canRedo(): boolean {
    return this.log.canRedo();
  }
  get seq(): number {
    return this.log.cursor();
  }
  /** Whether what is shown differs from what was last downloaded (or opened). */
  get dirty(): boolean {
    return this.wasm.bodyRev(this.current) !== this.savedRev;
  }

  /** Whether `header` (an `If-Match` value) names the current text. */
  matches(header: string | null): boolean {
    return header !== null && header.replace(/^W\//, "") === this.rev;
  }

  /** Records `raw` as the next state; the redo tail is dropped. No-op (false)
   * when it is the text already shown. */
  edit(raw: string, opKind: string): boolean {
    if (raw === this.current) return false;
    this.log.push(opKind, this.current, raw);
    this.current = raw;
    this.changed();
    return true;
  }

  undo(): boolean {
    return this.move(this.log.undo(this.current));
  }
  redo(): boolean {
    return this.move(this.log.redo(this.current));
  }
  goto(seq: number): boolean {
    return this.move(this.log.jumpTo(seq, this.current));
  }

  /** The state shown is the one that was saved (downloaded). */
  markSaved(): void {
    this.savedRev = this.wasm.bodyRev(this.current);
    this.changed();
  }

  /** The last `limit` applied steps plus the whole redo tail, most-future first
   * (what `GET /api/history` lists). */
  history(limit: number): HistoryEntry[] {
    const entries = JSON.parse(this.log.history(limit)) as Omit<HistoryEntry, "summary">[];
    return entries.map((e) => ({ ...e, summary: SUMMARIES[e.opKind] ?? e.opKind }));
  }

  onChange(listener: () => void): void {
    this.listeners.push(listener);
  }

  private move(next: string | undefined): boolean {
    if (next === undefined) return false;
    this.current = next;
    this.changed();
    return true;
  }

  private changed(): void {
    for (const l of this.listeners) l();
  }
}
