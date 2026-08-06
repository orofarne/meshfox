<!-- meshfox:canvas -->
# License & Third-Party Notices
<!-- meshfox:node id="root" -->

meshfox itself is MIT-licensed (see [LICENSE](./LICENSE), also inlined below). This document tracks every third-party dependency pulled into the published binary/bundle — backend (Rust crates, `Cargo.toml`) and UI (npm packages, `web/package.json`) — and records the license-compatibility check against MIT.

Only **direct** dependencies are listed by name below; the full transitive tree actually reachable from a *shipped* binary (321 Rust crates — normal-dependency edges only, dev/build-only trees excluded, since those never compile into what's published; 187 npm production packages) was checked programmatically and is summarized in "License compatibility" — nothing in it requires anything beyond attribution.

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
| libc | MIT OR Apache-2.0 |
| mime_guess | MIT |
| open | MIT |
| portable-pty | MIT |
| pulldown-cmark | MIT |
| rpassword | Apache-2.0 |
| rust-embed | MIT |
| serde | MIT OR Apache-2.0 |
| serde_json | MIT OR Apache-2.0 |
| starlark | Apache-2.0 |
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

