//! Per-canvas discovery lock for a `meshfox view` worker — one small file
//! next to the canvas (`.meshfox/<file>.worker.lock`, same colocation
//! convention as `crate::varcache`), held via a real `flock` rather than a
//! pid file + liveness check.
//!
//! A pid-recorded lock (record a pid, `kill(pid, 0)` to tell a live owner
//! from a stale one — the approach `crates/server/src/run_ledger.rs` uses
//! for the unrelated per-`service`-block conflict-detection table) is
//! correct, but needs a separate "is the recorded pid still alive" step,
//! which is its own (harmless but avoidable) race when two callers notice
//! the same stale lock at once. `flock` sidesteps that class of race
//! entirely for this use case: holding the lock *is* being alive, full
//! stop — the OS drops it automatically on any exit of the holding
//! process, including `SIGKILL`, with no cooperation from that process
//! required. So there's nothing here to distinguish "held but stale" from
//! "held" — only "held" or "free".
//!
//! Used to let independent `meshfox view <path>` invocations (separate
//! watcher processes — see `crates/server/src/run_ledger.rs` for the
//! unrelated per-`service`/`tty`/plain-run conflict-detection table, and
//! `crates/cli/src/watcher.rs` for what a worker actually is) on the same
//! canvas file discover and reuse whichever one of them is already serving
//! it, instead of each spawning its own worker/server pair.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Where a canvas's worker-discovery lock lives — canonicalizes `canvas_path`
/// first so callers that reach the same file via different (relative,
/// symlinked, differently-`cwd`'d) spellings still agree on one path; falls
/// back to the path as given if canonicalization fails (e.g. the canvas
/// doesn't exist yet) — needed here since a worker lock can be checked
/// before the canvas is known to exist.
pub fn lock_path(canvas_path: &Path) -> PathBuf {
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
        .join(format!("{file_name}.worker.lock"))
}

/// Held for as long as this process wants to be *the* worker for a canvas —
/// dropping it (including implicitly, on process exit/crash) releases the
/// underlying `flock` immediately; there's no separate `release` function to
/// forget to call. Keep this alive for the worker's whole lifetime (e.g. a
/// local binding that outlives the serve loop), not just around the moment
/// the port is written.
pub struct LockGuard {
    /// `None` for a read-only worker (see [`LockGuard::read_only`]): it holds
    /// no lock at all.
    file: Option<File>,
}

impl LockGuard {
    /// The guard of a worker for a canvas whose directory (or the canvas file
    /// itself) can't be written. It holds no lock and records no port: a
    /// read-only worker keeps nothing on disk, so several of them can serve
    /// the same canvas side by side without ever conflicting, and none of them
    /// is discoverable by anyone else.
    pub fn read_only() -> Self {
        LockGuard { file: None }
    }

    /// Whether this worker serves its canvas read-only — see
    /// [`LockGuard::read_only`].
    pub fn is_read_only(&self) -> bool {
        self.file.is_none()
    }

    /// Records the port this worker actually bound (only known after
    /// `TcpListener::bind`, so this is necessarily a separate step from
    /// acquiring the lock itself) — overwrites any previous contents. A no-op
    /// for a read-only worker, which has nowhere to record it.
    pub fn write_port(&mut self, port: u16) -> io::Result<()> {
        let Some(file) = self.file.as_mut() else {
            return Ok(());
        };
        file.set_len(0)?;
        file.seek(SeekFrom::Start(0))?;
        writeln!(file, "port={port}")?;
        file.flush()
    }
}

/// Whether `e` says "this location can't be written": a permission error or a
/// read-only filesystem. Anything else (a full disk, an I/O error) is a real
/// failure and is not mistaken for a read-only canvas.
pub fn is_read_only_error(e: &io::Error) -> bool {
    e.kind() == io::ErrorKind::PermissionDenied || e.raw_os_error() == Some(libc::EROFS)
}

/// Whether the canvas file itself refuses writes. Opening it for writing
/// without truncating changes nothing on disk. A file that isn't there (yet)
/// counts as writable, so the real "no such file" error is reported by
/// whatever reads it next.
fn canvas_file_is_read_only(canvas_path: &Path) -> bool {
    match OpenOptions::new().write(true).open(canvas_path) {
        Ok(_) => false,
        Err(e) => is_read_only_error(&e),
    }
}

/// Result of contending for a canvas's worker lock.
pub enum Acquired {
    /// Nobody else holds it — caller is now the worker; bind, then
    /// [`LockGuard::write_port`]. A canvas that can't be written also comes
    /// back as `Us`, with a [`LockGuard::read_only`] guard: the caller is the
    /// worker, it just holds nothing.
    Us(LockGuard),
    /// Somebody else already holds it and has recorded their bound port —
    /// caller should not bind its own listener at all.
    Other { port: u16 },
}


/// Whether `canvas_path` is served read-only — the same verdict
/// [`try_acquire`] reaches, for a caller that has no lock of its own to ask
/// (the interpreter picking where a venv goes). Probes the directory the way
/// the lock does: it must be possible to create `.meshfox/` and write in it.
pub fn is_read_only(canvas_path: &Path) -> bool {
    if canvas_file_is_read_only(canvas_path) {
        return true;
    }
    let Some(dir) = lock_path(canvas_path).parent().map(Path::to_path_buf) else {
        return false;
    };
    match std::fs::create_dir_all(&dir) {
        Ok(()) => {}
        Err(e) => return is_read_only_error(&e),
    }
    let Ok(c_dir) = std::ffi::CString::new(dir.as_os_str().as_encoded_bytes()) else {
        return false;
    };
    // SAFETY: `access(2)` on a valid NUL-terminated path; reads nothing else.
    unsafe { libc::access(c_dir.as_ptr(), libc::W_OK) != 0 }
}

/// Non-blocking: either wins the lock outright, or finds out who already
/// holds it and what port they're on. Never blocks waiting for the current
/// holder to release — there's nothing to wait for, a live holder is exactly
/// what makes this call resolve to `Other` instead of `Us`.
///
/// A canvas that can't be written — its file refuses writes, or its
/// `.meshfox/` can't be created or opened for the lock — is served read-only:
/// `Us` with [`LockGuard::read_only`], no lock taken and nothing left on disk.
/// The lock itself is the probe for the directory, so there's no separate test
/// write that could disagree with what's actually needed.
pub fn try_acquire(canvas_path: &Path) -> io::Result<Acquired> {
    if canvas_file_is_read_only(canvas_path) {
        return Ok(Acquired::Us(LockGuard::read_only()));
    }
    let path = lock_path(canvas_path);
    let opened = match path.parent() {
        Some(dir) => std::fs::create_dir_all(dir),
        None => Ok(()),
    }
    .and_then(|()| {
        OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            // Explicit, not the default: this handle may end up on the losing
            // side of the `flock` below (`Acquired::Other`), in which case
            // truncating here would wipe the current holder's own `port=...`
            // line out from under it before we even know who won.
            .truncate(false)
            .open(&path)
    });
    let file = match opened {
        Ok(file) => file,
        Err(e) if is_read_only_error(&e) => {
            return Ok(Acquired::Us(LockGuard::read_only()));
        }
        Err(e) => return Err(e),
    };
    // SAFETY: `flock(2)` on a valid, open fd we exclusively own here;
    // `LOCK_EX | LOCK_NB` never blocks — it fails fast with `EWOULDBLOCK`
    // if another process already holds it.
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if rc == 0 {
        return Ok(Acquired::Us(LockGuard { file: Some(file) }));
    }
    let err = io::Error::last_os_error();
    if err.raw_os_error() != Some(libc::EWOULDBLOCK) {
        return Err(err);
    }
    Ok(Acquired::Other {
        port: read_port_with_retry(&path)?,
    })
}

/// The current holder acquired the lock and then, a moment later, writes its
/// port into the same file — a caller that loses the race can in principle
/// observe the file before that write lands. Retries briefly rather than
/// failing outright; 20 attempts * 10ms is generous next to how fast that
/// write actually happens in practice, small next to any human-perceptible
/// delay.
fn read_port_with_retry(path: &Path) -> io::Result<u16> {
    for attempt in 0..20 {
        if let Ok(mut f) = File::open(path) {
            let mut buf = String::new();
            if f.read_to_string(&mut buf).is_ok() {
                if let Some(port) = parse_port(&buf) {
                    return Ok(port);
                }
            }
        }
        if attempt < 19 {
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        format!(
            "worker lock at {} is held, but never reported a port",
            path.display()
        ),
    ))
}

fn parse_port(contents: &str) -> Option<u16> {
    crate::dotenv::parse(contents).get("port")?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_canvas(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "meshfox-worker-lock-test-{name}-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("doc.canvas.md");
        std::fs::write(&path, "# Doc\n").unwrap();
        path
    }

    #[test]
    fn lock_path_is_a_sibling_meshfox_dir() {
        let dir = std::env::temp_dir().join(format!(
            "meshfox-worker-lock-test-path-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("doc.canvas.md");
        std::fs::write(&path, "# Doc\n").unwrap();
        let locked = lock_path(&path);
        assert_eq!(
            locked,
            dir.canonicalize()
                .unwrap()
                .join(".meshfox")
                .join("doc.canvas.md.worker.lock")
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn first_caller_wins_second_reads_its_port() {
        let canvas = tmp_canvas("first-wins");
        let mut us = match try_acquire(&canvas).unwrap() {
            Acquired::Us(guard) => guard,
            Acquired::Other { .. } => panic!("expected to win an uncontended lock"),
        };
        us.write_port(4242).unwrap();

        match try_acquire(&canvas).unwrap() {
            Acquired::Other { port } => assert_eq!(port, 4242),
            Acquired::Us(_) => panic!("second caller should not have won an already-held lock"),
        }
        std::fs::remove_dir_all(canvas.parent().unwrap()).ok();
    }

    #[test]
    fn releasing_the_guard_lets_a_later_caller_win() {
        let canvas = tmp_canvas("release-then-win");
        {
            let mut us = match try_acquire(&canvas).unwrap() {
                Acquired::Us(guard) => guard,
                Acquired::Other { .. } => panic!("expected to win an uncontended lock"),
            };
            us.write_port(1).unwrap();
            // `us` drops here, closing its fd and releasing the flock —
            // simulates the holding process exiting.
        }
        match try_acquire(&canvas).unwrap() {
            Acquired::Us(_) => {}
            Acquired::Other { .. } => panic!("dropped guard should have released the lock"),
        }
        std::fs::remove_dir_all(canvas.parent().unwrap()).ok();
    }

    /// A directory (or file) with its write bits off, and whether the test
    /// can tell: root ignores permission bits, so there's nothing to see.
    fn chmod(path: &Path, mode: u32) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
    }

    fn running_as_root() -> bool {
        // SAFETY: `geteuid` takes no arguments and can't fail.
        unsafe { libc::geteuid() == 0 }
    }

    fn restore_and_remove(dir: &Path) {
        chmod(dir, 0o755);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn an_unwritable_directory_is_served_read_only_without_a_lock() {
        if running_as_root() {
            return;
        }
        let canvas = tmp_canvas("ro-dir");
        let dir = canvas.parent().unwrap().to_path_buf();
        chmod(&dir, 0o555);

        assert!(is_read_only(&canvas));
        let mut first = match try_acquire(&canvas).unwrap() {
            Acquired::Us(guard) => guard,
            Acquired::Other { .. } => panic!("nothing can be holding a lock here"),
        };
        assert!(first.is_read_only());
        // Nowhere to record a port, and nothing to fail over it.
        first.write_port(4242).unwrap();
        // Read-only workers don't contend: a second one is just another `Us`.
        match try_acquire(&canvas).unwrap() {
            Acquired::Us(second) => assert!(second.is_read_only()),
            Acquired::Other { .. } => panic!("a read-only worker holds no lock to find"),
        }
        assert!(!dir.join(".meshfox").exists(), "state left in a read-only directory");
        restore_and_remove(&dir);
    }

    #[test]
    fn an_unwritable_file_is_read_only_even_in_a_writable_directory() {
        if running_as_root() {
            return;
        }
        let canvas = tmp_canvas("ro-file");
        let dir = canvas.parent().unwrap().to_path_buf();
        chmod(&canvas, 0o444);

        assert!(is_read_only(&canvas));
        match try_acquire(&canvas).unwrap() {
            Acquired::Us(guard) => assert!(guard.is_read_only()),
            Acquired::Other { .. } => panic!("nothing can be holding a lock here"),
        }
        assert!(!dir.join(".meshfox").exists(), "state left next to a read-only file");
        restore_and_remove(&dir);
    }

    #[test]
    fn an_existing_but_unwritable_meshfox_dir_is_read_only_too() {
        if running_as_root() {
            return;
        }
        let canvas = tmp_canvas("ro-meshfox-dir");
        let dir = canvas.parent().unwrap().to_path_buf();
        std::fs::create_dir_all(dir.join(".meshfox")).unwrap();
        chmod(&dir.join(".meshfox"), 0o555);

        assert!(is_read_only(&canvas));
        match try_acquire(&canvas).unwrap() {
            Acquired::Us(guard) => assert!(guard.is_read_only()),
            Acquired::Other { .. } => panic!("nothing can be holding a lock here"),
        }
        chmod(&dir.join(".meshfox"), 0o755);
        restore_and_remove(&dir);
    }

    #[test]
    fn a_writable_canvas_is_not_read_only_and_takes_a_real_lock() {
        let canvas = tmp_canvas("writable");
        assert!(!is_read_only(&canvas));
        match try_acquire(&canvas).unwrap() {
            Acquired::Us(guard) => assert!(!guard.is_read_only()),
            Acquired::Other { .. } => panic!("expected to win an uncontended lock"),
        }
        std::fs::remove_dir_all(canvas.parent().unwrap()).ok();
    }

    #[test]
    fn only_permission_and_read_only_filesystem_errors_mean_read_only() {
        assert!(is_read_only_error(&io::Error::from(io::ErrorKind::PermissionDenied)));
        assert!(is_read_only_error(&io::Error::from_raw_os_error(libc::EROFS)));
        assert!(!is_read_only_error(&io::Error::from_raw_os_error(libc::ENOSPC)));
        assert!(!is_read_only_error(&io::Error::from(io::ErrorKind::NotFound)));
    }
}
