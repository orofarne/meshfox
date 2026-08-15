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

use clap::{Parser, Subcommand};
use meshfox_core::{mdcanvas, Canvas, ExtraEdge, FileDisplay, NodeMeta, NodeType, VarDecl, VarCache};
use std::collections::HashMap;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};

mod prompt;
mod tui;

/// `commit <hash> (<date>)`, or `<tag> (<date>)` for a build made from a
/// release tag, captured at build time by `build.rs` from the repo `meshfox`
/// was actually compiled in — not the crate's Cargo.toml version, which
/// doesn't change between commits.
const VERSION: &str = concat!(env!("MESHFOX_VERSION_LABEL"), " (", env!("MESHFOX_GIT_DATE"), ")");

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
        /// Path to the .canvas.md file. If omitted: auto-discover the
        /// single candidate in the current directory.
        canvas: Option<PathBuf>,
    },
    /// Create a new, empty canvas file: just the `meshfox:canvas` marker
    /// followed by a lone root heading (`#`) named after the file itself
    /// (its name with a trailing `.canvas.md`/`.md` stripped). Fails if the
    /// file already exists — this never overwrites.
    Create {
        /// Path for the new .canvas.md file, e.g. `meshfox create
        /// hello.canvas.md`.
        canvas: PathBuf,
    },
    /// Start the local web UI: canvas view, run buttons. Opens read-only —
    /// running a block is always allowed, but click "Edit" in the browser
    /// to unlock dragging, resizing, saving layout, and persisting a
    /// `cache`d block's output back into the file.
    View {
        /// Path to the .canvas.md file, e.g. `meshfox view README.md`. If
        /// omitted: auto-discover the single candidate in the current
        /// directory.
        canvas: Option<PathBuf>,
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
    },
    /// Experimental: an ncurses-style terminal viewer — browse the node
    /// tree, read a node's rendered Markdown body (syntax-highlighted code,
    /// local images shown inline where the terminal supports it), and run
    /// blocks with live streamed output, right in the terminal. Same
    /// deps-chain/cache/`meshfox:var` handling as `meshfox run`/`meshfox
    /// view`. A `tty` block hands the real terminal over to it, same as
    /// `meshfox run`'s own `tty` handling. Mouse support covers clicking a
    /// tree row to select it (or its ▾/▸ marker to expand/collapse) and
    /// scrolling the tree/document panes. Read + run only for now — no
    /// structural editing (use `meshfox node ...` or the browser UI's Edit
    /// mode for that).
    Tui {
        /// Path to the .canvas.md file. If omitted: auto-discover the
        /// single candidate in the current directory.
        canvas: Option<PathBuf>,
    },
    /// Validate that a file parses as a meshfox canvas — same checks
    /// `run`/`view` already do before touching anything (single root,
    /// no duplicate ids, no dangling `meshfox:edge` targets, `group`/
    /// `file`/`link` body rules) — without executing anything or writing
    /// the file back. Exits non-zero on a parse error, so it's usable as a
    /// pre-commit/CI check.
    Validate {
        /// Path to the .canvas.md file. If omitted: auto-discover the
        /// single candidate in the current directory.
        canvas: Option<PathBuf>,
    },
    /// Run every embedded ` ```starlark constraint ` fence's Starlark
    /// contract against the document (see `crate::constraint`/SPEC.md's
    /// "Constraint nodes") and report which passed. Distinct from
    /// `validate`: `validate` checks that the file *parses* as a
    /// well-formed canvas; `check` asks whether the document as a whole
    /// satisfies whatever rules its own constraint fences declare (e.g.
    /// "every node tagged `table` has exactly one `file` child") — implies
    /// `validate` first, since an unparseable file has no constraints to
    /// run. Exits non-zero if the file fails to parse or any constraint
    /// fails, so it's usable as a pre-commit/CI check alongside (or
    /// instead of) `validate`.
    Check {
        /// Path to the .canvas.md file. If omitted: auto-discover the
        /// single candidate in the current directory.
        canvas: Option<PathBuf>,
    },
    /// Print every runnable code block in the canvas as an indented tree,
    /// each with a ready-to-paste `meshfox run <path...> <name>` — so you
    /// don't have to go spelunking through the file to find out what's
    /// runnable. Same raw-file-only scope as `run`/`validate` (no
    /// include resolution).
    List {
        /// Path to the .canvas.md file. If omitted: auto-discover the
        /// single candidate in the current directory.
        canvas: Option<PathBuf>,
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
        /// Path to the .canvas.md file. If omitted: auto-discover the
        /// single candidate in the current directory.
        canvas: Option<PathBuf>,
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
        command: NodeCommand,
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
    Completions {
        shell: clap_complete::Shell,
    },
}

#[derive(Subcommand)]
enum NodeCommand {
    /// Add a new, empty-bodied child node under `parent-id`, as the last
    /// item in its existing subtree (`mdcanvas::insert_child_node`) — same
    /// as the web UI's "add child" button. No position is set, so it stays
    /// unpositioned (auto-placed by the web UI's own client-side layout)
    /// until something gives it a real one — dragging it in the browser, or
    /// `node meta`. Prints the new node's id: a slug of `title`,
    /// de-duplicated against every id already in the file.
    Add {
        /// Path to the .canvas.md file. If omitted: auto-discover the
        /// single candidate in the current directory.
        #[arg(long)]
        canvas: Option<PathBuf>,
        parent_id: String,
        title: String,
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
    /// Set a node's position/size/style fields (`mdcanvas::set_node_meta`)
    /// — `--x`/`--y`/`--w`/`--h` for a manual position/size override,
    /// `--color`/`--type`/`--display`/`--lang`/`--interpreter` for
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
        /// `file`-node interpreter (e.g. `python`) — makes the node
        /// runnable as `interpreter target`.
        #[arg(long)]
        interpreter: Option<String>,
        /// Per-node fold-state override (see SPEC.md's "Options" section):
        /// `true`/`false` sets an explicit override, `default` clears it
        /// back to following the document's own default. Omit this flag
        /// entirely to leave whatever's already there untouched.
        #[arg(long)]
        fold: Option<String>,
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
    let has_leading_canvas =
        args.len() >= 2 && !args[1].starts_with('-') && args[1].to_ascii_lowercase().ends_with(".md");
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
        clap_complete::generate(shell, &mut Cli::command(), "meshfox", &mut std::io::stdout());
        return;
    }

    let mut buf = Vec::new();
    clap_complete::generate(shell, &mut Cli::command(), "meshfox", &mut buf);
    let generated = String::from_utf8(buf).expect("clap_complete output is valid UTF-8");

    let generated = generated.replacen("\n_meshfox() {\n", "\n_meshfox_generated() {\n", 1);
    let tail = "if [ \"$funcstack[1]\" = \"_meshfox\" ]; then\n    _meshfox \"$@\"\nelse\n    compdef _meshfox meshfox\nfi\n";
    let patched = generated.replacen(tail, &format!("{ZSH_CANVAS_DISPATCH}{tail}"), 1);
    debug_assert!(patched.contains("_meshfox_generated() {"), "clap_complete's zsh output shape changed");
    debug_assert!(patched.contains(ZSH_CANVAS_DISPATCH), "clap_complete's zsh tail block shape changed");

    print!("{patched}");
}

fn main() {
    let args = splice_leading_canvas(std::env::args().collect());

    if args.len() == 2 && !args[1].starts_with('-') && args[1].to_ascii_lowercase().ends_with(".md") {
        // A bare `meshfox test.md`, no subcommand: open it, same as
        // clicking the file — `meshfox view`'s own defaults otherwise.
        return view(PathBuf::from(&args[1]), 0, true, true);
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
        Command::Run { args, canvas, no_deps, set } => {
            let canvas_path = canvas.unwrap_or_else(find_canvas);
            run(&canvas_path, args, no_deps, set)
        }
        Command::Configure { canvas } => {
            let canvas_path = canvas.unwrap_or_else(find_canvas);
            configure(&canvas_path)
        }
        Command::Create { canvas } => create(&canvas),
        Command::View { canvas, port, no_open, create: create_if_missing, no_auto_exit } => {
            let canvas_path = match canvas {
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
            if create_if_missing && !canvas_path.exists() {
                write_canvas_template(&canvas_path);
                println!("meshfox view: created {}", canvas_path.display());
            }
            view(canvas_path, port, !no_open, !no_auto_exit)
        }
        Command::Tui { canvas } => {
            let canvas_path = canvas.unwrap_or_else(find_canvas);
            tui(canvas_path)
        }
        Command::Validate { canvas } => {
            let canvas_path = canvas.unwrap_or_else(find_canvas);
            validate(&canvas_path)
        }
        Command::Check { canvas } => {
            let canvas_path = canvas.unwrap_or_else(find_canvas);
            check(&canvas_path)
        }
        Command::List { canvas } => {
            let canvas_path = canvas.unwrap_or_else(find_canvas);
            list(&canvas_path)
        }
        Command::Static { canvas, template, out, force } => {
            let canvas_path = canvas.unwrap_or_else(find_canvas);
            static_cmd(&canvas_path, &template, &out, force)
        }
        Command::Node { command } => match command {
            NodeCommand::Add { canvas, parent_id, title } => {
                node_add(&canvas.unwrap_or_else(find_canvas), &parent_id, &title)
            }
            NodeCommand::Rm { canvas, node_id, keep_children } => {
                node_rm(&canvas.unwrap_or_else(find_canvas), &node_id, keep_children)
            }
            NodeCommand::Mv { canvas, node_id, new_parent_id } => {
                node_mv(&canvas.unwrap_or_else(find_canvas), &node_id, &new_parent_id)
            }
            NodeCommand::Rename { canvas, node_id, title } => {
                node_rename(&canvas.unwrap_or_else(find_canvas), &node_id, &title)
            }
            NodeCommand::SetId { canvas, node_id, new_id } => {
                node_set_id(&canvas.unwrap_or_else(find_canvas), &node_id, &new_id)
            }
            NodeCommand::Body { canvas, node_id, file } => {
                node_body(&canvas.unwrap_or_else(find_canvas), &node_id, file)
            }
            NodeCommand::Meta { canvas, node_id, x, y, width, height, color, node_type, display, lang, interpreter, fold } => {
                node_meta(&canvas.unwrap_or_else(find_canvas), &node_id, x, y, width, height, color, node_type, display, lang, interpreter, fold)
            }
            NodeCommand::Edges { canvas, node_id, from, clear } => {
                node_edges(&canvas.unwrap_or_else(find_canvas), &node_id, from, clear)
            }
            NodeCommand::Reorder { canvas } => node_reorder(&canvas.unwrap_or_else(find_canvas)),
            NodeCommand::Show { canvas, node_id } => node_show(&canvas.unwrap_or_else(find_canvas), &node_id),
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
    let is_release_tag = label.starts_with('v') && label[1..].starts_with(|c: char| c.is_ascii_digit());
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
/// in play — env, then cache, then its own `default` — same precedence
/// `vars::resolve` uses, minus the `--set`/form-override step `configure`
/// doesn't take. Shown as the prompt's own default so Enter keeps it.
fn current_value(decl: &VarDecl, cache: &VarCache) -> Option<String> {
    std::env::var(&decl.name)
        .ok()
        .or_else(|| cache.get(&decl.name).map(str::to_string))
        .or_else(|| decl.default.clone())
}

fn configure(canvas_path: &PathBuf) {
    let raw = std::fs::read_to_string(canvas_path).unwrap_or_else(|e| {
        eprintln!("failed to read {}: {e}", canvas_path.display());
        std::process::exit(1);
    });
    let decls = declared_vars_or_exit(canvas_path, &raw);
    let secret_count = decls.iter().filter(|d| d.secret).count();
    let configurable: Vec<&VarDecl> = decls.iter().filter(|d| !d.secret).collect();

    if configurable.is_empty() {
        if secret_count > 0 {
            println!(
                "meshfox configure: {secret_count} declared variable(s) are secret — never \
                 cached, so there's nothing to save for them; they're asked for fresh every \
                 time a run needs them."
            );
        } else {
            println!("meshfox configure: {} declares no variables", canvas_path.display());
        }
        return;
    }

    if !prompt::stdin_is_tty() {
        eprintln!("meshfox configure: requires an interactive terminal (stdin isn't one)");
        std::process::exit(1);
    }

    let mut cache = load_var_cache_or_exit(canvas_path);
    if secret_count > 0 {
        println!(
            "({secret_count} secret variable(s) skipped — never cached, always asked fresh at run time)"
        );
    }

    for decl in configurable.iter().copied() {
        let current = current_value(decl, &cache);
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

fn validate(canvas_path: &PathBuf) {
    let raw = std::fs::read_to_string(canvas_path).unwrap_or_else(|e| {
        eprintln!("failed to read {}: {e}", canvas_path.display());
        std::process::exit(1);
    });
    match Canvas::from_markdown(&raw) {
        Ok(canvas) => {
            // Resolving includes here is the only way to catch a broken
            // link, a cycle, or a target that doesn't itself parse before
            // `meshfox view` does — this file's own structure is already
            // known good at this point regardless of the outcome below.
            if let Err(e) = meshfox_core::include::resolve(&canvas, canvas_path) {
                eprintln!("meshfox validate: {}: {e}", canvas_path.display());
                std::process::exit(1);
            }
            // Same idea for runnable-fence `deps=`: a dangling reference or
            // a cycle should fail CI/pre-commit here, not surface only when
            // someone actually tries to run the block.
            if let Err(e) = meshfox_core::deps::validate(&canvas) {
                eprintln!("meshfox validate: {}: {e}", canvas_path.display());
                std::process::exit(1);
            }
            // Validates every `meshfox:var` declaration itself (unique
            // name, declared in the root, `select` has `choices=`, ...)
            // and every runnable fence's `env=` reference against them —
            // a typo'd variable name would otherwise just silently
            // resolve to nothing instead of failing loudly (see
            // `vars::resolve_block_env`).
            if let Err(e) = meshfox_core::validate_env_refs(&canvas) {
                eprintln!("meshfox validate: {}: {e}", canvas_path.display());
                std::process::exit(1);
            }
            // Same idea for `meshfox:option` (unique name, declared in the
            // root) — a misplaced or duplicated option should fail loudly
            // here rather than just being silently ignored by whatever
            // consumer reads `Canvas::options`.
            if let Err(e) = meshfox_core::declared_options(&canvas) {
                eprintln!("meshfox validate: {}: {e}", canvas_path.display());
                std::process::exit(1);
            }
            let n = canvas.nodes.len();
            println!(
                "meshfox validate: {} ok ({n} node{})",
                canvas_path.display(),
                if n == 1 { "" } else { "s" }
            );
        }
        Err(e) => {
            eprintln!("meshfox validate: {}: {e}", canvas_path.display());
            std::process::exit(1);
        }
    }
}

/// Runs every embedded constraint fence's Starlark contract
/// (`meshfox_core::constraint::evaluate`) and reports pass/fail per fence,
/// same exit-code convention as every other check here (non-zero if the
/// file doesn't parse, or if any constraint fails) — usable in CI/pre-commit
/// alongside `validate`.
fn check(canvas_path: &PathBuf) {
    let raw = std::fs::read_to_string(canvas_path).unwrap_or_else(|e| {
        eprintln!("failed to read {}: {e}", canvas_path.display());
        std::process::exit(1);
    });
    let canvas = Canvas::from_markdown(&raw).unwrap_or_else(|e| {
        eprintln!("meshfox check: {}: {e}", canvas_path.display());
        std::process::exit(1);
    });

    let results = meshfox_core::evaluate_constraints(&canvas);
    if results.is_empty() {
        println!("meshfox check: {} ok (no constraints)", canvas_path.display());
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

/// The empty-canvas template shared by `create` and `view --create`: the
/// `meshfox:canvas` marker plus a lone root heading named after the file
/// itself, so the new file is immediately valid and auto-discoverable.
fn write_canvas_template(canvas_path: &Path) {
    let title = canvas_title(canvas_path);
    let content = format!("{}\n# {title}\n", mdcanvas::CANVAS_MARKER);
    std::fs::write(canvas_path, content).unwrap_or_else(|e| {
        eprintln!("failed to write {}: {e}", canvas_path.display());
        std::process::exit(1);
    });
}

/// `path`'s file name with a trailing `.canvas.md` or `.md` stripped —
/// just the default root-heading text, not a format invariant (an id
/// derived from a heading never has to match the filename, see SPEC.md).
fn canvas_title(path: &Path) -> String {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    name.strip_suffix(".canvas.md").or_else(|| name.strip_suffix(".md")).unwrap_or(&name).to_string()
}

fn view(canvas_path: PathBuf, port: u16, open_browser: bool, auto_exit: bool) {
    let runtime = tokio::runtime::Runtime::new().unwrap_or_else(|e| {
        eprintln!("failed to start async runtime: {e}");
        std::process::exit(1);
    });
    if let Err(e) = runtime.block_on(meshfox_server::run(canvas_path, port, open_browser, auto_exit)) {
        eprintln!("meshfox view: {e}");
        std::process::exit(1);
    }
}

fn tui(canvas_path: PathBuf) {
    let runtime = tokio::runtime::Runtime::new().unwrap_or_else(|e| {
        eprintln!("failed to start async runtime: {e}");
        std::process::exit(1);
    });
    if let Err(e) = runtime.block_on(tui::run(canvas_path)) {
        eprintln!("meshfox tui: {e}");
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
        if name.ends_with(".canvas.md") {
            candidates.push(path);
        } else if name.ends_with(".md") {
            if let Ok(contents) = std::fs::read_to_string(&path) {
                if mdcanvas::has_marker(&contents) {
                    candidates.push(path);
                }
            }
        }
    }
    match candidates.as_slice() {
        [one] => one.clone(),
        [] => {
            eprintln!(
                "no .canvas.md file (or marked *.md canvas) found in the current directory; pass the path explicitly"
            );
            std::process::exit(1);
        }
        _ => {
            eprintln!(
                "multiple canvas files found in the current directory; pass the path explicitly"
            );
            std::process::exit(1);
        }
    }
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
fn validate_set_overrides(decls: &[VarDecl], overrides: &HashMap<String, String>) -> Result<(), String> {
    for (name, value) in overrides {
        if let Some(decl) = decls.iter().find(|d| &d.name == name) {
            meshfox_core::validate_value(decl, value).map_err(|e| format!("--set {name}={value:?} is invalid: {e}"))?;
        }
    }
    Ok(())
}

/// `--set NAME=VALUE` pins the cache regardless of whether anything in
/// *this* invocation's chain actually references it — same as `cmake -D`
/// always updating `CMakeCache.txt` — so a later plain `meshfox run`
/// (even for a different block) doesn't need `--set` again.
fn persist_set_overrides(decls: &[VarDecl], overrides: &HashMap<String, String>, cache: &mut VarCache) {
    for (name, value) in overrides {
        if decls.iter().any(|d| &d.name == name && !d.secret) {
            cache.set(name, value).unwrap_or_else(|e| {
                eprintln!("failed to save {name}: {e}");
                std::process::exit(1);
            });
        }
    }
}

/// Resolves *only* the declared variables `block`'s own `env=` references
/// (see SPEC.md's "Variables") — a block with no `env=` never resolves or
/// prompts for anything, however many variables the document declares.
/// Tries `overrides` (`--set`)/the process environment/the on-disk cache/
/// each declaration's own `default` first (`resolve_block_env` — a
/// `required` declaration skips that last step, so it shows up here even
/// when it has a `default`); whatever that leaves missing gets a terminal
/// prompt — pre-filled with the declaration's own `default` so a
/// `required` one can just be confirmed with Enter — its non-secret
/// answer saved back to the cache so a later run of *any* block
/// referencing the same variable doesn't ask again. Exits with an error
/// instead of prompting when stdin isn't a terminal, same as `configure`.
fn resolve_block_env_or_prompt(
    block: &meshfox_core::CodeBlock,
    decls: &[VarDecl],
    overrides: &HashMap<String, String>,
    cache: &mut VarCache,
) -> HashMap<String, String> {
    if block.env.is_empty() {
        return HashMap::new();
    }
    let mut resolution = meshfox_core::resolve_block_env(&block.env, decls, overrides, cache);
    if resolution.missing.is_empty() {
        return resolution.env;
    }
    if !prompt::stdin_is_tty() {
        let names: Vec<&str> = resolution.missing.iter().map(|d| d.name.as_str()).collect();
        eprintln!(
            "meshfox run: missing required variable(s): {} — pass --set NAME=VALUE, set the \
             environment variable, or run `meshfox configure` first",
            names.join(", ")
        );
        std::process::exit(1);
    }
    for decl in &resolution.missing {
        let value = prompt::ask(decl, decl.default.as_deref()).unwrap_or_else(|e| {
            eprintln!("failed to read input: {e}");
            std::process::exit(1);
        });
        if !decl.secret {
            cache.set(&decl.name, &value).unwrap_or_else(|e| {
                eprintln!("failed to save {}: {e}", decl.name);
                std::process::exit(1);
            });
        }
        // A block could (unusually) reference the same declared variable
        // under more than one local name — fill in every one of them.
        for er in block.env.iter().filter(|er| er.var_name == decl.name) {
            resolution.env.insert(er.local_name.clone(), value.clone());
        }
    }
    resolution.env
}

/// Runs a `tty` block: connects the child directly to the real terminal
/// (stdin/stdout/stderr all inherited) instead of the piped/captured
/// `stream_exec::spawn_bash` every other block goes through — so anything
/// that needs a genuine terminal (an interactive shell, `read -p`, `ssh`,
/// an editor, a password prompt) works exactly as it would run standalone.
/// Caller (`run_async`) has already checked stdin/stdout are actually a
/// terminal before calling this.
///
/// The child is left in `meshfox`'s own process group (no
/// `.process_group(0)`, unlike `stream_exec::spawn_bash`) so it stays part
/// of the terminal's *foreground* group and can read from it without
/// getting stopped by `SIGTTIN` — the same reason it must never become a
/// background job. That, in turn, means the terminal delivers `SIGINT`
/// (Ctrl+C) to `meshfox` itself and the child simultaneously and
/// independently, the same way a real interactive shell and whatever
/// foreground job it's running both see it. `meshfox` must not react by
/// exiting or killing the child here (default disposition, or the
/// same-process kill this file's non-`tty` branch does) — that would tear
/// the child away from the terminal mid-session while it might still be
/// legitimately running (e.g. an interactive `bash` that, like any
/// interactive shell, ignores `SIGINT` for itself and only lets it affect
/// whatever *it's* currently running in its own foreground). So `meshfox`
/// just absorbs every `SIGINT` while waiting and keeps waiting — the
/// child, as its own independent process, decides for itself whether that
/// signal ends it or not.
async fn run_tty_block(code: &str, envs: &HashMap<String, String>) -> std::io::Result<i32> {
    let mut child = tokio::process::Command::new("bash")
        .arg("-c")
        .arg(code)
        .envs(envs)
        .stdin(std::process::Stdio::inherit())
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
        .spawn()?;

    loop {
        tokio::select! {
            status = child.wait() => return Ok(status?.code().unwrap_or(-1)),
            _ = tokio::signal::ctrl_c() => continue,
        }
    }
}

/// Same chain-resolution/dedup/stop-on-failure logic `run` always had, but
/// executes each step with `meshfox_server::stream_exec` (the same async,
/// killable executor `meshfox view` uses) instead of `core`'s blocking
/// one — output prints line by line as the process produces it, instead
/// of all at once after it exits (which is also why the exit code now
/// prints *after* the output, not on the same line as `==> name`: it
/// genuinely isn't known any sooner). Ctrl+C kills whichever step is
/// currently running — the *whole process group* it spawned, not just
/// `bash` itself, so a hung child (`sleep`, a server it started, ...)
/// doesn't survive as an orphan — and stops there, persisting whatever
/// earlier steps already completed.
async fn run_async(canvas_path: &PathBuf, mut args: Vec<String>, no_deps: bool, set: Vec<(String, String)>) {
    let block_arg = args.pop().expect("clap requires at least one arg");
    let path: Vec<&str> = args.iter().map(String::as_str).collect();
    let block_names: Vec<&str> = block_arg.split(',').map(str::trim).collect();

    let mut raw = std::fs::read_to_string(canvas_path).unwrap_or_else(|e| {
        eprintln!("failed to read {}: {e}", canvas_path.display());
        std::process::exit(1);
    });

    // Declared once up front; each executed block below (in the main
    // loop) resolves only the subset its own `env=` actually references
    // (see `resolve_block_env_or_prompt`) — never the whole document's
    // variables just because *some* block somewhere declares one.
    let decls = declared_vars_or_exit(canvas_path, &raw);
    let overrides: HashMap<String, String> = set.into_iter().collect();
    validate_set_overrides_or_exit(&decls, &overrides);
    let mut var_cache = load_var_cache_or_exit(canvas_path);
    persist_set_overrides(&decls, &overrides, &mut var_cache);

    let mut had_failure = false;
    let mut already_ran: std::collections::HashSet<(String, String)> = std::collections::HashSet::new();
    for name in block_names {
        // Re-parse each iteration so a block run earlier in this loop
        // (which may have patched `raw`) is reflected before the next one.
        let canvas = Canvas::from_markdown(&raw).unwrap_or_else(|e| {
            eprintln!("failed to parse {}: {e}", canvas_path.display());
            std::process::exit(1);
        });

        // Running a block automatically runs whatever it `deps=` on first,
        // in dependency order — same chain the web UI's "⛓ run chain"
        // button triggers — unless `--no-deps` was passed, in which case
        // only `name` itself runs, same as the UI's plain "run" button. A
        // block with no deps resolves to just itself either way.
        let chain = match meshfox_core::resolve_run_chain(&canvas, &path, name, !no_deps) {
            Ok(chain) => chain,
            Err(e) => {
                eprintln!("error resolving dependencies for {name:?}: {e}");
                had_failure = true;
                continue;
            }
        };

        for addr in chain {
            let key = (addr.node_id.clone(), addr.block_name.clone());
            if !already_ran.insert(key) {
                continue; // shared dependency, already run for an earlier requested name
            }

            // Re-parse per step too, for the same reason as above.
            let canvas = Canvas::from_markdown(&raw).unwrap_or_else(|e| {
                eprintln!("failed to parse {}: {e}", canvas_path.display());
                std::process::exit(1);
            });

            let Some(node) = canvas.node(&addr.node_id) else {
                eprintln!("error running {:?}: node {:?} not found", addr.block_name, addr.node_id);
                had_failure = true;
                break;
            };
            let node_text = node.text.clone();
            let Some(block) = meshfox_core::scan_runnable_blocks(&addr.node_id, &node_text)
                .into_iter()
                .find(|b| b.name.as_deref() == Some(addr.block_name.as_str()))
            else {
                eprintln!(
                    "error running {:?}: no runnable block named {:?} in node {:?}",
                    addr.block_name, addr.block_name, addr.node_id
                );
                had_failure = true;
                break;
            };
            if !meshfox_server::stream_exec::supports(&block.lang) {
                eprintln!(
                    "error running {:?}: no executor registered for language {:?}",
                    addr.block_name, block.lang
                );
                had_failure = true;
                break;
            }

            let block_env = resolve_block_env_or_prompt(&block, &decls, &overrides, &mut var_cache);
            println!("==> {}", addr.block_name);

            let mut full_output = String::new();
            let exit_code = if block.tty {
                if !prompt::stdin_is_tty() || !std::io::stdout().is_terminal() {
                    eprintln!(
                        "error running {:?}: requires an interactive terminal (stdin/stdout isn't one)",
                        addr.block_name
                    );
                    had_failure = true;
                    break;
                }
                match run_tty_block(&block.code, &block_env).await {
                    Ok(code) => code,
                    Err(e) => {
                        eprintln!("error running {:?}: {e}", addr.block_name);
                        had_failure = true;
                        break;
                    }
                }
            } else {
                let mut proc = match meshfox_server::stream_exec::spawn_bash(&block.code, &block_env) {
                    Ok(p) => p,
                    Err(e) => {
                        eprintln!("error running {:?}: {e}", addr.block_name);
                        had_failure = true;
                        break;
                    }
                };

                loop {
                    tokio::select! {
                        line = proc.output_rx.recv() => {
                            match line {
                                Some(text) => {
                                    println!("{text}");
                                    full_output.push_str(&text);
                                    full_output.push('\n');
                                }
                                None => {
                                    let status = proc.child.wait().await;
                                    break status.ok().and_then(|s| s.code()).unwrap_or(-1);
                                }
                            }
                        }
                        _ = tokio::signal::ctrl_c() => {
                            eprintln!("^C — killing {:?} and stopping", addr.block_name);
                            let _ = proc.kill();
                            let _ = proc.child.wait().await;
                            // Persist whatever completed before this step, same
                            // as the web UI's Kill does — no reason to lose
                            // already-cached output just because a *later*
                            // step got interrupted.
                            let _ = std::fs::write(canvas_path, &raw);
                            std::process::exit(130); // 128 + SIGINT, the usual convention
                        }
                    }
                }
            };
            println!("(exit {exit_code})");

            // `tty` and `cache` are mutually exclusive (a `meshfox
            // validate` error) — `block.tty` here is belt-and-suspenders
            // against writing a `tty` block's (empty) `full_output` back
            // into the file for a document `run` was pointed at without
            // ever being validated first.
            if block.cache && !block.tty {
                let result = meshfox_core::ExecOutput { exit_code, output: full_output };
                if let Some(updated) = meshfox_core::write_output(&node_text, &addr.block_name, &result) {
                    if let Some(patched) = mdcanvas::set_node_body(&raw, &addr.node_id, &updated) {
                        raw = patched;
                    }
                }
            }

            if exit_code != 0 {
                had_failure = true;
                // Running what depends on a failed step wouldn't mean
                // anything — stop this chain, move on to the next
                // requested name (if any).
                break;
            }
        }
    }

    std::fs::write(canvas_path, &raw).unwrap_or_else(|e| {
        eprintln!("failed to write {}: {e}", canvas_path.display());
        std::process::exit(1);
    });

    if had_failure {
        std::process::exit(1);
    }
}

fn list(canvas_path: &PathBuf) {
    let raw = std::fs::read_to_string(canvas_path).unwrap_or_else(|e| {
        eprintln!("failed to read {}: {e}", canvas_path.display());
        std::process::exit(1);
    });
    let canvas = Canvas::from_markdown(&raw).unwrap_or_else(|e| {
        eprintln!("failed to parse {}: {e}", canvas_path.display());
        std::process::exit(1);
    });
    let blocks = canvas.list_runnable().unwrap_or_else(|e| {
        eprintln!("meshfox list: {}: {e}", canvas_path.display());
        std::process::exit(1);
    });
    if blocks.is_empty() {
        println!("meshfox list: no runnable blocks in {}", canvas_path.display());
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
            _ => groups.push(Group { path: &b.path, node_id: &b.node_id, blocks: vec![b] }),
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
        let default = g.blocks.len() == 1 && meshfox_core::fence::is_default(&g.blocks[0].block, g.node_id);
        let collapse = !g.path.is_empty() && default;
        let header_depth = if collapse { g.path.len() - 1 } else { g.path.len() };

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
                    Line { indent: depth, label: g.path[depth].clone(), command },
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
    let code_blocks: Vec<meshfox_core::CodeBlock> = blocks.iter().map(|b| b.block.clone()).collect();
    meshfox_core::fence::default_block(node_id, &code_blocks)
        .ok()
        .flatten()
        .is_some()
}

/// `[cache]`/`[tty]`/`[default]`/`[deps: ...]`/`[env: ...]` suffix for one block's
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
    if block.cache {
        annotations.push("cache".to_string());
    }
    if block.tty {
        annotations.push("tty".to_string());
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
    let path_args = if path.is_empty() { String::new() } else { format!("{} ", path.join(" ")) };
    format!("meshfox run {path_args}{name}")
}

fn read_raw_or_exit(canvas_path: &Path) -> String {
    std::fs::read_to_string(canvas_path).unwrap_or_else(|e| {
        eprintln!("failed to read {}: {e}", canvas_path.display());
        std::process::exit(1);
    })
}

fn write_raw_or_exit(canvas_path: &Path, content: &str) {
    std::fs::write(canvas_path, content).unwrap_or_else(|e| {
        eprintln!("failed to write {}: {e}", canvas_path.display());
        std::process::exit(1);
    });
}

fn node_add(canvas_path: &Path, parent_id: &str, title: &str) {
    let raw = read_raw_or_exit(canvas_path);
    match apply_node_add(&raw, parent_id, title) {
        Ok((updated, new_id)) => {
            write_raw_or_exit(canvas_path, &updated);
            println!("meshfox node add: added {new_id:?} under {parent_id:?} in {}", canvas_path.display());
        }
        Err(e) => {
            eprintln!("meshfox node add: {e} ({})", canvas_path.display());
            std::process::exit(1);
        }
    }
}

/// Pure logic behind `node add`: insert the child, then make sure the
/// result still parses before handing it back to the caller to write.
/// Returns `(updated document, new node's id)`.
fn apply_node_add(raw: &str, parent_id: &str, title: &str) -> Result<(String, String), String> {
    let (updated, new_id) = mdcanvas::insert_child_node(raw, parent_id, title)
        .ok_or_else(|| format!("no node {parent_id:?}"))?;
    validate_patch(&updated)?;
    Ok((updated, new_id))
}

fn node_rm(canvas_path: &Path, node_id: &str, keep_children: bool) {
    let raw = read_raw_or_exit(canvas_path);
    match apply_node_rm(&raw, node_id, keep_children) {
        Ok(updated) => {
            write_raw_or_exit(canvas_path, &updated);
            println!(
                "meshfox node rm: deleted {node_id:?}{} in {}",
                if keep_children { " (children promoted)" } else { "" },
                canvas_path.display()
            );
        }
        Err(e) => {
            eprintln!("meshfox node rm: {e} ({})", canvas_path.display());
            std::process::exit(1);
        }
    }
}

fn apply_node_rm(raw: &str, node_id: &str, keep_children: bool) -> Result<String, String> {
    let canvas = Canvas::from_markdown(raw).map_err(|e| e.to_string())?;
    let node = canvas.node(node_id).ok_or_else(|| format!("no node {node_id:?}"))?;
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
    let raw = read_raw_or_exit(canvas_path);
    match apply_node_mv(&raw, node_id, new_parent_id) {
        Ok(updated) => {
            write_raw_or_exit(canvas_path, &updated);
            println!("meshfox node mv: moved {node_id:?} under {new_parent_id:?} in {}", canvas_path.display());
        }
        Err(e) => {
            eprintln!("meshfox node mv: {e} ({})", canvas_path.display());
            std::process::exit(1);
        }
    }
}

fn apply_node_mv(raw: &str, node_id: &str, new_parent_id: &str) -> Result<String, String> {
    let canvas = Canvas::from_markdown(raw).map_err(|e| e.to_string())?;
    let node = canvas.node(node_id).ok_or_else(|| format!("no node {node_id:?}"))?;
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
    let mut updated = mdcanvas::reparent_node(&with_edge, node_id, new_parent_id).ok_or_else(|| {
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
                        edge_label: new_node.edge_label.clone(),
                        fold: new_node.fold,
                        tags: new_node.tags.clone(),
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
    let raw = read_raw_or_exit(canvas_path);
    match apply_node_rename(&raw, node_id, title) {
        Ok(updated) => {
            write_raw_or_exit(canvas_path, &updated);
            println!("meshfox node rename: renamed {node_id:?} in {}", canvas_path.display());
        }
        Err(e) => {
            eprintln!("meshfox node rename: {e} ({})", canvas_path.display());
            std::process::exit(1);
        }
    }
}

fn apply_node_rename(raw: &str, node_id: &str, title: &str) -> Result<String, String> {
    let updated =
        mdcanvas::set_node_title(raw, node_id, title).ok_or_else(|| format!("no node {node_id:?}"))?;
    validate_patch(&updated)?;
    Ok(updated)
}

fn node_set_id(canvas_path: &Path, node_id: &str, new_id: &str) {
    let raw = read_raw_or_exit(canvas_path);
    match apply_node_set_id(&raw, node_id, new_id) {
        Ok(updated) => {
            write_raw_or_exit(canvas_path, &updated);
            println!("meshfox node set-id: renamed {node_id:?} to {new_id:?} in {}", canvas_path.display());
            if let Ok(canvas) = Canvas::from_markdown(&updated) {
                if let Err(e) = meshfox_core::deps::validate(&canvas) {
                    eprintln!(
                        "meshfox node set-id: warning: {e} (a deps= reference may need fixing by hand — \
                         see `meshfox validate`)"
                    );
                }
            }
        }
        Err(e) => {
            eprintln!("meshfox node set-id: {e} ({})", canvas_path.display());
            std::process::exit(1);
        }
    }
}

fn apply_node_set_id(raw: &str, node_id: &str, new_id: &str) -> Result<String, String> {
    let updated = mdcanvas::rename_node_id(raw, node_id, new_id).map_err(|e| e.to_string())?;
    validate_patch(&updated)?;
    Ok(updated)
}

fn node_body(canvas_path: &Path, node_id: &str, file: Option<PathBuf>) {
    let raw = read_raw_or_exit(canvas_path);
    let new_body = match file {
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
    match apply_node_body(&raw, node_id, &new_body) {
        Ok(updated) => {
            write_raw_or_exit(canvas_path, &updated);
            println!("meshfox node body: updated {node_id:?} in {}", canvas_path.display());
        }
        Err(e) => {
            eprintln!("meshfox node body: {e} ({})", canvas_path.display());
            std::process::exit(1);
        }
    }
}

fn apply_node_body(raw: &str, node_id: &str, new_body: &str) -> Result<String, String> {
    let updated =
        mdcanvas::set_node_body(raw, node_id, new_body).ok_or_else(|| format!("no node {node_id:?}"))?;
    validate_patch(&updated)?;
    Ok(updated)
}

fn parse_node_type(s: &str) -> Result<NodeType, String> {
    match s {
        "text" => Ok(NodeType::Text),
        "file" => Ok(NodeType::File),
        "link" => Ok(NodeType::Link),
        "group" => Ok(NodeType::Group),
        "include" => Ok(NodeType::Include),
        _ => Err(format!("unknown --type {s:?} (expected text/file/link/group/include)")),
    }
}

fn parse_display(s: &str) -> Result<FileDisplay, String> {
    match s {
        "link" => Ok(FileDisplay::Link),
        "code" => Ok(FileDisplay::Code),
        _ => Err(format!("unknown --display {s:?} (expected link/code)")),
    }
}

#[allow(clippy::too_many_arguments)]
#[allow(clippy::too_many_arguments)]
fn node_meta(
    canvas_path: &Path,
    node_id: &str,
    x: Option<f64>,
    y: Option<f64>,
    width: Option<f64>,
    height: Option<f64>,
    color: Option<String>,
    node_type: Option<String>,
    display: Option<String>,
    lang: Option<String>,
    interpreter: Option<String>,
    fold: Option<String>,
) {
    let raw = read_raw_or_exit(canvas_path);
    match apply_node_meta(&raw, node_id, x, y, width, height, color, node_type, display, lang, interpreter, fold) {
        Ok(updated) => {
            write_raw_or_exit(canvas_path, &updated);
            println!("meshfox node meta: updated {node_id:?} in {}", canvas_path.display());
        }
        Err(e) => {
            eprintln!("meshfox node meta: {e} ({})", canvas_path.display());
            std::process::exit(1);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn apply_node_meta(
    raw: &str,
    node_id: &str,
    x: Option<f64>,
    y: Option<f64>,
    width: Option<f64>,
    height: Option<f64>,
    color: Option<String>,
    node_type: Option<String>,
    display: Option<String>,
    lang: Option<String>,
    interpreter: Option<String>,
    fold: Option<String>,
) -> Result<String, String> {
    let canvas = Canvas::from_markdown(raw).map_err(|e| e.to_string())?;
    let node = canvas.node(node_id).ok_or_else(|| format!("no node {node_id:?}"))?;

    let parsed_type = node_type.as_deref().map(parse_node_type).transpose()?;
    let parsed_display = display.as_deref().map(parse_display).transpose()?;
    // Omitted entirely (`None`) keeps whatever's already there; passed as
    // `"true"`/`"false"`/`"default"` resolves via the same shared sentinel
    // parsing the server's own node-update endpoint uses (see
    // `mdcanvas::parse_fold_override`).
    let parsed_fold = match fold.as_deref() {
        None => node.fold,
        Some(s) => meshfox_core::parse_fold_override(s)?,
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
        x: x.or(node.x),
        y: y.or(node.y),
        width: if is_group { None } else { width.or(node.width) },
        height: if is_group { None } else { height.or(node.height) },
        color: color.or_else(|| node.color.clone()),
        node_type: parsed_type,
        display: parsed_display.or(node.display),
        lang: lang.or_else(|| node.lang.clone()),
        interpreter: interpreter.or_else(|| node.interpreter.clone()),
        edge_label: node.edge_label.clone(),
        fold: parsed_fold,
        tags: node.tags.clone(),
    };
    let updated =
        mdcanvas::set_node_meta(raw, node_id, &meta).ok_or_else(|| format!("no node {node_id:?}"))?;
    validate_patch(&updated)?;
    Ok(updated)
}

fn node_edges(canvas_path: &Path, node_id: &str, from: Vec<String>, clear: bool) {
    let raw = read_raw_or_exit(canvas_path);
    let extra_parents: Vec<String> = if clear { Vec::new() } else { from };
    match apply_node_edges(&raw, node_id, &extra_parents) {
        Ok(updated) => {
            write_raw_or_exit(canvas_path, &updated);
            println!(
                "meshfox node edges: set {} extra parent(s) on {node_id:?} in {}",
                extra_parents.len(),
                canvas_path.display()
            );
        }
        Err(e) => {
            eprintln!("meshfox node edges: {e} ({})", canvas_path.display());
            std::process::exit(1);
        }
    }
}

fn apply_node_edges(raw: &str, node_id: &str, extra_parents: &[String]) -> Result<String, String> {
    let edges: Vec<ExtraEdge> = extra_parents.iter().map(|p| ExtraEdge::new(p.as_str())).collect();
    let updated = mdcanvas::set_node_edges(raw, node_id, &edges)
        .ok_or_else(|| format!("no node {node_id:?}"))?;
    validate_patch(&updated)?;
    Ok(updated)
}

fn node_reorder(canvas_path: &Path) {
    let raw = read_raw_or_exit(canvas_path);
    match apply_node_reorder(&raw) {
        Ok(updated) => {
            write_raw_or_exit(canvas_path, &updated);
            println!("meshfox node reorder: resynced sibling order in {}", canvas_path.display());
        }
        Err(e) => {
            eprintln!("meshfox node reorder: {e} ({})", canvas_path.display());
            std::process::exit(1);
        }
    }
}

fn apply_node_reorder(raw: &str) -> Result<String, String> {
    let updated = mdcanvas::reorder_by_position(raw).ok_or_else(|| "failed to parse".to_string())?;
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
    let node = canvas.node(node_id).ok_or_else(|| format!("no node {node_id:?}"))?;

    let mut out = String::new();
    out.push_str(&format!("id: {}\n", node.id));
    out.push_str(&format!("title: {}\n", node.title));
    out.push_str(&format!("type: {}\n", node.node_type.as_str()));
    out.push_str(&format!("parent: {}\n", node.parent.as_deref().unwrap_or("(root)")));
    let children: Vec<&str> = canvas.children(&node.id).iter().map(|n| n.id.as_str()).collect();
    out.push_str(&format!(
        "children: {}\n",
        if children.is_empty() { "(none)".to_string() } else { children.join(", ") }
    ));
    out.push_str(&format!(
        "extra parents: {}\n",
        if node.extra_parents.is_empty() {
            "(none)".to_string()
        } else {
            node.extra_parents.iter().map(|e| e.from.as_str()).collect::<Vec<_>>().join(", ")
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
    if node.parent.as_deref().is_some_and(|p| canvas.node(p).is_some_and(|n| n.node_type == NodeType::Group)) {
        if let Some((abs_x, abs_y)) = canvas.resolve_absolute_position(node_id) {
            out.push_str(&format!("resolved position (absolute): x={abs_x} y={abs_y}\n"));
        }
    }
    if let Some(c) = &node.color {
        out.push_str(&format!("color: {c}\n"));
    }
    if let Some(t) = &node.target {
        out.push_str(&format!("target: {t}\n"));
    }
    if let Some(d) = node.display {
        out.push_str(&format!("display: {}\n", d.as_str()));
    }
    if let Some(l) = &node.lang {
        out.push_str(&format!("lang: {l}\n"));
    }
    Ok(out)
}

fn fmt_opt_num(v: Option<f64>) -> String {
    v.map(|n| n.to_string()).unwrap_or_else(|| "?".to_string())
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
        .args(["-C", &canvas_dir.to_string_lossy(), "rev-parse", "--short", "HEAD"])
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
        eprintln!("meshfox static: {} is not a directory", template_dir.display());
        std::process::exit(1);
    }

    let out_non_empty =
        out_dir.exists() && std::fs::read_dir(out_dir).map(|mut d| d.next().is_some()).unwrap_or(false);
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
    let canvas_dir = canvas_path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or_else(|| Path::new("."));
    let (site, assets) = meshfox_core::staticgen::build(&canvas, canvas_dir, config.base_url.as_deref());
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
        eprintln!("meshfox static: failed to load templates from {}: {e}", template_dir.display());
        std::process::exit(1);
    });

    let mut count = 0;
    for name in tera.get_template_names() {
        // A `_`-prefixed basename is a partial — imported by another
        // template (`{% import "_macros.html.tera" as macros %}`), never
        // rendered as its own output page. Same convention Jekyll/
        // Eleventy use for includes/partials.
        let is_partial = Path::new(name).file_name().and_then(|f| f.to_str()).is_some_and(|b| b.starts_with('_'));
        if is_partial {
            continue;
        }
        let rendered = tera.render(name, &context).unwrap_or_else(|e| {
            eprintln!("meshfox static: failed to render {name}: {e}");
            std::process::exit(1);
        });
        write_output_file(&out_dir.join(Path::new(name).with_extension("")), rendered.as_bytes()).unwrap_or_else(|e| {
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
            eprintln!("meshfox static: failed to read {}: {e}", asset.source.display());
            std::process::exit(1);
        });
        write_output_file(&out_dir.join(&asset.dest_rel), &bytes).unwrap_or_else(|e| {
            eprintln!("meshfox static: {e}");
            std::process::exit(1);
        });
        count += 1;
    }

    println!("meshfox static: wrote {count} file(s) to {}", out_dir.display());
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
    for entry in std::fs::read_dir(dir).map_err(|e| format!("failed to read {}: {e}", dir.display()))? {
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
        let rel = path.strip_prefix(root).expect("walked from root, so always a prefix");
        let bytes = std::fs::read(&path).map_err(|e| format!("failed to read {}: {e}", path.display()))?;
        write_output_file(&out_root.join(rel), &bytes)?;
        count += 1;
    }
    Ok(count)
}

fn write_output_file(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("failed to create {}: {e}", parent.display()))?;
    }
    std::fs::write(path, bytes).map_err(|e| format!("failed to write {}: {e}", path.display()))
}

fn validate_patch(updated: &str) -> Result<(), String> {
    Canvas::from_markdown(updated).map(|_| ()).map_err(|e| e.to_string())
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
        assert_eq!(canvas.node("new-check").unwrap().parent.as_deref(), Some("tests"));
    }

    #[test]
    fn add_rejects_an_unknown_parent() {
        let err = apply_node_add(TEST_DOC, "nope", "New Check").unwrap_err();
        assert!(err.contains("no node"), "unexpected error: {err}");
    }

    #[test]
    fn rm_deletes_the_whole_subtree_by_default() {
        let updated = apply_node_rm(TEST_DOC, "tests", false).unwrap();
        let canvas = Canvas::from_markdown(&updated).unwrap();
        assert!(canvas.node("tests").is_none());
        assert!(canvas.node("smoke-test").is_none());
        // dangling edge from shared-smoke into the deleted subtree is
        // cleaned up too, so the result still parses.
        assert!(canvas.node("shared-smoke").unwrap().extra_parents.is_empty());
    }

    #[test]
    fn rm_keep_children_promotes_direct_children_instead() {
        let updated = apply_node_rm(TEST_DOC, "tests", true).unwrap();
        let canvas = Canvas::from_markdown(&updated).unwrap();
        assert!(canvas.node("tests").is_none());
        assert_eq!(canvas.node("smoke-test").unwrap().parent.as_deref(), Some("root"));
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
        let original_text = Canvas::from_markdown(TEST_DOC).unwrap().node("smoke-test").unwrap().text.clone();
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
        let updated =
            apply_node_meta(TEST_DOC, "smoke-test", Some(10.0), None, None, None, None, None, None, None, None, None)
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
    fn meta_rejects_a_size_on_a_group() {
        let err =
            apply_node_meta(TEST_DOC, "tests", None, None, Some(300.0), None, None, None, None, None, None, None)
                .unwrap_err();
        assert!(err.contains("group"), "unexpected error: {err}");

        let err =
            apply_node_meta(TEST_DOC, "tests", None, None, None, Some(120.0), None, None, None, None, None, None)
                .unwrap_err();
        assert!(err.contains("group"), "unexpected error: {err}");
    }

    #[test]
    fn meta_accepts_an_anchor_on_a_group() {
        let updated =
            apply_node_meta(TEST_DOC, "tests", Some(1000.0), Some(2000.0), None, None, None, None, None, None, None, None)
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
            TEST_DOC, "tests", None, None, None, None, None, None, None, None, None,
            Some("true".to_string()),
        )
        .unwrap();
        assert_eq!(Canvas::from_markdown(&updated).unwrap().node("tests").unwrap().fold, Some(true));

        let updated = apply_node_meta(
            &updated, "tests", None, None, None, None, None, None, None, None, None,
            Some("default".to_string()),
        )
        .unwrap();
        assert_eq!(Canvas::from_markdown(&updated).unwrap().node("tests").unwrap().fold, None);
    }

    #[test]
    fn meta_rejects_an_unknown_fold_value() {
        let err = apply_node_meta(
            TEST_DOC, "tests", None, None, None, None, None, None, None, None, None,
            Some("bogus".to_string()),
        )
        .unwrap_err();
        assert!(err.contains("fold"), "unexpected error: {err}");
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
            None,
            Some("bogus".to_string()),
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
        let updated = apply_node_edges(TEST_DOC, "shared-smoke", &["examples".to_string()]).unwrap();
        let canvas = Canvas::from_markdown(&updated).unwrap();
        assert_eq!(canvas.node("shared-smoke").unwrap().extra_parents, vec![ExtraEdge::new("examples")]);
    }

    #[test]
    fn edges_empty_list_clears_them() {
        let updated = apply_node_edges(TEST_DOC, "shared-smoke", &[]).unwrap();
        let canvas = Canvas::from_markdown(&updated).unwrap();
        assert!(canvas.node("shared-smoke").unwrap().extra_parents.is_empty());
    }

    #[test]
    fn reorder_is_idempotent_when_already_sorted() {
        let updated = apply_node_reorder(TEST_DOC).unwrap();
        assert_eq!(Canvas::from_markdown(&updated).unwrap().nodes.len(), Canvas::from_markdown(TEST_DOC).unwrap().nodes.len());
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
    fn show_reports_a_group_members_resolved_absolute_position() {
        const DOC: &str = concat!(
            "# Root\n<!-- meshfox:node id=\"root\" -->\n\n",
            "## Frame\n<!-- meshfox:node id=\"frame\" type=\"group\" x=1000 y=1000 -->\n\n",
            "### Member\n<!-- meshfox:node id=\"member\" x=20 y=20 w=100 h=60 -->\n\nbody\n",
        );
        let text = format_node_show(DOC, "member").unwrap();
        assert!(text.contains("position: x=20 y=20 w=100 h=60"), "{text}");
        assert!(text.contains("resolved position (absolute): x=1020 y=1020"), "{text}");
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
        let decls = vec![decl("LEVEL", meshfox_core::VarType::Select, &["debug", "info"])];
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
