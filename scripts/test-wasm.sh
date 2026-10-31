#!/usr/bin/env bash
# Build the wasm binding twice — the Node (CommonJS) build and the browser
# (ES module) build — and run the end-to-end tests in crates/wasm/tests, which
# load the real .wasm and render real canvases.
set -euo pipefail
cd "$(dirname "$0")/.."
scripts/build-wasm.sh nodejs crates/wasm/pkg-node
scripts/build-wasm.sh web crates/wasm/pkg
node --test crates/wasm/tests/
