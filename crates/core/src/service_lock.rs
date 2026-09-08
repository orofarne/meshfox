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

/// Writes a fresh lock file recording `pid`/`owner`, creating
/// `.meshfox/services/` if needed. Overwrites unconditionally — callers are
/// expected to have already called `check` and resolved any conflict
/// (including asking the user) before acquiring.
pub fn acquire(path: &Path, pid: u32, owner: &str) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
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
}
