<!-- meshfox:canvas -->
# Meshfox.app

The one macOS app meshfox ships: a menu-bar daemon (see `MeshfoxDaemon/`
for the Swift Package it's built from — SessionStore/UnixSocketServer/
AppDelegate) that also handles Finder's "open documents" Apple Event —
double-click/drag-onto-icon/"Open With" on a `.canvas.md` — the same role a
separate `MeshfoxCanvas.app` (an AppleScript droplet) used to play, folded
in here instead of kept as a second app, since the daemon already has all
the machinery a plain droplet didn't: a persistent socket, session
tracking, a menu to see/kill what's running.

On its first launch in each account, the daemon offers to add `server_socket`
to `~/.meshfox/config.toml`, so `view`/`tui`/`run`/`node <op>`/`mcp` use
its shared socket. It preserves existing settings and leaves an existing
`server_socket` value alone. If declined, use **Connect CLI to Daemon…**
in the menu bar later, or add the setting manually:

```toml
server_socket = "/Users/<you>/Library/Application Support/meshfox/daemon.sock"
```

(the exact path `MeshfoxDaemon/Sources/MeshfoxDaemon/main.swift`'s own
`defaultSocketPath()` uses — see `crates/core/src/config.rs`'s
`server_socket` and `crates/cli/src/coordinator.rs` on the Rust side for
what reads this).

**A daemon-spawned worker inherits launchd's own minimal environment**, not
your login shell's — `npm`/`cargo`/anything under `~/.cargo/bin` or
Homebrew that only ever gets onto `$PATH` via `.zshrc`/`.zprofile` is
invisible to it, even though it's on your normal interactive shell's
`$PATH`. Fix this with `[env]` in the same `~/.meshfox/config.toml` (or a
project-local `.meshfox/config.toml`) rather than trying to make launchd
itself source your shell config — see `crates/core/src/config.rs`'s
`env_overrides` for the full rationale:

```toml
[env]
PATH = "$PATH:/opt/homebrew/bin:/Users/<you>/.cargo/bin"
```

`$NAME`/`${NAME}` in a value is replaced with that same variable's current
value in the process actually spawning the block — so `PATH` above
*extends* whatever the worker already had rather than replacing it
outright. Applies to every spawned block/interpreter
(`stream_exec::spawn_bash`/`spawn_process`/`spawn_interpreter`) regardless
of what launched the worker, not just daemon-spawned ones — a block's own
`env=` locals still win over a same-named `[env]` entry on conflict.

Bundle id `net.orofarne.meshfox`; the app is named plainly "Meshfox"
everywhere a user sees it (Finder, `/Applications`, the tray icon's own
tooltip) even though the Swift package/source directory underneath keeps
its own internal name, `MeshfoxDaemon` — a build detail, not something
worth renaming a whole source tree over.

Run `build` to create `macos/dist/Meshfox.pkg` and a ZIP containing that graphical installer plus `Uninstall.command`. The package installs the app for all users in `/Applications` and registers a login agent for every user. Each user gets a separate socket-activated daemon and socket. The package seeds a private `~/.local/bin/meshfox` for each user. That CLI updates independently with `meshfox check-updates`; new worker sessions use the updated binary, while existing sessions must restart. The signed app and its fallback CLI change only when a newer package is installed. Run `Uninstall.command` from the ZIP to remove the app and its agents.

## Sources
<!-- meshfox:node id="sources" -->

[MeshfoxDaemon/](./MeshfoxDaemon/) — a plain Swift Package Manager
executable target (`swift build`/`swift run`, no Xcode project needed):
`Protocol.swift` (the wire format, mirrors `crates/server/src/watcher_protocol.rs`
byte-for-byte), `UnixSocketServer.swift` (raw POSIX socket — deliberately
not `Network.framework`, see its own doc comment; also where
`launch_activate_socket` inherits a launchd-bound socket when this app runs
under its own LaunchAgent, see `setup-user-agent.sh`),
`SessionStore.swift` (spawns/tracks/kills `meshfox view --watcher-socket`
workers, the daemon's own counterpart to `crates/cli/src/watcher.rs`'s
`Registry`), `AppDelegate.swift` (the tray menu + Finder's
`application(_:open:)`), `main.swift` (resolves the `meshfox` binary, sets
up `SIGTERM`/`SIGINT` handling, starts the run loop). `CLaunch/` is a tiny
system-library target exposing `<launch.h>`'s `launch_activate_socket` —
not bridged into Swift by any higher-level framework.

The package seeds a private `~/.local/bin/meshfox` for each user from the bundled release binary under `Contents/Resources`. The daemon prefers that personal CLI, then falls back to the bundled one. `MESHFOX_BIN` overrides both for development/testing.

## Build & package
<!-- meshfox:node id="build" -->

Builds the web UI and release CLI, embeds that CLI in the signed app, then makes a graphical macOS Installer package (`dist/Meshfox.pkg`), and a ZIP containing the package and `Uninstall.command`. The package pins `Meshfox.app` to `/Applications` (disabling Installer bundle relocation) and a machine-wide login agent into `/Library/LaunchAgents`. Each user gets a separate socket-activated daemon when they log in; the installer refreshes every active GUI session immediately, including on upgrades. The package seeds each user's `~/.local/bin/meshfox` only when absent. If `/usr/local/bin/meshfox` is free, a small wrapper selects the calling user's personal CLI (falling back to the bundled copy). An unrelated command at that path is preserved. Run this block from `macos/`. Building does not install anything.

`LSUIElement=true` makes the app menu-bar-only. Its Markdown document claim has Alternate rank, matching the type resolution behavior described in the prior build workflow.

```bash name="build" always default
set -euo pipefail

APP="dist/Meshfox.app"
OUT="dist/Meshfox.zip"
PKG="dist/Meshfox.pkg"
BUNDLE_ID="net.orofarne.meshfox"
RELEASE_TAG="${GITHUB_REF_NAME:-$(git describe --tags --exact-match HEAD 2>/dev/null || true)}"
if [[ "$RELEASE_TAG" =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  VERSION="${RELEASE_TAG#v}"
else
  VERSION="0.1.0"
fi
BUILD_NUMBER="$(git rev-list --count HEAD)"

echo "Building embedded web UI and release CLI..."
(cd ../web && npm install && npm run build)
(cd .. && cargo build -p meshfox-cli --release)
CLI_BIN="../target/release/meshfox"

echo "swift build -c release..."
(cd MeshfoxDaemon && swift build -c release)
BIN="$(cd MeshfoxDaemon && swift build -c release --show-bin-path)/MeshfoxDaemon"

mkdir -p dist
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$BIN" "$APP/Contents/MacOS/Meshfox"
cp "$CLI_BIN" "$APP/Contents/Resources/meshfox"
cp setup-user-agent.sh "$APP/Contents/Resources/setup-user-agent.sh"
cp uninstall-app.sh "$APP/Contents/Resources/Uninstall.command"
chmod 755 "$APP/Contents/Resources/"{setup-user-agent.sh,Uninstall.command}

ICON_WORKDIR="$(mktemp -d -t meshfox-app)"
ICONSET="$ICON_WORKDIR/AppIcon.iconset"
mkdir -p "$ICONSET"
ICON_SRC="../web/public/icon-512.png"
sips -z 16 16   "$ICON_SRC" --out "$ICONSET/icon_16x16.png" >/dev/null
sips -z 32 32   "$ICON_SRC" --out "$ICONSET/icon_16x16@2x.png" >/dev/null
sips -z 32 32   "$ICON_SRC" --out "$ICONSET/icon_32x32.png" >/dev/null
sips -z 64 64   "$ICON_SRC" --out "$ICONSET/icon_32x32@2x.png" >/dev/null
sips -z 128 128 "$ICON_SRC" --out "$ICONSET/icon_128x128.png" >/dev/null
sips -z 256 256 "$ICON_SRC" --out "$ICONSET/icon_128x128@2x.png" >/dev/null
sips -z 256 256 "$ICON_SRC" --out "$ICONSET/icon_256x256.png" >/dev/null
cp "$ICON_SRC" "$ICONSET/icon_256x256@2x.png"
cp "$ICON_SRC" "$ICONSET/icon_512x512.png"
iconutil -c icns "$ICONSET" -o "$APP/Contents/Resources/AppIcon.icns"
rm -rf "$ICON_WORKDIR"

PLIST="$APP/Contents/Info.plist"
PB=/usr/libexec/PlistBuddy
plutil -create xml1 "$PLIST"
"$PB" -c "Add :CFBundleExecutable string Meshfox" "$PLIST"
"$PB" -c "Add :CFBundleIdentifier string $BUNDLE_ID" "$PLIST"
"$PB" -c "Add :CFBundleName string Meshfox" "$PLIST"
"$PB" -c "Add :CFBundleDisplayName string Meshfox" "$PLIST"
"$PB" -c "Add :CFBundleVersion string $BUILD_NUMBER" "$PLIST"
"$PB" -c "Add :CFBundleShortVersionString string $VERSION" "$PLIST"
"$PB" -c "Add :CFBundlePackageType string APPL" "$PLIST"
"$PB" -c "Add :LSUIElement bool true" "$PLIST"
"$PB" -c "Add :CFBundleIconFile string AppIcon" "$PLIST"
"$PB" -c "Add :CFBundleIconName string AppIcon" "$PLIST"
"$PB" -c "Add :CFBundleDocumentTypes array" "$PLIST"
"$PB" -c "Add :CFBundleDocumentTypes:0 dict" "$PLIST"
"$PB" -c "Add :CFBundleDocumentTypes:0:CFBundleTypeName string 'Markdown / Meshfox Canvas'" "$PLIST"
"$PB" -c "Add :CFBundleDocumentTypes:0:CFBundleTypeRole string Viewer" "$PLIST"
"$PB" -c "Add :CFBundleDocumentTypes:0:LSHandlerRank string Alternate" "$PLIST"
"$PB" -c "Add :CFBundleDocumentTypes:0:LSItemContentTypes array" "$PLIST"
"$PB" -c "Add :CFBundleDocumentTypes:0:LSItemContentTypes:0 string net.daringfireball.markdown" "$PLIST"

codesign --force -s - "$APP/Contents/Resources/meshfox"
codesign --force -s - "$APP"

PKG_ROOT="dist/pkg-root"
PKG_SCRIPTS="dist/pkg-scripts"
rm -rf "$PKG_ROOT" "$PKG_SCRIPTS"
mkdir -p "$PKG_ROOT/Applications" "$PKG_ROOT/Library/LaunchAgents" "$PKG_SCRIPTS"
ditto "$APP" "$PKG_ROOT/Applications/Meshfox.app"
cp package-postinstall.sh "$PKG_SCRIPTS/postinstall"
chmod 755 "$PKG_SCRIPTS/postinstall"

SETUP_PLIST="$PKG_ROOT/Library/LaunchAgents/net.orofarne.meshfox.setup.plist"
plutil -create xml1 "$SETUP_PLIST"
"$PB" -c "Add :Label string net.orofarne.meshfox.setup" "$SETUP_PLIST"
"$PB" -c "Add :ProgramArguments array" "$SETUP_PLIST"
"$PB" -c "Add :ProgramArguments:0 string /bin/bash" "$SETUP_PLIST"
"$PB" -c "Add :ProgramArguments:1 string /Applications/Meshfox.app/Contents/Resources/setup-user-agent.sh" "$SETUP_PLIST"
"$PB" -c "Add :RunAtLoad bool true" "$SETUP_PLIST"

COMPONENTS_PLIST="dist/pkg-components.plist"
pkgbuild --analyze --root "$PKG_ROOT" "$COMPONENTS_PLIST"
"$PB" -c "Set :0:BundleIsRelocatable false" "$COMPONENTS_PLIST"
"$PB" -c "Set :0:BundleIsVersionChecked false" "$COMPONENTS_PLIST"

rm -f "$PKG"
pkgbuild --root "$PKG_ROOT" --install-location / \
  --identifier net.orofarne.meshfox.pkg --version "$VERSION" \
  --component-plist "$COMPONENTS_PLIST" \
  --ownership recommended --scripts "$PKG_SCRIPTS" "$PKG"

PACKAGE_DIR="dist/package"
rm -rf "$PACKAGE_DIR"
mkdir -p "$PACKAGE_DIR"
cp "$PKG" "$PACKAGE_DIR/Meshfox.pkg"
cp uninstall-app.sh "$PACKAGE_DIR/Uninstall.command"
chmod 755 "$PACKAGE_DIR/Uninstall.command"
rm -f "$OUT"
ditto -c -k --sequesterRsrc "$PACKAGE_DIR" "$OUT"

echo "Built $APP, $PKG and $OUT"
```

## Install
<!-- meshfox:node id="install" -->

Open the graphical macOS Installer package generated by `build`. Installer requests administrator access and installs the app for all users. The ZIP contains the same package, so installation on another Mac does not require this canvas or the source tree.

```bash name="install" tty always default
set -euo pipefail
open dist/Meshfox.pkg
```

## Uninstall
<!-- meshfox:node id="uninstall" -->

The distributed ZIP includes `Uninstall.command`. Run it in Terminal to remove `/Applications/Meshfox.app`, the machine-wide login agent, per-user LaunchAgents, sockets, Meshfox daemon logs, and the package receipt. It asks for administrator access. A personal CLI is removed only when the package originally seeded it; pre-existing CLI files are kept. Other personal `~/.meshfox` settings and project data are kept.

```bash name="uninstall" tty always default
set -euo pipefail
bash uninstall-app.sh
```

