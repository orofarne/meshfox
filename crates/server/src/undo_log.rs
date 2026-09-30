//! Per-canvas undo/redo history — a small SQLite session file
//! (`.meshfox/<canvas file name>.session.sqlite3`, colocated the same way
//! `crates/core/src/worker_lock.rs`'s own `.worker.lock` /
//! `crates/core/src/varcache.rs`'s own `.env` already are) recording every
//! edit a canvas goes through, plus external edits — another process, or a
//! worker-less CLI invocation, touching the file directly (see
//! `worker_client.rs`'s own doc comment for when CLI/MCP falls back to
//! direct-file editing) — noticed either live (`crate::spawn_file_watcher`)
//! or at startup ([`UndoLog::reconcile_startup_drift`], called once from
//! `crate::build_state`).
//!
//! Recording + rotation, plus the cursor-moving half `/api/undo`/`/api/redo`
//! need ([`UndoLog::peek_undo`]/[`UndoLog::peek_redo`]/[`UndoLog::commit_undo`]/
//! [`UndoLog::commit_redo`]) — see TODO.canvas.md's "Undo для правок
//! канваса" for the fuller design. This module deliberately never depends
//! on `meshfox_core::mdcanvas`: reconstructing the actual reverted/
//! reapplied document text from a stored diff is `lib.rs`'s own job
//! (`apply_history_entry`), since only it already has that dependency and
//! the mutating handlers' own setters to reuse for it. [`UndoLog::push`]
//! truncates the redo tail (`DELETE ... WHERE seq > cursor`) on every
//! fresh write, so a real edit after some undoing always drops whatever
//! redo history that undoing had left behind, same as any other editor.
//!
//! Three ways a row's own before/after state is stored, chosen by the caller
//! per op kind (see `crate::record_undo`): a small structured [`Payload::Diff`]
//! for a single-node/single-parent op that's cheap to describe precisely and
//! replay directly; the whole document's before/after text ([`Payload::Raw`])
//! for anything document-wide, rare, or otherwise not worth a bespoke replay
//! shape; or both at once ([`Payload::RawWithDiff`]) for a document-wide op
//! that's still cheap to *describe* specifically even though it isn't cheap
//! to *replay* precisely.

use rusqlite::{params, Connection, OptionalExtension};
use std::io;
use std::sync::{Arc, Mutex};

use crate::session_db::sqlite_err;

/// Kept well under any real editing session's own history without ever
/// growing unbounded — TODO.canvas.md's "оценка масштаба" sized this at
/// "~200 шагов".
const MAX_DEPTH: i64 = 200;

const SCHEMA_SQL: &str = "
    CREATE TABLE IF NOT EXISTS undo_log (
        seq         INTEGER PRIMARY KEY AUTOINCREMENT,
        created_at  TEXT NOT NULL,
        op_kind     TEXT NOT NULL,
        diff_json   TEXT,
        raw_before  TEXT,
        raw_after   TEXT
    );
    CREATE TABLE IF NOT EXISTS undo_meta (
        id        INTEGER PRIMARY KEY CHECK (id = 1),
        cursor    INTEGER NOT NULL,
        last_raw  TEXT NOT NULL
    );
";

/// One recorded step of history, as read back by [`UndoLog::history`]/
/// [`UndoLog::peek_undo`]/[`UndoLog::peek_redo`].
#[derive(Debug, Clone, PartialEq)]
pub struct UndoEntry {
    pub seq: i64,
    pub created_at: String,
    pub op_kind: String,
    pub diff_json: Option<String>,
    pub raw_before: Option<String>,
    pub raw_after: Option<String>,
}

/// What a [`UndoLog::push`] call actually records for one step — see this
/// module's own doc comment for which op kinds use which.
pub enum Payload<'a> {
    /// A small structured before/after description — `push`'s own
    /// `full_after` (the whole document's new text) still becomes this
    /// row's `raw_after` counterpart is *not* stored; only `undo_meta.
    /// last_raw` is updated to it. Use this when the diff is cheap and
    /// precise to describe without the whole document.
    Diff(serde_json::Value),
    /// The whole document's text before this op — `raw_after` is always
    /// `push`'s own `full_after` argument, so callers only need to supply
    /// `before` here.
    Raw { before: &'a str },
    /// Same replay data as `Raw` (`raw_before`/`raw_after` — `lib.rs`'s own
    /// `apply_history_entry` always uses these two columns to reconstruct
    /// this kind of step, dispatching by `op_kind` rather than by whether
    /// `diff_json` happens to be set), plus a small `diff` stored
    /// alongside purely for `describe_history_entry`'s own display text
    /// (e.g. a rename's old/new id) — never consulted for replay itself.
    /// Use this when the whole document is still the only reliable way to
    /// reconstruct the step, but a specific summary is cheap to describe
    /// anyway.
    RawWithDiff {
        before: &'a str,
        diff: serde_json::Value,
    },
}

pub struct UndoLog {
    conn: Arc<Mutex<Connection>>,
}

impl UndoLog {
    /// Runs this module's schema against an already-open connection —
    /// [`crate::session_db`] hands out one shared `Arc<Mutex<Connection>>`
    /// per canvas so this module and [`crate::run_ledger`] don't each open
    /// their own independent connection to the same sqlite file (see that
    /// module's own doc comment for why that'd invite `SQLITE_BUSY`
    /// contention). Runs this module's own schema, unconditionally —
    /// harmless if `run_ledger` already ran its own on the same connection.
    pub fn from_connection(conn: Arc<Mutex<Connection>>) -> io::Result<Self> {
        conn.lock()
            .unwrap()
            .execute_batch(SCHEMA_SQL)
            .map_err(sqlite_err)?;
        Ok(UndoLog { conn })
    }

    #[cfg(test)]
    fn open_in_memory() -> io::Result<Self> {
        let conn = Connection::open_in_memory().map_err(sqlite_err)?;
        conn.execute_batch(SCHEMA_SQL).map_err(sqlite_err)?;
        Ok(UndoLog {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// Records one history step: truncates any redo tail past the current
    /// cursor (see this module's own doc comment — a no-op today), inserts
    /// the row, advances `undo_meta`'s cursor/`last_raw` to `full_after`
    /// (always the whole document's new text, regardless of `payload`'s own
    /// shape), then caps total depth to [`MAX_DEPTH`].
    pub fn push(&self, op_kind: &str, payload: Payload, full_after: &str) -> io::Result<()> {
        let (diff_json, raw_before, raw_after): (Option<String>, Option<String>, Option<String>) =
            match payload {
                Payload::Diff(v) => (Some(v.to_string()), None, None),
                Payload::Raw { before } => {
                    (None, Some(before.to_string()), Some(full_after.to_string()))
                }
                Payload::RawWithDiff { before, diff } => (
                    Some(diff.to_string()),
                    Some(before.to_string()),
                    Some(full_after.to_string()),
                ),
            };
        let created_at = meshfox_core::timestamp::now_utc_rfc3339();

        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction().map_err(sqlite_err)?;
        tx.execute(
            "INSERT OR IGNORE INTO undo_meta (id, cursor, last_raw) VALUES (1, 0, '')",
            [],
        )
        .map_err(sqlite_err)?;
        let cursor: i64 = tx
            .query_row("SELECT cursor FROM undo_meta WHERE id = 1", [], |r| {
                r.get(0)
            })
            .map_err(sqlite_err)?;
        tx.execute("DELETE FROM undo_log WHERE seq > ?1", params![cursor])
            .map_err(sqlite_err)?;
        tx.execute(
            "INSERT INTO undo_log (created_at, op_kind, diff_json, raw_before, raw_after) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![created_at, op_kind, diff_json, raw_before, raw_after],
        )
        .map_err(sqlite_err)?;
        let new_seq = tx.last_insert_rowid();
        tx.execute(
            "UPDATE undo_meta SET cursor = ?1, last_raw = ?2 WHERE id = 1",
            params![new_seq, full_after],
        )
        .map_err(sqlite_err)?;
        tx.execute(
            "DELETE FROM undo_log WHERE seq NOT IN \
             (SELECT seq FROM undo_log ORDER BY seq DESC LIMIT ?1)",
            params![MAX_DEPTH],
        )
        .map_err(sqlite_err)?;
        tx.commit().map_err(sqlite_err)
    }

    /// Compares `current_raw` (freshly read off disk, at worker startup)
    /// against whatever this canvas's session file last recorded as its
    /// own known content. Three outcomes: never recorded anything before
    /// (a brand-new session file) — seed it with `current_raw`, nothing to
    /// diff against yet, returns `false`; recorded and unchanged — `false`;
    /// recorded and different — something wrote this file without going
    /// through this same `UndoLog` (another process's own worker, a text
    /// editor, or a worker-less CLI invocation) since the last time
    /// anything did — records one `"external_edit"` step and returns
    /// `true`.
    pub fn reconcile_startup_drift(&self, current_raw: &str) -> io::Result<bool> {
        let existing_last_raw: Option<String> = {
            let conn = self.conn.lock().unwrap();
            conn.query_row("SELECT last_raw FROM undo_meta WHERE id = 1", [], |r| {
                r.get(0)
            })
            .optional()
            .map_err(sqlite_err)?
        };
        match existing_last_raw {
            None => {
                let conn = self.conn.lock().unwrap();
                conn.execute(
                    "INSERT OR IGNORE INTO undo_meta (id, cursor, last_raw) VALUES (1, 0, ?1)",
                    params![current_raw],
                )
                .map_err(sqlite_err)?;
                Ok(false)
            }
            Some(last_raw) if last_raw == current_raw => Ok(false),
            Some(last_raw) => {
                self.push(
                    "external_edit",
                    Payload::Raw { before: &last_raw },
                    current_raw,
                )?;
                Ok(true)
            }
        }
    }

    /// The current cursor — how many steps into `undo_log` this canvas has
    /// committed, from the very start. `0` means "nothing to undo". `pub`
    /// so `lib.rs`'s own `jump_to` can compute an undo/redo's adjacent
    /// target (`cursor - 1`/`cursor + 1`) before delegating to it.
    pub fn cursor(&self) -> io::Result<i64> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT COALESCE((SELECT cursor FROM undo_meta WHERE id = 1), 0)",
            [],
            |r| r.get(0),
        )
        .map_err(sqlite_err)
    }

    /// `true` if there's a step `/api/undo` could act on right now.
    pub fn can_undo(&self) -> io::Result<bool> {
        Ok(self.cursor()? > 0)
    }

    /// `true` if there's a step `/api/redo` could act on right now — a row
    /// past the current cursor still sitting in `undo_log`, not yet
    /// overwritten by a fresh edit (which truncates it, see [`Self::push`]).
    pub fn can_redo(&self) -> io::Result<bool> {
        let cursor = self.cursor()?;
        let conn = self.conn.lock().unwrap();
        let max_seq: i64 = conn
            .query_row("SELECT COALESCE(MAX(seq), 0) FROM undo_log", [], |r| {
                r.get(0)
            })
            .map_err(sqlite_err)?;
        Ok(max_seq > cursor)
    }

    fn row_at(&self, seq: i64) -> io::Result<Option<UndoEntry>> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT seq, created_at, op_kind, diff_json, raw_before, raw_after \
             FROM undo_log WHERE seq = ?1",
            params![seq],
            |r| {
                Ok(UndoEntry {
                    seq: r.get(0)?,
                    created_at: r.get(1)?,
                    op_kind: r.get(2)?,
                    diff_json: r.get(3)?,
                    raw_before: r.get(4)?,
                    raw_after: r.get(5)?,
                })
            },
        )
        .optional()
        .map_err(sqlite_err)
    }

    /// The entry a call to `/api/undo` would act on right now — the row at
    /// the current cursor — without moving anything. `None` if there's
    /// nothing left to undo. This module deliberately doesn't depend on
    /// `meshfox_core::mdcanvas` (see its own doc comment), so the caller
    /// (`crate::api_undo`) computes the actual reverted document text
    /// itself, then reports back via [`Self::commit_undo`] once that write
    /// has actually landed on disk.
    pub fn peek_undo(&self) -> io::Result<Option<UndoEntry>> {
        let cursor = self.cursor()?;
        if cursor == 0 {
            return Ok(None);
        }
        self.row_at(cursor)
    }

    /// The entry a call to `/api/redo` would act on right now — the
    /// smallest surviving `seq` strictly above the current cursor. `None`
    /// if there's nothing left to redo. Same "caller computes, then
    /// reports back" contract as [`Self::peek_undo`], via
    /// [`Self::commit_redo`].
    ///
    /// Deliberately not `row_at(cursor + 1)` — the same gap hazard
    /// [`Self::commit_undo`]'s own doc comment explains: a redo-tail
    /// truncation (or `MAX_DEPTH` eviction) can delete the row that used
    /// to sit at `cursor + 1` while a later row further up survives, so a
    /// fixed `+1` offset can miss a real redoable entry entirely — `can_redo`
    /// (`MAX(seq) > cursor`, gap-proof already) would say `true` while this
    /// silently returned `None`, the mirror image of the bug
    /// [`Self::commit_undo`] fixes on the way down.
    pub fn peek_redo(&self) -> io::Result<Option<UndoEntry>> {
        let cursor = self.cursor()?;
        let next_seq: Option<i64> = {
            let conn = self.conn.lock().unwrap();
            conn.query_row(
                "SELECT MIN(seq) FROM undo_log WHERE seq > ?1",
                params![cursor],
                |r| r.get(0),
            )
            .map_err(sqlite_err)?
        };
        match next_seq {
            Some(seq) => self.row_at(seq),
            None => Ok(None),
        }
    }

    /// Moves the cursor back past `entry_seq` (the row [`Self::peek_undo`]
    /// just returned) — call only after `new_raw` has already been written
    /// to disk. The row itself is left in place (a later `/api/redo` can
    /// still reach it) — only `undo_meta`'s own pointer moves. `last_raw`
    /// is updated to `new_raw` too, so a later
    /// [`Self::reconcile_startup_drift`] sees this write as expected,
    /// never as external drift.
    ///
    /// The new cursor is the largest surviving `seq` strictly below
    /// `entry_seq` — deliberately **not** `entry_seq - 1` arithmetically:
    /// `seq` is a plain SQLite `AUTOINCREMENT` column, which never reuses a
    /// value once assigned, even after the row holding it is deleted (by
    /// [`Self::push`]'s own redo-tail truncation, or by its `MAX_DEPTH`
    /// eviction) — so `entry_seq - 1` can land on a gap with no row at all.
    /// [`Self::can_undo`] (`cursor > 0`) would then keep reporting `true`
    /// forever off that phantom position, while [`Self::peek_undo`] (which
    /// actually looks the row up) correctly finds nothing and silently
    /// no-ops — an undo button stuck permanently "enabled" but inert.
    /// Concretely: edit once, undo it, edit again, undo that — the second
    /// edit's own row's *arithmetic* predecessor is the first edit's row,
    /// but that row was just deleted by the second edit's own redo-tail
    /// truncation (it landed on top of an undone step, discarding it the
    /// same way any other editor discards a stale redo branch). Looking up
    /// the real surviving predecessor here, rather than assuming one
    /// exists at a fixed offset, is what keeps `can_undo`/`peek_undo`
    /// agreeing after that (see this module's own regression test).
    /// The cursor value undoing `entry_seq` would produce — the largest
    /// surviving `seq` strictly below it, or `0` if none survive — without
    /// actually moving anything. Exposed (not just inlined into
    /// [`Self::commit_undo`]) so a caller computing a *target* to walk
    /// toward (`crate::api_undo`, before it ever calls `crate::jump_to`)
    /// can know the real destination up front, instead of guessing
    /// `cursor - 1` and hoping it happens to be reachable. Skipping this
    /// and using that naive guess as `jump_to`'s own loop-termination
    /// target caused a genuine infinite loop the moment a gap existed
    /// (found live: `api_undo`/`api_redo`'s original `cursor ∓ 1` math,
    /// oscillating forever between undo and redo once a gap made that
    /// exact seq unreachable from either direction — see `jump_to`'s own
    /// doc comment).
    pub fn cursor_after_undoing(&self, entry_seq: i64) -> io::Result<i64> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT COALESCE(MAX(seq), 0) FROM undo_log WHERE seq < ?1",
            params![entry_seq],
            |r| r.get(0),
        )
        .map_err(sqlite_err)
    }

    pub fn commit_undo(&self, entry_seq: i64, new_raw: &str) -> io::Result<()> {
        let new_cursor = self.cursor_after_undoing(entry_seq)?;
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE undo_meta SET cursor = ?1, last_raw = ?2 WHERE id = 1",
            params![new_cursor, new_raw],
        )
        .map_err(sqlite_err)?;
        Ok(())
    }

    /// Moves the cursor forward onto `entry_seq` (the row
    /// [`Self::peek_redo`] just returned) — same "already written to disk,
    /// just move the pointer" contract as [`Self::commit_undo`].
    pub fn commit_redo(&self, entry_seq: i64, new_raw: &str) -> io::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE undo_meta SET cursor = ?1, last_raw = ?2 WHERE id = 1",
            params![entry_seq, new_raw],
        )
        .map_err(sqlite_err)?;
        Ok(())
    }

    /// Most recent steps first, up to `limit` — plain read-back, used by
    /// this module's own tests and `lib.rs`'s `undo_log_recording_tests`
    /// today (nothing outside `#[cfg(test)]` reads history back this way —
    /// `api_undo`/`api_redo` only ever need the one entry `peek_undo`/
    /// `peek_redo` already give them, never a whole listing).
    #[allow(dead_code)]
    pub fn history(&self, limit: usize) -> io::Result<Vec<UndoEntry>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT seq, created_at, op_kind, diff_json, raw_before, raw_after \
                 FROM undo_log ORDER BY seq DESC LIMIT ?1",
            )
            .map_err(sqlite_err)?;
        let rows = stmt
            .query_map(params![limit as i64], |r| {
                Ok(UndoEntry {
                    seq: r.get(0)?,
                    created_at: r.get(1)?,
                    op_kind: r.get(2)?,
                    diff_json: r.get(3)?,
                    raw_before: r.get(4)?,
                    raw_after: r.get(5)?,
                })
            })
            .map_err(sqlite_err)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(sqlite_err)
    }

    fn row_mapper(r: &rusqlite::Row) -> rusqlite::Result<UndoEntry> {
        Ok(UndoEntry {
            seq: r.get(0)?,
            created_at: r.get(1)?,
            op_kind: r.get(2)?,
            diff_json: r.get(3)?,
            raw_before: r.get(4)?,
            raw_after: r.get(5)?,
        })
    }

    /// Every currently-applied step, most recent first, capped to the last
    /// `limit`, plus the *entire* current redo tail prepended ahead of
    /// them (never separately capped — it's already bounded by how many
    /// steps have been undone, and dropped outright the moment a fresh
    /// edit lands, see [`Self::push`]) — the whole list ordered by `seq`
    /// descending throughout (redo tail's highest seq first, straight
    /// through into the applied steps), so a caller (`crate::api_history`)
    /// doesn't have to merge two separately-ordered slices itself. Each
    /// entry already knows which side of the cursor it's on — see
    /// [`HistoryEntry::applied`] — so the caller doesn't have to
    /// cross-reference `seq` against a separately-fetched cursor either.
    pub fn history_around(&self, limit: usize) -> io::Result<Vec<HistoryEntry>> {
        let cursor = self.cursor()?;
        let conn = self.conn.lock().unwrap();
        let redo_tail: Vec<UndoEntry> = {
            let mut stmt = conn
                .prepare(
                    "SELECT seq, created_at, op_kind, diff_json, raw_before, raw_after \
                     FROM undo_log WHERE seq > ?1 ORDER BY seq DESC",
                )
                .map_err(sqlite_err)?;
            let rows = stmt
                .query_map(params![cursor], Self::row_mapper)
                .map_err(sqlite_err)?;
            rows.collect::<Result<Vec<_>, _>>().map_err(sqlite_err)?
        };
        let applied: Vec<UndoEntry> = {
            let mut stmt = conn
                .prepare(
                    "SELECT seq, created_at, op_kind, diff_json, raw_before, raw_after \
                     FROM undo_log WHERE seq <= ?1 ORDER BY seq DESC LIMIT ?2",
                )
                .map_err(sqlite_err)?;
            let rows = stmt
                .query_map(params![cursor, limit as i64], Self::row_mapper)
                .map_err(sqlite_err)?;
            rows.collect::<Result<Vec<_>, _>>().map_err(sqlite_err)?
        };
        Ok(redo_tail
            .into_iter()
            .map(|entry| HistoryEntry {
                entry,
                applied: false,
            })
            .chain(applied.into_iter().map(|entry| HistoryEntry {
                entry,
                applied: true,
            }))
            .collect())
    }
}

/// One entry as listed by [`UndoLog::history_around`], resolved against
/// the cursor at the time of that call so the caller doesn't have to
/// fetch it separately just to know which side of "currently applied"
/// this row falls on.
#[derive(Debug, Clone, PartialEq)]
pub struct HistoryEntry {
    pub entry: UndoEntry,
    /// `true` if `entry.seq` was at or below the cursor (an `/api/undo`
    /// would revert it), `false` if it was sitting in the redo tail
    /// (`entry.seq` above the cursor — an `/api/redo`, or a
    /// `/api/history/goto` naming this same `seq`, would reapply it).
    pub applied: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn push_assigns_increasing_seq() {
        let log = UndoLog::open_in_memory().unwrap();
        log.push("node_upserted", Payload::Diff(json!({"a": 1})), "doc-v1")
            .unwrap();
        log.push("node_upserted", Payload::Diff(json!({"a": 2})), "doc-v2")
            .unwrap();

        let history = log.history(10).unwrap();
        assert_eq!(history.len(), 2);
        // Most recent first.
        assert_eq!(history[0].seq, 2);
        assert_eq!(history[1].seq, 1);
    }

    #[test]
    fn diff_payload_rows_have_null_raw_columns_and_vice_versa() {
        let log = UndoLog::open_in_memory().unwrap();
        log.push("node_upserted", Payload::Diff(json!({"a": 1})), "doc-v1")
            .unwrap();
        log.push("raw_replace", Payload::Raw { before: "old" }, "new")
            .unwrap();

        let history = log.history(10).unwrap();
        let diff_row = history
            .iter()
            .find(|e| e.op_kind == "node_upserted")
            .unwrap();
        assert!(diff_row.diff_json.is_some());
        assert!(diff_row.raw_before.is_none());
        assert!(diff_row.raw_after.is_none());

        let raw_row = history.iter().find(|e| e.op_kind == "raw_replace").unwrap();
        assert!(raw_row.diff_json.is_none());
        assert_eq!(raw_row.raw_before.as_deref(), Some("old"));
        assert_eq!(raw_row.raw_after.as_deref(), Some("new"));
    }

    #[test]
    fn depth_cap_keeps_only_the_most_recent_max_depth_rows() {
        let log = UndoLog::open_in_memory().unwrap();
        for i in 0..(MAX_DEPTH + 5) {
            log.push("node_upserted", Payload::Diff(json!({"i": i})), "doc")
                .unwrap();
        }
        let history = log.history(10_000).unwrap();
        assert_eq!(history.len() as i64, MAX_DEPTH);
        // The oldest 5 pushes were evicted — the newest surviving row is
        // the very last one pushed, the oldest surviving is exactly
        // MAX_DEPTH back from it.
        assert_eq!(history.first().unwrap().seq, MAX_DEPTH + 5);
        assert_eq!(history.last().unwrap().seq, 6);
    }

    #[test]
    fn reconcile_startup_drift_seeds_silently_on_a_fresh_log() {
        let log = UndoLog::open_in_memory().unwrap();
        let drifted = log.reconcile_startup_drift("# Doc\n").unwrap();
        assert!(!drifted);
        assert!(log.history(10).unwrap().is_empty());
    }

    #[test]
    fn reconcile_startup_drift_is_a_noop_when_unchanged() {
        let log = UndoLog::open_in_memory().unwrap();
        log.reconcile_startup_drift("# Doc\n").unwrap();
        let drifted = log.reconcile_startup_drift("# Doc\n").unwrap();
        assert!(!drifted);
        assert!(log.history(10).unwrap().is_empty());
    }

    #[test]
    fn reconcile_startup_drift_records_an_external_edit_when_content_moved() {
        let log = UndoLog::open_in_memory().unwrap();
        log.reconcile_startup_drift("# Doc v1\n").unwrap();
        let drifted = log.reconcile_startup_drift("# Doc v2\n").unwrap();
        assert!(drifted);

        let history = log.history(10).unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].op_kind, "external_edit");
        assert_eq!(history[0].raw_before.as_deref(), Some("# Doc v1\n"));
        assert_eq!(history[0].raw_after.as_deref(), Some("# Doc v2\n"));

        // A third call sees the now-updated last_raw, not the original.
        let drifted_again = log.reconcile_startup_drift("# Doc v2\n").unwrap();
        assert!(!drifted_again);
        assert_eq!(log.history(10).unwrap().len(), 1);
    }

    #[test]
    fn a_fresh_log_cannot_undo_or_redo() {
        let log = UndoLog::open_in_memory().unwrap();
        assert!(!log.can_undo().unwrap());
        assert!(!log.can_redo().unwrap());
        assert!(log.peek_undo().unwrap().is_none());
        assert!(log.peek_redo().unwrap().is_none());
    }

    #[test]
    fn peek_undo_returns_the_row_at_the_cursor_without_moving_it() {
        let log = UndoLog::open_in_memory().unwrap();
        log.push("node_upserted", Payload::Diff(json!({"a": 1})), "doc-v1")
            .unwrap();
        log.push("node_upserted", Payload::Diff(json!({"a": 2})), "doc-v2")
            .unwrap();

        assert!(log.can_undo().unwrap());
        assert!(!log.can_redo().unwrap());
        let entry = log.peek_undo().unwrap().unwrap();
        assert_eq!(entry.seq, 2);
        // A second peek sees the exact same row — nothing moved.
        assert_eq!(log.peek_undo().unwrap().unwrap().seq, 2);
    }

    #[test]
    fn commit_undo_moves_the_cursor_back_one_step_and_leaves_the_row_in_place() {
        let log = UndoLog::open_in_memory().unwrap();
        log.push("node_upserted", Payload::Diff(json!({"a": 1})), "doc-v1")
            .unwrap();
        log.push("node_upserted", Payload::Diff(json!({"a": 2})), "doc-v2")
            .unwrap();

        let entry = log.peek_undo().unwrap().unwrap();
        assert_eq!(entry.seq, 2);
        log.commit_undo(entry.seq, "doc-v1").unwrap();

        assert!(log.can_undo().unwrap());
        assert!(log.can_redo().unwrap());
        assert_eq!(log.peek_undo().unwrap().unwrap().seq, 1);
        assert_eq!(log.peek_redo().unwrap().unwrap().seq, 2);
        // The row itself is untouched, not deleted.
        assert_eq!(log.history(10).unwrap().len(), 2);
    }

    #[test]
    fn commit_undo_updates_last_raw_so_a_later_reconcile_sees_no_drift() {
        let log = UndoLog::open_in_memory().unwrap();
        log.reconcile_startup_drift("doc-v0").unwrap();
        log.push("node_upserted", Payload::Diff(json!({"a": 1})), "doc-v1")
            .unwrap();

        let entry = log.peek_undo().unwrap().unwrap();
        log.commit_undo(entry.seq, "doc-v0").unwrap();

        assert!(!log.reconcile_startup_drift("doc-v0").unwrap());
    }

    #[test]
    fn commit_redo_moves_the_cursor_forward_and_updates_last_raw() {
        let log = UndoLog::open_in_memory().unwrap();
        log.push("node_upserted", Payload::Diff(json!({"a": 1})), "doc-v1")
            .unwrap();
        log.push("node_upserted", Payload::Diff(json!({"a": 2})), "doc-v2")
            .unwrap();
        let entry = log.peek_undo().unwrap().unwrap();
        log.commit_undo(entry.seq, "doc-v1").unwrap();

        let redo_entry = log.peek_redo().unwrap().unwrap();
        assert_eq!(redo_entry.seq, 2);
        log.commit_redo(redo_entry.seq, "doc-v2").unwrap();

        assert!(!log.can_redo().unwrap());
        assert!(log.can_undo().unwrap());
        assert!(!log.reconcile_startup_drift("doc-v2").unwrap());
    }

    #[test]
    fn a_fresh_push_after_undoing_drops_the_redo_tail() {
        let log = UndoLog::open_in_memory().unwrap();
        log.push("node_upserted", Payload::Diff(json!({"a": 1})), "doc-v1")
            .unwrap();
        log.push("node_upserted", Payload::Diff(json!({"a": 2})), "doc-v2")
            .unwrap();
        let entry = log.peek_undo().unwrap().unwrap();
        log.commit_undo(entry.seq, "doc-v1").unwrap();
        assert!(log.can_redo().unwrap());

        // A brand-new edit lands on top of the undone cursor instead of
        // being redone — same as any other editor, this discards the redo
        // tail rather than branching history.
        log.push("node_upserted", Payload::Diff(json!({"a": 3})), "doc-v3")
            .unwrap();

        assert!(!log.can_redo().unwrap());
        let history = log.history(10).unwrap();
        assert_eq!(history.len(), 2);
        assert_eq!(
            history[0].diff_json.as_deref(),
            Some(json!({"a": 3}).to_string().as_str())
        );
    }

    // Regression test for a real bug (found while designing the web e2e
    // suite, before this fix): edit once, undo it, edit again (this is
    // exactly what discards the first edit's own row as a stale redo
    // tail — see `push`'s own doc comment), then undo that second edit
    // too. `commit_undo` used to set the new cursor to `entry_seq - 1`
    // arithmetically, landing on the now-deleted first row's own `seq` —
    // a value `AUTOINCREMENT` never reuses — so `can_undo` (`cursor > 0`)
    // kept reporting `true` forever from that phantom position, while
    // `peek_undo` (which actually looks the row up) correctly found
    // nothing and silently no-opped: an undo button stuck permanently
    // "enabled" but inert. See `commit_undo`'s own doc comment for the fix.
    #[test]
    fn undo_after_undo_then_a_fresh_edit_does_not_leave_can_undo_disagreeing_with_peek_undo() {
        let log = UndoLog::open_in_memory().unwrap();
        log.push("node_upserted", Payload::Diff(json!({"a": 1})), "doc-v1")
            .unwrap();
        let first = log.peek_undo().unwrap().unwrap();
        log.commit_undo(first.seq, "doc-v0").unwrap();

        log.push("node_upserted", Payload::Diff(json!({"a": 2})), "doc-v2")
            .unwrap();
        let second = log.peek_undo().unwrap().unwrap();
        log.commit_undo(second.seq, "doc-v0-again").unwrap();

        assert!(!log.can_undo().unwrap());
        assert!(log.peek_undo().unwrap().is_none());
    }

    // Mirror image of the test above, for `peek_redo`: after the same
    // edit/undo/edit/undo sequence, the low-seq row from the *first* edit
    // is gone (truncated by the second edit's own redo-tail cleanup) while
    // the second edit's own row survives, undone, above the cursor.
    // `peek_redo` used to look for it at exactly `cursor + 1` (a gap left
    // behind by that truncation, since `seq` never gets reused) instead of
    // wherever it actually survived — `can_redo` (`MAX(seq) > cursor`,
    // never assumed a fixed offset) correctly said `true` the whole time.
    #[test]
    fn redo_after_undo_then_a_fresh_edit_then_undo_reaches_the_surviving_row_past_the_gap() {
        let log = UndoLog::open_in_memory().unwrap();
        log.push("node_upserted", Payload::Diff(json!({"a": 1})), "doc-v1")
            .unwrap();
        let first = log.peek_undo().unwrap().unwrap();
        log.commit_undo(first.seq, "doc-v0").unwrap();

        log.push("node_upserted", Payload::Diff(json!({"a": 2})), "doc-v2")
            .unwrap();
        let second = log.peek_undo().unwrap().unwrap();
        log.commit_undo(second.seq, "doc-v0-again").unwrap();

        assert!(log.can_redo().unwrap());
        let redoable = log.peek_redo().unwrap();
        assert!(
            redoable.is_some(),
            "can_redo said true but peek_redo found nothing"
        );
        assert_eq!(redoable.unwrap().seq, second.seq);
    }

    #[test]
    fn history_around_marks_applied_vs_redo_tail_on_either_side_of_the_cursor() {
        let log = UndoLog::open_in_memory().unwrap();
        log.push("node_upserted", Payload::Diff(json!({"a": 1})), "doc-v1")
            .unwrap();
        log.push("node_upserted", Payload::Diff(json!({"a": 2})), "doc-v2")
            .unwrap();
        log.push("node_upserted", Payload::Diff(json!({"a": 3})), "doc-v3")
            .unwrap();
        // Undo the most recent one, so seq 3 sits in the redo tail and 1/2
        // stay applied.
        let entry = log.peek_undo().unwrap().unwrap();
        assert_eq!(entry.seq, 3);
        log.commit_undo(entry.seq, "doc-v2").unwrap();

        let listed = log.history_around(10).unwrap();
        let seqs_and_applied: Vec<(i64, bool)> =
            listed.iter().map(|h| (h.entry.seq, h.applied)).collect();
        // Most-recent/most-future first throughout: the redo tail (seq 3),
        // then the applied steps in descending order (seq 2, seq 1).
        assert_eq!(seqs_and_applied, vec![(3, false), (2, true), (1, true)]);
    }

    #[test]
    fn history_around_caps_only_the_applied_side_never_the_redo_tail() {
        let log = UndoLog::open_in_memory().unwrap();
        for i in 0..5 {
            log.push("node_upserted", Payload::Diff(json!({"i": i})), "doc")
                .unwrap();
        }
        // Undo three of the five, so seqs 3/4/5 sit in the redo tail.
        for _ in 0..3 {
            let entry = log.peek_undo().unwrap().unwrap();
            log.commit_undo(entry.seq, "doc").unwrap();
        }
        assert_eq!(log.cursor().unwrap(), 2);

        // A `limit` smaller than the redo tail's own size still returns
        // every redo-tail row — only the applied side is capped.
        let listed = log.history_around(1).unwrap();
        let seqs_and_applied: Vec<(i64, bool)> =
            listed.iter().map(|h| (h.entry.seq, h.applied)).collect();
        assert_eq!(
            seqs_and_applied,
            vec![(5, false), (4, false), (3, false), (2, true)]
        );
    }
}
