import AppKit
import Foundation

/// Whether a browser tab should be opened for a session the moment its
/// port becomes known — mirrors `crates/cli/src/watcher.rs`'s own
/// `Entry.pending_open: Option<Option<String>>` (outer "wanted at all",
/// inner "with this fragment").
enum PendingOpen {
    case none
    case wanted(fragment: String?)
}

/// One tracked worker: a `meshfox view <path> --watcher-socket <this
/// daemon's socket>` child process, plus whatever it's reported about
/// itself so far.
final class Session {
    let canvasPath: String
    var port: UInt16?
    var pendingOpen: PendingOpen
    let process: Process
    /// Every still-unanswered `get_port` request waiting on this session's
    /// own `Ready` — drained (each called exactly once) by `markReady`.
    /// Empty for a session nobody's asked `getPort` about yet.
    var pendingPortRequests: [(PortReply) -> Void] = []

    init(canvasPath: String, process: Process, pendingOpen: PendingOpen) {
        self.canvasPath = canvasPath
        self.process = process
        self.pendingOpen = pendingOpen
    }

    var displayTitle: String {
        (canvasPath as NSString).lastPathComponent
    }
}

/// The daemon's own registry — same role `crate::watcher::Registry` plays
/// for a private per-invocation watcher, just backed by a persistent,
/// well-known socket instead of a fresh one per launch, and living exactly
/// as long as the whole app does rather than exiting once empty (see this
/// package's own doc comment for why that's the deliberate difference
/// between the two: a menu-bar app is meant to be a visible, user-quittable
/// presence, not something that vanishes the moment its last tab closes).
final class SessionStore {
    private var sessions: [String: Session] = [:] // keyed by canonical canvas path
    private let lock = NSLock()
    private let socketPath: String
    private let meshfoxPath: String
    private var server: UnixSocketServer?

    /// How long a freshly spawned worker gets to report `Ready` before this
    /// store gives up on it — a real worker binding a port is fast (well
    /// under a second normally), so this is generous specifically so it
    /// never fires under ordinary load. Exists so a worker that never
    /// reports `Ready` at all (crashed before binding, or — the incident
    /// this was added for — a coordinator restart landing in a state where
    /// `Ready` never reaches `markReady`) fails every `getPort` caller
    /// waiting on it instead of leaving them (and, transitively, whatever
    /// `meshfox` CLI invocation is blocked in `request_port` on the other
    /// end) hanging forever. Shorter than that client-side timeout
    /// (`REQUEST_PORT_TIMEOUT`, `crates/server/src/watcher_protocol.rs`)
    /// so *this* is normally what answers first.
    private let getPortTimeoutSeconds: TimeInterval = 15

    /// Fired (always on the main queue) whenever `sessions` changes —
    /// `AppDelegate` rebuilds the menu from `allSessions` in response.
    var onChange: (() -> Void)?

    init(socketPath: String, meshfoxPath: String) {
        self.socketPath = socketPath
        self.meshfoxPath = meshfoxPath
    }

    func start() throws {
        let dir = (socketPath as NSString).deletingLastPathComponent
        try FileManager.default.createDirectory(atPath: dir, withIntermediateDirectories: true)

        let server = UnixSocketServer(
            path: socketPath,
            onLine: { [weak self] line in
                self?.handleLine(line)
            },
            onGetPort: { [weak self] canvasPath, completion in
                self?.getPort(path: canvasPath, completion: completion)
                    ?? completion(.error("daemon is shutting down"))
            }
        )
        try server.start()
        self.server = server
    }

    /// Tidies up the socket file on a graceful shutdown (Quit menu item,
    /// or `SIGTERM`/`SIGINT` — see `AppDelegate.shutdown`). Not load-
    /// bearing for correctness either way: `start()` already unlinks a
    /// stale socket file left over from an unclean exit before binding.
    func stop() {
        server?.stop()
    }

    private func handleLine(_ line: String) {
        guard let message = WatcherMessage.parse(line: line) else { return }
        switch message {
        case .ready(let canvasPath, let port):
            markReady(canvasPath: canvasPath, port: port)
        case .open(let canvasPath, let fragment):
            openCanvas(path: canvasPath, fragment: fragment)
        case .openFile(let path):
            openFile(path: path)
        case .getPort:
            // Never actually reaches here — `UnixSocketServer` intercepts
            // `get_port` itself and calls `getPort(path:completion:)`
            // directly, since (unlike everything else routed through
            // `onLine`) it needs a reply on the same connection. Kept only
            // so this switch stays exhaustive.
            break
        }
    }

    /// "Get-or-spawn a worker for `canvasPath`, tell me its port" —
    /// `UnixSocketServer`'s own `get_port` handling calls this directly
    /// (not via `handleLine`) since it needs the reply `completion`
    /// provides. Never opens a browser tab (`pendingOpen: .none`) — that's
    /// `openCanvas`'s own job. `completion` may run synchronously (a port's
    /// already known) or later, from `markReady` (still spawning) —
    /// exactly once either way.
    func getPort(path canvasPath: String, completion: @escaping (PortReply) -> Void) {
        let canonical = Self.canonicalize(canvasPath)

        lock.lock()
        if let existing = sessions[canonical] {
            if let port = existing.port {
                lock.unlock()
                completion(.port(port))
            } else {
                existing.pendingPortRequests.append(completion)
                lock.unlock()
            }
            return
        }
        lock.unlock()

        spawnWorker(canonicalPath: canonical, pendingOpen: .none, initialPortRequest: completion)
    }

    /// A worker's own `Ready` arrived — record its port, and open a tab
    /// now if anything was waiting on it. Mirrors `Registry::mark_ready`
    /// on the Rust side exactly, including *why* `path` is trusted as
    /// already canonical: it's echoed straight back from whatever this
    /// store itself passed the worker as an argument when spawning it.
    private func markReady(canvasPath: String, port: UInt16) {
        lock.lock()
        let session = sessions[canvasPath]
        session?.port = port
        let pending = session?.pendingOpen
        let portRequests = session?.pendingPortRequests ?? []
        session?.pendingPortRequests = []
        lock.unlock()
        if case .wanted(let fragment) = pending {
            openBrowserTab(port: port, fragment: fragment)
        }
        for completion in portRequests {
            completion(.port(port))
        }
        notifyChange()
    }

    /// "Show the user `canvasPath`" — the one path every source of that
    /// request funnels through: a worker's own cross-canvas "↗ open"
    /// (over the socket, from `handleLine`), `meshfox view`'s own
    /// `server_socket` hand-off (also over the socket — it's just another
    /// client of the exact same protocol), and
    /// Finder handing this app a file directly (`AppDelegate.application(_:open:)`,
    /// called in-process — no socket hop needed since we're already inside
    /// the one process that owns `SessionStore`). Same three-way dispatch
    /// as `crate::watcher::handle_connection`'s own `Open` handling either
    /// way: already open → show now; already spawning → just flag it
    /// wanted; never seen → spawn it, wanted from the start.
    func openCanvas(path canvasPath: String, fragment: String?) {
        let canonical = Self.canonicalize(canvasPath)

        lock.lock()
        if let existing = sessions[canonical] {
            if let port = existing.port {
                lock.unlock()
                openBrowserTab(port: port, fragment: fragment)
            } else {
                existing.pendingOpen = .wanted(fragment: fragment)
                lock.unlock()
            }
            return
        }
        lock.unlock()

        spawnWorker(canonicalPath: canonical, pendingOpen: .wanted(fragment: fragment), initialPortRequest: nil)
    }

    /// A "↗ open" on a plain (non-canvas) file node's target — this
    /// daemon's own answer is the OS's default application for it, same as
    /// `crate::watcher::open_plain_file` on the CLI's own private watcher
    /// (unlike the VS Code extension's coordinator, there's no in-app tab
    /// concept to prefer here). No session bookkeeping: unlike a canvas,
    /// there's no port to wait for and nothing worth tracking afterwards.
    func openFile(path: String) {
        DispatchQueue.main.async {
            NSWorkspace.shared.open(URL(fileURLWithPath: path))
        }
    }

    private static func canonicalize(_ path: String) -> String {
        URL(fileURLWithPath: path).resolvingSymlinksInPath().path
    }

    /// `port: 0` (let the OS pick) and auto-exit left on (no
    /// `--no-auto-exit`) — every daemon-spawned worker exits on its own
    /// once its own browser tabs all close, same defaults
    /// `crate::watcher::spawn_worker` always uses for a navigated-to
    /// worker. There's no "primary, don't auto-exit" case here at all —
    /// unlike the CLI's own private watcher, this daemon never starts with
    /// an initial canvas of its own (see `main.swift`); every session it
    /// ever has came from an explicit `Open`.
    private func spawnWorker(
        canonicalPath: String,
        pendingOpen: PendingOpen,
        initialPortRequest: ((PortReply) -> Void)?
    ) {
        let process = Process()
        process.executableURL = URL(fileURLWithPath: meshfoxPath)
        process.arguments = ["view", canonicalPath, "--port", "0", "--watcher-socket", socketPath]
        process.standardInput = FileHandle.nullDevice

        let session = Session(canvasPath: canonicalPath, process: process, pendingOpen: pendingOpen)
        if let initialPortRequest {
            session.pendingPortRequests = [initialPortRequest]
        }
        process.terminationHandler = { [weak self] _ in
            self?.remove(canonicalPath: canonicalPath)
        }

        lock.lock()
        sessions[canonicalPath] = session
        lock.unlock()
        notifyChange()

        do {
            try process.run()
            DispatchQueue.global(qos: .utility).asyncAfter(deadline: .now() + getPortTimeoutSeconds) { [weak self] in
                self?.failIfStillPending(canonicalPath: canonicalPath)
            }
        } catch {
            lock.lock()
            sessions.removeValue(forKey: canonicalPath)
            lock.unlock()
            initialPortRequest?(.error("couldn't spawn a worker: \(error.localizedDescription)"))
            notifyChange()
        }
    }

    /// Fires `getPortTimeoutSeconds` after a worker was spawned — a no-op
    /// if it already reported `Ready` (or already exited) by then. If it's
    /// still sitting at `port == nil`, this worker is stuck (crashed
    /// silently past the point that would call `remove`, or — the incident
    /// this exists for — the whole coordinator's own accept path is
    /// wedged and `Ready` can never reach `markReady` no matter how long
    /// anyone waits): kill it and fail everyone still waiting on its port
    /// with a clear reason instead of leaving them blocked forever. Only
    /// clears `pendingPortRequests`, not the whole session — the worker's
    /// own `terminationHandler` still runs once `terminate()` actually
    /// takes effect and does the real `sessions` cleanup via `remove`,
    /// same as any other worker that exits.
    private func failIfStillPending(canonicalPath: String) {
        lock.lock()
        guard let session = sessions[canonicalPath], session.port == nil else {
            lock.unlock()
            return
        }
        let portRequests = session.pendingPortRequests
        session.pendingPortRequests = []
        lock.unlock()

        session.process.terminate()
        for completion in portRequests {
            completion(.error(
                "worker for \(canonicalPath) didn't report ready within \(Int(getPortTimeoutSeconds))s — killed it"
            ))
        }
    }

    /// A session's worker is gone — normal exit (its own tabs all closed)
    /// or a crash before ever reporting `Ready`. Either way, nobody's
    /// waiting `getPort` on it should be left blocked forever
    /// (`UnixSocketServer.replyToGetPort`'s semaphore has no other way to
    /// wake up) — fail every still-pending request explicitly rather than
    /// silently dropping them. Also reached (with an already-empty
    /// `pendingPortRequests`) after `failIfStillPending` kills a stuck
    /// worker, once its `terminationHandler` actually fires.
    private func remove(canonicalPath: String) {
        lock.lock()
        let portRequests = sessions[canonicalPath]?.pendingPortRequests ?? []
        sessions.removeValue(forKey: canonicalPath)
        lock.unlock()
        for completion in portRequests {
            completion(.error("worker exited before reporting a port"))
        }
        notifyChange()
    }

    /// Kills one session's worker (`SIGTERM`, same as `Process.terminate()`
    /// always sends) — its own `terminationHandler` removes it from the
    /// registry once it's actually gone, same path a worker exiting on its
    /// own (all its tabs closed) already takes.
    func kill(canvasPath: String) {
        lock.lock()
        let session = sessions[canvasPath]
        lock.unlock()
        session?.process.terminate()
    }

    /// "Quit" — kills every tracked worker. Doesn't wait for them to
    /// actually exit before returning (the caller terminates the whole
    /// app right after) — each one's own `kill_on_drop`-equivalent
    /// cleanup is `Process.terminate()` itself, not something this needs
    /// to await.
    func killAll() {
        lock.lock()
        let all = Array(sessions.values)
        lock.unlock()
        for session in all {
            session.process.terminate()
        }
    }

    var allSessions: [Session] {
        lock.lock()
        defer { lock.unlock() }
        return sessions.values.sorted { $0.displayTitle.localizedStandardCompare($1.displayTitle) == .orderedAscending }
    }

    /// `http://127.0.0.1:<port>/[#fragment]`, best-effort — same
    /// reasoning `crate::watcher::open_browser_tab` already documents: no
    /// browser, no display, or a broken default-app association shouldn't
    /// be fatal to anything here either.
    private func openBrowserTab(port: UInt16, fragment: String?) {
        var urlString = "http://127.0.0.1:\(port)/"
        if let fragment = fragment {
            urlString += "#\(fragment)"
        }
        guard let url = URL(string: urlString) else { return }
        DispatchQueue.main.async {
            NSWorkspace.shared.open(url)
        }
    }

    private func notifyChange() {
        DispatchQueue.main.async { [weak self] in
            self?.onChange?()
        }
    }
}
