#!/bin/bash
# Remove the machine-wide app/agent and Meshfox daemon state for local users.
set -euo pipefail

if [ "$(id -u)" -ne 0 ]; then
  exec sudo /bin/bash "$0" "$@"
fi

APP="/Applications/Meshfox.app"
SETUP_LABEL="net.orofarne.meshfox.setup"
DAEMON_LABEL="net.orofarne.meshfox"
SETUP_PLIST="/Library/LaunchAgents/$SETUP_LABEL.plist"
CLI_LINK="/usr/local/bin/meshfox"
CLI="/Applications/Meshfox.app/Contents/Resources/meshfox"

while IFS= read -r user; do
  uid="$(id -u "$user" 2>/dev/null)" || continue
  home="$(dscl . -read "/Users/$user" NFSHomeDirectory 2>/dev/null | sed -n 's/^NFSHomeDirectory: //p')"
  [ -n "$home" ] && [ -d "$home" ] || continue
  launchctl bootout "gui/$uid/$SETUP_LABEL" 2>/dev/null || true
  launchctl bootout "gui/$uid/$DAEMON_LABEL" 2>/dev/null || true
  rm -f "$home/Library/LaunchAgents/$DAEMON_LABEL.plist"
  socket="$home/Library/Application Support/meshfox/daemon.sock"
  rm -f "$socket"
  rmdir "$home/Library/Application Support/meshfox" 2>/dev/null || true
  rm -rf "$home/Library/Logs/Meshfox"

  # Only remove the user's CLI when this package originally created it.
  cli_marker="$home/.local/bin/.meshfox-pkg-cli"
  if [ -f "$cli_marker" ]; then
    rm -f "$home/.local/bin/meshfox" "$cli_marker"
  fi

  # Remove only the daemon socket setting this package added.
  # Run as the file owner so an in-place edit does not make their config root-owned.
  config="$home/.meshfox/config.toml"
  if [ -f "$config" ]; then
    sudo -u "$user" env MESHFOX_SOCKET_TO_REMOVE="$socket" /usr/bin/perl -i -ne \
      'if (/^\s*server_socket\s*=\s*["\x27]([^"\x27]*)["\x27]\s*# meshfox-pkg-socket\s*$/ && $1 eq $ENV{MESHFOX_SOCKET_TO_REMOVE}) { next } print' \
      "$config"
  fi

done < <(dscl . -list /Users)

rm -f "$SETUP_PLIST"
if { [ -L "$CLI_LINK" ] && [ "$(readlink "$CLI_LINK")" = "$CLI" ]; } || \
   { [ -f "$CLI_LINK" ] && [ ! -L "$CLI_LINK" ] && head -n 2 "$CLI_LINK" | grep -qx '# meshfox-pkg-wrapper-v1'; }; then
  rm -f "$CLI_LINK"
fi
if [ -d "$APP" ]; then
  /System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister -u -f "$APP" 2>/dev/null || true
  rm -rf "$APP"
fi
pkgutil --forget net.orofarne.meshfox.pkg >/dev/null 2>&1 || true

echo "Removed Meshfox.app, LaunchAgents, sockets, logs and package receipt."
echo "Other personal ~/.meshfox settings and project data were kept."
