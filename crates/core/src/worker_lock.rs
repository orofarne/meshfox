//! Per-canvas discovery lock for a `meshfox view` worker — one small file
//! next to the canvas (`.meshfox/<file>.worker.lock`, same colocation
//! convention as `crate::service_lock`/`crate::varcache`), held via a real
//! `flock` rather than a pid file + liveness check.
//!
//! `crate::service_lock` records a pid and checks `kill(pid, 0)` to tell a
//! live owner from a stale one — correct, but it needs a separate "is the
//! recorded pid still alive" step, which is its own (harmless but avoidable)
//! race when two callers notice the same stale lock at once. `flock`
//! sidesteps that class of race entirely for this use case: holding the
//! lock *is* being alive, full stop — the OS drops it automatically on any
//! exit of the holding process, including `SIGKILL`, with no cooperation
//! from that process required. So there's nothing here to distinguish
//! "held but stale" from "held" — only "held" or "free".
//!
//! Used to let independent `meshfox view <path>` invocations (separate
//! watcher processes — see `crate::service_lock`'s own module for the
//! unrelated per-`service`-block lock, and `crates/cli/src/watcher.rs` for
//! what a worker actually is) on the same canvas file discover and reuse
//! whichever one of them is already serving it, instead of each spawning
//! its own worker/server pair.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Where a canvas's worker-discovery lock lives — canonicalizes `canvas_path`
/// first so callers that reach the same file via different (relative,
/// symlinked, differently-`cwd`'d) spellings still agree on one path; falls
/// back to the path as given if canonicalization fails (e.g. the canvas
/// doesn't exist yet), same graceful-degradation posture
/// `service_lock::lock_path` doesn't need but this one does, since a worker
/// lock can be checked before the canvas is known to exist.
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
    dir.join(".meshfox").join(format!("{file_name}.worker.lock"))
}

/// Held for as long as this process wants to be *the* worker for a canvas —
/// dropping it (including implicitly, on process exit/crash) releases the
/// underlying `flock` immediately; there's no separate `release` function to
/// forget to call. Keep this alive for the worker's whole lifetime (e.g. a
/// local binding that outlives the serve loop), not just around the moment
/// the port is written.
pub struct LockGuard {
    file: File,
}

impl LockGuard {
    /// Records the port this worker actually bound (only known after
    /// `TcpListener::bind`, so this is necessarily a separate step from
    /// acquiring the lock itself) — overwrites any previous contents.
    pub fn write_port(&mut self, port: u16) -> io::Result<()> {
        self.file.set_len(0)?;
        self.file.seek(SeekFrom::Start(0))?;
        writeln!(self.file, "port={port}")?;
        self.file.flush()
    }
}

/// Result of contending for a canvas's worker lock.
pub enum Acquired {
    /// Nobody else holds it — caller is now the worker; bind, then
    /// [`LockGuard::write_port`].
    Us(LockGuard),
    /// Somebody else already holds it and has recorded their bound port —
    /// caller should not bind its own listener at all.
    Other { port: u16 },
}

/// Non-blocking: either wins the lock outright, or finds out who already
/// holds it and what port they're on. Never blocks waiting for the current
/// holder to release — there's nothing to wait for, a live holder is exactly
/// what makes this call resolve to `Other` instead of `Us`.
pub fn try_acquire(canvas_path: &Path) -> io::Result<Acquired> {
    let path = lock_path(canvas_path);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        // Explicit, not the default: this handle may end up on the losing
        // side of the `flock` below (`Acquired::Other`), in which case
        // truncating here would wipe the current holder's own `port=...`
        // line out from under it before we even know who won.
        .truncate(false)
        .open(&path)?;
    // SAFETY: `flock(2)` on a valid, open fd we exclusively own here;
    // `LOCK_EX | LOCK_NB` never blocks — it fails fast with `EWOULDBLOCK`
    // if another process already holds it.
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if rc == 0 {
        return Ok(Acquired::Us(LockGuard { file }));
    }
    let err = io::Error::last_os_error();
    if err.raw_os_error() != Some(libc::EWOULDBLOCK) {
        return Err(err);
    }
    Ok(Acquired::Other { port: read_port_with_retry(&path)? })
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
        let dir = std::env::temp_dir().join(format!("meshfox-worker-lock-test-path-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("doc.canvas.md");
        std::fs::write(&path, "# Doc\n").unwrap();
        let locked = lock_path(&path);
        assert_eq!(locked, dir.canonicalize().unwrap().join(".meshfox").join("doc.canvas.md.worker.lock"));
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
}
