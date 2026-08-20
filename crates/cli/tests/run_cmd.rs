//! End-to-end test proving `meshfox run` can address and run a block that
//! lives inside an `include` target — it used to be silently unreachable
//! (`run`/the TUI never resolved `include`s for execution at all, only the
//! web UI did), same limitation `crates/server/src/lib.rs`'s own
//! `run_block_include_tests` used to have before `resolved_canvas`/
//! `locate_node` were wired into it.

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn unique_dir() -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("meshfox-run-include-cmd-test-{nanos}-{n}"));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn meshfox() -> Command {
    Command::new(env!("CARGO_BIN_EXE_meshfox"))
}

#[test]
fn run_finds_caches_and_reports_the_cwd_of_a_block_inside_an_included_canvas() {
    let dir = unique_dir();
    std::fs::write(
        dir.join("child.canvas.md"),
        concat!(
            "<!-- meshfox:canvas -->\n# Child\n<!-- meshfox:node id=\"root\" -->\n\n",
            "## Leaf\n<!-- meshfox:node id=\"leaf\" -->\n\n",
            "```bash name=\"report\" cache\npwd -P\n```\n",
        ),
    )
    .unwrap();
    let base_path = dir.join("base.canvas.md");
    std::fs::write(
        &base_path,
        concat!(
            "<!-- meshfox:canvas -->\n# Base\n<!-- meshfox:node id=\"base\" -->\n\n",
            "## Child\n<!-- meshfox:node id=\"child\" type=\"include\" -->\n\n[child](./child.canvas.md)\n",
        ),
    )
    .unwrap();
    let child_path = dir.join("child.canvas.md");

    let output = meshfox()
        .arg("run")
        .arg("--canvas")
        .arg(&base_path)
        .arg("child")
        .arg("child/root")
        .arg("child/leaf")
        .arg("report")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    // Ran with the *included* file's own directory as `PWD` — same
    // directory `child.canvas.md` itself lives in.
    let want_cwd = dir.canonicalize().unwrap();
    assert!(
        stdout.contains(&want_cwd.to_string_lossy().into_owned()),
        "stdout: {stdout}"
    );

    // Cache landed in `child.canvas.md`, addressed there by its own local
    // id `leaf` — the primary document is untouched.
    let base_after = std::fs::read_to_string(&base_path).unwrap();
    assert!(!base_after.contains("meshfox:output"));
    let child_after = std::fs::read_to_string(&child_path).unwrap();
    assert!(child_after.contains("meshfox:output name=\"report\""));

    let _ = std::fs::remove_dir_all(&dir);
}

/// A variable only a *later* step in the chain references (mirrors a
/// `PGPASSWORD` only `migrate`/`load` need, at the tail of a long
/// download→extract→merge→...→load chain) must be asked for — or, as here
/// in a non-interactive test process, fail loudly — *before* any earlier
/// step in the same chain runs at all, not only once execution actually
/// reaches that step. Without `preflight_chain_vars` (see `main.rs`), the
/// earlier `dep` step would run first (leaving its own marker file behind)
/// and only `target` itself would fail on the missing variable — this test
/// fails the same way that regression would, by checking the marker file
/// was never written.
#[test]
fn a_variable_only_a_later_step_needs_is_checked_before_any_earlier_step_runs() {
    let dir = unique_dir();
    let marker = dir.join("dep-ran.marker");
    let canvas_path = dir.join("base.canvas.md");
    std::fs::write(
        &canvas_path,
        format!(
            concat!(
                "<!-- meshfox:canvas -->\n# Base\n<!-- meshfox:node id=\"base\" -->\n\n",
                "<!-- meshfox:var name=\"LATE_VAR\" -->\n\n",
                "```bash name=\"dep\" cache\ntouch {marker:?}\n```\n\n",
                "```bash name=\"target\" deps=\"dep\" env=\"$LATE_VAR\"\necho \"$LATE_VAR\"\n```\n",
            ),
            marker = marker.to_string_lossy(),
        ),
    )
    .unwrap();

    let output = meshfox()
        .arg("run")
        .arg("--canvas")
        .arg(&canvas_path)
        .arg("target")
        .output()
        .unwrap();
    assert!(
        !output.status.success(),
        "expected failure (no tty to prompt LATE_VAR on, stdout: {}, stderr: {})",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("LATE_VAR"), "stderr: {stderr}");
    assert!(
        !marker.exists(),
        "dep must never have run — the whole chain's variables are checked before any step does"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// A runnable `file` node (`type="file" interpreter="..."`) used to only
/// be runnable from the web UI's own "▷ run" button — `meshfox run`
/// treated it as just an unaddressable link, same as `display`/`code`.
#[test]
fn run_runs_a_file_node_in_the_primary_document() {
    let dir = unique_dir();
    std::fs::write(dir.join("seed.sh"), "#!/bin/sh\necho hi from seed\n").unwrap();
    let canvas_path = dir.join("base.canvas.md");
    std::fs::write(
        &canvas_path,
        concat!(
            "<!-- meshfox:canvas -->\n# Base\n<!-- meshfox:node id=\"base\" -->\n\n",
            "## Seed\n<!-- meshfox:node id=\"seed\" type=\"file\" interpreter=\"bash\" -->\n\n",
            "[seed](./seed.sh)\n",
        ),
    )
    .unwrap();

    let output = meshfox()
        .arg("run")
        .arg("--canvas")
        .arg(&canvas_path)
        .arg("seed")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("hi from seed"));

    let _ = std::fs::remove_dir_all(&dir);
}

/// Same as the fenced-block include test above, but for a runnable `file`
/// node living inside the `include` target — its `target`/`PWD` must
/// resolve relative to `child.canvas.md`'s own directory, not the primary
/// document's, and it must be reachable at all (namespaced `child/seed`).
#[test]
fn run_runs_a_file_node_inside_an_included_canvas() {
    let dir = unique_dir();
    std::fs::write(dir.join("seed.sh"), "#!/bin/sh\npwd -P\n").unwrap();
    std::fs::write(
        dir.join("child.canvas.md"),
        concat!(
            "<!-- meshfox:canvas -->\n# Child\n<!-- meshfox:node id=\"root\" -->\n\n",
            "## Seed\n<!-- meshfox:node id=\"seed\" type=\"file\" interpreter=\"bash\" -->\n\n",
            "[seed](./seed.sh)\n",
        ),
    )
    .unwrap();
    let base_path = dir.join("base.canvas.md");
    std::fs::write(
        &base_path,
        concat!(
            "<!-- meshfox:canvas -->\n# Base\n<!-- meshfox:node id=\"base\" -->\n\n",
            "## Child\n<!-- meshfox:node id=\"child\" type=\"include\" -->\n\n[child](./child.canvas.md)\n",
        ),
    )
    .unwrap();

    let output = meshfox()
        .arg("run")
        .arg("--canvas")
        .arg(&base_path)
        .arg("child")
        .arg("child/root")
        .arg("child/seed")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let want_cwd = dir.canonicalize().unwrap();
    assert!(
        stdout.contains(&want_cwd.to_string_lossy().into_owned()),
        "stdout: {stdout}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
