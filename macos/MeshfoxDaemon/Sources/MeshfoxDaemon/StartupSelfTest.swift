import Foundation
#if canImport(Darwin)
import Darwin
#endif

/// A one-shot connectivity check against this daemon's own socket, run
/// shortly after `SessionStore.start()` succeeds — added after a real
/// incident where the daemon looked completely healthy from the outside
/// (menu responsive, status item alive, `store.start()` hadn't thrown) but
/// its socket's accept path was silently wedged: every client that
/// connected got accepted at the kernel level (`connect()` returned
/// immediately) yet never received a reply, hanging forever. A bare
/// `connect()`-only check would have missed that exact failure — it's
/// what several manual checks during that incident kept doing, and they
/// all reported "connected fine" right up until the real problem. Only a
/// full round trip through the *real* protocol (`get_port`, which always
/// gets a reply — see `watcher_protocol.rs`'s own doc comment — even when
/// it's a `{"error": ...}`) actually proves the accept-loop-to-reply path
/// is alive end to end.
///
/// Uses a canvas path that can never legitimately exist, purely so the
/// spawned worker fails fast (missing file) and this resolves in well
/// under a second in the healthy case — the tiny, self-cleaning "starting…"
/// blip this causes in the menu for that instant is an acceptable trade
/// for testing the *real* path instead of a synthetic one. Logs its result
/// to stderr (captured to `~/Library/Logs/Meshfox/daemon.log` — see
/// `macos/app.canvas.md`'s `build` node) rather than alerting the user:
/// this is a diagnostic breadcrumb for whoever's debugging a future
/// "the daemon looks stuck" report, not something to interrupt anyone
/// with on every ordinary, healthy launch.
enum StartupSelfTest {
    private static let sentinelCanvasPath = "/nonexistent/meshfox-daemon-startup-self-test.canvas.md"

    static func run(socketPath: String, timeoutSeconds: Int = 5) {
        DispatchQueue.global(qos: .utility).asyncAfter(deadline: .now() + 0.2) {
            let ok = getPortRoundTrip(socketPath: socketPath, timeoutSeconds: timeoutSeconds)
            let message = ok
                ? "meshfox-daemon: startup self-test ok (socket accept/reply path is alive)\n"
                : "meshfox-daemon: STARTUP SELF-TEST FAILED — get_port got no reply within \(timeoutSeconds)s; the socket accept/reply path may be wedged (see StartupSelfTest.swift)\n"
            FileHandle.standardError.write(message.data(using: .utf8)!)
        }
    }

    /// Connects to `socketPath`, sends one `get_port` line for
    /// `sentinelCanvasPath`, and reports whether *any* line came back
    /// within `timeoutSeconds` — `{"error": ...}` counts as success here
    /// just as much as a real port would (see this type's own doc
    /// comment); only silence means something's actually wrong.
    private static func getPortRoundTrip(socketPath: String, timeoutSeconds: Int) -> Bool {
        let fd = socket(AF_UNIX, SOCK_STREAM, 0)
        guard fd >= 0 else { return false }
        defer { close(fd) }

        var addr = sockaddr_un()
        addr.sun_family = sa_family_t(AF_UNIX)
        let pathBytes = Array(socketPath.utf8)
        guard pathBytes.count < MemoryLayout.size(ofValue: addr.sun_path) else { return false }
        withUnsafeMutableBytes(of: &addr.sun_path) { raw in
            let buf = raw.bindMemory(to: UInt8.self)
            for (i, byte) in pathBytes.enumerated() { buf[i] = byte }
        }

        let connectResult = withUnsafePointer(to: &addr) { ptr -> Int32 in
            ptr.withMemoryRebound(to: sockaddr.self, capacity: 1) { sockPtr in
                connect(fd, sockPtr, socklen_t(MemoryLayout<sockaddr_un>.size))
            }
        }
        guard connectResult == 0 else { return false }

        var tv = timeval(tv_sec: timeoutSeconds, tv_usec: 0)
        setsockopt(fd, SOL_SOCKET, SO_RCVTIMEO, &tv, socklen_t(MemoryLayout<timeval>.size))

        let message = "{\"op\":\"get_port\",\"canvas_path\":\"\(sentinelCanvasPath)\"}\n"
        let sent = message.withCString { write(fd, $0, strlen($0)) }
        guard sent > 0 else { return false }

        var buf = [UInt8](repeating: 0, count: 256)
        let n = buf.withUnsafeMutableBytes { read(fd, $0.baseAddress, $0.count) }
        return n > 0
    }
}
