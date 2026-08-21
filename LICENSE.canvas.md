<!-- meshfox:canvas -->
# License & Third-Party Notices
<!-- meshfox:node id="root" -->

meshfox itself is MIT-licensed (see [LICENSE](./LICENSE), also inlined below). This document tracks every third-party dependency pulled into the published binary/bundle — backend (Rust crates, `Cargo.toml`) and UI (npm packages, `web/package.json`) — and records the license-compatibility check against MIT.

Only **direct** dependencies are listed by name below; the full transitive tree actually reachable from a *shipped* binary (634 Rust crates on macOS — normal-dependency edges only, dev/build-only trees excluded, since those never compile into what's published; 187 npm production packages) was checked programmatically and is summarized in "License compatibility" — nothing in it requires anything beyond attribution, with weak-copyleft exceptions noted below (`option-ext`; `cssparser`/`selectors`, both via `scraper`). The Rust-side count jumped from 321 once `meshfox tui` (see "Usage") landed — syntax highlighting (`syntect`) and terminal image rendering (`ratatui-image`, `image`) both pull real transitive weight; `image`'s own default codec set was trimmed to just `png`/`jpeg`/`gif`/`webp`/`bmp` to keep that from growing further than it has to. It grew by another 27 once `meshfox check-updates` (see "Usage") landed — `self_update`'s GitHub-release polling, tarball extraction, and TLS (`rustls`, built with the `ureq` HTTP backend rather than `reqwest`, to keep that addition as small as it can be) each pull their own small tree. It grew by another 62 once `meshfox pdf` (see "Usage") landed — `headless_chrome` (drives a real Chrome/Chromium to print the HTML `static` export to PDF, downloading a pinned Chromium build via its own `fetch` feature when no system browser is found) and `lopdf` (merges the diagram-overview and document-flow pages into one PDF) each pull their own tree. One of `headless_chrome`'s transitive deps, `option-ext` (via `directories`/`dirs-sys`, used to locate the Chromium download cache), is MPL-2.0 rather than MIT — weak, file-level copyleft that only obligates keeping *that crate's own* source available/modifiable if redistributed in modified form; it doesn't extend to the rest of this binary (same as `cssparser`/`selectors` below, once `scraper` reached it for real). `meshfox pdf` also gave both its own printed pages and `meshfox static`'s own site-template the same font the web UI already uses (`crates/cli/src/pdf/templates/`'s CSS, `site-template/style.css`). `pdf`'s own copy isn't a second embedded copy at all: `web/dist` (Vite's build output) is already embedded into this same binary via `rust-embed` for the web UI's own sake (`crates/server`'s `WebAssets`), so `meshfox_server::find_web_asset` just pulls the same `@fontsource/fira-code` bytes back out of that already-embedded bundle at PDF-generation time (weights 400/700, `latin`+`cyrillic` subsets — `pdf` runs against arbitrary canvases, so it keeps both this project's own content actually mixes) rather than shipping a redundant `include_bytes!`-embedded copy of its own; if `web/` hasn't been built into this particular binary, the lookup just comes back empty and that `@font-face` silently falls through to the page's own CSS fallback stack instead of failing the export. `site-template/fonts/` is a separate, genuine copy (loose files, `latin` only, weights 400/500/600/700 — that template is this repo's own working example, used to publish README.md itself, all-English) since a static-site export has no already-compiled binary to borrow bytes from — copied straight from the same npm package's own build, own `fonts/LICENSE` alongside it, same OFL-1.1 font-file-level attribution requirement as the npm entry below. A template built for other content can add back whatever subsets it needs (`latin-ext`/`cyrillic-ext` — Central/Eastern-European accented Latin, historic/extended Cyrillic — neither this repo's own template nor `pdf` needs). It grew by another 4 once constraint fences gained read access to a `file`-type node's own already-declared target (`.content()`/`.json()`/`.yaml()`/`.toml()`/`.csv()` — see "Constraint fences") — JSON and TOML parsing already leaned on `serde_json`/`toml` (both already direct deps for other reasons), so only `.yaml()`/`.csv()` needed new ones: `csv` (plus its own small `csv-core`) and `serde_norway` (plus its own `unsafe-libyaml-norway`) — the latter a maintained fork of `serde_yaml`, which upstream itself now points people away from. It grew by another 19 (on macOS — this pulls a different, platform-specific clipboard backend on Linux/Windows) once `meshfox tui` gained its own fullscreen source editor (`e`, see "Usage") — `edtui` (the editor widget itself: buffer, cursor, vim-style input, and — via its own `syntax-highlighting` feature — a second, independent `syntect` instance from the one `markdown.rs`'s read-only code-fence highlighting already uses) pulls in `arboard` for system-clipboard copy/paste (`objc2`/`objc2-app-kit`/`objc2-core-foundation`/`objc2-core-graphics`/`objc2-encode`/`objc2-foundation` on macOS), plus `onig`/`onig_sys` — an alternate regex engine `syntect`'s own default features pull in alongside the `regex-fancy` one this binary already asks for elsewhere, since Cargo unifies a shared dependency's features across the whole build rather than keeping two differently-configured copies. `onig_sys` vendors the Oniguruma C library itself, BSD-2-Clause; everything else in this batch (`edtui`, `edtui-jagged`, `arboard`, the `objc2*` family, `plist`, `quick-xml`, `tiff`) is MIT or a permissive MIT/Apache-2.0/Zlib choice. `crossterm` itself was bumped `0.28` → `0.29` in the same change, purely to match the version `edtui` (and `ratatui` 0.30's own `ratatui-crossterm` backend) already depends on — Cargo can't unify two different `0.x` majors of the same crate the way it does minor/feature differences within one, so without this bump the binary would've ended up with two distinct, mutually-incompatible `crossterm::event::KeyEvent` types instead of one. `meshfox_server` gained two more direct deps once `link` nodes' `preview="true"` (see SPEC.md's "Node types") landed — `reqwest` (`default-features = false`, just the `rustls` feature, matching this binary's existing TLS-backend preference over `native-tls`/OpenSSL) for the SSRF-hardened OpenGraph fetch itself, and `scraper` (`Selector::parse`/`.select()` against the fetched HTML, `crates/server/src/link_preview.rs`) for parsing its `<meta property="og:...">` tags — this is also where `scraper`'s own transitive `cssparser`/`selectors` (both MPL-2.0, weak/file-level copyleft, same terms as `option-ext` above) first actually reached the published binary, despite `crates/cli`'s own separate *dev*-only copy of `scraper` (used by its own tests) predating it. Both `reqwest`/`scraper` were already reachable elsewhere in the workspace's own dependency graph before this — `reqwest` as a transitive dep of `self_update` (see the `check-updates` entry above), `scraper` as that `crates/cli`-dev-only copy — so promoting them to real, direct `meshfox_server` deps added meaningfully less new transitive weight than a from-scratch addition of either would have. It grew by one more once runnable fences gained their own `interpreter="..."` attribute (generalized from the `file`-node attribute of the same name, see SPEC.md's "Runnable code fences") — `shlex` splits that shebang-style command+flags string into a program name and argument list; it's a tiny, dependency-free crate (no transitive tree of its own). `node find` (see "Usage") promoted `crates/cli`'s own `scraper` copy from dev-only to a second real, direct use — CSS-selector matching against a synthetic HTML skeleton of the canvas tree (`crates/cli/src/main.rs`'s `find_node_ids`), the same engine `link_preview.rs` already uses — adding no new transitive weight at all, `cssparser`/`selectors` having already reached the binary via `meshfox_server` as described just above.

## MIT License
<!-- meshfox:node id="license-file" type="file" display="code" -->

[LICENSE](./LICENSE)

## Backend dependencies (Rust / crates.io)
<!-- meshfox:node id="backend-deps" -->

Direct dependencies across `crates/core`, `crates/server`, `crates/cli` (`cargo metadata`, deduplicated; dev-only deps omitted since they never ship — just `crates/server`'s `futures-util`/`tokio-tungstenite` now; `crates/cli`'s own `scraper` (ISC) copy is a real, shipped dependency too as of `node find`, not just a dev one — see the narrative above for its own transitive `cssparser`/`selectors`, both MPL-2.0). All are permissive and MIT-compatible, aside from the weak-copyleft exceptions already called out above.

| Crate | License |
|---|---|
| anyhow | MIT OR Apache-2.0 |
| async-stream | MIT |
| axum | MIT |
| base64 | MIT OR Apache-2.0 |
| clap | MIT OR Apache-2.0 |
| clap_complete | MIT OR Apache-2.0 |
| crossterm | MIT |
| csv | Unlicense OR MIT |
| edtui | MIT |
| headless_chrome | MIT |
| image | MIT OR Apache-2.0 |
| libc | MIT OR Apache-2.0 |
| lopdf | MIT |
| mime_guess | MIT |
| open | MIT |
| portable-pty | MIT |
| pulldown-cmark | MIT |
| ratatui | MIT |
| ratatui-image | MIT |
| reqwest | MIT OR Apache-2.0 |
| rpassword | Apache-2.0 |
| rust-embed | MIT |
| scraper | ISC |
| self_update | MIT |
| serde | MIT OR Apache-2.0 |
| serde_json | MIT OR Apache-2.0 |
| serde_norway | MIT OR Apache-2.0 |
| shlex | MIT OR Apache-2.0 |
| starlark | Apache-2.0 |
| syntect | MIT |
| tera | MIT |
| thiserror | MIT OR Apache-2.0 |
| tokio | MIT |
| toml | MIT OR Apache-2.0 |
| tower-http | MIT |
| uuid | Apache-2.0 OR MIT |

Cross-checked against the three manifests below (`core`/`server`/`cli` Cargo.toml, read via `.toml()` — see `examples/constraints.canvas.md`): every crate in each one's own `[dependencies]` (skipping workspace-internal path deps like `meshfox-core`) must have a row in the table above.

```starlark constraint name="every-direct-dep-is-documented"
def _table_names(text):
    names = []
    for raw_line in text.split("\n"):
        line = raw_line.strip()
        if not line.startswith("|"):
            continue
        cells = line.split("|")
        if len(cells) < 3:
            continue
        name = cells[1].strip()
        if name == "" or name == "Crate" or name.startswith("-"):
            continue
        names.append(name)
    return names

def _real_deps(dep_table):
    names = []
    for name in dep_table:
        v = dep_table[name]
        if type(v) == "dict" and "path" in v:
            continue  # workspace-internal crate, not third-party
        names.append(name)
    return names

manifests = [c for c in self.children() if c.type == "file"]
documented = _table_names(self.text)
actual = {}
for m in manifests:
    data = m.toml()
    if data == None or "dependencies" not in data:
        fail(m.id + ": could not read/parse Cargo.toml")
    else:
        for name in _real_deps(data["dependencies"]):
            actual[name] = True

for name in actual:
    if name not in documented:
        fail(name + " is a direct dependency (cargo) but has no entry in the table above")
```

### core/Cargo.toml
<!-- meshfox:node id="core-cargo-toml" type="file" -->

[core/Cargo.toml](crates/core/Cargo.toml)

### server/Cargo.toml
<!-- meshfox:node id="server-cargo-toml" type="file" -->

[server/Cargo.toml](crates/server/Cargo.toml)

### cli/Cargo.toml
<!-- meshfox:node id="cli-cargo-toml" type="file" -->

[cli/Cargo.toml](crates/cli/Cargo.toml)

## UI dependencies (npm)
<!-- meshfox:node id="ui-deps" -->

Direct `dependencies` from `web/package.json` — these ship inside the built browser bundle. All are permissive and MIT-compatible; `@fontsource/fira-code` (the self-hosted Fira Code font files) is SIL OFL-1.1 rather than MIT, which only requires the font itself keep its license/copyright notice — it doesn't reach the rest of the bundle:

| Package | License |
|---|---|
| @codemirror/lang-markdown | MIT |
| @codemirror/language | MIT |
| @codemirror/language-data | MIT |
| @codemirror/state | MIT |
| @fontsource/fira-code | OFL-1.1 |
| @uiw/react-codemirror | MIT |
| @xterm/addon-fit | MIT |
| @xterm/xterm | MIT |
| @xyflow/react | MIT |
| anser | MIT |
| react | MIT |
| react-dom | MIT |
| react-markdown | MIT |
| remark-gfm | MIT |

Direct `devDependencies` — build/test tooling only, never shipped:

| Package | License |
|---|---|
| @playwright/test | Apache-2.0 |
| @types/node | MIT |
| @types/react | MIT |
| @types/react-dom | MIT |
| @vitejs/plugin-react | MIT |
| typescript | Apache-2.0 |
| vite | MIT |

Cross-checked against `package.json` below (`web/package.json`, read via `.json()`): every key under both `dependencies` and `devDependencies` must have a row in one of the two tables above.

```starlark constraint name="every-direct-dep-is-documented"
def _table_names(text):
    names = []
    for raw_line in text.split("\n"):
        line = raw_line.strip()
        if not line.startswith("|"):
            continue
        cells = line.split("|")
        if len(cells) < 3:
            continue
        name = cells[1].strip()
        if name == "" or name == "Package" or name.startswith("-"):
            continue
        names.append(name)
    return names

pkg_nodes = [c for c in self.children() if c.type == "file"]
pkg = pkg_nodes[0].json() if len(pkg_nodes) == 1 else None
if pkg == None:
    fail("package.json: could not find/read/parse web/package.json")
else:
    documented = _table_names(self.text)
    actual = list(pkg.get("dependencies", {}).keys()) + list(pkg.get("devDependencies", {}).keys())
    for name in actual:
        if name not in documented:
            fail(name + " is a direct dependency (npm) but has no entry in either table above")
```

### package.json
<!-- meshfox:node id="package-json" type="file" -->

[package.json](web/package.json)

