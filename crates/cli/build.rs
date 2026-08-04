//! Captures the current git commit and its date at build time, so `meshfox
//! --version` can report exactly what was built instead of just the crate
//! version, which stays "0.1.0" across releases.

use std::process::Command;

fn git_output(args: &[&str]) -> Option<String> {
    let output = Command::new("git").args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout).ok().map(|s| s.trim().to_string())
}

fn main() {
    let commit = git_output(&["rev-parse", "--short=12", "HEAD"]).unwrap_or_else(|| "unknown".to_string());
    let date = git_output(&["log", "-1", "--format=%cd", "--date=short"]).unwrap_or_else(|| "unknown".to_string());

    println!("cargo:rustc-env=MESHFOX_GIT_COMMIT={commit}");
    println!("cargo:rustc-env=MESHFOX_GIT_DATE={date}");
    // Re-run whenever HEAD moves to a different commit (checkout, commit,
    // merge, ...), not on every build.
    println!("cargo:rerun-if-changed=../../.git/HEAD");
    println!("cargo:rerun-if-changed=../../.git/refs");
}
