import { test, expect, type Frame, type Page } from "@playwright/test";
import { type ChildProcess, execSync, spawn } from "node:child_process";
import { mkdirSync, writeFileSync } from "node:fs";
import { homedir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { launchVSCode, openMeshfoxCanvas } from "./helpers";

const __dirname = dirname(fileURLToPath(import.meta.url));

// Drives a real, installed VS Code through `editors/vscode`'s own Extension
// Development Host, exactly like `paste.spec.ts` — but this suite's own
// point is `crates/cli/src/coordinator.ts`'s new `server_socket`-routed
// path: a real macOS daemon (`macos/MeshfoxDaemon`, built by this repo's own
// `swift build`) get-or-spawns this canvas's worker over the real
// `watcher_protocol::Message::GetPort` wire, not the extension's own
// private `Coordinator.getOrSpawnWorker` spawn. Proven by process ancestry,
// not just "the canvas rendered" — a worker the extension spawned itself
// and one the daemon spawned would both make the canvas render identically;
// only checking who the worker's *parent* process actually is tells the
// two apart.
//
// Prerequisites beyond `playwright.config.ts`'s own list: the daemon binary
// itself, built via `(cd macos/MeshfoxDaemon && swift build)` — not part of
// this suite's own setup (same "cheap to re-run, left as a manual step"
// posture the other three prerequisites already have).

const REPO_ROOT = join(__dirname, "..", "..", "..");
const MESHFOX_BIN = join(REPO_ROOT, "target", "debug", "meshfox");
const DAEMON_BIN = join(REPO_ROOT, "macos", "MeshfoxDaemon", ".build", "debug", "MeshfoxDaemon");
const DAEMON_SOCKET = join(homedir(), "Library", "Application Support", "meshfox", "daemon.sock");

let daemon: ChildProcess;
let session: Awaited<ReturnType<typeof launchVSCode>>;
let page: Page;
let frame: Frame;

test.describe.serial("VS Code as a server_socket client of the real macOS daemon", () => {
  test.beforeAll(async () => {
    daemon = spawn(DAEMON_BIN, [], {
      env: { ...process.env, MESHFOX_BIN },
      stdio: ["ignore", "pipe", "pipe"],
    });
    // No IPC handshake to await — the socket file's own appearance is the
    // signal (same "unlink stale, then bind" contract `UnixSocketServer.
    // swift::start()` documents), so poll for it directly.
    for (let i = 0; i < 100; i++) {
      try {
        execSync(`test -S "${DAEMON_SOCKET}"`);
        break;
      } catch {
        await new Promise((r) => setTimeout(r, 100));
      }
    }

    session = await launchVSCode("daemon-coordinator.canvas.md");
    // `launchVSCode` only copies the named fixture file itself — the
    // `server_socket` config has to be dropped into the workspace
    // separately, before the canvas is actually opened (`getOrSpawnWorker`
    // reads it fresh at that point, not cached at extension activation).
    mkdirSync(join(session.workspaceDir, ".meshfox"), { recursive: true });
    writeFileSync(
      join(session.workspaceDir, ".meshfox", "config.toml"),
      `server_socket = "${DAEMON_SOCKET}"\n`,
    );

    page = session.page;
    frame = await openMeshfoxCanvas(page);
  });

  test.afterAll(async () => {
    await session?.app.close();
    session?.cleanup();
    daemon?.kill("SIGTERM");
  });

  test("the canvas renders via a worker the daemon spawned, not the extension's own", async () => {
    await expect(frame.getByText("Daemon Coordinator Fixture").first()).toBeVisible({ timeout: 15_000 });

    // Confirmed by ancestry, not just "a worker exists for this file":
    // `ps`'s own `ppid` column for the spawned `meshfox view --watcher-
    // socket ...` process must be the daemon's own pid — the extension's
    // *own* spawn path (had `server_socket` not been picked up at all)
    // would instead show VS Code's extension-host process as the parent.
    const psOutput = execSync(`ps -eww -o pid,ppid,command`).toString();
    const workerLine = psOutput
      .split("\n")
      .find((line) => line.includes("view") && line.includes("--watcher-socket") && line.includes(DAEMON_SOCKET));
    expect(workerLine, `no worker line found in ps output:\n${psOutput}`).toBeTruthy();
    const [, ppidStr] = workerLine!.trim().split(/\s+/);
    expect(Number(ppidStr)).toBe(daemon.pid!);
  });
});
