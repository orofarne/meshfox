#!/usr/bin/env bash
# Installs the Meshfox coordinator as a per-user, socket-activated systemd
# service (linux/systemd/meshfox.{socket,service}).
#
#   linux/install-user-units.sh [PATH_TO_MESHFOX]   install (and start the socket)
#   linux/install-user-units.sh --uninstall         stop and remove
#
# With PATH_TO_MESHFOX the binary is copied to ~/.local/bin/meshfox (the path
# meshfox.service runs); without it, ~/.local/bin/meshfox must already exist.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
unit_dir="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user"

if [[ "${1:-}" == "--uninstall" ]]; then
  systemctl --user disable --now meshfox.socket 2>/dev/null || true
  systemctl --user stop meshfox.service 2>/dev/null || true
  rm -f "$unit_dir/meshfox.socket" "$unit_dir/meshfox.service"
  systemctl --user daemon-reload
  echo "removed the meshfox user units (~/.local/bin/meshfox and your config are untouched)"
  exit 0
fi

if [[ -n "${1:-}" ]]; then
  mkdir -p "$HOME/.local/bin"
  install -m 755 "$1" "$HOME/.local/bin/meshfox"
fi
if [[ ! -x "$HOME/.local/bin/meshfox" ]]; then
  echo "install-user-units: $HOME/.local/bin/meshfox not found; pass the path to a meshfox binary" >&2
  exit 1
fi

mkdir -p "$unit_dir"
install -m 644 "$here/systemd/meshfox.socket" "$here/systemd/meshfox.service" "$unit_dir/"
systemctl --user daemon-reload
systemctl --user enable --now meshfox.socket

runtime="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"
echo "installed. To use it, add this to ~/.meshfox/config.toml:"
echo
echo "  server_socket = \"$runtime/meshfox/daemon.sock\""
