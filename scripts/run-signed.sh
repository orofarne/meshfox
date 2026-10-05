#!/usr/bin/env bash
# `cargo run`/`cargo test` runner on macOS (see .cargo/config.toml): the
# linker's own ad-hoc signature on a freshly built binary is flaky under
# AMFI — sometimes an instant silent `killed`, sometimes (large debug
# binaries especially) a long hang while AMFI/trustd get around to
# validating it, before `cargo` even prints "Running". Re-signing with a
# clean ad-hoc signature before exec avoids both — see this repo's memory
# note "macos-codesign-kill" for the original incident this was cut from.
#
# Many runners can start at once (the e2e suite's `webServer`s are ~40
# `cargo run`s of the same binary), and `codesign --force` rewrites the file
# in place: one runner exec'ing it while another is mid-rewrite fails with
# "object file format unrecognized, invalid, or unsuitable". So: sign only
# when the binary isn't already cleanly ad-hoc signed, and let one runner at
# a time do it (a `mkdir` lock — macOS has no `flock`), re-checking once the
# lock is held, since whoever held it before us has usually done the job.
set -euo pipefail

bin="$1"
shift

needs_sign() {
  ! codesign --verify "$bin" 2>/dev/null || codesign -dv "$bin" 2>&1 | grep -q "linker-signed"
}

if needs_sign; then
  lock="$bin.signlock"
  until mkdir "$lock" 2>/dev/null; do
    # A lock older than a minute belongs to a runner that died mid-sign.
    if [ -n "$(find "$lock" -maxdepth 0 -mmin +1 2>/dev/null)" ]; then
      rmdir "$lock" 2>/dev/null || true
    fi
    sleep 0.1
  done
  trap 'rmdir "$lock" 2>/dev/null || true' EXIT
  if needs_sign; then
    codesign --force -s - "$bin"
  fi
  rmdir "$lock" 2>/dev/null || true
  trap - EXIT
fi
exec "$bin" "$@"
