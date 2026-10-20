<!-- meshfox:canvas -->
# meshfox for Neovim
<!-- meshfox:node id="root" -->

<!-- meshfox:comment -->
> This is a [meshfox](https://meshfox.orofarne.net/) document — open it with `meshfox view` (or `meshfox tui`) for the interactive canvas. This note is only visible here, in a plain Markdown viewer.
<!-- /meshfox:comment -->

Editing meshfox canvases in Neovim, in two independent parts:

- **Syntax highlighting**, as a tree-sitter **injection into Markdown**. The file stays Markdown — Neovim's own `markdown` parser and every Markdown plugin keep working — and meshfox's own constructs get an extra layer on top:
  - `<!-- meshfox:node ... -->` and every other `meshfox:*` marker comment (keyword, attribute names, values), including a close marker in the middle of a line (`text<!-- /meshfox:comment -->`);
  - the attributes in a fence's info string — ` ```bash name="x" cache `, ` ```starlark constraint `;
  - the `field var=... label=...` lines in a ` ```form ` body;
  - optionally the Starlark body of ` ```starlark constraint ` fences.
- **A language server**, `meshfox lsp`: diagnostics, a node outline, completion for attributes and their values, and "turn this heading into a node" (`gra`, or `<leader>mn`). See "Language server" below.

Tag names and attribute keys are not enumerated in the grammar, so a new marker or attribute in SPEC.md needs no change to the highlighting. The grammar being mirrored is SPEC.md's "Formal grammar" section.

Highlighting needs Neovim 0.10+ (developed against 0.12), the `tree-sitter` CLI and a C compiler; no `nvim-treesitter`. The language server needs Neovim 0.11+ and a `meshfox` on `$PATH` that has the `lsp` subcommand.

<!-- meshfox:var name="NVIM_PLUGIN_DIR" type="dir" prompt="Where to install the Neovim plugin?" default="~/.local/share/nvim/site/pack/meshfox/start/meshfox" -->
<!-- meshfox:var name="WITH_STARLARK" type="bool" prompt="Also build and install the Starlark parser?" default="false" -->

Two settings (declared right here as `meshfox:var`s, asked for the first time they are needed and remembered in `.meshfox/README.canvas.md.env`; `meshfox run --set NAME=value` overrides one): `NVIM_PLUGIN_DIR`, the directory the plugin is installed into, and `WITH_STARLARK`, whether the optional Starlark parser is built and installed as well. See "Install".

## Install
<!-- meshfox:node id="install" -->

Three steps, each a block of this document — run one from the document (`meshfox editors/neovim/README.canvas.md run install <block>`, from any directory — blocks run in the canvas's own — or the run button in `meshfox tui`/`view`), or paste it into a shell in this directory. Running `install` runs the other two first.

**1. `build`** — the parser, into `parser/meshfox.so`; it is skipped while `grammar/src/parser.c` is unchanged and the output is there. It needs the `tree-sitter` CLI and a C compiler; `grammar/src/parser.c` is committed, so `tree-sitter generate` (and node) is not needed just to build.

```sh name="build" inputs="grammar/src/parser.c" outputs="parser/meshfox.so"
set -eu
mkdir -p parser
tree-sitter build -o parser/meshfox.so grammar
echo "built parser/meshfox.so"
```

**2. `build-starlark`** — the optional Starlark parser (a pinned tag, needs git and network), for the body of ` ```starlark constraint ` fences. It does something only when `WITH_STARLARK` is `true` (`meshfox run --set WITH_STARLARK=true …`, or answer yes when asked); otherwise it just says it is skipped.

```sh name="build-starlark" deps="build" env="$WITH_STARLARK"
set -eu
if [ "$WITH_STARLARK" != true ]; then
  echo "skipped: WITH_STARLARK is not true"
  exit 0
fi
# Pinned so a rebuild is reproducible.
STARLARK_REPO=https://github.com/tree-sitter-grammars/tree-sitter-starlark
STARLARK_REF=v1.3.0
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
git clone --depth 1 --branch "$STARLARK_REF" "$STARLARK_REPO" "$tmp/starlark"
tree-sitter build -o parser/starlark.so "$tmp/starlark"
mkdir -p queries/starlark
cp "$tmp/starlark/queries/highlights.scm" queries/starlark/highlights.scm
echo "built parser/starlark.so"
```

Without the Starlark parser everything else still works; a constraint fence's body is just left uncolored, because Neovim has no bundled Starlark parser and an injection into a missing language is a silent no-op. Alternatives that give the same result: `:TSInstall starlark` if you use `nvim-treesitter`, or build `tree-sitter-starlark` yourself into any `parser/` directory on the runtimepath.

**3. `install`** — copy the plugin (`plugin/`, `queries/`, `parser/`) into `$NVIM_PLUGIN_DIR`. The default, `~/.local/share/nvim/site/pack/meshfox/start/meshfox`, is a directory of Neovim's own package mechanism (`pack/*/start/*`), loaded at startup with nothing to add to `init.lua`; point `NVIM_PLUGIN_DIR` elsewhere if your Neovim's data directory is not `~/.local/share/nvim` (`:echo stdpath('data')`), or if you use another plugin manager's directory. It replaces only what it installs (never the whole directory); with `WITH_STARLARK` off it also removes the Starlark parser and queries it installed earlier. Run it again after updating the checkout.

```sh name="install" deps="build,build-starlark" env="$NVIM_PLUGIN_DIR,$WITH_STARLARK"
set -eu
dest="${NVIM_PLUGIN_DIR:?NVIM_PLUGIN_DIR is not set}"
mkdir -p "$dest/parser" "$dest/queries"
rm -rf "$dest/plugin" "$dest/queries/markdown" "$dest/queries/markdown_inline" "$dest/queries/meshfox"
cp -R plugin "$dest/plugin"
cp -R queries/markdown queries/markdown_inline queries/meshfox "$dest/queries/"
cp parser/meshfox.so "$dest/parser/meshfox.so"
if [ "$WITH_STARLARK" = true ]; then
  rm -rf "$dest/queries/starlark"
  cp -R queries/starlark "$dest/queries/starlark"
  cp parser/starlark.so "$dest/parser/starlark.so"
else
  rm -rf "$dest/queries/starlark" "$dest/parser/starlark.so"
fi
echo "installed to $dest"
```

**Or use the checkout in place**, without `install` (handy while working on the grammar): run `build` (and `build-starlark`), then put this directory on the runtimepath in `init.lua`:

```lua
vim.opt.runtimepath:append('/path/to/meshfox/editors/neovim')
```

(`vim.pack.add` on the whole repo won't do: the plugin lives in a subdirectory, not at the repo root.) Use one of the two, not both — with the plugin on the runtimepath twice its `plugin/meshfox.lua` runs twice.

The language server needs nothing built here: it is the `meshfox lsp` subcommand of the `meshfox` binary on your `$PATH` (from a checkout, `meshfox README.md run install-from-source` at the repository root installs a current one), and the plugin, installed either way, registers it — see "Language server".

## Filetype
<!-- meshfox:node id="filetype" -->

`plugin/meshfox.lua` sets the filetype to **`markdown.meshfox`** for

- any `*.canvas.md`, and
- any other `*.md` whose first line is `<!-- meshfox:canvas -->` (the repo's own README.md is one).

A compound filetype runs the ftplugins of both parts, and `vim.treesitter` resolves it to the `markdown` parser, so highlighting and your Markdown setup behave exactly as before. Put canvas-only settings in `after/ftplugin/meshfox.lua`. Other Markdown files (SPEC.md, ...) stay plain `markdown`.

The injection itself is attached to the `markdown` *language*, not to the filetype, so it also highlights `meshfox:` markers in any other Markdown file (a fence info string is only injected when it has something after the language).

## Layout
<!-- meshfox:node id="layout" -->

| | |
|---|---|
| `grammar/` | the tree-sitter grammar (`grammar.js`, generated `src/`, corpus tests) |
| `queries/meshfox/highlights.scm` | highlights for the injected language |
| `queries/markdown{,_inline}/injections.scm` | `;; extends` the bundled ones |
| `plugin/meshfox.lua` | filetype detection, registration of the language server, the `<leader>mn` mapping, and color-span attribute highlighting |
| `parser/` | the built parsers (`meshfox.so`, optionally `starlark.so`); git-ignored, made by the `build` blocks in "Install" |
| `test/colors.lua` | color-span attribute highlight checks, including inline-code exclusion |
| `test/check.lua` | the headless pass over the repo's canvases that this document's `test` block runs |

The language server itself is not in this directory: it is the `meshfox lsp` subcommand (`crates/cli/src/lsp/`), built on `meshfox_core::suggest` (`crates/core/src/suggest.rs`), which the TUI's source editor shares for `Ctrl-p`.

The parser's sources live in `grammar/`, not at the plugin root, because the `tree-sitter` CLI would otherwise try to load `queries/markdown/injections.scm` as the grammar's own injections.

After editing `grammar/grammar.js`, run `tree-sitter generate` in `grammar/` and commit `src/` — it's committed so a plain checkout only needs a C compiler.

## Testing
<!-- meshfox:node id="testing" -->

Corpus tests for the grammar (`grammar/test/corpus/`), then a headless Neovim pass (`test/check.lua`) over every meshfox document in the repo, failing on any `ERROR`/`MISSING` node in an injected region or a wrong filetype. Depends on the parser being built; run it as `meshfox editors/neovim/README.canvas.md run testing test` (it builds the parser first), or paste it into a shell in this directory after building:

```sh name="test" deps="install/build" default
set -eu
(cd grammar && tree-sitter test)
nvim --headless -u NONE -i NONE -l test/colors.lua
repo=$(git rev-parse --show-toplevel)
# shellcheck disable=SC2046
nvim --headless -u NONE -l test/check.lua \
  "$repo/README.md" "$repo/SPEC.md" \
  $(find "$repo" -name '*.canvas.md' -not -path '*/node_modules/*' -not -path '*/target/*' -not -path '*/.git/*')
```

The dedicated color-span check asserts attribute highlight ranges, short hex colors, and exclusion of inline code and invalid values. The general tree-sitter pass checks parse errors only (see the gotchas below). The language server is tested from the Rust side: `cargo test -p meshfox-core suggest`, `cargo test -p meshfox-cli --bin meshfox lsp::` and `cargo test -p meshfox-cli --test lsp_cmd` (the real binary over stdio).

## Query-authoring gotchas
<!-- meshfox:node id="query-authoring-gotchas" -->

Neovim's `#match?` is a Vim regex run in *very-magic* mode (it prepends `\v`), so `+` and `?` are quantifiers, but a literal `<` must be written `\\<` — a bare `<` means "start of word" and silently never matches.

An injection's `(language)` child is excluded from the injected range by default; that is what lets the fence-info injection hand the meshfox parser only what follows the language.

Neovim's bundled `markdown` injections also send every `html_block`/`html_tag` to the `html` language. The meshfox injection is added next to it (`;; extends`), not instead of it.

Because both layers cover the same `<!-- ... -->` range, the `html` layer's `@comment` is painted *after* ours and wins at equal priority — the whole marker then looks like a plain comment even though the injection, filetype and parser are all fine. Every pattern in `queries/meshfox/highlights.scm` therefore carries `(#set! priority 125)` (the default is 100). the `test` block only checks for parse errors, so it cannot catch this; look at `vim.inspect_pos()` on a marker, or the colour in a real session.

## Language server
<!-- meshfox:node id="language-server" -->

`plugin/meshfox.lua` registers `meshfox lsp` for the `markdown.meshfox` filetype (`vim.lsp.config` + `vim.lsp.enable`: Neovim 0.11+, `meshfox` on `$PATH`, no `nvim-lspconfig` needed). The server works on the buffer's text, saved or not; it never reads the canvas file or talks to the worker.

It is switched on once per session, on the first canvas you open, after a quick `meshfox lsp --help` to see that the installed `meshfox` has the subcommand — an older binary simply means no server, not an error on every file.

### What it gives you

- **Diagnostics** — the rules `meshfox validate` applies, except `include` targets (an included file is opened as its own canvas). The first failure is underlined on the node or attribute it is about.
- **An outline** of the nodes (`vim.lsp.buf.document_symbol()`, or any symbol picker or breadcrumb plugin).
- **Completion**, the same vocabulary as `Ctrl-p` in the TUI's source editor, through whatever LSP completion client you use (tried with `blink.cmp`: open the menu with its key, accept with `<C-y>` in the default preset). It opens by itself on a space, `"`, `,`, `/`, `<` or `$`.
  - *Attribute names* inside a `<!-- meshfox:node … -->` / `meshfox:edge` comment or a runnable fence's info string: the ones still open on that line (a node's `id` is not offered twice, `autoclose` only with `tty`, …). Accepting one inserts `name="…"` with the cursor between the quotes (`x`/`y`/`w`/`h` insert `x=0`, flags like `cache` just the word).
  - *Values*: the enumerated choices (`type=`, `display=`, `style=`, …), node ids for `parent=` and an edge's `from=`, tags already used in the document, the named blocks for `deps=` (bare in the same node, `node/block` elsewhere), and `meshfox:var` names for `env=`. In a comma-separated list the item under the cursor is completed and the ones already there are left out.
  - *Marker snippets*: typing the start of a comment (`<!-- m`) offers `meshfox:node` — with a freshly generated id preselected — and `meshfox:edge`.
  - *Making a fence runnable*: a fence in a language meshfox doesn't run by itself (`python`, `node`, …) becomes a block with `interpreter="…"`, so on its opening line that is the one attribute offered, followed by the values (`@agent`, `@python_venv`, `@node_env`, `python3`, `node`); once `interpreter=` is there, the fence gets the full set of block attributes.
- **Heading → node**, the counterpart of `Ctrl-n` in the TUI's source editor: `<!-- meshfox:node id="…" -->` with a fresh random id is added under the heading. `gra` (the standard code-action key) on a heading offers it for that heading and, as a second entry, for every plain heading of the document at once. `<leader>mn` (normal mode, canvases only) applies the single-heading action in one keystroke; on a line that isn't a plain heading Neovim just reports "No code actions available".

Completion reads the buffer, so it knows an id you typed a second ago.

### Settings

- `vim.g.meshfox_lsp_cmd = { '/path/to/meshfox', 'lsp' }` — use a particular binary (not probed).
- `vim.g.meshfox_lsp_disable = true` — no language server.
- `vim.g.meshfox_no_mappings = true` — no `<leader>mn`.

