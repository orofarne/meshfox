<!-- meshfox:canvas -->
# canvas-opener

Builds a tiny macOS "opener" app, `MeshfoxCanvas.app`, that hands every
file it's given to `meshfox view`. For now it's just that — pick it
manually via Finder's Get Info → Open with for a one-off file. Making it
the actual default handler for `*.canvas.md` is a separate, not-yet-done
step (see below for why that's less trivial than it looks).

It's built from an AppleScript droplet, not a plain shell script, because
a bare Unix executable as `CFBundleExecutable` never actually receives the
files Finder hands over on double-click/"Open With"/drag — it gets
launched, but always with an empty argv (also verified empirically, the
hard way). Only a real app with its own run loop gets Finder's "open
documents" Apple Event; `osacompile` is what turns a few lines of
AppleScript into exactly that.

`meshfox` itself is resolved fresh from `PATH` every time the app runs,
not baked in once at build time — so a `meshfox` installed or upgraded
after the last `build` is still picked up. The lookup also mixes
`~/.local/bin` into that `PATH` explicitly, since that's where meshfox's
own install script drops the binary by default and it isn't on a fresh
Mac's `PATH` otherwise — without that, a from-Terminal-just-now install
wouldn't be found until the user's shell rc file got updated (or a new
login session started) even though it succeeded. If `meshfox` isn't found
at all, the app opens a Terminal window and runs meshfox's own install
script instead of failing quietly; it doesn't try to open the file that
triggered it — just double-click it again once the install finishes.

The obvious-looking trick for that later step — a custom Uniform Type
Identifier whose filename extension is the whole compound suffix
`canvas.md`, the way `tar.gz` is told apart from a plain `.gz` — turns out
**not** to work: LaunchServices resolves a file's type from the system's
own apple-internal `net.daringfireball.markdown` claim (extension `md`)
before any third-party app's own extension list is even consulted, so a
narrower `canvas.md` claim never wins, no matter how specific (checked
directly with `mdimport -t`, not assumed). Actually binding this as the
default will need either claiming the whole `md` type and dispatching by
suffix inside `launcher.applescript`, or some other approach — left for
later.

Run `build` below to assemble and install the app under `~/Applications`;
`uninstall` removes it again. Both are safe to re-run.

## Sources
<!-- meshfox:node id="sources" -->

The one source file the app is compiled from — an AppleScript droplet, not
a plain shell script (see `build` below for why):

### launcher.applescript
<!-- meshfox:node id="launcher-applescript" type="file" display="code" lang="applescript" -->

[launcher.applescript](./src/launcher.applescript)

## Build & install
<!-- meshfox:node id="build" -->

Compiles `src/launcher.applescript` into `~/Applications/MeshfoxCanvas.app`
via `osacompile` (which produces its own default `Info.plist`/icon),
builds a real app icon from the same PNGs `web/scripts/gen-icons.mjs`
already renders from `mascot.txt` for the web UI/favicons (no
re-rendering — `icon-512.png` is the largest one committed, everything
smaller is a plain `sips` downscale of it), and patches just the handful
of `Info.plist` keys that matter — bundle id, background-only, the icon,
and a document-type declaration so the app is choosable via Finder's
per-file Open With — with `PlistBuddy` rather than overwriting the whole
file, so whatever else `osacompile` sets up survives. Then ad-hoc-signs it
(unsigned local bundles otherwise get silently killed by AMFI) and
re-registers it with LaunchServices. Unlike the app itself, this step
doesn't need `meshfox` on `PATH` at all — the app resolves it fresh on
every launch instead (see `launcher.applescript`'s own `resolveMeshfox`;
if it's missing at that point, the app opens Terminal with the install
command from https://meshfox.orofarne.net/ instead of failing silently).

The icon is deliberately saved as `AppIcon.icns`, not layered over
`osacompile`'s own `droplet.icns` — `osacompile` also ships a compiled
`Assets.car` asset catalog with a `droplet` icon set in it, and that
catalog is consulted *before* a loose `.icns` file with the same name, so
overwriting `droplet.icns` alone wouldn't actually change what Finder
shows. A name absent from the catalog has nothing to shadow it with.

It also opens the freshly built app once with no document, which sounds
like a no-op but isn't: a freshly ad-hoc-signed app's own type claims
start out marked "untrusted" and lose out to a "trusted" one during type
resolution — this is exactly what caused the compound-extension approach
(see this canvas's own intro) to look plausible right up until it was
actually tested — and only flips to "trusted" once the app has been
launched at least once (also verified directly, not assumed). That launch
is what `launcher.applescript`'s `on run` handler fires for — normally a
plain double-click on the app icon shows a `meshfox --version` dialog
(handy as a quick "is it actually working" check), but this particular
launch passes `MESHFOX_CANVAS_OPENER_WARMUP=1` via `open --env` so the
handler recognizes it and stays silent instead of popping a dialog on
every single `build` run.

This only installs the app — it does **not** make it the default handler
for anything yet (that's a separate, not-yet-done step; see the intro).
Until then, the only way to actually use it is Finder's own "Open With"
submenu, one file at a time. See `package` below to hand a copy to
someone else instead of just using it locally.

```bash always
set -euo pipefail

APP="$HOME/Applications/MeshfoxCanvas.app"
rm -rf "$APP"

osacompile -o "$APP" src/launcher.applescript

ICON_WORKDIR="$(mktemp -d -t meshfox-canvas-opener)"
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
# No 1024 (icon_512x512@2x) source is committed — iconutil is fine
# building without that one top slot, just without a Retina render at the
# very largest Finder icon-view zoom.
iconutil -c icns "$ICONSET" -o "$APP/Contents/Resources/AppIcon.icns"
rm -rf "$ICON_WORKDIR"

PLIST="$APP/Contents/Info.plist"
PB=/usr/libexec/PlistBuddy
"$PB" -c "Add :CFBundleIdentifier string net.orofarne.meshfox-canvas-opener" "$PLIST"
"$PB" -c "Set :CFBundleName MeshfoxCanvas" "$PLIST"
"$PB" -c "Add :CFBundleDisplayName string 'Meshfox Canvas'" "$PLIST"
"$PB" -c "Add :LSUIElement bool true" "$PLIST"
# osacompile's own default Info.plist already sets these two (to
# "droplet") — Set, not Add.
"$PB" -c "Set :CFBundleIconFile AppIcon" "$PLIST"
"$PB" -c "Set :CFBundleIconName AppIcon" "$PLIST"
# Replace osacompile's own generic "accept anything dragged onto the icon"
# declaration with one that also makes this app choosable via Finder's
# per-file Open With for a markdown/canvas file specifically. Alternate
# rank keeps it from silently becoming the default.
"$PB" -c "Delete :CFBundleDocumentTypes" "$PLIST"
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

# Warm up LaunchServices' trust for this app's own type claims (see prose
# above) — no document, so `on run` fires; the env var tells it to skip
# the `meshfox --version` dialog this one time.
open -g -a "$APP" --env MESHFOX_CANVAS_OPENER_WARMUP=1 >/dev/null 2>&1 || true
sleep 1

echo "Installed $APP"
echo "Not yet the default for anything — pick it by hand via Finder's"
echo "Get Info -> Open with, one file at a time, until that's tackled."
```

## Package for sharing
<!-- meshfox:node id="package" -->

Rebuilds fresh via `build` (so what you hand out always matches what
`src/` currently says, not some earlier install) and zips
`~/Applications/MeshfoxCanvas.app` into `~/Desktop/MeshfoxCanvas.zip`,
using `ditto` rather than a plain `zip -r` — that's also what Finder's own
"Compress" does, and unlike a naive `zip`, it preserves the bundle's
resource fork/extended attributes so `codesign`'s signature survives being
re-extracted on someone else's Mac.

Handing this to a friend is not the same as actually distributing it: it's
still only ad-hoc self-signed (`codesign -s -`), not notarized with a paid
Apple Developer ID, so their Mac's Gatekeeper will refuse to open it the
normal way the first time — "Apple could not verify ... is free of
malware" or similar. They need to right-click → Open once (or System
Settings → Privacy & Security → Open Anyway after the first refusal), same
as any other unsigned indie tool; after that one confirmation it opens
normally from then on. If they don't have `meshfox` on their own `PATH`
either, the app's own runtime check (`launcher.applescript`'s
`resolveMeshfox`) opens Terminal with the install command for them the
first time they try to use it, instead of just failing silently.

```bash deps="build/build" always
set -euo pipefail

APP="$HOME/Applications/MeshfoxCanvas.app"
OUT="$HOME/Desktop/MeshfoxCanvas.zip"

rm -f "$OUT"
ditto -c -k --sequesterRsrc --keepParent "$APP" "$OUT"

echo "Wrote $OUT"
echo "Whoever you send it to: right-click -> Open the first time (it's"
echo "ad-hoc signed, not notarized, so Gatekeeper will otherwise refuse it)."
```

## Uninstall
<!-- meshfox:node id="uninstall" -->

Removes the installed app and unregisters it.

```bash
set -euo pipefail

APP="$HOME/Applications/MeshfoxCanvas.app"

if [ -d "$APP" ]; then
  /System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister -u -f "$APP" || true
  rm -rf "$APP"
  echo "Removed $APP"
else
  echo "$APP not installed, nothing to do"
fi
```

