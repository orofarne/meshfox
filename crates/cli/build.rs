//! Captures the current git commit (or release tag) and its date at build
//! time, so `meshfox --version` can report exactly what was built instead of
//! just the crate version, which stays "0.1.0" across releases.

use std::env;
use std::process::Command;

fn git_output(args: &[&str]) -> Option<String> {
    let output = Command::new("git").args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout).ok().map(|s| s.trim().to_string())
}

/// The tag this build was made from, if any: `GITHUB_REF_NAME` when the
/// GitHub Actions release workflow triggers on a `v*` tag push, or a local
/// `git describe` for tag builds done outside CI.
fn release_tag() -> Option<String> {
    if env::var("GITHUB_REF_TYPE").as_deref() == Ok("tag") {
        if let Ok(tag) = env::var("GITHUB_REF_NAME") {
            return Some(tag);
        }
    }
    git_output(&["describe", "--tags", "--exact-match", "HEAD"])
}

fn main() {
    let label = match release_tag() {
        Some(tag) => tag,
        None => {
            let commit = git_output(&["rev-parse", "--short=12", "HEAD"]).unwrap_or_else(|| "unknown".to_string());
            format!("commit {commit}")
        }
    };
    let date = git_output(&["log", "-1", "--format=%cd", "--date=short"]).unwrap_or_else(|| "unknown".to_string());

    println!("cargo:rustc-env=MESHFOX_VERSION_LABEL={label}");
    println!("cargo:rustc-env=MESHFOX_GIT_DATE={date}");
    // Re-run whenever HEAD or the tags move (checkout, commit, tag, merge,
    // ...), not on every build.
    println!("cargo:rerun-if-changed=../../.git/HEAD");
    println!("cargo:rerun-if-changed=../../.git/refs");
    println!("cargo:rerun-if-env-changed=GITHUB_REF_TYPE");
    println!("cargo:rerun-if-env-changed=GITHUB_REF_NAME");
}
