<!-- meshfox:canvas -->
# Development
<!-- meshfox:node id="development" -->

Part of the [meshfox documentation](./README.md).

<!-- meshfox:var name="INSTALL_PATH" prompt="Install prefix?" default="/usr/local/bin" -->

Requires Rust (stable, via [rustup](https://rustup.rs/)) and Node.js for the web UI.

`crates/server` embeds `web/dist` at compile time (via `rust-embed`), so **build the frontend before building anything that depends on it** — a fresh clone has an empty `web/dist` (just a tracked `.gitkeep`, see `.gitignore`) and `meshfox view` will happily start and serve the API, but its UI route will just say the assets weren't built rather than show anything.

From a fresh clone, before `meshfox` is on your `PATH`, run any subcommand as `cargo run -p meshfox-cli -- <subcommand> <canvas>` (for example `validate examples/hello.canvas.md`). Each part of the build and test routine is a runnable block in its own node below; this button runs all of them (the "Full check" block below), same as `meshfox run development full-check`:

```button name="full-check" deps="full-check/full-check"
✅ Full check
```

Working on the frontend itself: the embedded copy is a snapshot from your last `npm run build`, so for live-reloading UI development run `cd web && npm run dev` (on :5173) in a second terminal alongside `meshfox view` (on :4590) — Vite proxies `/api` to :4590, same as before.

## Unit tests
<!-- meshfox:node id="unit-tests" -->

`cargo test --workspace` runs the Rust workspace's own unit/integration tests — `crates/core`'s parsing (`mdcanvas`), tree derivation, auto-layout, fence-scanning, dependency-resolution, and variable-handling logic is the bulk of the coverage, each in its own `#[cfg(test)]` module next to the code it exercises, plus a handful of higher-level tests in `crates/server`/`crates/cli` (e.g. the `tty` websocket path). Pure Rust — no Node.js, no built frontend, no browser needed, unlike the end-to-end suite below. `name=`d so it's runnable like any other block here, deliberately without `cache`, same reasoning as "End-to-end tests"/"Release build": the log is per-run noise, not something worth freezing into this file. Flagged `default` since its block name (`run`) doesn't match its node id (`unit-tests`):

```sh name="run" default
export CARGO_TERM_COLOR=always
cargo test --workspace -- --color=always
```

`web/scripts/*.test.mjs` are the frontend's own unit tests: plain `node --test` files over the pure-logic modules under `web/src/` (the fence/argument-signature parser, edge routing, the meshfox Markdown grammar, SVG theming, the table view's filter language and virtual-scroll arithmetic) — no browser, no server, a second or two in total. Nothing runs them implicitly (the release workflow only builds the web app, and `test:e2e`'s `pretest` hook only builds), so run them yourself, alongside the type-check under "Linting" below; the end-to-end suite that drives the real UI is the next section:

```sh name="web"
export FORCE_COLOR=1
cd web && npm run test:unit
```

`meshfox-model` is the document model with no disk and no OS under it, and `meshfox-wasm` is its WebAssembly binding (see `web/wasm-editor/README.md`). `wasm-prep` adds the `wasm32-unknown-unknown` compile target and the `wasm-bindgen` CLI at the exact version `Cargo.lock` pins (the CLI and the library must match) — both idempotent, so re-running once done costs nothing, the same arrangement as `e2e-prep` below; the CLI is a one-time `cargo install`, a few minutes. `wasm` then checks the model still builds for `wasm32-unknown-unknown` (nothing but pure Rust may creep back into it) and runs the binding's Node tests against the real `.wasm` (`scripts/test-wasm.sh`):

```sh name="wasm-prep"
set -e
rustup target add wasm32-unknown-unknown
want=$(grep -A1 '^name = "wasm-bindgen"$' Cargo.lock | sed -n 's/^version = "\(.*\)"/\1/p')
if [ "$(wasm-bindgen --version 2>/dev/null | awk '{print $2}')" != "$want" ]; then
  cargo install wasm-bindgen-cli --version "$want" --locked
fi
```

```sh name="wasm" deps="wasm-prep"
set -e
export CARGO_TERM_COLOR=always
cargo check -p meshfox-model --target wasm32-unknown-unknown
scripts/test-wasm.sh
```

## End-to-end tests
<!-- meshfox:node id="e2e-tests" -->

`web/e2e/` is a [Playwright](https://playwright.dev) suite that drives the real UI in a real browser against a real `meshfox view` — not a mocked frontend — because the bugs this suite exists to catch (a dependency badge clipped by `overflow: hidden`, a highlight's `box-shadow` eaten by that same `overflow: hidden`, "ok" on a node's settings modal resending a field nobody touched) were only visible in the genuinely rendered, genuinely laid-out canvas; a component-level test wouldn't have seen any of them. It runs against four fixture canvases (`web/e2e/fixtures/*.canvas.md`, one each for dependency-chain UI, scroll/pan interaction, text selection, and node-settings) — deterministic (no `date`/timestamps) and separate from `examples/hello.canvas.md`, so test stability never depends on the documentation example's own content. Most of the suite runs the UI in its default read-only mode (never clicks "Edit"), so nothing there ever writes back into a fixture file; `settings.spec.ts` is the one exception — it clicks "Edit" to reach the settings modal at all, but only ever clicks "ok" without changing a field, and asserts the raw file comes back byte-for-byte unchanged every time (see that file's own doc comment for the two regressions this caught). Each suite runs against both Chromium and Firefox (`chrome-*`/`firefox-*` projects in `playwright.config.ts`); a small, explicitly-commented handful of assertions are skipped on Firefox where they hit confirmed Gecko-specific limitations (nested-scrollframe wheel-event chaining; drag-selection anchoring under a CSS `transform` ancestor) rather than anything wrong in this app's own code.

`e2e-prep` installs `web/`'s npm dependencies and Playwright's own Chromium and Firefox copies (separate from any browser already on your system) — both idempotent, so re-running once already done costs nothing. `run` depends on it (`deps="e2e-prep"`), so the dependency chain always pulls prep in first — no manual first-time step to remember. Both `name=`d so they're runnable like any other block here, deliberately without `cache`, same reasoning as "Release build" below: their logs are per-run noise (install/test timings) rather than something worth freezing into this file. `run` is also flagged `default` — the one block per node `meshfox run <path>` can address without a trailing block name — so `meshfox run development e2e-tests` runs it (and its `e2e-prep` dependency) directly, without needing to spell out `... e2e-tests run`:

```sh name="e2e-prep"
cd web
npm install
npx playwright install chromium firefox
```

```sh name="run" deps="e2e-prep" default
export FORCE_COLOR=1
cd web
npm run test:e2e
```

`display="table"` has its own opt-in suite, `cd web && npm run test:table` (`web/table.playwright.config.ts`, `web/e2e/table.spec.ts`): one real worker over a generated 400,000-row CSV, driving sorting, filters, search, scrolling to the very end through the capped scroll track, cell selection and the expanded window. It needs the `duckdb` CLI on the machine (the spec skips itself when there isn't one) and, like `test:arguments`, isn't part of the main `playwright.config.ts` run.

`meshfox serve --http` has its own, much smaller suite, `cd web && npm run test:e2e:gateway` (`web/playwright.gateway.config.ts`, `web/e2e-gateway/`): two real gateways (open and token-protected) and a real browser, covering the page, assets, the lazy Shiki and Monaco chunks, the watch socket, runs, links between canvases, files, the path picker, the sign-in form and the session cookie. `crates/cli/tests/serve_http_cmd.rs` covers the processes: tokens, options, the printed address and a core killed under a live gateway.

For interactive debugging instead of a one-shot run, use `npm run test:e2e:ui` (opens Playwright's UI mode — not runnable here, since it doesn't exit on its own).

`playwright.config.ts`'s `webServer` starts `meshfox view` itself — no server needs to be already running, and no separate build step either: `test:e2e` runs a `pretest:e2e` hook that builds `web/dist`, builds the `meshfox` binary once (`cargo build -p meshfox-cli`) and, on macOS, signs it once (`scripts/run-signed.sh --sign-only`). The ~40 servers then run `target/debug/meshfox view …` directly; going through `cargo run` for each meant a workspace freshness check apiece (~3 s, serialized on the build lock) and a signing check before the first test, even for a single spec. If the binary is missing, older than the Rust sources or only linker-signed (a bare `npx playwright test` after an edit), `e2e/meshfox-bin.ts` falls back to `cargo run`, which rebuilds and signs as needed; set `MESHFOX_E2E_CARGO_RUN=1` to force that. Debug builds of `meshfox-server` read `web/dist` fresh off disk on every request (`rust-embed`'s `debug-embed` feature, which would force compile-time embedding even in a debug build, isn't enabled — see its `Cargo.toml`, and `build.rs` only watches `web/dist` for release builds), so a frontend-only change just needs `npm run build` again, not a Rust rebuild, between test runs.

Computed Starlark views have a focused real-worker/browser suite in `web/e2e/starlark-render.spec.ts`: rendered tables and links, collapsed/expanded source, script errors, and cross-node refresh after tag changes and node deletion. It uses a generated scratch canvas and checks that computed output does not change the source body revision.

```sh name="starlark-render"
export FORCE_COLOR=1
cd web
npm run test:starlark-render
```

The in-browser editor (the real UI over the WebAssembly model, no `meshfox` process; `web/wasm-editor/`, `web/e2e-wasm-editor/`) has its own suite, in Chromium and Firefox: `wasm-editor` builds the wasm package and the static site (`npm run build:wasm`) and then drives it from a file or folder picker. It needs `wasm-prep` (from "Unit tests") and `e2e-prep`, and serves on port 4600, which must be free (or already serving this build):

```sh name="wasm-editor" deps="unit-tests/wasm-prep,e2e-prep"
export FORCE_COLOR=1
cd web
npx playwright install webkit
npm run test:wasm-editor
```

## Editor tests
<!-- meshfox:node id="editor-tests" -->

The editor integrations keep their own build and test instructions in their own canvases — [editors/neovim/README.canvas.md](./editors/neovim/README.canvas.md) (the tree-sitter grammar corpus, the highlight checks, and a headless Neovim pass over every canvas in this repository) and [editors/vscode/README.canvas.md](./editors/vscode/README.canvas.md) (the extension's build, and its Playwright suite against a real, installed VS Code). The blocks below call into them, so `full-check` runs them with everything else.

```meshfox name="neovim-tests"
canvas: editors/neovim/README.canvas.md
run: testing test
```

```meshfox name="vscode-e2e"
canvas: editors/vscode/README.canvas.md
run: e2e-tests e2e-run
```

## TUI end-to-end tests
<!-- meshfox:node id="tui-end-to-end-tests" -->

`crates/cli/tests/tui_e2e/` is a third, separate, deliberately opt-in suite alongside the two above — Rust's own counterpart to them, for the TUI (`crates/cli/src/tui/`). Every existing TUI test (`crates/cli/src/tui/{app,ui,markdown}.rs`) drives `App` directly (`app.on_key(...).await`) or renders one frame via `ratatui::backend::TestBackend` — none of them ever touch the real `crossterm::event::read()`/raw-mode/`EnableMouseCapture` event loop in `crates/cli/src/tui/mod.rs::run`, so a bug specific to that real path (real terminal setup, real mouse escape-sequence parsing, real terminal cleanup on exit) is structurally invisible to them. This suite spawns the real, compiled `meshfox` binary inside a real pty (`portable-pty` — already a real dependency, `crates/server/src/pty_exec.rs` uses it for `tty` blocks) and drives it with real keystrokes and real xterm SGR mouse escape sequences, asserting on the real rendered screen via `vt100`.

Every test in it is `#[ignore]`d — Cargo has no other way to exclude one integration-test target from `cargo test --workspace`'s default run, so this is what keeps it out of that gate (confirmed: `cargo test --workspace` reports this target's tests as `ignored`, not run, adding ~0s). The suite's `mouse_*.rs` tests each mirror one mouse-support checklist item in TODO.canvas.md's "Мышь в панелях TUI (tree/document/output)" — written first, failing on purpose, against a feature that didn't exist yet, with implementing the item and greening its test happening together, the same red-then-green shape as TDD; every item on that checklist is now implemented, so the whole suite is green. `baseline.rs` covers the keyboard-driven flows underneath all of it (start up and render, select a node and run its block, quit and actually exit), so a regression in the real event loop itself doesn't slip through unnoticed.

`table_view.rs` there covers `display="table"` end to end — inline window, full-screen mode, sort/filter/search, wheel and header click — against the real worker and the real `duckdb` CLI (it skips itself without one), plus `display="code"` reading through the worker.

At most six `meshfox tui` sessions are alive at once, whatever `--test-threads` says (`MAX_LIVE_SESSIONS` in `harness.rs`, a semaphore that `TuiSession::spawn` takes a permit from and `Drop` gives back): each one is a real process, a real embedded worker and a pty, and with every test running at once the machine gets loaded enough that tests which look for something on screen right after the first frame fail at random — a different one each run, each green on its own.

```sh name="tui-e2e-run"
export CARGO_TERM_COLOR=always
cargo test --test tui_e2e -- --ignored --color=always
```

`starlark_render.rs` covers computed tables, initially collapsed source, keyboard and mouse source toggles, script diagnostics, and automatic refresh after another CLI client changes tags or deletes a node through the worker. It also checks that viewing never writes output to the canvas. Run this focused PTY scenario with:

```sh name="starlark-render"
export CARGO_TERM_COLOR=always
cargo test -p meshfox-cli --test tui_e2e starlark_render -- --ignored --test-threads=1
```

## Linux tests
<!-- meshfox:node id="linux-tests" -->

Linux-only code (the Secret Service backend behind `secret_store = "keychain"`, systemd socket activation of `meshfox serve`) is never compiled on a macOS host, so it is built and tested in a [Lima](https://lima-vm.io) VM named `mfx` (Debian 13, 4 CPUs, 8 GiB — a 4 GiB VM gets the linker OOM-killed).

`lima-create` ensures the VM exists. The `lima` service uses a one-shot start, waits for guest shell readiness, and checks whether the VM was already running before claiming ownership. `lima-prep` leases it and installs the toolchain, `gnome-keyring` (the real Secret Service the tests talk to) and `secret-tool`. `secrets` and `systemd` keep their `lima-prep!` dependency so provisioning is checked each run.

The lease lasts through the complete root run and its cleanup: the combined `run` keeps the VM up between both test suites. If meshfox started the VM, the last lease stops it on success, failure or cancellation. A VM already running before acquisition is borrowed and left running. Explicit Stop on an active service stops even a borrowed VM. Creation is persistent: stopping the VM does not delete it or its build cache. `reuse` is a probe, not an atomic reservation against another external starter; unexpected statuses and command failures abort instead of claiming ownership.

The repo is mounted into the VM read-only at the same path, so every test builds into `$HOME/target` inside the VM (with debug info off, for the linker's sake) and `Cargo.lock` has to be up to date on the host first.

```sh name="lima-create" always
instances=$(limactl list --format '{{.Name}}')
if ! printf '%s\n' "$instances" | grep -qx mfx; then
  limactl create --name=mfx --cpus=4 --memory=8 --disk=20 --tty=false template:debian-13
fi
```

```yaml name="lima" interpreter="@service" deps="lima-create" autostop
oneshot: true
reuse: |
  status=$(limactl list mfx --format '{{.Status}}') || exit 2
  case "$status" in
    Running|Starting) exit 0 ;;
    Stopped) exit 1 ;;
    *) printf 'Unexpected Lima status: %s\n' "$status" >&2; exit 2 ;;
  esac
start: limactl start mfx --tty=false
ready: limactl shell mfx -- true
check: limactl shell mfx -- true
check_interval: 30s
stop: limactl stop mfx --tty=false
start_timeout: 10m
stop_timeout: 2m
```

```sh name="lima-prep" lease="lima"
limactl shell mfx -- bash -lc '
  set -e
  command -v gnome-keyring-daemon >/dev/null && command -v secret-tool >/dev/null && command -v cc >/dev/null || {
    sudo apt-get update -qq
    sudo DEBIAN_FRONTEND=noninteractive apt-get install -y -qq \
      build-essential pkg-config curl git gnome-keyring libsecret-tools dbus-user-session python3
  }
  [ -x "$HOME/.cargo/bin/cargo" ] || { curl -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal; }
  # The 20 GiB guest disk fills up with stale artifacts across runs; drop the
  # build cache once it passes 8 GiB so a build never dies with ENOSPC.
  used=$(du -sk "$HOME/target" 2>/dev/null | cut -f1); used=${used:-0}
  [ "$used" -lt 8388608 ] || rm -rf "$HOME/target"
'
```

`secrets` runs `crates/core/tests/secret_service_it.rs` and `crates/cli/tests/secret_cmd_linux.rs` against `gnome-keyring` in a throwaway session bus (`scripts/secret-service-it.sh`): the backend's round trips, `meshfox secret set/show/rm/list` through the real CLI, a `run` that reads the stored value, and the loud failure when no Secret Service is reachable. It never touches your own keyring.

```sh name="secrets" deps="lima-prep!"
limactl shell mfx -- bash -lc "
  set -e
  cd '$PWD'
  export CARGO_TARGET_DIR=\$HOME/target PATH=\$HOME/.cargo/bin:\$PATH
  export CARGO_PROFILE_DEV_DEBUG=0 CARGO_TERM_COLOR=always FORCE_COLOR=1 CARGO_INCREMENTAL=0
  scripts/secret-service-it.sh
"
```

`systemd` builds `meshfox`, installs `linux/systemd/meshfox.{socket,service}` for the VM user under a real `systemd --user` and checks socket activation end to end (`scripts/systemd-socket-activation-it.sh`): the service starts on the first connection, `stop` takes the workers down but leaves the socket, a clean `SIGTERM` doesn't restart it, and a worker the service spawned reads a secret from the keyring. The units are removed again afterwards.

```sh name="systemd" deps="lima-prep!"
limactl shell mfx -- bash -lc "
  set -e
  cd '$PWD'
  export CARGO_TARGET_DIR=\$HOME/target PATH=\$HOME/.cargo/bin:\$PATH
  export CARGO_PROFILE_DEV_DEBUG=0 CARGO_TERM_COLOR=always FORCE_COLOR=1 CARGO_INCREMENTAL=0
  cargo build -p meshfox-cli
  scripts/systemd-socket-activation-it.sh
"
```

```sh name="run" deps="secrets!,systemd!" default
echo "Linux tests passed"
```

## Linting
<!-- meshfox:node id="linting" -->

`web/`'s TypeScript type-checking (`tsc --noEmit`) — no ESLint yet, see TODO.canvas.md (not tracked in this repo) for why. `name=`d so it's runnable like any other block here, deliberately without `cache`, same reasoning as "Unit tests":

```sh name="typecheck" default
cd web && npm run typecheck -- --pretty
```

## Release build
<!-- meshfox:node id="release-build" -->

An optimized, distributable single binary. The declared inputs cover Rust and frontend sources plus build configuration; the output is the release executable. Installing discovers this producer automatically. Unchanged inputs and an intact binary allow this step to skip when pulled into a chain. Requesting this block directly always builds. On macOS, the build checks the resulting executable and re-signs it ad hoc if it cannot start. The final output fingerprint includes this signature; install copies the finished binary. Toolchain and external environment changes require `--fresh`. Deliberately without `cache`: build logs are per-run output.

```sh name="release-build" inputs="Cargo.toml,Cargo.lock,.cargo/config.toml,crates/*/Cargo.toml,crates/*/build.rs,crates/*/src/**,web/src/**,web/public/**,web/package*.json,web/index.html,web/vite.config.ts,web/tsconfig*.json,grammars/**" outputs="target/release/meshfox"
set -e
export CARGO_TERM_COLOR=always
(cd web && npm ci && npm run build)
cargo build --workspace --release
if [ "$(uname -s)" = "Darwin" ]; then
  if ! target/release/meshfox --version; then
    echo "release binary does not start — re-signing it ad hoc"
    codesign --force -s - target/release/meshfox
    target/release/meshfox --version
  fi
fi
echo "binary: target/release/meshfox"
```

## Full check
<!-- meshfox:node id="full-check" -->

Run unit tests (macOS host and the Linux VM), the WebAssembly model's checks, typecheck, and e2e tests (including the in-browser build's):

```bash name="full-check" deps="unit-tests/run!,unit-tests/wasm!,linux-tests/run!,linting/typecheck!,e2e-tests/run!,e2e-tests/wasm-editor!,editor-tests/neovim-tests!,editor-tests/vscode-e2e!,release-build/release-build!"
echo "done"
```

## Editor bundle
<!-- meshfox:node id="editor-bundle-upload" -->

**Bundle to upload.** Builds the in-browser editor into `editor-bundle/`, a plain static site (relative URLs) plus `_headers`, which Cloudflare Pages reads itself: the security headers (Content-Security-Policy, …) and the cache rules. Cloudflare compresses (brotli) and serves `.wasm` correctly on its own. Serve it from its own origin (a subdomain): drafts live in that origin's IndexedDB.

```sh name="editor-bundle" deps="unit-tests/wasm-prep"
set -euo pipefail
out=editor-bundle
export PATH="$HOME/.cargo/bin:$PATH"

(cd web && npm run build:wasm)

rm -rf "$out"
mkdir -p "$out"
cp -R web/dist-wasm/. "$out/"
echo "$(find "$out" -type f | wc -l) files, $(du -sh "$out" | cut -f1)"
```

## Install
<!-- meshfox:node id="install" -->

Copies the release binary to `$INSTALL_PATH` (default `/usr/local/bin`), re-signing it ad-hoc on macOS. `inputs=` automatically finds the release build through its declared output, so no explicit `deps=` is needed. `outputs=` also tracks the installed binary: deleting or modifying it makes installation stale. A rebuild with identical binary contents does not invalidate installation when it is a dependency; explicitly requesting install always executes it, unless `meshfox run --skip-fresh` asks for the same up-to-date check a dependency gets. `env=` passes the declared path into the shell and the first run remembers the chosen value in `.meshfox/README.md.env`. The block name matches its node id, making it the implicit default block.

```sh name="install" inputs="target/release/meshfox" outputs="$INSTALL_PATH/meshfox" env="$INSTALL_PATH"
mkdir -p "$INSTALL_PATH"
cp target/release/meshfox "$INSTALL_PATH/meshfox"
# macOS: the linker's ad-hoc signature is flaky under AMFI (instant `killed`).
[ "$(uname -s)" = Darwin ] && codesign --force -s - "$INSTALL_PATH/meshfox"
echo "installed to $INSTALL_PATH/meshfox"
```

## Static site templates
<!-- meshfox:node id="static-site-templates" type="include" -->

[TEMPLATES.md](./TEMPLATES.md)

### Generating this repo's site
<!-- meshfox:node id="generating-this-repo-s-site" -->

This repo's own site (<https://meshfox.orofarne.net>) is generated from `README.md` with the archive template. [`wrangler.toml`](./wrangler.toml) serves the `site-dist` directory as Cloudflare assets, so that is where the output goes:

```sh
meshfox static README.md --template ./site-template-archive --out ./site-dist --force \
    --copy-files --recursive --sitemap --sitemap-git-dates
```

[`scripts/build-site.sh`](./scripts/build-site.sh) is the full build used for deploys: it also fetches the released `meshfox` binary and the DuckDB CLI (`display="table"` nodes in the examples need `duckdb` on `PATH`), and generates the Parquet file the tables example points at before running this command. `--sitemap-git-dates` needs a full git history, not a shallow clone.
