//! Lock file for a `service` block (see `crate::fence::CodeBlock::service`
//! and SPEC.md's "Service blocks (experimental)") — one small file per
//! service under a sibling `.meshfox/` directory, same colocation
//! convention as `crate::varcache`. Every frontend that can spawn a service
//! (the webui server, the TUI, `meshfox run`) checks/acquires this before
//! spawning, so two independent OS processes never silently both believe
//! they own the same long-lived background process — see the "Ownership/
//! locking" decision in SPEC.md's service-blocks section: any existing
//! lock, live owner or dead one, is always surfaced to the user rather than
//! silently cleaned up or silently blocked on.

use std::io;
use std::path::{Path, PathBuf};

/// Where a service's lock file lives — `.meshfox/services/` next to the
/// canvas file, named after the canvas file plus the block's own address.
/// Mirrors `crate::varcache::cache_path`'s "colocated with the document,
/// not the current directory" reasoning, so it stays correct regardless of
/// where meshfox happens to be invoked from.
pub fn lock_path(canvas_path: &Path, node_id: &str, block_name: &str) -> PathBuf {
    let dir = match canvas_path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };
    let canvas_file = canvas_path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    dir.join(".meshfox").join("services").join(format!(
        "{canvas_file}__{}__{}.lock",
        sanitize(node_id),
        sanitize(block_name)
    ))
}

/// Filesystem-safe stand-in for a node id/block name in a lock filename —
/// anything that isn't alphanumeric/`-`/`_` becomes `_`, so a node id
/// containing `/` (an `include`-spliced node's namespaced id, e.g.
/// `docs/setup`) can't escape the `services/` directory or collide with the
/// `__` separator.
fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect()
}

/// Who's recorded as owning a lock — enough for a conflict prompt to show a
/// useful message ("pid 1234, started via webui").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LockInfo {
    pub pid: u32,
    /// Which kind of frontend acquired this lock — `"webui"`, `"tui"`, or
    /// `"cli"` by convention, but treated as an opaque display string here.
    pub owner: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LockState {
    Free,
    /// A lock file exists. `alive` is whether `info.pid` currently answers
    /// to a liveness probe (`is_alive`) — **not** used to skip the conflict
    /// prompt (a dead owner's lock still prompts, by product decision, see
    /// this module's own doc comment); only to tell a caller resolving the
    /// conflict whether it actually needs to signal a process or can just
    /// remove an already-stale file.
    Held { info: LockInfo, alive: bool },
}

/// Reads `path` and reports whether it's free, or held (live or stale).
/// `NotFound` is `Free`, not an error — a service that's never been
/// started has no lock file yet.
pub fn check(path: &Path) -> io::Result<LockState> {
    let contents = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(LockState::Free),
        Err(e) => return Err(e),
    };
    let info = parse(&contents).unwrap_or(LockInfo {
        pid: 0,
        owner: "unknown".to_string(),
    });
    let alive = info.pid != 0 && is_alive(info.pid);
    Ok(LockState::Held { info, alive })
}

fn parse(contents: &str) -> Option<LockInfo> {
    let entries = crate::dotenv::parse(contents);
    let pid: u32 = entries.get("pid")?.parse().ok()?;
    let owner = entries.get("owner").cloned().unwrap_or_default();
    Some(LockInfo { pid, owner })
}

/// Why `acquire` failed — distinguishes "someone else already holds it"
/// (the caller may want to show *who*, or force-kill-and-retry) from a
/// genuine I/O problem (permissions, disk full, ...).
#[derive(Debug)]
pub enum AcquireError {
    /// Something else already holds this lock — best-effort info about who
    /// (read back from the file right after losing the race; `LockInfo {
    /// pid: 0, owner: "unknown" }` in the vanishingly rare case the file
    /// vanished again between losing the race and reading it back).
    Conflict(LockInfo),
    Io(io::Error),
}

impl std::fmt::Display for AcquireError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AcquireError::Conflict(info) => {
                write!(f, "lock already held by pid {} ({})", info.pid, info.owner)
            }
            AcquireError::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for AcquireError {}

/// Flattens a conflict into a plain `io::Error` (`AlreadyExists`) — lets
/// every existing `acquire(..)?` call site that only ever handled a bare
/// `io::Result` keep compiling/behaving exactly as before (it already did
/// its own separate `check` for conflict *info*; this is just what a plain
/// `?` now sees for the rare case that check-then-act call missed).
impl From<AcquireError> for io::Error {
    fn from(e: AcquireError) -> Self {
        match e {
            AcquireError::Conflict(info) => io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("lock already held by pid {} ({})", info.pid, info.owner),
            ),
            AcquireError::Io(e) => e,
        }
    }
}

/// Atomically creates a fresh lock file recording `pid`/`owner` — an
/// exclusive create (`O_EXCL`-equivalent), not a plain overwrite, so two
/// processes (or two threads in one process) racing to acquire the same
/// path can never both believe they won: exactly one `create_new` succeeds,
/// the other gets `AcquireError::Conflict`. Creates `.meshfox/services/` if
/// needed. Unlike the old check-then-write shape, callers no longer need to
/// (and shouldn't) call `check` first to decide whether to acquire — only
/// to *display* who currently holds it before deciding whether to retry/
/// force.
pub fn acquire(path: &Path, pid: u32, owner: &str) -> Result<(), AcquireError> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(AcquireError::Io)?;
    }
    let mut file = match std::fs::OpenOptions::new().write(true).create_new(true).open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
            let info = match check(path) {
                Ok(LockState::Held { info, .. }) => info,
                // Raced again: held a moment ago (our `create_new` lost),
                // free now (they released between then and this `check`).
                // Best-effort placeholder — the caller's real signal here
                // is "conflict, try again", not this specific info.
                Ok(LockState::Free) => LockInfo { pid: 0, owner: "unknown".to_string() },
                Err(e2) => return Err(AcquireError::Io(e2)),
            };
            return Err(AcquireError::Conflict(info));
        }
        Err(e) => return Err(AcquireError::Io(e)),
    };
    use std::io::Write as _;
    file.write_all(format!("pid={pid}\nowner={owner}\n").as_bytes())
        .map_err(AcquireError::Io)
}

/// Forcibly takes over `path`: kills whatever process the current holder's
/// own pid names (whole process group, `SIGKILL` — same reach as every
/// other kill in this codebase, since every spawner makes its child the
/// leader of a fresh group), releases the now-stale lock file, then
/// acquires a fresh one for `pid`/`owner`. Consolidates the "kill the pid
/// recorded on disk, even though it may belong to a process this one has no
/// live handle for" pattern every force-run/force-start path needs, instead
/// of each caller hand-rolling its own `unsafe { libc::kill(...) }`. A
/// no-op (straight to `acquire`) if `path` turns out already free by the
/// time this runs.
pub fn kill_and_acquire(path: &Path, pid: u32, owner: &str) -> io::Result<()> {
    if let LockState::Held { info, .. } = check(path)? {
        if info.pid != 0 {
            // SAFETY: see `is_alive`'s own comment on `kill(.., 0)`; here we
            // actually deliver `SIGKILL` to the whole process group `-pid`
            // leads, which is exactly the group every spawner in this
            // codebase creates for its own child.
            let ret = unsafe { libc::kill(-(info.pid as libc::pid_t), libc::SIGKILL) };
            if ret != 0 && io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH) {
                return Err(io::Error::last_os_error());
            }
        }
        release(path)?;
    }
    acquire(path, pid, owner).map_err(io::Error::from)
}

/// Overwrites the pid recorded in a lock file this caller already holds —
/// for when the real pid of whatever a lock protects only becomes known
/// *after* the lock itself was already claimed (a spawned child's pid
/// isn't known until it's actually spawned, but a queued-time lock — see
/// `crates/server/src/lib.rs`'s own up-front, whole-chain locking — has to
/// be atomically claimed *before* that, using a placeholder pid, purely to
/// win the race against a concurrent attempt on the same address). Not
/// itself atomic/exclusive like `acquire` — this assumes the caller
/// already exclusively owns `path` and is just correcting its own record,
/// not claiming it fresh. Keeps whatever `owner` string the file already
/// had; a missing/unparseable existing file (shouldn't happen in practice —
/// the caller is expected to have just `acquire`d it) falls back to an
/// empty owner rather than failing outright.
pub fn update_owner_pid(path: &Path, pid: u32) -> io::Result<()> {
    let owner = std::fs::read_to_string(path)
        .ok()
        .and_then(|s| parse(&s))
        .map(|info| info.owner)
        .unwrap_or_default();
    std::fs::write(path, format!("pid={pid}\nowner={owner}\n"))
}

/// Removes the lock file, if any — used both when a service is stopped
/// cleanly and when resolving a conflict against a dead/killed owner.
/// `NotFound` is not an error — releasing an already-absent lock is a
/// no-op, not a failure.
pub fn release(path: &Path) -> io::Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// Whether `pid` currently names a live process — `kill(pid, 0)` sends no
/// signal, it just probes existence/permission. `pid == 0` is never
/// considered alive (not a real process id meshfox itself would record).
pub fn is_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    // SAFETY: signal 0 is documented (POSIX `kill(2)`) to perform no actual
    // signal delivery, only existence/permission checking.
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lock_path_is_a_sibling_meshfox_services_dir() {
        let p = lock_path(Path::new("examples/hello.canvas.md"), "root", "dev-server");
        assert_eq!(
            p,
            PathBuf::from("examples/.meshfox/services/hello.canvas.md__root__dev-server.lock")
        );
    }

    #[test]
    fn lock_path_sanitizes_a_namespaced_node_id() {
        let p = lock_path(Path::new("doc.canvas.md"), "docs/setup", "srv");
        assert_eq!(
            p,
            PathBuf::from("./.meshfox/services/doc.canvas.md__docs_setup__srv.lock")
        );
    }

    fn tmp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("meshfox-service-lock-test-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn check_is_free_when_no_lock_file_exists() {
        let dir = tmp_dir("free");
        let path = dir.join("x.lock");
        assert_eq!(check(&path).unwrap(), LockState::Free);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn acquire_then_check_reports_held_and_alive_for_our_own_pid() {
        let dir = tmp_dir("held-alive");
        let path = dir.join("x.lock");
        let my_pid = std::process::id();
        acquire(&path, my_pid, "cli").unwrap();
        let state = check(&path).unwrap();
        assert_eq!(
            state,
            LockState::Held {
                info: LockInfo { pid: my_pid, owner: "cli".to_string() },
                alive: true,
            }
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn check_reports_a_dead_pid_as_held_but_not_alive() {
        let dir = tmp_dir("held-dead");
        let path = dir.join("x.lock");
        // PID 1 is always alive on a real system but never something this
        // test process could plausibly be, and an arbitrarily huge PID is
        // never assigned — use that as a stand-in for "definitely dead".
        acquire(&path, 999_999, "tui").unwrap();
        let state = check(&path).unwrap();
        assert_eq!(
            state,
            LockState::Held {
                info: LockInfo { pid: 999_999, owner: "tui".to_string() },
                alive: false,
            }
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn release_then_check_is_free_again() {
        let dir = tmp_dir("release");
        let path = dir.join("x.lock");
        acquire(&path, std::process::id(), "webui").unwrap();
        release(&path).unwrap();
        assert_eq!(check(&path).unwrap(), LockState::Free);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn release_of_a_nonexistent_lock_is_not_an_error() {
        let dir = tmp_dir("release-missing");
        let path = dir.join("x.lock");
        assert!(release(&path).is_ok());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn second_acquire_of_an_already_held_lock_is_a_conflict_not_an_overwrite() {
        let dir = tmp_dir("acquire-conflict");
        let path = dir.join("x.lock");
        acquire(&path, 111, "webui").unwrap();
        match acquire(&path, 222, "tui") {
            Err(AcquireError::Conflict(info)) => {
                assert_eq!(info, LockInfo { pid: 111, owner: "webui".to_string() });
            }
            other => panic!("expected a conflict against the first owner, got {other:?}"),
        }
        // The loser must not have clobbered the winner's file.
        assert_eq!(
            check(&path).unwrap(),
            LockState::Held {
                info: LockInfo { pid: 111, owner: "webui".to_string() },
                alive: false,
            }
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn concurrent_acquire_attempts_have_exactly_one_winner() {
        // Same real race a cross-process file lock exists to arbitrate,
        // reproduced with threads racing the same path — `create_new` is
        // atomic at the OS level regardless of whether the two callers are
        // threads or separate processes, so this exercises the same
        // guarantee. If `acquire` ever regressed to check-then-write, both
        // could plausibly "win" here.
        let dir = tmp_dir("acquire-race");
        let path = std::sync::Arc::new(dir.join("x.lock"));
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
        let handles: Vec<_> = (0..8u32)
            .map(|i| {
                let path = std::sync::Arc::clone(&path);
                let barrier = std::sync::Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    acquire(&path, 1000 + i, "race").is_ok()
                })
            })
            .collect();
        let wins = handles.into_iter().map(|h| h.join().unwrap()).filter(|&ok| ok).count();
        assert_eq!(wins, 1, "exactly one racer should have won the lock");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn kill_and_acquire_takes_over_a_lock_whose_owner_is_gone() {
        let dir = tmp_dir("kill-and-acquire");
        let path = dir.join("x.lock");
        // A huge, never-assigned pid stands in for "the recorded owner is
        // already gone" — `kill_and_acquire` should tolerate `ESRCH` from
        // signaling it (same as every other whole-process-group kill in
        // this codebase) and still take over the lock.
        acquire(&path, 999_999, "tui").unwrap();
        kill_and_acquire(&path, std::process::id(), "webui").unwrap();
        assert_eq!(
            check(&path).unwrap(),
            LockState::Held {
                info: LockInfo { pid: std::process::id(), owner: "webui".to_string() },
                alive: true,
            }
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn update_owner_pid_corrects_the_pid_while_keeping_the_owner() {
        let dir = tmp_dir("update-owner-pid");
        let path = dir.join("x.lock");
        // Same shape a queued-time acquire uses: claim with a placeholder
        // pid first (the calling process's own, before anything real has
        // spawned yet), then correct it once a real child's pid is known.
        acquire(&path, std::process::id(), "webui").unwrap();
        update_owner_pid(&path, 555).unwrap();
        assert_eq!(
            check(&path).unwrap(),
            LockState::Held {
                info: LockInfo { pid: 555, owner: "webui".to_string() },
                alive: false,
            }
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn kill_and_acquire_on_an_already_free_path_just_acquires() {
        let dir = tmp_dir("kill-and-acquire-free");
        let path = dir.join("x.lock");
        kill_and_acquire(&path, std::process::id(), "webui").unwrap();
        assert_eq!(
            check(&path).unwrap(),
            LockState::Held {
                info: LockInfo { pid: std::process::id(), owner: "webui".to_string() },
                alive: true,
            }
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}
