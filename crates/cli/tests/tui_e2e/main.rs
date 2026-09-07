//! End-to-end suite for the TUI (`crates/cli/src/tui/`) — spawns the real
//! compiled `meshfox` binary inside a real pty (`portable-pty`, the same
//! crate `crates/server/src/pty_exec.rs` already uses for `tty` blocks)
//! and drives it with real keystrokes/mouse escape sequences, asserting on
//! the real rendered screen (`vt100`). See `harness::TuiSession`.
//!
//! Deliberately separate from `cargo test --workspace`'s default run: every
//! test here is `#[ignore]` (Cargo has no other way to exclude one
//! integration-test target from the default run) — run explicitly via
//! `cargo test --test tui_e2e -- --ignored`, same "not part of any CI
//! gate, run by hand" framing README.md's "VS Code end-to-end tests"
//! section already uses for `editors/vscode/e2e/`. See README.md's own
//! "TUI end-to-end tests" section for the full rationale.
//!
//! Every mouse-support test here mirrors one checklist item in
//! TODO.canvas.md's "Мышь в панелях TUI (tree/document/output)" — each was
//! written first, failing (red), against a feature that didn't exist yet;
//! implementing the feature and ticking its TODO box happened together
//! with greening its test (its own `#[ignore]` reason then changes to the
//! ordinary "pty-based e2e" one below — every test here stays `#[ignore]`d
//! regardless, same as `mouse_output_pane`'s already were).

mod baseline;
mod fixtures;
mod harness;
mod mouse_deps_line;
mod mouse_drag_dblclick;
mod mouse_horizontal_scroll;
mod mouse_modals;
mod mouse_output_pane;
mod mouse_resize;
mod mouse_run_buttons;
