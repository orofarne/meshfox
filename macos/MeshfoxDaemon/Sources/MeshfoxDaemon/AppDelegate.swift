import AppKit
import Foundation

final class AppDelegate: NSObject, NSApplicationDelegate {
    private var statusItem: NSStatusItem!
    private var store: SessionStore!
    private let meshfoxPath: String
    private let socketPath: String

    /// The user's CLI version, which can advance independently of the app.
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
        StartupSelfTest.run(socketPath: socketPath)

        for path in pendingOpenPaths {
            store.openCanvas(path: path, fragment: nil, completion: Self.logIfFailed)
        }
        pendingOpenPaths.removeAll()

        rebuildMenu()
        offerSocketConfigurationIfNeeded()
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
            store.openCanvas(path: url.path, fragment: nil, completion: Self.logIfFailed)
        }
    }

    /// Nobody's waiting over the socket for these two call sites' own
    /// `openCanvas` (Finder/startup opens, not a `watcher_protocol`
    /// request) — but a failure (the exact case this whole ack mechanism
    /// exists for — see `watcher_protocol.rs`'s own doc comment) shouldn't
    /// just vanish silently for them either, so it still lands in
    /// `daemon.log` where `UnixSocketServer`'s own failure logging already
    /// goes.
    private static func logIfFailed(_ reply: AckReply) {
        if case .error(let message) = reply {
            FileHandle.standardError.write("meshfox-daemon: couldn't open a Finder-requested file: \(message)\n".data(using: .utf8)!)
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

        let configureItem = NSMenuItem(title: "Connect CLI to Daemon…", action: #selector(configureCLI), keyEquivalent: "")
        configureItem.target = self
        menu.addItem(configureItem)

        let versionItem = NSMenuItem(title: "Meshfox Daemon \(Self.daemonVersion)", action: nil, keyEquivalent: "")
        versionItem.isEnabled = false
        menu.addItem(versionItem)

        let cliVersionItem = NSMenuItem(title: "meshfox CLI \(cliVersion)", action: nil, keyEquivalent: "")
        cliVersionItem.isEnabled = false
        menu.addItem(cliVersionItem)

        let updateItem = NSMenuItem(title: "Check CLI Updates…", action: #selector(checkForUpdates), keyEquivalent: "")
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

    private func offerSocketConfigurationIfNeeded() {
        let defaults = UserDefaults.standard
        guard !defaults.bool(forKey: "didOfferSocketConfiguration") else { return }
        do {
            guard try !SocketConfiguration.hasServerSocket() else { return }
        } catch {
            FileHandle.standardError.write("meshfox-daemon: couldn't inspect CLI configuration: \(error)\n".data(using: .utf8)!)
            return
        }
        defaults.set(true, forKey: "didOfferSocketConfiguration")
        DispatchQueue.main.async { [weak self] in self?.configureCLI() }
    }

    @objc private func configureCLI() {
        do {
            if try SocketConfiguration.hasServerSocket() {
                let alert = NSAlert()
                alert.messageText = "Meshfox CLI is already configured"
                alert.informativeText = "A server_socket setting exists in ~/.meshfox/config.toml. Meshfox left it unchanged."
                alert.runModal()
                return
            }
        } catch {
            showConfigurationError(error)
            return
        }

        let alert = NSAlert()
        alert.messageText = "Connect Meshfox CLI to this daemon?"
        alert.informativeText = "Add server_socket to ~/.meshfox/config.toml for this account? CLI commands will use this daemon's socket at \(socketPath). Existing settings will be kept."
        alert.addButton(withTitle: "Connect")
        alert.addButton(withTitle: "Not Now")
        guard alert.runModal() == .alertFirstButtonReturn else { return }
        do {
            try SocketConfiguration.configure(socketPath: socketPath)
        } catch {
            showConfigurationError(error)
        }
    }

    private func showConfigurationError(_ error: Error) {
        let alert = NSAlert()
        alert.alertStyle = .warning
        alert.messageText = "Couldn't configure Meshfox CLI"
        alert.informativeText = "\(error)\n\nYou can add server_socket to ~/.meshfox/config.toml manually."
        alert.runModal()
    }

    /// The CLI lives in the user's home, so its existing self-update
    /// mechanism can replace it without touching the signed app bundle.
    @objc private func checkForUpdates() {
        let path = meshfoxPath
        DispatchQueue.global(qos: .userInitiated).async { [weak self] in
            let process = Process()
            process.executableURL = URL(fileURLWithPath: path)
            process.arguments = ["check-updates", "--yes"]
            let pipe = Pipe()
            process.standardOutput = pipe
            process.standardError = pipe
            do {
                try process.run()
                let data = pipe.fileHandleForReading.readDataToEndOfFile()
                process.waitUntilExit()
                let output = String(data: data, encoding: .utf8)?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
                DispatchQueue.main.async {
                    self?.refreshCliVersion()
                    self?.rebuildMenu()
                    let alert = NSAlert()
                    alert.messageText = process.terminationStatus == 0 ? "Meshfox CLI update" : "Meshfox CLI update failed"
                    alert.informativeText = (output.isEmpty ? "(no output)" : output)
                        + "\n\nNew workers use the updated CLI. Restart existing canvas sessions to switch them over."
                    alert.runModal()
                }
            } catch {
                DispatchQueue.main.async {
                    let alert = NSAlert()
                    alert.alertStyle = .warning
                    alert.messageText = "Couldn't run meshfox check-updates"
                    alert.informativeText = "\(error)"
                    alert.runModal()
                }
            }
        }
    }

    /// Runs `meshfox --version` and stores the trimmed output in
    /// `cliVersion`. Synchronous (`waitUntilExit`) — always called from
    /// somewhere that's already fine blocking briefly on a fast local
    /// binary (at launch or after an update), never from `acceptLoop`'s background
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
