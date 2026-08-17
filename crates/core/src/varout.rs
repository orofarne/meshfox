//! The `from=` output-file contract: how a `meshfox:var`'s computed value
//! actually gets out of the block that produces it.
//!
//! A process's own environment dies with it — that's an OS-level fact, not
//! something meshfox can work around, and it holds regardless of what
//! language a future executor runs. So a `from=` source block is instead
//! handed the path to a fresh, empty file (via the `VARS_OUT_ENV` variable
//! in its own process environment) and is expected to write `NAME=value`
//! lines to it before exiting — the same contract CI systems facing this
//! exact problem (GitHub Actions' `$GITHUB_OUTPUT`, etc.) converged on, and
//! for the same reason: it needs zero per-language code in the tool driving
//! it. Whoever runs a block (`crates/cli`, `crates/server`) is responsible
//! for allocating this file, injecting the env var only when the block
//! being run is actually a `from=` target, and reading + deleting it after
//! a successful (`exit == 0`) run.

use crate::deps::BlockAddr;
use crate::vars::VarDecl;
use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// The env var name a `from=` source block's process finds its own output
/// file's path under.
pub const VARS_OUT_ENV: &str = "MESHFOX_VARS_OUT";

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// A path for a fresh vars-out file, unique enough that two blocks running
/// concurrently (e.g. the web UI serving two runs at once) never collide —
/// process id plus a monotonic in-process counter, since two allocations
/// inside the same process are otherwise indistinguishable by time alone.
/// Nothing is created on disk yet; the source block may or may not ever
/// write to this path at all (see `read_and_cleanup`).
pub fn allocate_path() -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("meshfox-vars-{}-{n}.env", std::process::id()))
}

/// Reads back whatever a `from=` source block wrote to `path` (its own
/// `VARS_OUT_ENV`), parsing it the same `KEY=value`-per-line way
/// `crate::varcache` parses its own cache file — then removes the file.
/// A path the block never wrote to at all (it produced no output the
/// caller cares about, or wrote nothing) reads as an empty map, not an
/// error; deletion is best-effort (a file that's already gone, e.g. the
/// block deleted it itself, isn't a failure either).
pub fn read_and_cleanup(path: &Path) -> io::Result<HashMap<String, String>> {
    let values = match std::fs::read_to_string(path) {
        Ok(contents) => crate::dotenv::parse(&contents),
        Err(e) if e.kind() == io::ErrorKind::NotFound => HashMap::new(),
        Err(e) => return Err(e),
    };
    let _ = std::fs::remove_file(path);
    Ok(values)
}

/// Every declared variable in `decls` whose `from=` names `addr` — what a
/// runner checks right before spawning a block (to decide whether it needs
/// a `VARS_OUT_ENV` at all) and right after a successful run (to know which
/// names in the output file are actually expected). `VarDecl::from` is
/// always fully qualified by the time it reaches here (see
/// `vars::declared_vars`'s bare-name normalization), so this is a plain
/// equality check, not a resolution.
pub fn from_targets<'a>(decls: &'a [VarDecl], addr: &BlockAddr) -> Vec<&'a VarDecl> {
    decls
        .iter()
        .filter(|d| {
            d.from.as_ref().is_some_and(|r| {
                r.node_id.as_deref() == Some(addr.node_id.as_str())
                    && r.block_name == addr.block_name
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allocate_path_returns_distinct_paths() {
        let a = allocate_path();
        let b = allocate_path();
        assert_ne!(a, b);
    }

    #[test]
    fn read_and_cleanup_round_trips_and_deletes() {
        let path = allocate_path();
        std::fs::write(&path, "A=1\nB=hello\n").unwrap();
        let values = read_and_cleanup(&path).unwrap();
        assert_eq!(values.get("A").map(String::as_str), Some("1"));
        assert_eq!(values.get("B").map(String::as_str), Some("hello"));
        assert!(!path.exists());
    }

    #[test]
    fn read_and_cleanup_treats_a_missing_file_as_empty() {
        let path = allocate_path();
        let values = read_and_cleanup(&path).unwrap();
        assert!(values.is_empty());
    }
}
