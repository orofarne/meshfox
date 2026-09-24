//! meshfox: the CLI and local web viewer/editor for a canvas.
//!
//! `meshfox run tests smoke-test smoke` walks the tree from the root
//! through nodes with id "tests" then "smoke-test", and runs the code
//! block named "smoke" inside that node. `meshfox view` starts the same
//! backend used by the browser UI (see `meshfox_server`), with the built
//! frontend embedded into this binary — one executable, no separate
//! server process to install or remember to start. The UI opens
//! read-only: dragging, resizing, and saving layout are locked behind an
//! explicit "Edit" button. Running a block is always allowed (that's the
//! whole point of a canvas) — Edit only controls whether a `cache`d
//! block's output actually gets written back into the file, or just shown.

use clap::{Args, Parser, Subcommand};
use meshfox_core::{
    mdcanvas, Canvas, ExtraEdge, FileDisplay, Node, NodeType, VarCache,
    VarDecl, VarType,
};
#[cfg(test)]
use meshfox_core::{FenceAttrsPatch, NodeMeta};
use std::collections::HashMap;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};

mod coordinator;
mod mcp;
mod pdf;
mod prompt;
mod syntax_registry;
mod tui;
mod watcher;
mod worker_client;

/// `commit <hash> (<date>)`, or `<tag> (<date>)` for a build made from a
/// release tag, captured at build time by `build.rs` from the repo `meshfox`
/// was actually compiled in — not the crate's Cargo.toml version, which
/// doesn't change between commits.
const VERSION: &str = concat!(
    env!("MESHFOX_VERSION_LABEL"),
    " (",
    env!("MESHFOX_GIT_DATE"),
    ")"
);

const MASCOT: &str = r"

 /\_/\
( ¬‿¬ )──●──●──●
  c c
";

#[derive(Parser)]
#[command(
    name = "meshfox",
    before_help = MASCOT,
    about = "CLI and local web viewer/editor for a meshfox canvas. Run `meshfox spec` for the full .canvas.md format specification.",
    after_help = "Website: https://meshfox.orofarne.net/\n\nAgent Usage:\n  If you are an AI coding agent, run `meshfox --agent-help` before hand-editing a\n  .canvas.md file. It covers when to prefer `meshfox node <verb>` over a raw text\n  edit, how to run non-interactively, and other guidance not covered above.",
    version = VERSION
)]
struct Cli {
    /// Print usage guidance for AI coding agents (when to prefer `node`
    /// subcommands over hand-editing, non-interactive `run`, etc.) and exit.
    #[arg(long)]
    agent_help: bool,

    #[command(subcommand)]
    command: Option<Command>,
}

/// A canvas path, accepted either as a positional argument or via
/// `--canvas` — the two are equivalent and mutually exclusive. Kept as a
/// shared, flattened block so every subcommand that has a positional slot
/// free for it (i.e. every one except `run` and `node <op>`, whose own
/// positional slot is taken by other arguments — see `splice_leading_canvas`)
/// accepts both spellings the same way.
#[derive(Args)]
struct CanvasOpt {
    /// Path to the .canvas.md file. If omitted: auto-discover the single
    /// candidate in the current directory.
    canvas: Option<PathBuf>,
    /// Same as the positional argument above, spelled as a flag — for
    /// parity with `run`/`node <op>`, which only accept this form.
    #[arg(long = "canvas", value_name = "CANVAS", conflicts_with = "canvas")]
    canvas_flag: Option<PathBuf>,
}

impl CanvasOpt {
    fn resolve(self) -> Option<PathBuf> {
        self.canvas.or(self.canvas_flag)
    }
}

#[derive(Subcommand)]
enum Command {
    /// Run one or more named code blocks.
    ///
    /// The last argument is a comma-separated list of block names; every
    /// argument before it is a node-id path segment leading to the node
    /// that owns them. If a node's own id is given in the block-name
    /// position instead, it resolves to that node's `default` block (a
    /// block explicitly flagged `default`, or one whose name already
    /// matches the node's own id) when it has exactly one — so a node
    /// that's really just "one thing to run" can be addressed without
    /// repeating its id as a trailing block name too. Auto-discovery is
    /// used when no canvas path is given; pass one before the subcommand
    /// instead of after it (`meshfox examples/hello.canvas.md run tests
    /// smoke-test smoke`), or with `--canvas`, to name one explicitly.
    Run {
        #[arg(required = true, num_args = 1..)]
        args: Vec<String>,
        /// Path to the .canvas.md file. If omitted: auto-discover the
        /// single candidate in the current directory.
        #[arg(long)]
        canvas: Option<PathBuf>,
        /// Skip each requested block's `deps=` chain and run only the
        /// blocks named on the command line, in the order given — the CLI
        /// equivalent of the web UI's plain "run" button (as opposed to
        /// "⛓ run chain").
        #[arg(long)]
        no_deps: bool,
        /// Supply a declared `meshfox:var`'s value directly (repeatable),
        /// skipping any prompt for it — the non-interactive equivalent of
        /// answering one, e.g. for CI. Takes precedence over the process
        /// environment, the on-disk cache, and the declaration's own
        /// `default`. See SPEC.md's "Variables".
        #[arg(long = "set", value_name = "NAME=VALUE", value_parser = parse_key_val)]
        set: Vec<(String, String)>,
    },
    /// Interactively resolve every declared `meshfox:var` (see SPEC.md's
    /// "Variables") and save the answers to the on-disk cache
    /// (`.meshfox/<filename>.env`, next to the canvas file) so `run`
    /// doesn't have to ask again. Shows each variable's currently-resolved
    /// value as the prompt's own default — press Enter to keep it. Secret
    /// variables are never cached, so there's nothing for this to save for
    /// them; they're skipped here and asked for fresh at run time instead.
    /// Requires an interactive terminal.
    Configure {
        #[command(flatten)]
        canvas: CanvasOpt,
    },
    /// Create a new, empty canvas file: just the `meshfox:canvas` marker
    /// followed by a lone root heading (`#`) named after the file itself
    /// (its name with a trailing `.canvas.md`/`.md` stripped). Fails if the
    /// file already exists — this never overwrites.
    Create {
        /// Path for the new .canvas.md file, e.g. `meshfox create
        /// hello.canvas.md`. May also be passed via `--canvas`. Required
        /// (either way) — there's nothing to auto-discover for a file that
        /// doesn't exist yet.
        canvas: Option<PathBuf>,
        /// Same as the positional argument above, spelled as a flag.
        #[arg(long = "canvas", value_name = "CANVAS", conflicts_with = "canvas")]
        canvas_flag: Option<PathBuf>,
    },
    /// Start the local web UI: canvas view, run buttons. Opens read-only —
    /// running a block is always allowed, but click "Edit" in the browser
    /// to unlock dragging, resizing, saving layout, and persisting a
    /// `cache`d block's output back into the file.
    ///
    /// If `server_socket` is configured (`.meshfox/config.toml` — see
    /// `meshfox_core::config`), a top-level invocation hands the canvas off
    /// to that external coordinator instead of becoming its own watcher:
    /// prints a short confirmation and exits immediately, the same
    /// "hands off and returns, to whatever keeps running independent of
    /// this process" contract the now-removed `meshfox open` command used
    /// to have (see `crate::coordinator::hand_off_to_configured_coordinator`).
    View {
        #[command(flatten)]
        canvas: CanvasOpt,
        /// Port to listen on. If omitted, a random free port is chosen —
        /// pass this explicitly to pin a stable port (e.g. for scripts).
        #[arg(long, default_value_t = 0)]
        port: u16,
        /// Don't automatically open a browser tab.
        #[arg(long)]
        no_open: bool,
        /// If the file doesn't exist yet, create it (same empty template
        /// as `create`) before opening it. Requires an explicit path — has
        /// nothing to auto-discover when the file isn't there yet. A no-op
        /// if the file already exists.
        #[arg(long)]
        create: bool,
        /// Don't exit automatically once every browser tab connected to
        /// this server has closed. `meshfox view` is meant to run only as
        /// long as something's actually looking at it, so by default it
        /// exits a few seconds after the last tab goes away; pass this to
        /// keep it running headless instead (e.g. scripts/tests that cycle
        /// through pages with brief all-tabs-closed gaps a real user
        /// wouldn't have).
        #[arg(long)]
        no_auto_exit: bool,
        /// Hidden: this run *is* a worker, spawned by a watcher (see
        /// `crate::watcher`) — connect to this socket to report our own
        /// bound port, and forward any cross-canvas "↗ open" click there
        /// instead of failing outright. Presence of this flag is what
        /// distinguishes a worker from a top-level invocation (which has
        /// none, and instead *becomes* the watcher — see the dispatch
        /// arm below). Not meant to be passed by hand.
        #[arg(long, hide = true)]
        watcher_socket: Option<PathBuf>,
    },
    /// An ncurses-style terminal viewer — browse the node
    /// tree, read a node's rendered Markdown body (syntax-highlighted code,
    /// local images shown inline where the terminal supports it), and run
    /// blocks with live streamed output, right in the terminal. Same
    /// deps-chain/cache/`meshfox:var` handling as `meshfox run`/`meshfox
    /// view`. A `tty` block hands the real terminal over to it, same as
    /// `meshfox run`'s own `tty` handling. The tree/document panes' own
    /// mouse support covers clicking a tree row to select it (or its ▾/▸
    /// marker to expand/collapse) and scrolling either pane; each row's
    /// title is also colored to match the node's own `color=`. `e` opens a
    /// fullscreen raw-source editor (vim-style modal input via `edtui`,
    /// meshfox-specific syntax highlighting, full mouse support — click to
    /// position the cursor, drag to select, scroll to move the viewport)
    /// on the selected node's own file — the terminal counterpart to the
    /// browser UI's Source mode. `Ctrl-f` switches
    /// between the document and any `include`d file; `Ctrl-n` turns the
    /// heading under the cursor into a node in one keystroke; `Ctrl-p`
    /// suggests attributes for the current `meshfox:node`/`meshfox:edge`
    /// comment or runnable-fence line, or, with the cursor inside a
    /// `tags=` value, tags already used elsewhere in the document. Still
    /// no *structural* editing beyond that (use `meshfox
    /// node ...` or the browser UI's Edit mode for that).
    Tui {
        #[command(flatten)]
        canvas: CanvasOpt,
        /// Start with this node id already selected (and its ancestors
        /// expanded so its row is actually visible) — how a "↗ open" on a
        /// `[label](other.canvas.md#node-id)` deep link lands the child
        /// TUI it spawns on the right node instead of the root. Not meant
        /// to be typed by hand day to day, but not hidden either — same
        /// spirit as jumping straight to a line number.
        #[arg(long)]
        node: Option<String>,
    },
    /// An MCP stdio server giving an AI agent tool-call access
    /// to every canvas file under the current directory, without shelling
    /// out to this same binary. Takes no arguments — a host launches it the
    /// same way as any other stdio MCP server: `{"command": "meshfox",
    /// "args": ["mcp"]}`, and whichever directory it's started in becomes
    /// its root. Multi-canvas by design, but keeps "one file, one process"
    /// isolation underneath: `canvas_open`/`canvas_close`/`canvas_list`
    /// manage a registry of canvases, each backed by its own spawned,
    /// isolated child process (a crash or hung debug session on one canvas
    /// can't affect another) — resolved only under that root directory,
    /// never above it. Every other tool requires that `canvas_id` as its
    /// first argument, mirroring its single-canvas equivalent exactly:
    /// a stateful debug session (`debug_start`/`debug_send`/`debug_stop` —
    /// a persistent `bash` kept alive in a node/block's own resolved
    /// cwd/env, so a multi-step snippet's state — exported vars, files it
    /// wrote — survives between calls, unlike a one-shot `meshfox run`) and
    /// thin wrappers around the whole `node <op>` surface — every
    /// subcommand, not just a subset: `show`/`find` (find as structured
    /// JSON, CSS-selector matching, same as `node find`) and the mutating
    /// `add`/`meta`/`body`/`block`/`rm`/`mv`/`rename`/`set_id`/`edges`/
    /// `move`/`reorder`. Deliberately does *not* attempt batch/
    /// transactional multi-edit or optimistic-concurrency write conflicts
    /// (see TODO.canvas.md's own "MCP-редактирование файла"/"Оптимистичная
    /// конкурентность" — still open design questions, not implemented
    /// here) — every write here is the same immediate read-modify-write
    /// `node <op>` already does.
    Mcp,
    /// Validate that a file parses as a meshfox canvas — same checks
    /// `run`/`view` already do before touching anything (single root,
    /// no duplicate ids, no dangling `meshfox:edge` targets, `group`/
    /// `file`/`link` body rules) — without executing anything or writing
    /// the file back. Exits non-zero on a parse error, so it's usable as a
    /// pre-commit/CI check.
    Validate {
        #[command(flatten)]
        canvas: CanvasOpt,
    },
    /// Run every embedded ` ```starlark constraint ` fence's Starlark
    /// contract against the document (see `crate::constraint`/SPEC.md's
    /// "Constraint nodes") and report which passed. Distinct from
    /// `validate`: `validate` checks that the file *parses* as a
    /// well-formed canvas; `check` asks whether the document as a whole
    /// satisfies whatever rules its own constraint fences declare (e.g.
    /// "every node tagged `table` has exactly one `file` child") — implies
    /// `validate` first, since an unparseable file has no constraints to
    /// run. Resolves includes first (same as `validate`/`view`/`static`),
    /// so a constraint sees the fully resolved document — an `include`
    /// node's own dumped-in content included — same tree the web UI
    /// checks, not just this file in isolation. Exits non-zero if the file
    /// (or any include target) fails to parse, an include is broken, or
    /// any constraint fails, so it's usable as a pre-commit/CI check
    /// alongside (or instead of) `validate`.
    Check {
        #[command(flatten)]
        canvas: CanvasOpt,
    },
    /// Print every runnable code block in the canvas as an indented tree,
    /// each with a ready-to-paste `meshfox run <path...> <name>` — so you
    /// don't have to go spelunking through the file to find out what's
    /// runnable. Same raw-file-only scope as `run`/`validate` (no
    /// include resolution).
    List {
        #[command(flatten)]
        canvas: CanvasOpt,
    },
    /// Experimental: export a canvas as a static site. Resolves includes
    /// (same as `validate`/`view`), turns the canvas's node tree into a
    /// recursive `SiteData` (context key `site`) and hands it to a
    /// user-supplied Tera template. A node with no real, authored
    /// `x`/`y`/`width`/`height` gets no computed position at all — the
    /// template renders it as an ordinary nested HTML element and the
    /// *browser* lays it out and sizes it from its real content (no
    /// pre-computed/estimated pixels to get wrong); a node that does have
    /// all four real values keeps rendering at exactly that authored pixel
    /// position. A structural (parent/child) connector between two
    /// flow-positioned nodes is drawn in pure CSS (they're always
    /// DOM-adjacent); everything else — a `meshfox:edge` cross-reference,
    /// or a structural edge touching a real-positioned node — is left for
    /// a small non-interactive JS pass in the template to measure and draw.
    /// Every `*.tera` file in `--template` (except one whose basename
    /// starts with `_`, a partial meant to be `{% import %}`ed rather than
    /// rendered standalone) is rendered and written to `--out` at the same
    /// relative path minus `.tera`; every other file is copied verbatim
    /// (CSS, fonts, ...) — except `template.toml` itself, the template's
    /// own config file (optional; a template with none gets an empty
    /// `base_url` and no `icons`), read from `--template`'s own directory
    /// and never copied to `--out`. A local image referenced from a node's
    /// Markdown body is copied alongside the output automatically; a
    /// `file`-type node's `display="code"` target is read once and inlined
    /// into the HTML directly (nothing left to fetch once static). See
    /// `site-template/` in this repo for a working example, including its
    /// own `template.toml`.
    Static {
        #[command(flatten)]
        canvas: CanvasOpt,
        /// Template directory.
        #[arg(short, long)]
        template: PathBuf,
        /// Output directory. Refused if it already exists and is
        /// non-empty, unless `--force`.
        #[arg(short, long, default_value = "site")]
        out: PathBuf,
        /// Overwrite an existing, non-empty `--out` directory.
        #[arg(long)]
        force: bool,
    },
    /// Experimental: export a canvas as a PDF, via a real (headless)
    /// Chrome/Chromium — a system install is used if one can be found
    /// (`CHROME` env var, common binary names on `PATH`, well-known
    /// install locations); otherwise a pinned Chromium build is downloaded
    /// once and cached for next time. Two kinds of pages, both by default:
    /// a canvas page — every node at its own box, full body always shown
    /// (never folded, regardless of the document's own fold settings); a
    /// real authored `x`/`y`/`width` is kept exactly, everything else
    /// auto-laid-out the same way the live web UI would place it, but
    /// height always auto-sizes to the node's own real content, authored
    /// or not, so nothing is ever clipped — printed at true 1:1 CSS-px
    /// scale on its own custom-sized page rather than scaled to fit a
    /// fixed paper size, with connectors for both structural parent/child
    /// and `meshfox:edge` cross-references; then the full node tree in
    /// flow/document order (headings by depth, tags, body, target,
    /// standard A4 pagination).
    Pdf {
        #[command(flatten)]
        canvas: CanvasOpt,
        /// Output PDF path. Defaults to the canvas filename with its
        /// extension replaced by `.pdf`, in the same directory.
        #[arg(short, long)]
        out: Option<PathBuf>,
        /// Overwrite an existing `--out` file.
        #[arg(long)]
        force: bool,
        /// Render only the canvas page or only the document page(s)
        /// instead of both (the default: canvas page first, then the
        /// document page(s)). A node with no real, authored
        /// `x`/`y`/`width`/`height` is auto-laid-out on the canvas page the
        /// same way the live web UI would place it, so this always has
        /// something to render.
        #[arg(long, value_enum)]
        mode: Option<pdf::Mode>,
    },
    /// Structural edits to individual nodes in a canvas file: add, move,
    /// rename, delete, or set a node's body/position/style/edges — the CLI
    /// counterpart to the web UI's Edit-mode node operations (the same
    /// `mdcanvas` surgical patches `meshfox view`'s `/api/nodes*` routes
    /// use), for scripting/CI or whenever a hand-rewrite would risk getting
    /// heading depth, sibling order, or dangling-edge cleanup wrong. Every
    /// subcommand validates the fully-patched document still parses before
    /// writing it back, same as every other mutating command here.
    Node {
        #[command(subcommand)]
        command: Box<NodeCommand>,
    },
    /// Print the full .canvas.md format specification (SPEC.md, embedded in
    /// this binary at compile time) — the canonical reference for the
    /// format, available offline wherever `meshfox` is installed.
    Spec,
    /// Check github.com/orofarne/meshfox's releases for a newer version
    /// than this binary and, if one exists, offer to download and install
    /// it in place (replacing the running executable). A no-op if this
    /// build wasn't made from a release tag (e.g. a local/dev build) —
    /// there's no version to compare against a release with, so it just
    /// says so and exits.
    CheckUpdates {
        /// Install the update without asking for confirmation first.
        /// Required when stdin isn't an interactive terminal, since
        /// there's no one to confirm with.
        #[arg(short = 'y', long)]
        yes: bool,
    },
    /// Print a shell completion script to stdout. Source it directly or
    /// write it to the completions directory your shell scans on startup,
    /// e.g. `meshfox completions zsh > ~/.zfunc/_meshfox` (with `~/.zfunc`
    /// on `fpath`), or `meshfox completions bash > /etc/bash_completion.d/meshfox`.
    Completions { shell: clap_complete::Shell },
}

/// Position/size/style fields settable via `node meta`, and (since they're
/// exactly the same fields, just applied at creation time instead of as a
/// separate follow-up call) via `node add` too — flattened into both
/// variants below rather than repeated, so the two can never quietly drift
/// apart. `node meta`'s own `--clear-position` isn't here: it's meaningless
/// on a node that doesn't have a position yet.
#[derive(Args, Default)]
struct NodeMetaFields {
    #[arg(long, allow_negative_numbers = true)]
    x: Option<f64>,
    #[arg(long, allow_negative_numbers = true)]
    y: Option<f64>,
    #[arg(long = "w")]
    width: Option<f64>,
    #[arg(long = "h")]
    height: Option<f64>,
    #[arg(long)]
    color: Option<String>,
    /// `text` (the default), `file`, `link`, `group`, or `include`.
    #[arg(long = "type")]
    node_type: Option<String>,
    /// `file`-node display mode: `link` (the default) or `code`.
    #[arg(long)]
    display: Option<String>,
    /// `file`-node syntax-highlighting language hint.
    #[arg(long)]
    lang: Option<String>,
    /// `file`-node interpreter (e.g. `python`) — makes the node runnable
    /// as `interpreter target`.
    #[arg(long)]
    interpreter: Option<String>,
    /// `link`-node social preview toggle: `true` shows an OpenGraph
    /// preview card below the link, `false` (the default) doesn't.
    #[arg(long)]
    preview: Option<bool>,
    /// Per-node fold-state override (see SPEC.md's "Options" section):
    /// `true`/`false` sets an explicit override, `default` clears it back
    /// to following the document's own default. Omit this flag entirely
    /// to leave whatever's already there untouched (or, on `node add`, to
    /// leave the new node with no override at all).
    #[arg(long)]
    fold: Option<String>,
    /// Comma-separated tags (`meshfox:node`'s own `tags=` spelling — see
    /// `meshfox_core::parse_tags`), replacing the whole list. Omit this
    /// flag entirely to leave the current tags untouched (or, on `node
    /// add`, to leave the new node with none); pass `--tags ""` to clear
    /// them.
    #[arg(long)]
    tags: Option<String>,
    /// Override `createdAt=` (see SPEC.md's "Timestamps") — RFC3339, e.g.
    /// `2026-08-29T10:15:00Z` or with an explicit offset like
    /// `2026-08-29T13:15:00+03:00`. Meant for backfilling/importing
    /// existing data with a real historical date; meshfox only stamps a
    /// fresh one automatically at creation time when the document declares
    /// the `auto-timestamps` option (off by default), so this is the only
    /// way to get a `createdAt` on a document that doesn't. Omit this flag
    /// entirely to leave whatever's already there untouched.
    #[arg(long = "created-at")]
    created_at: Option<String>,
}

impl NodeMetaFields {
    /// True if any field was actually given — `node add` only bothers
    /// calling into `apply_node_meta` at all when this is true, so a plain
    /// `node add <parent> <title>` with none of these flags produces
    /// exactly the same output it always has.
    fn is_set(&self) -> bool {
        self.x.is_some()
            || self.y.is_some()
            || self.width.is_some()
            || self.height.is_some()
            || self.color.is_some()
            || self.node_type.is_some()
            || self.display.is_some()
            || self.lang.is_some()
            || self.interpreter.is_some()
            || self.preview.is_some()
            || self.fold.is_some()
            || self.tags.is_some()
            || self.created_at.is_some()
    }
}

#[derive(Subcommand)]
enum NodeCommand {
    /// Add a new child node under `parent-id`, as the last item in its
    /// existing subtree (`mdcanvas::insert_child_node`) — same as the web
    /// UI's "add child" button. Empty-bodied and unpositioned by default,
    /// same as before `--body-file`/the position/style flags below existed
    /// — either can still be set later with `node body`/`node meta`
    /// instead, if not given here. Prints the new node's id: a slug of
    /// `title`, de-duplicated against every id already in the file.
    Add {
        /// Path to the .canvas.md file. If omitted: auto-discover the
        /// single candidate in the current directory.
        #[arg(long)]
        canvas: Option<PathBuf>,
        parent_id: String,
        title: String,
        /// Sets the new node's body in the same call, from this file — or
        /// from stdin if the value is `-`, same convention `git`/`tar`
        /// use. Omitted entirely (the default): the node stays
        /// empty-bodied, same as before this flag existed.
        #[arg(long)]
        body_file: Option<PathBuf>,
        #[command(flatten)]
        fields: NodeMetaFields,
    },
    /// Delete a node. By default the whole subtree goes with it
    /// (`mdcanvas::delete_node`), and any `meshfox:edge from="..."`
    /// elsewhere that pointed into the deleted subtree is dropped too, so
    /// the file can't be left with a dangling reference. `--keep-children`
    /// instead deletes just this node, promoting its direct children (and
    /// everything under them, untouched otherwise) to its own former
    /// parent (`mdcanvas::delete_node_reparent_children`). Refuses to
    /// delete the root either way.
    Rm {
        /// Path to the .canvas.md file. If omitted: auto-discover the
        /// single candidate in the current directory.
        #[arg(long)]
        canvas: Option<PathBuf>,
        node_id: String,
        /// Promote direct children to this node's own parent instead of
        /// deleting them too.
        #[arg(long)]
        keep_children: bool,
    },
    /// Move a node to a new structural parent (`mdcanvas::reparent_node`).
    /// That core function only ever promotes an *existing* extra-parent
    /// edge to structural parent — the web UI's two-step dance (drag a new
    /// edge onto the node, then promote it) — so this adds the
    /// `meshfox:edge from="new-parent-id"` line itself first, making the
    /// move a single atomic step from the CLI. Refuses to move the root,
    /// or to move a node into itself or one of its own descendants (would
    /// make the tree cyclic).
    Mv {
        /// Path to the .canvas.md file. If omitted: auto-discover the
        /// single candidate in the current directory.
        #[arg(long)]
        canvas: Option<PathBuf>,
        node_id: String,
        new_parent_id: String,
    },
    /// Rename a node's heading text, leaving its id, heading level, and
    /// body untouched (`mdcanvas::set_node_title`) — a node's id is pinned
    /// the first time it's written and never follows later title edits.
    Rename {
        /// Path to the .canvas.md file. If omitted: auto-discover the
        /// single candidate in the current directory.
        #[arg(long)]
        canvas: Option<PathBuf>,
        node_id: String,
        title: String,
    },
    /// Change a node's id (`mdcanvas::rename_node_id`) — the stable handle
    /// used for CLI/API addressing, `meshfox:edge from=`/`parent=`
    /// references, and `deps="node-id/block"` fence references. Rewrites
    /// every reference to the old id it can find: other nodes' `parent=`
    /// and `meshfox:edge from=` attributes are updated exactly (they're
    /// structurally tracked by the parser), and `deps=` references are
    /// updated best-effort (plain text, not parser-validated — run
    /// `meshfox validate` afterward to catch anything this missed, e.g. a
    /// reference that was already stale). Fails if `new-id` is empty,
    /// contains a `"` character, or is already used by another node.
    SetId {
        /// Path to the .canvas.md file. If omitted: auto-discover the
        /// single candidate in the current directory.
        #[arg(long)]
        canvas: Option<PathBuf>,
        node_id: String,
        new_id: String,
    },
    /// Replace a node's whole Markdown body (`mdcanvas::set_node_body`) —
    /// what the web UI's in-node editor would send, if it had one yet (see
    /// README's roadmap; for now the UI can reposition and run, not edit
    /// text). For a `file`/`link` node the body is its one Markdown link
    /// (`[title](target)`); a `group` node's body must stay empty. Reads
    /// the new body from `--file`, or from stdin if `--file` is omitted.
    Body {
        /// Path to the .canvas.md file. If omitted: auto-discover the
        /// single candidate in the current directory.
        #[arg(long)]
        canvas: Option<PathBuf>,
        node_id: String,
        /// Read the new body from this file instead of stdin.
        #[arg(long)]
        file: Option<PathBuf>,
    },
    /// Appends to the end of a node's existing Markdown body
    /// (`mdcanvas::append_node_body`) — after whatever's already there,
    /// still before its first child's own heading — without having to
    /// first read the current body back just to hand it to `node body`
    /// unchanged. Reads the text to append from `--file`, or from stdin if
    /// omitted, same convention as `node body`. Bumps `updatedAt=` the same
    /// way `node body` does (see SPEC.md's "Timestamps"), since it's
    /// implemented on top of the same `set_node_body`.
    Append {
        /// Path to the .canvas.md file. If omitted: auto-discover the
        /// single candidate in the current directory.
        #[arg(long)]
        canvas: Option<PathBuf>,
        node_id: String,
        /// Read the text to append from this file instead of stdin.
        #[arg(long)]
        file: Option<PathBuf>,
    },
    /// Rewrites just one runnable fence's own info-string attributes (and,
    /// optionally, its code — `--code-file`/`--code -`) inside a node
    /// (`mdcanvas::set_fence_attrs`) — every other fence in the node, the
    /// rest of its body, and the rest of the document are left byte-for-
    /// byte untouched. Unlike `node body`, never needs the whole node body
    /// reconstructed just to flip one flag on one block. `block-name` is
    /// resolved the same way `meshfox run`/`meshfox list` already do
    /// (explicit `name=`, the sole unnamed fence, or an explicit `default`
    /// flag) — see SPEC.md's "Runnable code fences". Any field left
    /// entirely unset keeps its current value; a paired `--no-`/`--clear-`
    /// flag explicitly removes it instead. `--deps` is validated
    /// (existing targets, no cycle) against the whole document right away,
    /// not deferred to a separate `meshfox validate`.
    Block {
        /// Path to the .canvas.md file. If omitted: auto-discover the
        /// single candidate in the current directory.
        #[arg(long)]
        canvas: Option<PathBuf>,
        node_id: String,
        block_name: String,
        /// New `name=` for the block — a fence's own runnable identity
        /// isn't addressable any other way, so this *is* the rename.
        #[arg(long)]
        rename: Option<String>,
        /// The bare language word right after the opening delimiter
        /// (`bash` in `` ```bash ``).
        #[arg(long)]
        lang: Option<String>,
        #[arg(long)]
        cache: bool,
        #[arg(long = "no-cache")]
        no_cache: bool,
        #[arg(long)]
        always: bool,
        #[arg(long = "no-always")]
        no_always: bool,
        #[arg(long)]
        default: bool,
        #[arg(long = "no-default")]
        no_default: bool,
        #[arg(long)]
        tty: bool,
        #[arg(long = "no-tty")]
        no_tty: bool,
        #[arg(long)]
        autoclose: bool,
        #[arg(long = "no-autoclose")]
        no_autoclose: bool,
        /// **Experimental** — see SPEC.md's "Service blocks (experimental)".
        #[arg(long)]
        service: bool,
        #[arg(long = "no-service")]
        no_service: bool,
        /// Comma-separated `deps=` targets (`node-id/block-name`, or a bare
        /// `block-name` for one in this same node), replacing the whole
        /// list outright — same spelling as the file's own `deps=`.
        #[arg(long)]
        deps: Option<String>,
        /// Removes `deps=` entirely instead of replacing it. Mutually
        /// exclusive with `--deps` in the same call.
        #[arg(long)]
        clear_deps: bool,
        /// Comma-separated `env=` entries (`$VAR` or `LOCAL=$VAR`),
        /// replacing the whole list outright — same spelling as the
        /// file's own `env=`.
        #[arg(long)]
        env: Option<String>,
        /// Removes `env=` entirely instead of replacing it. Mutually
        /// exclusive with `--env` in the same call.
        #[arg(long)]
        clear_env: bool,
        /// New `interpreter=` command (e.g. `"python3 -u"`).
        #[arg(long)]
        interpreter: Option<String>,
        /// Removes `interpreter=` entirely instead of setting it. Mutually
        /// exclusive with `--interpreter` in the same call.
        #[arg(long)]
        clear_interpreter: bool,
        /// Replaces the block's code, from this file — or from stdin if
        /// the value is `-`, same convention `node add --body-file` uses.
        /// Omitted entirely (the default): the code stays as it is.
        #[arg(long)]
        code_file: Option<PathBuf>,
    },
    /// Set a node's position/size/style fields (`mdcanvas::set_node_meta`)
    /// — `--x`/`--y`/`--w`/`--h` for a manual position/size override,
    /// `--color`/`--type`/`--display`/`--lang`/`--interpreter`/`--tags` for
    /// style/type. Any field left unset keeps its current value. `group`
    /// nodes never store a *size* (its box is always derived from its
    /// children instead), so `--w`/`--h` are rejected for one — but a
    /// group's own *position* is a real anchor its members' own `x`/`y` are
    /// relative to, so `--x`/`--y` is allowed on a group same as any other
    /// node.
    Meta {
        /// Path to the .canvas.md file. If omitted: auto-discover the
        /// single candidate in the current directory.
        #[arg(long)]
        canvas: Option<PathBuf>,
        node_id: String,
        #[command(flatten)]
        fields: NodeMetaFields,
        /// Drops any authored `x`/`y`/`w`/`h`, reverting the node to
        /// auto-placement (the web UI's own client-side layout picks a
        /// position/size for it again, same as a node that never had one)
        /// — mutually exclusive with `--x`/`--y`/`--w`/`--h` in the same
        /// call.
        #[arg(long)]
        clear_position: bool,
    },
    /// Replace a node's whole set of extra incoming edges (`meshfox:edge
    /// from="..."` lines, `mdcanvas::set_node_edges`) — the
    /// non-structural, non-nesting cross-references JSON Canvas-style
    /// graphs use. The given `--from` list (repeatable) *replaces*
    /// whatever was already there, it doesn't add to it; `--clear` removes
    /// them all.
    Edges {
        /// Path to the .canvas.md file. If omitted: auto-discover the
        /// single candidate in the current directory.
        #[arg(long)]
        canvas: Option<PathBuf>,
        node_id: String,
        /// An id to add as an extra parent (repeatable). Ignored if
        /// `--clear` is also given.
        #[arg(long = "from")]
        from: Vec<String>,
        /// Remove every extra parent instead of setting a new list.
        #[arg(long)]
        clear: bool,
    },
    /// Moves a node's whole subtree to sit immediately before or after
    /// another sibling under the same structural parent
    /// (`mdcanvas::move_sibling`) — the on-disk heading order is a node's
    /// *only* sibling order until it also has a real `x`/`y` (see `node
    /// reorder`), so this is the CLI's way to change it directly instead of
    /// hand-editing the file. Exactly one of `--before`/`--after` is
    /// required. Fails if the two nodes aren't siblings — moving to a
    /// *different* parent's children is `node mv`'s job.
    Move {
        /// Path to the .canvas.md file. If omitted: auto-discover the
        /// single candidate in the current directory.
        #[arg(long)]
        canvas: Option<PathBuf>,
        node_id: String,
        /// Move `node-id` to sit immediately before this sibling.
        #[arg(long)]
        before: Option<String>,
        /// Move `node-id` to sit immediately after this sibling.
        #[arg(long)]
        after: Option<String>,
    },
    /// Reorder every parent's direct children in the file to match their
    /// canvas layout (`mdcanvas::reorder_by_position`, sorted by `y` then
    /// `x` among ties) — the same resync the server runs on every save
    /// from the web UI, exposed standalone for whenever positions changed
    /// by hand (or via `node meta`) and the on-disk heading order should
    /// catch up to match what's actually drawn.
    Reorder {
        /// Path to the .canvas.md file. If omitted: auto-discover the
        /// single candidate in the current directory.
        #[arg(long)]
        canvas: Option<PathBuf>,
    },
    /// Print one node's parent, children, extra parents, type, and
    /// position/style fields — a read-only lookup, since eyeballing the
    /// tree shape directly from the file gets harder the deeper it nests.
    Show {
        /// Path to the .canvas.md file. If omitted: auto-discover the
        /// single candidate in the current directory.
        #[arg(long)]
        canvas: Option<PathBuf>,
        node_id: String,
    },
    /// Finds every node matching a CSS selector, a text substring, and/or
    /// a created/updated date range — the three axes AND together; any
    /// may be omitted. `selector` alone (its original form, still the
    /// default when nothing else is given) behaves byte-for-byte as
    /// before: the CSS engine stays the right tool for structure
    /// (`#todo > .bag`, tag/type/color matching, arbitrary-depth
    /// nesting) — `--text`/the date flags are independent predicates
    /// layered next to it, not a CSS extension, since CSS selectors have
    /// no substring-search or numeric-range primitives to begin with. The
    /// tree maps onto CSS almost directly: a node is an element, each tag
    /// is a class (`.bag`), `id`/`type`/`color` are ordinary attributes
    /// (`[type="file"]`), and structural nesting is DOM nesting —
    /// `#todo > .bag` for direct children, `#todo .bag` for descendants
    /// at any depth. Matching runs against a synthetic HTML document
    /// built from the canvas tree (never against real rendered content)
    /// via `scraper` — the same CSS engine a browser uses, not a bespoke
    /// query language to learn.
    Find {
        /// Path to the .canvas.md file. If omitted: auto-discover the
        /// single candidate in the current directory.
        #[arg(long)]
        canvas: Option<PathBuf>,
        /// Omit entirely (or pass "*") to match every node — useful when
        /// filtering purely by `--text`/the date flags below.
        selector: Option<String>,
        /// Print each match's full `node show` report instead of just its
        /// id.
        #[arg(long)]
        show: bool,
        /// Case-insensitive substring match against each node's title or
        /// body text. A match is followed by a short excerpt of the
        /// matching line, so a caller doesn't need a separate `node show`
        /// just to see why it matched.
        #[arg(long)]
        text: Option<String>,
        /// Keep only nodes `createdAt`'d at or after this instant —
        /// RFC3339 (`2026-08-29T10:00:00Z`), or a relative duration ago
        /// (`7d`, `2w`, `1h`, `30m`, `45s`). A node with no `createdAt` at
        /// all never matches any `--created-*`/`--updated-*`/`--since`
        /// filter (see SPEC.md's "Timestamps") — "unset" isn't "in range".
        #[arg(long = "created-after")]
        created_after: Option<String>,
        /// Keep only nodes `createdAt`'d strictly before this instant.
        /// Same RFC3339-or-relative parsing as `--created-after`.
        #[arg(long = "created-before")]
        created_before: Option<String>,
        /// Keep only nodes `updatedAt`'d at or after this instant. Same
        /// RFC3339-or-relative parsing as `--created-after`.
        #[arg(long = "updated-after")]
        updated_after: Option<String>,
        /// Keep only nodes `updatedAt`'d strictly before this instant.
        /// Same RFC3339-or-relative parsing as `--created-after`.
        #[arg(long = "updated-before")]
        updated_before: Option<String>,
        /// Keep only nodes touched (created OR updated) at or after this
        /// instant — shorthand for "what changed recently", equivalent to
        /// `--created-after`/`--updated-after` together. Same
        /// RFC3339-or-relative parsing.
        #[arg(long)]
        since: Option<String>,
    },
}

/// A leading `meshfox test.md run new-node-6` canvas path — recognized by
/// the same ".md" convention `run`'s own leading-arg sniffing already uses
/// (node ids/subcommand names never end in .md, see slugify) — spliced into
/// whatever position the subcommand that follows actually expects a canvas
/// path/`--canvas` in, before clap ever sees the argument list. Kept out of
/// the `Cli`/`Command` derive entirely (rather than a top-level `Option`
/// field next to `command`) because clap_complete's generated *shell*
/// completions are position-based: an extra optional positional ahead of
/// the subcommand shifts every subcommand one slot over in zsh's
/// `_arguments`, so e.g. `meshfox run <TAB>` (no canvas) starts completing
/// top-level subcommand names again instead of `run`'s own args. Doing the
/// reordering here instead means every subcommand's shape — and the
/// completions generated from it — is exactly what it was before this
/// existed, whichever order the canvas path is actually typed in.
fn splice_leading_canvas(mut args: Vec<String>) -> Vec<String> {
    let has_leading_canvas = args.len() >= 2
        && !args[1].starts_with('-')
        && args[1].to_ascii_lowercase().ends_with(".md");
    if !has_leading_canvas {
        return args;
    }
    let canvas = args.remove(1);
    match args.get(1).map(String::as_str) {
        // No subcommand at all (`meshfox test.md`): leave it as the sole
        // argument, handled below as "open it, same as `meshfox view`".
        None => args.insert(1, canvas),
        // `run` and `node <op> ...` take an explicit `--canvas` flag
        // instead of their own leading positional — `run`'s own args are a
        // free-form node-path/block-name list with nothing else to anchor
        // a bare path against, and `node <op>`'s positional slot is two
        // segments in (`node add`, `node rm`, ...), not on `node` itself.
        Some("run") => {
            args.insert(2, "--canvas".to_string());
            args.insert(3, canvas);
        }
        Some("node") if args.len() >= 3 => {
            args.insert(3, "--canvas".to_string());
            args.insert(4, canvas);
        }
        // Every other subcommand (including a `node ...` with no op, or an
        // unrecognized word — left for clap's own error to explain) takes
        // its own canvas path as the positional right after its name.
        _ => args.insert(2, canvas),
    }
    args
}

/// zsh's generated completion function is positional: each subcommand
/// dispatches off a fixed word index (`$line[1]`/`$words[N]`), so it has no
/// way to know a leading `meshfox test.md run ...` canvas path (see
/// `splice_leading_canvas`) is there — it just sees "test.md" where it
/// expects a subcommand name. bash's generated script is unaffected (it
/// walks words looking for known subcommand tokens rather than reading
/// fixed positions, so an extra leading word it doesn't recognize is
/// simply skipped over) — only zsh gets patched here.
///
/// Renames clap_complete's generated `_meshfox` to `_meshfox_generated` and
/// installs a small hand-written `_meshfox` in front of it that: detects a
/// leading canvas word by the same ".md" convention `splice_leading_canvas`
/// uses, reorders `words`/`CURRENT` the same way that function reorders
/// `argv` (so `_meshfox_generated`'s untouched, still-fully-generated
/// per-subcommand blocks see the shape they were generated for), and falls
/// through to `_meshfox_generated` unchanged whenever no such canvas word
/// is present. The `run`/`node <op>` vs. "every other subcommand" split
/// here has to be kept in sync with `splice_leading_canvas` by hand if a
/// future subcommand needs the same `--canvas`-flag-instead-of-positional
/// treatment; everything else (the command list, every subcommand's own
/// flags) stays fully generated and needs no maintenance here at all.
const ZSH_CANVAS_DISPATCH: &str = r#"_meshfox() {
    local w2="${words[2]:-}"
    local canvas_word=""
    case "${(L)w2}" in
        -*) ;;
        *.md) canvas_word="$w2" ;;
    esac

    if [[ -z "$canvas_word" ]]; then
        if (( CURRENT == 2 )) && [[ "$w2" != -* ]]; then
            _alternative \
                'commands:meshfox subcommand:_meshfox_commands' \
                'files:canvas path:_files -g "*.md"'
            return
        fi
        _meshfox_generated "$@"
        return
    fi

    if (( CURRENT == 3 )); then
        _meshfox_commands
        return
    fi

    local sub="${words[3]}"
    local -a new_words
    local new_current

    case "$sub" in
        run)
            new_words=(meshfox run --canvas "$canvas_word" "${words[@]:3}")
            new_current=$(( CURRENT + 1 ))
            ;;
        node)
            if (( CURRENT == 4 )); then
                new_words=(meshfox node "${words[@]:3}")
                new_current=$(( CURRENT - 1 ))
            else
                new_words=(meshfox node "${words[4]}" --canvas "$canvas_word" "${words[@]:4}")
                new_current=$(( CURRENT + 1 ))
            fi
            ;;
        *)
            new_words=(meshfox "$sub" "$canvas_word" "${words[@]:3}")
            new_current=$CURRENT
            ;;
    esac

    local -a words
    local CURRENT
    words=("${new_words[@]}")
    CURRENT=$new_current
    _meshfox_generated "$@"
}

"#;

fn print_completions(shell: clap_complete::Shell) {
    use clap::CommandFactory;
    if shell != clap_complete::Shell::Zsh {
        clap_complete::generate(
            shell,
            &mut Cli::command(),
            "meshfox",
            &mut std::io::stdout(),
        );
        return;
    }

    let mut buf = Vec::new();
    clap_complete::generate(shell, &mut Cli::command(), "meshfox", &mut buf);
    let generated = String::from_utf8(buf).expect("clap_complete output is valid UTF-8");

    let generated = generated.replacen("\n_meshfox() {\n", "\n_meshfox_generated() {\n", 1);
    let tail = "if [ \"$funcstack[1]\" = \"_meshfox\" ]; then\n    _meshfox \"$@\"\nelse\n    compdef _meshfox meshfox\nfi\n";
    let patched = generated.replacen(tail, &format!("{ZSH_CANVAS_DISPATCH}{tail}"), 1);
    debug_assert!(
        patched.contains("_meshfox_generated() {"),
        "clap_complete's zsh output shape changed"
    );
    debug_assert!(
        patched.contains(ZSH_CANVAS_DISPATCH),
        "clap_complete's zsh tail block shape changed"
    );

    print!("{patched}");
}

fn main() {
    // reqwest (meshfox-server's link-preview fetch) is built with rustls's
    // "rustls-no-provider" feature — see crates/server/Cargo.toml — so it
    // needs a process-wide default crypto provider installed before any TLS
    // connection happens. `ring` (not aws-lc-rs) so the binary only ever
    // compiles the one crypto backend headless_chrome/self_update already
    // need via ureq.
    rustls::crypto::ring::default_provider()
        .install_default()
        .expect("no default rustls CryptoProvider installed yet");

    let args = splice_leading_canvas(std::env::args().collect());

    if args.len() == 2 && !args[1].starts_with('-') && args[1].to_ascii_lowercase().ends_with(".md")
    {
        // A bare `meshfox test.md`, no subcommand: open it, same as
        // clicking the file — `meshfox view`'s own defaults otherwise.
        return view_watcher(PathBuf::from(&args[1]), 0, true, true);
    }

    let cli = Cli::parse_from(args);

    if cli.agent_help {
        print!("{}", include_str!("../../../AGENT_HELP.md"));
        return;
    }

    let Some(command) = cli.command else {
        use clap::CommandFactory;
        Cli::command().print_help().unwrap();
        std::process::exit(2);
    };

    match command {
        Command::Run {
            args,
            canvas,
            no_deps,
            set,
        } => {
            let canvas_path = canvas.unwrap_or_else(|| {
                // No `--canvas` given — about to fall back to
                // auto-discovery. If one of `run`'s own positional args
                // looks like it was actually meant as the canvas path
                // (`meshfox run foo.md ...` instead of `meshfox foo.md run
                // ...`), warn before find_canvas() has its own say — auto-
                // discovery may still succeed (a single real candidate in
                // the directory), in which case the mistake surfaces again,
                // more concretely, once `args` fails to resolve as a
                // node-id path below.
                let arg_strs: Vec<&str> = args.iter().map(String::as_str).collect();
                if let Some(hint) = run_hint_for_misplaced_canvas(&arg_strs) {
                    eprintln!("meshfox run: warning:{hint}");
                }
                find_canvas()
            });
            run(&canvas_path, args, no_deps, set)
        }
        Command::Configure { canvas } => {
            let canvas_path = canvas.resolve().unwrap_or_else(find_canvas);
            configure(&canvas_path)
        }
        Command::Create {
            canvas,
            canvas_flag,
        } => {
            let Some(canvas_path) = canvas.or(canvas_flag) else {
                eprintln!("meshfox create: a canvas path is required (positional, or --canvas)");
                std::process::exit(1);
            };
            create(&canvas_path)
        }
        Command::View {
            canvas,
            port,
            no_open,
            create: create_if_missing,
            no_auto_exit,
            watcher_socket,
        } => {
            let canvas_path = match canvas.resolve() {
                Some(p) => p,
                None => {
                    if create_if_missing {
                        eprintln!(
                            "meshfox view: --create requires an explicit path (nothing to \
                             auto-discover when the file doesn't exist yet)"
                        );
                        std::process::exit(1);
                    }
                    find_canvas()
                }
            };
            match watcher_socket {
                // Spawned by a watcher — just run the server; see
                // `meshfox_server::run`'s own doc comment for why it
                // never opens a browser tab itself any more.
                Some(socket) => view_worker(canvas_path, port, !no_auto_exit, socket),
                // A top-level invocation — this process itself becomes
                // the watcher (see `crate::watcher`'s own module doc
                // comment for why that's a plain process tree rather than
                // a detached daemon), spawning a worker for `canvas_path`
                // as its own child. `--create` only makes sense here:
                // the file has to exist before any worker (spawned by
                // either this or a later `Open` request) tries to read
                // it.
                None => {
                    if create_if_missing && !canvas_path.exists() {
                        write_canvas_template(&canvas_path);
                        println!("meshfox view: created {}", canvas_path.display());
                    }
                    view_or_hand_off(canvas_path, port, no_open, no_auto_exit)
                }
            }
        }
        Command::Tui { canvas, node } => {
            let canvas_path = canvas.resolve().unwrap_or_else(find_canvas);
            tui(canvas_path, node)
        }
        Command::Mcp => mcp_cmd(),
        Command::Validate { canvas } => {
            let canvas_path = canvas.resolve().unwrap_or_else(find_canvas);
            validate(&canvas_path)
        }
        Command::Check { canvas } => {
            let canvas_path = canvas.resolve().unwrap_or_else(find_canvas);
            check(&canvas_path)
        }
        Command::List { canvas } => {
            let canvas_path = canvas.resolve().unwrap_or_else(find_canvas);
            list(&canvas_path)
        }
        Command::Static {
            canvas,
            template,
            out,
            force,
        } => {
            let canvas_path = canvas.resolve().unwrap_or_else(find_canvas);
            static_cmd(&canvas_path, &template, &out, force)
        }
        Command::Pdf {
            canvas,
            out,
            force,
            mode,
        } => {
            let canvas_path = canvas.resolve().unwrap_or_else(find_canvas);
            pdf_cmd(&canvas_path, out.as_deref(), force, mode)
        }
        Command::Node { command } => match *command {
            NodeCommand::Add {
                canvas,
                parent_id,
                title,
                body_file,
                fields,
            } => node_add(
                &canvas.unwrap_or_else(find_canvas),
                &parent_id,
                &title,
                body_file,
                fields,
            ),
            NodeCommand::Rm {
                canvas,
                node_id,
                keep_children,
            } => node_rm(&canvas.unwrap_or_else(find_canvas), &node_id, keep_children),
            NodeCommand::Mv {
                canvas,
                node_id,
                new_parent_id,
            } => node_mv(
                &canvas.unwrap_or_else(find_canvas),
                &node_id,
                &new_parent_id,
            ),
            NodeCommand::Rename {
                canvas,
                node_id,
                title,
            } => node_rename(&canvas.unwrap_or_else(find_canvas), &node_id, &title),
            NodeCommand::SetId {
                canvas,
                node_id,
                new_id,
            } => node_set_id(&canvas.unwrap_or_else(find_canvas), &node_id, &new_id),
            NodeCommand::Body {
                canvas,
                node_id,
                file,
            } => node_body(&canvas.unwrap_or_else(find_canvas), &node_id, file),
            NodeCommand::Append {
                canvas,
                node_id,
                file,
            } => node_append(&canvas.unwrap_or_else(find_canvas), &node_id, file),
            NodeCommand::Block {
                canvas,
                node_id,
                block_name,
                rename,
                lang,
                cache,
                no_cache,
                always,
                no_always,
                default,
                no_default,
                tty,
                no_tty,
                autoclose,
                no_autoclose,
                service,
                no_service,
                deps,
                clear_deps,
                env,
                clear_env,
                interpreter,
                clear_interpreter,
                code_file,
            } => node_block(
                &canvas.unwrap_or_else(find_canvas),
                &node_id,
                &block_name,
                BlockArgs {
                    rename,
                    lang,
                    cache,
                    no_cache,
                    always,
                    no_always,
                    default,
                    no_default,
                    tty,
                    no_tty,
                    autoclose,
                    no_autoclose,
                    service,
                    no_service,
                    deps,
                    clear_deps,
                    env,
                    clear_env,
                    interpreter,
                    clear_interpreter,
                    code_file,
                },
            ),
            NodeCommand::Meta {
                canvas,
                node_id,
                fields,
                clear_position,
            } => node_meta(
                &canvas.unwrap_or_else(find_canvas),
                &node_id,
                fields.x,
                fields.y,
                fields.width,
                fields.height,
                clear_position,
                fields.color,
                fields.node_type,
                fields.display,
                fields.lang,
                fields.interpreter,
                fields.preview,
                fields.fold,
                fields.tags,
                fields.created_at,
            ),
            NodeCommand::Edges {
                canvas,
                node_id,
                from,
                clear,
            } => node_edges(&canvas.unwrap_or_else(find_canvas), &node_id, from, clear),
            NodeCommand::Move {
                canvas,
                node_id,
                before,
                after,
            } => node_move(
                &canvas.unwrap_or_else(find_canvas),
                &node_id,
                before,
                after,
            ),
            NodeCommand::Reorder { canvas } => node_reorder(&canvas.unwrap_or_else(find_canvas)),
            NodeCommand::Show { canvas, node_id } => {
                node_show(&canvas.unwrap_or_else(find_canvas), &node_id)
            }
            NodeCommand::Find {
                canvas,
                selector,
                show,
                text,
                created_after,
                created_before,
                updated_after,
                updated_before,
                since,
            } => node_find(
                &canvas.unwrap_or_else(find_canvas),
                selector.as_deref().unwrap_or("*"),
                show,
                text.as_deref(),
                created_after.as_deref(),
                created_before.as_deref(),
                updated_after.as_deref(),
                updated_before.as_deref(),
                since.as_deref(),
            ),
        },
        Command::Spec => print!("{}", include_str!("../../../SPEC.md")),
        Command::CheckUpdates { yes } => check_updates(yes),
        Command::Completions { shell } => print_completions(shell),
    }
}

/// `MESHFOX_VERSION_LABEL` (see its own doc comment above `VERSION`) is
/// either a release tag like `v0.2.1` or `commit <hash>` for a build made
/// off-tag. Only the former has anything to compare against a GitHub
/// release, so that's what distinguishes the two here rather than trying
/// to parse `commit ...` as a non-version and fall through.
fn check_updates(yes: bool) {
    let label = env!("MESHFOX_VERSION_LABEL");
    let is_release_tag =
        label.starts_with('v') && label[1..].starts_with(|c: char| c.is_ascii_digit());
    if !is_release_tag {
        println!(
            "meshfox check-updates: this build ({label}) wasn't made from a release tag, so \
             there's no version to compare against a GitHub release."
        );
        return;
    }
    let current_version = &label[1..];

    if !yes && !prompt::stdin_is_tty() {
        eprintln!(
            "meshfox check-updates: requires an interactive terminal to confirm an update \
             (stdin isn't one) — pass --yes to update without asking"
        );
        std::process::exit(1);
    }

    let result = self_update::backends::github::Update::configure()
        .repo_owner("orofarne")
        .repo_name("meshfox")
        .bin_name("meshfox")
        .bin_path_in_archive("{{ target }}/meshfox")
        .current_version(current_version)
        .no_confirm(yes)
        .show_download_progress(true)
        .build()
        .and_then(|update| update.update());

    match result {
        Ok(self_update::Status::UpToDate(v)) => println!("meshfox: already up to date (v{v})"),
        Ok(self_update::Status::Updated(v)) => {
            println!("meshfox: updated to v{v} — restart meshfox to use it")
        }
        Err(e) => {
            eprintln!("meshfox check-updates: {e}");
            std::process::exit(1);
        }
    }
}

fn parse_key_val(s: &str) -> Result<(String, String), String> {
    match s.split_once('=') {
        Some((k, v)) if !k.is_empty() => Ok((k.to_string(), v.to_string())),
        _ => Err(format!("expected NAME=VALUE, got {s:?}")),
    }
}

/// A canvas file's own directory — `.` when `canvas_path` is a bare
/// filename with no directory component (`Path::parent()` on one of those
/// returns `Some("")`, not `None`). A block runs with this as its `PWD`
/// unless its own node was spliced in from an `include` elsewhere on disk
/// (see `meshfox_core::canvas::Node::cwd`) — `meshfox run` doesn't resolve
/// includes for execution yet, so this is every block's `PWD` here for now.
pub(crate) fn canvas_root_dir(canvas_path: &Path) -> &Path {
    canvas_path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
}

/// Every `meshfox:var` the canvas declares, in document order — same
/// error-and-exit convention as every other parse step here.
fn declared_vars_or_exit(canvas_path: &Path, raw: &str) -> Vec<VarDecl> {
    let canvas = Canvas::from_markdown(raw).unwrap_or_else(|e| {
        eprintln!("failed to parse {}: {e}", canvas_path.display());
        std::process::exit(1);
    });
    meshfox_core::declared_vars(&canvas).unwrap_or_else(|e| {
        eprintln!("meshfox: {}: {e}", canvas_path.display());
        std::process::exit(1);
    })
}

fn load_var_cache_or_exit(canvas_path: &Path) -> VarCache {
    VarCache::load(canvas_path).unwrap_or_else(|e| {
        eprintln!(
            "failed to load variable cache for {}: {e}",
            meshfox_core::varcache::cache_path(canvas_path).display()
        );
        std::process::exit(1);
    })
}

/// A non-secret declaration's currently-resolved value with no overrides
/// in play — env, then cache, then shared/global config, then its own
/// `default` — same precedence `vars::resolve_with_shared` uses, minus the
/// `--set`/form-override step `configure` doesn't take. Shown as the
/// prompt's own default so Enter keeps it. Returns the shared origin
/// alongside the value when that's where it came from, so `configure` can
/// annotate the prompt with "(inherited from ...)".
fn current_value(
    decl: &VarDecl,
    cache: &VarCache,
    shared: &meshfox_core::SharedEnv,
) -> (Option<String>, Option<meshfox_core::SharedOrigin>) {
    if let Ok(v) = std::env::var(&decl.name) {
        return (Some(v), None);
    }
    if let Some(v) = cache.get(&decl.name) {
        return (Some(v.to_string()), None);
    }
    if let Some(sv) = shared.get(&decl.name) {
        return (Some(sv.value.clone()), Some(sv.origin.clone()));
    }
    (decl.default.clone(), None)
}

/// Short, human-readable note for `configure`'s terminal prompt when a
/// field's current value was inherited from shared/global config — see
/// `current_value`.
fn shared_origin_note(origin: &meshfox_core::SharedOrigin) -> String {
    match origin {
        meshfox_core::SharedOrigin::Project => {
            "(inherited from this project's .meshfox/config.toml)".to_string()
        }
        meshfox_core::SharedOrigin::Global { path: Some(p) } => {
            format!("(inherited from ~/.meshfox/config.toml, scoped to {p})")
        }
        meshfox_core::SharedOrigin::Global { path: None } => {
            "(inherited from ~/.meshfox/config.toml)".to_string()
        }
    }
}

fn configure(canvas_path: &PathBuf) {
    let raw = read_raw_or_exit(canvas_path);
    let decls = declared_vars_or_exit(canvas_path, &raw);
    // `secret` and `session` are both never cached -- `configure`'s whole
    // job is walking the cache, so neither has anything for it to do.
    let skipped_count = decls.iter().filter(|d| d.secret || d.session).count();
    let configurable: Vec<&VarDecl> = decls.iter().filter(|d| !d.secret && !d.session).collect();

    if configurable.is_empty() {
        if skipped_count > 0 {
            println!(
                "meshfox configure: {skipped_count} declared variable(s) are secret/session — \
                 never cached, so there's nothing to save for them; they're asked for fresh \
                 every time a run needs them."
            );
        } else {
            println!(
                "meshfox configure: {} declares no variables",
                canvas_path.display()
            );
        }
        return;
    }

    if !prompt::stdin_is_tty() {
        eprintln!("meshfox configure: requires an interactive terminal (stdin isn't one)");
        std::process::exit(1);
    }

    let mut cache = load_var_cache_or_exit(canvas_path);
    let shared = meshfox_core::load_shared_env(canvas_root_dir(canvas_path));
    if skipped_count > 0 {
        println!(
            "({skipped_count} secret/session variable(s) skipped -- never cached, always asked fresh at run time)"
        );
    }

    for decl in configurable.iter().copied() {
        let (current, origin) = current_value(decl, &cache, &shared);
        if let Some(origin) = &origin {
            println!("{} {}", decl.prompt, shared_origin_note(origin));
        }
        let value = prompt::ask(decl, current.as_deref()).unwrap_or_else(|e| {
            eprintln!("failed to read input: {e}");
            std::process::exit(1);
        });
        cache.set(&decl.name, &value).unwrap_or_else(|e| {
            eprintln!("failed to save {}: {e}", decl.name);
            std::process::exit(1);
        });
    }

    println!(
        "meshfox configure: saved {} variable(s) to {}",
        configurable.len(),
        meshfox_core::varcache::cache_path(canvas_path).display()
    );
}

/// Pure logic behind `meshfox validate` — the full validity pipeline
/// (parse, includes, `deps=`, var refs/scope, options, known attrs) —
/// returning the node count on success or the first failure's message.
/// Shared by the CLI's own `validate` (below, wraps this in
/// `println!`/`exit(1)`) and the MCP `validate` tool (wraps it in a
/// structured tool result/error) so the two front doors can't drift on
/// what "valid" means.
fn validate_canvas(raw: &str, canvas_path: &Path) -> Result<usize, String> {
    let canvas = Canvas::from_markdown(raw).map_err(|e| e.to_string())?;
    // Resolving includes here is the only way to catch a broken link, a
    // cycle, or a target that doesn't itself parse before `meshfox view`
    // does — this file's own structure is already known good at this
    // point regardless of the outcome below.
    meshfox_core::include::resolve(&canvas, canvas_path).map_err(|e| e.to_string())?;
    // Same idea for runnable-fence `deps=`: a dangling reference or a
    // cycle should fail CI/pre-commit here, not surface only when someone
    // actually tries to run the block.
    meshfox_core::deps::validate(&canvas).map_err(|e| e.to_string())?;
    // Validates every `meshfox:var` declaration itself (unique name,
    // declared in the root, `select` has `choices=`, ...) and every
    // runnable fence's `env=` reference against them — a typo'd variable
    // name would otherwise just silently resolve to nothing instead of
    // failing loudly (see `vars::resolve_block_env`).
    meshfox_core::validate_env_refs(&canvas).map_err(|e| e.to_string())?;
    // Same idea again for `default_var=`/`choices_var=`: every reference
    // must name a real declared variable, and the reference graph must be
    // acyclic.
    meshfox_core::validate_var_refs(&canvas).map_err(|e| e.to_string())?;
    // A node-scoped `meshfox:var` (declared outside root) is only visible
    // to `env=` inside its own subtree — same lenient-at-runtime,
    // strict-at-`validate` split as every check above.
    meshfox_core::validate_var_scope(&canvas).map_err(|e| e.to_string())?;
    // Same idea for `meshfox:option` (unique name, declared in the root)
    // — a misplaced or duplicated option should fail loudly here rather
    // than just being silently ignored by whatever consumer reads
    // `Canvas::options`.
    meshfox_core::declared_options(&canvas).map_err(|e| e.to_string())?;
    // `validate`-only, unlike every check above: an attribute name a
    // construct doesn't recognize (a typo, most likely) — every other
    // reader keeps silently accepting one it doesn't know, for
    // forward/backward compatibility between format versions (see
    // `validate_known_attrs`'s own doc comment).
    meshfox_core::validate_known_attrs(raw).map_err(|e| e.to_string())?;
    Ok(canvas.nodes.len())
}

fn validate(canvas_path: &PathBuf) {
    let raw = read_raw_or_exit(canvas_path);
    match validate_canvas(&raw, canvas_path) {
        Ok(n) => println!(
            "meshfox validate: {} ok ({n} node{})",
            canvas_path.display(),
            if n == 1 { "" } else { "s" }
        ),
        Err(e) => {
            eprintln!("meshfox validate: {}: {e}", canvas_path.display());
            std::process::exit(1);
        }
    }
}

/// Runs every embedded constraint fence's Starlark contract
/// (`meshfox_core::constraint::evaluate`) and reports pass/fail per fence,
/// same exit-code convention as every other check here (non-zero if the
/// file doesn't parse, an include is broken, or any constraint fails) —
/// usable in CI/pre-commit alongside `validate`. Resolves includes first
/// (same as `validate`/`view`/`static`) so a constraint runs against the
/// fully composed document rather than just this file in isolation — the
/// same tree `meshfox view`/the tui already evaluate constraints against
/// (`meshfox_core::constraint::annotate_status`, called after
/// `include::resolve` there too), so a constraint living inside an
/// included canvas is checked here as well, under its namespaced
/// `{include_id}/{original_id}`. Also passes the canvas's own directory as
/// `base_dir`, so a constraint's `.content()`/`.json()`/`.yaml()`/
/// `.toml()`/`.csv()` on a `file`-type node can actually resolve that
/// node's target (see `meshfox_core::constraint`).
/// Pure logic behind `meshfox check`: resolves includes, then evaluates
/// every constraint fence. `Err` only for a parse/include failure (same
/// class `validate_canvas` fails on) — an empty or partially-failing
/// result list is not an error, same as `find_node_ids` returning an
/// empty match list rather than erroring: the caller (CLI `check` below,
/// or the MCP `check` tool) decides what to do with per-constraint
/// failures.
fn check_canvas(raw: &str, canvas_path: &Path) -> Result<Vec<meshfox_core::ConstraintResult>, String> {
    let canvas = Canvas::from_markdown(raw).map_err(|e| e.to_string())?;
    let canvas = meshfox_core::include::resolve(&canvas, canvas_path).map_err(|e| e.to_string())?;
    Ok(meshfox_core::evaluate_constraints(
        &canvas,
        Some(canvas_root_dir(canvas_path)),
    ))
}

fn check(canvas_path: &PathBuf) {
    let raw = read_raw_or_exit(canvas_path);
    let results = check_canvas(&raw, canvas_path).unwrap_or_else(|e| {
        eprintln!("meshfox check: {}: {e}", canvas_path.display());
        std::process::exit(1);
    });
    if results.is_empty() {
        println!(
            "meshfox check: {} ok (no constraints)",
            canvas_path.display()
        );
        return;
    }

    for r in &results {
        if r.ok {
            println!("meshfox check: ok   {} {:?}", r.label, r.title);
        } else {
            eprintln!("meshfox check: FAIL {} {:?}", r.label, r.title);
            for msg in &r.messages {
                eprintln!("meshfox check:         {msg}");
            }
        }
    }

    let n = results.len();
    let failed = results.iter().filter(|r| !r.ok).count();
    if failed > 0 {
        eprintln!(
            "meshfox check: {}: {failed}/{n} constraint{} failed",
            canvas_path.display(),
            if n == 1 { "" } else { "s" }
        );
        std::process::exit(1);
    }
    println!(
        "meshfox check: {} ok ({n} constraint{})",
        canvas_path.display(),
        if n == 1 { "" } else { "s" }
    );
}

fn create(canvas_path: &Path) {
    if canvas_path.exists() {
        eprintln!("meshfox create: {} already exists", canvas_path.display());
        std::process::exit(1);
    }
    write_canvas_template(canvas_path);
    println!("meshfox create: wrote {}", canvas_path.display());
}

/// A short note, wrapped in a `meshfox:comment` marker pair (see SPEC.md's
/// "Comments"), that a freshly `create`d file's root body opens with:
/// invisible in meshfox's own tooling (web UI, TUI, `static`/`pdf`), but
/// shown normally by a plain Markdown renderer — exactly the audience that
/// doesn't already know this is a meshfox document from looking at it.
const NEW_CANVAS_NOTE: &str = "<!-- meshfox:comment -->\n> This is a [meshfox](https://meshfox.orofarne.net/) document — open it with `meshfox view` (or `meshfox tui`) for the interactive canvas. This note is only visible here, in a plain Markdown viewer.\n<!-- /meshfox:comment -->";

/// The empty-canvas template shared by `create`, `view --create`, and MCP's
/// `canvas_open`'s own `create` flag: the `meshfox:canvas` marker, a lone
/// root heading named after the file itself (so the new file is
/// immediately valid and auto-discoverable), and `NEW_CANVAS_NOTE` as the
/// root's own body.
pub(crate) fn canvas_template_content(canvas_path: &Path) -> String {
    let title = canvas_title(canvas_path);
    format!(
        "{}\n# {title}\n\n{NEW_CANVAS_NOTE}\n",
        mdcanvas::CANVAS_MARKER
    )
}

fn write_canvas_template(canvas_path: &Path) {
    use std::io::Write;
    let result = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(canvas_path)
        .and_then(|mut file| file.write_all(canvas_template_content(canvas_path).as_bytes()));
    result.unwrap_or_else(|e| {
        eprintln!("failed to write {}: {e}", canvas_path.display());
        std::process::exit(1);
    });
}

/// `path`'s file name with a trailing `.canvas.md` or `.md` stripped —
/// just the default root-heading text, not a format invariant (an id
/// derived from a heading never has to match the filename, see SPEC.md).
fn canvas_title(path: &Path) -> String {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    name.strip_suffix(".canvas.md")
        .or_else(|| name.strip_suffix(".md"))
        .unwrap_or(&name)
        .to_string()
}

/// A worker's own run — spawned by a watcher (`watcher_socket` names where
/// to report our own port and forward cross-canvas navigation). Never
/// opens a browser tab itself; see `meshfox_server::run`'s own doc
/// comment.
fn view_worker(canvas_path: PathBuf, port: u16, auto_exit: bool, watcher_socket: PathBuf) {
    let runtime = tokio::runtime::Runtime::new().unwrap_or_else(|e| {
        eprintln!("failed to start async runtime: {e}");
        std::process::exit(1);
    });
    if let Err(e) = runtime.block_on(meshfox_server::run(canvas_path, port, auto_exit, Some(watcher_socket), false)) {
        eprintln!("meshfox view: {e}");
        std::process::exit(1);
    }
}

/// A top-level `meshfox view <path>` invocation — this process itself
/// becomes the watcher for the whole session (see `crate::watcher`'s own
/// module doc comment), spawning `canvas_path`'s own worker as its first
/// child.
fn view_watcher(canvas_path: PathBuf, port: u16, open_browser: bool, auto_exit: bool) {
    let runtime = tokio::runtime::Runtime::new().unwrap_or_else(|e| {
        eprintln!("failed to start async runtime: {e}");
        std::process::exit(1);
    });
    let exe = std::env::current_exe().unwrap_or_else(|e| {
        eprintln!("meshfox view: couldn't resolve this binary's own path: {e}");
        std::process::exit(1);
    });
    if let Err(e) = runtime.block_on(watcher::run(exe, canvas_path, port, open_browser, auto_exit)) {
        eprintln!("meshfox view: {e}");
        std::process::exit(1);
    }
}

/// A top-level `meshfox view <path>` invocation's own entry point: hands
/// off to a configured `server_socket` coordinator instead of becoming a
/// watcher, if one's configured — see
/// `coordinator::hand_off_to_configured_coordinator`'s own doc comment for
/// the exact contract this replaces (the former `meshfox open` command).
fn view_or_hand_off(canvas_path: PathBuf, port: u16, no_open: bool, no_auto_exit: bool) {
    let runtime = tokio::runtime::Runtime::new().unwrap_or_else(|e| {
        eprintln!("failed to start async runtime: {e}");
        std::process::exit(1);
    });
    let fragment = None; // `view`'s own path argument has no `#node-id` deep-link syntax.
    match runtime.block_on(coordinator::hand_off_to_configured_coordinator(&canvas_path, fragment)) {
        Ok(Some(())) => {
            println!(
                "meshfox view: handed {} off to the configured coordinator",
                canvas_path.display()
            );
        }
        Ok(None) => {
            drop(runtime);
            view_watcher(canvas_path, port, !no_open, !no_auto_exit);
        }
        Err(e) => {
            eprintln!("meshfox view: {e}");
            std::process::exit(1);
        }
    }
}

fn tui(canvas_path: PathBuf, initial_node: Option<String>) {
    let runtime = tokio::runtime::Runtime::new().unwrap_or_else(|e| {
        eprintln!("failed to start async runtime: {e}");
        std::process::exit(1);
    });
    if let Err(e) = runtime.block_on(tui::run(canvas_path, initial_node)) {
        eprintln!("meshfox tui: {e}");
        std::process::exit(1);
    }
}

fn mcp_cmd() {
    let runtime = tokio::runtime::Runtime::new().unwrap_or_else(|e| {
        eprintln!("failed to start async runtime: {e}");
        std::process::exit(1);
    });
    if let Err(e) = runtime.block_on(mcp::run()) {
        eprintln!("meshfox mcp: {e}");
        std::process::exit(1);
    }
}

/// Files named `*.canvas.md` always count. A plain `*.md` file also counts
/// if it opens with the `<!-- meshfox:canvas -->` marker (see README) —
/// this is how e.g. README.md itself can be run without renaming it.
fn find_canvas() -> PathBuf {
    let mut candidates: Vec<PathBuf> = Vec::new();
    for entry in std::fs::read_dir(".")
        .expect("failed to read current directory")
        .filter_map(|e| e.ok())
    {
        let path = entry.path();
        let name = path.to_string_lossy();
        if mdcanvas::is_canvas_path(&path) {
            candidates.push(path);
        } else if name.ends_with(".md") {
            if let Ok(contents) = std::fs::read_to_string(&path) {
                if mdcanvas::has_marker(&contents) {
                    candidates.push(path);
                }
            }
        }
    }
    candidates.sort();
    match candidates.as_slice() {
        [one] => one.clone(),
        [] => {
            eprintln!(
                "no .canvas.md file (or marked *.md canvas) found in the current directory; pass the path explicitly"
            );
            std::process::exit(1);
        }
        many => {
            let names: Vec<String> = many
                .iter()
                .map(|p| p.file_name().unwrap_or_default().to_string_lossy().into_owned())
                .collect();
            eprintln!(
                "multiple canvas files found: {} — pass one explicitly",
                names.join(", ")
            );
            std::process::exit(1);
        }
    }
}

/// True for a positional argument that looks like it was meant as a canvas
/// path — same ".md" heuristic `splice_leading_canvas` uses to recognize
/// one *before* the subcommand. `run`'s own positional args are a
/// free-form node-id-path/block-name list with no such convention of their
/// own, so a canvas path typed *after* the subcommand (`meshfox run
/// foo.md ...` instead of `meshfox foo.md run ...`) is silently swallowed
/// as if it were a node id instead — see `run_hint_for_misplaced_canvas`'s
/// callers for where this gets flagged back to the user.
fn looks_like_a_canvas_path(s: &str) -> bool {
    s.to_ascii_lowercase().ends_with(".md")
}

/// A one-line hint appended to an error already caused by `run` (or a
/// `node <op>`, someday) misresolving its own node-id-path args, *iff* one
/// of those args looks like it was actually meant as the canvas path (see
/// `looks_like_a_canvas_path`) — `None` when nothing here looks suspicious,
/// so callers can just unwrap-or-default it onto the end of their own
/// error message without an extra branch.
fn run_hint_for_misplaced_canvas(args: &[&str]) -> Option<String> {
    let culprit = args.iter().find(|a| looks_like_a_canvas_path(a))?;
    Some(format!(
        " (note: {culprit:?} looks like a canvas path — for `run`, pass it *before* the \
         subcommand, e.g. `meshfox {culprit} run ...`, or via `--canvas`, not as one of `run`'s \
         own node-id-path/block-name arguments)"
    ))
}

/// Plain Levenshtein edit distance, `char`-wise — just for
/// `closest_node_path`'s "did you mean" suggestion below, so pulling in a
/// crate (`strsim`, already resolved transitively via `clap`, just not a
/// direct dependency here) isn't worth it for one small use.
fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut curr = vec![0usize; b.len() + 1];
    for (i, ca) in a.iter().enumerate() {
        curr[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            curr[j + 1] = (prev[j + 1] + 1).min(curr[j] + 1).min(prev[j] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[b.len()]
}

/// Node-id path from just below the root down to `node_id` — the same
/// space-joined spelling `run`'s own positional arguments (and `meshfox
/// list`'s printed example commands) already use.
fn node_path_string(canvas: &Canvas, node_id: &str) -> String {
    let mut segments = Vec::new();
    let mut current = canvas.node(node_id);
    while let Some(node) = current {
        let Some(parent_id) = &node.parent else {
            break; // the root itself is never part of the addressable path
        };
        segments.push(node.id.clone());
        current = canvas.node(parent_id);
    }
    segments.reverse();
    segments.join(" ")
}

/// "did you mean ...?" suggestion for a node-id path segment `run` (via
/// `resolve_run_chain`/`Canvas::resolve_path`, or the worker-routed
/// counterpart in `run_via_worker`) couldn't find. An exact id match
/// *anywhere else* in the tree wins outright regardless of distance — by
/// far the most common real mistake is the right node id addressed with
/// the wrong (missing/extra) ancestor chain, not a typo in the id itself.
/// Falls back to the closest id by edit distance, only offered when close
/// enough to plausibly be a typo rather than a coincidence — `None` means
/// nothing found worth suggesting.
fn closest_node_path(canvas: &Canvas, missing: &str) -> Option<String> {
    let addressable = || canvas.nodes.iter().filter(|n| n.parent.is_some());
    if let Some(exact) = addressable().find(|n| n.id == missing) {
        return Some(node_path_string(canvas, &exact.id));
    }
    let threshold = (missing.chars().count() / 3).max(1);
    addressable()
        .map(|n| (levenshtein(missing, &n.id), n))
        .filter(|(dist, _)| *dist <= threshold)
        .min_by_key(|(dist, _)| *dist)
        .map(|(_, n)| node_path_string(canvas, &n.id))
}

fn run(canvas_path: &PathBuf, args: Vec<String>, no_deps: bool, set: Vec<(String, String)>) {
    let runtime = tokio::runtime::Runtime::new().unwrap_or_else(|e| {
        eprintln!("failed to start async runtime: {e}");
        std::process::exit(1);
    });
    runtime.block_on(run_async(canvas_path, args, no_deps, set));
}

/// `--set NAME=VALUE` is the one place a value can enter `resolve`'s
/// `overrides` without ever passing through `prompt::ask`'s own
/// type-aware handling (a `select`'s numbered menu, a `bool`'s y/n, an
/// `int`'s own loop — see `prompt.rs`) — so this is where that same check
/// (`meshfox_core::validate_value`) has to happen instead, before the
/// value ever reaches the cache or a running block's environment. Only
/// checks a `--set` naming a variable this document actually declares;
/// one that doesn't (already accepted as an ordinary, non-`meshfox:var`
/// environment override — see `resolve`) has no type to check it against.
fn validate_set_overrides_or_exit(decls: &[VarDecl], overrides: &HashMap<String, String>) {
    if let Err(e) = validate_set_overrides(decls, overrides) {
        eprintln!("meshfox run: {e}");
        std::process::exit(1);
    }
}

/// The pure check `validate_set_overrides_or_exit` wraps — split out so
/// it's testable without a subprocess (`std::process::exit` isn't).
fn validate_set_overrides(
    decls: &[VarDecl],
    overrides: &HashMap<String, String>,
) -> Result<(), String> {
    for (name, value) in overrides {
        if let Some(decl) = decls.iter().find(|d| &d.name == name) {
            meshfox_core::validate_value(decl, value)
                .map_err(|e| format!("--set {name}={value:?} is invalid: {e}"))?;
        }
    }
    Ok(())
}

/// `--set NAME=VALUE` pins the cache regardless of whether anything in
/// *this* invocation's chain actually references it — same as `cmake -D`
/// always updating `CMakeCache.txt` — so a later plain `meshfox run`
/// (even for a different block) doesn't need `--set` again.
fn persist_set_overrides(
    decls: &[VarDecl],
    overrides: &HashMap<String, String>,
    cache: &mut VarCache,
) {
    for (name, value) in overrides {
        if decls
            .iter()
            .any(|d| &d.name == name && !d.secret && !d.session)
        {
            cache.set(name, value).unwrap_or_else(|e| {
                eprintln!("failed to save {name}: {e}");
                std::process::exit(1);
            });
        }
    }
}

async fn run_async(
    canvas_path: &PathBuf,
    mut args: Vec<String>,
    no_deps: bool,
    set: Vec<(String, String)>,
) {
    let block_arg = args.pop().expect("clap requires at least one arg");
    let path: Vec<&str> = args.iter().map(String::as_str).collect();
    let block_names: Vec<&str> = block_arg.split(',').map(str::trim).collect();

    let port = coordinator::get_or_spawn(canvas_path).await.unwrap_or_else(|e| {
        eprintln!("failed to reach worker for {}: {e}", canvas_path.display());
        std::process::exit(1);
    });
    let initial_raw = worker_client::get_canvas_raw(port).await.unwrap_or_else(|e| {
        eprintln!("failed to read {} through worker: {e}", canvas_path.display());
        std::process::exit(1);
    });

    // `meshfox:var` only ever lives in the root node, which is always in
    // the primary document — never affected by an `include`.
    let decls = declared_vars_or_exit(canvas_path, &initial_raw);
    let overrides: HashMap<String, String> = set.into_iter().collect();
    validate_set_overrides_or_exit(&decls, &overrides);
    let mut var_cache = load_var_cache_or_exit(canvas_path);
    persist_set_overrides(&decls, &overrides, &mut var_cache);

    // `run` becomes a pure client instead of executing in-process the
    // moment a worker for this file exists — a local `view`/`tui` session
    // (found via `worker_lock`), an externally-configured `server_socket`
    // coordinator, or (below) one this very invocation spins up itself.
    // `Other` hands the whole thing to `run_via_worker`, which needs none
    // of this function's own chain-resolution/`from=`-wiring/cache
    // machinery below — the worker's own `/api/run` already does all of
    // that server-side in one call. `--set` overrides are already
    // persisted into the on-disk var cache above regardless of which path
    // runs — same file the worker's own `state.vars_cache` reads, so
    // there's nothing worker-specific to redo here.
    //
    // `Us` used to just drop its `LockGuard` immediately and fall straight
    // through to the in-process loop below — a snapshot, not a held lock,
    // so a second `run` (or a `node <op>`) starting moments later saw "Us"
    // too and raced this one at the file level instead of ever seeing
    // "Other". Now it spins up its own core in the background instead —
    // the same `serve_as_worker` call `crate::tui::mod`'s own startup
    // already makes for itself — and holds the guard for that core's
    // whole lifetime (this process's own, since nothing outlives a plain
    // `run` invocation), so a concurrent caller now genuinely finds a
    // worker to join instead of a moment where nobody's there yet.
    //
    // `run_via_worker` handles a `tty` step itself (per requested name, via
    // `run_worker_tty`'s own `/api/run/tty` pty relay — see its doc
    // comment) — no reason left to bypass worker-routing for one at this
    // outer level the way an earlier version of this function did.
    match coordinator::resolve(canvas_path).await {
        Ok(coordinator::Resolved::Us(guard)) => {
            let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
            // `auto_exit: false` — this core's only client for now is
            // this very `run` invocation; nothing should exit it out
            // from under a still-running chain. `quiet: true` — this
            // core's own `println!`s (e.g. "serving ... on http://...")
            // would otherwise land intermixed with `run_via_worker`'s
            // own clean, `RunEvent`-driven console output.
            tokio::spawn(meshfox_server::serve_as_worker(
                canvas_path.clone(),
                0,
                false,
                None,
                true,
                guard,
                Some(ready_tx),
            ));
            match ready_rx.await {
                Ok(port) => {
                    return run_via_worker(port, canvas_path, &path, &block_names, no_deps, overrides)
                        .await;
                }
                Err(_) => {
                    eprintln!("meshfox run: failed to start the core for this file");
                    std::process::exit(1);
                }
            }
        }
        Ok(coordinator::Resolved::Other(port)) => {
            return run_via_worker(port, canvas_path, &path, &block_names, no_deps, overrides).await;
        }
        Err(e) => {
            eprintln!("meshfox run: {e}");
            std::process::exit(1);
        }
    }
}

/// Whether any of `block_names`' own resolved chains (deps included, unless
/// `no_deps`) contains a `tty` step. Two call sites, two different
/// purposes: `run_via_worker` calls this per requested name (against the
/// worker's own fetched raw canvas) to pick between its own two streaming
/// endpoints (`run_worker_tty`'s pty relay vs the plain `RunEvent`-only
/// stream) — see that module's own doc comment. `false` for anything that
/// fails to parse or resolve here — not this function's job to report
/// that; whichever path the caller ends up choosing already has its own
/// real error message for it.
fn chain_contains_tty(
    canvas_path: &Path,
    raw: &str,
    path: &[&str],
    block_names: &[&str],
    no_deps: bool,
) -> bool {
    let Ok(primary) = Canvas::from_markdown(raw) else {
        return false;
    };
    let Ok(canvas) = meshfox_core::include::resolve(&primary, canvas_path) else {
        return false;
    };
    for name in block_names {
        let Ok(chain) = meshfox_core::resolve_run_chain(&canvas, path, name, !no_deps) else {
            continue;
        };
        for addr in &chain {
            let Some(node) = canvas.node(&addr.node_id) else {
                continue;
            };
            let blocks = meshfox_core::scan_runnable_blocks(&addr.node_id, &node.text);
            if blocks
                .iter()
                .any(|b| b.name.as_deref() == Some(addr.block_name.as_str()) && b.tty)
            {
                return true;
            }
        }
    }
    false
}

/// `run_async`'s own coordinator-routed path (see its own call site's doc
/// comment) — driven entirely through `crate::worker_client`'s existing
/// run/vars calls, the same ones TUI's own run orchestration already uses.
/// Deliberately much smaller than the in-process loop above: dependency
/// resolution, `from=`/computed-var wiring between chain steps, and
/// cache/persist semantics are all the worker's own job inside a single
/// `worker_client::run_stream` call per requested block name — this only
/// owns var preflight (mirroring `preflight_chain_vars`'s own interactive
/// UX, against the worker's dep-aware `GET /api/vars` instead of a
/// locally-resolved chain) and turning each streamed `RunEvent` into the
/// same console shape the in-process loop already prints.
///
/// A `tty` step is handled specially per requested name (see
/// `run_worker_tty`, checked via `chain_contains_tty` right after
/// `preflight_worker_vars` succeeds, below) rather than through this
/// function's own ordinary `run_stream_persisted`/`drain_worker_run_events`
/// pair — the plain `RunEvent`-only stream those two use has no
/// binary-frame vocabulary for actual pty bytes at all.
///
/// One remaining known gap against the in-process path, flagged inline
/// below rather than silently papered over: a `service` block conflict
/// (`RunEvent::LockConflict`) has no interactive kill-and-retry here the
/// way the in-process path's `confirm_yn` does — `/api/run/force` exists
/// server-side for this, just not wired up here.
async fn run_via_worker(
    port: u16,
    canvas_path: &Path,
    path: &[&str],
    block_names: &[&str],
    no_deps: bool,
    mut overrides: HashMap<String, String>,
) {
    let path: Vec<String> = path.iter().map(|s| s.to_string()).collect();
    let mut had_failure = false;
    // Every `(nodeId, block)` any requested name's own chain started as a
    // `service` — accumulated across the whole loop below (mirroring the
    // in-process path's own `services: Vec<ServiceHandle>`), then watched
    // together once every requested name has been dispatched (see the
    // "stay attached" loop after this one).
    let mut started_services: Vec<(String, String)> = Vec::new();

    for name in block_names {
        // Mirrors the in-process loop's own "not a fenced-block address —
        // maybe `path`+`name` together name a runnable `file` node
        // instead" fallback (see `run_async`'s own `resolve_run_chain`
        // error arm) — a `get_vars` 404/422 here means exactly the same
        // thing server-side, just discovered a step later since this path
        // never parses the canvas itself. Checked *before* trusting that
        // error as final, not after, so a real file-node address (e.g. a
        // node whose only content is a runnable target file) keeps working
        // once a worker exists, exactly as it already does with no worker
        // at all — this is what `include_edit_tests`' sibling suite,
        // `crates/cli/tests/run_cmd.rs`, actually caught missing here.
        let vars = match preflight_worker_vars(port, &path, name, no_deps, &mut overrides).await {
            Ok(vars) => vars,
            Err(chain_err) => {
                match resolve_worker_file_node(port, canvas_path, &path, name).await {
                    Some(node_id) => {
                        match worker_client::run_file_node_stream(port, &node_id).await {
                            Ok(rx) => {
                                if !drain_worker_run_events(rx, port, name, &mut started_services).await {
                                    had_failure = true;
                                }
                            }
                            Err(e) => {
                                eprintln!("error running {name:?}: {e} (worker on port {port})");
                                had_failure = true;
                            }
                        }
                        continue;
                    }
                    None => {
                        let hint = worker_did_you_mean_hint(canvas_path, port, &path, name).await;
                        eprintln!("meshfox run: {chain_err}{hint}");
                        std::process::exit(1);
                    }
                }
            }
        };

        // A `tty` step needs the real pty-relay socket (`/api/run/tty`),
        // never the plain `RunEvent`-only stream `run_stream_persisted`
        // connects to (which has no binary-frame vocabulary at all for
        // actual pty bytes) — checked per requested `name` here, reusing
        // `chain_contains_tty` against the *worker's own* raw canvas (the
        // one source of truth once a worker exists) rather than this
        // process's own possibly-stale copy. `run_async`'s own top-level
        // check only ever gates *whether to route through a worker at
        // all* — this is the finer-grained "which of the worker's own two
        // streaming endpoints does this particular name need" decision.
        if let Ok(raw) = worker_client::get_canvas_raw(port).await {
            let path_str: Vec<&str> = path.iter().map(String::as_str).collect();
            if chain_contains_tty(canvas_path, &raw, &path_str, &[name], no_deps) {
                if !run_worker_tty(port, &path, name, no_deps, vars).await {
                    had_failure = true;
                }
                continue;
            }
        }

        let rx = match worker_client::run_stream_persisted(
            port,
            &path,
            name,
            no_deps,
            vars,
            std::collections::HashSet::new(),
            None,
        )
        .await
        {
            Ok(rx) => rx,
            Err(e) => {
                eprintln!("error running {name:?}: {e} (worker on port {port})");
                had_failure = true;
                continue;
            }
        };
        if !drain_worker_run_events(rx, port, name, &mut started_services).await {
            had_failure = true;
        }
    }

    // Mirrors the in-process loop's own tail exactly (see its own doc
    // comment on why `meshfox run` stays attached to a `service` block
    // instead of exiting once the chain that started it is "done") —
    // just polled over HTTP (`worker_client::get_service_log`/
    // `list_services`) instead of a local `ServiceHandle`, since the
    // service itself is a worker-owned, `crates/server/src/services.rs`-
    // registered process now, not one this CLI process spawned directly.
    if !started_services.is_empty() {
        println!(
            "meshfox: {} service(s) running — streaming their output below; Ctrl-C stops them and exits",
            started_services.len()
        );
        let mut printed = vec![0usize; started_services.len()];
        let mut crash_reported = vec![false; started_services.len()];
        let mut any_crashed = false;
        let mut ctrl_c = std::pin::pin!(tokio::signal::ctrl_c());
        'watch: loop {
            tokio::select! {
                _ = tokio::time::sleep(std::time::Duration::from_millis(200)) => {
                    let services = worker_client::list_services(port).await.unwrap_or_default();
                    let mut any_running = false;
                    for (i, (node_id, block)) in started_services.iter().enumerate() {
                        let log = worker_client::get_service_log(port, node_id, block)
                            .await
                            .unwrap_or_default();
                        for (stream, text) in log.iter().skip(printed[i]) {
                            let tag = match stream {
                                meshfox_server::stream_exec::OutputStream::Stderr => " (stderr)",
                                meshfox_server::stream_exec::OutputStream::Stdout => "",
                            };
                            println!("[{block}]{tag} {text}");
                        }
                        printed[i] = log.len();
                        let status = services
                            .iter()
                            .find(|s| &s.node_id == node_id && &s.block == block)
                            .map(|s| s.status.as_str());
                        match status {
                            Some("running") => any_running = true,
                            Some("crashed") => {
                                any_crashed = true;
                                if !crash_reported[i] {
                                    eprintln!("[{block}] crashed");
                                    crash_reported[i] = true;
                                }
                            }
                            // "stopped", or gone from the list entirely
                            // (the worker itself exited) — either way,
                            // nothing more to watch for this one.
                            _ => {}
                        }
                    }
                    if !any_running {
                        break 'watch;
                    }
                }
                _ = &mut ctrl_c => {
                    println!("^C — stopping {} service(s)", started_services.len());
                    for (node_id, block) in &started_services {
                        let _ = worker_client::stop_service(port, node_id, block).await;
                    }
                    std::process::exit(130); // 128 + SIGINT, the usual convention
                }
            }
        }
        if any_crashed {
            had_failure = true;
        }
    }

    if had_failure {
        std::process::exit(1);
    }
}

/// `run_async`'s own coordinator-routed path, checked only once
/// `preflight_worker_vars` has already failed to resolve `path`+`name` as
/// a fenced-block chain: fetches the worker's own raw canvas text (the
/// same one `App::new`/`crate::worker_client::get_canvas_raw` already use
/// for this purpose elsewhere) and resolves it *locally* — the one place
/// this coordinator-routed path parses anything client-side at all, purely
/// to answer "is this actually a runnable `file` node?" rather than to run
/// anything itself. `None` for any failure along the way (unreachable
/// worker, unparseable canvas, unresolvable path, or a real node that just
/// isn't a runnable file) — the caller already has the original chain-
/// resolution error to report in every one of those cases, so there's
/// nothing more specific worth surfacing from here.
async fn resolve_worker_file_node(
    port: u16,
    canvas_path: &Path,
    path: &[String],
    name: &str,
) -> Option<String> {
    let raw = worker_client::get_canvas_raw(port).await.ok()?;
    let primary = Canvas::from_markdown(&raw).ok()?;
    let canvas = meshfox_core::include::resolve(&primary, canvas_path).ok()?;
    let full_path: Vec<&str> = path.iter().map(String::as_str).chain(std::iter::once(name)).collect();
    let node = canvas.resolve_path(&full_path).ok()?;
    node.is_runnable_file().then(|| node.id.clone())
}

/// A "did you mean ...?" suffix for `chain_err`'s own message once *both*
/// `preflight_worker_vars` and `resolve_worker_file_node` have given up on
/// `path`+`name` — computed by re-resolving it locally against the
/// worker's own raw canvas purely for this diagnostic (the structured
/// `TreeError` the in-process path used to match on never reaches this
/// process at all once resolution happens server-side; `chain_err` itself
/// is only ever the server's already-stringified message by the time it
/// gets here). Empty string for anything that isn't specifically an
/// unresolvable node-id-path segment, or that fails to even re-fetch/parse
/// — same "nothing worth suggesting" posture `closest_node_path` already
/// has.
async fn worker_did_you_mean_hint(canvas_path: &Path, port: u16, path: &[String], name: &str) -> String {
    let Ok(raw) = worker_client::get_canvas_raw(port).await else {
        return String::new();
    };
    let Ok(primary) = Canvas::from_markdown(&raw) else {
        return String::new();
    };
    let Ok(canvas) = meshfox_core::include::resolve(&primary, canvas_path) else {
        return String::new();
    };
    let path_str: Vec<&str> = path.iter().map(String::as_str).collect();
    match meshfox_core::resolve_run_chain(&canvas, &path_str, name, true) {
        Err(meshfox_core::RunError::Tree(meshfox_core::TreeError::NodeNotFound(missing))) => {
            closest_node_path(&canvas, &missing)
                .map(|p| format!(" (did you mean `{p} {name}`?)"))
                .unwrap_or_default()
        }
        _ => String::new(),
    }
}

/// Runs one `tty`-containing chain through the worker's own pty-relay
/// socket (`GET /api/run/tty`) instead of the plain `RunEvent`-only stream
/// every other worker-routed name uses — real interactive bytes each
/// direction need a binary-frame vocabulary `run_stream_persisted`'s own
/// endpoint doesn't have at all. Reuses `crate::tui::bridge_http_tty`
/// verbatim (see its own doc comment for why that's safe: nothing in it
/// touches TUI-specific state) — this function's only own job is the
/// raw-mode enable/disable TUI's alt-screen already keeps on for its whole
/// session, which a plain `run` invocation has to switch on/off itself
/// around the call instead.
async fn run_worker_tty(
    port: u16,
    path: &[String],
    name: &str,
    no_deps: bool,
    vars: HashMap<String, String>,
) -> bool {
    if !prompt::stdin_is_tty() || !std::io::stdout().is_terminal() {
        eprintln!("error running {name:?}: requires an interactive terminal (stdin/stdout isn't one)");
        return false;
    }
    let (cols, rows) = crossterm::terminal::size().unwrap_or((80, 24));
    let mut socket = match worker_client::tty_connect(
        port,
        path,
        name,
        no_deps,
        vars,
        std::collections::HashSet::new(),
        cols,
        rows,
    )
    .await
    {
        Ok(socket) => socket,
        Err(worker_client::TtyConnectError::Conflict(c)) => {
            eprintln!(
                "error running {name:?}: already running elsewhere (pid {}, started via {}) — \
                 killing and retrying isn't wired up yet for a worker-routed tty run; stop it there first and rerun",
                c.owner_pid, c.owner_desc
            );
            return false;
        }
        Err(worker_client::TtyConnectError::Other(e)) => {
            eprintln!("error running {name:?}: {e} (worker on port {port})");
            return false;
        }
    };
    let _ = crossterm::terminal::enable_raw_mode();
    let exit_code = crate::tui::bridge_http_tty(&mut socket).await;
    let _ = crossterm::terminal::disable_raw_mode();
    let _ = futures_util::SinkExt::close(&mut socket).await;
    exit_code == 0
}

/// Worker-routed counterpart to `apply_node_mv`'s own two-step direct-file
/// dance: add an extra-parent edge from `new_parent_id` first (if `node_id`
/// doesn't already declare one), then promote it via
/// `worker_client::reparent_node` — `crates/server/src/lib.rs::reparent_node`
/// only ever promotes an *existing* extra-parent edge, same precondition
/// the direct-file path works around the same way (see `apply_node_mv`'s
/// own doc comment). Unlike `apply_node_mv`, the position-frame conversion
/// a move into/out of/between groups needs is entirely the server's own
/// job here (`reparent_node`'s own doc comment) — this never touches that
/// math at all. Fetches the node's *current* extra parents via
/// `include::resolve` (not the primary-document-only `Canvas::from_markdown`
/// `apply_node_mv` uses) — same resolved view every other worker-routed
/// read here already goes through.
async fn node_mv_via_worker(
    port: u16,
    canvas_path: &Path,
    node_id: &str,
    new_parent_id: &str,
) -> Result<(), String> {
    let raw = worker_client::get_canvas_raw(port).await.map_err(|e| e.to_string())?;
    let primary = Canvas::from_markdown(&raw).map_err(|e| e.to_string())?;
    let canvas = meshfox_core::include::resolve(&primary, canvas_path).map_err(|e| e.to_string())?;
    let node = canvas
        .node(node_id)
        .ok_or_else(|| format!("no node {node_id:?}"))?;
    if node.parent.is_none() {
        return Err("can't move the root node".to_string());
    }
    if canvas.node(new_parent_id).is_none() {
        return Err(format!("no node {new_parent_id:?}"));
    }
    if !node.extra_parents.iter().any(|e| e.from == new_parent_id) {
        let mut extra_parents = node.extra_parents.clone();
        extra_parents.push(ExtraEdge::new(new_parent_id));
        let update = worker_client::NodeUpdate {
            extra_parents: Some(extra_parents),
            ..Default::default()
        };
        worker_client::update_node(port, node_id, &update).await?;
    }
    worker_client::reparent_node(port, node_id, new_parent_id).await
}

/// Drains one requested name's own `RunEvent` stream to completion (a
/// fenced-block chain from `run_stream_persisted`, or a single file node
/// from `run_file_node_stream` — same event vocabulary either way, see
/// `run_file_node`'s own doc comment server-side), printing the same
/// console shape the in-process loop already does. Returns whether this
/// name's own run was clean (`true`) or reported *some* failure (`false`)
/// — never exits the process itself, so a single failing name in a
/// multi-name `meshfox run a,b,c` doesn't stop the rest from being
/// attempted, matching the in-process loop's own `had_failure`-without-
/// early-exit posture.
async fn drain_worker_run_events(
    mut rx: tokio::sync::mpsc::UnboundedReceiver<worker_client::RunEvent>,
    port: u16,
    name: &str,
    started_services: &mut Vec<(String, String)>,
) -> bool {
    let mut had_failure = false;
    {
        // The address `StepStart` most recently named — who Ctrl-C below
        // asks the worker to kill. `run_stream`'s own chain may run several
        // steps in sequence; only the currently-executing one is ever a
        // meaningful kill target.
        let mut current_step: Option<(String, String)> = None;
        // One listener for this whole chain's stream, not recreated per
        // event — same reasoning the in-process loop's own per-step
        // `ctrl_c` binding documents (recreating it too often leaves a gap
        // a real SIGINT can land in and be missed).
        let mut ctrl_c = std::pin::pin!(tokio::signal::ctrl_c());
        loop {
            tokio::select! {
                event = rx.recv() => {
                    let Some(event) = event else {
                        // Channel closed without a terminal event reaching
                        // it (a dropped connection mid-stream) — nothing
                        // more to do for this requested name.
                        break;
                    };
                    match event {
                        worker_client::RunEvent::Started { .. } => {}
                        worker_client::RunEvent::StepStart { node_id, block } => {
                            println!("==> {block}");
                            current_step = Some((node_id, block));
                        }
                        worker_client::RunEvent::StepSkipped { block, output, .. } => {
                            // No equivalent in the in-process loop above,
                            // which always re-executes every step
                            // regardless of a previous cached run — this is
                            // the worker's own session-fingerprint skip
                            // logic (see `crates/server/src/lib.rs`'s
                            // `compute_forced_reruns`), a deliberate benefit
                            // of joining a shared session rather than a gap
                            // to hide.
                            println!("==> {block} (already run this session, skipped)");
                            if !output.is_empty() {
                                println!("{output}");
                            }
                        }
                        worker_client::RunEvent::Output { stream, text, .. } => match stream {
                            meshfox_server::stream_exec::OutputStream::Stdout => println!("{text}"),
                            meshfox_server::stream_exec::OutputStream::Stderr => eprintln!("{text}"),
                        },
                        worker_client::RunEvent::ServiceStarted { node_id, block, pid } => {
                            println!("==> {block} (service started, pid {pid})");
                            started_services.push((node_id, block));
                        }
                        worker_client::RunEvent::StepEnd { exit_code, duration_ms, .. } => {
                            println!(
                                "(exit {exit_code} · {})",
                                meshfox_core::format_duration_ms(duration_ms)
                            );
                        }
                        worker_client::RunEvent::LockConflict { block, owner_pid, owner_desc, .. } => {
                            eprintln!(
                                "error running {block:?}: already running elsewhere (pid {owner_pid}, started via {owner_desc}) — \
                                 killing and retrying isn't wired up yet for a worker-routed run; stop it there first and rerun"
                            );
                            had_failure = true;
                        }
                        // Shouldn't actually happen: `run_via_worker`'s own
                        // per-name `chain_contains_tty` check routes a real
                        // `tty` step through `run_worker_tty`'s pty relay
                        // instead, before this stream is ever opened at
                        // all — kept only for exhaustiveness/defense, e.g.
                        // if that check somehow missed a step this stream
                        // still reaches.
                        worker_client::RunEvent::TtyStart { block, .. } => {
                            eprintln!(
                                "error running {block:?}: unexpected `tty` step on the plain run stream \
                                 (this is a bug — `run_via_worker`'s own tty pre-check should have caught it)"
                            );
                            had_failure = true;
                        }
                        worker_client::RunEvent::Killed { block, .. } => {
                            eprintln!("{block:?} was killed");
                            had_failure = true;
                        }
                        worker_client::RunEvent::Error { message } => {
                            eprintln!("error running {name:?}: {message}");
                            had_failure = true;
                        }
                        worker_client::RunEvent::Done { exit_code } => {
                            if exit_code != 0 {
                                had_failure = true;
                            }
                            break;
                        }
                    }
                }
                _ = &mut ctrl_c => {
                    if let Some((node_id, block)) = &current_step {
                        let _ = worker_client::kill_run(port, node_id, block).await;
                    }
                    eprintln!("^C — asked the worker to stop {name:?}");
                    // No local `file_raws`/`write_all_files` to flush here
                    // — the worker persists a `cache`d step's output as
                    // each step actually completes (`crates/server/src/lib.rs`'s
                    // own `persist` handling), not batched up for this
                    // process to write out at the end the way the
                    // in-process path's own `file_raws` map is.
                    std::process::exit(130); // 128 + SIGINT, the usual convention
                }
            }
        }
    }
    !had_failure
}

/// `run_via_worker`'s own var preflight — mirrors `preflight_chain_vars`'s
/// interactive UX (check `overrides` first, otherwise prompt, refuse
/// non-interactively) against the worker's own dep-aware `GET /api/vars`
/// instead of a locally-resolved chain. Only returns entries for variables
/// this call is *overriding* (an `--set` value, or a fresh interactive
/// answer) — anything the worker already reports `resolved: true` for is
/// left out entirely and resolved server-side from its own cache/env/
/// default, exactly as if this call had never mentioned it; sending it
/// again would be harmless but redundant. A prompted-for, non-secret
/// answer is folded back into `overrides` (not written to the on-disk
/// cache directly) — `worker_client::run_stream`'s own request already
/// persists any non-secret entry in its `vars` to the worker's on-disk
/// cache itself (`crates/server/src/lib.rs`'s `run_block_impl`, the exact
/// file `crate::VarCache` also reads), so a later requested block name in
/// this same invocation just sees it there already, and there's nothing
/// separate for this function to persist on its own.
async fn preflight_worker_vars(
    port: u16,
    path: &[String],
    name: &str,
    no_deps: bool,
    overrides: &mut HashMap<String, String>,
) -> Result<HashMap<String, String>, String> {
    let statuses = worker_client::get_vars(port, path, name, no_deps).await?;
    let mut vars = HashMap::new();
    let mut missing: Vec<String> = Vec::new();
    for status in &statuses {
        if let Some(value) = overrides.get(&status.name) {
            vars.insert(status.name.clone(), value.clone());
            continue;
        }
        if status.resolved {
            continue; // already resolvable server-side (cache/env/default) — nothing to override
        }
        if !prompt::stdin_is_tty() {
            missing.push(status.name.clone());
            continue;
        }
        let decl = var_decl_from_status(status);
        let value = prompt::ask(&decl, decl.default.as_deref()).map_err(|e| e.to_string())?;
        if !status.secret {
            overrides.insert(status.name.clone(), value.clone());
        }
        vars.insert(status.name.clone(), value);
    }
    if !missing.is_empty() {
        return Err(format!(
            "missing required variable(s): {} — pass --set NAME=VALUE, set the environment \
             variable, or run interactively",
            missing.join(", ")
        ));
    }
    Ok(vars)
}

/// A synthetic `VarDecl` built from a worker-reported `VarStatus`, just
/// enough for `prompt::ask` to reuse the exact same interactive UX
/// (secret masking, `select`/`bool`/`int` validation) the in-process path
/// already has. `VarStatus` is a strictly smaller wire-shaped view than a
/// real `VarDecl` (the server has already fully resolved everything
/// `from`/`session`/`default_var`/`choices_var` would have affected by the
/// time it reports one) — those fields are left at their now-meaningless
/// defaults, since none of them change `ask`'s own prompting behavior.
fn var_decl_from_status(status: &worker_client::VarStatus) -> VarDecl {
    VarDecl {
        name: status.name.clone(),
        var_type: match status.var_type.as_str() {
            "int" => VarType::Int,
            "bool" => VarType::Bool,
            "select" => VarType::Select,
            _ => VarType::String,
        },
        prompt: status.prompt.clone(),
        default: None,
        choices: status.choices.clone(),
        secret: status.secret,
        required: false,
        from: None,
        session: false,
        default_var: None,
        choices_var: None,
    }
}

fn list(canvas_path: &PathBuf) {
    let raw = read_raw_or_exit(canvas_path);
    let canvas = Canvas::from_markdown(&raw).unwrap_or_else(|e| {
        eprintln!("failed to parse {}: {e}", canvas_path.display());
        std::process::exit(1);
    });
    let blocks = canvas.list_runnable().unwrap_or_else(|e| {
        eprintln!("meshfox list: {}: {e}", canvas_path.display());
        std::process::exit(1);
    });
    if blocks.is_empty() {
        println!(
            "meshfox list: no runnable blocks in {}",
            canvas_path.display()
        );
        return;
    }

    // Group consecutive entries by owning node — already contiguous, since
    // list_runnable's depth-first walk emits one node's own blocks
    // together, before recursing into its children.
    struct Group<'a> {
        path: &'a [String],
        node_id: &'a str,
        blocks: Vec<&'a meshfox_core::RunnableBlock>,
    }
    let mut groups: Vec<Group> = Vec::new();
    for b in &blocks {
        match groups.last_mut() {
            Some(g) if g.path == b.path.as_slice() => g.blocks.push(b),
            _ => groups.push(Group {
                path: &b.path,
                node_id: &b.node_id,
                blocks: vec![b],
            }),
        }
    }

    // Two passes: first build every line's (indent+label, command) pair —
    // printing a header for each new ancestor path segment the first time
    // it's seen, same grouping idea as before — then right-pad every
    // line's label to the widest one so the "meshfox run ..." column lines
    // up. A node whose *only* runnable block is its `default` (implicitly,
    // via a matching `name=`/sole unnamed fence, or via an explicit
    // `default` flag) skips its own header line entirely: the block line
    // takes its place, since showing both would just repeat the same
    // identifier twice for no reason. A node with more than one block that
    // still has a `default` among them gets the node-id shortcut command
    // printed on its own header line instead, alongside its full ordinary
    // per-block lines below.
    let mut headers: Vec<(usize, Line)> = Vec::new(); // (position in `lines`, header), interleaved by position
    let mut lines: Vec<Line> = Vec::new();
    let mut last_path: Vec<String> = Vec::new();

    for g in &groups {
        let default =
            g.blocks.len() == 1 && meshfox_core::fence::is_default(&g.blocks[0].block, g.node_id);
        let collapse = !g.path.is_empty() && default;
        let header_depth = if collapse {
            g.path.len() - 1
        } else {
            g.path.len()
        };

        for depth in 0..header_depth {
            if depth >= last_path.len() || last_path[depth] != g.path[depth] {
                // Only the node's own header (the last depth reached here)
                // can carry a shortcut command, and only when this group's
                // blocks (not yet collapsed into one line) include a
                // default one to shortcut to.
                let is_own_header = depth == g.path.len() - 1;
                let command = (is_own_header && !collapse && has_default(g.node_id, &g.blocks))
                    .then(|| format!("meshfox run {}", g.path[..=depth].join(" ")));
                headers.push((
                    lines.len(),
                    Line {
                        indent: depth,
                        label: g.path[depth].clone(),
                        command,
                    },
                ));
            }
        }
        last_path = g.path.to_vec();

        let indent = header_depth;
        for b in &g.blocks {
            let name = b.block.name.as_deref().unwrap_or("?");
            lines.push(Line {
                indent,
                label: format!("{name}{}", annotate(&b.block)),
                command: Some(run_command(g.path, name, g.node_id)),
            });
        }
    }

    let max_width = lines
        .iter()
        .chain(headers.iter().map(|(_, h)| h))
        .map(|l| l.indent * 2 + l.label.len())
        .max()
        .unwrap_or(0);

    let mut header_iter = headers.into_iter().peekable();
    for (i, line) in lines.iter().enumerate() {
        while header_iter.peek().is_some_and(|(pos, _)| *pos == i) {
            let (_, header) = header_iter.next().unwrap();
            print_line(&header, max_width);
        }
        print_line(line, max_width);
    }
}

struct Line {
    indent: usize,
    label: String,
    command: Option<String>,
}

fn print_line(line: &Line, max_width: usize) {
    let prefix = format!("{}{}", "  ".repeat(line.indent), line.label);
    match &line.command {
        Some(command) => println!("{:<width$}    {}", prefix, command, width = max_width),
        None => println!("{prefix}"),
    }
}

/// Whether `node_id`'s blocks include a `default` one (see
/// `meshfox_core::fence::default_block`) — used by `list` to decide
/// whether a non-collapsed node header gets a `meshfox run <path>`
/// shortcut line. Ambiguous (more than one qualifying block) counts as
/// "no" here, same as `resolve_target`'s runtime fallback — `meshfox
/// validate` is what reports the conflict.
fn has_default(node_id: &str, blocks: &[&meshfox_core::RunnableBlock]) -> bool {
    let code_blocks: Vec<meshfox_core::CodeBlock> =
        blocks.iter().map(|b| b.block.clone()).collect();
    meshfox_core::fence::default_block(node_id, &code_blocks)
        .ok()
        .flatten()
        .is_some()
}

/// `[button]`/`[cache]`/`[tty]`/`[default]`/`[deps: ...]`/`[env: ...]` suffix for one block's
/// tree line. `default` is only shown for the explicit flag — a block
/// that's default purely by its name matching the node's own id doesn't
/// need the annotation, since that's already visible from the node/block
/// labels matching (or from the collapsed line, when it's the node's only
/// block). `env` lists exactly what this block opts into — a bare name
/// for pass-through, `local=declared` for a rename — so it's obvious at a
/// glance which blocks will ever prompt for a `meshfox:var` and which
/// never will (see SPEC.md's "Variables").
fn annotate(block: &meshfox_core::CodeBlock) -> String {
    let mut annotations = Vec::new();
    if meshfox_core::is_button(&block.lang) {
        annotations.push("button".to_string());
    }
    if meshfox_core::is_form(&block.lang) {
        annotations.push("form".to_string());
    }
    if block.cache {
        annotations.push("cache".to_string());
    }
    if block.tty {
        annotations.push("tty".to_string());
    }
    if block.autorun {
        annotations.push("autorun".to_string());
    }
    if block.fold {
        annotations.push("fold".to_string());
    }
    if block.default {
        annotations.push("default".to_string());
    }
    if !block.deps.is_empty() {
        let deps: Vec<String> = block
            .deps
            .iter()
            .map(|d| match &d.node_id {
                Some(n) => format!("{n}/{}", d.block_name),
                None => d.block_name.clone(),
            })
            .collect();
        annotations.push(format!("deps: {}", deps.join(", ")));
    }
    if !block.env.is_empty() {
        let env: Vec<String> = block
            .env
            .iter()
            .map(|e| {
                if e.local_name == e.var_name {
                    e.var_name.clone()
                } else {
                    format!("{}={}", e.local_name, e.var_name)
                }
            })
            .collect();
        annotations.push(format!("env: {}", env.join(", ")));
    }
    if annotations.is_empty() {
        String::new()
    } else {
        format!(" [{}]", annotations.join(", "))
    }
}

/// The `meshfox run ...` command for one block. When `name` equals the
/// owning node's own id *and* `path` actually reaches that node (i.e.
/// isn't empty — root's own blocks have no path segment to begin with, so
/// there's nothing to shorten there), the path's own last segment already
/// *is* the block's name — appending it again would just repeat the same
/// token, so it's dropped.
fn run_command(path: &[String], name: &str, node_id: &str) -> String {
    if !path.is_empty() && name == node_id {
        return format!("meshfox run {}", path.join(" "));
    }
    let path_args = if path.is_empty() {
        String::new()
    } else {
        format!("{} ", path.join(" "))
    };
    format!("meshfox run {path_args}{name}")
}

fn read_raw_or_exit(canvas_path: &Path) -> String {
    let runtime = tokio::runtime::Runtime::new().expect("Tokio runtime");
    let port = worker_port_or_exit(&runtime, canvas_path);
    runtime.block_on(worker_client::get_canvas_raw(port)).unwrap_or_else(|e| {
        eprintln!("failed to read {} through worker: {e}", canvas_path.display());
        std::process::exit(1);
    })
}

fn worker_port_or_exit(runtime: &tokio::runtime::Runtime, canvas_path: &Path) -> u16 {
    runtime
        .block_on(coordinator::get_or_spawn(canvas_path))
        .unwrap_or_else(|e| {
            eprintln!("failed to start or reach the worker for {}: {e}", canvas_path.display());
            std::process::exit(1);
        })
}

fn node_add(
    canvas_path: &Path,
    parent_id: &str,
    title: &str,
    body_file: Option<PathBuf>,
    fields: NodeMetaFields,
) {
    let body = body_file.as_deref().map(read_body_source_or_exit);
    let runtime = tokio::runtime::Runtime::new().unwrap_or_else(|e| {
        eprintln!("failed to start async runtime: {e}");
        std::process::exit(1);
    });
    let port = worker_port_or_exit(&runtime, canvas_path);
    // `title_slug_id: true` — `node add` has always produced a
    // title-slug id (`mdcanvas::insert_child_node`), never the web
    // UI's own random one; routing through a worker can't be allowed
    // to change which scheme a caller gets back (see
    // `crates/server/src/lib.rs`'s `CreateNodeRequest::title_slug_id`
    // doc comment).
    let new_id = match runtime.block_on(worker_client::create_node(port, parent_id, title, true)) {
        Ok(id) => id,
        Err(e) => {
            eprintln!("meshfox node add: {e} (worker on port {port})");
            std::process::exit(1);
        }
    };
    if body.is_some() || fields.is_set() {
        let update = match node_update_from_fields(&fields, body.as_deref()) {
            Ok(u) => u,
            Err(e) => {
                eprintln!("meshfox node add: added {new_id:?}, but {e}");
                std::process::exit(1);
            }
        };
        if let Err(e) = runtime.block_on(worker_client::update_node(port, &new_id, &update)) {
            eprintln!(
                "meshfox node add: added {new_id:?}, but failed to set its extra fields: {e} \
                 (worker on port {port})"
            );
            std::process::exit(1);
        }
    }
    println!(
        "meshfox node add: added {new_id:?} under {parent_id:?} via the running worker on port {port}"
    );
}

/// Reads `--body-file`'s value: `path` itself, unless it's the literal `-`
/// (the same "read stdin instead" sentinel `git`/`tar` use), in which case
/// stdin. Distinct from `node body`'s own `--file`, which already treats
/// *omitting* the flag entirely as "read stdin" — `node add`'s
/// `--body-file` can't reuse that trick, since omitting it here has to
/// mean "no body at all" instead.
fn read_body_source_or_exit(path: &Path) -> String {
    if path == Path::new("-") {
        use std::io::Read;
        let mut buf = String::new();
        std::io::stdin().read_to_string(&mut buf).unwrap_or_else(|e| {
            eprintln!("failed to read stdin: {e}");
            std::process::exit(1);
        });
        buf
    } else {
        std::fs::read_to_string(path).unwrap_or_else(|e| {
            eprintln!("failed to read {}: {e}", path.display());
            std::process::exit(1);
        })
    }
}

/// Pure logic behind `node add`: insert the child, then make sure the
/// result still parses before handing it back to the caller to write.
/// Returns `(updated document, new node's id)`.
#[cfg(test)]
fn apply_node_add(raw: &str, parent_id: &str, title: &str) -> Result<(String, String), String> {
    let (updated, new_id) = mdcanvas::insert_child_node(raw, parent_id, title)
        .ok_or_else(|| format!("no node {parent_id:?}"))?;
    validate_patch(&updated)?;
    Ok((updated, new_id))
}

/// `apply_node_add`, then optionally `apply_node_body`/`apply_node_meta`
/// on the freshly created node in the same pass — `node add
/// --body-file`/the position/style flags, so a node with real starting
/// content doesn't need a separate `node body`/`node meta` follow-up call
/// just to carry its own id across. `fields` is only ever applied when at
/// least one of them was actually given (`NodeMetaFields::is_set`) — a
/// plain `node add` with none of these flags behaves exactly as it always
/// has, byte for byte.
#[cfg(test)]
fn apply_node_add_with_extras(
    raw: &str,
    parent_id: &str,
    title: &str,
    body: Option<&str>,
    fields: NodeMetaFields,
) -> Result<(String, String), String> {
    let (mut updated, new_id) = apply_node_add(raw, parent_id, title)?;
    if let Some(body) = body {
        updated = apply_node_body(&updated, &new_id, body)?;
    }
    if fields.is_set() {
        updated = apply_node_meta(
            &updated,
            &new_id,
            fields.x,
            fields.y,
            fields.width,
            fields.height,
            false, // clear-position never applies to a brand-new node
            fields.color,
            fields.node_type,
            fields.display,
            fields.lang,
            fields.interpreter,
            fields.preview,
            fields.fold,
            fields.tags,
            fields.created_at,
        )?;
    }
    Ok((updated, new_id))
}

fn node_rm(canvas_path: &Path, node_id: &str, keep_children: bool) {
    let runtime = tokio::runtime::Runtime::new().unwrap_or_else(|e| {
        eprintln!("failed to start async runtime: {e}");
        std::process::exit(1);
    });
    let port = worker_port_or_exit(&runtime, canvas_path);
    match runtime.block_on(worker_client::remove_node(port, node_id, keep_children)) {
        Ok(()) => println!(
            "meshfox node rm: deleted {node_id:?}{} via the running worker on port {port}",
            if keep_children { " (children promoted)" } else { "" }
        ),
        Err(e) => {
            eprintln!("meshfox node rm: {e} (worker on port {port})");
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
fn apply_node_rm(raw: &str, node_id: &str, keep_children: bool) -> Result<String, String> {
    let canvas = Canvas::from_markdown(raw).map_err(|e| e.to_string())?;
    let node = canvas
        .node(node_id)
        .ok_or_else(|| format!("no node {node_id:?}"))?;
    if node.parent.is_none() {
        return Err("can't delete the root node".to_string());
    }
    let updated = if keep_children {
        mdcanvas::delete_node_reparent_children(raw, node_id)
    } else {
        mdcanvas::delete_node(raw, node_id)
    }
    .ok_or_else(|| format!("failed to delete {node_id:?}"))?;
    validate_patch(&updated)?;
    Ok(updated)
}

fn node_mv(canvas_path: &Path, node_id: &str, new_parent_id: &str) {
    let runtime = tokio::runtime::Runtime::new().unwrap_or_else(|e| {
        eprintln!("failed to start async runtime: {e}");
        std::process::exit(1);
    });
    let port = worker_port_or_exit(&runtime, canvas_path);
    match runtime.block_on(node_mv_via_worker(port, canvas_path, node_id, new_parent_id)) {
        Ok(()) => println!(
            "meshfox node mv: moved {node_id:?} under {new_parent_id:?} via the running worker on port {port}"
        ),
        Err(e) => {
            eprintln!("meshfox node mv: {e} (worker on port {port})");
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
fn apply_node_mv(raw: &str, node_id: &str, new_parent_id: &str) -> Result<String, String> {
    let canvas = Canvas::from_markdown(raw).map_err(|e| e.to_string())?;
    let node = canvas
        .node(node_id)
        .ok_or_else(|| format!("no node {node_id:?}"))?;
    if node.parent.is_none() {
        return Err("can't move the root node".to_string());
    }
    if canvas.node(new_parent_id).is_none() {
        return Err(format!("no node {new_parent_id:?}"));
    }

    // `reparent_node` only ever promotes an *existing* extra-parent edge to
    // structural parent (see its doc comment) — the web UI's two-step
    // dance (drag a new edge onto the node, then promote it). Add the edge
    // ourselves first so this is a single atomic move from the CLI.
    let mut extra_parents = node.extra_parents.clone();
    if !extra_parents.iter().any(|e| e.from == new_parent_id) {
        extra_parents.push(ExtraEdge::new(new_parent_id));
    }
    let with_edge = mdcanvas::set_node_edges(raw, node_id, &extra_parents)
        .ok_or_else(|| format!("failed to add edge to {node_id:?}"))?;

    // Same position-frame conversion the web UI's own reparent endpoint
    // does (`crates/server/src/lib.rs::reparent_node`) — moving into, out
    // of, or between groups silently flips what a real `x`/`y` *means* (see
    // `Canvas::resolve_absolute_position`) unless corrected here too, since
    // this is a separate front door onto the same `mdcanvas::reparent_node`
    // primitive. Resolve the pre-move absolute position first...
    let abs_before = canvas.resolve_absolute_position(node_id);
    let mut updated =
        mdcanvas::reparent_node(&with_edge, node_id, new_parent_id).ok_or_else(|| {
            format!("can't move {node_id:?} under {new_parent_id:?} (would make the tree cyclic)")
        })?;
    validate_patch(&updated)?;
    // ...then convert it back into whatever frame `node_id` should now
    // store its position in, given its new parent chain. `None` on either
    // side (an unanchored group ancestor in the old or new chain — the
    // common case for a group nobody's ever dragged) leaves the node's
    // stored position untouched, a documented, bounded limitation rather
    // than inventing a synthetic anchor.
    if let Some((abs_x, abs_y)) = abs_before {
        let new_canvas = Canvas::from_markdown(&updated).map_err(|e| e.to_string())?;
        if let Some((local_x, local_y)) = new_canvas.absolute_to_local(node_id, abs_x, abs_y) {
            if let Some(new_node) = new_canvas.node(node_id) {
                if new_node.x != Some(local_x) || new_node.y != Some(local_y) {
                    let meta = NodeMeta {
                        x: Some(local_x),
                        y: Some(local_y),
                        width: new_node.width,
                        height: new_node.height,
                        color: new_node.color.clone(),
                        node_type: None,
                        display: new_node.display,
                        lang: new_node.lang.clone(),
                        interpreter: new_node.interpreter.clone(),
                        preview: Some(new_node.preview),
                        edge_label: new_node.edge_label.clone(),
                        edge_source_side: new_node.edge_source_side,
                        edge_target_side: new_node.edge_target_side,
                        edge_via: new_node.edge_via.clone(),
                        fold: new_node.fold,
                        tags: new_node.tags.clone(),
                        created_at: new_node.created_at.clone(),
                    };
                    if let Some(patched) = mdcanvas::set_node_meta(&updated, node_id, &meta) {
                        updated = patched;
                    }
                }
            }
        }
    }
    Ok(updated)
}

fn node_rename(canvas_path: &Path, node_id: &str, title: &str) {
    let runtime = tokio::runtime::Runtime::new().unwrap_or_else(|e| {
        eprintln!("failed to start async runtime: {e}");
        std::process::exit(1);
    });
    let port = worker_port_or_exit(&runtime, canvas_path);
    let update = worker_client::NodeUpdate {
        title: Some(title.to_string()),
        ..Default::default()
    };
    match runtime.block_on(worker_client::update_node(port, node_id, &update)) {
        Ok(()) => println!(
            "meshfox node rename: renamed {node_id:?} via the running worker on port {port}"
        ),
        Err(e) => {
            eprintln!("meshfox node rename: {e} (worker on port {port})");
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
fn apply_node_rename(raw: &str, node_id: &str, title: &str) -> Result<String, String> {
    let updated = mdcanvas::set_node_title(raw, node_id, title)
        .ok_or_else(|| format!("no node {node_id:?}"))?;
    validate_patch(&updated)?;
    Ok(updated)
}

fn node_set_id(canvas_path: &Path, node_id: &str, new_id: &str) {
    let runtime = tokio::runtime::Runtime::new().unwrap_or_else(|e| {
        eprintln!("failed to start async runtime: {e}");
        std::process::exit(1);
    });
    let port = worker_port_or_exit(&runtime, canvas_path);
    match runtime.block_on(worker_client::rename_node_id(port, node_id, new_id)) {
        Ok(()) => {
            println!(
                "meshfox node set-id: renamed {node_id:?} to {new_id:?} via the running worker on port {port}"
            );
            // Same post-success `deps=` sanity check the direct-file
            // path runs — the server's own `rename-id` endpoint
            // doesn't do this itself, so it's replicated here against
            // a fresh fetch rather than silently dropped.
            if let Ok(raw) = runtime.block_on(worker_client::get_canvas_raw(port)) {
                if let Ok(canvas) = Canvas::from_markdown(&raw) {
                    if let Err(e) = meshfox_core::deps::validate(&canvas) {
                        eprintln!(
                            "meshfox node set-id: warning: {e} (a deps= reference may need fixing by hand — \
                             see `meshfox validate`)"
                        );
                    }
                }
            }
        }
        Err(e) => {
            eprintln!("meshfox node set-id: {e} (worker on port {port})");
            std::process::exit(1);
        }
    }
}

fn node_body(canvas_path: &Path, node_id: &str, file: Option<PathBuf>) {
    let new_body = match &file {
        Some(path) => std::fs::read_to_string(path).unwrap_or_else(|e| {
            eprintln!("failed to read {}: {e}", path.display());
            std::process::exit(1);
        }),
        None => {
            use std::io::Read;
            let mut buf = String::new();
            std::io::stdin()
                .read_to_string(&mut buf)
                .unwrap_or_else(|e| {
                    eprintln!("failed to read stdin: {e}");
                    std::process::exit(1);
                });
            buf
        }
    };

    let runtime = tokio::runtime::Runtime::new().unwrap_or_else(|e| {
        eprintln!("failed to start async runtime: {e}");
        std::process::exit(1);
    });
    let port = worker_port_or_exit(&runtime, canvas_path);
    match runtime.block_on(worker_client::update_node_body(port, node_id, &new_body)) {
        Ok(()) => println!(
            "meshfox node body: updated {node_id:?} via the running worker on port {port}"
        ),
        Err(e) => {
            eprintln!("meshfox node body: {e} (worker on port {port})");
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
fn apply_node_body(raw: &str, node_id: &str, new_body: &str) -> Result<String, String> {
    let updated = mdcanvas::set_node_body(raw, node_id, new_body)
        .ok_or_else(|| format!("no node {node_id:?}"))?;
    validate_patch(&updated)?;
    Ok(updated)
}

fn node_append(canvas_path: &Path, node_id: &str, file: Option<PathBuf>) {
    let addition = match file {
        Some(path) => std::fs::read_to_string(&path).unwrap_or_else(|e| {
            eprintln!("failed to read {}: {e}", path.display());
            std::process::exit(1);
        }),
        None => {
            use std::io::Read;
            let mut buf = String::new();
            std::io::stdin().read_to_string(&mut buf).unwrap_or_else(|e| {
                eprintln!("failed to read stdin: {e}");
                std::process::exit(1);
            });
            buf
        }
    };
    let runtime = tokio::runtime::Runtime::new().unwrap_or_else(|e| {
        eprintln!("failed to start async runtime: {e}");
        std::process::exit(1);
    });
    let port = worker_port_or_exit(&runtime, canvas_path);
    runtime.block_on(worker_client::append_node_body(port, node_id, &addition)).unwrap_or_else(|e| {
        eprintln!("meshfox node append: {e} (worker on port {port})");
        std::process::exit(1);
    });
    println!("meshfox node append: updated {node_id:?} via the worker on port {port}");
}

/// Every `node block` flag except `canvas`/`node_id`/`block_name`
/// themselves — bundled into one struct since there are too many to pass
/// as bare positional arguments legibly. `--x`/`--no-x` pairs stay as two
/// raw `bool`s here; `apply_node_block` is what resolves each pair (and
/// rejects both being given at once) into the `Option<bool>`
/// `FenceAttrsPatch` actually wants.
#[allow(clippy::struct_excessive_bools)]
#[derive(Default)]
struct BlockArgs {
    rename: Option<String>,
    lang: Option<String>,
    cache: bool,
    no_cache: bool,
    always: bool,
    no_always: bool,
    default: bool,
    no_default: bool,
    tty: bool,
    no_tty: bool,
    autoclose: bool,
    no_autoclose: bool,
    service: bool,
    no_service: bool,
    deps: Option<String>,
    clear_deps: bool,
    env: Option<String>,
    clear_env: bool,
    interpreter: Option<String>,
    clear_interpreter: bool,
    code_file: Option<PathBuf>,
}

fn node_block(canvas_path: &Path, node_id: &str, block_name: &str, args: BlockArgs) {
    let code = args.code_file.as_deref().map(read_body_source_or_exit);
    let runtime = tokio::runtime::Runtime::new().unwrap_or_else(|e| {
        eprintln!("failed to start async runtime: {e}");
        std::process::exit(1);
    });
    let port = worker_port_or_exit(&runtime, canvas_path);
    let update = match block_attrs_update_from_args(&args, code.as_deref()) {
        Ok(u) => u,
        Err(e) => {
            eprintln!("meshfox node block: {e}");
            std::process::exit(1);
        }
    };
    match runtime.block_on(worker_client::set_block_attrs(port, node_id, block_name, &update)) {
        Ok(()) => println!(
            "meshfox node block: updated {block_name:?} in {node_id:?} via the running worker on port {port}"
        ),
        Err(e) => {
            eprintln!("meshfox node block: {e} (worker on port {port})");
            std::process::exit(1);
        }
    }
}

/// Resolves one `--x`/`--no-x` pair to `Some(true)`/`Some(false)`/`None`
/// ("not touched") — rejects both being set in the same call, the one
/// shape that would otherwise silently pick a winner.
fn resolve_bool_pair(on: bool, off: bool, on_flag: &str, off_flag: &str) -> Result<Option<bool>, String> {
    match (on, off) {
        (true, true) => Err(format!("{on_flag} is mutually exclusive with {off_flag}")),
        (true, false) => Ok(Some(true)),
        (false, true) => Ok(Some(false)),
        (false, false) => Ok(None),
    }
}

/// Builds a `worker_client::BlockAttrsUpdate` from `BlockArgs`, resolving
/// each `--x`/`--no-x` pair and the `--deps`/`--clear-deps`,
/// `--env`/`--clear-env`, `--interpreter`/`--clear-interpreter` mutual-
/// exclusion checks the same way `apply_node_block` does — `deps`/`env`
/// are sent as the same raw comma-separated text those flags already are
/// (see `BlockAttrsUpdate`'s own doc comment for why), not pre-parsed into
/// a typed list, so there's nothing to convert here beyond the
/// `--clear-*` flags' own "send an empty string" convention.
fn block_attrs_update_from_args(
    args: &BlockArgs,
    code: Option<&str>,
) -> Result<worker_client::BlockAttrsUpdate, String> {
    let cache = resolve_bool_pair(args.cache, args.no_cache, "--cache", "--no-cache")?;
    let always = resolve_bool_pair(args.always, args.no_always, "--always", "--no-always")?;
    let default = resolve_bool_pair(args.default, args.no_default, "--default", "--no-default")?;
    let tty = resolve_bool_pair(args.tty, args.no_tty, "--tty", "--no-tty")?;
    let autoclose = resolve_bool_pair(
        args.autoclose,
        args.no_autoclose,
        "--autoclose",
        "--no-autoclose",
    )?;
    let service = resolve_bool_pair(args.service, args.no_service, "--service", "--no-service")?;

    if args.deps.is_some() && args.clear_deps {
        return Err("--deps is mutually exclusive with --clear-deps".to_string());
    }
    let deps = if args.clear_deps {
        Some(String::new())
    } else {
        args.deps.clone()
    };

    if args.env.is_some() && args.clear_env {
        return Err("--env is mutually exclusive with --clear-env".to_string());
    }
    let env = if args.clear_env {
        Some(String::new())
    } else {
        args.env.clone()
    };

    if args.interpreter.is_some() && args.clear_interpreter {
        return Err("--interpreter is mutually exclusive with --clear-interpreter".to_string());
    }

    Ok(worker_client::BlockAttrsUpdate {
        name: args.rename.clone(),
        lang: args.lang.clone(),
        cache,
        always,
        default,
        tty,
        autoclose,
        service,
        deps,
        env,
        interpreter: args.interpreter.clone(),
        clear_interpreter: args.clear_interpreter,
        code: code.map(str::to_string),
    })
}

/// Pure logic behind `node block`: resolve every flag pair, look up the
/// target block (for error messages precise enough to say *which* of
/// node-id/block-name didn't resolve), build the patch, apply it, and — if
/// `--deps`/`--clear-deps` actually touched the dependency list this call —
/// validate the whole resulting document's `deps=` graph right away
/// (`deps::validate`: dangling targets, cycles) rather than leaving that
/// for a separate `meshfox validate`.
#[cfg(test)]
fn apply_node_block(
    raw: &str,
    node_id: &str,
    block_name: &str,
    args: &BlockArgs,
    code: Option<&str>,
) -> Result<String, String> {
    let canvas = Canvas::from_markdown(raw).map_err(|e| e.to_string())?;
    let node = canvas
        .node(node_id)
        .ok_or_else(|| format!("no node {node_id:?}"))?;
    if !meshfox_core::scan_runnable_blocks(node_id, &node.text)
        .iter()
        .any(|b| b.name.as_deref() == Some(block_name))
    {
        return Err(format!(
            "no runnable code block named {block_name:?} in node {node_id:?}"
        ));
    }

    let cache = resolve_bool_pair(args.cache, args.no_cache, "--cache", "--no-cache")?;
    let always = resolve_bool_pair(args.always, args.no_always, "--always", "--no-always")?;
    let default = resolve_bool_pair(args.default, args.no_default, "--default", "--no-default")?;
    let tty = resolve_bool_pair(args.tty, args.no_tty, "--tty", "--no-tty")?;
    let autoclose = resolve_bool_pair(
        args.autoclose,
        args.no_autoclose,
        "--autoclose",
        "--no-autoclose",
    )?;
    let service = resolve_bool_pair(args.service, args.no_service, "--service", "--no-service")?;

    if args.deps.is_some() && args.clear_deps {
        return Err("--deps is mutually exclusive with --clear-deps".to_string());
    }
    let deps_touched = args.deps.is_some() || args.clear_deps;
    let deps = if args.clear_deps {
        Some(Vec::new())
    } else {
        args.deps.as_deref().map(meshfox_core::parse_deps_list)
    };

    if args.env.is_some() && args.clear_env {
        return Err("--env is mutually exclusive with --clear-env".to_string());
    }
    let env = if args.clear_env {
        Some(Vec::new())
    } else {
        args.env.as_deref().map(meshfox_core::parse_env_list)
    };

    if args.interpreter.is_some() && args.clear_interpreter {
        return Err("--interpreter is mutually exclusive with --clear-interpreter".to_string());
    }
    let interpreter = if args.clear_interpreter {
        Some(None)
    } else {
        args.interpreter.clone().map(Some)
    };

    let patch = FenceAttrsPatch {
        name: args.rename.clone(),
        lang: args.lang.clone(),
        cache,
        always,
        default,
        tty,
        autoclose,
        service,
        deps,
        env,
        interpreter,
        code: code.map(str::to_string),
    };
    let updated = mdcanvas::set_fence_attrs(raw, node_id, block_name, &patch).ok_or_else(|| {
        format!("no runnable code block named {block_name:?} in node {node_id:?}")
    })?;
    validate_patch(&updated)?;

    if deps_touched {
        let updated_canvas = Canvas::from_markdown(&updated).map_err(|e| e.to_string())?;
        meshfox_core::deps::validate(&updated_canvas).map_err(|e| e.to_string())?;
    }

    Ok(updated)
}

fn parse_node_type(s: &str) -> Result<NodeType, String> {
    match s {
        "text" => Ok(NodeType::Text),
        "file" => Ok(NodeType::File),
        "link" => Ok(NodeType::Link),
        "group" => Ok(NodeType::Group),
        "include" => Ok(NodeType::Include),
        _ => Err(format!(
            "unknown --type {s:?} (expected text/file/link/group/include)"
        )),
    }
}

fn parse_display(s: &str) -> Result<FileDisplay, String> {
    match s {
        "link" => Ok(FileDisplay::Link),
        "code" => Ok(FileDisplay::Code),
        _ => Err(format!("unknown --display {s:?} (expected link/code)")),
    }
}

/// Builds a `worker_client::NodeUpdate` from `NodeMetaFields` (plus an
/// optional new body, for `node add`'s own `--body-file`), parsing/
/// validating `--type`/`--display`/`--tags` the same way the direct-file
/// `apply_node_meta` does. Unlike that function, this never fetches the
/// node's *current* value for anything — `PATCH /api/nodes/:id` already
/// merges each sent field with whatever the node currently has server-side
/// (`req.x.or(existing_x)`, etc. — see `UpdateNodeRequest`'s own doc
/// comment), so there's nothing left for a client to merge itself.
/// Deliberately doesn't replicate `apply_node_meta`'s own "`--preview`
/// only applies to link nodes" upfront rejection — checking that would
/// need the node's current type, which (unlike every other field here)
/// isn't knowable without a separate fetch; the server just silently
/// drops `preview` for a non-link node instead, which is a harmless no-op,
/// not data loss.
fn node_update_from_fields(
    fields: &NodeMetaFields,
    body: Option<&str>,
) -> Result<worker_client::NodeUpdate, String> {
    let node_type = fields.node_type.as_deref().map(parse_node_type).transpose()?;
    let display = fields.display.as_deref().map(parse_display).transpose()?;
    let tags = match &fields.tags {
        None => None,
        Some(s) => {
            validate_tags_input(s)?;
            Some(meshfox_core::parse_tags(Some(s)))
        }
    };
    Ok(worker_client::NodeUpdate {
        text: body.map(str::to_string),
        x: fields.x,
        y: fields.y,
        width: fields.width,
        height: fields.height,
        color: fields.color.clone(),
        node_type,
        display,
        lang: fields.lang.clone(),
        interpreter: fields.interpreter.clone(),
        preview: fields.preview,
        fold: fields.fold.clone(),
        tags,
        created_at: fields.created_at.clone(),
        ..Default::default()
    })
}

#[allow(clippy::too_many_arguments)]
fn node_meta(
    canvas_path: &Path,
    node_id: &str,
    x: Option<f64>,
    y: Option<f64>,
    width: Option<f64>,
    height: Option<f64>,
    clear_position: bool,
    color: Option<String>,
    node_type: Option<String>,
    display: Option<String>,
    lang: Option<String>,
    interpreter: Option<String>,
    preview: Option<bool>,
    fold: Option<String>,
    tags: Option<String>,
    created_at: Option<String>,
) {
    let fields = NodeMetaFields {
        x, y, width, height, color, node_type, display, lang, interpreter,
        preview, fold, tags, created_at,
    };
    let runtime = tokio::runtime::Runtime::new().unwrap_or_else(|e| {
        eprintln!("failed to start async runtime: {e}");
        std::process::exit(1);
    });
    let port = worker_port_or_exit(&runtime, canvas_path);
    let mut update = node_update_from_fields(&fields, None).unwrap_or_else(|e| {
        eprintln!("meshfox node meta: {e}");
        std::process::exit(1);
    });
    update.clear_position = clear_position;
    runtime.block_on(worker_client::update_node(port, node_id, &update)).unwrap_or_else(|e| {
        eprintln!("meshfox node meta: {e} (worker on port {port})");
        std::process::exit(1);
    });
    println!("meshfox node meta: updated {node_id:?} via the running worker on port {port}");
}

/// Rejects a `--tags`/`tags` value carrying characters that were never a
/// legitimate tag — `parse_tags` only ever splits on commas, so a value
/// that's accidentally JSON (`["foo","bar"]`, or an array whose elements
/// got joined by a raw newline instead of `, `) doesn't fail: it just gets
/// folded into one mangled tag with the stray brackets/quotes/newline baked
/// in. Catching it here turns that silent corruption into an immediate,
/// actionable error instead.
fn validate_tags_input(s: &str) -> Result<(), String> {
    if let Some(c) = s.chars().find(|c| c.is_control()) {
        return Err(format!(
            "--tags contains a control character ({c:?}) — expected plain comma-separated tags, e.g. \"foo,bar\", not a JSON-encoded list"
        ));
    }
    if let Some(c) = s.chars().find(|c| matches!(c, '"' | '\'' | '[' | ']' | '{' | '}')) {
        return Err(format!(
            "--tags contains {c:?}, which looks like accidental JSON — expected plain comma-separated tags, e.g. \"foo,bar\""
        ));
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
#[cfg(test)]
fn apply_node_meta(
    raw: &str,
    node_id: &str,
    x: Option<f64>,
    y: Option<f64>,
    width: Option<f64>,
    height: Option<f64>,
    clear_position: bool,
    color: Option<String>,
    node_type: Option<String>,
    display: Option<String>,
    lang: Option<String>,
    interpreter: Option<String>,
    preview: Option<bool>,
    fold: Option<String>,
    tags: Option<String>,
    created_at: Option<String>,
) -> Result<String, String> {
    let canvas = Canvas::from_markdown(raw).map_err(|e| e.to_string())?;
    let node = canvas
        .node(node_id)
        .ok_or_else(|| format!("no node {node_id:?}"))?;
    if let Some(v) = &created_at {
        if !meshfox_core::timestamp::is_valid_rfc3339(v) {
            return Err(format!(
                "invalid --created-at {v:?} — expected RFC3339, e.g. \"2026-08-29T10:15:00Z\""
            ));
        }
    }

    if clear_position && (x.is_some() || y.is_some() || width.is_some() || height.is_some()) {
        return Err(
            "--clear-position is mutually exclusive with --x/--y/--w/--h".to_string(),
        );
    }

    let parsed_type = node_type.as_deref().map(parse_node_type).transpose()?;
    let parsed_display = display.as_deref().map(parse_display).transpose()?;
    if preview.is_some() && parsed_type.unwrap_or(node.node_type) != NodeType::Link {
        return Err("--preview only applies to link nodes".to_string());
    }
    // Omitted entirely (`None`) keeps whatever's already there; passed as
    // `"true"`/`"false"`/`"default"` resolves via the same shared sentinel
    // parsing the server's own node-update endpoint uses (see
    // `mdcanvas::parse_fold_override`).
    let parsed_fold = match fold.as_deref() {
        None => node.fold,
        Some(s) => meshfox_core::parse_fold_override(s)?,
    };
    // Same "omitted keeps current, given replaces outright" contract as
    // `--from` on `node edges` — `--tags ""` parses to an empty list (see
    // `meshfox_core::parse_tags`), clearing every tag rather than being a
    // no-op.
    let parsed_tags = match &tags {
        None => node.tags.clone(),
        Some(s) => {
            validate_tags_input(s)?;
            meshfox_core::parse_tags(Some(s))
        }
    };

    // A group's *size* is always derived from its children, never stored —
    // so reject an explicit `--w`/`--h` for one, whether it's already a
    // group or is becoming one with this call. Its
    // own *position*, though, is a real anchor a member's own `x`/`y` is
    // relative to (see `Canvas::resolve_absolute_position`), so `--x`/`--y`
    // is allowed on a group same as any other node.
    let is_group = parsed_type.unwrap_or(node.node_type) == NodeType::Group;
    if is_group && (width.is_some() || height.is_some()) {
        return Err(
            "group nodes never store a size — their box is always derived from their children"
                .to_string(),
        );
    }

    // `set_node_meta` only carries forward whatever wasn't explicitly
    // passed for `type` (and always for `parent`) — every other field left
    // `None` here would simply be omitted from the rewritten line instead
    // of preserved, so any field not given on the command line is filled
    // in from the node's current value. A group's own width/height stay
    // forced to `None` (omitted) regardless of what the node's current
    // value happens to be, same invariant the rejection above enforces on
    // the command-line side.
    let meta = NodeMeta {
        x: if clear_position { None } else { x.or(node.x) },
        y: if clear_position { None } else { y.or(node.y) },
        width: if is_group || clear_position {
            None
        } else {
            width.or(node.width)
        },
        height: if is_group || clear_position {
            None
        } else {
            height.or(node.height)
        },
        color: color.or_else(|| node.color.clone()),
        node_type: parsed_type,
        display: parsed_display.or(node.display),
        lang: lang.or_else(|| node.lang.clone()),
        interpreter: interpreter.or_else(|| node.interpreter.clone()),
        preview: Some(preview.unwrap_or(node.preview)),
        edge_label: node.edge_label.clone(),
        edge_source_side: node.edge_source_side,
        edge_target_side: node.edge_target_side,
        edge_via: node.edge_via.clone(),
        fold: parsed_fold,
        tags: parsed_tags,
        created_at: created_at.or_else(|| node.created_at.clone()),
    };
    let updated = mdcanvas::set_node_meta(raw, node_id, &meta)
        .ok_or_else(|| format!("no node {node_id:?}"))?;
    validate_patch(&updated)?;
    Ok(updated)
}

fn node_edges(canvas_path: &Path, node_id: &str, from: Vec<String>, clear: bool) {
    let extra_parents: Vec<String> = if clear { Vec::new() } else { from };
    let runtime = tokio::runtime::Runtime::new().unwrap_or_else(|e| {
        eprintln!("failed to start async runtime: {e}");
        std::process::exit(1);
    });
    let port = worker_port_or_exit(&runtime, canvas_path);
    let edges: Vec<ExtraEdge> = extra_parents.iter().map(|p| ExtraEdge::new(p.as_str())).collect();
    let update = worker_client::NodeUpdate {
        extra_parents: Some(edges),
        ..Default::default()
    };
    match runtime.block_on(worker_client::update_node(port, node_id, &update)) {
        Ok(()) => println!(
            "meshfox node edges: set {} extra parent(s) on {node_id:?} via the running worker on port {port}",
            extra_parents.len()
        ),
        Err(e) => {
            eprintln!("meshfox node edges: {e} (worker on port {port})");
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
fn apply_node_edges(raw: &str, node_id: &str, extra_parents: &[String]) -> Result<String, String> {
    let edges: Vec<ExtraEdge> = extra_parents
        .iter()
        .map(|p| ExtraEdge::new(p.as_str()))
        .collect();
    let updated = mdcanvas::set_node_edges(raw, node_id, &edges)
        .ok_or_else(|| format!("no node {node_id:?}"))?;
    validate_patch(&updated)?;
    Ok(updated)
}

fn node_move(canvas_path: &Path, node_id: &str, before: Option<String>, after: Option<String>) {
    let runtime = tokio::runtime::Runtime::new().unwrap_or_else(|e| {
        eprintln!("failed to start async runtime: {e}");
        std::process::exit(1);
    });
    let port = worker_port_or_exit(&runtime, canvas_path);
    let (target_id, is_before) = match (&before, &after) {
        (Some(t), None) => (t.clone(), true),
        (None, Some(t)) => (t.clone(), false),
        (None, None) => {
            eprintln!("meshfox node move: exactly one of --before/--after is required");
            std::process::exit(1);
        }
        (Some(_), Some(_)) => {
            eprintln!("meshfox node move: --before and --after are mutually exclusive");
            std::process::exit(1);
        }
    };
    match runtime.block_on(worker_client::move_sibling(port, node_id, &target_id, is_before)) {
        Ok(()) => println!(
            "meshfox node move: moved {node_id:?} {} {target_id:?} via the running worker on port {port}",
            if is_before { "before" } else { "after" }
        ),
        Err(e) => {
            eprintln!("meshfox node move: {e} (worker on port {port})");
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
fn apply_node_move(
    raw: &str,
    node_id: &str,
    before: Option<&str>,
    after: Option<&str>,
) -> Result<(String, String, &'static str), String> {
    let (target_id, position, label) = match (before, after) {
        (Some(t), None) => (t, mdcanvas::MoveSiblingPosition::Before, "before"),
        (None, Some(t)) => (t, mdcanvas::MoveSiblingPosition::After, "after"),
        (None, None) => return Err("exactly one of --before/--after is required".to_string()),
        (Some(_), Some(_)) => return Err("--before and --after are mutually exclusive".to_string()),
    };
    let updated =
        mdcanvas::move_sibling(raw, node_id, target_id, position).map_err(|e| e.to_string())?;
    validate_patch(&updated)?;
    Ok((updated, target_id.to_string(), label))
}

fn node_reorder(canvas_path: &Path) {
    let runtime = tokio::runtime::Runtime::new().unwrap_or_else(|e| {
        eprintln!("failed to start async runtime: {e}");
        std::process::exit(1);
    });
    let port = worker_port_or_exit(&runtime, canvas_path);
    match runtime.block_on(worker_client::reorder_document(port)) {
        Ok(()) => println!(
            "meshfox node reorder: resynced sibling order via the running worker on port {port}"
        ),
        Err(e) => {
            eprintln!("meshfox node reorder: {e} (worker on port {port})");
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
fn apply_node_reorder(raw: &str) -> Result<String, String> {
    // No client-side auto-layout to hint from here (a bare CLI/MCP call,
    // not the web UI's own drag) — same "unpositioned siblings sort last,
    // stable" behavior `reorder_by_position` always had.
    let updated = mdcanvas::reorder_by_position(raw, &std::collections::HashMap::new())
        .ok_or_else(|| "failed to parse".to_string())?;
    validate_patch(&updated)?;
    Ok(updated)
}

fn node_show(canvas_path: &Path, node_id: &str) {
    let raw = read_raw_or_exit(canvas_path);
    match format_node_show(&raw, node_id) {
        Ok(text) => print!("{text}"),
        Err(e) => {
            eprintln!("meshfox node show: {e} ({})", canvas_path.display());
            std::process::exit(1);
        }
    }
}

/// The read-only `node show` report for one node, as plain text (one
/// trailing newline per line, so the caller can just `print!` it as-is).
fn format_node_show(raw: &str, node_id: &str) -> Result<String, String> {
    let canvas = Canvas::from_markdown(raw).map_err(|e| e.to_string())?;
    let node = canvas
        .node(node_id)
        .ok_or_else(|| format!("no node {node_id:?}"))?;

    let mut out = String::new();
    out.push_str(&format!("id: {}\n", node.id));
    out.push_str(&format!("title: {}\n", node.title));
    out.push_str(&format!("type: {}\n", node.node_type.as_str()));
    out.push_str(&format!(
        "parent: {}\n",
        node.parent.as_deref().unwrap_or("(root)")
    ));
    let children: Vec<&str> = canvas
        .children(&node.id)
        .iter()
        .map(|n| n.id.as_str())
        .collect();
    out.push_str(&format!(
        "children: {}\n",
        if children.is_empty() {
            "(none)".to_string()
        } else {
            children.join(", ")
        }
    ));
    out.push_str(&format!(
        "extra parents: {}\n",
        if node.extra_parents.is_empty() {
            "(none)".to_string()
        } else {
            node.extra_parents
                .iter()
                .map(|e| e.from.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        }
    ));
    out.push_str(&format!(
        "position: x={} y={} w={} h={}\n",
        fmt_opt_num(node.x),
        fmt_opt_num(node.y),
        fmt_opt_num(node.width),
        fmt_opt_num(node.height)
    ));
    // A direct child of a `group` stores x/y relative to that group's own
    // anchor, not absolute (see SPEC.md) — spelling out the resolved
    // absolute position too avoids `position: x=... y=...` above reading
    // as document-absolute when it isn't. Omitted for anything not nested
    // under a group (there, `resolve_absolute_position` is exactly the raw
    // x/y already shown, so repeating it would just be noise), and for a
    // node whose group ancestor has no anchor of its own yet (nothing to
    // resolve against).
    if node.parent.as_deref().is_some_and(|p| {
        canvas
            .node(p)
            .is_some_and(|n| n.node_type == NodeType::Group)
    }) {
        if let Some((abs_x, abs_y)) = canvas.resolve_absolute_position(node_id) {
            out.push_str(&format!(
                "resolved position (absolute): x={abs_x} y={abs_y}\n"
            ));
        }
    }
    if let Some(c) = &node.created_at {
        out.push_str(&format!("created: {c}\n"));
    }
    if let Some(u) = &node.updated_at {
        out.push_str(&format!("updated: {u}\n"));
    }
    if let Some(c) = &node.color {
        out.push_str(&format!("color: {c}\n"));
    }
    if !node.tags.is_empty() {
        out.push_str(&format!("tags: {}\n", node.tags.join(", ")));
    }
    if let Some(t) = &node.target {
        out.push_str(&format!("target: {t}\n"));
    }
    if let Some(d) = node.display {
        out.push_str(&format!("display: {}\n", d.as_str()));
    }
    if node.preview {
        out.push_str("preview: true\n");
    }
    if let Some(l) = &node.lang {
        out.push_str(&format!("lang: {l}\n"));
    }
    Ok(out)
}

fn fmt_opt_num(v: Option<f64>) -> String {
    v.map(|n| n.to_string()).unwrap_or_else(|| "?".to_string())
}

#[allow(clippy::too_many_arguments)]
fn node_find(
    canvas_path: &Path,
    selector: &str,
    show: bool,
    text: Option<&str>,
    created_after: Option<&str>,
    created_before: Option<&str>,
    updated_after: Option<&str>,
    updated_before: Option<&str>,
    since: Option<&str>,
) {
    let raw = read_raw_or_exit(canvas_path);
    let canvas = Canvas::from_markdown(&raw).unwrap_or_else(|e| {
        eprintln!("failed to parse {}: {e}", canvas_path.display());
        std::process::exit(1);
    });
    let ids = find_node_ids(&canvas, selector).unwrap_or_else(|e| {
        eprintln!("meshfox node find: {e}");
        std::process::exit(1);
    });
    let ids = filter_by_text(&canvas, ids, text);
    let ids = filter_by_dates(
        &canvas,
        ids,
        created_after,
        created_before,
        updated_after,
        updated_before,
        since,
    )
    .unwrap_or_else(|e| {
        eprintln!("meshfox node find: {e}");
        std::process::exit(1);
    });
    if ids.is_empty() {
        println!("meshfox node find: no matches for {selector:?}");
        return;
    }
    for id in &ids {
        let excerpt = text.and_then(|needle| canvas.node(id).and_then(|n| excerpt_for(n, needle)));
        if show {
            match format_node_show(&raw, id) {
                Ok(report) => {
                    println!("=== {id} ===");
                    print!("{report}");
                    if let Some(e) = &excerpt {
                        println!("matched: {e}");
                    }
                }
                Err(e) => eprintln!("meshfox node find: {e}"),
            }
        } else if let Some(e) = &excerpt {
            println!("{id}: {e}");
        } else {
            println!("{id}");
        }
    }
}

/// Case-insensitive substring filter over `ids` against each node's own
/// `title`/`text` — the second, independent axis `node find` layers next
/// to the CSS selector (see `NodeCommand::Find`'s doc comment for why this
/// isn't a CSS extension). `None` (no `--text` given) is a no-op.
fn filter_by_text(canvas: &Canvas, ids: Vec<String>, text: Option<&str>) -> Vec<String> {
    let Some(needle) = text else {
        return ids;
    };
    let needle_lower = needle.to_lowercase();
    ids.into_iter()
        .filter(|id| {
            canvas.node(id).is_some_and(|n| {
                n.title.to_lowercase().contains(&needle_lower)
                    || n.text.to_lowercase().contains(&needle_lower)
            })
        })
        .collect()
}

/// The third axis: keeps only nodes whose `created_at`/`updated_at` fall
/// in the requested range(s) — every given bound is resolved via
/// `meshfox_core::timestamp::parse_since` (RFC3339 or a relative duration
/// like `7d`) and ANDed together with whatever `filter_by_text` already
/// narrowed down. `after` is inclusive (`ts >= x`), `before` is exclusive
/// (`ts < x`), so `--updated-after A --updated-before B` never double-
/// counts the boundary. A node missing the timestamp a given bound checks
/// never satisfies that bound — "unset" isn't "in range" (see
/// `NodeCommand::Find`'s doc comment).
#[allow(clippy::too_many_arguments)]
fn filter_by_dates(
    canvas: &Canvas,
    ids: Vec<String>,
    created_after: Option<&str>,
    created_before: Option<&str>,
    updated_after: Option<&str>,
    updated_before: Option<&str>,
    since: Option<&str>,
) -> Result<Vec<String>, String> {
    let parse = |s: Option<&str>, flag: &str| -> Result<Option<i64>, String> {
        s.map(|s| {
            meshfox_core::timestamp::parse_since(s).ok_or_else(|| {
                format!(
                    "invalid {flag} {s:?} — expected RFC3339 or a relative duration \
                     like \"7d\"/\"2w\"/\"1h\""
                )
            })
        })
        .transpose()
    };
    let created_after = parse(created_after, "--created-after")?;
    let created_before = parse(created_before, "--created-before")?;
    let updated_after = parse(updated_after, "--updated-after")?;
    let updated_before = parse(updated_before, "--updated-before")?;
    let since = parse(since, "--since")?;

    if created_after.is_none()
        && created_before.is_none()
        && updated_after.is_none()
        && updated_before.is_none()
        && since.is_none()
    {
        return Ok(ids);
    }

    Ok(ids
        .into_iter()
        .filter(|id| {
            let Some(node) = canvas.node(id) else {
                return false;
            };
            let c_ts = node
                .created_at
                .as_deref()
                .and_then(meshfox_core::timestamp::unix_timestamp);
            let u_ts = node
                .updated_at
                .as_deref()
                .and_then(meshfox_core::timestamp::unix_timestamp);

            if let Some(x) = created_after {
                if !c_ts.is_some_and(|t| t >= x) {
                    return false;
                }
            }
            if let Some(x) = created_before {
                if !c_ts.is_some_and(|t| t < x) {
                    return false;
                }
            }
            if let Some(x) = updated_after {
                if !u_ts.is_some_and(|t| t >= x) {
                    return false;
                }
            }
            if let Some(x) = updated_before {
                if !u_ts.is_some_and(|t| t < x) {
                    return false;
                }
            }
            if let Some(x) = since {
                let touched =
                    c_ts.is_some_and(|t| t >= x) || u_ts.is_some_and(|t| t >= x);
                if !touched {
                    return false;
                }
            }
            true
        })
        .collect())
}

/// A short, single-line preview of why `node` matched a `--text` query —
/// the first line (checking its title, then its body) that contains
/// `needle` case-insensitively, truncated to a sane length. `None` only
/// if `needle` doesn't actually appear in either — shouldn't happen for a
/// node that already passed `filter_by_text`, but this is reused
/// defensively rather than assumed.
fn excerpt_for(node: &Node, needle: &str) -> Option<String> {
    let needle_lower = needle.to_lowercase();
    let candidate = if node.title.to_lowercase().contains(&needle_lower) {
        node.title.as_str()
    } else {
        node.text
            .lines()
            .find(|line| line.to_lowercase().contains(&needle_lower))?
    };
    const MAX_CHARS: usize = 100;
    let trimmed = candidate.trim();
    if trimmed.chars().count() > MAX_CHARS {
        Some(format!(
            "{}…",
            trimmed.chars().take(MAX_CHARS).collect::<String>()
        ))
    } else {
        Some(trimmed.to_string())
    }
}

/// Pure logic behind `node find`: build a synthetic HTML skeleton of the
/// canvas tree (id/class/attrs only, never real node content — see
/// `canvas_node_html`) and match `selector` against it with `scraper`'s
/// CSS engine, the same one a browser uses — see `NodeCommand::Find`'s own
/// doc comment for the id/tag/type/color/nesting mapping this relies on.
/// Matches come back in document order (`scraper::Html::select`'s own
/// order, which walks the synthetic tree depth-first — the same order
/// `Canvas::children` built it in).
fn find_node_ids(canvas: &Canvas, selector: &str) -> Result<Vec<String>, String> {
    let Some(root) = canvas.nodes.iter().find(|n| n.parent.is_none()) else {
        return Ok(Vec::new());
    };
    let html = format!(
        "<html><body>{}</body></html>",
        canvas_node_html(canvas, root)
    );
    let document = scraper::Html::parse_document(&html);
    let parsed = scraper::Selector::parse(selector)
        .map_err(|_| format!("invalid CSS selector {selector:?}"))?;
    Ok(document
        .select(&parsed)
        .filter_map(|el| el.value().attr("id").map(str::to_string))
        .collect())
}

/// One node, and (recursively) its structural children — `Canvas::children`,
/// the same nesting `run`/`node show` already address by — rendered as a
/// `<div>`: `id` is the node's own id, `class` is its tags (space-joined,
/// CSS's own native multi-value-attribute shape — a direct fit for
/// `tags="a,b,c"`), plus `type`/`color` as ordinary attributes. Never the
/// node's title or body text — this is a structural skeleton for matching
/// against, not a rendering.
fn canvas_node_html(canvas: &Canvas, node: &Node) -> String {
    let mut attrs = format!(" id=\"{}\"", html_escape_attr(&node.id));
    if !node.tags.is_empty() {
        attrs.push_str(&format!(
            " class=\"{}\"",
            html_escape_attr(&node.tags.join(" "))
        ));
    }
    attrs.push_str(&format!(
        " type=\"{}\"",
        html_escape_attr(node.node_type.as_str())
    ));
    if let Some(c) = &node.color {
        attrs.push_str(&format!(" color=\"{}\"", html_escape_attr(c)));
    }
    let children: String = canvas
        .children(&node.id)
        .into_iter()
        .map(|child| canvas_node_html(canvas, child))
        .collect();
    format!("<div{attrs}>{children}</div>")
}

/// Just enough escaping for a value dropped into a double-quoted HTML
/// attribute — a node id/tag/color containing `&`/`"`/`<`/`>` (unusual,
/// but not disallowed) would otherwise corrupt the synthetic markup
/// `find_node_ids` builds around it.
fn html_escape_attr(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Basename of a template's own optional config file (`--template`'s own
/// directory, never a page — see `load_template_config`) — excluded by name
/// from `copy_template_assets`'s otherwise-copy-everything sweep the same
/// way a `.tera` file already is, since it configures the export rather
/// than being part of it.
const TEMPLATE_CONFIG_FILE: &str = "template.toml";

/// A template's own settings, read from `template.toml` in its directory —
/// deliberately not a `--static` CLI flag: both fields are a property of
/// *this template* (how it wants relative links resolved, which icons it
/// ships), not something a caller picks per invocation, so they belong
/// checked into the template alongside its own `.tera`/CSS files instead of
/// repeated on every command line that uses it. Both are optional — a
/// template with no `template.toml` at all gets `Default::default()`
/// (no `base_url`, no `icons`), same as today's behavior before this file
/// existed.
#[derive(Debug, Default, serde::Deserialize)]
struct TemplateConfig {
    /// Prefixed onto a relative link/target `static` doesn't already copy
    /// into `--out` (a plain Markdown link, or a `file`/`link` node's own
    /// target when not `display="code"`) — a local image and a
    /// `display="code"` target are unaffected, since they're already
    /// self-contained in the output. Useful when publishing to a host
    /// where the site's own root isn't the canvas's own directory, so a
    /// leftover relative reference should resolve against e.g. the
    /// original repo instead. Left as-is (`None`) when the template
    /// doesn't set one.
    #[serde(default)]
    base_url: Option<String>,
    /// `<link>` tags for the page's own icons (favicon, apple-touch-icon,
    /// ...) — exposed to every template as the `icons` context key so
    /// `index.html.tera` (or any other page) can render them itself; see
    /// `IconLink`. Each `href` is expected to be a relative path to a file
    /// this same template directory actually ships (copied to `--out`
    /// verbatim by `copy_template_assets`, same as any other asset) —
    /// nothing here fetches or copies an icon from anywhere else.
    #[serde(default)]
    icons: Vec<IconLink>,
}

/// One `<link rel="..." href="...">` icon tag — see `TemplateConfig::icons`.
/// Mirrors the shape of a real `<link>` element closely enough that a
/// template can render one directly from each entry's fields, e.g.:
/// `<link rel="{{ icon.rel }}" href="{{ icon.href }}">`.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
struct IconLink {
    /// `"icon"` or `"apple-touch-icon"`, same as the real attribute.
    rel: String,
    /// Relative path (from the rendered page) to the icon file itself.
    href: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    sizes: Option<String>,
    /// Named `mime_type` (not `type`, a reserved word awkward to use as a
    /// Rust field) but serialized/read back as plain `type` — both in
    /// `template.toml` and in the Tera context a template reads it from —
    /// since that's the real HTML attribute name.
    #[serde(default, rename = "type", skip_serializing_if = "Option::is_none")]
    mime_type: Option<String>,
}

/// Reads `template_dir`'s own `template.toml`, if it has one — a template
/// with none gets `TemplateConfig::default()` (no `base_url`, no `icons`),
/// exactly today's behavior before this file existed. A `template.toml`
/// that exists but fails to parse is a hard error (same "fail loud, don't
/// silently fall back" stance every other malformed-input path in this CLI
/// takes), not silently ignored.
fn load_template_config(template_dir: &Path) -> TemplateConfig {
    let path = template_dir.join(TEMPLATE_CONFIG_FILE);
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return TemplateConfig::default();
    };
    toml::from_str(&raw).unwrap_or_else(|e| {
        eprintln!("meshfox static: failed to parse {}: {e}", path.display());
        std::process::exit(1);
    })
}

/// `canvas_dir`'s repo HEAD, short form (e.g. `"a1b2c3d"`) — `None` when
/// `canvas_dir` isn't inside a git working tree, or `git` itself isn't
/// installed. A canvas exported outside any repo (or by a build machine
/// without `git`) still gets a static site; it just has no `canvas_commit`
/// for the template to show.
fn canvas_git_commit(canvas_dir: &Path) -> Option<String> {
    let output = std::process::Command::new("git")
        .args([
            "-C",
            &canvas_dir.to_string_lossy(),
            "rev-parse",
            "--short",
            "HEAD",
        ])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let commit = String::from_utf8(output.stdout).ok()?.trim().to_string();
    if commit.is_empty() {
        None
    } else {
        Some(commit)
    }
}

/// Every `node` subcommand's last step before handing a patch back to be
/// written: make sure it still parses — the same validate-before-commit
/// shape every mutating `/api/nodes*` server handler uses.
fn static_cmd(canvas_path: &Path, template_dir: &Path, out_dir: &Path, force: bool) {
    let raw = read_raw_or_exit(canvas_path);
    let canvas = Canvas::from_markdown(&raw).unwrap_or_else(|e| {
        eprintln!("failed to parse {}: {e}", canvas_path.display());
        std::process::exit(1);
    });
    // Same as `validate`/`view`: splice in `include` nodes so the exported
    // site shows the fully composed document, not the bare link `run`
    // sees in the raw file.
    let canvas = meshfox_core::include::resolve(&canvas, canvas_path).unwrap_or_else(|e| {
        eprintln!("meshfox static: {}: {e}", canvas_path.display());
        std::process::exit(1);
    });

    if !template_dir.is_dir() {
        eprintln!(
            "meshfox static: {} is not a directory",
            template_dir.display()
        );
        std::process::exit(1);
    }

    let out_non_empty = out_dir.exists()
        && std::fs::read_dir(out_dir)
            .map(|mut d| d.next().is_some())
            .unwrap_or(false);
    if out_non_empty && !force {
        eprintln!(
            "meshfox static: {} already exists and is not empty (pass --force to overwrite)",
            out_dir.display()
        );
        std::process::exit(1);
    }

    let config = load_template_config(template_dir);

    // Same "bare filename has an empty, not missing, parent" edge case
    // `meshfox_server::get_node_file_content` handles — a canvas passed as
    // just `README.md` (no directory component) resolves relative images/
    // `display="code"` targets against the current directory.
    let canvas_dir = canvas_path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let (site, assets) =
        meshfox_core::staticgen::build(&canvas, canvas_dir, config.base_url.as_deref());
    let mut context = tera::Context::new();
    context.insert("site", &site);
    context.insert("icons", &config.icons);
    // `meshfox_version` is always set (same string `--version` prints —
    // whichever binary is running this export); `canvas_commit` is the
    // canvas's own repo HEAD, short form, and `None` when `canvas_dir` isn't
    // inside a git working tree — a template decides for itself whether to
    // show either at all.
    context.insert("meshfox_version", VERSION);
    context.insert("canvas_commit", &canvas_git_commit(canvas_dir));

    // A proper glob-registered `Tera` instance, not a one-off render per
    // file: a template needs cross-file `{% import %}` to define the
    // canvas tree's recursive rendering macro just once (see
    // `site-template/_macros.html.tera`) rather than duplicating it inline
    // in every page.
    let glob = format!("{}/**/*.tera", template_dir.display());
    let tera = tera::Tera::new(&glob).unwrap_or_else(|e| {
        eprintln!(
            "meshfox static: failed to load templates from {}: {e}",
            template_dir.display()
        );
        std::process::exit(1);
    });

    let mut count = 0;
    for name in tera.get_template_names() {
        // A `_`-prefixed basename is a partial — imported by another
        // template (`{% import "_macros.html.tera" as macros %}`), never
        // rendered as its own output page. Same convention Jekyll/
        // Eleventy use for includes/partials.
        let is_partial = Path::new(name)
            .file_name()
            .and_then(|f| f.to_str())
            .is_some_and(|b| b.starts_with('_'));
        if is_partial {
            continue;
        }
        let rendered = tera.render(name, &context).unwrap_or_else(|e| {
            eprintln!("meshfox static: failed to render {name}: {e}");
            std::process::exit(1);
        });
        write_output_file(
            &out_dir.join(Path::new(name).with_extension("")),
            rendered.as_bytes(),
        )
        .unwrap_or_else(|e| {
            eprintln!("meshfox static: {e}");
            std::process::exit(1);
        });
        count += 1;
    }

    count += copy_template_assets(template_dir, template_dir, out_dir).unwrap_or_else(|e| {
        eprintln!("meshfox static: {e}");
        std::process::exit(1);
    });

    for asset in &assets {
        let bytes = std::fs::read(&asset.source).unwrap_or_else(|e| {
            eprintln!(
                "meshfox static: failed to read {}: {e}",
                asset.source.display()
            );
            std::process::exit(1);
        });
        write_output_file(&out_dir.join(&asset.dest_rel), &bytes).unwrap_or_else(|e| {
            eprintln!("meshfox static: {e}");
            std::process::exit(1);
        });
        count += 1;
    }

    println!(
        "meshfox static: wrote {count} file(s) to {}",
        out_dir.display()
    );
}

/// `out`'s default (`--out` omitted): `canvas_path`'s own filename with its
/// extension replaced by `.pdf`, same directory — mirrors `static_cmd`'s
/// `--out` handling in spirit (a sensible default next to the source file),
/// though `static`'s own default is a fixed `site` dirname since it has no
/// input filename stem worth reusing the same way a single output file does
/// here.
fn pdf_cmd(canvas_path: &Path, out: Option<&Path>, force: bool, mode: Option<pdf::Mode>) {
    let raw = read_raw_or_exit(canvas_path);
    let canvas = Canvas::from_markdown(&raw).unwrap_or_else(|e| {
        eprintln!("failed to parse {}: {e}", canvas_path.display());
        std::process::exit(1);
    });
    // Same as `static`/`validate`/`view`: splice in `include` nodes so the
    // exported PDF shows the fully composed document, not the bare link
    // `run` sees in the raw file.
    let canvas = meshfox_core::include::resolve(&canvas, canvas_path).unwrap_or_else(|e| {
        eprintln!("meshfox pdf: {}: {e}", canvas_path.display());
        std::process::exit(1);
    });

    let out_path = out
        .map(PathBuf::from)
        .unwrap_or_else(|| canvas_path.with_extension("pdf"));
    if out_path.exists() && !force {
        eprintln!(
            "meshfox pdf: {} already exists (pass --force to overwrite)",
            out_path.display()
        );
        std::process::exit(1);
    }

    // Same "bare filename has an empty, not missing, parent" edge case
    // `static_cmd` handles — a canvas passed as just `README.md` (no
    // directory component) resolves relative images against the current
    // directory.
    let canvas_dir = canvas_path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let bytes = pdf::generate(&canvas, canvas_dir, mode).unwrap_or_else(|e| {
        eprintln!("meshfox pdf: {e}");
        std::process::exit(1);
    });

    if let Some(parent) = out_path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).unwrap_or_else(|e| {
                eprintln!("meshfox pdf: failed to create {}: {e}", parent.display());
                std::process::exit(1);
            });
        }
    }
    std::fs::write(&out_path, &bytes).unwrap_or_else(|e| {
        eprintln!("meshfox pdf: failed to write {}: {e}", out_path.display());
        std::process::exit(1);
    });
    println!("meshfox pdf: wrote {}", out_path.display());
}

/// Recursively copies every non-`.tera` file under `dir` (a subtree of
/// `root`) into `out_root` at the same relative path — CSS, fonts, and any
/// other asset a template needs verbatim. `.tera` files are rendered
/// separately by the caller through a proper glob-registered `Tera`
/// instance (needed for cross-file `{% import %}`), not copied here; nor is
/// `root`'s own `template.toml` (see `TEMPLATE_CONFIG_FILE`/
/// `load_template_config`) — a template's own config, not one of its pages.
/// Returns the number of files copied.
fn copy_template_assets(root: &Path, dir: &Path, out_root: &Path) -> Result<usize, String> {
    let mut count = 0;
    for entry in
        std::fs::read_dir(dir).map_err(|e| format!("failed to read {}: {e}", dir.display()))?
    {
        let entry = entry.map_err(|e| format!("failed to read {}: {e}", dir.display()))?;
        let path = entry.path();
        if path.is_dir() {
            count += copy_template_assets(root, &path, out_root)?;
            continue;
        }
        if path.extension().and_then(|e| e.to_str()) == Some("tera") {
            continue;
        }
        if path == root.join(TEMPLATE_CONFIG_FILE) {
            continue;
        }
        let rel = path
            .strip_prefix(root)
            .expect("walked from root, so always a prefix");
        let bytes =
            std::fs::read(&path).map_err(|e| format!("failed to read {}: {e}", path.display()))?;
        write_output_file(&out_root.join(rel), &bytes)?;
        count += 1;
    }
    Ok(count)
}

fn write_output_file(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("failed to create {}: {e}", parent.display()))?;
    }
    std::fs::write(path, bytes).map_err(|e| format!("failed to write {}: {e}", path.display()))
}

#[cfg(test)]
fn validate_patch(updated: &str) -> Result<(), String> {
    Canvas::from_markdown(updated)
        .map(|_| ())
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod node_command_tests {
    use super::*;

    const TEST_DOC: &str = r#"# Root
<!-- meshfox:node id="root" x=0 y=0 w=250 h=60 -->

Root body.

## Tests
<!-- meshfox:node id="tests" type="group" -->

### Smoke Test
<!-- meshfox:node id="smoke-test" x=0 y=320 w=420 h=240 color="1" -->

Smoke body.

## Examples
<!-- meshfox:node id="examples" -->

### Shared Smoke Check
<!-- meshfox:node id="shared-smoke" x=560 y=320 w=420 h=200 -->
<!-- meshfox:edge from="tests" -->

Shared body.
"#;

    #[test]
    fn add_inserts_under_parent_and_returns_new_id() {
        let (updated, new_id) = apply_node_add(TEST_DOC, "tests", "New Check").unwrap();
        assert_eq!(new_id, "new-check");
        let canvas = Canvas::from_markdown(&updated).unwrap();
        assert_eq!(
            canvas.node("new-check").unwrap().parent.as_deref(),
            Some("tests")
        );
    }

    #[test]
    fn add_rejects_an_unknown_parent() {
        let err = apply_node_add(TEST_DOC, "nope", "New Check").unwrap_err();
        assert!(err.contains("no node"), "unexpected error: {err}");
    }

    #[test]
    fn add_with_extras_sets_body_and_meta_in_one_call() {
        let (updated, new_id) = apply_node_add_with_extras(
            TEST_DOC,
            "tests",
            "New Check",
            Some("Its own body."),
            NodeMetaFields {
                x: Some(10.0),
                y: Some(20.0),
                color: Some("2".to_string()),
                tags: Some("bag".to_string()),
                ..Default::default()
            },
        )
        .unwrap();
        let canvas = Canvas::from_markdown(&updated).unwrap();
        let node = canvas.node(&new_id).unwrap();
        assert_eq!(node.text, "Its own body.");
        assert_eq!(node.x, Some(10.0));
        assert_eq!(node.y, Some(20.0));
        assert_eq!(node.color.as_deref(), Some("2"));
        assert_eq!(node.tags, vec!["bag".to_string()]);
    }

    #[test]
    fn add_with_extras_matches_plain_add_when_nothing_extra_is_given() {
        let (with_extras, id_a) =
            apply_node_add_with_extras(TEST_DOC, "tests", "New Check", None, NodeMetaFields::default())
                .unwrap();
        let (plain, id_b) = apply_node_add(TEST_DOC, "tests", "New Check").unwrap();
        assert_eq!(id_a, id_b);
        // Byte-for-byte equality no longer holds now that every `node add`
        // auto-stamps `createdAt`/`updatedAt` (see `insert_child_node`) —
        // the two calls happen at genuinely different instants, so those
        // two values may legitimately differ even though nothing else
        // does. Compare structurally instead, blanking out just those two
        // dynamic fields on every node.
        let mut canvas_a = Canvas::from_markdown(&with_extras).unwrap();
        let mut canvas_b = Canvas::from_markdown(&plain).unwrap();
        for n in canvas_a.nodes.iter_mut().chain(canvas_b.nodes.iter_mut()) {
            n.created_at = None;
            n.updated_at = None;
        }
        assert_eq!(canvas_a, canvas_b);
    }

    #[test]
    fn rm_deletes_the_whole_subtree_by_default() {
        let updated = apply_node_rm(TEST_DOC, "tests", false).unwrap();
        let canvas = Canvas::from_markdown(&updated).unwrap();
        assert!(canvas.node("tests").is_none());
        assert!(canvas.node("smoke-test").is_none());
        // dangling edge from shared-smoke into the deleted subtree is
        // cleaned up too, so the result still parses.
        assert!(canvas
            .node("shared-smoke")
            .unwrap()
            .extra_parents
            .is_empty());
    }

    #[test]
    fn rm_keep_children_promotes_direct_children_instead() {
        let updated = apply_node_rm(TEST_DOC, "tests", true).unwrap();
        let canvas = Canvas::from_markdown(&updated).unwrap();
        assert!(canvas.node("tests").is_none());
        assert_eq!(
            canvas.node("smoke-test").unwrap().parent.as_deref(),
            Some("root")
        );
    }

    #[test]
    fn rm_refuses_to_delete_the_root() {
        let err = apply_node_rm(TEST_DOC, "root", false).unwrap_err();
        assert!(err.contains("root"), "unexpected error: {err}");
    }

    #[test]
    fn mv_moves_a_node_under_a_new_parent_in_one_step() {
        // shared-smoke has no existing edge to "tests" as a plain extra
        // parent target here — apply_node_mv must add it itself before
        // reparent_node will accept the promotion.
        let updated = apply_node_mv(TEST_DOC, "shared-smoke", "tests").unwrap();
        let canvas = Canvas::from_markdown(&updated).unwrap();
        let node = canvas.node("shared-smoke").unwrap();
        assert_eq!(node.parent.as_deref(), Some("tests"));
        // the promoted edge is dropped, not left dangling as a duplicate
        assert!(!node.extra_parents.iter().any(|e| e.from == "tests"));
    }

    #[test]
    fn mv_converts_position_into_the_new_groups_frame() {
        // Same position-frame conversion `crates/server`'s own reparent
        // endpoint does, exercised through this separate CLI front door
        // onto the same `mdcanvas::reparent_node` primitive.
        const DOC: &str = concat!(
            "# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
            "## Frame\n<!-- meshfox:node id=\"frame\" type=\"group\" x=1000 y=1000 -->\n\n",
            "### Existing Member\n<!-- meshfox:node id=\"existing-member\" x=10 y=10 w=100 h=60 -->\n\nbody\n\n",
            "## Wanderer\n<!-- meshfox:node id=\"wanderer\" x=1050 y=1030 w=100 h=60 -->\n\nbody\n",
        );
        let updated = apply_node_mv(DOC, "wanderer", "frame").unwrap();
        let canvas = Canvas::from_markdown(&updated).unwrap();
        let node = canvas.node("wanderer").unwrap();
        assert_eq!(node.parent.as_deref(), Some("frame"));
        assert_eq!(node.x, Some(50.0));
        assert_eq!(node.y, Some(30.0));
    }

    #[test]
    fn mv_refuses_to_move_the_root() {
        let err = apply_node_mv(TEST_DOC, "root", "tests").unwrap_err();
        assert!(err.contains("root"), "unexpected error: {err}");
    }

    #[test]
    fn mv_refuses_a_cycle_onto_its_own_descendant() {
        let err = apply_node_mv(TEST_DOC, "tests", "smoke-test").unwrap_err();
        assert!(err.contains("cyclic"), "unexpected error: {err}");
    }

    #[test]
    fn rename_changes_title_but_not_id_or_body() {
        let original_text = Canvas::from_markdown(TEST_DOC)
            .unwrap()
            .node("smoke-test")
            .unwrap()
            .text
            .clone();
        let updated = apply_node_rename(TEST_DOC, "smoke-test", "Renamed").unwrap();
        let canvas = Canvas::from_markdown(&updated).unwrap();
        let node = canvas.node("smoke-test").unwrap();
        assert_eq!(node.id, "smoke-test");
        assert_eq!(node.title, "Renamed");
        assert_eq!(node.text, original_text);
    }

    #[test]
    fn body_replaces_whole_text() {
        let updated = apply_node_body(TEST_DOC, "smoke-test", "New body.").unwrap();
        let canvas = Canvas::from_markdown(&updated).unwrap();
        assert_eq!(canvas.node("smoke-test").unwrap().text, "New body.");
    }

    #[test]
    fn body_rejects_an_unknown_node() {
        let err = apply_node_body(TEST_DOC, "nope", "text").unwrap_err();
        assert!(err.contains("no node"), "unexpected error: {err}");
    }

    #[test]
    fn meta_sets_given_fields_and_preserves_the_rest() {
        let updated = apply_node_meta(
            TEST_DOC,
            "smoke-test",
            Some(10.0),
            None,
            None,
            None,
            false,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();
        let canvas = Canvas::from_markdown(&updated).unwrap();
        let node = canvas.node("smoke-test").unwrap();
        assert_eq!(node.x, Some(10.0));
        // untouched fields keep their prior value rather than being wiped
        assert_eq!(node.y, Some(320.0));
        assert_eq!(node.width, Some(420.0));
        assert_eq!(node.color.as_deref(), Some("1"));
    }

    #[test]
    fn meta_sets_and_clears_tags() {
        // Omitted (`None`) leaves the node's tags untouched, same contract
        // as every other field here.
        let updated = apply_node_meta(
            TEST_DOC,
            "smoke-test",
            None,
            None,
            None,
            None,
            false,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();
        assert!(Canvas::from_markdown(&updated).unwrap().node("smoke-test").unwrap().tags.is_empty());

        // Given, replaces the whole list outright — trimmed/split the same
        // way the file's own `tags="a, b"` attribute is.
        let updated = apply_node_meta(
            TEST_DOC,
            "smoke-test",
            None,
            None,
            None,
            None,
            false,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            Some("bag, fixed".to_string()),
            None,
        )
        .unwrap();
        assert_eq!(
            Canvas::from_markdown(&updated).unwrap().node("smoke-test").unwrap().tags,
            vec!["bag".to_string(), "fixed".to_string()],
        );
        // Untouched fields (here, color) still keep their prior value.
        assert_eq!(
            Canvas::from_markdown(&updated).unwrap().node("smoke-test").unwrap().color.as_deref(),
            Some("1")
        );

        // `--tags ""` explicitly clears rather than being a no-op.
        let cleared = apply_node_meta(
            &updated,
            "smoke-test",
            None,
            None,
            None,
            None,
            false,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            Some(String::new()),
            None,
        )
        .unwrap();
        assert!(Canvas::from_markdown(&cleared).unwrap().node("smoke-test").unwrap().tags.is_empty());
        assert!(!cleared.contains("tags="));
    }

    #[test]
    fn meta_rejects_tags_that_look_like_accidental_json() {
        // A JSON-encoded list handed to `--tags` instead of plain
        // comma-separated text — the exact shape an agent slips into when
        // it forgets this field isn't an array — must be rejected instead
        // of silently landing as one mangled tag full of stray syntax.
        for bad in [
            "[\"foo\",\"bar\"]",
            "[\"foo\nbar\"]",
            "foo,bar\n",
            "{\"foo\":\"bar\"}",
            "it's a tag",
        ] {
            let err = apply_node_meta(
                TEST_DOC,
                "smoke-test",
                None,
                None,
                None,
                None,
                false,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                Some(bad.to_string()),
                None,
            )
            .unwrap_err();
            assert!(err.contains("--tags"), "{bad:?} -> {err}");
        }
    }

    #[test]
    fn meta_rejects_a_size_on_a_group() {
        let err = apply_node_meta(
            TEST_DOC,
            "tests",
            None,
            None,
            Some(300.0),
            None,
            false,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap_err();
        assert!(err.contains("group"), "unexpected error: {err}");

        let err = apply_node_meta(
            TEST_DOC,
            "tests",
            None,
            None,
            None,
            Some(120.0),
            false,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap_err();
        assert!(err.contains("group"), "unexpected error: {err}");
    }

    #[test]
    fn meta_accepts_an_anchor_on_a_group() {
        let updated = apply_node_meta(
            TEST_DOC,
            "tests",
            Some(1000.0),
            Some(2000.0),
            None,
            None,
            false,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();
        let canvas = Canvas::from_markdown(&updated).unwrap();
        let node = canvas.node("tests").unwrap();
        assert_eq!(node.x, Some(1000.0));
        assert_eq!(node.y, Some(2000.0));
        // a group's size stays forever derived, never stored, even though
        // its position can now be set.
        assert_eq!(node.width, None);
        assert_eq!(node.height, None);
    }

    #[test]
    fn meta_sets_and_clears_a_fold_override() {
        let updated = apply_node_meta(
            TEST_DOC,
            "tests",
            None,
            None,
            None,
            None,
            false,
            None,
            None,
            None,
            None,
            None,
            None,
            Some("true".to_string()),
            None,
            None,
        )
        .unwrap();
        assert_eq!(
            Canvas::from_markdown(&updated)
                .unwrap()
                .node("tests")
                .unwrap()
                .fold,
            Some(true)
        );

        let updated = apply_node_meta(
            &updated,
            "tests",
            None,
            None,
            None,
            None,
            false,
            None,
            None,
            None,
            None,
            None,
            None,
            Some("default".to_string()),
            None,
            None,
        )
        .unwrap();
        assert_eq!(
            Canvas::from_markdown(&updated)
                .unwrap()
                .node("tests")
                .unwrap()
                .fold,
            None
        );
    }

    #[test]
    fn meta_rejects_an_unknown_fold_value() {
        let err = apply_node_meta(
            TEST_DOC,
            "tests",
            None,
            None,
            None,
            None,
            false,
            None,
            None,
            None,
            None,
            None,
            None,
            Some("bogus".to_string()),
            None,
            None,
        )
        .unwrap_err();
        assert!(err.contains("fold"), "unexpected error: {err}");
    }

    #[test]
    fn meta_clear_position_drops_x_y_w_h_and_preserves_everything_else() {
        let updated = apply_node_meta(
            TEST_DOC,
            "smoke-test",
            None,
            None,
            None,
            None,
            true,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();
        let canvas = Canvas::from_markdown(&updated).unwrap();
        let node = canvas.node("smoke-test").unwrap();
        assert_eq!(node.x, None);
        assert_eq!(node.y, None);
        assert_eq!(node.width, None);
        assert_eq!(node.height, None);
        // untouched fields keep their prior value.
        assert_eq!(node.color.as_deref(), Some("1"));
    }

    #[test]
    fn meta_clear_position_rejects_being_combined_with_an_explicit_x_y_w_h() {
        let err = apply_node_meta(
            TEST_DOC,
            "smoke-test",
            Some(10.0),
            None,
            None,
            None,
            true,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap_err();
        assert!(
            err.contains("mutually exclusive"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn meta_rejects_an_unknown_type_value() {
        let err = apply_node_meta(
            TEST_DOC,
            "smoke-test",
            None,
            None,
            None,
            None,
            false,
            None,
            Some("bogus".to_string()),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap_err();
        assert!(err.contains("unknown --type"), "unexpected error: {err}");
    }

    #[test]
    fn edges_replaces_the_extra_parent_list() {
        let updated =
            apply_node_edges(TEST_DOC, "shared-smoke", &["examples".to_string()]).unwrap();
        let canvas = Canvas::from_markdown(&updated).unwrap();
        assert_eq!(
            canvas.node("shared-smoke").unwrap().extra_parents,
            vec![ExtraEdge::new("examples")]
        );
    }

    #[test]
    fn edges_empty_list_clears_them() {
        let updated = apply_node_edges(TEST_DOC, "shared-smoke", &[]).unwrap();
        let canvas = Canvas::from_markdown(&updated).unwrap();
        assert!(canvas
            .node("shared-smoke")
            .unwrap()
            .extra_parents
            .is_empty());
    }

    #[test]
    fn move_before_reorders_siblings() {
        // "examples" comes after "tests" in TEST_DOC — move it before.
        let (updated, target_id, label) =
            apply_node_move(TEST_DOC, "examples", Some("tests"), None).unwrap();
        assert_eq!(target_id, "tests");
        assert_eq!(label, "before");
        let order: Vec<String> = Canvas::from_markdown(&updated)
            .unwrap()
            .nodes
            .iter()
            .map(|n| n.id.clone())
            .collect();
        assert_eq!(order[1], "examples");
        assert!(order.iter().position(|i| i == "examples").unwrap()
            < order.iter().position(|i| i == "tests").unwrap());
    }

    #[test]
    fn move_after_reorders_siblings() {
        let (updated, ..) = apply_node_move(TEST_DOC, "tests", None, Some("examples")).unwrap();
        let order: Vec<String> = Canvas::from_markdown(&updated)
            .unwrap()
            .nodes
            .iter()
            .map(|n| n.id.clone())
            .collect();
        assert!(order.iter().position(|i| i == "examples").unwrap()
            < order.iter().position(|i| i == "tests").unwrap());
    }

    #[test]
    fn move_requires_exactly_one_of_before_after() {
        let err = apply_node_move(TEST_DOC, "examples", None, None).unwrap_err();
        assert!(err.contains("exactly one"), "unexpected error: {err}");

        let err = apply_node_move(TEST_DOC, "examples", Some("tests"), Some("root")).unwrap_err();
        assert!(err.contains("mutually exclusive"), "unexpected error: {err}");
    }

    #[test]
    fn move_rejects_non_siblings() {
        let err = apply_node_move(TEST_DOC, "smoke-test", Some("examples"), None).unwrap_err();
        assert!(
            err.contains("parent"),
            "expected a not-siblings error, got: {err}"
        );
    }

    #[test]
    fn reorder_is_idempotent_when_already_sorted() {
        let updated = apply_node_reorder(TEST_DOC).unwrap();
        assert_eq!(
            Canvas::from_markdown(&updated).unwrap().nodes.len(),
            Canvas::from_markdown(TEST_DOC).unwrap().nodes.len()
        );
    }

    #[test]
    fn show_reports_parent_children_and_position() {
        let text = format_node_show(TEST_DOC, "tests").unwrap();
        assert!(text.contains("id: tests"));
        assert!(text.contains("type: group"));
        assert!(text.contains("parent: root"));
        assert!(text.contains("children: smoke-test"));
    }

    #[test]
    fn show_reports_tags_when_set_and_omits_the_line_when_not() {
        const DOC: &str = concat!(
            "# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
            "## Tagged\n<!-- meshfox:node id=\"tagged\" tags=\"bag,fixed\" -->\n\nbody\n\n",
            "## Untagged\n<!-- meshfox:node id=\"untagged\" -->\n\nbody\n",
        );
        let tagged = format_node_show(DOC, "tagged").unwrap();
        assert!(tagged.contains("tags: bag, fixed"), "{tagged}");
        let untagged = format_node_show(DOC, "untagged").unwrap();
        assert!(!untagged.contains("tags:"), "{untagged}");
    }

    #[test]
    fn show_reports_a_group_members_resolved_absolute_position() {
        const DOC: &str = concat!(
            "# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
            "## Frame\n<!-- meshfox:node id=\"frame\" type=\"group\" x=1000 y=1000 -->\n\n",
            "### Member\n<!-- meshfox:node id=\"member\" x=20 y=20 w=100 h=60 -->\n\nbody\n",
        );
        let text = format_node_show(DOC, "member").unwrap();
        assert!(text.contains("position: x=20 y=20 w=100 h=60"), "{text}");
        assert!(
            text.contains("resolved position (absolute): x=1020 y=1020"),
            "{text}"
        );
    }

    #[test]
    fn show_omits_resolved_position_when_the_group_has_no_anchor() {
        const DOC: &str = concat!(
            "# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
            "## Frame\n<!-- meshfox:node id=\"frame\" type=\"group\" -->\n\n",
            "### Member\n<!-- meshfox:node id=\"member\" x=20 y=20 w=100 h=60 -->\n\nbody\n",
        );
        let text = format_node_show(DOC, "member").unwrap();
        assert!(!text.contains("resolved position"), "{text}");
    }

    #[test]
    fn show_rejects_an_unknown_node() {
        let err = format_node_show(TEST_DOC, "nope").unwrap_err();
        assert!(err.contains("no node"), "unexpected error: {err}");
    }

    const BLOCK_DOC: &str = concat!(
        "# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
        "```bash name=\"build\" cache\necho hi\n```\n\n",
        "```bash name=\"other\"\necho untouched\n```\n",
    );

    #[test]
    fn block_renames_and_toggles_flags() {
        let updated = apply_node_block(
            BLOCK_DOC,
            "root",
            "build",
            &BlockArgs {
                rename: Some("built".to_string()),
                always: true,
                ..Default::default()
            },
            None,
        )
        .unwrap();
        let node = Canvas::from_markdown(&updated).unwrap();
        let blocks = meshfox_core::scan_runnable_blocks("root", &node.node("root").unwrap().text);
        let block = blocks
            .iter()
            .find(|b| b.name.as_deref() == Some("built"))
            .unwrap();
        assert!(block.always);
        assert!(block.cache, "untouched cache flag should survive");
        assert!(updated.contains("echo untouched"), "sibling block untouched");
    }

    #[test]
    fn block_rejects_both_halves_of_a_flag_pair_at_once() {
        let err = apply_node_block(
            BLOCK_DOC,
            "root",
            "build",
            &BlockArgs {
                cache: true,
                no_cache: true,
                ..Default::default()
            },
            None,
        )
        .unwrap_err();
        assert!(err.contains("mutually exclusive"), "unexpected error: {err}");
    }

    #[test]
    fn block_replaces_deps_and_rejects_a_dangling_target() {
        let updated = apply_node_block(
            BLOCK_DOC,
            "root",
            "build",
            &BlockArgs {
                deps: Some("other".to_string()),
                ..Default::default()
            },
            None,
        )
        .unwrap();
        assert!(updated.contains("deps=\"other\""));

        let err = apply_node_block(
            BLOCK_DOC,
            "root",
            "build",
            &BlockArgs {
                deps: Some("nonexistent".to_string()),
                ..Default::default()
            },
            None,
        )
        .unwrap_err();
        assert!(err.contains("nonexistent"), "unexpected error: {err}");
    }

    #[test]
    fn block_clears_deps_and_env_explicitly() {
        let with_deps = apply_node_block(
            BLOCK_DOC,
            "root",
            "build",
            &BlockArgs {
                deps: Some("other".to_string()),
                ..Default::default()
            },
            None,
        )
        .unwrap();
        let cleared = apply_node_block(
            &with_deps,
            "root",
            "build",
            &BlockArgs {
                clear_deps: true,
                ..Default::default()
            },
            None,
        )
        .unwrap();
        assert!(!cleared.contains("deps="));
    }

    #[test]
    fn block_sets_and_clears_interpreter() {
        let set = apply_node_block(
            BLOCK_DOC,
            "root",
            "build",
            &BlockArgs {
                interpreter: Some("python3 -u".to_string()),
                ..Default::default()
            },
            None,
        )
        .unwrap();
        assert!(set.contains("interpreter=\"python3 -u\""));

        let cleared = apply_node_block(
            &set,
            "root",
            "build",
            &BlockArgs {
                clear_interpreter: true,
                ..Default::default()
            },
            None,
        )
        .unwrap();
        assert!(!cleared.contains("interpreter="));
    }

    #[test]
    fn block_replaces_the_code() {
        let updated =
            apply_node_block(BLOCK_DOC, "root", "build", &BlockArgs::default(), Some("echo new"))
                .unwrap();
        assert!(updated.contains("echo new"));
        assert!(!updated.contains("echo hi"));
    }

    #[test]
    fn block_rejects_an_unknown_node_or_block() {
        let err =
            apply_node_block(BLOCK_DOC, "nope", "build", &BlockArgs::default(), None).unwrap_err();
        assert!(err.contains("no node"), "unexpected error: {err}");

        let err =
            apply_node_block(BLOCK_DOC, "root", "nope", &BlockArgs::default(), None).unwrap_err();
        assert!(err.contains("no runnable"), "unexpected error: {err}");
    }

    const FIND_DOC: &str = concat!(
        "# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
        "## Todo\n<!-- meshfox:node id=\"todo\" tags=\"bag\" -->\n\n",
        "### Fixed Bug\n<!-- meshfox:node id=\"fixed-bug\" tags=\"bag,fixed\" -->\n\n",
        "#### Nested\n<!-- meshfox:node id=\"nested\" tags=\"bag\" -->\n\n",
        "### Other\n<!-- meshfox:node id=\"other\" type=\"group\" color=\"4\" -->\n",
    );

    #[test]
    fn find_matches_a_tag_class_selector_in_document_order() {
        let canvas = Canvas::from_markdown(FIND_DOC).unwrap();
        let ids = find_node_ids(&canvas, ".bag").unwrap();
        assert_eq!(ids, vec!["todo", "fixed-bug", "nested"]);
    }

    #[test]
    fn find_direct_child_combinator_excludes_deeper_descendants() {
        let canvas = Canvas::from_markdown(FIND_DOC).unwrap();
        assert_eq!(find_node_ids(&canvas, "#todo > .bag").unwrap(), vec!["fixed-bug"]);
        // Plain descendant combinator (space) reaches any depth.
        assert_eq!(
            find_node_ids(&canvas, "#todo .bag").unwrap(),
            vec!["fixed-bug", "nested"]
        );
    }

    #[test]
    fn find_matches_type_and_color_attributes() {
        let canvas = Canvas::from_markdown(FIND_DOC).unwrap();
        assert_eq!(find_node_ids(&canvas, "[type=\"group\"]").unwrap(), vec!["other"]);
        assert_eq!(find_node_ids(&canvas, "[color=\"4\"]").unwrap(), vec!["other"]);
    }

    #[test]
    fn find_combines_multiple_tags_and_id_selectors() {
        let canvas = Canvas::from_markdown(FIND_DOC).unwrap();
        assert_eq!(find_node_ids(&canvas, ".bag.fixed").unwrap(), vec!["fixed-bug"]);
        assert_eq!(find_node_ids(&canvas, "#fixed-bug").unwrap(), vec!["fixed-bug"]);
    }

    #[test]
    fn find_returns_empty_not_an_error_for_no_matches() {
        let canvas = Canvas::from_markdown(FIND_DOC).unwrap();
        assert_eq!(find_node_ids(&canvas, ".nonexistent").unwrap(), Vec::<String>::new());
    }

    #[test]
    fn find_rejects_an_invalid_selector() {
        let canvas = Canvas::from_markdown(FIND_DOC).unwrap();
        let err = find_node_ids(&canvas, "###bad").unwrap_err();
        assert!(err.contains("invalid CSS selector"), "unexpected error: {err}");
    }

    #[test]
    fn find_escapes_html_special_characters_without_corrupting_the_document() {
        // `&`/`<` are legitimate, parseable characters in a real tags="..."
        // value (only `"` itself isn't — it's the attribute's own
        // delimiter, so it can't appear in a real parsed document any more
        // than it could in the synthetic HTML this builds). Left
        // unescaped, `<` in particular would open a bogus element and
        // corrupt parsing for the rest of the document, not just this
        // node — a sibling node still resolving correctly afterward is
        // what actually proves the escaping worked.
        let doc = concat!(
            "# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
            "## Odd\n<!-- meshfox:node id=\"odd\" tags=\"a&b,c<d\" -->\n\n",
            "## Safe\n<!-- meshfox:node id=\"safe-node\" tags=\"plain\" -->\n",
        );
        let canvas = Canvas::from_markdown(doc).unwrap();
        assert_eq!(find_node_ids(&canvas, ".plain").unwrap(), vec!["safe-node"]);
        assert_eq!(find_node_ids(&canvas, "#odd").unwrap(), vec!["odd"]);
    }

    #[test]
    fn parse_node_type_accepts_every_variant_and_rejects_garbage() {
        assert_eq!(parse_node_type("text").unwrap(), NodeType::Text);
        assert_eq!(parse_node_type("file").unwrap(), NodeType::File);
        assert_eq!(parse_node_type("link").unwrap(), NodeType::Link);
        assert_eq!(parse_node_type("group").unwrap(), NodeType::Group);
        assert_eq!(parse_node_type("include").unwrap(), NodeType::Include);
        assert!(parse_node_type("bogus").is_err());
    }

    #[test]
    fn parse_display_accepts_every_variant_and_rejects_garbage() {
        assert_eq!(parse_display("link").unwrap(), FileDisplay::Link);
        assert_eq!(parse_display("code").unwrap(), FileDisplay::Code);
        assert!(parse_display("bogus").is_err());
    }
}

#[cfg(test)]
mod run_hint_tests {
    use super::*;

    #[test]
    fn looks_like_a_canvas_path_matches_dot_md_case_insensitively() {
        assert!(looks_like_a_canvas_path("README.md"));
        assert!(looks_like_a_canvas_path("weird.MD"));
        assert!(!looks_like_a_canvas_path("linting"));
        assert!(!looks_like_a_canvas_path("typecheck"));
    }

    #[test]
    fn misplaced_canvas_hint_fires_only_when_an_arg_looks_like_a_path() {
        assert!(run_hint_for_misplaced_canvas(&["README.md", "linting"]).is_some());
        assert!(run_hint_for_misplaced_canvas(&["linting", "typecheck"]).is_none());
    }

    #[test]
    fn levenshtein_matches_known_distances() {
        assert_eq!(levenshtein("linting", "linting"), 0);
        assert_eq!(levenshtein("kitten", "sitting"), 3);
        assert_eq!(levenshtein("", "abc"), 3);
    }

    const TREE_DOC: &str = concat!(
        "# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
        "## Development\n<!-- meshfox:node id=\"development\" -->\n\n",
        "### Linting\n<!-- meshfox:node id=\"linting\" -->\n\nbody\n",
    );

    #[test]
    fn closest_node_path_finds_an_exact_id_at_the_right_depth() {
        let canvas = Canvas::from_markdown(TREE_DOC).unwrap();
        // "linting" itself isn't a direct child of root — it's two levels
        // down, under "development" — the exact mistake this is for.
        assert_eq!(
            closest_node_path(&canvas, "linting"),
            Some("development linting".to_string())
        );
    }

    #[test]
    fn closest_node_path_suggests_a_close_typo_but_not_a_wild_guess() {
        let canvas = Canvas::from_markdown(TREE_DOC).unwrap();
        assert_eq!(
            closest_node_path(&canvas, "lintin"),
            Some("development linting".to_string())
        );
        assert_eq!(closest_node_path(&canvas, "completely-unrelated-id"), None);
    }
}

#[cfg(test)]
mod set_override_tests {
    use super::*;

    fn decl(name: &str, var_type: meshfox_core::VarType, choices: &[&str]) -> VarDecl {
        VarDecl {
            name: name.to_string(),
            var_type,
            prompt: name.to_string(),
            default: None,
            choices: choices.iter().map(|s| s.to_string()).collect(),
            secret: false,
            required: false,
            from: None,
            session: false,
            default_var: None,
            choices_var: None,
        }
    }

    #[test]
    fn accepts_a_well_typed_set_for_every_declared_type() {
        let decls = vec![
            decl("COUNT", meshfox_core::VarType::Int, &[]),
            decl("VERBOSE", meshfox_core::VarType::Bool, &[]),
            decl("LEVEL", meshfox_core::VarType::Select, &["debug", "info"]),
            decl("NAME", meshfox_core::VarType::String, &[]),
        ];
        let overrides = HashMap::from([
            ("COUNT".to_string(), "42".to_string()),
            ("VERBOSE".to_string(), "true".to_string()),
            ("LEVEL".to_string(), "info".to_string()),
            ("NAME".to_string(), "anything".to_string()),
        ]);
        assert!(validate_set_overrides(&decls, &overrides).is_ok());
    }

    #[test]
    fn rejects_a_non_integer_set_for_an_int_variable() {
        let decls = vec![decl("COUNT", meshfox_core::VarType::Int, &[])];
        let overrides = HashMap::from([("COUNT".to_string(), "not-a-number".to_string())]);
        let err = validate_set_overrides(&decls, &overrides).unwrap_err();
        assert!(err.contains("COUNT"), "unexpected error: {err}");
    }

    #[test]
    fn rejects_a_set_outside_a_selects_own_choices() {
        let decls = vec![decl(
            "LEVEL",
            meshfox_core::VarType::Select,
            &["debug", "info"],
        )];
        let overrides = HashMap::from([("LEVEL".to_string(), "trace".to_string())]);
        assert!(validate_set_overrides(&decls, &overrides).is_err());
    }

    #[test]
    fn rejects_a_non_canonical_set_for_a_bool_variable() {
        let decls = vec![decl("VERBOSE", meshfox_core::VarType::Bool, &[])];
        let overrides = HashMap::from([("VERBOSE".to_string(), "yes".to_string())]);
        assert!(validate_set_overrides(&decls, &overrides).is_err());
    }

    #[test]
    fn ignores_a_set_for_a_name_this_document_never_declared() {
        // An ordinary environment override with no matching `meshfox:var`
        // has no type to check it against — same as `resolve`'s own
        // handling of an override name it doesn't recognize.
        let decls: Vec<VarDecl> = Vec::new();
        let overrides = HashMap::from([("UNRELATED".to_string(), "whatever".to_string())]);
        assert!(validate_set_overrides(&decls, &overrides).is_ok());
    }
}
