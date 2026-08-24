//! One shared `syntect::parsing::SyntaxSet` for both TUI highlighting
//! surfaces (`tui::markdown::Highlighter`'s read-only node-body preview and
//! `edtui`'s full-screen source editor, via `SyntaxHighlighter::with_sets`)
//! — bundled defaults plus any local custom grammars a user drops into
//! `.meshfox/syntax/` (next to the canvas) or `~/.meshfox/syntax/` (global,
//! every project) — see `meshfox_core::syntax_dirs`, shared with
//! `meshfox-server`'s `/api/syntax` so the web UI can list/fetch the same
//! files. Either directory's own `tui/` subdirectory is a *TUI-only*
//! override for a same-named grammar — see `syntax_dirs::tui_syntax_dir`'s
//! doc comment. See TODO.canvas.md's "Единая база языков подсветки" for the
//! design discussion this implements.

use meshfox_core::syntax_dirs::{global_syntax_dir, local_syntax_dir, tui_syntax_dir};
use std::path::Path;
use syntect::parsing::{SyntaxSet, SyntaxSetBuilder};

/// meshfox's own `<!-- meshfox:... -->` marker-comment grammar, bundled
/// into the binary at compile time (not read from `.meshfox/syntax/` —
/// this is a built-in feature, not a user-supplied grammar). `include`s
/// `syntect`'s own bundled `text.html.markdown` scope rather than
/// reproducing it — see `crates/cli/src/grammars/meshfox.tmLanguage.json`'s
/// own comments, and TODO.canvas.md's "Единая база языков подсветки" for
/// why this *isn't* the same file the web side's injection grammar uses
/// (real VS Code TextMate injections aren't something `syntect` supports
/// at all, so this is a plain `include`-based grammar instead — same
/// marker rules, different top-level shape per engine).
const MESHFOX_GRAMMAR: &str = include_str!("grammars/meshfox.tmLanguage.json");

/// The `find_syntax_by_name` token `ui.rs`'s full-screen source editor
/// resolves to pick this grammar over plain `text.html.markdown` — see
/// `crates/cli/src/grammars/meshfox.tmLanguage.json`'s own `"name"`.
pub const MESHFOX_MARKDOWN_SYNTAX_NAME: &str = "meshfox-markdown";

/// meshfox's own bundled grammar *pool* (repo-root `grammars/`, not
/// crate-local — see `grammars/README.md`, same directory the web side's
/// `starlarkGrammar.ts` reads its own copy from). This is the TUI-only
/// (`syntect`-compatible) copy from `grammars/tui/`, not the canonical
/// upstream one (which uses a `\G` anchor `syntect`'s `onig` backend can't
/// match at all — see `grammars/tui/README.md`); the web side loads the
/// unmodified original directly. Used for ` ```starlark constraint ` fences
/// (SPEC.md's "Constraint fences").
const STARLARK_GRAMMAR_TUI: &str = include_str!("../../../grammars/tui/starlark.tmLanguage.json");

/// `grammars/elixir.tmLanguage.json` — the *other* half of meshfox's own
/// pool, and the original motivating case for the whole "Единая база
/// языков подсветки" effort (TODO.canvas.md): Elixir wasn't bundled by
/// either engine at all. Loads into `syntect` unmodified (0 `begin`/`while`
/// rules, 0 `\G` anchors — checked directly), so unlike Starlark this one
/// has no `grammars/tui/` counterpart; both sides load the exact same
/// file. Also reused by this module's own test suite below as a real,
/// non-trivial third-party grammar (nested contexts, captures, `include`)
/// to stress-test the `.meshfox/syntax/` loading path — not just a
/// hand-written toy fixture.
const ELIXIR_GRAMMAR: &str = include_str!("../../../grammars/elixir.tmLanguage.json");

/// TUI-only pool additions from TODO.canvas.md's 2026-08-24 audit ("which of
/// Shiki's 346 bundled languages can `syntect_tmlanguage` actually load") —
/// see `grammars/README.md` for the full methodology and each file's own
/// upstream source/license. None of these 16 are wired into the *web* side
/// (`web/src/shiki.ts`): Shiki already bundles every one of them natively
/// (that's exactly how they were found/vendored — dumped from Shiki's own
/// `@shikijs/langs` package), so a web-side copy would be redundant, same
/// reasoning as `ELIXIR_GRAMMAR` above. Two shapes: loads into `syntect`
/// unmodified (from the plain `grammars/` pool), or needed a `\G`-anchor
/// patch first (from `grammars/tui/`, same idea as `STARLARK_GRAMMAR_TUI`)
/// — never `begin`/`while`, which the audit confirmed is a hard blocker
/// none of these 16 hit. (Swift was in the same audit batch but isn't
/// vendored anywhere at all, not even unwired — its real grammar uses `\G`
/// 65 times, woven through core declaration/generics parsing rather than
/// confined to one peripheral rule, so a mechanical strip risks silently
/// wrong highlighting across ordinary Swift code, and there's no point
/// carrying a file that can't be wired in — see `grammars/README.md`.)
const ASM_GRAMMAR: &str = include_str!("../../../grammars/asm.tmLanguage.json");
const CSV_GRAMMAR: &str = include_str!("../../../grammars/csv.tmLanguage.json");
const DART_GRAMMAR: &str = include_str!("../../../grammars/dart.tmLanguage.json");
const DOCKER_GRAMMAR: &str = include_str!("../../../grammars/docker.tmLanguage.json");
const GHERKIN_GRAMMAR: &str = include_str!("../../../grammars/gherkin.tmLanguage.json");
const GRAPHQL_GRAMMAR: &str = include_str!("../../../grammars/graphql.tmLanguage.json");
const HTTP_GRAMMAR: &str = include_str!("../../../grammars/http.tmLanguage.json");
const KOTLIN_GRAMMAR: &str = include_str!("../../../grammars/kotlin.tmLanguage.json");
const NUSHELL_GRAMMAR: &str = include_str!("../../../grammars/nushell.tmLanguage.json");
const PUPPET_GRAMMAR: &str = include_str!("../../../grammars/puppet.tmLanguage.json");
const TSV_GRAMMAR: &str = include_str!("../../../grammars/tsv.tmLanguage.json");
const INI_GRAMMAR_TUI: &str = include_str!("../../../grammars/tui/ini.tmLanguage.json");
const POWERSHELL_GRAMMAR_TUI: &str = include_str!("../../../grammars/tui/powershell.tmLanguage.json");
const TYPESCRIPT_GRAMMAR_TUI: &str = include_str!("../../../grammars/tui/typescript.tmLanguage.json");
const TSX_GRAMMAR_TUI: &str = include_str!("../../../grammars/tui/tsx.tmLanguage.json");
const JSX_GRAMMAR_TUI: &str = include_str!("../../../grammars/tui/jsx.tmLanguage.json");

/// `(name, source)` pairs for every grammar above — looped over in
/// `build_syntax_set` rather than 16 near-identical `syntect_tmlanguage::
/// load(...).expect(...)` blocks. `name` is only used to make a load
/// failure's panic message point at the right file.
const BUNDLED_POOL_ADDITIONS: &[(&str, &str)] = &[
    ("asm", ASM_GRAMMAR),
    ("csv", CSV_GRAMMAR),
    ("dart", DART_GRAMMAR),
    ("docker", DOCKER_GRAMMAR),
    ("gherkin", GHERKIN_GRAMMAR),
    ("graphql", GRAPHQL_GRAMMAR),
    ("http", HTTP_GRAMMAR),
    ("kotlin", KOTLIN_GRAMMAR),
    ("nushell", NUSHELL_GRAMMAR),
    ("puppet", PUPPET_GRAMMAR),
    ("tsv", TSV_GRAMMAR),
    ("ini", INI_GRAMMAR_TUI),
    ("powershell", POWERSHELL_GRAMMAR_TUI),
    ("typescript", TYPESCRIPT_GRAMMAR_TUI),
    ("tsx", TSX_GRAMMAR_TUI),
    ("jsx", JSX_GRAMMAR_TUI),
];

/// Bundled `syntect` defaults plus meshfox's own grammar pool (the marker-
/// comment grammar and every `grammars/*.tmLanguage.json` entry, TUI-
/// compatible versions where one exists — see `grammars/README.md`),
/// extended with every `.tmLanguage.json`/`.sublime-syntax` grammar found
/// in the global then local syntax directories, *then* whatever TUI-only
/// override each one's own `tui/` subdirectory holds (see
/// `meshfox_core::syntax_dirs::tui_syntax_dir`'s own doc comment for why
/// that override mechanism exists at all — some real grammars just can't
/// load into `syntect`, override or not, but a same-named file rewritten
/// for its engine can). Load order is the precedence: global, then local
/// (local wins a same-named clash), then global's own `tui/` override,
/// then local's — a local TUI override beats everything else, a global one
/// beats the plain shared grammars, exactly mirroring the plain local/
/// global precedence one level down. A grammar file that fails to parse is
/// skipped with a warning on stderr, never fatal — one broken file
/// shouldn't keep the TUI from starting. The bundled pool itself is never
/// expected to fail (it ships with the binary, not user-edited), so a
/// failure there panics rather than silently losing meshfox's own
/// highlighting.
pub fn build_syntax_set(canvas_root: &Path) -> SyntaxSet {
    let mut builder = SyntaxSet::load_defaults_newlines().into_builder();
    let meshfox_def =
        syntect_tmlanguage::load(MESHFOX_GRAMMAR).expect("meshfox's own bundled grammar must parse");
    builder.add(meshfox_def);
    let starlark_def = syntect_tmlanguage::load(STARLARK_GRAMMAR_TUI)
        .expect("meshfox's own bundled Starlark grammar must parse");
    builder.add(starlark_def);
    let elixir_def = syntect_tmlanguage::load(ELIXIR_GRAMMAR)
        .expect("meshfox's own bundled Elixir grammar must parse");
    builder.add(elixir_def);
    for (label, src) in BUNDLED_POOL_ADDITIONS {
        let def = syntect_tmlanguage::load(src)
            .unwrap_or_else(|e| panic!("meshfox's own bundled {label} grammar must parse: {e}"));
        builder.add(def);
    }
    let local_dir = local_syntax_dir(canvas_root);
    if let Some(dir) = global_syntax_dir() {
        load_dir(&mut builder, &dir);
    }
    load_dir(&mut builder, &local_dir);
    if let Some(dir) = global_syntax_dir() {
        load_dir(&mut builder, &tui_syntax_dir(&dir));
    }
    load_dir(&mut builder, &tui_syntax_dir(&local_dir));
    builder.build()
}

/// Same hex values as the web side's own `--accent`/`--syntax-attr`/
/// `--syntax-value` *dark*-mode CSS custom properties (`web/src/
/// meshfoxGrammar.ts`'s `MESHFOX_DARK_HEX`) — the TUI has no light/dark
/// toggle of its own (just whichever single `syntect` theme
/// `ui::SOURCE_EDITOR_THEME` resolves to), and every bundled `syntect`
/// theme this could plausibly resolve to (`dracula`, `base16-ocean.dark`,
/// ...) is a dark-background one, so the dark palette is the only one that
/// makes sense as a single default. Returns a modified copy — `theme` is
/// typically a bundled `syntect` theme with no rules at all for meshfox's
/// own scope names (`keyword.other.meshfox`/etc, from
/// `crates/cli/src/grammars/meshfox.tmLanguage.json`), so without this a
/// meshfox marker just renders in whatever plain-text color the theme
/// happens to default unmatched scopes to — confirmed directly (a
/// throwaway scratch test comparing `HighlightLines` output before/after
/// this call, on a real `<!-- meshfox:node ... -->` line).
pub fn with_meshfox_scope_colors(mut theme: syntect::highlighting::Theme) -> syntect::highlighting::Theme {
    use syntect::highlighting::{Color, FontStyle, StyleModifier, ThemeItem};
    let accent = Color { r: 0xff, g: 0x6e, b: 0x15, a: 0xff };
    let attr = Color { r: 0xd8, g: 0xb6, b: 0x56, a: 0xff };
    let value = Color { r: 0x6f, g: 0xcf, b: 0x97, a: 0xff };
    theme.scopes.push(ThemeItem {
        scope: "keyword.other.meshfox".parse().unwrap(),
        style: StyleModifier { foreground: Some(accent), background: None, font_style: Some(FontStyle::BOLD) },
    });
    theme.scopes.push(ThemeItem {
        scope: "entity.other.attribute-name.meshfox".parse().unwrap(),
        style: StyleModifier { foreground: Some(attr), background: None, font_style: None },
    });
    theme.scopes.push(ThemeItem {
        scope: "string.unquoted.meshfox".parse().unwrap(),
        style: StyleModifier { foreground: Some(value), background: None, font_style: None },
    });
    theme
}

/// Adds every grammar in `dir` to `builder` — `.sublime-syntax` files via
/// `syntect`'s own folder loader, `.tmLanguage.json` files via
/// `syntect_tmlanguage::load_file`. A no-op (not an error) if `dir` doesn't
/// exist, which is the common case for a project/user that hasn't dropped
/// any custom grammars in yet.
fn load_dir(builder: &mut SyntaxSetBuilder, dir: &Path) {
    if !dir.is_dir() {
        return;
    }
    if let Err(e) = builder.add_from_folder(dir, true) {
        eprintln!(
            "meshfox: failed to load .sublime-syntax grammars from {}: {e}",
            dir.display()
        );
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let is_tmlanguage = path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.ends_with(".tmLanguage.json"));
        if !is_tmlanguage {
            continue;
        }
        match syntect_tmlanguage::load_file(&path) {
            Ok(def) => builder.add(def),
            Err(e) => eprintln!("meshfox: skipping {}: {e}", path.display()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    /// A fresh scratch directory, `<base>/root` standing in for a canvas
    /// root — mirrors `mcp::tests::fixture`'s style.
    fn scratch_dir() -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("meshfox-syntax-registry-test-{nanos}-{n}"))
    }

    const TINY_TMLANGUAGE: &str = r#"{
        "scopeName": "source.meshfox-test-lang",
        "name": "Meshfox Test Lang",
        "patterns": [
            { "match": "\\btest-keyword\\b", "name": "keyword.control.meshfox-test-lang" }
        ]
    }"#;

    #[test]
    fn build_syntax_set_loads_a_local_tmlanguage_grammar() {
        let root = scratch_dir();
        let syntax_dir = root.join(".meshfox").join("syntax");
        std::fs::create_dir_all(&syntax_dir).unwrap();
        std::fs::write(syntax_dir.join("test-lang.tmLanguage.json"), TINY_TMLANGUAGE).unwrap();

        let ss = build_syntax_set(&root);
        assert!(
            ss.find_syntax_by_name("Meshfox Test Lang").is_some(),
            "custom grammar should be loaded into the built SyntaxSet"
        );

        std::fs::remove_dir_all(&root).ok();
    }

    /// `ELIXIR_GRAMMAR` (this module's own bundled copy, `grammars/
    /// elixir.tmLanguage.json`) doubles as a real, non-trivial third-party
    /// grammar fixture here (not a hand-written toy one) — guards against
    /// the toy `TINY_TMLANGUAGE` fixture above passing while something real
    /// (nested contexts, captures, `include`) doesn't. Loading it a second
    /// time from a *local* `.meshfox/syntax/` (simulating a user-supplied
    /// copy) alongside the one `build_syntax_set` already bundles is
    /// harmless — `find_syntax_by_name` below doesn't care which of the two
    /// identical registrations it gets back.
    #[test]
    fn build_syntax_set_loads_a_real_third_party_grammar() {
        let root = scratch_dir();
        let syntax_dir = root.join(".meshfox").join("syntax");
        std::fs::create_dir_all(&syntax_dir).unwrap();
        std::fs::write(syntax_dir.join("elixir.tmLanguage.json"), ELIXIR_GRAMMAR).unwrap();

        let ss = build_syntax_set(&root);
        let syntax = ss
            .find_syntax_by_name("Elixir")
            .expect("real elixir.tmLanguage.json should load");
        assert_eq!(syntax.name, "Elixir");

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn build_syntax_set_skips_a_broken_grammar_without_panicking() {
        let root = scratch_dir();
        let syntax_dir = root.join(".meshfox").join("syntax");
        std::fs::create_dir_all(&syntax_dir).unwrap();
        std::fs::write(syntax_dir.join("broken.tmLanguage.json"), "{ not valid json").unwrap();

        // Must not panic, and defaults must still be there.
        let ss = build_syntax_set(&root);
        assert!(ss.find_syntax_by_extension("rs").is_some());

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn build_syntax_set_is_fine_with_no_syntax_directories_at_all() {
        let root = scratch_dir(); // never created on disk
        let ss = build_syntax_set(&root);
        assert!(ss.find_syntax_by_extension("rs").is_some());
    }

    #[test]
    fn build_syntax_set_always_includes_the_bundled_meshfox_markdown_grammar() {
        let root = scratch_dir(); // never created on disk — no user grammars involved
        let ss = build_syntax_set(&root);
        let syntax = ss
            .find_syntax_by_name(MESHFOX_MARKDOWN_SYNTAX_NAME)
            .expect("meshfox's own bundled markdown grammar should always be present");
        assert_eq!(syntax.name, MESHFOX_MARKDOWN_SYNTAX_NAME);
    }

    /// The pool's whole reason to exist: a real ` ```starlark constraint `
    /// fence (SPEC.md's "Constraint fences") should get real syntax
    /// highlighting, not plain text — checked against an actual snippet
    /// from `examples/constraints.canvas.md`, not a toy one-liner.
    #[test]
    fn build_syntax_set_always_includes_the_bundled_starlark_grammar() {
        let root = scratch_dir(); // never created on disk — no user grammars involved
        let ss = build_syntax_set(&root);
        let syntax = ss
            .find_syntax_by_name("Starlark")
            .expect("meshfox's own bundled Starlark grammar should always be present");

        let ts = syntect::highlighting::ThemeSet::load_defaults();
        let theme = ts.themes.values().next().expect("syntect ships at least one theme");
        let mut hl = syntect::easy::HighlightLines::new(syntax, theme);
        let ranges = hl
            .highlight_line("for n in self.descendants():\n", &ss)
            .unwrap();
        let plain_default = theme.settings.foreground.unwrap();
        let for_color = ranges
            .iter()
            .find(|(_, text)| text.contains("for"))
            .expect("expected a highlighted range containing \"for\"")
            .0
            .foreground;
        assert_ne!(for_color, plain_default, "the `for` keyword should not render as plain text");
    }

    /// The original motivating case (TODO.canvas.md's "Единая база языков
    /// подсветки") — Elixir wasn't bundled by either engine at all, so a
    /// ` ```elixir ` fence just rendered as plain text. Confirms it's
    /// actually part of the *default* `SyntaxSet` now, not only loadable
    /// via `.meshfox/syntax/`.
    #[test]
    fn build_syntax_set_always_includes_the_bundled_elixir_grammar() {
        let root = scratch_dir(); // never created on disk — no user grammars involved
        let ss = build_syntax_set(&root);
        let syntax = ss
            .find_syntax_by_name("Elixir")
            .expect("meshfox's own bundled Elixir grammar should always be present");

        let ts = syntect::highlighting::ThemeSet::load_defaults();
        let theme = ts.themes.values().next().expect("syntect ships at least one theme");
        let mut hl = syntect::easy::HighlightLines::new(syntax, theme);
        let ranges = hl.highlight_line("defmodule Foo do\n", &ss).unwrap();
        let plain_default = theme.settings.foreground.unwrap();
        let keyword_color = ranges
            .iter()
            .find(|(_, text)| text.contains("defmodule"))
            .expect("expected a highlighted range containing \"defmodule\"")
            .0
            .foreground;
        assert_ne!(keyword_color, plain_default, "the `defmodule` keyword should not render as plain text");
    }

    /// TODO.canvas.md's 2026-08-24 audit + the follow-up batch it led to —
    /// every one of `BUNDLED_POOL_ADDITIONS` should actually be findable by
    /// name in the built `SyntaxSet`, not just present in the source tree.
    #[test]
    fn build_syntax_set_always_includes_every_bundled_pool_addition() {
        let root = scratch_dir(); // never created on disk — no user grammars involved
        let ss = build_syntax_set(&root);
        for (label, _) in BUNDLED_POOL_ADDITIONS {
            ss.find_syntax_by_name(label)
                .unwrap_or_else(|| panic!("bundled pool addition {label:?} should always be present"));
        }
    }

    /// Spot-check for the five grammars that needed a `\G`-anchor patch
    /// (`grammars/tui/`) before they'd load at all — real tokenization, not
    /// just "is present", the same way `build_syntax_set_always_includes_
    /// the_bundled_starlark_grammar` checks Starlark's own patched rule.
    #[test]
    fn the_g_anchor_patched_grammars_still_highlight_real_code() {
        let root = scratch_dir(); // never created on disk — no user grammars involved
        let ss = build_syntax_set(&root);
        let ts = syntect::highlighting::ThemeSet::load_defaults();
        let theme = ts.themes.values().next().expect("syntect ships at least one theme");
        let plain_default = theme.settings.foreground.unwrap();

        let cases: &[(&str, &str, &str)] = &[
            ("ini", "[section]\nkey = value\n# a comment\n", "comment"),
            ("powershell", "function Foo {\n    $x = 1\n}\n", "function"),
            ("typescript", "function greet(name: string): void {}\n", "function"),
            ("tsx", "const el = <div className=\"a\">hi</div>;\n", "className"),
            ("jsx", "const el = <div className=\"a\">hi</div>;\n", "className"),
        ];
        for (name, snippet, needle) in cases {
            let syntax = ss.find_syntax_by_name(name).unwrap_or_else(|| panic!("{name} should be present"));
            let mut hl = syntect::easy::HighlightLines::new(syntax, theme);
            let ranges = hl.highlight_line(snippet, &ss).unwrap();
            let color = ranges
                .iter()
                .find(|(_, text)| text.contains(needle))
                .unwrap_or_else(|| panic!("{name}: expected a range containing {needle:?}"))
                .0
                .foreground;
            assert_ne!(color, plain_default, "{name}: {needle:?} should not render as plain text");
        }
    }

    #[test]
    fn build_syntax_set_loads_a_grammar_that_lives_only_in_a_tui_override_directory() {
        let root = scratch_dir();
        let tui_dir = root.join(".meshfox").join("syntax").join("tui");
        std::fs::create_dir_all(&tui_dir).unwrap();
        std::fs::write(tui_dir.join("test-lang.tmLanguage.json"), TINY_TMLANGUAGE).unwrap();

        let ss = build_syntax_set(&root);
        assert!(
            ss.find_syntax_by_name("Meshfox Test Lang").is_some(),
            "a grammar that only exists under syntax/tui/ (no plain counterpart) should still load"
        );

        std::fs::remove_dir_all(&root).ok();
    }

    /// The actual point of `tui/`: a same-named grammar that also exists in
    /// the plain (shared, web-visible) directory gets *overridden* for the
    /// TUI specifically, not just "also loaded" — verified by tokenizing
    /// real text, not just checking the definition is present (two
    /// same-scope definitions can both technically be in the `SyntaxSet`;
    /// what matters is which one actually wins the lookup real rendering
    /// uses).
    #[test]
    fn a_tui_override_grammar_wins_over_the_plain_shared_one_with_the_same_name() {
        const PLAIN: &str = r#"{
            "scopeName": "source.meshfox-override-test",
            "name": "Meshfox Override Test",
            "patterns": [
                { "match": "\\bplain-only-keyword\\b", "name": "keyword.control.meshfox-override-test" }
            ]
        }"#;
        const OVERRIDE: &str = r#"{
            "scopeName": "source.meshfox-override-test",
            "name": "Meshfox Override Test",
            "patterns": [
                { "match": "\\boverride-only-keyword\\b", "name": "keyword.control.meshfox-override-test" }
            ]
        }"#;

        let root = scratch_dir();
        let syntax_dir = root.join(".meshfox").join("syntax");
        let tui_dir = syntax_dir.join("tui");
        std::fs::create_dir_all(&tui_dir).unwrap();
        std::fs::write(syntax_dir.join("override-test.tmLanguage.json"), PLAIN).unwrap();
        std::fs::write(tui_dir.join("override-test.tmLanguage.json"), OVERRIDE).unwrap();

        let ss = build_syntax_set(&root);
        let syntax = ss
            .find_syntax_by_name("Meshfox Override Test")
            .expect("the (overridden) grammar should still be findable by name");

        let ts = syntect::highlighting::ThemeSet::load_defaults();
        let theme = &ts.themes["base16-ocean.dark"];
        let mut hl = syntect::easy::HighlightLines::new(syntax, theme);

        // The override's own keyword must be highlighted (a distinct style
        // from plain text)...
        let override_ranges = hl.highlight_line("override-only-keyword\n", &ss).unwrap();
        assert!(
            override_ranges.iter().any(|(style, text)| !text.trim().is_empty() && style.foreground != theme.settings.foreground.unwrap()),
            "the tui/ override's own keyword should be highlighted — its patterns should be the ones actually active"
        );

        // ...while the plain shared file's own keyword (never loaded once
        // overridden) must not be — it just reads as plain, unstyled text.
        let mut hl2 = syntect::easy::HighlightLines::new(syntax, theme);
        let plain_ranges = hl2.highlight_line("plain-only-keyword\n", &ss).unwrap();
        assert!(
            plain_ranges.iter().all(|(style, _)| style.foreground == theme.settings.foreground.unwrap()),
            "the plain shared file's own keyword must NOT be highlighted — the tui/ override replaced it entirely, not just added to it"
        );

        std::fs::remove_dir_all(&root).ok();
    }
}

#[cfg(test)]
mod meshfox_grammar_tests {
    use super::*;

    /// The whole point of `crates/cli/src/grammars/meshfox.tmLanguage.json`
    /// existing at all: a real `<!-- meshfox:... -->` marker comment, run
    /// through the actual production `build_syntax_set` + `with_meshfox_
    /// scope_colors`, must come out with the keyword/attribute-name/
    /// attribute-value each in their own distinct color — not a single flat
    /// "it's a comment" color the base `text.html.markdown` grammar alone
    /// would give it. Caught two real bugs during development this way,
    /// neither of which a "does it load" test alone would have: (1) the
    /// grammar's own `#meshfox-comment` pattern had to be listed *before*
    /// `text.html.markdown` in `patterns` — array order breaks a same-
    /// position tie, and markdown's own HTML-comment rule was winning it
    /// outright, so the marker rendered as one plain comment with none of
    /// meshfox's own sub-scopes at all; (2) meshfox's own scope names need
    /// explicit theme rules (`with_meshfox_scope_colors`) — a bundled
    /// `syntect` theme has no idea what `keyword.other.meshfox` even is, so
    /// without this the (correctly-scoped) tokens still all rendered in one
    /// plain default color.
    #[test]
    fn a_meshfox_marker_comment_gets_distinct_colors_for_keyword_attr_name_and_value() {
        let root = std::env::temp_dir().join("meshfox-grammar-test-never-created");
        let ss = build_syntax_set(&root);
        let syntax = ss
            .find_syntax_by_name(MESHFOX_MARKDOWN_SYNTAX_NAME)
            .expect("the bundled meshfox-markdown grammar should always be present");

        let ts = syntect::highlighting::ThemeSet::load_defaults();
        let base_theme = ts.themes.values().next().expect("syntect ships at least one theme").clone();
        let theme = with_meshfox_scope_colors(base_theme);

        let mut hl = syntect::easy::HighlightLines::new(syntax, &theme);
        let ranges = hl
            .highlight_line("<!-- meshfox:node id=\"root\" -->\n", &ss)
            .unwrap();

        let color_of = |needle: &str| {
            ranges
                .iter()
                .find(|(_, text)| text.contains(needle))
                .unwrap_or_else(|| panic!("expected a highlighted range containing {needle:?}"))
                .0
                .foreground
        };

        let keyword_color = color_of("meshfox:node");
        let attr_name_color = color_of("id");
        let attr_value_color = color_of("\"root\"");
        let plain_default = theme.settings.foreground.unwrap();

        assert_ne!(keyword_color, plain_default, "the meshfox:node keyword should not render as plain text");
        assert_ne!(attr_name_color, plain_default, "the id attribute name should not render as plain text");
        assert_ne!(attr_value_color, plain_default, "the \"root\" attribute value should not render as plain text");
        assert_ne!(keyword_color, attr_name_color, "keyword and attribute-name should be distinctly colored");
        assert_ne!(attr_name_color, attr_value_color, "attribute-name and attribute-value should be distinctly colored");
    }
}
