/** Unsaved work, kept in the browser (IndexedDB) so that closing the tab, a
 * crash or a reload does not lose it. One draft per document (`key`: the picked
 * folder + canvas path, or `new:<name>`). Only the edited canvas text is kept:
 * it is the only file the editor changes.
 *
 * Every call is best-effort: with no IndexedDB (a private window, blocked site
 * data) nothing is kept and nothing throws — the page then simply behaves as it
 * did before drafts existed. */
export interface Draft {
  key: string;
  path: string;
  raw: string;
  /** `Date.now()` of the last change. */
  savedAt: number;
}

const DB_NAME = "meshfox-editor";
const STORE = "drafts";

function open(): Promise<IDBDatabase | null> {
  return new Promise((resolve) => {
    try {
      const request = indexedDB.open(DB_NAME, 1);
      request.onupgradeneeded = () => request.result.createObjectStore(STORE, { keyPath: "key" });
      request.onsuccess = () => resolve(request.result);
      request.onerror = () => resolve(null);
      request.onblocked = () => resolve(null);
    } catch {
      resolve(null);
    }
  });
}

async function run<T>(mode: IDBTransactionMode, work: (store: IDBObjectStore) => IDBRequest<T>): Promise<T | undefined> {
  const db = await open();
  if (!db) return undefined;
  try {
    return await new Promise<T | undefined>((resolve) => {
      const transaction = db.transaction(STORE, mode);
      const request = work(transaction.objectStore(STORE));
      transaction.oncomplete = () => resolve(request.result);
      transaction.onerror = () => resolve(undefined);
      transaction.onabort = () => resolve(undefined);
    });
  } catch {
    return undefined;
  } finally {
    db.close();
  }
}

export async function putDraft(draft: Draft): Promise<void> {
  await run("readwrite", (store) => store.put(draft));
}

export async function getDraft(key: string): Promise<Draft | undefined> {
  return run<Draft | undefined>("readonly", (store) => store.get(key));
}

export async function deleteDraft(key: string): Promise<void> {
  await run("readwrite", (store) => store.delete(key));
}

/** Newest first. */
export async function listDrafts(): Promise<Draft[]> {
  const all = (await run<Draft[]>("readonly", (store) => store.getAll())) ?? [];
  return all.sort((a, b) => b.savedAt - a.savedAt);
}

/** "just now", "5 min ago", "3 h ago", "2 d ago". */
export function ago(timestamp: number, now = Date.now()): string {
  const seconds = Math.max(0, Math.round((now - timestamp) / 1000));
  if (seconds < 45) return "just now";
  const minutes = Math.round(seconds / 60);
  if (minutes < 60) return `${minutes} min ago`;
  const hours = Math.round(minutes / 60);
  if (hours < 48) return `${hours} h ago`;
  return `${Math.round(hours / 24)} d ago`;
}
