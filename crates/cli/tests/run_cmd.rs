//! End-to-end test proving `meshfox run` can address and run a block that
//! lives inside an `include` node's own dumped-in body — it used to be
//! silently unreachable (`run`/the TUI never resolved `include`s for
//! execution at all, only the web UI did), same limitation
//! `crates/server/src/lib.rs`'s own chain-execution loops used to have
//! before they resolved includes for each step's own text/cwd.

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
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_meshfox"));
    // Isolates this process from whatever the *real* machine's
    // `~/.meshfox/config.toml` happens to declare — `meshfox_core::config`
    // reads `$HOME/.meshfox/config.toml` unconditionally, so a developer
    // who has (say) `server_socket` set for their own daily use would
    // otherwise have every `run` here silently routed through *their*
    // real external coordinator instead of exercising the in-process path
    // this suite means to test (confirmed live: this exact leak broke
    // this file's own file-node tests once `server_socket` was set on the
    // machine this was developed on). `unique_dir()`'s own directory
    // already has no `.meshfox/config.toml` of its own, so pointing `HOME`
    // there too gets both "no global config" and "no local config" in one
    // change, without needing a real home directory to exist at all.
    cmd.env("HOME", unique_dir());
    cmd
}

#[test]
fn run_finds_and_reports_the_cwd_of_a_block_inside_an_included_node_without_caching() {
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

    // `child.canvas.md`'s own content (headings and all) is dumped
    // verbatim into the `child` node's own body — no separate `child/root`/
    // `child/leaf` nodes exist, so `report` is addressed directly under
    // `child`, the include node's own id.
    let output = meshfox()
        .arg("run")
        .arg("--canvas")
        .arg(&base_path)
        .arg("child")
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

    // Never cached, even with `cache` set on the fence: the include node's
    // real on-disk body is just the bare link — there's nowhere to write a
    // `meshfox:output` comment without clobbering it, so this run streams
    // its output live and leaves the file untouched, same as it would for
    // any other body edit attempted on this node (see
    // `update_node_on_a_plain_markdown_include_rejects_a_text_edit_with_a_clear_reason`
    // in `crates/server/src/lib.rs`).
    let base_after = std::fs::read_to_string(&base_path).unwrap();
    assert!(!base_after.contains("meshfox:output"));

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

/// Regression test for a real bug this suite itself caught: once a live
/// worker exists for a canvas (discovered via `crate::coordinator::resolve`
/// — a local `view`/`tui` session's own `worker_lock` flock, exactly like
/// here, or an externally-configured `server_socket`), `meshfox run`
/// routes through `run_via_worker` instead of running in-process. An
/// earlier version of that function only knew how to run a fenced-block
/// chain (`worker_client::run_stream_persisted`) — addressing a runnable
/// `file` node while a worker was live failed outright ("no runnable code
/// block named ..."), even though the exact same address ran fine with no
/// worker around at all. Fixed by having `run_via_worker` fall back to
/// `worker_client::run_file_node_stream` (`GET /api/nodes/:id/run`) the
/// same way the in-process loop already falls back to `run_file_node_cli`.
/// Spawns a worker directly via `--watcher-socket` (a fake, never-read
/// path — same convention `service_shutdown_cmd.rs` already established
/// for "a worker, not a whole watcher+worker process tree"), not a bare
/// `meshfox view`, which would spawn an unrelated watcher/worker pair of
/// its own.
#[test]
fn run_via_an_already_running_worker_still_runs_a_file_node() {
    let dir = unique_dir();
    std::fs::write(dir.join("seed.sh"), "#!/bin/sh\necho hi from worker-routed seed\n").unwrap();
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

    let mut worker = meshfox()
        .arg("view")
        .arg(&canvas_path)
        .arg("--port")
        .arg("0")
        .arg("--watcher-socket")
        .arg(dir.join("fake.sock"))
        .stdout(std::process::Stdio::null())
        .spawn()
        .unwrap();

    // Poll the same discovery file `coordinator::resolve`'s own
    // `worker_lock` fallback reads, rather than a blind sleep — this is
    // exactly what proves a *live* worker exists for `run` to route
    // through, not just that the process has started.
    let lock_path = dir.join(".meshfox").join("base.canvas.md.worker.lock");
    let mut discovered = false;
    for _ in 0..100 {
        if std::fs::read_to_string(&lock_path)
            .map(|s| s.contains("port="))
            .unwrap_or(false)
        {
            discovered = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert!(discovered, "worker never reported a port within 5s");

    let output = meshfox()
        .arg("run")
        .arg("--canvas")
        .arg(&canvas_path)
        .arg("seed")
        .output()
        .unwrap();

    let _ = worker.kill();
    let _ = worker.wait();
    let _ = std::fs::remove_dir_all(&dir);

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("hi from worker-routed seed"));
}
