//! Persistent audit log + startup reconciliation for every run/service/tty
//! execution — a `runs` table in the same shared `.meshfox/<canvas>.
//! session.sqlite3` connection [`crate::undo_log`] uses (see
//! [`crate::session_db`]). Two jobs: an audit trail (who ran what, when,
//! how it ended), and — the actual point — surviving a core crash: a row
//! still `outcome = 'running'` at the next core's own startup is exactly
//! the "did this process actually stop" question `crate::service_lock`
//! used to answer with a lock file; see [`RunLedger::reconcile_startup`]
//! and TODO.canvas.md's "Персистентное состояние сессии" for the fuller
//! design.
//!
//! A partial unique index (`runs_one_running_per_address`) gives conflict
//! detection for free: [`RunLedger::start`]'s own `INSERT` just fails with
//! a UNIQUE violation if the address already has a `running` row — no
//! separate detect-then-act race the way a plain file `create_new` would
//! have. No canvas-path column is needed either — unlike the old per-
//! address lock *files* (whose own file name encoded the address), this
//! whole database is already colocated one-per-canvas.

use rusqlite::{params, Connection, ErrorCode, OptionalExtension};
use std::io;
use std::sync::{Arc, Mutex};

use crate::session_db::sqlite_err;
use crate::stream_exec::OutputStream;

const SCHEMA_SQL: &str = "
    CREATE TABLE IF NOT EXISTS runs (
        id          INTEGER PRIMARY KEY AUTOINCREMENT,
        node_id     TEXT NOT NULL,
        block       TEXT NOT NULL,
        kind        TEXT NOT NULL,
        owner       TEXT NOT NULL,
        pid         INTEGER NOT NULL,
        started_at  TEXT NOT NULL,
        ended_at    TEXT,
        outcome     TEXT NOT NULL,
        exit_code   INTEGER,
        stale       INTEGER NOT NULL DEFAULT 0,
        fingerprint TEXT
    );
    CREATE UNIQUE INDEX IF NOT EXISTS runs_one_running_per_address
        ON runs (node_id, block) WHERE outcome = 'running';
    CREATE INDEX IF NOT EXISTS runs_by_address ON runs (node_id, block, id);
    CREATE TABLE IF NOT EXISTS run_lines (
        run_id INTEGER PRIMARY KEY,
        lines  TEXT NOT NULL
    );
";

/// Columns added after `runs` first shipped — `CREATE TABLE IF NOT EXISTS`
/// above leaves an older table as it was. Each fails with "duplicate column
/// name" on an up-to-date one, which is the normal case and ignored. Then
/// `run_output` (a one-row-per-address predecessor of `run_lines`, never
/// released) is dropped.
const MIGRATIONS_SQL: [&str; 3] = [
    "ALTER TABLE runs ADD COLUMN stale INTEGER NOT NULL DEFAULT 0",
    "ALTER TABLE runs ADD COLUMN fingerprint TEXT",
    "DROP TABLE IF EXISTS run_output",
];

/// Which kind of runnable this row tracks — mirrors the same three-way
/// split `run_registry`/`tty_registry`/`services` already keep as three
/// separate in-memory registries; this table is where they converge for
/// persistence, not a replacement for those registries' own live output-
/// streaming job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunKind {
    Plain,
    Tty,
    Service,
}

impl RunKind {
    fn as_str(self) -> &'static str {
        match self {
            RunKind::Plain => "plain",
            RunKind::Tty => "tty",
            RunKind::Service => "service",
        }
    }
}

/// One row as read back by [`RunLedger::active_running`]/
/// [`RunLedger::reconcile_startup`].
#[derive(Debug, Clone, PartialEq)]
pub struct RunRow {
    pub id: i64,
    pub node_id: String,
    pub block: String,
    pub kind: String,
    pub owner: String,
    pub pid: u32,
    pub started_at: String,
}

/// What a caller reports once a tracked run/service/tty is actually done.
#[derive(Debug, Clone, Copy)]
pub enum FinishOutcome {
    Exited(i32),
    Killed,
}

impl FinishOutcome {
    fn outcome_str(self) -> &'static str {
        match self {
            FinishOutcome::Exited(_) => "exited",
            FinishOutcome::Killed => "killed",
        }
    }

    fn exit_code(self) -> Option<i32> {
        match self {
            FinishOutcome::Exited(code) => Some(code),
            FinishOutcome::Killed => None,
        }
    }
}

/// Who already holds `(node_id, block)`'s `running` row — read back on a
/// [`StartError::Conflict`].
#[derive(Debug, Clone, PartialEq)]
pub struct ConflictInfo {
    pub pid: u32,
    pub owner: String,
    pub kind: String,
    pub started_at: String,
}

#[derive(Debug)]
pub enum StartError {
    Conflict(ConflictInfo),
    Io(io::Error),
}

impl std::fmt::Display for StartError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StartError::Conflict(info) => {
                write!(
                    f,
                    "already running elsewhere (pid {}, {})",
                    info.pid, info.owner
                )
            }
            StartError::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for StartError {}

/// Cheap to clone — just another `Arc` handle to the same underlying
/// connection, same as `Arc<AppState>` itself. Cloned into every background
/// task that outlives the request that started it (`run_registry`/
/// `tty_registry`'s own drain tasks, `services::spawn`'s handle) so each
/// can call `finish`/`update_pid` on its own row once the process it
/// tracks actually ends, independent of whether the original HTTP
/// connection that requested it is even still open.
#[derive(Clone)]
pub struct RunLedger {
    conn: Arc<Mutex<Connection>>,
    /// Byte budget for one run's stored output (see [`Self::save_output`]);
    /// `0` disables storing it. Set once at startup from
    /// `[session] max_output_bytes` (`meshfox_core::config`).
    max_output_bytes: usize,
    /// How many finished runs of one address [`Self::finish`] keeps — older
    /// ones are rotated out, row and stored output together. Never below
    /// `1`. From `[session] max_runs_per_block`.
    max_runs_per_block: usize,
    /// Called after a row is started or finished (or every finished run is
    /// marked stale) — set once by `build_state` to broadcast `runs-changed`
    /// on `/api/watch`, so other frontends refetch `GET /api/runs`. A
    /// `OnceLock` shared by every clone, so setting it after the ledger has
    /// already been handed around still reaches all of them.
    notifier: Arc<std::sync::OnceLock<Arc<dyn Fn() + Send + Sync>>>,
}

/// One finished run as [`RunLedger::latest_fresh_run`]/[`RunLedger::history`]
/// read it back. `stale` is already resolved for the caller: the run was
/// marked stale by a session reset, *or* its stored fingerprint no longer
/// matches the current one it was compared against.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunSummary {
    pub id: i64,
    pub outcome: String,
    pub exit_code: Option<i32>,
    pub started_at: String,
    pub ended_at: Option<String>,
    /// Wall-clock length of the run, from `started_at` to `ended_at`.
    pub duration_ms: Option<u64>,
    pub stale: bool,
}

impl RunLedger {
    /// Runs this module's own schema against an already-open connection —
    /// see [`crate::session_db`] for why this takes a shared `Arc` rather
    /// than opening its own. Idempotent, safe to call every worker startup.
    pub fn from_connection(conn: Arc<Mutex<Connection>>) -> io::Result<Self> {
        {
            let conn = conn.lock().unwrap();
            conn.execute_batch(SCHEMA_SQL).map_err(sqlite_err)?;
            for migration in MIGRATIONS_SQL {
                let _ = conn.execute(migration, []);
            }
        }
        Ok(RunLedger {
            conn,
            max_output_bytes: meshfox_core::config::DEFAULT_SESSION_MAX_OUTPUT_BYTES,
            max_runs_per_block: meshfox_core::config::DEFAULT_SESSION_MAX_RUNS_PER_BLOCK,
            notifier: Arc::new(std::sync::OnceLock::new()),
        })
    }

    /// Overrides the per-address byte budget [`Self::save_output`] keeps —
    /// see `meshfox_core::config::session_max_output_bytes`.
    pub fn with_max_output_bytes(mut self, max_output_bytes: usize) -> Self {
        self.max_output_bytes = max_output_bytes;
        self
    }

    /// `pub(crate)` (not just `fn`, unlike most test-only helpers here) —
    /// `services::tests` needs one too, to stand in for the shared ledger a
    /// real caller would pass into `spawn`.
    #[cfg(test)]
    pub(crate) fn open_in_memory() -> io::Result<Self> {
        let conn = Connection::open_in_memory().map_err(sqlite_err)?;
        conn.execute_batch(SCHEMA_SQL).map_err(sqlite_err)?;
        Ok(RunLedger {
            conn: Arc::new(Mutex::new(conn)),
            max_output_bytes: meshfox_core::config::DEFAULT_SESSION_MAX_OUTPUT_BYTES,
            max_runs_per_block: meshfox_core::config::DEFAULT_SESSION_MAX_RUNS_PER_BLOCK,
            notifier: Arc::new(std::sync::OnceLock::new()),
        })
    }

    /// Overrides how many finished runs of one address [`Self::finish`]
    /// keeps — see `meshfox_core::config::session_max_runs_per_block`.
    /// Registers the callback run after a row starts or finishes (only the
    /// first call takes effect).
    pub fn set_notifier(&self, notifier: Arc<dyn Fn() + Send + Sync>) {
        let _ = self.notifier.set(notifier);
    }

    fn notify(&self) {
        if let Some(notifier) = self.notifier.get() {
            notifier();
        }
    }

    pub fn with_max_runs_per_block(mut self, max_runs_per_block: usize) -> Self {
        self.max_runs_per_block = max_runs_per_block.max(1);
        self
    }

    /// Stores `lines` as run `run_id`'s output. Keeps the *tail* that fits
    /// in `max_output_bytes` (summed line text plus one newline each,
    /// matching how the run's output is otherwise measured); older lines
    /// are dropped, not the newest. A no-op when the budget is `0`. Rotated
    /// out together with its run row by [`Self::finish`].
    pub fn save_output(&self, run_id: i64, lines: &[(OutputStream, String)]) -> io::Result<()> {
        if self.max_output_bytes == 0 {
            return Ok(());
        }
        let mut used = 0usize;
        let mut keep_from = lines.len();
        for (i, (_, text)) in lines.iter().enumerate().rev() {
            used += text.len() + 1;
            if used > self.max_output_bytes {
                break;
            }
            keep_from = i;
        }
        let json = serde_json::to_string(&lines[keep_from..]).map_err(io::Error::other)?;
        self.conn
            .lock()
            .unwrap()
            .execute(
                "INSERT INTO run_lines (run_id, lines) VALUES (?1, ?2) \
                 ON CONFLICT (run_id) DO UPDATE SET lines = excluded.lines",
                params![run_id, json],
            )
            .map_err(sqlite_err)?;
        Ok(())
    }

    /// The output [`Self::save_output`] stored for `run_id`, if any.
    pub fn load_output(&self, run_id: i64) -> io::Result<Option<Vec<(OutputStream, String)>>> {
        let json = self
            .conn
            .lock()
            .unwrap()
            .query_row(
                "SELECT lines FROM run_lines WHERE run_id = ?1",
                params![run_id],
                |r| r.get::<_, String>(0),
            )
            .optional()
            .map_err(sqlite_err)?;
        json.map(|j| serde_json::from_str(&j).map_err(io::Error::other))
            .transpose()
    }

    /// Records what [`meshfox_core::closure_fingerprint`] said about the
    /// document when run `id` started — what [`Self::latest_fresh_run`]
    /// later compares against. A run that never gets one (a crash before
    /// this call, a row from an older database) is never fresh.
    pub fn set_fingerprint(&self, id: i64, fingerprint: &str) -> io::Result<()> {
        self.conn
            .lock()
            .unwrap()
            .execute(
                "UPDATE runs SET fingerprint = ?1 WHERE id = ?2",
                params![fingerprint, id],
            )
            .map_err(sqlite_err)?;
        Ok(())
    }

    /// Marks every finished run stale — a session reset says none of what
    /// they did or printed (exit code, timing, output) describes the current
    /// state. The rows stay as run history; [`Self::latest_fresh_run`] just
    /// skips them. A run still `running` is left alone, and a later run of
    /// the same address is stored fresh.
    pub fn mark_finished_runs_stale(&self) -> io::Result<()> {
        self.conn
            .lock()
            .unwrap()
            .execute("UPDATE runs SET stale = 1 WHERE outcome != 'running'", [])
            .map_err(sqlite_err)?;
        self.notify();
        Ok(())
    }

    /// Every plain-block or `tty` address that has at least one finished run
    /// not marked stale, with its kind (`"plain"`/`"tty"`) — the candidates
    /// whose latest *current* run [`Self::latest_fresh_run`] may find (it
    /// also needs the fingerprint). A `service` is left out: it's tracked
    /// live by `crate::services`. A `tty` run has no stored output (only
    /// its outcome and timing), so there's nothing to replay for it.
    pub fn addresses_with_runs(&self) -> io::Result<Vec<(String, String, String)>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT node_id, block, kind FROM runs \
                 WHERE kind IN ('plain', 'tty') AND outcome != 'running' AND stale = 0 \
                 GROUP BY node_id, block ORDER BY MAX(id)",
            )
            .map_err(sqlite_err)?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })
            .map_err(sqlite_err)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(sqlite_err)
    }

    pub fn latest_fresh_run(
        &self,
        node_id: &str,
        block: &str,
        fingerprint: &str,
    ) -> io::Result<Option<RunSummary>> {
        self.conn
            .lock()
            .unwrap()
            .query_row(
                &format!(
                    "SELECT id, outcome, exit_code, started_at, ended_at, {DURATION_SQL} FROM runs \
                     WHERE node_id = ?1 AND block = ?2 AND outcome != 'running' \
                       AND stale = 0 AND fingerprint = ?3 \
                     ORDER BY id DESC LIMIT 1"
                ),
                params![node_id, block, fingerprint],
                |r| summary_row(r, false),
            )
            .optional()
            .map_err(sqlite_err)
    }

    /// One finished run, by id, if it belongs to `(node_id, block)` — for a
    /// caller that asks for a specific historical run. `current` is the
    /// address's fingerprint now (`None` if it couldn't be computed), used
    /// only to fill in [`RunSummary::stale`].
    pub fn get_run(
        &self,
        id: i64,
        node_id: &str,
        block: &str,
        current: Option<&str>,
    ) -> io::Result<Option<RunSummary>> {
        self.conn
            .lock()
            .unwrap()
            .query_row(
                &format!(
                    "SELECT id, outcome, exit_code, started_at, ended_at, stale, fingerprint, {DURATION_SQL} FROM runs \
                     WHERE id = ?1 AND node_id = ?2 AND block = ?3 AND outcome != 'running'"
                ),
                params![id, node_id, block],
                |r| summary_with_staleness(r, current),
            )
            .optional()
            .map_err(sqlite_err)
    }

    /// Every finished run of `(node_id, block)` still kept, newest first.
    pub fn history(
        &self,
        node_id: &str,
        block: &str,
        current: Option<&str>,
    ) -> io::Result<Vec<RunSummary>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare(&format!(
                "SELECT id, outcome, exit_code, started_at, ended_at, stale, fingerprint, {DURATION_SQL} FROM runs \
                 WHERE node_id = ?1 AND block = ?2 AND outcome != 'running' ORDER BY id DESC"
            ))
            .map_err(sqlite_err)?;
        let rows = stmt
            .query_map(params![node_id, block], |r| {
                summary_with_staleness(r, current)
            })
            .map_err(sqlite_err)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(sqlite_err)
    }

    /// Records a new `running` row for `(node_id, block)` — fails with
    /// `StartError::Conflict` if one already exists (the partial unique
    /// index does the actual atomic check; this just turns that constraint
    /// violation into a readback of who's holding it, for the caller's own
    /// error message/kill-and-retry prompt). `pid` is often a placeholder —
    /// queued-time locking (see `crates/server/src/lib.rs`'s
    /// `acquire_chain_locks`) claims the address before the real child
    /// process exists — see [`Self::update_pid`].
    pub fn start(
        &self,
        node_id: &str,
        block: &str,
        kind: RunKind,
        owner: &str,
        pid: u32,
    ) -> Result<i64, StartError> {
        let id = self.start_row(node_id, block, kind, owner, pid)?;
        self.notify();
        Ok(id)
    }

    fn start_row(
        &self,
        node_id: &str,
        block: &str,
        kind: RunKind,
        owner: &str,
        pid: u32,
    ) -> Result<i64, StartError> {
        let started_at = meshfox_core::timestamp::now_utc_rfc3339();
        let conn = self.conn.lock().unwrap();
        let result = conn.execute(
            "INSERT INTO runs (node_id, block, kind, owner, pid, started_at, outcome) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'running')",
            params![node_id, block, kind.as_str(), owner, pid, started_at],
        );
        match result {
            Ok(_) => Ok(conn.last_insert_rowid()),
            Err(rusqlite::Error::SqliteFailure(e, _))
                if e.code == ErrorCode::ConstraintViolation =>
            {
                let info = conn
                    .query_row(
                        "SELECT pid, owner, kind, started_at FROM runs \
                         WHERE node_id = ?1 AND block = ?2 AND outcome = 'running'",
                        params![node_id, block],
                        |r| {
                            Ok(ConflictInfo {
                                pid: r.get(0)?,
                                owner: r.get(1)?,
                                kind: r.get(2)?,
                                started_at: r.get(3)?,
                            })
                        },
                    )
                    .map_err(|e| StartError::Io(sqlite_err(e)))?;
                Err(StartError::Conflict(info))
            }
            Err(e) => Err(StartError::Io(sqlite_err(e))),
        }
    }

    /// Corrects a `start`-placeholder `pid` once the real child's is known
    /// — the equivalent of `service_lock::update_owner_pid`.
    pub fn update_pid(&self, id: i64, pid: u32) -> io::Result<()> {
        self.conn
            .lock()
            .unwrap()
            .execute("UPDATE runs SET pid = ?1 WHERE id = ?2", params![pid, id])
            .map_err(sqlite_err)?;
        Ok(())
    }

    /// Marks row `id` resolved — the equivalent of `service_lock::release`,
    /// just recording how it ended instead of deleting the record.
    pub fn finish(&self, id: i64, outcome: FinishOutcome) -> io::Result<()> {
        self.finish_row(id, outcome)?;
        self.notify();
        Ok(())
    }

    fn finish_row(&self, id: i64, outcome: FinishOutcome) -> io::Result<()> {
        let ended_at = meshfox_core::timestamp::now_utc_rfc3339();
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE runs SET outcome = ?1, exit_code = ?2, ended_at = ?3 WHERE id = ?4",
            params![outcome.outcome_str(), outcome.exit_code(), ended_at, id],
        )
        .map_err(sqlite_err)?;
        // Rotation: everything past this address's newest
        // `max_runs_per_block` finished runs goes, output included. Done
        // here because a run finishing is the only moment the count grows.
        let keep = self.max_runs_per_block as i64;
        let old_ids = "SELECT id FROM runs WHERE outcome != 'running' \
             AND (node_id, block) = (SELECT node_id, block FROM runs WHERE id = ?1) \
             AND id NOT IN (SELECT id FROM runs WHERE outcome != 'running' \
                 AND (node_id, block) = (SELECT node_id, block FROM runs WHERE id = ?1) \
                 ORDER BY id DESC LIMIT ?2)";
        conn.execute(
            &format!("DELETE FROM run_lines WHERE run_id IN ({old_ids})"),
            params![id, keep],
        )
        .map_err(sqlite_err)?;
        conn.execute(
            &format!("DELETE FROM runs WHERE id IN ({old_ids})"),
            params![id, keep],
        )
        .map_err(sqlite_err)?;
        Ok(())
    }

    /// Kills whatever process `(node_id, block)`'s current `running` row
    /// names (its whole process group, plus any daemonized descendant —
    /// see `services::kill_orphaned_descendants`) and marks that row
    /// `killed` — a no-op if the address turns out already free. Doesn't
    /// claim a fresh row itself; see `force_take_over` for "kill, then
    /// immediately claim it myself" — this half exists on its own for a
    /// caller (`force_run_kill_prep`) whose own subsequent, separate
    /// `acquire_chain_locks` pass needs to find the address genuinely free
    /// to claim itself, not already claimed by this call.
    pub fn kill_running(&self, node_id: &str, block: &str) -> io::Result<()> {
        let stale = {
            let conn = self.conn.lock().unwrap();
            conn.query_row(
                "SELECT id, pid FROM runs WHERE node_id = ?1 AND block = ?2 AND outcome = 'running'",
                params![node_id, block],
                |r| Ok((r.get::<_, i64>(0)?, r.get::<_, u32>(1)?)),
            )
            .optional()
            .map_err(sqlite_err)?
        };
        let Some((stale_id, stale_pid)) = stale else {
            return Ok(());
        };
        // Snapshot descendants before *and* after the group kill — same
        // "while it's still alive" reasoning `force_run_kill_prep`'s own
        // doc comment gives for calling this twice (crates/server/src/
        // lib.rs).
        crate::services::kill_orphaned_descendants(stale_pid);
        kill_process_group(stale_pid)?;
        crate::services::kill_orphaned_descendants(stale_pid);
        self.finish(stale_id, FinishOutcome::Killed)
    }

    /// The `kill_and_acquire` equivalent: `kill_running`, then starts a
    /// fresh `running` row for `new_owner`/`new_pid` — a no-op straight to
    /// `start` if the address was already free.
    pub fn force_take_over(
        &self,
        node_id: &str,
        block: &str,
        kind: RunKind,
        new_owner: &str,
        new_pid: u32,
    ) -> io::Result<i64> {
        self.kill_running(node_id, block)?;
        self.start(node_id, block, kind, new_owner, new_pid).map_err(|e| match e {
            StartError::Io(e) => e,
            StartError::Conflict(_) => io::Error::other(
                "address still locked immediately after force-take-over — a fresh claimant slipped in",
            ),
        })
    }

    /// Every row still `running`, for reconciliation.
    pub fn active_running(&self) -> io::Result<Vec<RunRow>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare("SELECT id, node_id, block, kind, owner, pid, started_at FROM runs WHERE outcome = 'running'")
            .map_err(sqlite_err)?;
        let rows = stmt
            .query_map([], |r| {
                Ok(RunRow {
                    id: r.get(0)?,
                    node_id: r.get(1)?,
                    block: r.get(2)?,
                    kind: r.get(3)?,
                    owner: r.get(4)?,
                    pid: r.get(5)?,
                    started_at: r.get(6)?,
                })
            })
            .map_err(sqlite_err)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(sqlite_err)
    }

    /// Called once at core startup, right after `from_connection` — for
    /// every row still `running` (necessarily left there by a core that
    /// never reached its own graceful-shutdown path, see TODO.canvas.md's
    /// "ни один дочерний процесс не переживает ядро"): a dead pid is quiet
    /// housekeeping (`finish(Killed)`, nothing to show — the process
    /// really is gone, matching what the row already implies); a *live*
    /// pid is a real orphan and gets returned to the caller to show, not
    /// silently resolved — same "any conflict is shown, never silently
    /// resolved" principle `service_lock.rs` already established.
    pub fn reconcile_startup(&self) -> io::Result<Vec<RunRow>> {
        let mut orphaned = Vec::new();
        for row in self.active_running()? {
            if meshfox_core::service_lock::is_alive(row.pid) {
                orphaned.push(row);
            } else {
                self.finish(row.id, FinishOutcome::Killed)?;
            }
        }
        Ok(orphaned)
    }
}

/// SQL expression for [`RunSummary::duration_ms`], selected `AS duration_ms`
/// (`julianday` reads the RFC 3339 timestamps `start`/`finish` write).
const DURATION_SQL: &str =
    "CAST(ROUND((julianday(ended_at) - julianday(started_at)) * 86400000.0) AS INTEGER) AS duration_ms";

fn summary_row(r: &rusqlite::Row<'_>, stale: bool) -> rusqlite::Result<RunSummary> {
    Ok(RunSummary {
        id: r.get(0)?,
        outcome: r.get(1)?,
        exit_code: r.get(2)?,
        started_at: r.get(3)?,
        ended_at: r.get(4)?,
        duration_ms: r
            .get::<_, Option<i64>>("duration_ms")?
            .map(|ms| ms.max(0) as u64),
        stale,
    })
}

/// Columns 5 and 6 (`stale`, `fingerprint`) folded into [`RunSummary::stale`]
/// against `current`: stale if flagged, or if there's no way to show the run
/// is still current (no stored fingerprint, or `current` unknown/different).
fn summary_with_staleness(
    r: &rusqlite::Row<'_>,
    current: Option<&str>,
) -> rusqlite::Result<RunSummary> {
    let flagged: i64 = r.get(5)?;
    let stored: Option<String> = r.get(6)?;
    let fresh = flagged == 0 && stored.is_some() && stored.as_deref() == current;
    summary_row(r, !fresh)
}

/// Same whole-process-group `SIGKILL` primitive `services.rs`'s own
/// `kill_process_group` carries — duplicated, not shared, matching that
/// function's own doc comment on why (`stream_exec`/`pty_exec` each already
/// carry their own copy too).
fn kill_process_group(pid: u32) -> io::Result<()> {
    if pid == 0 {
        return Ok(());
    }
    // SAFETY: `-pid` signals the whole process group this pid leads —
    // every spawn function in `stream_exec` makes its child a fresh group
    // leader, so this reaches the same set `kill_and_acquire`/
    // `ServiceHandle::stop` already do.
    let ret = unsafe { libc::kill(-(pid as libc::pid_t), libc::SIGKILL) };
    if ret != 0 && io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH) {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::process::CommandExt;

    /// A real, short-lived process to stand in for "whatever a `running`
    /// row names" — `process_group(0)` so a `kill_process_group` test can
    /// actually verify it died, same reasoning
    /// `crates/cli/tests/service_run_cmd.rs`'s own `spawn_dummy_owner`
    /// documents.
    fn spawn_dummy() -> std::process::Child {
        std::process::Command::new("sleep")
            .arg("60")
            .process_group(0)
            .spawn()
            .expect("spawn dummy process")
    }

    fn out(lines: &[&str]) -> Vec<(OutputStream, String)> {
        lines
            .iter()
            .map(|l| (OutputStream::Stdout, l.to_string()))
            .collect()
    }

    /// What `run_registry::attach` does for a real run of `("a", "b")`:
    /// claim a row, stamp its fingerprint, finish it, store its output.
    fn finished_run(
        ledger: &RunLedger,
        fp: &str,
        outcome: FinishOutcome,
        lines: &[(OutputStream, String)],
    ) -> i64 {
        let id = ledger.start("a", "b", RunKind::Plain, "test", 1).unwrap();
        ledger.set_fingerprint(id, fp).unwrap();
        ledger.finish(id, outcome).unwrap();
        ledger.save_output(id, lines).unwrap();
        id
    }

    #[test]
    fn output_round_trips_per_run_and_history_keeps_every_run() {
        let ledger = RunLedger::open_in_memory().unwrap();
        let mixed = vec![
            (OutputStream::Stdout, "one".to_string()),
            (OutputStream::Stderr, "two".to_string()),
        ];
        let first = finished_run(&ledger, "fp", FinishOutcome::Exited(3), &mixed);
        let second = finished_run(&ledger, "fp", FinishOutcome::Killed, &out(&["x"]));
        assert_eq!(ledger.load_output(first).unwrap().unwrap(), mixed);
        assert_eq!(ledger.load_output(second).unwrap().unwrap(), out(&["x"]));

        let latest = ledger.latest_fresh_run("a", "b", "fp").unwrap().unwrap();
        assert_eq!(
            (latest.id, latest.outcome.as_str(), latest.exit_code),
            (second, "killed", None)
        );
        let history = ledger.history("a", "b", Some("fp")).unwrap();
        assert_eq!(
            history.iter().map(|r| r.id).collect::<Vec<_>>(),
            vec![second, first]
        );
        assert_eq!(history[1].exit_code, Some(3));
        assert!(history.iter().all(|r| !r.stale));
        assert_eq!(ledger.latest_fresh_run("a", "other", "fp").unwrap(), None);
    }

    #[test]
    fn a_changed_fingerprint_makes_runs_stale_and_a_reverted_one_fresh_again() {
        let ledger = RunLedger::open_in_memory().unwrap();
        let old = finished_run(&ledger, "v1", FinishOutcome::Exited(0), &out(&["v1"]));
        assert_eq!(ledger.latest_fresh_run("a", "b", "v2").unwrap(), None);
        let history = ledger.history("a", "b", Some("v2")).unwrap();
        assert!(history[0].stale);
        // The history is still all there; only what counts as current moved.
        assert_eq!(ledger.load_output(old).unwrap().unwrap(), out(&["v1"]));

        let newer = finished_run(&ledger, "v2", FinishOutcome::Exited(0), &out(&["v2"]));
        assert_eq!(
            ledger.latest_fresh_run("a", "b", "v2").unwrap().unwrap().id,
            newer
        );
        // Reverting the edit: the older run describes the document again,
        // and it is now the newest one that does.
        assert_eq!(
            ledger.latest_fresh_run("a", "b", "v1").unwrap().unwrap().id,
            old
        );
    }

    #[test]
    fn a_finished_run_reports_its_duration() {
        let ledger = RunLedger::open_in_memory().unwrap();
        let id = ledger.start("a", "b", RunKind::Plain, "test", 1).unwrap();
        ledger.set_fingerprint(id, "fp").unwrap();
        ledger
            .conn
            .lock()
            .unwrap()
            .execute(
                "UPDATE runs SET started_at = '2026-09-30T10:00:00Z' WHERE id = ?1",
                params![id],
            )
            .unwrap();
        ledger.finish(id, FinishOutcome::Exited(0)).unwrap();
        ledger
            .conn
            .lock()
            .unwrap()
            .execute(
                "UPDATE runs SET ended_at = '2026-09-30T10:00:02.500Z' WHERE id = ?1",
                params![id],
            )
            .unwrap();
        let run = ledger.latest_fresh_run("a", "b", "fp").unwrap().unwrap();
        assert_eq!(run.duration_ms, Some(2500));
        assert_eq!(
            ledger.addresses_with_runs().unwrap(),
            vec![("a".to_string(), "b".to_string(), "plain".to_string())]
        );
    }

    #[test]
    fn a_run_without_a_fingerprint_is_never_fresh() {
        let ledger = RunLedger::open_in_memory().unwrap();
        let id = ledger.start("a", "b", RunKind::Plain, "test", 1).unwrap();
        ledger.finish(id, FinishOutcome::Exited(0)).unwrap();
        assert_eq!(ledger.latest_fresh_run("a", "b", "").unwrap(), None);
        assert!(ledger.history("a", "b", Some("")).unwrap()[0].stale);
    }

    #[test]
    fn a_reset_marks_finished_runs_stale_but_keeps_them_and_spares_a_running_one() {
        let ledger = RunLedger::open_in_memory().unwrap();
        let old = finished_run(&ledger, "fp", FinishOutcome::Exited(0), &out(&["old"]));
        let running = ledger.start("a", "b", RunKind::Plain, "test", 1).unwrap();
        ledger.set_fingerprint(running, "fp").unwrap();
        ledger.mark_finished_runs_stale().unwrap();
        assert_eq!(ledger.latest_fresh_run("a", "b", "fp").unwrap(), None);
        assert!(
            ledger
                .get_run(old, "a", "b", Some("fp"))
                .unwrap()
                .unwrap()
                .stale
        );
        assert_eq!(ledger.load_output(old).unwrap().unwrap(), out(&["old"]));

        ledger.finish(running, FinishOutcome::Exited(0)).unwrap();
        ledger.save_output(running, &out(&["new"])).unwrap();
        assert_eq!(
            ledger.latest_fresh_run("a", "b", "fp").unwrap().unwrap().id,
            running
        );
    }

    #[test]
    fn rotation_keeps_only_the_newest_runs_per_address_and_drops_their_output() {
        let ledger = RunLedger::open_in_memory()
            .unwrap()
            .with_max_runs_per_block(2);
        let ids: Vec<i64> = (0..4)
            .map(|i| finished_run(&ledger, "fp", FinishOutcome::Exited(i), &out(&["x"])))
            .collect();
        // Another address must not be affected by a and b's rotation.
        let other = ledger
            .start("a", "other", RunKind::Plain, "test", 1)
            .unwrap();
        ledger.finish(other, FinishOutcome::Exited(0)).unwrap();

        let kept: Vec<i64> = ledger
            .history("a", "b", Some("fp"))
            .unwrap()
            .iter()
            .map(|r| r.id)
            .collect();
        assert_eq!(kept, vec![ids[3], ids[2]]);
        assert_eq!(ledger.load_output(ids[0]).unwrap(), None);
        assert_eq!(ledger.load_output(ids[3]).unwrap().unwrap(), out(&["x"]));
        assert_eq!(ledger.history("a", "other", None).unwrap().len(), 1);
    }

    #[test]
    fn a_running_row_never_counts_against_the_rotation_budget() {
        let ledger = RunLedger::open_in_memory()
            .unwrap()
            .with_max_runs_per_block(1);
        let done = finished_run(&ledger, "fp", FinishOutcome::Exited(0), &out(&["x"]));
        let running = ledger.start("a", "b", RunKind::Plain, "test", 1).unwrap();
        ledger.finish(done, FinishOutcome::Exited(0)).unwrap();
        assert!(ledger
            .get_run(done, "a", "b", Some("fp"))
            .unwrap()
            .is_some());
        assert_eq!(ledger.active_running().unwrap()[0].id, running);
    }

    #[test]
    fn output_keeps_the_newest_lines_within_the_byte_budget() {
        // Each "lineN" costs 6 bytes (5 + newline); a 13-byte budget fits two.
        let ledger = RunLedger::open_in_memory()
            .unwrap()
            .with_max_output_bytes(13);
        let id = finished_run(
            &ledger,
            "fp",
            FinishOutcome::Exited(0),
            &out(&["line1", "line2", "line3", "line4"]),
        );
        assert_eq!(
            ledger.load_output(id).unwrap().unwrap(),
            out(&["line3", "line4"])
        );
    }

    #[test]
    fn a_zero_byte_budget_stores_no_output_but_keeps_the_run() {
        let ledger = RunLedger::open_in_memory()
            .unwrap()
            .with_max_output_bytes(0);
        let id = finished_run(&ledger, "fp", FinishOutcome::Exited(0), &out(&["x"]));
        assert_eq!(ledger.load_output(id).unwrap(), None);
        assert_eq!(
            ledger.latest_fresh_run("a", "b", "fp").unwrap().unwrap().id,
            id
        );
    }

    #[test]
    fn start_on_a_free_address_succeeds_and_returns_an_id() {
        let ledger = RunLedger::open_in_memory().unwrap();
        let id = ledger
            .start("a", "block", RunKind::Plain, "cli", 1234)
            .unwrap();
        assert!(id > 0);
    }

    #[test]
    fn a_second_start_on_the_same_running_address_conflicts() {
        let ledger = RunLedger::open_in_memory().unwrap();
        ledger
            .start("a", "block", RunKind::Service, "cli", 1234)
            .unwrap();
        let err = ledger
            .start("a", "block", RunKind::Service, "tui", 5678)
            .unwrap_err();
        match err {
            StartError::Conflict(info) => {
                assert_eq!(info.pid, 1234);
                assert_eq!(info.owner, "cli");
                assert_eq!(info.kind, "service");
            }
            StartError::Io(e) => panic!("expected a conflict, got an io error: {e}"),
        }
    }

    #[test]
    fn finishing_a_row_frees_the_address_for_a_fresh_start() {
        let ledger = RunLedger::open_in_memory().unwrap();
        let id = ledger
            .start("a", "block", RunKind::Plain, "cli", 1234)
            .unwrap();
        ledger.finish(id, FinishOutcome::Exited(0)).unwrap();
        // No longer conflicts — the address's only row is no longer `running`.
        let second = ledger.start("a", "block", RunKind::Plain, "cli", 5678);
        assert!(second.is_ok());
    }

    #[test]
    fn update_pid_corrects_a_placeholder_without_disturbing_the_conflict_check() {
        let ledger = RunLedger::open_in_memory().unwrap();
        let id = ledger
            .start("a", "block", RunKind::Service, "cli", 0)
            .unwrap();
        ledger.update_pid(id, 4321).unwrap();
        let err = ledger
            .start("a", "block", RunKind::Service, "tui", 1)
            .unwrap_err();
        match err {
            StartError::Conflict(info) => assert_eq!(info.pid, 4321),
            StartError::Io(e) => panic!("expected a conflict, got an io error: {e}"),
        }
    }

    #[test]
    fn force_take_over_kills_the_stale_owner_and_claims_a_fresh_running_row() {
        let ledger = RunLedger::open_in_memory().unwrap();
        let mut dummy = spawn_dummy();
        let dummy_pid = dummy.id();
        let stale_id = ledger
            .start("a", "block", RunKind::Service, "cli", dummy_pid)
            .unwrap();

        let new_id = ledger
            .force_take_over("a", "block", RunKind::Service, "webui", 999)
            .unwrap();
        assert_ne!(new_id, stale_id);

        // `try_wait` (not a raw `kill(pid, 0)` probe) — SIGKILL leaves a
        // zombie until its parent reaps it, and this test *is* that parent
        // (`dummy` is this process's own `Child`), so a raw probe would
        // keep reporting "alive" long after the kill actually landed. Same
        // pitfall `crates/cli/tests/service_run_cmd.rs`'s own
        // `lock_conflict_on_worker_routed_run_kills_and_retries_when_
        // confirmed` test hit and fixed the same way.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut reaped = false;
        while !reaped && std::time::Instant::now() < deadline {
            if matches!(dummy.try_wait(), Ok(Some(_))) {
                reaped = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let _ = dummy.wait();
        assert!(reaped, "force_take_over should have killed the stale owner");

        // A second force-take-over succeeds again (kills the new row's own
        // fake pid 999 — a no-op signal, ESRCH-tolerant — then re-claims).
        let third_id = ledger
            .force_take_over("a", "block", RunKind::Service, "cli", 1)
            .unwrap();
        assert_ne!(third_id, new_id);
    }

    #[test]
    fn reconcile_startup_quietly_closes_a_dead_row_and_surfaces_a_live_one() {
        let ledger = RunLedger::open_in_memory().unwrap();
        let mut dummy = spawn_dummy();
        let dummy_pid = dummy.id();
        let live_id = ledger
            .start("a", "block", RunKind::Service, "cli", dummy_pid)
            .unwrap();

        // A pid essentially guaranteed dead: implausibly large, well past
        // any real OS pid_max (Linux's own absolute ceiling is 2^22), but
        // still well under `i32::MAX` — `is_alive` casts to `libc::pid_t`
        // (`i32`), and a value that overflowed *that* would be
        // reinterpreted as a negative pid, which `kill(2)` treats as a
        // process-*group* signal instead of "no such process" — a
        // correctness trap, not just a flaky test. Safer than spawning-
        // then-reaping a real child and hoping its now-free pid isn't
        // reused before this asserts on it (a real, if unlikely, race).
        let dead_pid: u32 = 999_999_999;
        let dead_id = ledger
            .start("b", "block", RunKind::Plain, "cli", dead_pid)
            .unwrap();

        let orphaned = ledger.reconcile_startup().unwrap();
        assert_eq!(orphaned.len(), 1, "orphaned: {orphaned:?}");
        assert_eq!(orphaned[0].id, live_id);

        let active = ledger.active_running().unwrap();
        assert_eq!(active.len(), 1, "active: {active:?}");
        assert_eq!(active[0].id, live_id);
        let _ = dead_id;

        let _ = dummy.kill();
        let _ = dummy.wait();
    }
}
