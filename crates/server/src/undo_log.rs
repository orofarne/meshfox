//! Per-canvas undo/redo history, stored in a small SQLite session file
//! (`.meshfox/<canvas file name>.session.sqlite3`, colocated the same way
//! `crates/core/src/worker_lock.rs`'s own `.worker.lock` /
//! `crates/core/src/varcache.rs`'s own `.env` already are): every edit a
//! canvas goes through, plus external edits — another process, or a
//! worker-less CLI invocation, touching the file directly (see
//! `worker_client.rs`'s own doc comment for when CLI/MCP falls back to
//! direct-file editing) — noticed either live (`crate::spawn_file_watcher`)
//! or at startup (`UndoLog::reconcile_startup_drift`, called once from
//! `crate::build_state`).
//!
//! What undo *means* — the cursor, the redo tail a fresh edit drops, `seq`
//! gaps, the depth cap, drift, replay — is `meshfox_model::undo`'s, shared
//! with the in-browser editor; this module is only the SQLite
//! [`UndoStorage`] under it ([`SqliteStorage`]) and the schema (a migration
//! for session files written before document-wide steps became a splice).
//! The log never builds a document from a structured diff: reconstructing the
//! reverted/reapplied text from one is `lib.rs`'s own job
//! (`apply_history_entry`), since only it has the node-level setters to reuse.
//! `crate::record_undo` picks, per op kind, how a step is stored (a small
//! structured [`Payload::Diff`], a document-wide [`Payload::Raw`] splice, or
//! [`Payload::RawWithDiff`]).

use meshfox_model::undo::{NewRow, Range, UndoLog as Log, UndoStorage};
use rusqlite::{params, Connection, OptionalExtension};
use std::io;
use std::ops::Deref;
use std::sync::{Arc, Mutex};

pub use meshfox_model::undo::{Payload, UndoEntry};

use crate::session_db::sqlite_err;

const SCHEMA_SQL: &str = "
    CREATE TABLE IF NOT EXISTS undo_log (
        seq         INTEGER PRIMARY KEY AUTOINCREMENT,
        created_at  TEXT NOT NULL,
        op_kind     TEXT NOT NULL,
        diff_json   TEXT,
        raw_before  TEXT,
        raw_after   TEXT,
        splice_json TEXT
    );
    CREATE TABLE IF NOT EXISTS undo_meta (
        id        INTEGER PRIMARY KEY CHECK (id = 1),
        cursor    INTEGER NOT NULL,
        last_raw  TEXT NOT NULL
    );
";

/// The columns a read for replay selects, in the order [`row_mapper`] reads
/// them.
const ENTRY_COLUMNS: &str =
    "seq, created_at, op_kind, diff_json, raw_before, raw_after, splice_json";

/// A listing is only ever described for a person, never replayed: it reads
/// each row's kind and summary and leaves the replay payload (whole documents,
/// spans of text) in the database — a panel of 200 steps must not pull 200
/// copies of the canvas into memory.
const LISTING_COLUMNS: &str = "seq, created_at, op_kind, diff_json, NULL, NULL, NULL";

fn row_mapper(r: &rusqlite::Row) -> rusqlite::Result<UndoEntry> {
    let splice_json: Option<String> = r.get(6)?;
    Ok(UndoEntry {
        seq: r.get(0)?,
        created_at: r.get(1)?,
        op_kind: r.get(2)?,
        diff_json: r.get(3)?,
        raw_before: r.get(4)?,
        raw_after: r.get(5)?,
        splice: splice_json.and_then(|j| serde_json::from_str(&j).ok()),
    })
}

/// The undo log's rows and cursor in the canvas's SQLite session file.
pub struct SqliteStorage {
    conn: Arc<Mutex<Connection>>,
}

impl UndoStorage for SqliteStorage {
    fn cursor(&self) -> io::Result<i64> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT COALESCE((SELECT cursor FROM undo_meta WHERE id = 1), 0)",
            [],
            |r| r.get(0),
        )
        .map_err(sqlite_err)
    }

    fn max_seq(&self) -> io::Result<i64> {
        let conn = self.conn.lock().unwrap();
        conn.query_row("SELECT COALESCE(MAX(seq), 0) FROM undo_log", [], |r| {
            r.get(0)
        })
        .map_err(sqlite_err)
    }

    fn row(&self, seq: i64) -> io::Result<Option<UndoEntry>> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            &format!("SELECT {ENTRY_COLUMNS} FROM undo_log WHERE seq = ?1"),
            params![seq],
            row_mapper,
        )
        .optional()
        .map_err(sqlite_err)
    }

    fn min_seq_above(&self, seq: i64) -> io::Result<Option<i64>> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT MIN(seq) FROM undo_log WHERE seq > ?1",
            params![seq],
            |r| r.get(0),
        )
        .map_err(sqlite_err)
    }

    fn max_seq_below(&self, seq: i64) -> io::Result<i64> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT COALESCE(MAX(seq), 0) FROM undo_log WHERE seq < ?1",
            params![seq],
            |r| r.get(0),
        )
        .map_err(sqlite_err)
    }

    fn last_raw(&self) -> io::Result<Option<String>> {
        let conn = self.conn.lock().unwrap();
        conn.query_row("SELECT last_raw FROM undo_meta WHERE id = 1", [], |r| {
            r.get(0)
        })
        .optional()
        .map_err(sqlite_err)
    }

    fn seed(&self, last_raw: &str) -> io::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT OR IGNORE INTO undo_meta (id, cursor, last_raw) VALUES (1, 0, ?1)",
            params![last_raw],
        )
        .map_err(sqlite_err)?;
        Ok(())
    }

    fn set_position(&self, cursor: i64, last_raw: &str) -> io::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE undo_meta SET cursor = ?1, last_raw = ?2 WHERE id = 1",
            params![cursor, last_raw],
        )
        .map_err(sqlite_err)?;
        Ok(())
    }

    fn append(&self, row: NewRow<'_>, last_raw: &str, max_depth: i64) -> io::Result<()> {
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
            "INSERT INTO undo_log (created_at, op_kind, diff_json, splice_json) \
             VALUES (?1, ?2, ?3, ?4)",
            params![row.created_at, row.op_kind, row.diff_json, row.splice_json],
        )
        .map_err(sqlite_err)?;
        let new_seq = tx.last_insert_rowid();
        tx.execute(
            "UPDATE undo_meta SET cursor = ?1, last_raw = ?2 WHERE id = 1",
            params![new_seq, last_raw],
        )
        .map_err(sqlite_err)?;
        tx.execute(
            "DELETE FROM undo_log WHERE seq NOT IN \
             (SELECT seq FROM undo_log ORDER BY seq DESC LIMIT ?1)",
            params![max_depth],
        )
        .map_err(sqlite_err)?;
        tx.commit().map_err(sqlite_err)
    }

    fn list(
        &self,
        range: Range,
        descending: bool,
        limit: Option<usize>,
        payload: bool,
    ) -> io::Result<Vec<UndoEntry>> {
        let columns = if payload {
            ENTRY_COLUMNS
        } else {
            LISTING_COLUMNS
        };
        let (filter, bound) = match range {
            Range::All => ("1 = 1", 0),
            Range::AtOrBelow(c) => ("seq <= ?1", c),
            Range::Above(c) => ("seq > ?1", c),
        };
        let order = if descending { "DESC" } else { "ASC" };
        // SQLite: a negative LIMIT is "no limit".
        let limit = limit.map_or(-1, |l| l as i64);
        let sql = format!(
            "SELECT {columns} FROM undo_log WHERE {filter} ORDER BY seq {order} LIMIT {limit}"
        );
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(&sql).map_err(sqlite_err)?;
        let rows = if range == Range::All {
            stmt.query_map([], row_mapper)
        } else {
            stmt.query_map(params![bound], row_mapper)
        }
        .map_err(sqlite_err)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(sqlite_err)
    }
}

/// This canvas's undo log: `meshfox_model`'s [`Log`] over a [`SqliteStorage`].
/// Every method of the log is reachable on it (it derefs to it).
pub struct UndoLog(Log<SqliteStorage>);

impl Deref for UndoLog {
    type Target = Log<SqliteStorage>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl UndoLog {
    /// Runs this module's schema against an already-open connection —
    /// [`crate::session_db`] hands out one shared `Arc<Mutex<Connection>>`
    /// per canvas so this module and [`crate::run_ledger`] don't each open
    /// their own independent connection to the same sqlite file (see that
    /// module's own doc comment for why that'd invite `SQLITE_BUSY`
    /// contention). The schema is run unconditionally — harmless if
    /// `run_ledger` already ran its own on the same connection.
    pub fn from_connection(conn: Arc<Mutex<Connection>>) -> io::Result<Self> {
        {
            let guard = conn.lock().unwrap();
            guard.execute_batch(SCHEMA_SQL).map_err(sqlite_err)?;
            Self::migrate(&guard)?;
        }
        Ok(UndoLog(Log::new(SqliteStorage { conn })))
    }

    /// Adds `splice_json` to a session file created before it existed
    /// (`CREATE TABLE IF NOT EXISTS` leaves an existing table alone).
    fn migrate(conn: &Connection) -> io::Result<()> {
        let has_splice: bool = conn
            .prepare("PRAGMA table_info(undo_log)")
            .map_err(sqlite_err)?
            .query_map([], |r| r.get::<_, String>(1))
            .map_err(sqlite_err)?
            .filter_map(Result::ok)
            .any(|name| name == "splice_json");
        if !has_splice {
            conn.execute("ALTER TABLE undo_log ADD COLUMN splice_json TEXT", [])
                .map_err(sqlite_err)?;
        }
        Ok(())
    }

    #[cfg(test)]
    fn open_in_memory() -> io::Result<Self> {
        let conn = Connection::open_in_memory().map_err(sqlite_err)?;
        Self::from_connection(Arc::new(Mutex::new(conn)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_sqlite_log_behaves_as_every_log_must() {
        meshfox_model::undo_conformance::run_all(&|| {
            let UndoLog(log) = UndoLog::open_in_memory().unwrap();
            log
        });
    }

    #[test]
    fn a_row_written_before_splices_existed_still_replays_and_a_session_file_is_migrated() {
        // An old session file: the table as it was, one whole-document row.
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE undo_log (
                seq INTEGER PRIMARY KEY AUTOINCREMENT, created_at TEXT NOT NULL,
                op_kind TEXT NOT NULL, diff_json TEXT, raw_before TEXT, raw_after TEXT);
             CREATE TABLE undo_meta (id INTEGER PRIMARY KEY CHECK (id = 1),
                cursor INTEGER NOT NULL, last_raw TEXT NOT NULL);
             INSERT INTO undo_log (created_at, op_kind, raw_before, raw_after)
                VALUES ('2026-01-01T00:00:00Z', 'raw_replace', 'old text', 'new text');
             INSERT INTO undo_meta (id, cursor, last_raw) VALUES (1, 1, 'new text');",
        )
        .unwrap();
        let log = UndoLog::from_connection(Arc::new(Mutex::new(conn))).unwrap();

        let entry = log.peek_undo().unwrap().unwrap();
        assert!(entry.splice.is_none());
        assert_eq!(
            entry.replay_document("whatever", true).as_deref(),
            Some("old text")
        );
        assert_eq!(
            entry.replay_document("whatever", false).as_deref(),
            Some("new text")
        );

        // New steps go on top in the new format, in the same file.
        log.push(
            "raw_replace",
            Payload::Raw { before: "new text" },
            "newer text",
        )
        .unwrap();
        let history = log.history(10).unwrap();
        assert!(history[0].splice.is_some() && history[0].raw_before.is_none());
        assert!(history[1].splice.is_none() && history[1].raw_before.is_some());
    }

    #[test]
    fn a_history_listing_leaves_the_replay_payload_in_the_database() {
        let log = UndoLog::open_in_memory().unwrap();
        log.push("raw_replace", Payload::Raw { before: "old" }, "new")
            .unwrap();
        let listing = log.history_around(10).unwrap();
        assert!(listing[0].entry.splice.is_none());
        // The real row, read for replay, still carries its span.
        assert!(log.peek_undo().unwrap().unwrap().splice.is_some());
    }
}
