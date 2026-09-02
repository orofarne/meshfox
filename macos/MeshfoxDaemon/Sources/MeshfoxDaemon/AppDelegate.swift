import AppKit
import Foundation

final class AppDelegate: NSObject, NSApplicationDelegate {
    private var statusItem: NSStatusItem!
    private var store: SessionStore!
    private let meshfoxPath: String
    private let socketPath: String

    /// The `meshfox` CLI binary's own version — distinct from
    /// `Self.daemonVersion` (this app's own). Shown right underneath it in
    /// the menu specifically because "Check for Updates" updates the CLI,
    /// not this app itself (see `checkForUpdates`'s own doc comment) — a
    /// menu that only ever showed the daemon's version would leave that
    /// update invisible. Re-read at launch and again once a
    /// `checkForUpdates` run finishes, not on every menu rebuild: it can
    /// only change from those two events, so there's no reason to shell out
    /// more often than that.
    private var cliVersion: String = "…"

    /// Finder's own "open documents" Apple Event (double-click/drag-onto-
    /// icon/"Open With" on a `.canvas.md`, once this app is bundled with
    /// the document-type claim that makes it choosable there — same
    /// `net.daringfireball.markdown`-as-Alternate-rank trick
    /// `macos/canvas-opener.canvas.md` already worked out, since
    /// LaunchServices resolves a file's type from that Apple-internal
    /// claim before any third-party extension list) can arrive via
    /// `application(_:open:)` *before* `applicationDidFinishLaunching` has
    /// run `store.start()` — buffered here and flushed once it has, rather
    /// than assuming a delivery order Cocoa doesn't actually guarantee.
    private var pendingOpenPaths: [String] = []

    init(meshfoxPath: String, socketPath: String) {
        self.meshfoxPath = meshfoxPath
        self.socketPath = socketPath
    }

    func applicationDidFinishLaunching(_ notification: Notification) {
        statusItem = NSStatusBar.system.statusItem(withLength: NSStatusItem.squareLength)
        statusItem.button?.title = "🦊"
        statusItem.button?.toolTip = "Meshfox"

        refreshCliVersion()

        store = SessionStore(socketPath: socketPath, meshfoxPath: meshfoxPath)
        store.onChange = { [weak self] in self?.rebuildMenu() }
        do {
            try store.start()
        } catch {
            let alert = NSAlert()
            alert.alertStyle = .critical
            alert.messageText = "Meshfox daemon couldn't start"
            alert.informativeText = "\(error)\n\nSocket: \(socketPath)"
            alert.runModal()
            NSApplication.shared.terminate(nil)
            return
        }

        for path in pendingOpenPaths {
            store.openCanvas(path: path, fragment: nil)
        }
        pendingOpenPaths.removeAll()

        rebuildMenu()
    }

    /// Finder's "open documents" Apple Event — a double-click, drag onto
    /// the Dock/Finder icon, or "Open With" on a `.canvas.md` (or marker-
    /// carrying `.md`). Each `url` becomes an ordinary `openCanvas` call,
    /// same as any other source of an "open this" request — see
    /// `SessionStore.openCanvas`'s own doc comment.
    func application(_ application: NSApplication, open urls: [URL]) {
        guard store != nil else {
            pendingOpenPaths.append(contentsOf: urls.map(\.path))
            return
        }
        for url in urls {
            store.openCanvas(path: url.path, fragment: nil)
        }
    }

    private func rebuildMenu() {
        let menu = NSMenu()

        let sessions = store.allSessions
        if sessions.isEmpty {
            let item = NSMenuItem(title: "No open canvases", action: nil, keyEquivalent: "")
            item.isEnabled = false
            menu.addItem(item)
        } else {
            for session in sessions {
                let title = session.port == nil ? "\(session.displayTitle) (starting…)" : session.displayTitle
                let item = NSMenuItem(title: title, action: #selector(openSession(_:)), keyEquivalent: "")
                item.target = self
                item.representedObject = session.canvasPath
                item.toolTip = session.canvasPath
                item.isEnabled = session.port != nil

                let submenu = NSMenu()
                let openItem = NSMenuItem(title: "Open", action: #selector(openSession(_:)), keyEquivalent: "")
                openItem.target = self
                openItem.representedObject = session.canvasPath
                openItem.isEnabled = session.port != nil
                let killItem = NSMenuItem(title: "Kill", action: #selector(killSession(_:)), keyEquivalent: "")
                killItem.target = self
                killItem.representedObject = session.canvasPath
                submenu.addItem(openItem)
                submenu.addItem(killItem)
                item.submenu = submenu

                menu.addItem(item)
            }
        }

        menu.addItem(NSMenuItem.separator())

        let versionItem = NSMenuItem(title: "Meshfox Daemon \(Self.daemonVersion)", action: nil, keyEquivalent: "")
        versionItem.isEnabled = false
        menu.addItem(versionItem)

        let cliVersionItem = NSMenuItem(title: "meshfox CLI \(cliVersion)", action: nil, keyEquivalent: "")
        cliVersionItem.isEnabled = false
        menu.addItem(cliVersionItem)

        let updateItem = NSMenuItem(title: "Check for Updates…", action: #selector(checkForUpdates), keyEquivalent: "")
        updateItem.target = self
        menu.addItem(updateItem)

        menu.addItem(NSMenuItem.separator())

        let quitItem = NSMenuItem(title: "Quit Meshfox Daemon", action: #selector(quit), keyEquivalent: "q")
        quitItem.target = self
        menu.addItem(quitItem)

        statusItem.menu = menu
    }

    @objc private func openSession(_ sender: NSMenuItem) {
        guard let path = sender.representedObject as? String,
              let session = store.allSessions.first(where: { $0.canvasPath == path }),
              let port = session.port,
              let url = URL(string: "http://127.0.0.1:\(port)/")
        else { return }
        NSWorkspace.shared.open(url)
    }

    @objc private func killSession(_ sender: NSMenuItem) {
        guard let path = sender.representedObject as? String else { return }
        store.kill(canvasPath: path)
    }

    /// Shells out to the CLI's own already-built, already-tested update
    /// mechanism (`meshfox check-updates`, `self_update` against GitHub
    /// releases — `crates/cli/src/main.rs`) rather than reimplementing any
    /// of that here. `--yes`: this process has no TTY for the CLI's own
    /// interactive confirmation prompt to work against.
    @objc private func checkForUpdates() {
        let process = Process()
        process.executableURL = URL(fileURLWithPath: meshfoxPath)
        process.arguments = ["check-updates", "--yes"]
        let pipe = Pipe()
        process.standardOutput = pipe
        process.standardError = pipe
        process.terminationHandler = { [weak self] _ in
            let data = pipe.fileHandleForReading.readDataToEndOfFile()
            let output = String(data: data, encoding: .utf8)?.trimmingCharacters(in: .whitespacesAndNewlines)
            DispatchQueue.main.async {
                self?.refreshCliVersion()
                self?.rebuildMenu()
                let alert = NSAlert()
                alert.messageText = "Meshfox update check"
                alert.informativeText = (output?.isEmpty ?? true) ? "(no output)" : output!
                alert.runModal()
            }
        }
        do {
            try process.run()
        } catch {
            let alert = NSAlert()
            alert.alertStyle = .warning
            alert.messageText = "Couldn't run meshfox check-updates"
            alert.informativeText = "\(error)"
            alert.runModal()
        }
    }

    /// Runs `meshfox --version` and stores the trimmed output in
    /// `cliVersion`. Synchronous (`waitUntilExit`) — always called from
    /// somewhere that's already fine blocking briefly on a fast local
    /// binary (launch, or a `checkForUpdates` completion already hopped
    /// onto `DispatchQueue.main`), never from `acceptLoop`'s background
    /// thread directly.
    private func refreshCliVersion() {
        let process = Process()
        process.executableURL = URL(fileURLWithPath: meshfoxPath)
        process.arguments = ["--version"]
        let pipe = Pipe()
        process.standardOutput = pipe
        process.standardError = Pipe()
        do {
            try process.run()
            process.waitUntilExit()
            let data = pipe.fileHandleForReading.readDataToEndOfFile()
            if let text = String(data: data, encoding: .utf8)?.trimmingCharacters(in: .whitespacesAndNewlines),
               !text.isEmpty
            {
                cliVersion = text
            }
        } catch {
            cliVersion = "unknown"
        }
    }

    @objc private func quit() {
        shutdown()
    }

    /// Kills every tracked worker, then terminates this app — the "Quit"
    /// menu item's own action, but also called directly from `main.swift`'s
    /// `SIGTERM`/`SIGINT` handlers, so a `kill`/force-quit that bypasses
    /// the menu entirely still cascades to every worker instead of
    /// orphaning them. `store` is force-unwrapped: by the time either
    /// signal handler can fire, `applicationDidFinishLaunching` (which
    /// assigns it) has always already run.
    func shutdown() {
        store.killAll()
        store.stop()
        NSApplication.shared.terminate(nil)
    }

    /// The daemon app's *own* version — a separate concern from the
    /// `meshfox` CLI binary's version (`Self.daemonVersion` vs. whatever
    /// `meshfoxPath --version` reports), since this is a distinct binary
    /// with its own release cadence, not baked into the same build. Not
    /// wired to anything yet in the core-only MVP — a literal placeholder
    /// until this has its own real version scheme (see TODO.canvas.md).
    private static let daemonVersion = "0.1.0-core-mvp"
}
