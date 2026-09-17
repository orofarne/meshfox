<!-- meshfox:canvas -->
# Daemon Coordinator Fixture
<!-- meshfox:node id="root" -->

Drives `server-socket.spec.ts` — a real macOS daemon (`macos/MeshfoxDaemon`)
must get-or-spawn this canvas's own worker via `Message::GetPort`, not this
extension's own private `Coordinator.getOrSpawnWorker` spawn path.
