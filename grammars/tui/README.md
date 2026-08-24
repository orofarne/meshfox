# TUI-only overrides for the pool above

`syntect`-compatible rewrites of a same-named `../*.tmLanguage.json` entry
that doesn't load into `syntect` as-is — the *built-in* equivalent of
`.meshfox/syntax/tui/`'s own override mechanism (see
`meshfox_core::syntax_dirs::tui_syntax_dir`'s doc comment for the general
idea), same `<dir>/tui/<name>` shape, just for meshfox's own bundled pool
rather than a user-supplied grammar — see `crates/cli/src/
syntax_registry.rs` for where each side's own copy actually gets loaded
from (`include_str!`, not read from disk at runtime). `vscode-textmate`
(the web side's engine) never shares the `\G` limitation that makes these
rewrites necessary — but whether the web side actually loads the original,
unmodified file from `../` directly depends on whether Shiki already
bundles the language natively. Starlark: yes (`web/src/starlarkGrammar.ts`
reads `../starlark.tmLanguage.json`). The five below: no — Shiki already
had all five natively (that's how they were found in the first place, see
`../README.md`), so nothing on the web side references `../` for them at
all; only `syntect_tmlanguage::load` ever sees these patched copies.

- `starlark.tmLanguage.json` — same file as `../starlark.tmLanguage.json`,
  minus the one `\G` anchor in its `line-continuation` rule's `end` pattern
  (`syntect`'s `onig` backend never matches `\G` in any position — a
  documented `syntect-tmlanguage` limitation). Loses precise highlighting
  for the one construct that pattern guards against (a `\`-continued line
  immediately followed by nothing) — an edge case Starlark constraint
  fences essentially never hit in practice.
- `ini.tmLanguage.json` — the two comment rules' (`#`/`;`) outer wrapper
  used `end: "(?!\\G)"` as a "don't close before the inner comment-body
  rule gets a chance to run once" guard. Replaced with `end: "(?!#)"` /
  `end: "(?!;)"` respectively — same effect without `\G`: right after
  `begin` the cursor sits exactly on the `#`/`;`, so the negative lookahead
  is false there (forcing the inner rule to run first) and becomes true
  only once that character's been consumed.
- `powershell.tmLanguage.json` — the two comment-based-help patterns
  (`.SYNOPSIS`, `.PARAMETER`, ...) matched `(?:^|\G)` (start of line, or
  right after the previous match). Dropped the `\G` alternative, keeping
  just `^` — these directives are always written at the start of their own
  line in real PowerShell comment-help blocks, so this loses nothing in
  practice.
- `typescript.tmLanguage.json` / `tsx.tmLanguage.json` / `jsx.tmLanguage.json`
  — identical embedded JSDoc-comment sub-grammar in all three (`@example
  <caption>`, `{@link ...}` inline tags, `{Type}` annotations), each using
  a leading `\G` to mean "immediately here, not a later match". All four
  occurrences are the first (only meaningful) pattern tried at that
  position anyway, so dropping the anchor changes nothing for well-formed
  JSDoc — verified against real code, not just "does it load"
  (`crates/cli/src/syntax_registry.rs`'s
  `the_g_anchor_patched_grammars_still_highlight_real_code` test). Core
  TS/TSX/JSX language parsing (outside doc-comment internals) is
  untouched.

Swift isn't vendored anywhere in this pool at all (not even in `../`,
unpatched) — its real grammar uses `\G` 65 times, woven through core
declaration/generics parsing rather than one peripheral rule, so a
mechanical strip risks silently wrong highlighting across ordinary Swift
code, and there's no point carrying an unused file around either. Swift
stays plain-text in the TUI for now (`../README.md` has the source it'd
come from, if someone wants to hand-adapt it properly later).
