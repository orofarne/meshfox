<!-- meshfox:canvas -->
# Meshfox.app

The one macOS app meshfox ships: a menu-bar daemon (see `MeshfoxDaemon/`
for the Swift Package it's built from — SessionStore/UnixSocketServer/
AppDelegate) that also handles Finder's "open documents" Apple Event —
double-click/drag-onto-icon/"Open With" on a `.canvas.md` — the same role a
separate `MeshfoxCanvas.app` (an AppleScript droplet) used to play, folded
in here instead of kept as a second app, since the daemon already has all
the machinery a plain droplet didn't: a persistent, `meshfox open`-reachable
socket, session tracking, a menu to see/kill what's running.

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
not `Network.framework`, see its own doc comment), `SessionStore.swift`
(spawns/tracks/kills `meshfox view --watcher-socket` workers, the daemon's
own counterpart to `crates/cli/src/watcher.rs`'s `Registry`), `AppDelegate.swift`
(the tray menu + Finder's `application(_:open:)`), `main.swift` (resolves
the `meshfox` binary, sets up `SIGTERM`/`SIGINT` handling, starts the run
loop).

`MESHFOX_BIN`/`MESHFOX_DAEMON_BIN` environment variables — not for normal
use, just for developing/testing this daemon (respectively: which `meshfox`
CLI it spawns workers with, and — read by `meshfox open` on the Rust side,
`crates/cli/src/main.rs::resolve_daemon_app` — which daemon binary counts
as "installed") against a `target/debug`/`swift build` output instead of
whatever's actually installed.

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

The final `open -a` both warms up LaunchServices' trust in this app's own
type claims (a freshly ad-hoc-signed app's claims start out "untrusted" and
lose out to a "trusted" one during type resolution until the app has
actually been launched once — same finding as before) *and* is the daemon's
real first launch — unlike the old droplet (which needed
`MESHFOX_CANVAS_OPENER_WARMUP` to suppress a dialog on this specific
launch), a plain no-arguments launch here is already exactly what a normal
first run looks like: tray icon appears, starts listening. Nothing further
to suppress.

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

echo "Installed $APP"
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

Kills any running instance (so a stale process doesn't keep the socket
file/tray icon around after this), removes the installed app, and
unregisters it.

```bash
set -euo pipefail

APP="$HOME/Applications/Meshfox.app"

pkill -f "$APP/Contents/MacOS/Meshfox" 2>/dev/null || true

if [ -d "$APP" ]; then
  /System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister -u -f "$APP" || true
  rm -rf "$APP"
  echo "Removed $APP"
else
  echo "$APP not installed, nothing to do"
fi
```

