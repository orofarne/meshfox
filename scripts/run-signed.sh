#!/usr/bin/env bash
# `cargo run`/`cargo test` runner on macOS (see .cargo/config.toml): the
# linker's own ad-hoc signature on a freshly built binary is flaky under
# AMFI — sometimes an instant silent `killed`, sometimes (large debug
# binaries especially) a long hang while AMFI/trustd get around to
# validating it, before `cargo` even prints "Running". Re-signing with a
# clean ad-hoc signature before exec avoids both — see this repo's memory
# note "macos-codesign-kill" for the original incident this was cut from.
set -euo pipefail

bin="$1"
shift
codesign --force -s - "$bin"
exec "$bin" "$@"
