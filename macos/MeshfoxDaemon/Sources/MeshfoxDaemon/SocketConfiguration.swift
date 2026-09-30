import Foundation
import Darwin

enum SocketConfiguration {
    static var url: URL {
        FileManager.default.homeDirectoryForCurrentUser
            .appendingPathComponent(".meshfox/config.toml")
    }

    static func hasServerSocket() throws -> Bool {
        guard FileManager.default.fileExists(atPath: url.path) else { return false }
        let contents = try String(contentsOf: url, encoding: .utf8)
        for line in contents.components(separatedBy: .newlines) {
            let trimmed = line.trimmingCharacters(in: .whitespaces)
            if trimmed.hasPrefix("[") { break }
            if trimmed.range(of: #"^(?:server_socket|"server_socket"|'server_socket')\s*="#,
                             options: .regularExpression) != nil { return true }
        }
        return false
    }

    static func configure(socketPath: String) throws {
        if try hasServerSocket() { return }
        let file = url
        let manager = FileManager.default
        try manager.createDirectory(at: file.deletingLastPathComponent(),
                                    withIntermediateDirectories: true)
        if (try? manager.destinationOfSymbolicLink(atPath: file.path)) != nil {
            throw CocoaError(.fileWriteNoPermission)
        }

        // A top-level TOML key must precede any [table]. Keep all existing
        // settings and permissions, and replace the file atomically.
        let old = manager.fileExists(atPath: file.path)
            ? try Data(contentsOf: file) : Data()
        let escaped = socketPath
            .replacingOccurrences(of: "\\", with: "\\\\")
            .replacingOccurrences(of: "\"", with: "\\\"")
        let setting = "server_socket = \"\(escaped)\" # meshfox-pkg-socket\n"
        var contents = Data(setting.utf8)
        contents.append(old)

        let mode = (try? manager.attributesOfItem(atPath: file.path)[.posixPermissions] as? NSNumber)?.int16Value ?? 0o600
        let temp = file.deletingLastPathComponent().appendingPathComponent(".config.toml.\(UUID().uuidString)")
        let descriptor = open(temp.path, O_WRONLY | O_CREAT | O_EXCL, 0o600)
        guard descriptor >= 0 else { throw CocoaError(.fileWriteUnknown) }
        do {
            let handle = FileHandle(fileDescriptor: descriptor, closeOnDealloc: true)
            try handle.write(contentsOf: contents)
            try handle.close()
            guard chmod(temp.path, mode_t(mode)) == 0,
                  rename(temp.path, file.path) == 0 else {
                throw CocoaError(.fileWriteUnknown)
            }
        } catch {
            try? manager.removeItem(at: temp)
            throw error
        }
    }
}
