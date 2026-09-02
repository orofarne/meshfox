import AppKit
import Foundation

/// Same lookup `macos/src/launcher.applescript`'s own `resolveMeshfox`
/// already uses, and for the same reason: resolved fresh on every launch
/// (not baked in at build time) so a `meshfox` installed/upgraded after
/// this daemon was last built is still found, and `~/.local/bin` is
/// checked explicitly first since that's where meshfox's own install
/// script puts it — not on a fresh Mac's `PATH` by default.
///
/// `MESHFOX_BIN`, if set, wins over both — not meant for a normal launch,
/// just for developing/testing this daemon against a `target/debug`
/// build instead of whatever's actually installed.
func resolveMeshfoxPath() -> String? {
    if let override = ProcessInfo.processInfo.environment["MESHFOX_BIN"],
       FileManager.default.isExecutableFile(atPath: override)
    {
        return override
    }
    let localBin = NSHomeDirectory() + "/.local/bin/meshfox"
    if FileManager.default.isExecutableFile(atPath: localBin) {
        return localBin
    }
    let pathEnv = ProcessInfo.processInfo.environment["PATH"] ?? ""
    for dir in pathEnv.split(separator: ":") {
        let candidate = "\(dir)/meshfox"
        if FileManager.default.isExecutableFile(atPath: candidate) {
            return candidate
        }
    }
    return nil
}

/// `~/Library/Application Support/meshfox/daemon.sock` — the well-known
/// socket path a future `meshfox open` (unlike `meshfox view`'s own
/// private per-invocation one) connects to directly, no bootstrap-race
/// needed since this app itself is the only thing that ever creates it.
/// `Application Support` (not `Caches`/`../tmp`) since this is meant to be
/// long-lived for as long as the daemon runs, same idiom any other
/// per-user macOS app data lives under.
func defaultSocketPath() -> String {
    NSHomeDirectory() + "/Library/Application Support/meshfox/daemon.sock"
}

guard let meshfoxPath = resolveMeshfoxPath() else {
    FileHandle.standardError.write(
        "meshfox-daemon: couldn't find the meshfox binary (checked ~/.local/bin and PATH)\n"
            .data(using: .utf8)!
    )
    exit(1)
}

let app = NSApplication.shared
let delegate = AppDelegate(meshfoxPath: meshfoxPath, socketPath: defaultSocketPath())
app.delegate = delegate
// Menu-bar-only — no Dock icon, no app switcher entry. Launching with no
// arguments just starts the daemon; it never opens an initial canvas of
// its own (contrast `meshfox view`'s private watcher) — every session it
// ever tracks comes from an explicit `Open` request, whether from a
// worker's own cross-canvas navigation or (eventually) `meshfox open`.
app.setActivationPolicy(.accessory)

// `kill <pid>`/Activity Monitor "Quit"/force-quit all bypass the "Quit"
// menu item entirely — without this, any of them would leave every
// tracked worker orphaned, the exact "dangling background process"
// outcome the whole design is meant to avoid (mirrors
// `crate::watcher::wait_for_terminate`'s own reasoning on the CLI side).
// `DispatchSourceSignal`, not a raw `signal()` handler: it delivers the
// notification through the normal run loop instead of inside actual
// signal-handler context, so it's safe to run arbitrary Swift/Cocoa code
// (`AppDelegate.shutdown`) in response. `signal(_, SIG_IGN)` first is
// required — a dispatch signal source only *observes* a signal that the
// process would otherwise still act on by default (which for `SIGTERM`/
// `SIGINT` is "terminate immediately", pre-empting the source's own
// handler from ever running).
var signalSources: [DispatchSourceSignal] = []
for sig in [SIGTERM, SIGINT] {
    Foundation.signal(sig, SIG_IGN)
    let source = DispatchSource.makeSignalSource(signal: sig, queue: .main)
    source.setEventHandler { delegate.shutdown() }
    source.resume()
    signalSources.append(source)
}

app.run()
