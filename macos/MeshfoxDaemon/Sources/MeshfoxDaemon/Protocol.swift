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
