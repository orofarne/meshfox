import Foundation
#if canImport(Darwin)
import Darwin
#endif

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
final class UnixSocketServer {
    private let path: String
    private let onLine: (String) -> Void
    private var listenFD: Int32 = -1

    init(path: String, onLine: @escaping (String) -> Void) {
        self.path = path
        self.onLine = onLine
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

    /// Binds and starts listening, then returns immediately — the accept
    /// loop runs on its own background thread. A stale socket file left
    /// behind by an unclean previous exit is removed first (nothing can be
    /// listening behind a leftover file from a process that's already
    /// gone), same reasoning the old `view_registry`'s `serve()` had for
    /// its own bind.
    func start() throws {
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

        let thread = Thread { [weak self] in
            self?.acceptLoop()
        }
        thread.name = "meshfox-daemon-socket-accept"
        thread.start()
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
            }
        }
        if let line = foundLine {
            onLine(line)
        }
    }

    /// Stops accepting new connections and removes the socket file. Not
    /// called on the "Quit" path today (the whole process exits right
    /// after, which cleans the fd up anyway) — kept for symmetry/tests.
    func stop() {
        if listenFD >= 0 {
            close(listenFD)
            listenFD = -1
        }
        unlink(path)
    }
}
