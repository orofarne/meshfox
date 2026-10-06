<!-- meshfox:canvas -->
# meshfox for Neovim
<!-- meshfox:node id="root" -->

<!-- meshfox:comment -->
> This is a [meshfox](https://meshfox.orofarne.net/) document — open it with `meshfox view` (or `meshfox tui`) for the interactive canvas. This note is only visible here, in a plain Markdown viewer.
<!-- /meshfox:comment -->

Syntax highlighting for meshfox canvases, as a tree-sitter **injection into Markdown**. The file stays Markdown — Neovim's own `markdown` parser and every Markdown plugin keep working — and meshfox's own constructs get an extra layer on top:

- `<!-- meshfox:node ... -->` and every other `meshfox:*` marker comment (keyword, attribute names, values), including a close marker in the middle of a line (`text<!-- /meshfox:comment -->`);
- the attributes in a fence's info string — ` ```bash name="x" cache `, ` ```starlark constraint `;
- the `field var=... label=...` lines in a ` ```form ` body;
- optionally the Starlark body of ` ```starlark constraint ` fences.

Tag names and attribute keys are not enumerated, so a new marker or attribute in SPEC.md needs no change here. The grammar being mirrored is SPEC.md's "Formal grammar" section.

Needs Neovim 0.10+ (developed against 0.12), the `tree-sitter` CLI and a C compiler. No `nvim-treesitter` required.

## Install
<!-- meshfox:node id="install" -->

Build the parser into `parser/meshfox.so`, the place Neovim looks for it once this directory is on the runtimepath:

```sh name="build"
./build.sh
```

Optionally also build `tree-sitter-starlark` (a pinned tag, needs git and network) for the body of ` ```starlark constraint ` fences:

```sh name="build-starlark"
./build.sh --with-starlark
```

Then put this directory on the runtimepath in `init.lua`:

```lua
vim.opt.runtimepath:append('/path/to/meshfox/editors/neovim')
```

(`vim.pack.add` on the whole repo won't do: the plugin lives in a subdirectory, not at the repo root.)

Without the Starlark parser everything above still works; a constraint fence's body is just left uncolored, because Neovim has no bundled Starlark parser and an injection into a missing language is a silent no-op. Alternatives that give the same result: `:TSInstall starlark` if you use `nvim-treesitter`, or build `tree-sitter-starlark` yourself into any `parser/` directory on the runtimepath.

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
| `plugin/meshfox.lua` | filetype detection |
| `build.sh`, `test.sh` | build the parser(s); run corpus tests plus a headless pass over the repo's canvases |

The parser's sources live in `grammar/`, not at the plugin root, because the `tree-sitter` CLI would otherwise try to load `queries/markdown/injections.scm` as the grammar's own injections.

After editing `grammar/grammar.js`, run `tree-sitter generate` in `grammar/` and commit `src/` — it's committed so a plain checkout only needs a C compiler.

## Testing
<!-- meshfox:node id="testing" -->

Corpus tests for the grammar (`grammar/test/corpus/`), then a headless Neovim pass over every meshfox document in the repo, failing on any `ERROR`/`MISSING` node in an injected region or a wrong filetype. Depends on the parser being built:

```sh name="test" deps="install/build" default
./test.sh
```

## Query-authoring gotchas
<!-- meshfox:node id="query-authoring-gotchas" -->

Neovim's `#match?` is a Vim regex run in *very-magic* mode (it prepends `\v`), so `+` and `?` are quantifiers, but a literal `<` must be written `\\<` — a bare `<` means "start of word" and silently never matches.

An injection's `(language)` child is excluded from the injected range by default; that is what lets the fence-info injection hand the meshfox parser only what follows the language.

Neovim's bundled `markdown` injections also send every `html_block`/`html_tag` to the `html` language. The meshfox injection is added next to it (`;; extends`), not instead of it.

