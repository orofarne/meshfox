import Foundation
#if canImport(Darwin)
import Darwin
#endif
import CLaunch

/// The `Sockets` dictionary key this app's own LaunchAgent plist
/// (`macos/app.canvas.md`'s `install-launch-agent` node) declares its
/// socket entry under — must match exactly, it's how `launch_activate_socket`
/// finds the right one. Arbitrary otherwise; not the socket *path* itself
/// (that's still `defaultSocketPath()` in `main.swift`, which the plist
/// also names).
private let launchSocketName = "Listener"

/// A persistent, well-known Unix domain socket listener — the daemon's own
/// counterpart to `crates/cli/src/watcher.rs`'s private per-invocation one.
/// Plain POSIX `socket`/`bind`/`listen`/`accept`, not `Network.framework`'s
/// higher-level API: this is the one piece everything else depends on
/// working correctly, so it's built on the C sockets API directly rather
/// than a less-thoroughly-exercised (from this codebase's own experience)
/// abstraction over it.
///
/// One background thread blocks in `accept()`; each accepted connection is
/// handled on the global concurrent queue, reading exactly one
/// newline-delimited line (matches `watcher_protocol::send`'s own
/// one-shot-then-shutdown write side) before closing. `onLine` fires once
/// per accepted connection with that raw line, off the main thread —
/// callers that touch UI state must hop back to `DispatchQueue.main`
/// themselves (see `SessionStore`).
///
/// `get_port` is the one request that gets a reply on the same connection
/// (see `Protocol.swift`'s own doc comment on `WatcherMessage.getPort`) —
/// `onGetPort` is called instead of `onLine` for that one op, *without*
/// waiting for the peer's own EOF first (the Rust client,
/// `watcher_protocol::request_port`, keeps its write half open to read the
/// reply, so it never sends one): `handleClient` blocks its own background
/// thread on `onGetPort`'s completion, then writes the reply and closes.
final class UnixSocketServer {
    private let path: String
    private let onLine: (String) -> Void
    private let onGetPort: (String, @escaping (PortReply) -> Void) -> Void
    private var listenFD: Int32 = -1
    /// Whether `listenFD` is a socket this instance itself created (own
    /// `path` file, safe — and necessary — to `unlink` in `stop()`) versus
    /// one inherited from launchd via socket activation (launchd's own
    /// socket, tied to the LaunchAgent's lifetime, not this run's — see
    /// `stop()`'s own doc comment for why unlinking that one would be
    /// actively harmful).
    private var ownsSocketFile = false

    init(
        path: String,
        onLine: @escaping (String) -> Void,
        onGetPort: @escaping (String, @escaping (PortReply) -> Void) -> Void
    ) {
        self.path = path
        self.onLine = onLine
        self.onGetPort = onGetPort
    }

    enum ServerError: Error, CustomStringConvertible {
        case pathTooLong(String)
        case socketFailed(String)
        case bindFailed(String)
        case listenFailed(String)

        var description: String {
            switch self {
            case .pathTooLong(let p): return "socket path too long for sockaddr_un: \(p)"
            case .socketFailed(let e): return "socket() failed: \(e)"
            case .bindFailed(let e): return "bind() failed: \(e)"
            case .listenFailed(let e): return "listen() failed: \(e)"
            }
        }
    }

    private static func errnoString() -> String {
        String(cString: strerror(errno))
    }

    /// Starts listening, then returns immediately — the accept loop runs on
    /// its own background thread. Two ways this can happen: inheriting an
    /// already-bound-and-listening socket launchd created for us (socket
    /// activation — see `activatedSocketFD`), or binding one ourselves the
    /// old way, when this process isn't running under launchd's management
    /// at all (a plain `swift run`, or `open -a` before the LaunchAgent is
    /// installed). Tries the former first; only self-binds as a fallback.
    func start() throws {
        if let fd = activatedSocketFD() {
            listenFD = fd
            ownsSocketFile = false
            startAcceptLoop()
            return
        }
        try startBoundSocket()
    }

    /// Inherits a socket launchd already bound for this LaunchAgent's own
    /// `Sockets.\(launchSocketName)` entry (`macos/app.canvas.md`'s
    /// `install-launch-agent` node) — `nil` (not an error) whenever this
    /// process isn't running under launchd's management at all, which is
    /// the ordinary case for a `swift run`/manual `open -a` launch outside
    /// the installed LaunchAgent. Every documented failure code (`ENOENT`:
    /// no such socket declared for this job; `ESRCH`: not managed by
    /// launchd at all; `EALREADY`: already activated once) means exactly
    /// "nothing to inherit" here, not a real error — `start()`'s own
    /// fallback to binding one itself is the correct response to all three.
    private func activatedSocketFD() -> Int32? {
        // The C signature wants `int * _Nonnull *` — a pointer to a
        // non-optional `UnsafeMutablePointer<Int32>`. An
        // implicitly-unwrapped `var fds: UnsafeMutablePointer<Int32>!`
        // looks like it should bridge to that directly, but doesn't:
        // confirmed live (a real crash report, not theoretical) — taking
        // `&fds` there force-unwraps its *current* value (nil, before the
        // call has run at all) immediately, trapping with "Unexpectedly
        // found nil" before `launch_activate_socket` is ever even reached.
        // `Optional<UnsafeMutablePointer<Int32>>` and a bare
        // `UnsafeMutablePointer<Int32>` share the exact same memory layout
        // in Swift (pointer optionals cost nothing extra — nil *is* the
        // null pointer bit pattern), so rebinding a pointer to the
        // optional's storage as if it pointed to the non-optional type is
        // safe and is the standard way to bridge this: no eager unwrap, no
        // dummy initial value needed.
        var fdsOpt: UnsafeMutablePointer<Int32>?
        var count = 0
        let rc = withUnsafeMutablePointer(to: &fdsOpt) { optPtr -> Int32 in
            optPtr.withMemoryRebound(to: UnsafeMutablePointer<Int32>.self, capacity: 1) { nonOptPtr in
                launchSocketName.withCString { namePtr in
                    launch_activate_socket(namePtr, nonOptPtr, &count)
                }
            }
        }
        guard rc == 0, count > 0, let fds = fdsOpt else { return nil }
        defer { free(fds) }
        return fds[0]
    }

    private func startAcceptLoop() {
        let thread = Thread { [weak self] in
            self?.acceptLoop()
        }
        thread.name = "meshfox-daemon-socket-accept"
        thread.start()
    }

    /// The pre-socket-activation path, kept as the fallback for whenever
    /// nothing was inherited: binds `path` ourselves. A stale socket file
    /// left behind by an unclean previous exit is removed first (nothing
    /// can be listening behind a leftover file from a process that's
    /// already gone), same reasoning the old `view_registry`'s `serve()`
    /// had for its own bind.
    private func startBoundSocket() throws {
        unlink(path) // best-effort; ENOENT if it never existed is fine

        let fd = socket(AF_UNIX, SOCK_STREAM, 0)
        guard fd >= 0 else { throw ServerError.socketFailed(Self.errnoString()) }

        var addr = sockaddr_un()
        addr.sun_family = sa_family_t(AF_UNIX)
        let pathBytes = Array(path.utf8)
        let capacity = MemoryLayout.size(ofValue: addr.sun_path)
        guard pathBytes.count < capacity else {
            close(fd)
            throw ServerError.pathTooLong(path)
        }
        withUnsafeMutableBytes(of: &addr.sun_path) { raw in
            let buf = raw.bindMemory(to: UInt8.self)
            for (i, byte) in pathBytes.enumerated() { buf[i] = byte }
            // Everything else in `addr` (including the rest of `sun_path`)
            // is already zero from `sockaddr_un()`'s own default init, so
            // the string is implicitly null-terminated.
        }

        let bindResult = withUnsafePointer(to: &addr) { ptr -> Int32 in
            ptr.withMemoryRebound(to: sockaddr.self, capacity: 1) { sockPtr in
                bind(fd, sockPtr, socklen_t(MemoryLayout<sockaddr_un>.size))
            }
        }
        guard bindResult == 0 else {
            let message = Self.errnoString()
            close(fd)
            throw ServerError.bindFailed(message)
        }

        guard listen(fd, 16) == 0 else {
            let message = Self.errnoString()
            close(fd)
            throw ServerError.listenFailed(message)
        }

        listenFD = fd
        ownsSocketFile = true
        startAcceptLoop()
    }

    private func acceptLoop() {
        while true {
            let clientFD = accept(listenFD, nil, nil)
            if clientFD < 0 {
                // EINTR: a signal interrupted the call, just retry. Any
                // other error (most likely EBADF, from `stop()` closing
                // the listening socket out from under this loop) means
                // there's nothing left to accept.
                if errno == EINTR { continue }
                break
            }
            DispatchQueue.global(qos: .utility).async { [weak self] in
                self?.handleClient(clientFD)
            }
        }
    }

    /// Reads until the peer closes its own write side (EOF), *not* just
    /// until the first newline — closing our own end the instant a line
    /// is found raced the Rust client's own `stream.shutdown()` call right
    /// after its `write_all` (`watcher_protocol::send`): closing first
    /// from this side sometimes made that `shutdown()` fail with ENOTCONN
    /// on a real Unix domain socket, even though the write itself had
    /// already fully landed — reproduced directly against a real
    /// `meshfox view --watcher-socket` worker, not theoretical. Waiting
    /// for the peer to finish on its own terms before this side closes
    /// avoids the race entirely; the line is decoded once EOF arrives, but
    /// found (and remembered) as soon as it shows up in the buffer.
    private func handleClient(_ fd: Int32) {
        defer { close(fd) }
        var data = Data()
        var buf = [UInt8](repeating: 0, count: 4096)
        var foundLine: String?
        while true {
            let n = buf.withUnsafeMutableBytes { rawBuf -> Int in
                read(fd, rawBuf.baseAddress, rawBuf.count)
            }
            if n <= 0 { break } // EOF or error — the peer is done
            data.append(contentsOf: buf[0..<n])
            if foundLine == nil, let newlineIndex = data.firstIndex(of: 0x0A) {
                let lineData = data[data.startIndex..<newlineIndex]
                foundLine = String(data: lineData, encoding: .utf8)
                // `get_port` alone gets a reply, and its own client never
                // shuts down its write half waiting for one — looping on
                // for an EOF that's never coming would just hang this
                // thread. Every other op still falls through to the
                // EOF-then-`onLine` path below, unchanged.
                if let line = foundLine, case let .getPort(canvasPath)? = WatcherMessage.parse(line: line) {
                    replyToGetPort(fd: fd, canvasPath: canvasPath)
                    return
                }
            }
        }
        if let line = foundLine {
            onLine(line)
        }
    }

    /// Blocks this background thread on `onGetPort`'s completion (itself
    /// possibly async — see `SessionStore.getPort`, which may need to wait
    /// for a freshly-spawned worker's own `Ready`), then writes the one
    /// JSON reply line and returns, closing the connection.
    private func replyToGetPort(fd: Int32, canvasPath: String) {
        let semaphore = DispatchSemaphore(value: 0)
        var reply: PortReply = .error("no response")
        onGetPort(canvasPath) { result in
            reply = result
            semaphore.signal()
        }
        semaphore.wait()
        guard var replyData = try? JSONEncoder().encode(reply) else { return }
        replyData.append(0x0A)
        replyData.withUnsafeBytes { raw in
            _ = write(fd, raw.baseAddress, raw.count)
        }
    }

    /// Stops accepting new connections. Only removes the socket file when
    /// this instance bound it itself (`ownsSocketFile`) — a launchd-
    /// activated socket belongs to the LaunchAgent's own lifetime, not this
    /// one run's: unlinking it here would break launchd's own ability to
    /// on-demand-relaunch this app on the *next* connection (a fresh
    /// `connect()` needs the pathname to still resolve to the socket
    /// launchd is holding open on our behalf), turning a clean "Quit" into
    /// "nobody can ever reach this again until the LaunchAgent itself is
    /// reloaded." Called on every shutdown path (`AppDelegate.shutdown`),
    /// not just tests, now that getting this wrong would actually matter.
    func stop() {
        if listenFD >= 0 {
            close(listenFD)
            listenFD = -1
        }
        if ownsSocketFile {
            unlink(path)
        }
    }
}
