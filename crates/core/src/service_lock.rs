//! Liveness check shared by whatever now owns per-run conflict detection
//! (`crates/server/src/run_ledger.rs`'s own `reconcile_startup`) — this
//! module used to be the conflict-detection mechanism itself (a lock file
//! per `service` block under `.meshfox/services/`, recording `pid=...\n
//! owner=...\n`), replaced by `run_ledger`'s SQLite-backed `runs` table
//! (see TODO.canvas.md's own "персистентное состояние сессии" entry for
//! why: one shared audit log doubling as startup reconciliation, instead of
//! a lock file with no history and no way to tell "conflict" from "crashed
//! and orphaned" without a human noticing). `is_alive` is the one piece of
//! that old module with nothing to do with locking at all — just "does this
//! pid still answer" — so it outlived the rest.

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
    fn is_alive_is_true_for_our_own_pid() {
        assert!(is_alive(std::process::id()));
    }

    #[test]
    fn is_alive_is_false_for_pid_zero() {
        assert!(!is_alive(0));
    }

    #[test]
    fn is_alive_is_false_for_an_implausible_pid() {
        // Never actually assigned on a real system — see
        // `crates/server/src/run_ledger.rs`'s own tests for why a huge
        // `u32` (rather than this one) would be the wrong choice for a
        // "definitely dead" stand-in used with a real `kill()` call: it'd
        // overflow `libc::pid_t` (`i32`) and get reinterpreted as a
        // process-group signal. This one stays safely within `i32::MAX`.
        assert!(!is_alive(999_999_999));
    }
}
