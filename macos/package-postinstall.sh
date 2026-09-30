#!/bin/bash
# pkgbuild runs this as root after placing the app and machine-wide agent.
set -euo pipefail

LABEL="net.orofarne.meshfox.setup"
AGENT="/Library/LaunchAgents/$LABEL.plist"
CLI="/Applications/Meshfox.app/Contents/Resources/meshfox"
CLI_LINK="/usr/local/bin/meshfox"

if [ ! -x /Applications/Meshfox.app/Contents/MacOS/Meshfox ] || [ ! -x "$CLI" ]; then
  echo "Meshfox: app payload is missing from /Applications; refusing to register agents" >&2
  exit 1
fi

mkdir -p /usr/local/bin
if [ ! -e "$CLI_LINK" ] && [ ! -L "$CLI_LINK" ]; then
  install_wrapper=true
elif [ -L "$CLI_LINK" ] && [ "$(readlink "$CLI_LINK")" = "$CLI" ]; then
  install_wrapper=true # Migrate the former link to the per-user selector.
elif [ -f "$CLI_LINK" ] && [ ! -L "$CLI_LINK" ] && head -n 2 "$CLI_LINK" | grep -qx '# meshfox-pkg-wrapper-v1'; then
  install_wrapper=true
else
  install_wrapper=false
fi

if [ "$install_wrapper" = true ]; then
  rm -f "$CLI_LINK"
  cat > "$CLI_LINK" <<'WRAPPER'
#!/bin/sh
# meshfox-pkg-wrapper-v1
if [ -x "$HOME/.local/bin/meshfox" ]; then
  exec "$HOME/.local/bin/meshfox" "$@"
fi
exec /Applications/Meshfox.app/Contents/Resources/meshfox "$@"
WRAPPER
  chmod 755 "$CLI_LINK"
else
  echo "Meshfox: $CLI_LINK already exists; leaving it unchanged" >&2
fi

# Refresh every currently logged-in GUI session after an upgrade. The
# machine-wide agent will also run for users who log in later.
while IFS= read -r user; do
  uid="$(id -u "$user" 2>/dev/null)" || continue
  if [ "$uid" -gt 0 ] && launchctl print "gui/$uid" >/dev/null 2>&1; then
    launchctl bootout "gui/$uid/$LABEL" 2>/dev/null || true
    launchctl bootstrap "gui/$uid" "$AGENT"
  fi
done < <(dscl . -list /Users)
