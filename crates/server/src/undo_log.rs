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
//! Recording + rotation only, for now — see TODO.canvas.md's "Undo для
//! правок канваса" for the fuller design and what's still to come
//! (`/api/undo`/`/api/redo`, actually applying a reversal). [`UndoLog::push`]
//! already truncates the redo tail (`DELETE ... WHERE seq > cursor`) since
//! that's the right place for it, but it's inert until something can move
//! `cursor` backward.
//!
//! Two ways a row's own before/after state is stored, chosen by the caller
//! per op kind (see `crate::record_undo`): a small structured [`Payload::Diff`]
//! for a single-node/single-parent op that's cheap to describe precisely, or
//! the whole document's before/after text ([`Payload::Raw`]) for anything
//! document-wide, rare, or otherwise not worth a bespoke shape.

use rusqlite::{params, Connection, OptionalExtension};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

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

fn sqlite_err(e: rusqlite::Error) -> io::Error {
    io::Error::other(e)
}

/// Where a canvas's session database lives — same canonicalize-with-
/// fallback and `.meshfox/<file name>.<suffix>` shape as
/// `meshfox_core::worker_lock::lock_path`, just a different suffix.
fn session_db_path(canvas_path: &Path) -> PathBuf {
    let canvas_path = canvas_path
        .canonicalize()
        .unwrap_or_else(|_| canvas_path.to_path_buf());
    let dir = match canvas_path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        _ => PathBuf::from("."),
    };
    let file_name = canvas_path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    dir.join(".meshfox")
        .join(format!("{file_name}.session.sqlite3"))
}

/// One recorded step of history, as read back by [`UndoLog::history`].
/// `history` itself is only ever called from this module's own tests and
/// `lib.rs`'s `undo_log_recording_tests` today (nothing outside `#[cfg(test)]`
/// reads history back yet — that's the later `/api/undo` slice) — hence the
/// blanket `#[allow(dead_code)]` below rather than one that'll need removing
/// the moment a real caller lands.
#[allow(dead_code)]
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
}

pub struct UndoLog {
    conn: Mutex<Connection>,
}

impl UndoLog {
    /// Opens (creating if needed) `canvas_path`'s own session database and
    /// runs its schema — idempotent (`CREATE TABLE IF NOT EXISTS`), safe to
    /// call every time a worker starts. Does *not* seed `undo_meta`'s
    /// singleton row — see [`Self::reconcile_startup_drift`], which needs to
    /// tell "never seeded" apart from "seeded and unchanged" itself.
    pub fn open(canvas_path: &Path) -> io::Result<Self> {
        let path = session_db_path(canvas_path);
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let conn = Connection::open(path).map_err(sqlite_err)?;
        conn.execute_batch(SCHEMA_SQL).map_err(sqlite_err)?;
        Ok(UndoLog { conn: Mutex::new(conn) })
    }

    #[cfg(test)]
    fn open_in_memory() -> io::Result<Self> {
        let conn = Connection::open_in_memory().map_err(sqlite_err)?;
        conn.execute_batch(SCHEMA_SQL).map_err(sqlite_err)?;
        Ok(UndoLog { conn: Mutex::new(conn) })
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
                Payload::Raw { before } => (None, Some(before.to_string()), Some(full_after.to_string())),
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
            .query_row("SELECT cursor FROM undo_meta WHERE id = 1", [], |r| r.get(0))
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
            conn.query_row("SELECT last_raw FROM undo_meta WHERE id = 1", [], |r| r.get(0))
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
                self.push("external_edit", Payload::Raw { before: &last_raw }, current_raw)?;
                Ok(true)
            }
        }
    }

    /// Most recent steps first, up to `limit` — plain read-back, used by
    /// this module's own tests today and by the later `/api/undo` slice.
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn push_assigns_increasing_seq() {
        let log = UndoLog::open_in_memory().unwrap();
        log.push("node_upserted", Payload::Diff(json!({"a": 1})), "doc-v1").unwrap();
        log.push("node_upserted", Payload::Diff(json!({"a": 2})), "doc-v2").unwrap();

        let history = log.history(10).unwrap();
        assert_eq!(history.len(), 2);
        // Most recent first.
        assert_eq!(history[0].seq, 2);
        assert_eq!(history[1].seq, 1);
    }

    #[test]
    fn diff_payload_rows_have_null_raw_columns_and_vice_versa() {
        let log = UndoLog::open_in_memory().unwrap();
        log.push("node_upserted", Payload::Diff(json!({"a": 1})), "doc-v1").unwrap();
        log.push("raw_replace", Payload::Raw { before: "old" }, "new").unwrap();

        let history = log.history(10).unwrap();
        let diff_row = history.iter().find(|e| e.op_kind == "node_upserted").unwrap();
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
            log.push("node_upserted", Payload::Diff(json!({"i": i})), "doc").unwrap();
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
}
