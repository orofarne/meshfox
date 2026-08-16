<!-- meshfox:canvas -->
# License & Third-Party Notices
<!-- meshfox:node id="root" -->

meshfox itself is MIT-licensed (see [LICENSE](./LICENSE), also inlined below). This document tracks every third-party dependency pulled into the published binary/bundle — backend (Rust crates, `Cargo.toml`) and UI (npm packages, `web/package.json`) — and records the license-compatibility check against MIT.

Only **direct** dependencies are listed by name below; the full transitive tree actually reachable from a *shipped* binary (611 Rust crates — normal-dependency edges only, dev/build-only trees excluded, since those never compile into what's published; 187 npm production packages) was checked programmatically and is summarized in "License compatibility" — nothing in it requires anything beyond attribution, with one weak-copyleft exception noted below. The Rust-side count jumped from 321 once `meshfox tui` (see "Usage") landed — syntax highlighting (`syntect`) and terminal image rendering (`ratatui-image`, `image`) both pull real transitive weight; `image`'s own default codec set was trimmed to just `png`/`jpeg`/`gif`/`webp`/`bmp` to keep that from growing further than it has to. It grew by another 27 once `meshfox check-updates` (see "Usage") landed — `self_update`'s GitHub-release polling, tarball extraction, and TLS (`rustls`, built with the `ureq` HTTP backend rather than `reqwest`, to keep that addition as small as it can be) each pull their own small tree. It grew by another 62 once `meshfox pdf` (see "Usage") landed — `headless_chrome` (drives a real Chrome/Chromium to print the HTML `static` export to PDF, downloading a pinned Chromium build via its own `fetch` feature when no system browser is found) and `lopdf` (merges the diagram-overview and document-flow pages into one PDF) each pull their own tree. One of `headless_chrome`'s transitive deps, `option-ext` (via `directories`/`dirs-sys`, used to locate the Chromium download cache), is MPL-2.0 rather than MIT — weak, file-level copyleft that only obligates keeping *that crate's own* source available/modifiable if redistributed in modified form; it doesn't extend to the rest of this binary, unlike the MPL-2.0 crates mentioned above that never reach the binary at all. `meshfox pdf` also gave both its own printed pages and `meshfox static`'s own site-template the same font the web UI already uses (`crates/cli/src/pdf/templates/`'s CSS, `site-template/style.css`). `pdf`'s own copy isn't a second embedded copy at all: `web/dist` (Vite's build output) is already embedded into this same binary via `rust-embed` for the web UI's own sake (`crates/server`'s `WebAssets`), so `meshfox_server::find_web_asset` just pulls the same `@fontsource/fira-code` bytes back out of that already-embedded bundle at PDF-generation time (weights 400/700, `latin`+`cyrillic` subsets — `pdf` runs against arbitrary canvases, so it keeps both this project's own content actually mixes) rather than shipping a redundant `include_bytes!`-embedded copy of its own; if `web/` hasn't been built into this particular binary, the lookup just comes back empty and that `@font-face` silently falls through to the page's own CSS fallback stack instead of failing the export. `site-template/fonts/` is a separate, genuine copy (loose files, `latin` only, weights 400/500/600/700 — that template is this repo's own working example, used to publish README.md itself, all-English) since a static-site export has no already-compiled binary to borrow bytes from — copied straight from the same npm package's own build, own `fonts/LICENSE` alongside it, same OFL-1.1 font-file-level attribution requirement as the npm entry below. A template built for other content can add back whatever subsets it needs (`latin-ext`/`cyrillic-ext` — Central/Eastern-European accented Latin, historic/extended Cyrillic — neither this repo's own template nor `pdf` needs).

## MIT License
<!-- meshfox:node id="license-file" type="file" display="code" -->

[LICENSE](./LICENSE)

## Backend dependencies (Rust / crates.io)
<!-- meshfox:node id="backend-deps" -->

Direct dependencies across `crates/core`, `crates/server`, `crates/cli` (`cargo metadata`, deduplicated; dev-only deps omitted since they never ship — `crates/server`'s `futures-util`/`tokio-tungstenite`, `crates/cli`'s `scraper` (ISC) and its own transitive tree, which includes a few MPL-2.0-licensed crates (`cssparser`, `selectors`, ...) that never reach the published binary either way). All are permissive and MIT-compatible.

| Crate | License |
|---|---|
| anyhow | MIT OR Apache-2.0 |
| async-stream | MIT |
| axum | MIT |
| clap | MIT OR Apache-2.0 |
| clap_complete | MIT OR Apache-2.0 |
| crossterm | MIT |
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
| rpassword | Apache-2.0 |
| rust-embed | MIT |
| self_update | MIT |
| serde | MIT OR Apache-2.0 |
| serde_json | MIT OR Apache-2.0 |
| starlark | Apache-2.0 |
| syntect | MIT |
| tera | MIT |
| thiserror | MIT OR Apache-2.0 |
| tokio | MIT |
| toml | MIT OR Apache-2.0 |
| tower-http | MIT |
| uuid | Apache-2.0 OR MIT |

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

