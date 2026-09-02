// swift-tools-version:5.9
// Core-only MVP (see TODO.canvas.md's "Ссылки и навигация между
// канвасами" → macOS daemon): a menu-bar app that speaks the exact same
// socket protocol `crates/server/src/watcher_protocol.rs`/
// `crates/cli/src/watcher.rs` already do, just with a persistent,
// well-known socket and a menu instead of a private per-invocation one
// that dies with its own process tree. Deliberately no distribution
// story yet (no .app bundle, no code signing, no launch-at-login) — run
// via `swift run` or the built executable directly. See this package's
// own README for how to try it.
import PackageDescription

let package = Package(
    name: "MeshfoxDaemon",
    platforms: [.macOS(.v13)],
    targets: [
        .executableTarget(name: "MeshfoxDaemon")
    ]
)
