//! One shared SQLite connection per canvas — `.meshfox/<canvas file name>.
//! session.sqlite3`, colocated the same way `crates/core/src/worker_lock.
//! rs`'s own `.worker.lock`/`crates/core/src/varcache.rs`'s own `.env`
//! already are. [`undo_log`](crate::undo_log) and
//! [`run_ledger`](crate::run_ledger) are two independent sets of tables
//! living on this one connection — a single `Arc<Mutex<Connection>>`, not
//! two separate `Connection::open` calls against the same file, which would
//! invite the well-known `SQLITE_BUSY` contention two connections to one
//! sqlite file can hit even from the same process. Each module runs its own
//! `CREATE TABLE IF NOT EXISTS` schema against whatever connection it's
//! handed (`UndoLog::from_connection`/`RunLedger::from_connection`) — order
//! between the two doesn't matter, both are idempotent.

use rusqlite::Connection;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

pub(crate) fn sqlite_err(e: rusqlite::Error) -> io::Error {
    io::Error::other(e)
}

/// Where a canvas's session database lives — canonicalizes `canvas_path`
/// first so callers that reach the same file via different (relative,
/// symlinked, differently-`cwd`'d) spellings still agree on one path; falls
/// back to the path as given if canonicalization fails (e.g. the canvas
/// doesn't exist yet).
pub(crate) fn session_db_path(canvas_path: &Path) -> PathBuf {
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

/// Opens (creating the file and its parent directory if needed)
/// `canvas_path`'s own session database — no schema of its own here, just
/// the connection; each table-owning module runs its own schema against the
/// `Arc` this returns. `pub` (not `pub(crate)`) so `meshfox-cli`'s own
/// tests can open the exact same connection a real worker would, to seed a
/// `run_ledger` row directly (see this module's own doc comment).
pub fn open(canvas_path: &Path) -> io::Result<Arc<Mutex<Connection>>> {
    let path = session_db_path(canvas_path);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let conn = Connection::open(path).map_err(sqlite_err)?;
    Ok(Arc::new(Mutex::new(conn)))
}

/// The connection for a canvas served read-only: an in-memory database, so
/// undo history and the run ledger work for as long as the worker lives and
/// nothing is written next to the canvas (or anywhere else).
pub fn open_in_memory() -> io::Result<Arc<Mutex<Connection>>> {
    let conn = Connection::open_in_memory().map_err(sqlite_err)?;
    Ok(Arc::new(Mutex::new(conn)))
}
