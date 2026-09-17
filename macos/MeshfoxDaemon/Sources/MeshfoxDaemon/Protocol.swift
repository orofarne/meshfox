import Foundation

/// Mirrors `crates/server/src/watcher_protocol.rs`'s `Message` enum
/// byte-for-byte (`#[serde(tag = "op", rename_all = "snake_case")]`, plain
/// field names — no field-level renaming there, so `canvas_path` stays
/// snake_case on the wire). One JSON object per line, newline-delimited,
/// one message per connection — see that module's own doc comment for the
/// full protocol rationale (a named socket, not an inherited descriptor,
/// specifically so a coordinator in any language can speak it).
enum WatcherMessage {
    case ready(canvasPath: String, port: UInt16)
    case open(canvasPath: String, fragment: String?)
    /// A "↗ open" on a plain (non-canvas) file node's target — see the
    /// Rust side's `Message::OpenFile` doc comment for why this is a
    /// separate case rather than a reused field on `.open`.
    case openFile(path: String)
    /// "Get-or-spawn a worker for `canvasPath`, don't open a browser tab,
    /// just tell me its port" — the one message that gets a reply on the
    /// same connection instead of being fire-and-forget; see the Rust
    /// side's `Message::GetPort`/`PortResponse` doc comments and
    /// `UnixSocketServer`'s own handling of this case specifically.
    case getPort(canvasPath: String)
}

extension WatcherMessage: Decodable {
    private enum CodingKeys: String, CodingKey {
        case op
        case canvasPath = "canvas_path"
        case port
        case fragment
        case path
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        let op = try container.decode(String.self, forKey: .op)
        switch op {
        case "ready":
            let path = try container.decode(String.self, forKey: .canvasPath)
            let port = try container.decode(UInt16.self, forKey: .port)
            self = .ready(canvasPath: path, port: port)
        case "open":
            let path = try container.decode(String.self, forKey: .canvasPath)
            let fragment = try container.decodeIfPresent(String.self, forKey: .fragment)
            self = .open(canvasPath: path, fragment: fragment)
        case "open_file":
            let path = try container.decode(String.self, forKey: .path)
            self = .openFile(path: path)
        case "get_port":
            let path = try container.decode(String.self, forKey: .canvasPath)
            self = .getPort(canvasPath: path)
        default:
            throw DecodingError.dataCorruptedError(
                forKey: .op,
                in: container,
                debugDescription: "unknown watcher-protocol op \(op)"
            )
        }
    }

    /// Parses one already-trimmed newline-delimited-JSON line — `nil`
    /// (not thrown) for anything malformed, matching the Rust watcher's
    /// own "a bad line is just dropped, nothing meaningful to reply with
    /// over this one-way protocol" stance (`crates/cli/src/watcher.rs`'s
    /// `handle_connection`).
    static func parse(line: String) -> WatcherMessage? {
        guard let data = line.data(using: .utf8) else { return nil }
        return try? JSONDecoder().decode(WatcherMessage.self, from: data)
    }
}

/// The one JSON line written back after a `.getPort` request — mirrors the
/// Rust side's own (untagged) `PortResponse`: exactly one of `port`/`error`
/// present, never both. See `UnixSocketServer`'s own handling of `.getPort`
/// for where this gets written.
enum PortReply {
    case port(UInt16)
    case error(String)
}

extension PortReply: Encodable {
    private enum CodingKeys: String, CodingKey {
        case port
        case error
    }

    func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        switch self {
        case .port(let port):
            try container.encode(port, forKey: .port)
        case .error(let message):
            try container.encode(message, forKey: .error)
        }
    }
}
