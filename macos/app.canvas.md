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

To make `view`/`tui`/`run`/`node <op>`/`mcp` actually use this daemon as
their shared coordinator instead of each spawning its own worker, add
`server_socket` to `~/.meshfox/config.toml`:

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

Run `build` below to compile, bundle, ad-hoc sign, and install it to
`~/Applications/Meshfox.app`; `uninstall` removes it again. Both are safe
to re-run.

## Sources
<!-- meshfox:node id="sources" -->

[MeshfoxDaemon/](./MeshfoxDaemon/) — a plain Swift Package Manager
executable target (`swift build`/`swift run`, no Xcode project needed):
`Protocol.swift` (the wire format, mirrors `crates/server/src/watcher_protocol.rs`
byte-for-byte), `UnixSocketServer.swift` (raw POSIX socket — deliberately
not `Network.framework`, see its own doc comment; also where
`launch_activate_socket` inherits a launchd-bound socket when this app runs
under its own LaunchAgent, see `build`'s own "Socket activation" section),
`SessionStore.swift` (spawns/tracks/kills `meshfox view --watcher-socket`
workers, the daemon's own counterpart to `crates/cli/src/watcher.rs`'s
`Registry`), `AppDelegate.swift` (the tray menu + Finder's
`application(_:open:)`), `main.swift` (resolves the `meshfox` binary, sets
up `SIGTERM`/`SIGINT` handling, starts the run loop). `CLaunch/` is a tiny
system-library target exposing `<launch.h>`'s `launch_activate_socket` —
not bridged into Swift by any higher-level framework.

`MESHFOX_BIN` environment variable — not for normal use, just for
developing/testing this daemon against a `target/debug`/`swift build`
`meshfox` output instead of whatever's actually installed; which `meshfox`
CLI this daemon spawns workers with.

## Build & install
<!-- meshfox:node id="build" -->

Compiles a release build of `MeshfoxDaemon`, assembles `~/Applications/Meshfox.app`
by hand (no `osacompile` this time — unlike the old AppleScript droplet,
this is a real compiled binary, so the bundle is just a directory structure
+ `Info.plist` this script writes directly with `PlistBuddy`, starting from
an empty plist via `plutil -create`), builds the same app icon
`web/scripts/gen-icons.mjs` already renders from `mascot.txt` for the web
UI/favicons (no re-rendering — `icon-512.png` is the largest one committed,
everything smaller is a plain `sips` downscale of it, same pipeline the old
droplet's own build used), then ad-hoc-signs it (unsigned local bundles
otherwise get silently killed by AMFI) and re-registers it with
LaunchServices.

`LSUIElement=true`: menu-bar-only, no Dock icon, no Cmd-Tab entry.
`CFBundleDocumentTypes` claims `net.daringfireball.markdown` at `Alternate`
rank rather than a compound `canvas.md` extension — the same finding the
old droplet's own build already had to work around: LaunchServices resolves
a file's type from that Apple-internal claim before any third-party app's
own (narrower) extension list ever gets consulted (checked directly with
`mdimport -t`, not assumed), so a `canvas.md`-specific claim would never
win regardless of specificity.

The `open -a` right after signing/registering warms up LaunchServices'
trust in this app's own type claims (a freshly ad-hoc-signed app's claims
start out "untrusted" and lose out to a "trusted" one during type
resolution until the app has actually been launched once — same finding as
before); that one-off instance is then killed again immediately —
everything from here on is the *LaunchAgent*'s own job (see below), and
letting this warmup instance keep running would race it for the very same
socket.

**Socket activation (LaunchAgent), replacing "just run the app and hope it
stays up":** registers `~/Library/LaunchAgents/net.orofarne.meshfox.plist`
with `launchctl bootstrap` — `RunAtLoad` (starts at login), `KeepAlive`
with `SuccessfulExit: false` (restarts on a crash, *not* after a clean
"Quit" — a normal `exit(0)` isn't a "successful exit" in launchd's own
sense to restart from... it is a successful exit, which is exactly why
`SuccessfulExit: false` means "don't restart after one" — see `man
launchd.plist`), and a `Sockets` entry naming the same well-known path
`main.swift`'s own `defaultSocketPath()` uses. That last part is the actual
point: launchd itself creates and holds that socket open from the moment
this LaunchAgent is loaded, independent of whether the daemon process
happens to be running at any given instant — a client (`meshfox`'s own
`coordinator::resolve`, or this extension) can always connect immediately;
launchd spawns (or wakes) the real process behind the scenes on first use,
handing it the already-bound fd via `launch_activate_socket` (see
`UnixSocketServer.swift`). This is what let every "find the app, spawn it,
poll until its socket comes up" retry logic on the client side (Rust *and*
TypeScript) go away entirely — there's no "not started yet" state for a
client to ever observe once this LaunchAgent is loaded.

`rm -f "$SOCKET_PATH"` right after `bootout`, before re-registering: a real
incident (2026-09-17) left launchd's own kernel-level socket holding an
unread, permanently-stuck connection across multiple `bootstrap`/process
restarts — every `getPort` waiting on that socket hung forever, even
against a freshly-launched daemon process, until the socket *file* itself
was deleted and the LaunchAgent bootstrapped fresh against a clean one.
Root cause not fully pinned down (likely stale state surviving an earlier
`bootstrap` that wasn't cleanly `bootout`'d first, from iterating on this
exact feature) — this line makes re-running `build` always start from a
known-clean socket regardless, rather than relying on that never
recurring. Two independent, code-level backstops for the same failure
mode now also exist so it fails loudly instead of hanging forever even if
this happens again: `SessionStore.getPortTimeoutSeconds` (kills a worker
that never reports ready and fails whoever's waiting on it),
`watcher_protocol::REQUEST_PORT_TIMEOUT` on the Rust client side, and
`StartupSelfTest.swift` (a round-trip `get_port` self-check a few hundred
ms after every launch, logged to `daemon.log`).

```bash always
set -euo pipefail

APP="$HOME/Applications/Meshfox.app"
BUNDLE_ID="net.orofarne.meshfox"

echo "swift build -c release..."
(cd MeshfoxDaemon && swift build -c release)
BIN="MeshfoxDaemon/.build/arm64-apple-macosx/release/MeshfoxDaemon"

rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$BIN" "$APP/Contents/MacOS/Meshfox"

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
"$PB" -c "Add :CFBundleVersion string 1" "$PLIST"
"$PB" -c "Add :CFBundleShortVersionString string 0.1.0" "$PLIST"
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

codesign --force -s - "$APP"

LSREGISTER=/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister
"$LSREGISTER" -f "$APP"

open -a "$APP"
sleep 2
pkill -f "$APP/Contents/MacOS/Meshfox" 2>/dev/null || true
sleep 1

LABEL="$BUNDLE_ID"
AGENT_PLIST="$HOME/Library/LaunchAgents/$LABEL.plist"
SOCKET_PATH="$HOME/Library/Application Support/meshfox/daemon.sock"
mkdir -p "$HOME/Library/LaunchAgents" "$(dirname "$SOCKET_PATH")"

launchctl bootout "gui/$(id -u)/$LABEL" 2>/dev/null || true
rm -f "$SOCKET_PATH"

plutil -create xml1 "$AGENT_PLIST"
"$PB" -c "Add :Label string $LABEL" "$AGENT_PLIST"
"$PB" -c "Add :ProgramArguments array" "$AGENT_PLIST"
"$PB" -c "Add :ProgramArguments:0 string $APP/Contents/MacOS/Meshfox" "$AGENT_PLIST"
"$PB" -c "Add :RunAtLoad bool true" "$AGENT_PLIST"
"$PB" -c "Add :KeepAlive dict" "$AGENT_PLIST"
"$PB" -c "Add :KeepAlive:SuccessfulExit bool false" "$AGENT_PLIST"
"$PB" -c "Add :Sockets dict" "$AGENT_PLIST"
"$PB" -c "Add :Sockets:Listener dict" "$AGENT_PLIST"
"$PB" -c "Add :Sockets:Listener:SockPathName string $SOCKET_PATH" "$AGENT_PLIST"
"$PB" -c "Add :Sockets:Listener:SockType string stream" "$AGENT_PLIST"
LOG_DIR="$HOME/Library/Logs/Meshfox"
mkdir -p "$LOG_DIR"
"$PB" -c "Add :StandardOutPath string $LOG_DIR/daemon.log" "$AGENT_PLIST"
"$PB" -c "Add :StandardErrorPath string $LOG_DIR/daemon.log" "$AGENT_PLIST"

launchctl bootstrap "gui/$(id -u)" "$AGENT_PLIST"

echo "Installed $APP"
echo "Registered LaunchAgent $AGENT_PLIST (socket: $SOCKET_PATH)"
```

## Package for sharing
<!-- meshfox:node id="package" -->

Rebuilds fresh via `build` (so what you hand out always matches what
`MeshfoxDaemon/` currently says) and zips `~/Applications/Meshfox.app` into
`~/Desktop/Meshfox.zip`, using `ditto` rather than a plain `zip -r` — same
as Finder's own "Compress", and unlike a naive `zip`, it preserves the
bundle's resource fork/extended attributes so `codesign`'s signature
survives being re-extracted on someone else's Mac.

Ad-hoc signed only, not notarized with a paid Apple Developer ID — whoever
you send this to needs to right-click → Open once (or System Settings →
Privacy & Security → Open Anyway after the first refusal) before
Gatekeeper lets it run normally, same as any other unsigned indie tool.

```bash deps="build/build" always default
set -euo pipefail

APP="$HOME/Applications/Meshfox.app"
OUT="$HOME/Desktop/Meshfox.zip"

rm -f "$OUT"
ditto -c -k --sequesterRsrc --keepParent "$APP" "$OUT"

echo "Wrote $OUT"
echo "Whoever you send it to: right-click -> Open the first time (it's"
echo "ad-hoc signed, not notarized, so Gatekeeper will otherwise refuse it)."
```

## Uninstall
<!-- meshfox:node id="uninstall" -->

Unregisters the LaunchAgent first (`launchctl bootout`, which also stops
the running instance it manages), removes its plist, then falls back to a
plain `pkill` too (covers a manually-launched instance predating the
LaunchAgent, or a `swift run` left over from development) before removing
the installed app and unregistering it from LaunchServices.

```bash
set -euo pipefail

APP="$HOME/Applications/Meshfox.app"
LABEL="net.orofarne.meshfox"
AGENT_PLIST="$HOME/Library/LaunchAgents/$LABEL.plist"

launchctl bootout "gui/$(id -u)/$LABEL" 2>/dev/null || true
rm -f "$AGENT_PLIST"

pkill -f "$APP/Contents/MacOS/Meshfox" 2>/dev/null || true

if [ -d "$APP" ]; then
  /System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister -u -f "$APP" || true
  rm -rf "$APP"
  echo "Removed $APP"
else
  echo "$APP not installed, nothing to do"
fi
```

