//! Where a canvas's custom syntax-highlighting grammars live — shared by
//! `meshfox-cli`'s TUI (`crates/cli/src/syntax_registry.rs`, which actually
//! loads them into a `syntect::parsing::SyntaxSet`) and `meshfox-server`'s
//! `/api/syntax` (which just lists/serves the raw files to the browser).
//! Neither directory's existence is required — both are simply absent for
//! a project/user that hasn't dropped any custom grammars in yet.
//!
//! A `tui/` subdirectory under each one is a *TUI-only override* — see
//! `tui_syntax_dir`'s own doc comment for why this exists at all (some
//! real, unmodified grammars simply can't load into `syntect`, but a
//! same-named file rewritten for `syntect`'s own engine can). Deliberately
//! a subdirectory, not a separate top-level one: `meshfox-server`'s own
//! directory listing (`list_grammar_files`) never recurses, so a `tui/`
//! override is automatically invisible to `/api/syntax` — the browser
//! keeps seeing (and Shiki/Monaco keep using) only the original file, with
//! no extra filtering code needed to keep the override from leaking there.

use std::path::{Path, PathBuf};

/// `~/.meshfox/syntax` — global, every project. `None` isn't an error, just
/// "no global grammars this run" (e.g. an unusual environment with no
/// `HOME` set).
pub fn global_syntax_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|home| Path::new(&home).join(".meshfox").join("syntax"))
}

/// `<canvas_root>/.meshfox/syntax` — local to one project, same `.meshfox/`
/// directory `crate::varcache` already colocates with the canvas file,
/// a sibling `syntax/` subdirectory instead of the var-cache's own
/// `<filename>.env`.
pub fn local_syntax_dir(canvas_root: &Path) -> PathBuf {
    canvas_root.join(".meshfox").join("syntax")
}

/// `<syntax_dir>/tui` — a **TUI-only override** for a same-named grammar in
/// `syntax_dir` itself. Some real, unmodified `.tmLanguage.json` grammars
/// (Markdown, AsciiDoc, and — more lightly — YAML, confirmed by grepping
/// their actual upstream files) use TextMate's `begin`/`while` construct,
/// which `syntect`'s own matching engine has no equivalent for at all —
/// `syntect-tmlanguage` can't translate them, full stop, regardless of
/// `include`s or injections (see TODO.canvas.md's "Единая база языков
/// подсветки" for the investigation). Shiki/Monaco (`vscode-textmate` in
/// the browser) don't share this limitation, so the *shared* file at
/// `syntax_dir/<name>` keeps working there unmodified.
///
/// A `tui/` override is for exactly that gap: the same conceptual grammar,
/// hand-rewritten to use only constructs `syntect` actually supports
/// (typically `include`-ing `syntect`'s own already-loaded, independently-
/// authored default for the same language — see
/// `crates/cli/src/grammars/meshfox.tmLanguage.json` for the built-in
/// example, which does exactly this for Markdown). Loaded *after* the
/// plain shared grammars in `syntax_registry::build_syntax_set`, so it
/// wins the TUI's own name/scope resolution — the web side never reads
/// this directory at all (`meshfox-server`'s directory listing doesn't
/// recurse), so it can't diverge from what the browser shows regardless.
pub fn tui_syntax_dir(syntax_dir: &Path) -> PathBuf {
    syntax_dir.join("tui")
}
