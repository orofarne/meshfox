#!/usr/bin/env bash
# Build the WebAssembly binding (crates/wasm) and generate its JavaScript glue.
#
#   scripts/build-wasm.sh [web|bundler|nodejs] [out-dir]
#
# `web` (default) writes an ES module that loads the .wasm by URL — for a
# browser <script type="module"> or Vite. `bundler` is for webpack-style
# bundlers, `nodejs` a CommonJS module for Node (what the tests use).
#
# Needs the wasm32 target (`rustup target add wasm32-unknown-unknown`) and a
# `wasm-bindgen` CLI whose version matches the one in Cargo.lock
# (`cargo install wasm-bindgen-cli --version <locked> --locked`). `wasm-opt`
# (binaryen) is used if present and shrinks the module further.
set -euo pipefail
cd "$(dirname "$0")/.."

target="${1:-web}"
case "$target" in web | bundler | nodejs) ;; *)
  echo "usage: $0 [web|bundler|nodejs] [out-dir]" >&2
  exit 2
  ;;
esac
out="${2:-crates/wasm/pkg}"

locked=$(grep -A1 '^name = "wasm-bindgen"$' Cargo.lock | sed -n 's/^version = "\(.*\)"/\1/p')
if ! command -v wasm-bindgen >/dev/null; then
  echo "wasm-bindgen CLI not found; install it with:" >&2
  echo "  cargo install wasm-bindgen-cli --version $locked --locked" >&2
  exit 1
fi
have=$(wasm-bindgen --version | awk '{print $2}')
if [[ "$have" != "$locked" ]]; then
  echo "wasm-bindgen CLI is $have but Cargo.lock pins $locked; they must match:" >&2
  echo "  cargo install wasm-bindgen-cli --version $locked --locked" >&2
  exit 1
fi

cargo build -p meshfox-wasm --profile wasm --target wasm32-unknown-unknown
wasm-bindgen --target "$target" --out-dir "$out" --out-name meshfox_wasm \
  target/wasm32-unknown-unknown/wasm/meshfox_wasm.wasm

if command -v wasm-opt >/dev/null; then
  wasm-opt -Oz --enable-bulk-memory --enable-nontrapping-float-to-int --enable-sign-ext \
    --enable-mutable-globals --enable-reference-types -o "$out/meshfox_wasm_bg.wasm" "$out/meshfox_wasm_bg.wasm"
fi
ls -l "$out/meshfox_wasm_bg.wasm" | awk '{print $5, "bytes:", $9}'
