//! Small canvas-fixture builders — same convention `crates/cli/tests/run_cmd.rs`
//! already uses (no `tempfile` crate, a fresh `std::env::temp_dir()`
//! subdirectory named with a nanosecond timestamp + atomic counter for
//! uniqueness), so this suite doesn't introduce a second way to do the
//! same thing.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn unique_dir() -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("meshfox-tui-e2e-{nanos}-{n}"));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Writes `body` to `<fresh temp dir>/canvas.canvas.md` and returns
/// `(canvas_path, fixture_dir)` — pass `fixture_dir` straight to
/// `TuiSession::spawn`, which removes it on `Drop`.
pub fn write_fixture(body: &str) -> (PathBuf, PathBuf) {
    let dir = unique_dir();
    let path = dir.join("canvas.canvas.md");
    std::fs::write(&path, body).unwrap();
    (path, dir)
}

/// Root with one child node holding a single `cache`d block that prints a
/// fixed, greppable line — the smallest fixture that can prove "the real
/// event loop starts, renders, and can run a block" (`baseline.rs`).
pub const SIMPLE_RUNNABLE: &str = concat!(
    "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
    "## Leaf\n<!-- meshfox:node id=\"leaf\" parent=\"root\" -->\n\n",
    "```bash name=\"leaf\" cache\necho tui-e2e-marker-output\n```\n",
);

/// Root's own sole block is a `service` — **experimental**, see SPEC.md's
/// "Service blocks (experimental)". `echo` first so the process is
/// confirmed up (its own line reaches stdout) before the long `sleep`,
/// same shape `crates/cli/tests/service_run_cmd.rs` already uses. Used by
/// `services.rs`.
pub const SERVICE_RUNNABLE: &str = concat!(
    "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
    "```bash name=\"srv\" service\necho tui-e2e-service-marker\nsleep 30\n```\n",
);

/// A `button` fence (see SPEC.md's "Button fences") over one block — the
/// one real "run"-looking clickable affordance the document pane renders
/// today (`▶ Go (r to run)`, `markdown.rs`'s `BUTTON_LANG` branch);
/// everything else runs from the tree's own `r`/`R`, with no inline
/// button at all. Deliberately no `cache` (see `WIDE_OUTPUT_LINE`'s own
/// comment) and the marker text built from fragments so it never appears
/// contiguously in the fixture's own source, only in real output — same
/// reasoning as `WIDE_OUTPUT_LINE`. Used by `mouse_run_buttons.rs`.
pub const BUTTON_FENCE: &str = concat!(
    "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
    "```bash name=\"root\"\na='tui-e2e-button'; b='marker'\necho \"${a}-${b}\"\n```\n\n",
    "```button deps=\"root\"\nGo\n```\n",
);

/// Two sibling nodes: "Producer" owns a `cache`d block, "Consumer" has one
/// whose `env=` references a `from=`-computed variable sourced from it —
/// an implicit dependency, so its own deps line reads
/// `via RESOURCE: producer/make` (see `markdown.rs`'s `dep_line`). Used by
/// `mouse_deps_line.rs` to click the block-name text and jump to
/// "Producer".
pub const DEPS_LINE_JUMP: &str = concat!(
    "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
    "<!-- meshfox:var name=\"RESOURCE\" from=\"producer/make\" -->\n\n",
    "## Producer\n<!-- meshfox:node id=\"producer\" parent=\"root\" -->\n\n",
    "```bash name=\"make\" cache\necho id=abc123 >> \"$MESHFOX_VARS_OUT\"\n```\n\n",
    "## Consumer\n<!-- meshfox:node id=\"consumer\" parent=\"root\" -->\n\n",
    "```bash name=\"consumer\" env=\"$RESOURCE\" cache\necho \"resource is $RESOURCE\"\n```\n",
);

/// A node with two runnable blocks — the smallest fixture that makes `r`
/// open the block picker (`App::trigger_run`'s own `blocks.len() == 1`
/// short-circuit only skips it for a single block). Used by
/// `mouse_modals.rs`.
pub const TWO_BLOCKS: &str = concat!(
    "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
    "```bash name=\"alpha\" cache\necho tui-e2e-alpha-output\n```\n\n",
    "```bash name=\"beta\" cache\necho tui-e2e-beta-output\n```\n",
);

/// A block whose *output* (not source — the document pane already wraps a
/// long source line, so that alone can't demonstrate a horizontal-scroll
/// gap) is one line far wider than any reasonable terminal. Built from
/// separate fragments at runtime so neither `line-start-marker` nor
/// `line-end-marker` appears contiguously anywhere in the fixture's own
/// source text — only in the real, concatenated run output. Deliberately
/// no `cache` — a cached run's output gets written back and re-rendered
/// *inside the document pane too* (its own cached-output block wraps,
/// unlike the live Output pane below), which would make `line-end-marker`
/// show up from the wrong place entirely. For `mouse_horizontal_scroll.rs`.
pub const WIDE_OUTPUT_LINE: &str = concat!(
    "<!-- meshfox:canvas -->\n# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
    "```bash name=\"root\"\n",
    "a='line-start'; b='marker'; mid=$(printf '0%.0s' {1..100}); c='line-end'; d='marker'\n",
    "echo \"${a}-${b}-${mid}-${c}-${d}\"\n",
    "```\n",
);
