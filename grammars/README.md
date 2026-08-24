# Shared grammar pool

meshfox's own bundled TextMate grammars for languages `syntect` (TUI)
doesn't ship by default. Compiled into the CLI binary at build time, not
read from disk at runtime (`include_str!`, see
`crates/cli/src/syntax_registry.rs`) — this is a first-class,
meshfox-maintained set, not the user-supplied `.meshfox/syntax/`
repository (see README.md's "Syntax grammars" section for that one).

Not every entry here is also loaded by the web side — it depends on
whether Shiki's own (much larger) bundled set already covers the
language. Check before adding a new one here: Shiki's set is genuinely
large, and a fence's own info string might already just work with zero
code changes on the web side at all.

Where a file here doesn't load into `syntect` as-is, a hand-adapted
`syntect`-compatible copy lives in `tui/` instead (same name, same
`<dir>/tui/<name>` shape as `.meshfox/syntax/tui/`'s own user-facing
override mechanism — see `syntax_dirs::tui_syntax_dir`'s doc comment for
the general idea, and `tui/README.md` here for this pool's own
specifics).

- `starlark.tmLanguage.json` — vendored from
  [bazelbuild/vscode-bazel](https://github.com/bazelbuild/vscode-bazel)'s
  `syntaxes/starlark.tmLanguage.json` (Apache-2.0, `package.json` version
  0.14.0), for ` ```starlark constraint ` fences (SPEC.md's "Constraint
  fences") — neither engine bundles Starlark, so **both sides** load a
  copy from here (`web/src/starlarkGrammar.ts` for the web side).
  Doesn't load into `syntect` as-is — one rule (`line-continuation`) uses
  a `\G` anchor, which `syntect`'s `onig` backend never matches in any
  position (a documented `syntect-tmlanguage` limitation) — see `tui/`'s
  patched copy, which drops just that one anchor.
- `elixir.tmLanguage.json` — vendored from
  [elixir-lsp/vscode-elixir-ls](https://github.com/elixir-lsp/vscode-elixir-ls)'s
  `syntaxes/elixir.json` (MIT). The original motivating case for the whole
  "Единая база языков подсветки" effort (TODO.canvas.md) — but **only
  `syntect` needed one**: Shiki already bundles Elixir natively (`'elixir'
  in bundledLanguages`), so nothing on the web side references this file
  at all — doing so would be redundant, and potentially a step behind
  Shiki's own, separately-maintained copy. Loads into `syntect` unmodified
  (0 `begin`/`while` rules, 0 `\G` anchors — checked directly), so unlike
  Starlark this one has no `tui/` counterpart either.

## Batch added from the 2026-08-24 audit

TODO.canvas.md's "Единая база языков подсветки" audit fed all 346 of
Shiki's bundled languages through `syntect_tmlanguage::load` directly
(dumped from `@shikijs/langs`, the same package Shiki itself ships) to find
which ones load into `syntect` for free. 124 did, with zero changes; the 16
below (chosen from that set) are the ones actually pulled in so far — same
reasoning as Elixir throughout:
**every one of these is already bundled by Shiki natively**, so none of
them are referenced anywhere on the web side (`web/src/shiki.ts`) — only
`crates/cli/src/syntax_registry.rs`'s `BUNDLED_POOL_ADDITIONS` loads them,
TUI-only. Upstream source/license for each was checked against the actual
GitHub repo (via `sources-grammars.ts` in
[shikijs/textmate-grammars-themes](https://github.com/shikijs/textmate-grammars-themes),
the project Shiki's own grammar package is built from), not assumed from
Shiki's own blanket MIT repackaging license — that distinction mattered in
practice: the one candidate not on this list, **nginx**
(`hangxingliu/vscode-nginx-conf-hint`), turned out to be genuinely
**GPL-3.0** upstream, a real copyleft conflict with meshfox's MIT
licensing, so it was deliberately skipped rather than vendored.

Loads unmodified, no `tui/` counterpart needed:

- `asm.tmLanguage.json` — [13xforever/x86_64-assembly-vscode](https://github.com/13xforever/x86_64-assembly-vscode) (MIT)
- `csv.tmLanguage.json` / `tsv.tmLanguage.json` — [mechatroner/vscode_rainbow_csv](https://github.com/mechatroner/vscode_rainbow_csv) (MIT)
- `dart.tmLanguage.json` — [microsoft/vscode](https://github.com/microsoft/vscode)'s `extensions/dart` (MIT)
- `docker.tmLanguage.json` — [microsoft/vscode](https://github.com/microsoft/vscode)'s `extensions/docker` (MIT)
- `gherkin.tmLanguage.json` — [alexkrechik/VSCucumberAutoComplete](https://github.com/alexkrechik/VSCucumberAutoComplete) (MIT)
- `graphql.tmLanguage.json` — [graphql/vscode-graphql](https://github.com/graphql/vscode-graphql) (MIT)
- `http.tmLanguage.json` — [Huachao/vscode-restclient](https://github.com/Huachao/vscode-restclient) (MIT)
- `kotlin.tmLanguage.json` — [fwcd/vscode-kotlin](https://github.com/fwcd/vscode-kotlin) (MIT)
- `nushell.tmLanguage.json` — [nushell/vscode-nushell-lang](https://github.com/nushell/vscode-nushell-lang) (MIT)
- `puppet.tmLanguage.json` — [octref/puppet-vscode](https://github.com/octref/puppet-vscode) (Apache-2.0)

Needed a `\G`-anchor patch first — canonical (this file, unmodified) plus a
patched `tui/` copy, same shape as Starlark (see `tui/README.md` for what
each patch actually does):

- `ini.tmLanguage.json` — [microsoft/vscode](https://github.com/microsoft/vscode)'s `extensions/ini` (MIT)
- `powershell.tmLanguage.json` — [microsoft/vscode](https://github.com/microsoft/vscode)'s `extensions/powershell` (MIT)
- `typescript.tmLanguage.json` — [microsoft/vscode](https://github.com/microsoft/vscode)'s `extensions/typescript-basics` (MIT)
- `tsx.tmLanguage.json` — [microsoft/vscode](https://github.com/microsoft/vscode)'s `extensions/typescript-basics` (MIT)
- `jsx.tmLanguage.json` — [microsoft/vscode](https://github.com/microsoft/vscode)'s `extensions/javascript` (MIT)

Swift was in the same audit batch ([jtbandes/swift-tmlanguage](https://github.com/jtbandes/swift-tmlanguage),
MIT) but isn't vendored here at all: its real grammar uses `\G` 65 times,
woven through core declaration/generics parsing rather than confined to
one peripheral rule — not a safe mechanical patch the way the five above
were, and not worth carrying an unused file for. Swift stays plain-text in
the TUI until someone's willing to either hand-adapt the grammar properly
or write an independent one.
