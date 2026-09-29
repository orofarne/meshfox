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
        exit_code   INTEGER
    );
    CREATE UNIQUE INDEX IF NOT EXISTS runs_one_running_per_address
        ON runs (node_id, block) WHERE outcome = 'running';
";

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
                write!(f, "already running elsewhere (pid {}, {})", info.pid, info.owner)
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
}

impl RunLedger {
    /// Runs this module's own schema against an already-open connection —
    /// see [`crate::session_db`] for why this takes a shared `Arc` rather
    /// than opening its own. Idempotent, safe to call every worker startup.
    pub fn from_connection(conn: Arc<Mutex<Connection>>) -> io::Result<Self> {
        conn.lock().unwrap().execute_batch(SCHEMA_SQL).map_err(sqlite_err)?;
        Ok(RunLedger { conn })
    }

    /// `pub(crate)` (not just `fn`, unlike most test-only helpers here) —
    /// `services::tests` needs one too, to stand in for the shared ledger a
    /// real caller would pass into `spawn`.
    #[cfg(test)]
    pub(crate) fn open_in_memory() -> io::Result<Self> {
        let conn = Connection::open_in_memory().map_err(sqlite_err)?;
        conn.execute_batch(SCHEMA_SQL).map_err(sqlite_err)?;
        Ok(RunLedger { conn: Arc::new(Mutex::new(conn)) })
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
        let started_at = meshfox_core::timestamp::now_utc_rfc3339();
        let conn = self.conn.lock().unwrap();
        let result = conn.execute(
            "INSERT INTO runs (node_id, block, kind, owner, pid, started_at, outcome) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'running')",
            params![node_id, block, kind.as_str(), owner, pid, started_at],
        );
        match result {
            Ok(_) => Ok(conn.last_insert_rowid()),
            Err(rusqlite::Error::SqliteFailure(e, _)) if e.code == ErrorCode::ConstraintViolation => {
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
        let ended_at = meshfox_core::timestamp::now_utc_rfc3339();
        self.conn
            .lock()
            .unwrap()
            .execute(
                "UPDATE runs SET outcome = ?1, exit_code = ?2, ended_at = ?3 WHERE id = ?4",
                params![outcome.outcome_str(), outcome.exit_code(), ended_at, id],
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
        let Some((stale_id, stale_pid)) = stale else { return Ok(()) };
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

    #[test]
    fn start_on_a_free_address_succeeds_and_returns_an_id() {
        let ledger = RunLedger::open_in_memory().unwrap();
        let id = ledger.start("a", "block", RunKind::Plain, "cli", 1234).unwrap();
        assert!(id > 0);
    }

    #[test]
    fn a_second_start_on_the_same_running_address_conflicts() {
        let ledger = RunLedger::open_in_memory().unwrap();
        ledger.start("a", "block", RunKind::Service, "cli", 1234).unwrap();
        let err = ledger.start("a", "block", RunKind::Service, "tui", 5678).unwrap_err();
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
        let id = ledger.start("a", "block", RunKind::Plain, "cli", 1234).unwrap();
        ledger.finish(id, FinishOutcome::Exited(0)).unwrap();
        // No longer conflicts — the address's only row is no longer `running`.
        let second = ledger.start("a", "block", RunKind::Plain, "cli", 5678);
        assert!(second.is_ok());
    }

    #[test]
    fn update_pid_corrects_a_placeholder_without_disturbing_the_conflict_check() {
        let ledger = RunLedger::open_in_memory().unwrap();
        let id = ledger.start("a", "block", RunKind::Service, "cli", 0).unwrap();
        ledger.update_pid(id, 4321).unwrap();
        let err = ledger.start("a", "block", RunKind::Service, "tui", 1).unwrap_err();
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
        let stale_id = ledger.start("a", "block", RunKind::Service, "cli", dummy_pid).unwrap();

        let new_id = ledger.force_take_over("a", "block", RunKind::Service, "webui", 999).unwrap();
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
        let third_id = ledger.force_take_over("a", "block", RunKind::Service, "cli", 1).unwrap();
        assert_ne!(third_id, new_id);
    }

    #[test]
    fn reconcile_startup_quietly_closes_a_dead_row_and_surfaces_a_live_one() {
        let ledger = RunLedger::open_in_memory().unwrap();
        let mut dummy = spawn_dummy();
        let dummy_pid = dummy.id();
        let live_id = ledger.start("a", "block", RunKind::Service, "cli", dummy_pid).unwrap();

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
        let dead_id = ledger.start("b", "block", RunKind::Plain, "cli", dead_pid).unwrap();

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
