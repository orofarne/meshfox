//! Restart-surviving copy of the in-memory session bookkeeping
//! `AppState::session_runs` (which blocks already ran this session and are
//! still fresh) and `AppState::session_vars` (values a `form` fence
//! submitted) — two more tables on the same per-canvas connection
//! [`crate::session_db`] hands `undo_log`/`run_ledger`. The in-memory maps
//! stay the source of truth while the core runs: every change is written
//! through here, and [`SessionStore::load_runs`]/[`SessionStore::load_vars`]
//! only seed them once at startup.
//!
//! Never stored: the value of a `secret` variable, whether a form field's
//! or a `from=` block's output. (A secret's value still reaches a run's
//! `fingerprint`, but only as part of a hash, so after a restart a block
//! stays fresh exactly as long as the same value gets supplied again.)

use rusqlite::{params, Connection};
use std::collections::HashMap;
use std::io;
use std::sync::{Arc, Mutex};

use crate::session_db::sqlite_err;

const SCHEMA_SQL: &str = "
    CREATE TABLE IF NOT EXISTS session_runs (
        node_id       TEXT NOT NULL,
        block         TEXT NOT NULL,
        fingerprint   TEXT NOT NULL,
        produced_vars TEXT NOT NULL,
        output        TEXT NOT NULL,
        duration_ms   INTEGER NOT NULL,
        PRIMARY KEY (node_id, block)
    );
    CREATE TABLE IF NOT EXISTS session_vars (
        name  TEXT PRIMARY KEY,
        value TEXT NOT NULL
    );
";

/// One `session_runs` row — the plain-data twin of `crate::SessionRun`.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredRun {
    pub node_id: String,
    pub block: String,
    pub fingerprint: String,
    pub produced_vars: HashMap<String, String>,
    pub output: String,
    pub duration_ms: u64,
}

#[derive(Clone)]
pub struct SessionStore {
    conn: Arc<Mutex<Connection>>,
    /// Byte budget for one run's stored `output`, same setting as
    /// `RunLedger`'s (`[session] max_output_bytes`); `0` stores an empty
    /// output (freshness and variables are still kept).
    max_output_bytes: usize,
}

impl SessionStore {
    pub fn from_connection(
        conn: Arc<Mutex<Connection>>,
        max_output_bytes: usize,
    ) -> io::Result<Self> {
        conn.lock()
            .unwrap()
            .execute_batch(SCHEMA_SQL)
            .map_err(sqlite_err)?;
        Ok(SessionStore {
            conn,
            max_output_bytes,
        })
    }

    #[cfg(test)]
    fn open_in_memory(max_output_bytes: usize) -> io::Result<Self> {
        Self::from_connection(
            Arc::new(Mutex::new(
                Connection::open_in_memory().map_err(sqlite_err)?,
            )),
            max_output_bytes,
        )
    }

    /// Upserts one block's fresh-run record. `produced_vars` must already
    /// have secrets filtered out by the caller (it knows the declarations).
    /// `output` is cut to its newest `max_output_bytes` bytes.
    pub fn save_run(&self, run: &StoredRun) -> io::Result<()> {
        let output = tail_at_char_boundary(&run.output, self.max_output_bytes);
        let vars = serde_json::to_string(&run.produced_vars).map_err(io::Error::other)?;
        self.conn
            .lock()
            .unwrap()
            .execute(
                "INSERT INTO session_runs (node_id, block, fingerprint, produced_vars, output, duration_ms) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6) \
                 ON CONFLICT (node_id, block) DO UPDATE SET fingerprint = excluded.fingerprint, \
                 produced_vars = excluded.produced_vars, output = excluded.output, \
                 duration_ms = excluded.duration_ms",
                params![run.node_id, run.block, run.fingerprint, vars, output, run.duration_ms as i64],
            )
            .map_err(sqlite_err)?;
        Ok(())
    }

    pub fn load_runs(&self) -> io::Result<Vec<StoredRun>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare("SELECT node_id, block, fingerprint, produced_vars, output, duration_ms FROM session_runs")
            .map_err(sqlite_err)?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, i64>(5)?,
                ))
            })
            .map_err(sqlite_err)?;
        let mut runs = Vec::new();
        for row in rows {
            let (node_id, block, fingerprint, vars, output, duration_ms) =
                row.map_err(sqlite_err)?;
            // A row whose JSON no longer parses is dropped rather than
            // failing startup — worst case that block just re-runs.
            let Ok(produced_vars) = serde_json::from_str(&vars) else {
                continue;
            };
            runs.push(StoredRun {
                node_id,
                block,
                fingerprint,
                produced_vars,
                output,
                duration_ms: duration_ms as u64,
            });
        }
        Ok(runs)
    }

    /// Upserts one non-secret session variable (a `form` field's value).
    pub fn save_var(&self, name: &str, value: &str) -> io::Result<()> {
        self.conn
            .lock()
            .unwrap()
            .execute(
                "INSERT INTO session_vars (name, value) VALUES (?1, ?2) \
                 ON CONFLICT (name) DO UPDATE SET value = excluded.value",
                params![name, value],
            )
            .map_err(sqlite_err)?;
        Ok(())
    }

    pub fn load_vars(&self) -> io::Result<HashMap<String, String>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare("SELECT name, value FROM session_vars")
            .map_err(sqlite_err)?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
            .map_err(sqlite_err)?;
        rows.collect::<Result<HashMap<_, _>, _>>()
            .map_err(sqlite_err)
    }

    /// Forgets everything stored — `reset_session`'s persistent half.
    pub fn clear(&self) -> io::Result<()> {
        self.conn
            .lock()
            .unwrap()
            .execute_batch("DELETE FROM session_runs; DELETE FROM session_vars;")
            .map_err(sqlite_err)
    }
}

/// The last `max` bytes of `s`, moved forward to the next char boundary.
fn tail_at_char_boundary(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut start = s.len() - max;
    while !s.is_char_boundary(start) {
        start += 1;
    }
    &s[start..]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(output: &str) -> StoredRun {
        StoredRun {
            node_id: "n".into(),
            block: "b".into(),
            fingerprint: "abc".into(),
            produced_vars: HashMap::from([("K".to_string(), "v".to_string())]),
            output: output.into(),
            duration_ms: 42,
        }
    }

    #[test]
    fn runs_round_trip_and_upsert() {
        let store = SessionStore::open_in_memory(1024).unwrap();
        store.save_run(&run("first")).unwrap();
        let mut second = run("second");
        second.fingerprint = "def".into();
        store.save_run(&second).unwrap();
        assert_eq!(store.load_runs().unwrap(), vec![second]);
    }

    #[test]
    fn stored_output_keeps_the_tail_on_a_char_boundary() {
        let store = SessionStore::open_in_memory(5).unwrap();
        // "ééé" is 6 bytes; the last 5 start mid-char, so 4 remain ("éé").
        store.save_run(&run("ééé")).unwrap();
        assert_eq!(store.load_runs().unwrap()[0].output, "éé");
        let store = SessionStore::open_in_memory(0).unwrap();
        store.save_run(&run("anything")).unwrap();
        assert_eq!(store.load_runs().unwrap()[0].output, "");
    }

    #[test]
    fn vars_round_trip_and_clear_wipes_both_tables() {
        let store = SessionStore::open_in_memory(1024).unwrap();
        store.save_var("A", "1").unwrap();
        store.save_var("A", "2").unwrap();
        store.save_run(&run("x")).unwrap();
        assert_eq!(
            store.load_vars().unwrap(),
            HashMap::from([("A".to_string(), "2".to_string())])
        );
        store.clear().unwrap();
        assert!(store.load_vars().unwrap().is_empty());
        assert!(store.load_runs().unwrap().is_empty());
    }
}
