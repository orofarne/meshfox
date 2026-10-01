#!/bin/bash
# Run by the machine-wide login agent, once in each user's GUI session.
set -euo pipefail

APP="/Applications/Meshfox.app"
LABEL="net.orofarne.meshfox"
AGENT_PLIST="$HOME/Library/LaunchAgents/$LABEL.plist"
SOCKET_PATH="$HOME/Library/Application Support/meshfox/daemon.sock"
LOG_DIR="$HOME/Library/Logs/Meshfox"
PB=/usr/libexec/PlistBuddy

if [ ! -x "$APP/Contents/MacOS/Meshfox" ]; then
  echo "Meshfox is missing from /Applications" >&2
  exit 1
fi

# Seed a private CLI once. Subsequent package installs leave it alone so
# `meshfox check-updates` can advance it independently of the app bundle.
LOCAL_BIN="$HOME/.local/bin/meshfox"
if [ ! -e "$LOCAL_BIN" ] && [ ! -L "$LOCAL_BIN" ]; then
  mkdir -p "$(dirname "$LOCAL_BIN")"
  ditto "$APP/Contents/Resources/meshfox" "$LOCAL_BIN"
  chmod 755 "$LOCAL_BIN"
  codesign --force -s - "$LOCAL_BIN"
  touch "$HOME/.local/bin/.meshfox-pkg-cli"
fi

launchctl bootout "gui/$(id -u)/$LABEL" 2>/dev/null || true

mkdir -p "$HOME/Library/LaunchAgents" "$(dirname "$SOCKET_PATH")" "$LOG_DIR"
rm -f "$SOCKET_PATH" "$AGENT_PLIST"
plutil -create xml1 "$AGENT_PLIST"
"$PB" -c "Add :Label string $LABEL" "$AGENT_PLIST"
"$PB" -c "Add :ProgramArguments array" "$AGENT_PLIST"
"$PB" -c "Add :ProgramArguments:0 string $APP/Contents/MacOS/Meshfox" "$AGENT_PLIST"
"$PB" -c "Add :RunAtLoad bool true" "$AGENT_PLIST"
# Leave KeepAlive unset: a clean Quit must return to socket-triggered demand.
"$PB" -c "Add :Sockets dict" "$AGENT_PLIST"
"$PB" -c "Add :Sockets:Listener dict" "$AGENT_PLIST"
"$PB" -c "Add :Sockets:Listener:SockPathName string $SOCKET_PATH" "$AGENT_PLIST"
"$PB" -c "Add :Sockets:Listener:SockType string stream" "$AGENT_PLIST"
"$PB" -c "Add :StandardOutPath string $LOG_DIR/daemon.log" "$AGENT_PLIST"
"$PB" -c "Add :StandardErrorPath string $LOG_DIR/daemon.log" "$AGENT_PLIST"

launchctl bootstrap "gui/$(id -u)" "$AGENT_PLIST"
