//! `meshfox prune`: lists — and with `--yes` removes — the regenerable files
//! meshfox leaves behind (see `meshfox_core::prune` for what counts and what
//! never does). Runs in this process like `meshfox secret prune`: it works
//! across every canvas in a directory, including ones that no longer exist
//! and so have no worker to ask.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use meshfox_core::prune::{self, Candidate, Scan};

pub fn run(root: PathBuf, recursive: bool, yes: bool) -> Result<(), String> {
    if !root.is_dir() {
        return Err(format!("{} isn't a directory", root.display()));
    }
    let report = prune::scan(&Scan {
        root: &root,
        recursive,
        temp_dir: &std::env::temp_dir(),
        now: SystemTime::now(),
    });

    for skipped in &report.skipped {
        println!(
            "skipped {} — {}",
            shown(&skipped.path, &root),
            skipped.reason
        );
    }
    if report.candidates.is_empty() {
        println!("nothing to prune");
        return Ok(());
    }

    let mut last = None;
    for candidate in &report.candidates {
        if last != Some(candidate.kind) {
            println!("\n{}:", candidate.kind.heading());
            last = Some(candidate.kind);
        }
        println!(
            "  {} ({}) — {}",
            shown(&candidate.path, &root),
            size(candidate.bytes),
            candidate.reason
        );
    }
    let total: u64 = report.candidates.iter().map(|c| c.bytes).sum();
    println!();

    if !yes {
        println!(
            "{} would be removed ({}) — run again with --yes to remove {}",
            count(&report.candidates),
            size(total),
            if report.candidates.len() == 1 {
                "it"
            } else {
                "them"
            }
        );
        println!("never touched: run history (.session.sqlite3) and variable caches (.env)");
        return Ok(());
    }

    let mut removed = 0;
    let mut freed = 0;
    let mut failed = Vec::new();
    for candidate in &report.candidates {
        match prune::remove(candidate) {
            Ok(()) => {
                removed += 1;
                freed += candidate.bytes;
            }
            Err(e) => failed.push(format!("{}: {e}", candidate.path.display())),
        }
    }
    println!(
        "removed {removed} of {} ({})",
        report.candidates.len(),
        size(freed)
    );
    if failed.is_empty() {
        Ok(())
    } else {
        Err(format!("couldn't remove:\n  {}", failed.join("\n  ")))
    }
}

fn count(candidates: &[Candidate]) -> String {
    let n = candidates.len();
    format!("{n} item{}", if n == 1 { "" } else { "s" })
}

/// `path` relative to `root` when it is inside it, otherwise as is.
fn shown(path: &Path, root: &Path) -> String {
    path.strip_prefix(root)
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| path.display().to_string())
}

fn size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KiB", "MiB", "GiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use meshfox_core::prune::Kind;

    #[test]
    fn sizes_are_human_readable() {
        assert_eq!(size(0), "0 B");
        assert_eq!(size(1023), "1023 B");
        assert_eq!(size(1536), "1.5 KiB");
        assert_eq!(size(5 * 1024 * 1024), "5.0 MiB");
    }

    #[test]
    fn paths_inside_the_root_are_shown_relative() {
        assert_eq!(
            shown(Path::new("/p/.meshfox/a.venv"), Path::new("/p")),
            ".meshfox/a.venv"
        );
        assert_eq!(shown(Path::new("/tmp/x"), Path::new("/p")), "/tmp/x");
    }

    #[test]
    fn kinds_have_headings() {
        for kind in [Kind::TangledFile, Kind::StateDirectory, Kind::StaleTempFile] {
            assert!(!kind.heading().is_empty());
        }
    }
}
