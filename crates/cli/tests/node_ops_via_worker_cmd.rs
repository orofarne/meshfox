//! Regression coverage for the data-race fix that routes `node
//! add`/`meta`/`rename`/`set-id`/`edges`/`mv`/`move`/`reorder`/`block`
//! through `crate::coordinator::discover` + `crate::worker_client` whenever
//! a worker (a `view`/`tui`/`server_socket` daemon with the same file
//! already open in its own in-memory `state.raw`) is already running for
//! the canvas, instead of always doing a direct file read-modify-write that
//! could silently clobber — or be clobbered by — that worker's own next
//! save. Before this fix, only `node body`/`node rm` did this; every op
//! covered here used to race the worker unconditionally.
//!
//! Each test spawns a bare worker via `--watcher-socket` (a fake,
//! never-read path — the same convention `service_shutdown_cmd.rs`/
//! `run_cmd.rs`'s own `run_via_an_already_running_worker_still_runs_a_file_node`
//! already established for "a worker, not a whole watcher+worker process
//! tree"), polls its `worker_lock` file for a reported port (proving a
//! *live* worker exists, not just that the process has started), runs the
//! op, and checks two things: the CLI's own "via the running worker on port
//! ..." message (proof it actually routed through the worker rather than
//! silently falling back to direct-file editing), and the on-disk result
//! after the worker is killed (proof the change was actually persisted,
//! not just accepted in memory).

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn unique_dir() -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("meshfox-node-ops-worker-cmd-test-{nanos}-{n}"));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn meshfox() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_meshfox"));
    // Isolates this process from whatever the *real* machine's
    // `~/.meshfox/config.toml` happens to declare — see `run_cmd.rs`'s own
    // `meshfox()` helper for the full rationale (a `server_socket` set for
    // someone's daily use would otherwise silently reroute every op here
    // through their real external coordinator instead of the bare worker
    // this suite spawns itself).
    cmd.env("HOME", unique_dir());
    cmd
}

/// Spawns a bare worker for `canvas_path` and blocks until its
/// `worker_lock` file reports a port — the same discovery file
/// `coordinator::resolve`'s own fallback reads. Returns the child process
/// (kill it when done) — the caller owns `dir` and cleans it up itself.
fn spawn_worker(dir: &Path, canvas_path: &Path) -> std::process::Child {
    let worker = meshfox()
        .arg("view")
        .arg(canvas_path)
        .arg("--port")
        .arg("0")
        .arg("--watcher-socket")
        .arg(dir.join("fake.sock"))
        .stdout(std::process::Stdio::null())
        .spawn()
        .unwrap();

    let lock_name = format!("{}.worker.lock", canvas_path.file_name().unwrap().to_string_lossy());
    let lock_path = dir.join(".meshfox").join(lock_name);
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
    worker
}

fn kill(mut worker: std::process::Child) {
    let _ = worker.kill();
    let _ = worker.wait();
}

#[test]
fn node_add_routes_through_a_running_worker() {
    let dir = unique_dir();
    let canvas_path = dir.join("base.canvas.md");
    std::fs::write(
        &canvas_path,
        "<!-- meshfox:canvas -->\n# Base\n<!-- meshfox:node id=\"root\" -->\n",
    )
    .unwrap();

    let worker = spawn_worker(&dir, &canvas_path);

    let output = meshfox()
        .arg("node")
        .arg("add")
        .arg("--canvas")
        .arg(&canvas_path)
        .arg("root")
        .arg("New Child")
        .arg("--x")
        .arg("10")
        .arg("--y")
        .arg("20")
        .output()
        .unwrap();

    kill(worker);

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("via the running worker on port"),
        "stdout: {stdout}"
    );

    let after = std::fs::read_to_string(&canvas_path).unwrap();
    assert!(after.contains("New Child"), "after: {after}");
    assert!(after.contains("x=10 y=20"), "after: {after}");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn node_meta_routes_through_a_running_worker() {
    let dir = unique_dir();
    let canvas_path = dir.join("base.canvas.md");
    std::fs::write(
        &canvas_path,
        concat!(
            "<!-- meshfox:canvas -->\n# Base\n<!-- meshfox:node id=\"root\" -->\n\n",
            "## Child\n<!-- meshfox:node id=\"child\" -->\n",
        ),
    )
    .unwrap();

    let worker = spawn_worker(&dir, &canvas_path);

    let output = meshfox()
        .arg("node")
        .arg("meta")
        .arg("--canvas")
        .arg(&canvas_path)
        .arg("child")
        .arg("--x")
        .arg("5")
        .arg("--y")
        .arg("7")
        .arg("--color")
        .arg("#ff0000")
        .output()
        .unwrap();

    kill(worker);

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("via the running worker on port"),
        "stdout: {stdout}"
    );

    let after = std::fs::read_to_string(&canvas_path).unwrap();
    assert!(after.contains("x=5 y=7"), "after: {after}");
    assert!(after.contains("color=\"#ff0000\""), "after: {after}");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn node_rename_routes_through_a_running_worker() {
    let dir = unique_dir();
    let canvas_path = dir.join("base.canvas.md");
    std::fs::write(
        &canvas_path,
        concat!(
            "<!-- meshfox:canvas -->\n# Base\n<!-- meshfox:node id=\"root\" -->\n\n",
            "## Old Title\n<!-- meshfox:node id=\"child\" -->\n",
        ),
    )
    .unwrap();

    let worker = spawn_worker(&dir, &canvas_path);

    let output = meshfox()
        .arg("node")
        .arg("rename")
        .arg("--canvas")
        .arg(&canvas_path)
        .arg("child")
        .arg("New Title")
        .output()
        .unwrap();

    kill(worker);

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("via the running worker on port"),
        "stdout: {stdout}"
    );

    let after = std::fs::read_to_string(&canvas_path).unwrap();
    assert!(after.contains("## New Title"), "after: {after}");
    assert!(!after.contains("Old Title"), "after: {after}");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn node_set_id_routes_through_a_running_worker() {
    let dir = unique_dir();
    let canvas_path = dir.join("base.canvas.md");
    std::fs::write(
        &canvas_path,
        concat!(
            "<!-- meshfox:canvas -->\n# Base\n<!-- meshfox:node id=\"root\" -->\n\n",
            "## Child\n<!-- meshfox:node id=\"old-id\" -->\n",
        ),
    )
    .unwrap();

    let worker = spawn_worker(&dir, &canvas_path);

    let output = meshfox()
        .arg("node")
        .arg("set-id")
        .arg("--canvas")
        .arg(&canvas_path)
        .arg("old-id")
        .arg("new-id")
        .output()
        .unwrap();

    kill(worker);

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("via the running worker on port"),
        "stdout: {stdout}"
    );

    let after = std::fs::read_to_string(&canvas_path).unwrap();
    assert!(after.contains("id=\"new-id\""), "after: {after}");
    assert!(!after.contains("old-id"), "after: {after}");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn node_edges_routes_through_a_running_worker() {
    let dir = unique_dir();
    let canvas_path = dir.join("base.canvas.md");
    std::fs::write(
        &canvas_path,
        concat!(
            "<!-- meshfox:canvas -->\n# Base\n<!-- meshfox:node id=\"root\" -->\n\n",
            "## A\n<!-- meshfox:node id=\"a\" -->\n\n",
            "## B\n<!-- meshfox:node id=\"b\" -->\n",
        ),
    )
    .unwrap();

    let worker = spawn_worker(&dir, &canvas_path);

    let output = meshfox()
        .arg("node")
        .arg("edges")
        .arg("--canvas")
        .arg(&canvas_path)
        .arg("b")
        .arg("--from")
        .arg("a")
        .output()
        .unwrap();

    kill(worker);

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("via the running worker on port"),
        "stdout: {stdout}"
    );

    let after = std::fs::read_to_string(&canvas_path).unwrap();
    assert!(
        after.contains("meshfox:edge from=\"a\""),
        "after: {after}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn node_mv_routes_through_a_running_worker() {
    let dir = unique_dir();
    let canvas_path = dir.join("base.canvas.md");
    std::fs::write(
        &canvas_path,
        concat!(
            "<!-- meshfox:canvas -->\n# Base\n<!-- meshfox:node id=\"root\" -->\n\n",
            "## Parent One\n<!-- meshfox:node id=\"p1\" -->\n\n",
            "### Child\n<!-- meshfox:node id=\"child\" -->\n\n",
            "## Parent Two\n<!-- meshfox:node id=\"p2\" -->\n",
        ),
    )
    .unwrap();

    let worker = spawn_worker(&dir, &canvas_path);

    let output = meshfox()
        .arg("node")
        .arg("mv")
        .arg("--canvas")
        .arg(&canvas_path)
        .arg("child")
        .arg("p2")
        .output()
        .unwrap();

    kill(worker);

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("via the running worker on port"),
        "stdout: {stdout}"
    );

    // Verified against the persisted file via a plain read-only `node
    // show`, not a raw grep — `child`'s structural parent is expressed by
    // heading nesting/order, not a single attribute this test could search
    // for directly.
    let show = meshfox()
        .arg("node")
        .arg("show")
        .arg("--canvas")
        .arg(&canvas_path)
        .arg("child")
        .output()
        .unwrap();
    assert!(show.status.success());
    let show_stdout = String::from_utf8_lossy(&show.stdout);
    assert!(
        show_stdout.contains("parent: p2"),
        "node show child: {show_stdout}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn node_move_routes_through_a_running_worker() {
    let dir = unique_dir();
    let canvas_path = dir.join("base.canvas.md");
    std::fs::write(
        &canvas_path,
        concat!(
            "<!-- meshfox:canvas -->\n# Base\n<!-- meshfox:node id=\"root\" -->\n\n",
            "## A\n<!-- meshfox:node id=\"a\" -->\n\n",
            "## B\n<!-- meshfox:node id=\"b\" -->\n\n",
            "## C\n<!-- meshfox:node id=\"c\" -->\n",
        ),
    )
    .unwrap();

    let worker = spawn_worker(&dir, &canvas_path);

    let output = meshfox()
        .arg("node")
        .arg("move")
        .arg("--canvas")
        .arg(&canvas_path)
        .arg("c")
        .arg("--before")
        .arg("a")
        .output()
        .unwrap();

    kill(worker);

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("via the running worker on port"),
        "stdout: {stdout}"
    );

    let after = std::fs::read_to_string(&canvas_path).unwrap();
    let pos_c = after.find("## C").expect("C heading present");
    let pos_a = after.find("## A").expect("A heading present");
    assert!(pos_c < pos_a, "expected C to now sit before A: {after}");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn node_reorder_routes_through_a_running_worker() {
    let dir = unique_dir();
    let canvas_path = dir.join("base.canvas.md");
    // On-disk heading order is B then A, but A's `y` sits above B's — a
    // resync should flip the heading order to match layout.
    std::fs::write(
        &canvas_path,
        concat!(
            "<!-- meshfox:canvas -->\n# Base\n<!-- meshfox:node id=\"root\" -->\n\n",
            "## B\n<!-- meshfox:node id=\"b\" x=0 y=0 -->\n\n",
            "## A\n<!-- meshfox:node id=\"a\" x=0 y=-100 -->\n",
        ),
    )
    .unwrap();

    let worker = spawn_worker(&dir, &canvas_path);

    let output = meshfox()
        .arg("node")
        .arg("reorder")
        .arg("--canvas")
        .arg(&canvas_path)
        .output()
        .unwrap();

    kill(worker);

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("via the running worker on port"),
        "stdout: {stdout}"
    );

    let after = std::fs::read_to_string(&canvas_path).unwrap();
    let pos_a = after.find("## A").expect("A heading present");
    let pos_b = after.find("## B").expect("B heading present");
    assert!(
        pos_a < pos_b,
        "expected A (y=-100) to now sit before B (y=0): {after}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn node_block_routes_through_a_running_worker() {
    let dir = unique_dir();
    let canvas_path = dir.join("base.canvas.md");
    std::fs::write(
        &canvas_path,
        concat!(
            "<!-- meshfox:canvas -->\n# Base\n<!-- meshfox:node id=\"root\" -->\n\n",
            "```bash name=\"hi\"\necho hi\n```\n",
        ),
    )
    .unwrap();

    let worker = spawn_worker(&dir, &canvas_path);

    let output = meshfox()
        .arg("node")
        .arg("block")
        .arg("--canvas")
        .arg(&canvas_path)
        .arg("root")
        .arg("hi")
        .arg("--cache")
        .output()
        .unwrap();

    kill(worker);

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("via the running worker on port"),
        "stdout: {stdout}"
    );

    let after = std::fs::read_to_string(&canvas_path).unwrap();
    assert!(
        after.contains("```bash name=\"hi\" cache"),
        "after: {after}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
