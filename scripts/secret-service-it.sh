#!/usr/bin/env bash
# Runs crates/core/tests/secret_service_it.rs and
# crates/cli/tests/secret_cmd_linux.rs against a real Secret Service:
# gnome-keyring in a private, throwaway session bus, so it never touches the
# user's own keyring. Linux only; needs `dbus-run-session`, `gnome-keyring`,
# `secret-tool` (libsecret-tools) and a Rust toolchain.
set -euo pipefail
cd "$(dirname "$0")/.."

cargo test -p meshfox-core --test secret_service_it --no-run
cargo test -p meshfox-cli --test secret_cmd_linux --no-run

# No session bus at all: the error path.
DBUS_SESSION_BUS_ADDRESS=unix:path=/nonexistent/bus \
  cargo test -p meshfox-core --test secret_service_it -- --ignored no_secret_service
cargo test -p meshfox-cli --test secret_cmd_linux -- --ignored fail_loudly

# A real service: a fresh keyring, unlocked with a throwaway password.
dbus-run-session -- bash -c '
  set -euo pipefail
  echo -n throwaway-password | gnome-keyring-daemon --unlock --components=secrets >/dev/null
  cargo test -p meshfox-core --test secret_service_it -- --ignored \
    round_trip accounts_do_not_leak awkward_values items_are_visible
  cargo test -p meshfox-cli --test secret_cmd_linux -- --ignored secret_set_show
'
